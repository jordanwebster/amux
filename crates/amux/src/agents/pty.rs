use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{Mutex, mpsc};
use tracing::Instrument;
use uuid::Uuid;

use super::StructuredLogSource;
use crate::agents::{ByteReplayQuery, MultiplexByteBuffer, MultiplexByteReader, TerminalSize};

/// Maximum replay buffer size for PTY bytes.
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

/// PTY I/O handle — input, output subscription, resize.
#[derive(Clone)]
pub(crate) struct PtyHandle {
    input_tx: mpsc::Sender<Vec<u8>>,
    pty_master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    current_size: Arc<Mutex<(u16, u16)>>,
    buffer: Arc<MultiplexByteBuffer>,
}

impl PtyHandle {
    #[cfg(any(test, feature = "testnet"))]
    pub(crate) fn test_echo() -> Self {
        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
        let buffer = Arc::new(MultiplexByteBuffer::new(MAX_REPLAY_BUFFER));
        let echo_buffer = buffer.clone();
        tokio::spawn(async move {
            while let Some(data) = input_rx.recv().await {
                echo_buffer.write(data).await;
            }
            echo_buffer.close().await;
        });

        Self {
            input_tx,
            pty_master: Arc::new(Mutex::new(None)),
            current_size: Arc::new(Mutex::new((24, 80))),
            buffer,
        }
    }

    /// Send raw input bytes to the PTY.
    pub(crate) async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .await
            .map_err(|_| anyhow!("session closed"))
    }

    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<ByteReplayQuery>,
    ) -> Option<MultiplexByteReader> {
        self.buffer.subscribe_with_query(query).await
    }

    /// Resize the PTY.
    pub(crate) async fn resize(&self, size: TerminalSize) -> Result<()> {
        let mut current = self.current_size.lock().await;
        if *current != (size.rows, size.cols) {
            let master_guard = self.pty_master.lock().await;
            if let Some(master) = master_guard.as_ref() {
                master
                    .resize(PtySize {
                        rows: size.rows,
                        cols: size.cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("failed to resize pty")?;
                tracing::debug!(cols = size.cols, rows = size.rows, "pty resized");
                *current = (size.rows, size.cols);
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

/// Apply environment additions and removals to a spawn command.
///
/// `CommandBuilder` inherits the daemon's full environment by default;
/// `env_remove` scrubs inherited variables that must not reach the child
/// (see `ClaudeSession::start` for the Claude scrub list and its rationale).
pub(in crate::agents) fn apply_env(
    cmd: &mut CommandBuilder,
    env: &[(&str, String)],
    env_remove: &[&str],
) {
    for key in env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
}

/// Spawn a PTY process and return a handle + structured log source + exit handle.
///
/// Creates the PTY, spawns the command, and starts reader/writer/exit-monitor
/// tasks. The exit handle completes when the child exits (after internal cleanup).
/// The spawned environment is the daemon's environment minus `env_remove` plus
/// `env`. Used by both [`super::ClaudeSession`] and [`super::TestAgentSession`].
pub(crate) fn spawn_pty_agent(
    agent_id: Uuid,
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(&str, String)],
    env_remove: &[&str],
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
    apply_env(&mut cmd, env, env_remove);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to spawn '{command}'"))?;
    // Close the slave pty handle in the parent so EOF propagates to the child on exit.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `apply_env` scrubs inherited variables and applies additions on top —
    /// the seam every PTY agent spawn goes through.
    #[test]
    fn apply_env_removes_inherited_vars_and_sets_additions() {
        let mut cmd = CommandBuilder::new("some-agent");
        // Simulate an inherited (daemon) environment carrying a poisoned var.
        cmd.env("POISONED_INHERITED_VAR", "1");
        cmd.env("UNRELATED_VAR", "keep");

        apply_env(
            &mut cmd,
            &[("ADDED_VAR", "added".to_string())],
            &["POISONED_INHERITED_VAR", "NEVER_SET_VAR"],
        );

        assert_eq!(cmd.get_env("POISONED_INHERITED_VAR"), None);
        assert_eq!(
            cmd.get_env("UNRELATED_VAR").and_then(|v| v.to_str()),
            Some("keep")
        );
        assert_eq!(
            cmd.get_env("ADDED_VAR").and_then(|v| v.to_str()),
            Some("added")
        );
    }
}
