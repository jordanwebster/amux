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
use crate::claude::types::{Hook, StructuredInput};
use crate::error::{AmuxError, Result};
use crate::message::TerminalSize;
use crate::route::Route;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::Instrument;
use uuid::Uuid;

/// Maximum replay buffer size for PTY bytes
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

/// Event sent when a session ends
#[derive(Clone)]
pub enum SessionEvent {
    /// Session ended (agent exited)
    Ended { agent_id: Uuid, user_id: Uuid },
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

/// Spawn a PTY process and return a handle + structured log source.
///
/// Creates the PTY, spawns the command, and starts reader/writer/exit-monitor
/// tasks. Used by both [`ClaudeSession`] and [`TestAgentSession`].
pub(crate) fn spawn_pty_agent(
    agent_id: Uuid,
    command: &str,
    working_dir: &Path,
    env: &[(&str, String)],
    terminal_size: Option<TerminalSize>,
    event_tx: mpsc::Sender<SessionEvent>,
    user_id: Uuid,
) -> Result<(PtyHandle, StructuredLogSource)> {
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

    // Task: Wait for child to exit, then clean up and notify server
    let master_clone = master.clone();
    let buffer_clone = buffer.clone();
    let log_buffer_clone = log_source.buffer().clone();
    let span = session_span;
    tokio::task::spawn_blocking(move || {
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
            log_buffer_clone.close().await;

            // Notify server
            let _ = event_tx
                .send(SessionEvent::Ended { agent_id, user_id })
                .await;
        });
    });

    let pty = PtyHandle {
        input_tx,
        pty_master: master,
        current_size,
        buffer,
    };

    Ok((pty, log_source))
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

    /// Start the agent process (two-phase init: new() stores metadata, start() spawns).
    pub fn start(&mut self) -> Result<()> {
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

    /// Send structured input to the agent.
    pub async fn send_input(&self, input: StructuredInput) -> Result<()> {
        match self {
            Self::Claude(s) => s.send_input(input).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => Ok(()),
        }
    }

    /// Handle a hook event for this agent.
    pub async fn handle_hook(&self, hook: Hook) -> Result<()> {
        match self {
            Self::Claude(s) => s.handle_hook(hook).await,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent(_) => Ok(()),
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

    /// Convert to Agent for listing/registry.
    pub fn to_agent(&self) -> Agent {
        Agent {
            id: self.agent_id(),
            name: self.name().map(String::from),
            command: self.command().to_string(),
            working_dir: self.working_dir().to_path_buf(),
            route: Route::empty(),
        }
    }
}
