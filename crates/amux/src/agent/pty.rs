use super::StructuredLogSource;
use crate::buffer::{MultiplexByteBuffer, MultiplexByteReader};
use crate::protocol::message::TerminalSize;
use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::Instrument;
use uuid::Uuid;

/// Maximum replay buffer size for PTY bytes.
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

/// PTY I/O handle — input, output subscription, resize.
pub(crate) struct PtyHandle {
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
            .map_err(|_| anyhow!("session closed"))
    }

    /// Subscribe to PTY output (replay + live).
    ///
    /// Returns `None` if the session has ended.
    pub(crate) async fn subscribe(&self) -> Option<MultiplexByteReader> {
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
                    .context("failed to resize pty")?;
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
/// Used by both [`super::ClaudeSession`] and [`super::TestAgentSession`].
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
        .with_context(|| format!("failed to open PTY for '{command}'"))?;

    let mut cmd = CommandBuilder::new(command);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.cwd(working_dir);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to spawn '{command}'"))?;
    drop(pair.slave);

    let master = pair.master;
    let mut pty_reader = master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let mut pty_writer = master.take_writer().context("failed to open PTY writer")?;

    let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> = Arc::new(Mutex::new(Some(master)));
    let current_size: Arc<Mutex<(u16, u16)>> = Arc::new(Mutex::new((size.rows, size.cols)));
    let buffer = Arc::new(MultiplexByteBuffer::new(MAX_REPLAY_BUFFER));
    let log_source = StructuredLogSource::new();
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);

    // Task: Read from PTY, write to multiplex buffer.
    let buffer_clone = buffer.clone();
    let span = session_span.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = span.enter();
        let rt = tokio::runtime::Handle::current();
        let mut read_buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut read_buf) {
                Ok(0) => break,
                Ok(size) => {
                    rt.block_on(buffer_clone.write(read_buf[..size].to_vec()));
                }
                Err(_) => break,
            }
        }
        tracing::debug!("pty reader ended");
    });

    // Task: Forward input to PTY.
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

    // Task: Wait for child to exit, then clean up (server monitors this handle).
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
            {
                let mut master = master_clone.lock().await;
                master.take();
            }

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
