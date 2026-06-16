//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentSession`] is the agent handle enum dispatching to concrete session types
//! ([`ClaudeSession`], [`TestAgentSession`]). [`PtyHandle`] encapsulates PTY I/O
//! (input, output subscription, resize). [`spawn_pty_agent`] is the shared helper
//! that creates the PTY, spawns reader/writer/exit-monitor tasks, and returns a
//! `PtyHandle` + `StructuredLogSource`.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
use super::TestAgentSession;
#[cfg(feature = "local-agents")]
use super::claude::{ClaudeSession, ClaudeStructuredInputTarget};
#[cfg(feature = "local-agents")]
use super::{ExternalHookBootstrap, HookError, HookOutcome, PtyHandle};
use super::{LocalAgentNameSource, StructuredLogSource};
#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
use crate::agents::AGENT_TYPE_TEST_AGENT;
#[cfg(all(feature = "local-agents", any(test, feature = "testnet")))]
use crate::agents::TEST_ECHO_V1;
use crate::agents::claude::io as claude_io;
use crate::agents::{AGENT_TYPE_CLAUDE, Agent, AgentEvent, AgentType, CreateAgentRequest};
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

/// Internal agent metadata owned by the runtime.
#[derive(Debug, Clone)]
pub(crate) struct AgentRecord {
    pub(crate) id: Uuid,
    pub(crate) host_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    pub(crate) working_dir: PathBuf,
    pub(crate) agent_type: String,
    pub(crate) io_protocols: Vec<String>,
    pub(crate) readonly: bool,
    pub(crate) args: Vec<String>,
    pub(crate) created_at: DateTime<Utc>,
}

impl AgentRecord {
    pub(crate) fn agent_event(&self) -> AgentEvent {
        AgentEvent::AgentUp {
            agent: Agent::from(self),
        }
    }

    pub(crate) fn agent_updated_event(&self) -> AgentEvent {
        AgentEvent::AgentUpdated {
            agent: Agent::from(self),
        }
    }
}

impl From<&AgentRecord> for Agent {
    fn from(agent: &AgentRecord) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name.clone(),
            command: agent.command.clone(),
            working_dir: agent.working_dir.clone(),
            agent_type: agent.agent_type.clone(),
            io_protocols: agent.io_protocols.clone(),
            readonly: agent.readonly,
            args: agent.args.clone(),
            created_at: agent.created_at,
        }
    }
}

impl From<AgentRecord> for Agent {
    fn from(agent: AgentRecord) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name,
            command: agent.command,
            working_dir: agent.working_dir,
            agent_type: agent.agent_type,
            io_protocols: agent.io_protocols.clone(),
            readonly: agent.readonly,
            args: agent.args,
            created_at: agent.created_at,
        }
    }
}

/// Events sent from agent sessions to their owning AgentService.
#[derive(Clone)]
pub(crate) enum SessionEvent {
    /// Session ended (agent exited)
    Ended { agent_id: Uuid },
    /// Session created (for post-creation side effects like fork detection)
    Created {
        agent_id: Uuid,
        agent_type: AgentType,
        args: Vec<String>,
    },
    /// A provider discovered a stronger local name candidate for this session.
    NameCandidateChanged {
        agent_id: Uuid,
        name: String,
        source: LocalAgentNameSource,
    },
}

/// Policy for stopping an agent session
pub(crate) enum StopPolicy {
    /// Send interrupt signal (close PTY master)
    Interrupt,
}

/// Unified agent session handle, dispatching to concrete session types.
pub(crate) enum AgentSession {
    #[cfg(feature = "local-agents")]
    Claude(ClaudeSession),
    #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
    TestAgent(TestAgentSession),
    #[cfg(not(feature = "local-agents"))]
    Disabled,
}

#[derive(Clone)]
pub(crate) enum StructuredInputTarget {
    #[cfg(feature = "local-agents")]
    Claude(ClaudeStructuredInputTarget),
    #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
    Unsupported,
    #[cfg(not(feature = "local-agents"))]
    Disabled,
}

impl StructuredInputTarget {
    pub(crate) async fn send_structured_input(
        &self,
        client_seq: u64,
        payload: Value,
    ) -> std::result::Result<(), ProtocolError> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(target) => target.send_structured_input(client_seq, payload).await,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::Unsupported => Err(ProtocolError::ServerError {
                message: "structured input not supported".to_string(),
            }),
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => Err(ProtocolError::FailedPrecondition {
                message: "local agent support is disabled".to_string(),
            }),
        }
    }
}

impl AgentSession {
    pub(crate) fn try_new(req: &CreateAgentRequest) -> Result<Self> {
        #[cfg(not(feature = "local-agents"))]
        {
            let _ = req;
            return Err(anyhow!("local agent support is disabled"));
        }
        #[cfg(feature = "local-agents")]
        match &req.agent_type {
            AgentType::Claude => Ok(Self::Claude(ClaudeSession::new(req))),
            #[cfg(any(debug_assertions, test))]
            AgentType::TestAgent { command } => {
                Ok(Self::TestAgent(TestAgentSession::new(req, command.clone())))
            }
        }
    }

    pub(crate) fn agent_id(&self) -> Uuid {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.agent_id,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.agent_id,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.name.as_deref(),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.name.as_deref(),
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn command(&self) -> &str {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => &s.command,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => &s.command,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn working_dir(&self) -> &Path {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => &s.working_dir,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => &s.working_dir,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn readonly(&self) -> bool {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.readonly,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => false,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    /// Start the agent process (two-phase init: new() stores metadata, start() spawns).
    /// Returns an exit handle that completes when the agent process exits.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.start(),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.start(),
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    /// Stop the agent according to the given policy.
    pub(crate) async fn stop(&self, policy: StopPolicy) {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.stop(policy).await,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.stop().await,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn maybe_start_name_sniffer(&mut self, event_tx: &mpsc::Sender<SessionEvent>) {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.maybe_start_name_sniffer(event_tx),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => {}
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => Some(s.name_source()),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => None,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.set_name_and_source(name, source),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.name = name,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn log_source(&self) -> Option<StructuredLogSource> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.log_source(),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.log_source(),
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn io_protocols(&self) -> Vec<String> {
        let mut protocols = Vec::new();
        #[cfg(feature = "local-agents")]
        if self.pty_handle().is_some() {
            protocols.push(claude_io::RAW_V1.to_string());
        }
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(_) => protocols.push(claude_io::PTY_TRANSCRIPT_V1.to_string()),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => {
                #[cfg(any(test, feature = "testnet"))]
                protocols.push(TEST_ECHO_V1.to_string());
            }
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => {}
        }
        protocols
    }

    pub(crate) fn structured_input_target(&self) -> StructuredInputTarget {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => StructuredInputTarget::Claude(s.structured_input_target()),
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => StructuredInputTarget::Unsupported,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => StructuredInputTarget::Disabled,
        }
    }

    /// Validate seq and send structured input to the agent.
    #[cfg(test)]
    pub(crate) async fn send_structured_input(
        &self,
        client_seq: u64,
        payload: Value,
    ) -> std::result::Result<(), ProtocolError> {
        self.structured_input_target()
            .send_structured_input(client_seq, payload)
            .await
    }

    /// Handle an opaque hook payload for this agent.
    #[cfg(feature = "local-agents")]
    pub(crate) async fn handle_hook(
        &mut self,
        payload: &[u8],
    ) -> std::result::Result<HookOutcome, HookError> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.handle_hook_payload(payload).await,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => Err(HookError::UnsupportedAgentType),
        }
    }

    #[cfg(feature = "local-agents")]
    pub(crate) async fn bootstrap_external_hook(
        agent_id: Uuid,
        payload: &[u8],
    ) -> std::result::Result<ExternalHookBootstrap, HookError> {
        ClaudeSession::bootstrap_external_hook(agent_id, payload)
            .await
            .map(|session| match session {
                Some(session) => ExternalHookBootstrap::Register(Self::Claude(session)),
                None => ExternalHookBootstrap::Noop,
            })
    }

    /// Get the PTY handle (if this session type has one).
    #[cfg(feature = "local-agents")]
    pub(crate) fn pty_handle(&self) -> Option<&PtyHandle> {
        match self {
            Self::Claude(s) => s.pty.as_ref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.pty.as_ref(),
        }
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => s.created_at,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => s.created_at,
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    pub(crate) fn args(&self) -> &[String] {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => &s.args,
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(_) => &[],
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    /// Convert to an agent record for listing/registry.
    pub(crate) fn to_agent(&self, host_id: Uuid) -> AgentRecord {
        AgentRecord {
            id: self.agent_id(),
            host_id,
            name: self.name().map(String::from),
            command: self.command().to_string(),
            working_dir: self.working_dir().to_path_buf(),
            agent_type: match self {
                #[cfg(feature = "local-agents")]
                Self::Claude(_) => AGENT_TYPE_CLAUDE.to_string(),
                #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
                Self::TestAgent(_) => AGENT_TYPE_TEST_AGENT.to_string(),
                #[cfg(not(feature = "local-agents"))]
                Self::Disabled => unreachable!("local agent support is disabled"),
            },
            io_protocols: self.io_protocols(),
            readonly: self.readonly(),
            args: self.args().to_vec(),
            created_at: self.created_at(),
        }
    }

    pub(crate) fn from_suspended(suspended: SuspendedAgent) -> Self {
        match suspended {
            #[cfg(feature = "local-agents")]
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
                Self::Claude(ClaudeSession::from_suspended(
                    &req,
                    name_source.into(),
                    session_id,
                    created_at,
                ))
            }
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
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
                Self::TestAgent(TestAgentSession::from_suspended(&req, command, created_at))
            }
            #[cfg(not(feature = "local-agents"))]
            SuspendedAgent::Disabled => unreachable!("local agent support is disabled"),
        }
    }

    /// Build the serializable suspend state without stopping the running agent.
    pub(crate) fn suspended_state(&self) -> Result<SuspendedAgent> {
        match self {
            #[cfg(feature = "local-agents")]
            Self::Claude(s) => {
                let name_source = s.name_source();
                let session_id = s.session_id.ok_or_else(|| {
                    anyhow!(
                        "cannot suspend claude agent {}: no session_id (SessionStart hook not received)",
                        s.agent_id
                    )
                })?;
                Ok(SuspendedAgent::Claude {
                    agent_id: s.agent_id,
                    name: s.name.clone(),
                    name_source: name_source.into(),
                    working_dir: s.working_dir.clone(),
                    terminal_size: s.terminal_size,
                    created_at: s.created_at,
                    args: s.args.clone(),
                    session_id,
                })
            }
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            Self::TestAgent(s) => Ok(SuspendedAgent::TestAgent {
                agent_id: s.agent_id,
                name: s.name.clone(),
                command: s.command.clone(),
                working_dir: s.working_dir.clone(),
                terminal_size: s.terminal_size,
                created_at: s.created_at,
            }),
            #[cfg(not(feature = "local-agents"))]
            Self::Disabled => unreachable!("local agent support is disabled"),
        }
    }
}

impl serde::Serialize for crate::debug::DebugView<'_, AgentSession> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.inner {
            #[cfg(feature = "local-agents")]
            AgentSession::Claude(s) => {
                crate::debug::DebugView::new(s, self.verbose).serialize(serializer)
            }
            #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
            AgentSession::TestAgent(s) => {
                crate::debug::DebugView::new(s, self.verbose).serialize(serializer)
            }
            #[cfg(not(feature = "local-agents"))]
            AgentSession::Disabled => serializer.serialize_none(),
        }
    }
}

#[cfg(all(test, feature = "local-agents"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::{AgentType, CreateAgentRequest};
    use crate::suspend::SuspendedLocalAgentNameSource;

    #[tokio::test]
    #[cfg(any(debug_assertions, test))]
    async fn test_test_agent_rejects_structured_input() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "test-agent".to_string(),
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![],
        };
        let session =
            AgentSession::TestAgent(TestAgentSession::new(&req, "test-agent".to_string()));

        let err = session
            .send_structured_input(0, json!({"SubmitPrompt": "hello"}))
            .await
            .unwrap_err();

        assert_eq!(
            err,
            ProtocolError::ServerError {
                message: "structured input not supported".to_string(),
            },
        );
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

        let session = AgentSession::from_suspended(sa);

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

        let suspended = AgentSession::Claude(session).suspended_state().unwrap();

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
