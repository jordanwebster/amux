//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentSession`] owns a dynamic [`AgentBackend`]. [`PtyHandle`] encapsulates
//! PTY I/O (input, output subscription, resize), while [`spawn_pty_agent`] is
//! the shared helper that creates the PTY and its reader/writer/exit-monitor
//! tasks. Structured log state is concrete-backend policy, not PTY policy.
//!
//! This whole module owns or drives a live agent process, so it is gated at its
//! `mod` declaration behind the `local-agents` feature. The data types it
//! produces ([`AgentRecord`], [`SessionEvent`], [`StopPolicy`]) live in
//! [`super::record`] and stay compiled in every build.

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

#[cfg(any(debug_assertions, test))]
use super::TestAgentSession;
use super::claude::ClaudeSession;
use super::{
    AgentRecord, ExternalHookBootstrap, HookError, HookOutcome, LocalAgentNameSource, PtyHandle,
    SessionEvent, StopPolicy, StructuredLogSource,
};
use crate::agents::terminal_io;
use crate::agents::{AgentType, CreateAgentRequest};
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

/// An owned structured-input endpoint detached from the session registry lock.
#[async_trait]
pub(crate) trait StructuredInput: Send + Sync {
    async fn send(&self, client_seq: u64, payload: Value)
    -> std::result::Result<(), ProtocolError>;
}

/// Instance behavior implemented by every locally hosted agent backend.
#[async_trait]
pub(crate) trait AgentBackend: Send + Sync {
    fn agent_id(&self) -> Uuid;
    fn name(&self) -> Option<&str>;
    fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource);
    fn command(&self) -> &str;
    fn working_dir(&self) -> &Path;
    fn readonly(&self) -> bool;
    fn args(&self) -> &[String];
    fn created_at(&self) -> DateTime<Utc>;
    fn start(&mut self) -> Result<tokio::task::JoinHandle<()>>;
    async fn stop(&self, policy: StopPolicy);
    fn agent_type(&self) -> &'static str;

    fn io_protocols(&self) -> Vec<String> {
        terminal_io_protocols(self.pty_handle())
    }

    fn log_source(&self) -> Option<StructuredLogSource>;
    fn pty_handle(&self) -> Option<&PtyHandle>;

    fn structured_input(&self) -> Option<Box<dyn StructuredInput>> {
        None
    }

    async fn handle_hook_payload(
        &mut self,
        _payload: &[u8],
    ) -> std::result::Result<HookOutcome, HookError> {
        Err(HookError::UnsupportedAgentType)
    }

    fn maybe_start_name_sniffer(&mut self, _event_tx: &mpsc::Sender<SessionEvent>) {}

    fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        None
    }

    fn to_agent(&self, host_id: Uuid) -> AgentRecord {
        AgentRecord {
            id: self.agent_id(),
            host_id,
            name: self.name().map(String::from),
            command: self.command().to_string(),
            working_dir: self.working_dir().to_path_buf(),
            agent_type: self.agent_type().to_string(),
            io_protocols: self.io_protocols(),
            readonly: self.readonly(),
            args: self.args().to_vec(),
            created_at: self.created_at(),
        }
    }

    fn suspended_state(&self) -> Result<SuspendedAgent>;

    fn debug_json(&self, verbose: bool) -> serde_json::Result<Value>;
}

/// The shared terminal protocol advertisement for every PTY-backed backend.
pub(crate) fn terminal_io_protocols(pty: Option<&PtyHandle>) -> Vec<String> {
    if pty.is_some() {
        vec![terminal_io::TERMINAL_V1.to_string()]
    } else {
        Vec::new()
    }
}

/// Unified agent session handle backed by dynamic trait dispatch.
pub(crate) type AgentSession = Box<dyn AgentBackend>;

pub(crate) fn new_agent(req: &CreateAgentRequest) -> Result<AgentSession> {
    match &req.agent_type {
        AgentType::Claude => Ok(Box::new(ClaudeSession::new(req))),
        #[cfg(any(debug_assertions, test))]
        AgentType::TestAgent { command } => {
            Ok(Box::new(TestAgentSession::new(req, command.clone())))
        }
    }
}

pub(crate) fn agent_from_suspended(suspended: SuspendedAgent) -> AgentSession {
    match suspended {
        SuspendedAgent::Claude {
            agent_id,
            name,
            name_source,
            working_dir,
            terminal_size,
            args,
            session_id,
            created_at,
        } => {
            let req = CreateAgentRequest {
                agent_id,
                host_id: None,
                name,
                agent_type: AgentType::Claude,
                working_dir,
                terminal_size,
                args,
            };
            Box::new(ClaudeSession::from_suspended(
                &req,
                name_source.into(),
                session_id,
                created_at,
            ))
        }
        #[cfg(any(debug_assertions, test))]
        SuspendedAgent::TestAgent {
            agent_id,
            name,
            command,
            working_dir,
            terminal_size,
            created_at,
        } => {
            let req = CreateAgentRequest {
                agent_id,
                host_id: None,
                name,
                agent_type: AgentType::TestAgent {
                    command: command.clone(),
                },
                working_dir,
                terminal_size,
                args: vec![],
            };
            Box::new(TestAgentSession::from_suspended(&req, command, created_at))
        }
    }
}

pub(crate) async fn bootstrap_external_hook(
    agent_id: Uuid,
    payload: &[u8],
) -> std::result::Result<ExternalHookBootstrap, HookError> {
    ClaudeSession::bootstrap_external_hook(agent_id, payload)
        .await
        .map(|session| match session {
            Some(session) => ExternalHookBootstrap::Register(Box::new(session)),
            None => ExternalHookBootstrap::Noop,
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agents::{AgentType, CreateAgentRequest};
    use crate::suspend::SuspendedLocalAgentNameSource;

    #[tokio::test]
    async fn test_agent_has_no_structured_input() {
        let session = TestAgentSession::echo_for_tests(Uuid::new_v4(), None);
        assert!(session.structured_input().is_none());
    }

    #[test]
    fn suspended_claude_into_session_filters_resume_unsafe_args() {
        let sa = SuspendedAgent::Claude {
            agent_id: Uuid::new_v4(),
            name: Some("claude".to_string()),
            name_source: SuspendedLocalAgentNameSource::ProviderName,
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![
                "--dangerously-skip-permissions".to_string(),
                "--resume".to_string(),
                Uuid::new_v4().to_string(),
                "--fork-session".to_string(),
                "--continue".to_string(),
                "--from-pr=123".to_string(),
                "--session-id".to_string(),
                Uuid::new_v4().to_string(),
                "--worktree".to_string(),
                "feature-branch".to_string(),
                "--tmux=classic".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ],
            session_id: Uuid::new_v4(),
            created_at: Utc::now(),
        };

        let session = agent_from_suspended(sa);

        assert_eq!(
            session.to_agent(Uuid::new_v4()).args,
            vec![
                "--dangerously-skip-permissions".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn suspended_claude_persists_raw_args_before_resume_sanitization() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("claude".to_string()),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![
                "--resume".to_string(),
                Uuid::new_v4().to_string(),
                "--fork-session".to_string(),
                "--dangerously-skip-permissions".to_string(),
            ],
        };
        let mut session = ClaudeSession::new(&req);
        session.session_id = Some(Uuid::new_v4());

        let suspended = session.suspended_state().unwrap();

        let SuspendedAgent::Claude { args, .. } = suspended else {
            panic!("expected suspended claude agent");
        };
        assert_eq!(
            args,
            vec![
                req.args[0].clone(),
                req.args[1].clone(),
                req.args[2].clone(),
                req.args[3].clone(),
            ]
        );
    }
}
