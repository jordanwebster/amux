//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentBackend`] is the instance behavior every locally hosted agent
//! implements; [`AgentSession`] is the owned handle the runtime stores.
//! [`StructuredInput`] and [`RawPtyTarget`] are owned endpoints a backend hands
//! out so callers can do input or raw-PTY preparation once the session registry
//! lock is released. The Claude and test-agent impls live beside their sessions;
//! this module keeps only the factories and the shared
//! [`terminal_io_protocols`] advertisement.
//!
//! This module constructs live agent sessions, so it is gated at its `mod`
//! declaration behind the `local-agents` feature. The data types it produces
//! ([`AgentRecord`], [`SessionEvent`], [`StopPolicy`]) live in
//! [`super::record`] and stay compiled in every build.

use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

#[cfg(any(debug_assertions, test))]
use super::TestAgentSession;
use super::claude::ClaudeSession;
#[cfg(unix)]
use super::codex::{CodexClient, CodexRawPtyTarget, CodexSession};
use super::{
    AgentRecord, ExternalHookBootstrap, HookEnvironment, HookError, HookOutcome,
    LocalAgentNameSource, PtyHandle, SessionEvent, StopPolicy, StructuredLogSource,
};
use crate::agent_tools::AgentToolRequest;
use crate::agents::{AgentParent, AgentType, CreateAgentRequest, terminal_io};
use crate::envelope::Envelope;
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

/// The backend carrier that accepted an agent message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    Socket,
    Pty,
    InjectQueued,
    InjectStarted,
    TurnStarted,
}

/// The provider-specific launch policy a same-kind child inherits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpawnInheritance {
    pub(crate) claude_permission_args: Vec<String>,
    pub(crate) codex_approval_policy: Option<String>,
    pub(crate) codex_sandbox_policy: Option<String>,
}

impl Delivery {
    pub(crate) const fn carrier(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Pty => "pty",
            Self::InjectQueued => "inject_queued",
            Self::InjectStarted => "inject_started",
            Self::TurnStarted => "turn_started",
        }
    }
}

/// A backend could not accept an agent message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DeliveryError {
    #[error("{0} agents do not support message delivery")]
    UnsupportedAgentType(&'static str),
    #[error("message delivery failed: {0}")]
    Failed(String),
}

/// An owned message-delivery endpoint detached from the session registry.
///
/// Native carriers may wait on backend-specific readiness or confirmation,
/// so delivery must not retain the registry lock that protects mutable
/// session metadata.
#[async_trait]
pub(crate) trait AgentDeliveryTarget: Send + Sync {
    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError>;
}

struct UnsupportedAgentDelivery {
    agent_type: &'static str,
}

#[async_trait]
impl AgentDeliveryTarget for UnsupportedAgentDelivery {
    async fn deliver(&self, _envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        Err(DeliveryError::UnsupportedAgentType(self.agent_type))
    }
}

/// An owned structured-input endpoint detached from the session registry lock.
#[async_trait]
pub(crate) trait StructuredInput: Send + Sync {
    async fn send(&self, client_seq: u64, payload: Value)
    -> std::result::Result<(), ProtocolError>;
}

/// Codex-owned structured input endpoint, separate from Claude's sequence-
/// checked JSON seam.
#[cfg(unix)]
#[async_trait]
pub(crate) trait CodexInput: Send + Sync {
    async fn send(&self, input_id: Vec<u8>, input: super::codex::io::CodexSdkV1Input);
}

/// In-process route from a model-facing tool call to ClientService.
#[async_trait]
pub(crate) trait AgentToolExecutor: Send + Sync {
    async fn execute(&self, caller: Uuid, request: AgentToolRequest) -> Result<Value>;
}

/// Late-bound executor shared by sessions created before ClientService starts.
#[derive(Clone, Default)]
pub(crate) struct AgentToolRouter {
    executor: Arc<StdRwLock<Option<Arc<dyn AgentToolExecutor>>>>,
}

impl AgentToolRouter {
    pub(crate) fn bind(&self, executor: Arc<dyn AgentToolExecutor>) {
        *self
            .executor
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(executor);
    }

    pub(crate) async fn execute(&self, caller: Uuid, request: AgentToolRequest) -> Result<Value> {
        let executor = self
            .executor
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!("agent tools are unavailable until startup completes")
            })?;
        executor.execute(caller, request).await
    }
}

/// An owned raw-PTY preparation target detached from the session registry.
pub(crate) enum RawPtyTarget {
    Existing(PtyHandle),
    #[cfg(unix)]
    Codex(CodexRawPtyTarget),
}

/// Host-owned resources shared by agent backends.
#[derive(Clone)]
pub(crate) struct AgentDeps {
    pub(crate) runtime_dir: std::path::PathBuf,
    #[cfg(unix)]
    pub(crate) codex_client: Arc<CodexClient>,
    pub(crate) agent_tools: AgentToolRouter,
}

impl AgentDeps {
    pub(crate) fn new(
        runtime_dir: std::path::PathBuf,
        codex_private_socket: std::path::PathBuf,
    ) -> Self {
        #[cfg(not(unix))]
        let _ = codex_private_socket;
        Self {
            runtime_dir,
            #[cfg(unix)]
            codex_client: Arc::new(CodexClient::new(codex_private_socket)),
            agent_tools: AgentToolRouter::default(),
        }
    }
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
    fn start(
        &mut self,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>>;
    async fn stop(&self, policy: StopPolicy);
    fn agent_type(&self) -> &'static str;

    fn spawn_inheritance(&self) -> SpawnInheritance {
        SpawnInheritance::default()
    }

    fn parent(&self) -> Option<AgentParent> {
        None
    }

    /// Advertised I/O protocols; PTY-backed backends build on
    /// [`terminal_io_protocols`].
    fn io_protocols(&self) -> Vec<String>;

    fn log_source(&self) -> Option<StructuredLogSource>;
    fn pty_handle(&self) -> Result<Option<PtyHandle>>;

    /// Snapshot the smallest owned target needed to deliver a message.
    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(UnsupportedAgentDelivery {
            agent_type: self.agent_type(),
        })
    }

    /// Deliver a daemon-authored envelope through this backend's native
    /// input carrier. Runtime dispatch snapshots [`Self::delivery_target`]
    /// directly so registry locks are released before an await; this method
    /// remains the convenient backend-owned entry point for focused callers.
    #[allow(dead_code)]
    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        self.delivery_target().deliver(envelope).await
    }

    /// Snapshot the smallest owned target needed to prepare a raw subscription.
    /// The default covers backends whose PTY already exists for the session.
    fn raw_pty_target(&self) -> Result<Option<RawPtyTarget>> {
        self.pty_handle()
            .map(|handle| handle.map(RawPtyTarget::Existing))
    }

    fn structured_input(&self) -> Option<Box<dyn StructuredInput>> {
        None
    }

    #[cfg(unix)]
    fn codex_input(&self) -> Option<Box<dyn CodexInput>> {
        None
    }

    async fn handle_hook_payload(
        &mut self,
        _payload: &[u8],
        _env: &HookEnvironment,
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
            parent: self.parent(),
            working_on: None,
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

pub(crate) fn new_agent(req: &CreateAgentRequest, deps: &AgentDeps) -> Result<AgentSession> {
    match &req.agent_type {
        AgentType::Claude => Ok(Box::new(ClaudeSession::new(req, deps.runtime_dir.clone()))),
        #[cfg(unix)]
        AgentType::Codex { .. } => Ok(Box::new(CodexSession::new(
            req,
            deps.codex_client.clone(),
            deps.agent_tools.clone(),
        ))),
        #[cfg(not(unix))]
        AgentType::Codex { .. } => Err(anyhow::anyhow!(
            "Codex agents are unavailable on this platform"
        )),
        #[cfg(any(debug_assertions, test))]
        AgentType::TestAgent { command } => {
            Ok(Box::new(TestAgentSession::new(req, command.clone())))
        }
    }
}

pub(crate) fn agent_from_suspended(suspended: SuspendedAgent, deps: &AgentDeps) -> AgentSession {
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
            parent,
            working_on: _,
        } => {
            let req = CreateAgentRequest {
                agent_id,
                host_id: None,
                name,
                agent_type: AgentType::Claude,
                working_dir,
                terminal_size,
                args,
                parent,
                initial_prompt: None,
            };
            Box::new(ClaudeSession::from_suspended(
                &req,
                name_source.into(),
                session_id,
                created_at,
                deps.runtime_dir.clone(),
            ))
        }
        #[cfg(unix)]
        SuspendedAgent::Codex {
            agent_id,
            name,
            working_dir,
            model,
            approval_policy,
            sandbox_policy,
            thread_id,
            daemon_mode,
            created_at,
            parent,
            working_on: _,
        } => {
            let req = CreateAgentRequest {
                agent_id,
                host_id: None,
                name,
                agent_type: AgentType::Codex {
                    model,
                    approval_policy,
                    sandbox_policy,
                    resume_thread_id: Some(thread_id),
                },
                working_dir,
                terminal_size: None,
                args: Vec::new(),
                parent,
                initial_prompt: None,
            };
            Box::new(CodexSession::from_suspended(
                &req,
                deps.codex_client.clone(),
                deps.agent_tools.clone(),
                daemon_mode,
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
            parent: _,
            working_on: _,
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
                parent: None,
                initial_prompt: None,
            };
            Box::new(TestAgentSession::from_suspended(&req, command, created_at))
        }
    }
}

pub(crate) async fn bootstrap_external_hook(
    agent_id: Uuid,
    payload: &[u8],
    env: &HookEnvironment,
) -> std::result::Result<ExternalHookBootstrap, HookError> {
    ClaudeSession::bootstrap_external_hook(agent_id, payload, env)
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
            parent: None,
            working_on: None,
        };

        let deps = AgentDeps::new(
            std::env::temp_dir(),
            std::env::temp_dir().join("amux-test-codex.sock"),
        );
        let session = agent_from_suspended(sa, &deps);

        assert_eq!(
            session.to_agent(Uuid::new_v4()).args,
            vec![
                "--dangerously-skip-permissions".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn suspended_codex_into_session_preserves_resume_identity() {
        let agent_id = Uuid::new_v4();
        let created_at = Utc::now();
        let suspended = SuspendedAgent::Codex {
            agent_id,
            name: Some("codex".into()),
            working_dir: PathBuf::from("/tmp"),
            model: Some("test-model".into()),
            approval_policy: Some("on-request".into()),
            sandbox_policy: Some("workspace-write".into()),
            thread_id: "thread-resume".into(),
            daemon_mode: Some("spawned-well-known".into()),
            created_at,
            parent: None,
            working_on: None,
        };
        let deps = AgentDeps::new(
            std::env::temp_dir(),
            std::env::temp_dir().join("amux-test-codex.sock"),
        );

        let session = agent_from_suspended(suspended, &deps);
        let restored = session.suspended_state().unwrap();

        assert!(matches!(
            restored,
            SuspendedAgent::Codex {
                agent_id: restored_id,
                thread_id,
                daemon_mode,
                created_at: restored_at,
                ..
            } if restored_id == agent_id
                && thread_id == "thread-resume"
                && daemon_mode.as_deref() == Some("spawned-well-known")
                && restored_at == created_at
        ));
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
            parent: None,
            initial_prompt: None,
        };
        let mut session = ClaudeSession::new(&req, std::env::temp_dir());
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
