use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::CodexConfig;
use crate::dispatch::ServerInner;

// ── JSON-RPC wire types ──────────────────────────────────────────

/// A raw JSON-RPC message (no "jsonrpc" field — codex variant).
#[derive(Debug, Deserialize)]
pub(crate) struct RawMessage {
    /// Present on requests and responses.
    pub id: Option<serde_json::Value>,
    /// Present on requests and notifications.
    pub method: Option<String>,
    /// Present on successful responses.
    pub result: Option<serde_json::Value>,
    /// Present on error responses.
    pub error: Option<RpcError>,
    /// Present on requests and notifications.
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingRequest {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub(crate) struct OutgoingNotification {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingResponse {
    pub id: crate::approval::RequestId,
    pub result: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingErrorResponse {
    pub id: crate::approval::RequestId,
    pub error: RpcError,
}

// ── Process spawning ─────────────────────────────────────────────

pub(crate) fn spawn_process(
    config: &CodexConfig,
) -> Result<(Child, ChildStdin, BufReader<ChildStdout>, ChildStderr)> {
    let program = config
        .codex_path
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "codex".into());

    let mut cmd = tokio::process::Command::new(&program);

    // Add --config overrides
    for (key, value) in &config.config_overrides {
        cmd.arg("--config");
        cmd.arg(format!("{key}={value}"));
    }

    cmd.arg("app-server");
    cmd.arg("--listen");
    cmd.arg("stdio://");

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    if let Some(ref env_map) = config.env {
        for (k, v) in env_map {
            cmd.env(k, v);
        }
    }

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    configure_process_group(&mut cmd);

    let mut child = cmd.spawn().context("failed to spawn codex app-server")?;

    let stdin = child
        .stdin
        .take()
        .context("failed to capture codex stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture codex stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture codex stderr")?;

    Ok((child, stdin, BufReader::new(stdout), stderr))
}

// ── Background tasks ─────────────────────────────────────────────

/// Spawn the reader task that reads lines from stdout and dispatches them.
pub(crate) fn spawn_reader_task(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    inner: Arc<ServerInner>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reader_loop(reader, inner, cancel).await;
    })
}

async fn reader_loop(
    mut reader: impl AsyncBufRead + Unpin,
    inner: Arc<ServerInner>,
    cancel: CancellationToken,
) {
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        inner.dispatch_line(trimmed).await;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Drop all thread channel senders so TurnStreams see end-of-stream,
    // and resolve all pending requests so callers don't hang.
    inner.close_thread_channels().await;
    let pending = std::mem::take(&mut *inner.pending_requests.lock().await);
    drop(pending);
}

/// Spawn the writer task that drains the write channel to stdin.
pub(crate) fn spawn_writer_task(
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    mut rx: mpsc::Receiver<Vec<u8>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                        Some(data) => {
                            if stdin.write_all(&data).await.is_err() {
                                break;
                            }
                            if stdin.write_all(b"\n").await.is_err() {
                                break;
                            }
                            if stdin.flush().await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        drop(stdin);
    })
}

/// Spawn a task that waits for the child process to exit.
pub(crate) fn spawn_child_waiter(
    mut child: Child,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => {
                terminate_child_group(&mut child).await;
            }
            _ = child.wait() => {}
        }
    })
}

/// Forward child stderr one bounded line at a time.
pub(crate) fn spawn_stderr_task(
    stderr: impl AsyncRead + Unpin + Send + 'static,
    process: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        stderr_loop(stderr, process).await;
    })
}

const STDERR_CHUNK_BYTES: usize = 4096;
const STDERR_LINE_BYTES: usize = 16 * 1024;

async fn stderr_loop(mut stderr: impl AsyncRead + Unpin, process: &'static str) {
    let mut chunk = [0_u8; STDERR_CHUNK_BYTES];
    let mut line = Vec::with_capacity(STDERR_CHUNK_BYTES);
    let mut truncated = false;

    loop {
        let read = match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tracing::debug!(%error, process, "failed reading child stderr");
                return;
            }
        };

        for byte in &chunk[..read] {
            if *byte == b'\n' {
                trace_stderr_line(process, &line, truncated);
                line.clear();
                truncated = false;
            } else if line.len() < STDERR_LINE_BYTES {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }

    if !line.is_empty() || truncated {
        trace_stderr_line(process, &line, truncated);
    }
}

fn trace_stderr_line(process: &'static str, line: &[u8], truncated: bool) {
    let line = String::from_utf8_lossy(line);
    tracing::debug!(process, truncated, line = %line, "child stderr");
}

pub(crate) fn configure_process_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
}

pub(crate) async fn terminate_child_group(child: &mut Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        use std::time::Duration;

        if let Some(id) = child.id() {
            let group = Pid::from_raw(id as i32);
            let _ = killpg(group, Signal::SIGTERM);
            if tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                let _ = killpg(group, Signal::SIGKILL);
                let _ = child.wait().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
