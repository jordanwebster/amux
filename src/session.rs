use crate::buffer::{MultiplexBuffer, MultiplexReader};
use crate::error::{AmuxError, Result};
use crate::message::{AgentInfo, AgentType, CreateAgentRequest};
use crate::multiplex_log_buffer::{MultiplexLogBuffer, MultiplexLogReader};
use crate::transcript::TranscriptTailer;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Maximum replay buffer size for PTY bytes
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

/// Maximum number of structured log entries to keep
const MAX_LOG_ENTRIES: usize = 1000;

/// Event sent when a session ends
#[derive(Clone)]
pub enum SessionEvent {
    /// Session ended (agent exited)
    Ended(Uuid),
}

/// A local agent session with PTY
pub struct LocalAgentSession {
    /// Unique identifier (UUID)
    pub agent_id: Uuid,

    /// Human-readable alias (from -t flag)
    pub alias: Option<String>,

    /// Command used to spawn this agent
    pub command: String,

    /// Working directory where agent was spawned
    pub working_dir: PathBuf,

    /// PTY master (None if closed)
    pty_master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,

    /// Multiplex buffer for PTY output (handles both replay and broadcast)
    buffer: Arc<MultiplexBuffer>,

    /// Multiplex buffer for structured logs (populated by transcript tailer)
    log_buffer: Arc<MultiplexLogBuffer>,

    /// Transcript tailer handle (None if no transcript linked)
    transcript_tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,

    /// Channel to send input to PTY
    input_tx: mpsc::Sender<Vec<u8>>,

    /// Current terminal size
    current_size: Arc<Mutex<(u16, u16)>>,
}

impl LocalAgentSession {
    /// Create a new agent session from a CreateAgentRequest
    pub fn new(req: &CreateAgentRequest, event_tx: mpsc::Sender<SessionEvent>) -> Result<Self> {
        // Build command and args based on agent type
        let (command, args): (String, Vec<String>) = match &req.agent_type {
            AgentType::Claude => (
                "claude".to_string(),
                vec![format!("--session-id={}", req.agent_id)],
            ),
            #[cfg(any(debug_assertions, test))]
            AgentType::TestAgent(cmd) => (cmd.clone(), vec![]),
        };

        log!(
            "session [{}]: creating with command '{}' args={:?} in {:?} ({}x{})",
            req.agent_id,
            command,
            args,
            req.working_dir,
            req.cols,
            req.rows
        );

        // Create PTY
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: req.rows,
                cols: req.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AmuxError::Pty(e.to_string()))?;

        // Spawn command with arguments
        let mut cmd = CommandBuilder::new(&command);
        for arg in &args {
            cmd.arg(arg);
        }
        cmd.cwd(&req.working_dir);
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AmuxError::Pty(e.to_string()))?;
        drop(pair.slave);

        let master = pair.master;
        let mut pty_reader = master
            .try_clone_reader()
            .map_err(|e| AmuxError::Pty(e.to_string()))?;
        let mut pty_writer = master
            .take_writer()
            .map_err(|e| AmuxError::Pty(e.to_string()))?;

        let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(master)));
        let current_size: Arc<Mutex<(u16, u16)>> = Arc::new(Mutex::new((req.rows, req.cols)));
        let buffer = Arc::new(MultiplexBuffer::new(MAX_REPLAY_BUFFER));
        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);

        // Task: Read from PTY, write to multiplex buffer
        let buffer_clone = buffer.clone();
        let session_id = req.agent_id;
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let mut read_buf = [0u8; 4096];
            loop {
                match pty_reader.read(&mut read_buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        rt.block_on(buffer_clone.write(&read_buf[..n]));
                    }
                    Err(_) => break,
                }
            }
            log!("session [{}]: PTY reader ended", session_id);
        });

        // Task: Forward input to PTY
        let session_id = req.agent_id;
        tokio::spawn(async move {
            while let Some(data) = input_rx.recv().await {
                if pty_writer.write_all(&data).is_err() {
                    break;
                }
                let _ = pty_writer.flush();
            }
            log!("session [{}]: PTY writer ended", session_id);
        });

        // Task: Wait for child to exit, then clean up and notify server
        let session_id = req.agent_id;
        let master_clone = master.clone();
        let buffer_clone = buffer.clone();
        tokio::task::spawn_blocking(move || {
            let status = child.wait();
            log!("session [{}]: command exited: {:?}", session_id, status);

            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                // Drop the PTY master to kill any remaining processes
                {
                    let mut master = master_clone.lock().await;
                    master.take();
                    log!("session [{}]: PTY master dropped", session_id);
                }

                // Close the multiplex buffer to disconnect all clients
                buffer_clone.close().await;
                log!("session [{}]: multiplex buffer closed", session_id);

                // Notify server
                let _ = event_tx.send(SessionEvent::Ended(session_id)).await;
            });
        });

        Ok(Self {
            agent_id: req.agent_id,
            alias: req.alias.clone(),
            command: command.to_string(),
            working_dir: req.working_dir.clone(),
            pty_master: master,
            buffer,
            log_buffer: Arc::new(MultiplexLogBuffer::new(MAX_LOG_ENTRIES)),
            transcript_tailer: Mutex::new(None),
            input_tx,
            current_size,
        })
    }

    /// Subscribe to the session.
    ///
    /// Returns a tuple of (MultiplexReader, input_sender) for bidirectional
    /// communication with the agent:
    /// - MultiplexReader receives all existing output (replay) followed by new output
    /// - input_sender sends input bytes to the agent
    ///
    /// This operation is atomic - no data will be lost or duplicated between
    /// the replay and live output.
    ///
    /// Returns `None` if the session has ended.
    pub async fn subscribe(&self) -> Option<(MultiplexReader, mpsc::Sender<Vec<u8>>)> {
        let output = self.buffer.subscribe().await?;
        Some((output, self.input_tx.clone()))
    }

    /// Send input data to the agent
    pub async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .await
            .map_err(|_| AmuxError::Pty("Session closed".to_string()))
    }

    /// Resize the PTY
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
                    .map_err(|e| AmuxError::Pty(e.to_string()))?;
                log!("session [{}]: resized to {}x{}", self.agent_id, cols, rows);
                *current = (rows, cols);
            }
        }
        Ok(())
    }

    /// Shutdown the session
    pub async fn shutdown(&self) {
        log!("session [{}]: shutting down", self.agent_id);
        // Stop transcript tailer if running
        if let Some((tailer, _handle)) = self.transcript_tailer.lock().await.take() {
            tailer.stop();
        }
        // Drop the PTY master to kill any remaining processes
        self.pty_master.lock().await.take();
        // Close the multiplex buffer to disconnect clients
        self.buffer.close().await;
        // Close the log buffer
        self.log_buffer.close().await;
    }

    /// Link a transcript file to this session.
    ///
    /// Starts a background task that tails the transcript file and writes
    /// parsed log entries to the log buffer.
    pub async fn link_transcript(&self, path: PathBuf) {
        log!("session [{}]: linking transcript {:?}", self.agent_id, path);
        let tailer = TranscriptTailer::new(path, self.log_buffer.clone());
        let handle = tailer.start();
        *self.transcript_tailer.lock().await = Some((tailer, handle));
    }

    /// Subscribe to structured logs.
    ///
    /// Returns a reader that receives all existing log entries (replay) followed
    /// by new entries as they are parsed from the transcript.
    ///
    /// Returns `None` if the log buffer has been closed.
    pub async fn subscribe_logs(&self) -> Option<MultiplexLogReader> {
        self.log_buffer.subscribe().await
    }

    /// Write a structured log entry directly (not from transcript).
    /// Used for permission requests and other events that don't come from the transcript.
    pub async fn write_log(&self, entry: crate::structured_log::StructuredLog) {
        self.log_buffer.write(entry).await;
    }

    /// Convert to AgentInfo for listing
    pub fn to_agent_info(&self) -> AgentInfo {
        AgentInfo {
            agent_id: self.agent_id,
            alias: self.alias.clone(),
            command: self.command.clone(),
            working_dir: self.working_dir.clone(),
        }
    }
}
