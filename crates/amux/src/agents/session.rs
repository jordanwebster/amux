//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentBackend`] is the instance behavior every locally hosted agent
//! implements; [`AgentSession`] is the owned handle the runtime stores.
//! [`Plane`] is the owned endpoint a backend hands out so callers can prepare
//! input or output once the session registry lock is released. The Claude and
//! test-agent impls live beside their sessions; this module keeps only the
//! factories and shared session behavior. Protocol exposure is derived from
//! each backend's [`AgentKind`].
//!
//! This module constructs live agent sessions, so it is gated at its `mod`
//! declaration behind the `local-agents` feature. The data types it produces
//! ([`AgentRecord`], [`SessionEvent`], [`StopPolicy`]) live in
//! [`super::record`] and stay compiled in every build.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

#[cfg(any(debug_assertions, test))]
use super::TestAgentSession;
use super::claude::{ClaudeSdkBackend, ClaudeSession, ClaudeVersionCache};
#[cfg(unix)]
use super::codex::{CodexBackend, CodexClient, CodexRawPtyTarget};
use super::types::SpawnInheritance;
use super::{
    AgentRecord, ExternalHookBootstrap, HookEnvironment, HookError, HookOutcome,
    LocalAgentNameSource, PtyHandle, SessionEvent, StopPolicy, StructuredLogSource,
};
use crate::agents::{
    AgentKind, AgentParent, AgentType, ClaudeDriver, CreateAgentRequest, Protocol,
};
use crate::config::Config;
use crate::envelope::Envelope;
use crate::protocol::ProtocolError;
use crate::suspend::SuspendedAgent;

/// The backend carrier that accepted an agent message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    Socket,
    Pty,
    Stream,
    InjectQueued,
    InjectStarted,
    TurnStarted,
}

impl Delivery {
    pub(crate) const fn carrier(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Pty => "pty",
            Self::Stream => "stream",
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
    #[error("message delivery is not available: {0}")]
    FailedPrecondition(String),
    #[error("message delivery failed: {0}")]
    Failed(String),
}

pub(crate) enum DeliveryLiveness {
    Live,
    Pending(String),
}

/// An owned message-delivery endpoint detached from the session registry.
///
/// Native carriers may wait on backend-specific readiness or confirmation,
/// so delivery must not retain the registry lock that protects mutable
/// session metadata.
#[async_trait]
pub(crate) trait AgentDeliveryTarget: Send + Sync {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError>;

    async fn wait_until_live(&self, timeout: Duration) -> std::result::Result<(), DeliveryError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.liveness()? {
                DeliveryLiveness::Live => return Ok(()),
                DeliveryLiveness::Pending(reason) if tokio::time::Instant::now() >= deadline => {
                    return Err(DeliveryError::FailedPrecondition(format!(
                        "{reason}; did not become ready within {}s",
                        timeout.as_secs()
                    )));
                }
                DeliveryLiveness::Pending(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError>;
}

struct UnsupportedAgentDelivery {
    provider: &'static str,
}

#[async_trait]
impl AgentDeliveryTarget for UnsupportedAgentDelivery {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        Err(DeliveryError::UnsupportedAgentType(self.provider))
    }

    async fn deliver(&self, _envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        Err(DeliveryError::UnsupportedAgentType(self.provider))
    }
}

/// A typed input accepted by a structured protocol plane.
pub(crate) enum StructuredInputEvent {
    ClaudePty {
        client_seq: u64,
        payload: Value,
    },
    ClaudeSdk {
        input_id: Vec<u8>,
        input: super::claude::sdk_io::ClaudeSdkV1Input,
    },
    #[cfg(unix)]
    Codex {
        input_id: Vec<u8>,
        input: super::codex::io::CodexSdkV1Input,
    },
}

/// An owned structured-input endpoint detached from the session registry lock.
#[async_trait]
pub(crate) trait StructuredInput: Send + Sync {
    async fn send(&self, input: StructuredInputEvent) -> std::result::Result<(), ProtocolError>;
}

/// An owned raw-PTY preparation target detached from the session registry.
pub(crate) enum RawPtyTarget {
    Existing(PtyHandle),
    #[cfg(unix)]
    Codex(CodexRawPtyTarget),
}

/// An owned backend endpoint selected by a closed session protocol.
pub(crate) enum Plane {
    Terminal(RawPtyTarget),
    Structured {
        log: StructuredLogSource,
        input: Box<dyn StructuredInput>,
    },
}

/// Effective configuration provenance frozen when the daemon starts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum McpConfigSource {
    File(PathBuf),
    TrueDefault,
}

/// Immutable daemon-owned route used by every managed agent session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpLaunchRoute {
    executable: PathBuf,
    config_source: McpConfigSource,
    socket_path: PathBuf,
    host_id: Uuid,
}

impl McpLaunchRoute {
    pub(crate) fn for_current_process(config: &Config, host_id: Uuid) -> io::Result<Self> {
        Self::new(
            std::env::current_exe()?,
            config.path.clone(),
            config.socket_path.clone(),
            host_id,
        )
    }

    pub(crate) fn new(
        executable: PathBuf,
        config_path: Option<PathBuf>,
        socket_path: PathBuf,
        host_id: Uuid,
    ) -> io::Result<Self> {
        let config_source = match config_path {
            Some(path) => McpConfigSource::File(path),
            None => McpConfigSource::TrueDefault,
        };
        let route = Self {
            executable,
            config_source,
            socket_path,
            host_id,
        };
        route.validate()?;
        Ok(route)
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        validate_route_path(&self.executable, "amux executable", true)?;
        validate_route_path(&self.socket_path, "daemon socket", false)?;
        if let McpConfigSource::File(path) = &self.config_source {
            validate_route_path(path, "amux config", true)?;
        }
        Ok(())
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn config_path(&self) -> Option<&Path> {
        match &self.config_source {
            McpConfigSource::File(path) => Some(path),
            McpConfigSource::TrueDefault => None,
        }
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    #[cfg(test)]
    pub(crate) fn is_true_default(&self) -> bool {
        matches!(self.config_source, McpConfigSource::TrueDefault)
    }
}

fn validate_route_path(path: &Path, label: &str, must_be_file: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} path must be absolute: {}", path.display()),
        ));
    }
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} path is not valid UTF-8: {}", path.display()),
        )
    })?;
    if must_be_file && !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{label} path does not exist: {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn mcp_launch_route_for_tests(host_id: Uuid) -> McpLaunchRoute {
    McpLaunchRoute::new(
        std::env::current_exe().expect("test executable path"),
        None,
        std::env::temp_dir().join(format!("amux-test-{host_id}.sock")),
        host_id,
    )
    .expect("test MCP launch route")
}

/// Host-owned resources shared by agent backends.
#[derive(Clone)]
pub(crate) struct AgentDeps {
    pub(crate) runtime_dir: std::path::PathBuf,
    pub(crate) claude_version_cache: ClaudeVersionCache,
    #[cfg(unix)]
    pub(crate) codex_client: Arc<CodexClient>,
    pub(crate) mcp_launch_route: McpLaunchRoute,
}

impl AgentDeps {
    pub(crate) fn new(
        runtime_dir: std::path::PathBuf,
        codex_private_socket: std::path::PathBuf,
        mcp_launch_route: McpLaunchRoute,
    ) -> Self {
        #[cfg(not(unix))]
        let _ = codex_private_socket;
        Self {
            runtime_dir,
            claude_version_cache: ClaudeVersionCache::default(),
            #[cfg(unix)]
            codex_client: Arc::new(CodexClient::new(codex_private_socket)),
            mcp_launch_route,
        }
    }

    /// Finish the daemon's one Claude version probe before taking the registry
    /// write lock for this process start.
    pub(crate) async fn for_claude_spawn(self) -> Self {
        self.claude_version_cache.probe_once().await;
        self
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
    fn kind(&self) -> AgentKind;
    fn plane(&self, protocol: Protocol) -> std::result::Result<Plane, ProtocolError>;

    fn spawn_inheritance(&self) -> SpawnInheritance {
        SpawnInheritance::default()
    }

    fn parent(&self) -> Option<AgentParent> {
        None
    }

    /// Snapshot the smallest owned target needed to deliver a message.
    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(UnsupportedAgentDelivery {
            provider: self.kind().provider(),
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
            kind: self.kind(),
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

/// Unified agent session handle backed by dynamic trait dispatch.
pub(crate) type AgentSession = Box<dyn AgentBackend>;

pub(crate) fn new_agent(req: &CreateAgentRequest, deps: &AgentDeps) -> Result<AgentSession> {
    match &req.agent_type {
        AgentType::Claude {
            driver: ClaudeDriver::Pty,
        } => Ok(Box::new(ClaudeSession::new(
            req,
            deps.runtime_dir.clone(),
            deps.claude_version_cache.clone(),
            deps.mcp_launch_route.clone(),
        ))),
        AgentType::Claude {
            driver: ClaudeDriver::Sdk,
        } => Ok(Box::new(ClaudeSdkBackend::new(
            req,
            deps.mcp_launch_route.clone(),
        ))),
        #[cfg(unix)]
        AgentType::Codex { .. } => Ok(Box::new(CodexBackend::new(
            req,
            deps.codex_client.clone(),
            deps.mcp_launch_route.clone(),
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
            driver,
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
                agent_type: AgentType::Claude { driver },
                working_dir,
                terminal_size,
                args,
                parent,
                initial_prompt: None,
            };
            match driver {
                ClaudeDriver::Pty => Box::new(ClaudeSession::from_suspended(
                    &req,
                    name_source.into(),
                    session_id,
                    created_at,
                    deps.runtime_dir.clone(),
                    deps.claude_version_cache.clone(),
                    deps.mcp_launch_route.clone(),
                )),
                ClaudeDriver::Sdk => Box::new(ClaudeSdkBackend::from_suspended(
                    &req,
                    name_source.into(),
                    session_id,
                    created_at,
                    deps.mcp_launch_route.clone(),
                )),
            }
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
            Box::new(CodexBackend::from_suspended(
                &req,
                deps.codex_client.clone(),
                deps.mcp_launch_route.clone(),
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
            parent,
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
                parent,
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

    #[test]
    fn managed_mcp_route_requires_absolute_existing_launch_facts() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("amux");
        let config = dir.path().join("amux.yaml");
        let socket = dir.path().join("amux.sock");
        std::fs::write(&executable, b"test executable").unwrap();
        std::fs::write(&config, b"host_name: test\n").unwrap();
        let host_id = Uuid::from_u128(50);

        let route = McpLaunchRoute::new(
            executable.clone(),
            Some(config.clone()),
            socket.clone(),
            host_id,
        )
        .unwrap();
        assert_eq!(route.executable(), executable);
        assert_eq!(route.config_path(), Some(config.as_path()));
        assert_eq!(route.socket_path(), socket);
        assert_eq!(route.host_id(), host_id);
        assert!(!route.is_true_default());

        std::fs::write(&executable, b"replacement executable bytes").unwrap();
        route
            .validate()
            .expect("route validity is based on the exact path, not a content or build pin");

        assert!(
            McpLaunchRoute::new(
                PathBuf::from("relative-amux"),
                None,
                socket.clone(),
                host_id,
            )
            .is_err()
        );
        assert!(
            McpLaunchRoute::new(
                executable.clone(),
                None,
                PathBuf::from("relative.sock"),
                host_id,
            )
            .is_err()
        );

        std::fs::remove_file(&executable).unwrap();
        assert!(route.validate().is_err());
    }

    #[test]
    fn managed_mcp_route_tags_the_true_default_config_explicitly() {
        let route = mcp_launch_route_for_tests(Uuid::from_u128(51));
        assert!(route.is_true_default());
        assert_eq!(route.config_path(), None);
    }

    #[tokio::test]
    async fn test_agent_refuses_structured_protocols() {
        let session = TestAgentSession::echo_for_tests(Uuid::new_v4(), None);
        assert!(matches!(
            session.plane(Protocol::ClaudePtyTranscriptV1),
            Err(ProtocolError::NotExposed {
                kind: AgentKind::TestAgent,
                protocol: Protocol::ClaudePtyTranscriptV1,
            })
        ));
    }

    #[test]
    fn suspended_claude_into_session_filters_resume_unsafe_args() {
        let sa = SuspendedAgent::Claude {
            driver: ClaudeDriver::Sdk,
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
            mcp_launch_route_for_tests(Uuid::new_v4()),
        );
        let session = agent_from_suspended(sa, &deps);

        assert_eq!(
            session.kind(),
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            }
        );

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
            mcp_launch_route_for_tests(Uuid::new_v4()),
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
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Pty,
            },
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
        let session = ClaudeSession::new(
            &req,
            std::env::temp_dir(),
            ClaudeVersionCache::default(),
            mcp_launch_route_for_tests(Uuid::new_v4()),
        );
        session.set_session_id_for_tests(Uuid::new_v4());

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
