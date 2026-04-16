//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentSession`] is the agent handle enum dispatching to concrete session types
//! ([`ClaudeSession`], [`TestAgentSession`]). [`PtyHandle`] encapsulates PTY I/O
//! (input, output subscription, resize). [`spawn_pty_agent`] is the shared helper
//! that creates the PTY, spawns reader/writer/exit-monitor tasks, and returns a
//! `PtyHandle` + `StructuredLogSource`.

pub(crate) mod claude;
#[cfg(any(debug_assertions, test))]
pub(crate) mod test_agent;

pub use claude::ClaudeSession;
#[cfg(any(debug_assertions, test))]
pub use test_agent::TestAgentSession;

use crate::agent::claude::log_source::StructuredLogSource;
use crate::buffer::{MultiplexByteBuffer, MultiplexByteReader, MultiplexStructuredReader};
use crate::protocol::message::{
    AgentType, HookProvider, ProtocolError, SubscribeQuery, TerminalSize,
};
use crate::protocol::route::Route;
use crate::suspend::SuspendedAgent;
use chrono::{DateTime, Utc};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};
use tracing::Instrument;
use uuid::Uuid;

/// Maximum replay buffer size for PTY bytes
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Error)]
pub(crate) enum AgentError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("{0}")]
    InvalidState(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookOutcome {
    Noop,
    KeepSession,
    WithdrawSession,
}

pub(crate) enum ExternalHookBootstrap {
    Noop,
    Register(AgentSession),
}

#[derive(Debug, Error)]
pub(crate) enum HookError {
    #[error("hook provider mismatch: expected {expected:?}, got {actual:?}")]
    ProviderMismatch {
        expected: HookProvider,
        actual: HookProvider,
    },
    #[error("unsupported hook provider: {0:?}")]
    UnsupportedProvider(HookProvider),
    #[error("invalid {provider:?} hook payload: {message}")]
    InvalidPayload {
        provider: HookProvider,
        message: String,
    },
    #[error("external {provider:?} hook missing required field '{field}'")]
    MissingBootstrapField {
        provider: HookProvider,
        field: &'static str,
    },
    #[error("failed to handle {provider:?} hook: {message}")]
    Handling {
        provider: HookProvider,
        message: String,
    },
}

impl HookError {
    pub(crate) fn into_protocol_error(self) -> ProtocolError {
        ProtocolError::ServerError {
            message: self.to_string(),
        }
    }
}

/// Internal agent metadata owned by the runtime.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Agent {
    pub id: Uuid,
    pub host_id: Uuid,
    pub name: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub route: Route,
    pub agent_type: String,
    pub structured_protocol: Option<String>,
    pub readonly: bool,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl Agent {
    pub fn is_remote(&self) -> bool {
        self.route.peek().is_some()
    }
}

/// Local-only provenance for an agent session's current display name.
///
/// This is used to decide whether provider-derived candidates may rename a
/// local session. It is never sent over the peer protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAgentNameSource {
    Unset,
    Amux,
    ProviderName,
    ProviderSlug,
}

impl LocalAgentNameSource {
    /// Whether this source can be overridden by automatic name discovery.
    pub fn is_automatic(self) -> bool {
        !matches!(self, Self::Amux)
    }

    /// Precedence rank among automatic sources. Higher wins.
    /// Only valid for automatic sources — panics for `Amux`.
    pub fn rank(self) -> u8 {
        match self {
            Self::Unset => 0,
            Self::ProviderSlug => 1,
            Self::ProviderName => 2,
            Self::Amux => unreachable!("check is_automatic() before calling rank()"),
        }
    }
}

/// Events sent from agent sessions to the server event loop
#[derive(Clone)]
pub enum SessionEvent {
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
pub enum StopPolicy {
    /// Send interrupt signal (close PTY master)
    Interrupt,
}

/// PTY I/O handle — input, output subscription, resize.
pub struct PtyHandle {
    input_tx: mpsc::Sender<Vec<u8>>,
    pty_master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    current_size: Arc<Mutex<(u16, u16)>>,
    buffer: Arc<MultiplexByteBuffer>,
}

impl PtyHandle {
    /// Send raw input bytes to the PTY.
    pub(crate) async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .await
            .map_err(|_| AgentError::Pty("session closed".to_string()))
    }

    /// Subscribe to PTY output (replay + live).
    ///
    /// Returns `None` if the session has ended.
    pub async fn subscribe(&self) -> Option<MultiplexByteReader> {
        self.buffer.subscribe().await
    }

    /// Resize the PTY.
    pub(crate) async fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let mut current = self.current_size.lock().await;
        if *current != (rows, cols) {
            let master_guard = self.pty_master.lock().await;
            if let Some(master) = master_guard.as_ref() {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| AgentError::Pty(format!("failed to resize pty: {e}")))?;
                tracing::debug!(cols, rows, "pty resized");
                *current = (rows, cols);
            }
        }
        Ok(())
    }

    /// Close the PTY master and output buffer.
    pub(crate) async fn close(&self) {
        self.pty_master.lock().await.take();
        self.buffer.close().await;
    }
}

/// Spawn a PTY process and return a handle + structured log source + exit handle.
///
/// Creates the PTY, spawns the command, and starts reader/writer/exit-monitor
/// tasks. The exit handle completes when the child exits (after internal cleanup).
/// Used by both [`ClaudeSession`] and [`TestAgentSession`].
pub(crate) fn spawn_pty_agent(
    agent_id: Uuid,
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(&str, String)],
    terminal_size: Option<TerminalSize>,
) -> Result<(PtyHandle, StructuredLogSource, tokio::task::JoinHandle<()>)> {
    let session_span = tracing::info_span!("session", agent_id = %agent_id, command = %command);
    tracing::info!(parent: &session_span, dir = %working_dir.display(), "creating session");

    let pty_system = native_pty_system();
    let size = terminal_size.unwrap_or_default();
    let pair = pty_system
        .openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AgentError::Pty(format!("failed to open pty: {e}")))?;

    let mut cmd = CommandBuilder::new(command);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.cwd(working_dir);
    for (key, val) in env {
        cmd.env(key, val);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| AgentError::Pty(format!("failed to spawn '{command}': {e}")))?;
    drop(pair.slave);

    let master = pair.master;
    let mut pty_reader = master
        .try_clone_reader()
        .map_err(|e| AgentError::Pty(format!("failed to clone pty reader: {e}")))?;
    let mut pty_writer = master
        .take_writer()
        .map_err(|e| AgentError::Pty(format!("failed to open pty writer: {e}")))?;

    let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> = Arc::new(Mutex::new(Some(master)));
    let current_size: Arc<Mutex<(u16, u16)>> = Arc::new(Mutex::new((size.rows, size.cols)));
    let buffer = Arc::new(MultiplexByteBuffer::new(MAX_REPLAY_BUFFER));
    let log_source = StructuredLogSource::new();
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);

    // Task: Read from PTY, write to multiplex buffer
    let buffer_clone = buffer.clone();
    let span = session_span.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = span.enter();
        let rt = tokio::runtime::Handle::current();
        let mut read_buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => {
                    rt.block_on(buffer_clone.write(read_buf[..n].to_vec()));
                }
                Err(_) => break,
            }
        }
        tracing::debug!("pty reader ended");
    });

    // Task: Forward input to PTY
    tokio::spawn(
        async move {
            while let Some(data) = input_rx.recv().await {
                if pty_writer.write_all(&data).is_err() {
                    break;
                }
                let _ = pty_writer.flush();
            }
            tracing::debug!("pty writer ended");
        }
        .instrument(session_span.clone()),
    );

    // Task: Wait for child to exit, then clean up (server monitors this handle)
    let master_clone = master.clone();
    let buffer_clone = buffer.clone();
    let log_source_clone = log_source.clone();
    let span = session_span;
    let exit_handle = tokio::task::spawn_blocking(move || {
        let _guard = span.enter();
        let status = child.wait();
        tracing::info!(?status, "agent exited");

        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            // Drop the PTY master to kill any remaining processes
            {
                let mut master = master_clone.lock().await;
                master.take();
            }

            // Close the multiplex buffers to disconnect all clients
            buffer_clone.close().await;
            log_source_clone.close().await;
        });
    });

    let pty = PtyHandle {
        input_tx,
        pty_master: master,
        current_size,
        buffer,
    };

    Ok((pty, log_source, exit_handle))
}

/// Unified agent session handle, dispatching to concrete session types.
pub enum AgentSession {
    Claude(ClaudeSession),
    #[cfg(any(debug_assertions, test))]
    TestAgent(TestAgentSession),
}

impl AgentSession {
    pub fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude(s) => s.agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.agent_id,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Claude(s) => s.name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.name.as_deref(),
        }
    }

    pub fn command(&self) -> &str {
        match self {
            Self::Claude(s) => &s.command,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => &s.command,
        }
    }

    pub fn working_dir(&self) -> &Path {
        match self {
            Self::Claude(s) => &s.working_dir,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => &s.working_dir,
        }
    }

    pub fn readonly(&self) -> bool {
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
    pub async fn stop(&self, policy: StopPolicy) {
        match self {
            Self::Claude(s) => s.stop(policy).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.stop().await,
        }
    }

    /// Return the current structured output sequence number.
    pub async fn current_seq(&self) -> u64 {
        match self {
            Self::Claude(s) => s.current_seq().await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.current_seq().await,
        }
    }

    pub fn maybe_start_name_sniffer(
        &mut self,
        user_id: Uuid,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) {
        if let Self::Claude(s) = self {
            s.maybe_start_name_sniffer(user_id, event_tx);
        }
    }

    pub fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        match self {
            Self::Claude(s) => Some(s.name_source()),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => None,
        }
    }

    pub fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        match self {
            Self::Claude(s) => s.set_name_and_source(name, source),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.name = name,
        }
    }

    pub fn log_source(&self) -> Option<StructuredLogSource> {
        match self {
            Self::Claude(s) => s.log_source(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.log_source(),
        }
    }

    /// Subscribe to structured log output with an optional query filter
    /// and return the matching seq.
    pub async fn subscribe_with_query(
        &self,
        query: Option<SubscribeQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        match self {
            Self::Claude(s) => s.subscribe_with_query(query).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.subscribe_with_query(query).await,
        }
    }

    pub fn structured_protocol(&self) -> Option<String> {
        match self {
            Self::Claude(_) => Some("claude_pty_v1".to_string()),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => None,
        }
    }

    /// Validate seq and send structured input to the agent.
    pub async fn send_structured_input(
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

    /// Subscribe to structured log output.
    ///
    /// Returns `None` if the log buffer has been closed.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        match self {
            Self::Claude(s) => s.subscribe().await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.subscribe().await,
        }
    }

    /// Get the PTY handle (if this session type has one).
    pub fn get_pty_handle(&self) -> Option<&PtyHandle> {
        match self {
            Self::Claude(s) => s.pty.as_ref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.pty.as_ref(),
        }
    }

    /// Get the terminal size this session was created with.
    pub fn terminal_size(&self) -> Option<TerminalSize> {
        match self {
            Self::Claude(s) => s.terminal_size,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.terminal_size,
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        match self {
            Self::Claude(s) => s.created_at,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.created_at,
        }
    }

    pub fn args(&self) -> &[String] {
        match self {
            Self::Claude(s) => &s.args,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => &[],
        }
    }

    /// Convert to Agent for listing/registry.
    pub fn to_agent(&self, host_id: Uuid) -> Agent {
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

    /// Suspend this session: stop the agent and return serializable state.
    /// Consumes self.
    pub(crate) async fn suspend(self) -> Result<SuspendedAgent> {
        match self {
            Self::Claude(s) => {
                s.stop(StopPolicy::Interrupt).await;
                let name_source = s.name_source();
                let session_id = s.session_id.ok_or_else(|| {
                    AgentError::InvalidState(format!(
                        "cannot suspend claude agent {}: no session_id (SessionStart hook not received)",
                        s.agent_id
                    ))
                })?;
                Ok(SuspendedAgent::Claude {
                    agent_id: s.agent_id,
                    name: s.name,
                    name_source,
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
            name_source: LocalAgentNameSource::ProviderName,
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

        let session = sa.into_session();

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
