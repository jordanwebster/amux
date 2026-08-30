use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(test, feature = "testnet"))]
use anyhow::anyhow;
use anyhow::{Context, Result};
#[cfg(any(test, feature = "testnet"))]
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use crate::agents::{ByteReplayQuery, MultiplexByteBuffer, MultiplexByteReader, TerminalSize};

/// Maximum replay buffer size for PTY bytes.
const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB
const TERMINATE_GRACE: Duration = Duration::from_millis(250);

#[derive(Clone)]
enum HostedPty {
    Process(Arc<pty_host::PtyProcess>),
    Claude(claude::pty::Control),
    #[cfg(any(test, feature = "testnet"))]
    TestEcho(mpsc::Sender<Vec<u8>>),
}

/// amux's replaying view over a provider-neutral hosted PTY.
#[derive(Clone)]
pub(crate) struct PtyHandle {
    hosted: HostedPty,
    buffer: Arc<MultiplexByteBuffer>,
}

impl PtyHandle {
    pub(crate) fn from_claude(control: claude::pty::Control) -> Option<Self> {
        let mut output = control.terminal_output()?;
        let buffer = Arc::new(MultiplexByteBuffer::new(MAX_REPLAY_BUFFER));
        let output_buffer = buffer.clone();
        tokio::spawn(async move {
            while let Some(bytes) = output.recv().await {
                output_buffer.write(bytes.to_vec()).await;
            }
            output_buffer.close().await;
        });
        Some(Self {
            hosted: HostedPty::Claude(control),
            buffer,
        })
    }

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
            hosted: HostedPty::TestEcho(input_tx),
            buffer,
        }
    }

    /// Send raw input bytes to the PTY.
    pub(crate) async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        match &self.hosted {
            HostedPty::Process(process) => process.handle.write(&data).await.map_err(Into::into),
            HostedPty::Claude(control) => control
                .send_program(vec![claude::pty::PtyInput::Bytes(data)])
                .await
                .map(|_| ())
                .map_err(Into::into),
            #[cfg(any(test, feature = "testnet"))]
            HostedPty::TestEcho(input_tx) => input_tx
                .send(data)
                .await
                .map_err(|_| anyhow!("session closed")),
        }
    }

    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<ByteReplayQuery>,
    ) -> Option<MultiplexByteReader> {
        self.buffer.subscribe_with_query(query).await
    }

    /// Resize the PTY.
    pub(crate) async fn resize(&self, size: TerminalSize) -> Result<()> {
        match &self.hosted {
            HostedPty::Process(process) => process
                .handle
                .resize(pty_size(size))
                .context("failed to resize pty"),
            HostedPty::Claude(control) => control
                .resize(pty_size(size))
                .context("failed to resize Claude PTY"),
            #[cfg(any(test, feature = "testnet"))]
            HostedPty::TestEcho(_) => Ok(()),
        }
    }

    /// Close amux output and terminate a real hosted process group.
    pub(crate) async fn close(&self) {
        let _ = self.terminate().await;
    }

    /// Terminate the PTY child process group, then close amux output.
    pub(crate) async fn terminate(&self) -> Result<()> {
        match &self.hosted {
            HostedPty::Process(process) => {
                pty_host::terminate(
                    process,
                    pty_host::Terminate::Graceful {
                        grace: TERMINATE_GRACE,
                    },
                )
                .await;
            }
            HostedPty::Claude(control) => {
                control
                    .clone()
                    .stop(pty_host::Terminate::Graceful {
                        grace: TERMINATE_GRACE,
                    })
                    .await;
            }
            #[cfg(any(test, feature = "testnet"))]
            HostedPty::TestEcho(_) => {}
        }
        self.buffer.close().await;
        Ok(())
    }

    /// Signal a real hosted process group synchronously.
    pub(crate) fn signal_process_group(&self, signal: pty_host::ProcessGroupSignal) -> Result<()> {
        match &self.hosted {
            HostedPty::Process(process) => process
                .handle
                .signal_process_group(signal)
                .map_err(Into::into),
            HostedPty::Claude(control) => control
                .terminal()
                .ok_or_else(|| anyhow::anyhow!("Claude session has no live PTY handle"))?
                .signal_process_group(signal)
                .map_err(Into::into),
            #[cfg(any(test, feature = "testnet"))]
            HostedPty::TestEcho(_) => Ok(()),
        }
    }
}

/// Spawn through pty-host and feed its single output stream into amux replay.
pub(crate) fn spawn_pty_agent(
    agent_id: Uuid,
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(&str, String)],
    env_remove: &[&str],
    terminal_size: Option<TerminalSize>,
) -> Result<(PtyHandle, tokio::task::JoinHandle<()>)> {
    let session_span = tracing::info_span!("session", agent_id = %agent_id, command = %command);
    tracing::info!(parent: &session_span, dir = %working_dir.display(), "creating session");

    let process = pty_host::spawn(pty_spawn(
        command,
        args,
        working_dir,
        env,
        env_remove,
        terminal_size,
    ))
    .with_context(|| format!("failed to spawn '{command}'"))?;
    let mut output = process.handle.output();
    let process = Arc::new(process);
    let buffer = Arc::new(MultiplexByteBuffer::new(MAX_REPLAY_BUFFER));

    let output_buffer = buffer.clone();
    let output_span = session_span.clone();
    let output_task = tokio::spawn(
        async move {
            while let Some(bytes) = output.recv().await {
                output_buffer.write(bytes.to_vec()).await;
            }
        }
        .instrument(output_span),
    );

    let exit_process = process.clone();
    let exit_buffer = buffer.clone();
    let exit_handle = tokio::spawn(
        async move {
            let mut exit = exit_process.exit.clone();
            let status = exit.wait().await;
            tracing::info!(?status, "agent exited");
            let _ = output_task.await;
            exit_buffer.close().await;
        }
        .instrument(session_span),
    );

    Ok((
        PtyHandle {
            hosted: HostedPty::Process(process),
            buffer,
        },
        exit_handle,
    ))
}

pub(in crate::agents) fn pty_spawn(
    command: &str,
    args: &[String],
    working_dir: &Path,
    env: &[(&str, String)],
    env_remove: &[&str],
    terminal_size: Option<TerminalSize>,
) -> pty_host::PtySpawn {
    pty_host::PtySpawn {
        command: command.into(),
        args: args.to_vec(),
        cwd: working_dir.to_path_buf(),
        env: env
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        env_remove: env_remove.iter().map(OsString::from).collect(),
        size: pty_size(terminal_size.unwrap_or_default()),
    }
}

fn pty_size(size: TerminalSize) -> pty_host::PtySize {
    pty_host::PtySize {
        rows: size.rows,
        cols: size.cols,
    }
}
