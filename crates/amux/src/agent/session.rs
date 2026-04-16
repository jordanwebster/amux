//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentSession`] is the agent handle enum dispatching to concrete session types
//! ([`ClaudeSession`], [`TestAgentSession`]). [`PtyHandle`] encapsulates PTY I/O
//! (input, output subscription, resize). [`spawn_pty_agent`] is the shared helper
//! that creates the PTY, spawns reader/writer/exit-monitor tasks, and returns a
//! `PtyHandle` + `StructuredLogSource`.

#[cfg(any(debug_assertions, test))]
use super::TestAgentSession;
use super::{
    ClaudeSession, ExternalHookBootstrap, HookError, HookOutcome, LocalAgentNameSource, PtyHandle,
};
#[cfg(test)]
use crate::agent::StructuredLogSource;
use anyhow::{Result, anyhow};

use crate::buffer::MultiplexStructuredReader;
use crate::protocol::message::{
    AgentType, CreateAgentRequest, DirectMessage, HookProvider, ProtocolError, SubscribeQuery,
};
use crate::protocol::route::Route;
use crate::suspend::SuspendedAgent;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Internal agent metadata owned by the runtime.
#[derive(Debug, Clone)]
pub(crate) struct Agent {
    pub(crate) id: Uuid,
    pub(crate) host_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    pub(crate) working_dir: PathBuf,
    pub(crate) route: Route,
    pub(crate) agent_type: String,
    pub(crate) structured_protocol: Option<String>,
    pub(crate) readonly: bool,
    pub(crate) args: Vec<String>,
    pub(crate) created_at: DateTime<Utc>,
}

impl Agent {
    pub(crate) fn is_remote(&self) -> bool {
        self.route.peek().is_some()
    }

    pub(crate) fn announce_message(&self) -> DirectMessage {
        DirectMessage::AnnounceAgent {
            agent_id: self.id,
            host_id: self.host_id,
            name: self.name.clone(),
            command: self.command.clone(),
            working_dir: self.working_dir.clone(),
            agent_type: self.agent_type.clone(),
            structured_protocol: self.structured_protocol.clone(),
            readonly: self.readonly,
            args: self.args.clone(),
            created_at: self.created_at,
        }
    }
}

impl From<Agent> for crate::protocol::Agent {
    fn from(agent: Agent) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name,
            command: agent.command,
            working_dir: agent.working_dir,
            route: agent.route,
            agent_type: agent.agent_type,
            structured_protocol: agent.structured_protocol,
            readonly: agent.readonly,
            args: agent.args,
            created_at: agent.created_at,
        }
    }
}

/// Events sent from agent sessions to the server event loop
#[derive(Clone)]
pub(crate) enum SessionEvent {
    /// Session ended (agent exited)
    Ended { agent_id: Uuid, user_id: Uuid },
    /// Session created (for post-creation side effects like fork detection)
    Created {
        agent_id: Uuid,
        user_id: Uuid,
        agent_type: AgentType,
        args: Vec<String>,
    },
    /// A provider discovered a stronger local name candidate for this session.
    NameCandidateChanged {
        agent_id: Uuid,
        user_id: Uuid,
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
    Claude(ClaudeSession),
    #[cfg(any(debug_assertions, test))]
    TestAgent(TestAgentSession),
}

impl AgentSession {
    pub(crate) fn try_new(req: &CreateAgentRequest) -> Result<Self> {
        match &req.agent_type {
            AgentType::Claude => Ok(Self::Claude(ClaudeSession::new(req))),
            #[cfg(any(debug_assertions, test))]
            AgentType::TestAgent { command } => {
                Ok(Self::TestAgent(TestAgentSession::new(req, command.clone())))
            }
            AgentType::Unknown => Err(anyhow!("unknown agent type")),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_readonly_claude(agent_id: Uuid, working_dir: PathBuf) -> Self {
        Self::Claude(ClaudeSession::new_readonly(agent_id, working_dir))
    }

    pub(crate) fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude(s) => s.agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.agent_id,
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Claude(s) => s.name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.name.as_deref(),
        }
    }

    pub(crate) fn command(&self) -> &str {
        match self {
            Self::Claude(s) => &s.command,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => &s.command,
        }
    }

    pub(crate) fn working_dir(&self) -> &Path {
        match self {
            Self::Claude(s) => &s.working_dir,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => &s.working_dir,
        }
    }

    pub(crate) fn readonly(&self) -> bool {
        match self {
            Self::Claude(s) => s.readonly,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => false,
        }
    }

    /// Start the agent process (two-phase init: new() stores metadata, start() spawns).
    /// Returns an exit handle that completes when the agent process exits.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        match self {
            Self::Claude(s) => s.start(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.start(),
        }
    }

    /// Stop the agent according to the given policy.
    pub(crate) async fn stop(&self, policy: StopPolicy) {
        match self {
            Self::Claude(s) => s.stop(policy).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.stop().await,
        }
    }

    pub(crate) fn maybe_start_name_sniffer(
        &mut self,
        user_id: Uuid,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) {
        if let Self::Claude(s) = self {
            s.maybe_start_name_sniffer(user_id, event_tx);
        }
    }

    pub(crate) fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        match self {
            Self::Claude(s) => Some(s.name_source()),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => None,
        }
    }

    pub(crate) fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        match self {
            Self::Claude(s) => s.set_name_and_source(name, source),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.name = name,
        }
    }

    #[cfg(test)]
    pub(crate) fn log_source(&self) -> Option<StructuredLogSource> {
        match self {
            Self::Claude(s) => s.log_source(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.log_source(),
        }
    }

    /// Subscribe to structured log output with an optional query filter
    /// and return the matching seq.
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SubscribeQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        match self {
            Self::Claude(s) => s.subscribe_with_query(query).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.subscribe_with_query(query).await,
        }
    }

    pub(crate) fn structured_protocol(&self) -> Option<String> {
        match self {
            Self::Claude(_) => Some("claude_pty_v1".to_string()),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => None,
        }
    }

    /// Validate seq and send structured input to the agent.
    pub(crate) async fn send_structured_input(
        &self,
        client_seq: u64,
        payload: Value,
    ) -> std::result::Result<(), ProtocolError> {
        match self {
            Self::Claude(s) => s.send_structured_input(client_seq, payload).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => Err(ProtocolError::ServerError {
                message: "structured input not supported".to_string(),
            }),
        }
    }

    /// Handle an opaque hook payload for this agent.
    pub(crate) async fn handle_hook(
        &mut self,
        provider: HookProvider,
        payload: &[u8],
    ) -> std::result::Result<HookOutcome, HookError> {
        match self {
            Self::Claude(s) => {
                if provider != HookProvider::Claude {
                    return Err(HookError::ProviderMismatch {
                        expected: HookProvider::Claude,
                        actual: provider,
                    });
                }
                s.handle_hook_payload(payload).await
            }
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => Err(HookError::ProviderMismatch {
                expected: HookProvider::Unknown,
                actual: provider,
            }),
        }
    }

    pub(crate) async fn bootstrap_external_hook(
        agent_id: Uuid,
        provider: HookProvider,
        payload: &[u8],
    ) -> std::result::Result<ExternalHookBootstrap, HookError> {
        match provider {
            HookProvider::Claude => ClaudeSession::bootstrap_external_hook(agent_id, payload)
                .await
                .map(|session| match session {
                    Some(session) => ExternalHookBootstrap::Register(Self::Claude(session)),
                    None => ExternalHookBootstrap::Noop,
                }),
            HookProvider::Unknown => Err(HookError::UnsupportedProvider(provider)),
        }
    }

    /// Get the PTY handle (if this session type has one).
    pub(crate) fn pty_handle(&self) -> Option<&PtyHandle> {
        match self {
            Self::Claude(s) => s.pty.as_ref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.pty.as_ref(),
        }
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        match self {
            Self::Claude(s) => s.created_at,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.created_at,
        }
    }

    pub(crate) fn args(&self) -> &[String] {
        match self {
            Self::Claude(s) => &s.args,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => &[],
        }
    }

    /// Convert to Agent for listing/registry.
    pub(crate) fn to_agent(&self, host_id: Uuid) -> Agent {
        Agent {
            id: self.agent_id(),
            host_id,
            name: self.name().map(String::from),
            command: self.command().to_string(),
            working_dir: self.working_dir().to_path_buf(),
            route: Route::empty(),
            agent_type: match self {
                Self::Claude(_) => "claude".to_string(),
                #[cfg(any(debug_assertions, test))]
                Self::TestAgent(_) => "test_agent".to_string(),
            },
            structured_protocol: self.structured_protocol(),
            readonly: self.readonly(),
            args: self.args().to_vec(),
            created_at: self.created_at(),
        }
    }

    pub(crate) fn from_suspended(suspended: SuspendedAgent) -> Self {
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
        }
    }

    /// Suspend this session: stop the agent and return serializable state.
    /// Consumes self.
    pub(crate) async fn suspend(self) -> Result<SuspendedAgent> {
        match self {
            Self::Claude(s) => {
                s.stop(StopPolicy::Interrupt).await;
                let name_source = s.name_source();
                let session_id = s.session_id.ok_or_else(|| {
                    anyhow!(
                        "cannot suspend claude agent {}: no session_id (SessionStart hook not received)",
                        s.agent_id
                    )
                })?;
                Ok(SuspendedAgent::Claude {
                    agent_id: s.agent_id,
                    name: s.name,
                    name_source: name_source.into(),
                    working_dir: s.working_dir,
                    terminal_size: s.terminal_size,
                    created_at: s.created_at,
                    args: s.args,
                    session_id,
                })
            }
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => {
                s.stop().await;
                Ok(SuspendedAgent::TestAgent {
                    agent_id: s.agent_id,
                    name: s.name,
                    command: s.command,
                    working_dir: s.working_dir,
                    terminal_size: s.terminal_size,
                    created_at: s.created_at,
                })
            }
        }
    }
}

impl serde::Serialize for crate::debug::DebugView<'_, AgentSession> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.inner {
            AgentSession::Claude(s) => {
                crate::debug::DebugView::new(s, self.verbose).serialize(serializer)
            }
            #[cfg(any(debug_assertions, test))]
            AgentSession::TestAgent(s) => {
                crate::debug::DebugView::new(s, self.verbose).serialize(serializer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CreateAgentRequest;
    use crate::protocol::message::AgentType;
    use crate::suspend::SuspendedLocalAgentNameSource;
    use serde_json::json;

    #[tokio::test]
    #[cfg(any(debug_assertions, test))]
    async fn test_test_agent_rejects_structured_input() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
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

        let suspended = AgentSession::Claude(session).suspend().await.unwrap();

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
