//! Agent session abstraction: lifecycle management decoupled from PTY details.
//!
//! [`AgentSession`] is the agent handle enum dispatching to concrete session types
//! ([`ClaudeSession`], [`TestAgentSession`]). [`PtyHandle`] encapsulates PTY I/O
//! (input, output subscription, resize). [`spawn_pty_agent`] is the shared helper
//! that creates the PTY, spawns reader/writer/exit-monitor tasks, and returns a
//! `PtyHandle` + `StructuredLogSource`.

mod claude;
#[cfg(any(debug_assertions, test))]
mod testagent;

pub use claude::ClaudeSession;
#[cfg(any(debug_assertions, test))]
pub use testagent::TestAgentSession;

use crate::agent_registry::Agent;
use crate::buffer::{MultiplexByteBuffer, MultiplexByteReader, MultiplexStructuredReader};
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{AgentStructuredInput, Hook};
use crate::error::{AmuxError, Result};
use crate::message::{AgentType, CreateAgentRequest, ProtocolError, TerminalSize};
use crate::route::Route;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::Instrument;
use uuid::Uuid;

/// Maximum replay buffer size for PTY bytes
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

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
    pub async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .await
            .map_err(|_| AmuxError::Pty("session closed".to_string()))
    }

    /// Subscribe to PTY output (replay + live).
    ///
    /// Returns `None` if the session has ended.
    pub async fn subscribe(&self) -> Option<MultiplexByteReader> {
        self.buffer.subscribe().await
    }

    /// Resize the PTY.
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<()> {
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
                    .map_err(|e| AmuxError::Pty(format!("failed to resize pty: {e}")))?;
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
        .map_err(|e| AmuxError::Pty(format!("failed to open pty: {e}")))?;

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
        .map_err(|e| AmuxError::Pty(format!("failed to spawn '{command}': {e}")))?;
    drop(pair.slave);

    let master = pair.master;
    let mut pty_reader = master
        .try_clone_reader()
        .map_err(|e| AmuxError::Pty(format!("failed to clone pty reader: {e}")))?;
    let mut pty_writer = master
        .take_writer()
        .map_err(|e| AmuxError::Pty(format!("failed to open pty writer: {e}")))?;

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
    fn structured_input_type_label(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude",
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => "no structured input",
        }
    }

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
    pub fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
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
            Self::TestAgent(_) => {}
        }
    }

    pub fn log_source(&self) -> Option<StructuredLogSource> {
        match self {
            Self::Claude(s) => s.log_source(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.log_source(),
        }
    }

    /// Subscribe to structured log output and return the matching seq.
    pub async fn subscribe_with_current_seq(&self) -> Option<(MultiplexStructuredReader, u64)> {
        match self {
            Self::Claude(s) => s.subscribe_with_current_seq().await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(s) => s.subscribe_with_current_seq().await,
        }
    }

    /// Validate seq and send structured input to the agent.
    pub async fn send_structured_input(
        &self,
        client_seq: u64,
        input: AgentStructuredInput,
    ) -> std::result::Result<(), ProtocolError> {
        match (self, input) {
            (Self::Claude(s), AgentStructuredInput::Claude(input)) => {
                s.send_structured_input(client_seq, input).await
            }
            #[cfg(any(debug_assertions, test))]
            (Self::TestAgent(_), input) => Err(ProtocolError::StructuredInputTypeMismatch {
                expected: self.structured_input_type_label().to_string(),
                received: input.type_label().to_string(),
            }),
            #[allow(unreachable_patterns)]
            (session, input) => Err(ProtocolError::StructuredInputTypeMismatch {
                expected: session.structured_input_type_label().to_string(),
                received: input.type_label().to_string(),
            }),
        }
    }

    /// Handle a hook event for this agent.
    pub async fn handle_hook(&mut self, hook: Hook, source_ppid: Option<u32>) -> Result<()> {
        match self {
            Self::Claude(s) => s.handle_hook(hook, source_ppid).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => Ok(()),
        }
    }

    pub fn external_pid(&self) -> Option<u32> {
        match self {
            Self::Claude(s) => s.external_pid(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => None,
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

    /// Convert to Agent for listing/registry.
    pub fn to_agent(&self) -> Agent {
        Agent {
            id: self.agent_id(),
            name: self.name().map(String::from),
            command: self.command().to_string(),
            working_dir: self.working_dir().to_path_buf(),
            route: Route::empty(),
            agent_type: match self {
                Self::Claude(_) => AgentType::Claude,
                #[cfg(any(debug_assertions, test))]
                Self::TestAgent(s) => AgentType::TestAgent(s.command.clone()),
            },
            readonly: self.readonly(),
        }
    }

    /// Suspend this session: stop the agent and return serializable state.
    /// Consumes self.
    pub async fn suspend(self) -> Result<SuspendedAgent> {
        match self {
            Self::Claude(s) => {
                s.stop(StopPolicy::Interrupt).await;
                let name_source = s.name_source();
                let session_id = s.session_id.ok_or_else(|| {
                    AmuxError::ServerError(format!(
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
                })
            }
        }
    }
}

/// All suspended agent sessions, serialized to disk across server restarts.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SuspendedServerState {
    pub agents: Vec<SuspendedAgent>,
}

/// Serializable representation of a suspended agent session.
/// Used to persist agent state across server restarts (e.g., during `amux update`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SuspendedAgent {
    Claude {
        agent_id: Uuid,
        name: Option<String>,
        name_source: LocalAgentNameSource,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
        session_id: Uuid,
    },
    #[cfg(any(debug_assertions, test))]
    TestAgent {
        agent_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
    },
}

impl SuspendedAgent {
    /// Get the agent_id from the suspended agent.
    pub fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude { agent_id, .. } => *agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { agent_id, .. } => *agent_id,
        }
    }

    /// Get the name from the suspended agent.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Claude { name, .. } => name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { name, .. } => name.as_deref(),
        }
    }

    /// Create an unstarted AgentSession from this suspended agent.
    /// For Claude agents, sets `session_id` so `start()` passes `--resume`.
    /// Caller must call `session.start()` to spawn the process.
    pub fn into_session(self) -> AgentSession {
        match self {
            Self::Claude {
                agent_id,
                name,
                name_source,
                working_dir,
                terminal_size,
                session_id,
            } => {
                let req = CreateAgentRequest {
                    agent_id,
                    name,
                    agent_type: crate::message::AgentType::Claude,
                    working_dir,
                    terminal_size,
                    args: vec![],
                };
                let mut session = ClaudeSession::new(&req);
                session.session_id = Some(session_id);
                session.set_name_and_source(req.name, name_source);
                AgentSession::Claude(session)
            }
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent {
                agent_id,
                name,
                command,
                working_dir,
                terminal_size,
            } => {
                let req = CreateAgentRequest {
                    agent_id,
                    name,
                    agent_type: crate::message::AgentType::TestAgent(command.clone()),
                    working_dir,
                    terminal_size,
                    args: vec![],
                };
                AgentSession::TestAgent(TestAgentSession::new(&req, command))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::{AgentStructuredInput, ClaudeStructuredInput};
    use crate::message::AgentType;

    #[tokio::test]
    #[cfg(any(debug_assertions, test))]
    async fn test_test_agent_rejects_claude_structured_input_with_type_mismatch() {
        let req = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            name: Some("test".to_string()),
            agent_type: AgentType::TestAgent("test-agent".to_string()),
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![],
        };
        let session =
            AgentSession::TestAgent(TestAgentSession::new(&req, "test-agent".to_string()));

        let err = session
            .send_structured_input(
                0,
                AgentStructuredInput::Claude(ClaudeStructuredInput::SubmitMessage {
                    data: b"hello".to_vec(),
                }),
            )
            .await
            .unwrap_err();

        assert_eq!(
            err,
            ProtocolError::StructuredInputTypeMismatch {
                expected: "no structured input".to_string(),
                received: "Claude".to_string(),
            }
        );
    }
}
