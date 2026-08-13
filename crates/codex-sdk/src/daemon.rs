use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use crate::Codex;
use crate::error::Error;
use crate::transport;

const SOCKET_RELATIVE_PATH: &str = "app-server-control/app-server-control.sock";
const MAX_SOCKET_PATH_BYTES: usize = 103;
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const START_TIMEOUT: Duration = Duration::from_secs(5);
const READY_RETRIES: usize = 60;
const READY_RETRY_DELAY: Duration = Duration::from_millis(50);

/// How an app-server daemon was obtained.
#[derive(Debug)]
pub enum DaemonMode {
    /// A healthy server was already listening on the well-known socket.
    Existing,
    /// The SDK spawned and supervises a server on the well-known socket.
    Spawned(DaemonProcess),
    /// The SDK spawned and supervises a server on a caller-provided socket.
    Private(DaemonProcess),
    /// A healthy server was already listening on the caller-provided socket.
    PrivateExisting(PathBuf),
}

/// A supervised app-server process. Dropping it terminates the entire process group.
pub struct DaemonProcess {
    socket_path: PathBuf,
    process_group: i32,
    running: Arc<AtomicBool>,
    cancel: CancellationToken,
    waiter: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for DaemonProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonProcess")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl DaemonProcess {
    /// The resolved, non-symlink-parent socket path used by this process.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Terminate the process group and wait for it to exit.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.await;
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if self.running.load(Ordering::Acquire) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.process_group),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        self.cancel.cancel();
    }
}

/// Return the conventional control socket below a Codex home directory.
pub fn daemon_socket_path(codex_home: &Path) -> PathBuf {
    codex_home.join(SOCKET_RELATIVE_PATH)
}

/// Connect to the well-known daemon socket and perform the initialize handshake.
pub async fn connect_daemon(codex_home: &Path) -> Result<Codex, Error> {
    let codex_home = tokio::fs::canonicalize(codex_home).await?;
    connect_socket(&daemon_socket_path(&codex_home)).await
}

/// Connect to an app-server using WebSocket text frames over a Unix socket.
pub async fn connect_socket(socket_path: &Path) -> Result<Codex, Error> {
    let stream = UnixStream::connect(socket_path).await?;
    let (websocket, _) = client_async("ws://localhost/rpc", stream)
        .await
        .map_err(|error| Error::WebSocket(error.to_string()))?;

    let (sdk_reader, bridge_inbound) = tokio::io::duplex(64 * 1024);
    let (sdk_writer, bridge_outbound) = tokio::io::duplex(64 * 1024);
    tokio::spawn(websocket_bridge(
        websocket,
        bridge_inbound,
        BufReader::new(bridge_outbound),
    ));

    Codex::from_io(BufReader::new(sdk_reader), sdk_writer).await
}

/// Ensure a healthy server is listening on the well-known control socket.
pub async fn ensure_daemon(codex_home: &Path) -> Result<DaemonMode, Error> {
    ensure_well_known(codex_home).await
}

/// Ensure the well-known daemon, falling back to a caller-provided private socket.
pub async fn ensure_daemon_with_fallback(
    codex_home: &Path,
    private_socket: &Path,
) -> Result<DaemonMode, Error> {
    match ensure_well_known(codex_home).await {
        Ok(mode) => Ok(mode),
        Err(error) => {
            tracing::warn!(%error, "well-known Codex daemon unavailable; using private socket");
            let socket_path = resolve_socket_path(private_socket).await?;
            if !prepare_spawn_socket(&socket_path).await? {
                return Ok(DaemonMode::PrivateExisting(socket_path));
            }
            spawn_server(socket_path, true).await
        }
    }
}

async fn ensure_well_known(codex_home: &Path) -> Result<DaemonMode, Error> {
    tokio::fs::create_dir_all(codex_home).await?;
    let codex_home = tokio::fs::canonicalize(codex_home).await?;
    let socket_path = daemon_socket_path(&codex_home);

    match probe_socket(&socket_path).await? {
        SocketState::Ready => return Ok(DaemonMode::Existing),
        SocketState::Occupied(message) => return Err(Error::Daemon(message)),
        SocketState::Missing | SocketState::Stale => {}
    }

    if try_managed_daemon_start().await? && wait_until_ready(&socket_path).await? {
        return Ok(DaemonMode::Existing);
    }

    tokio::fs::create_dir_all(
        socket_path
            .parent()
            .ok_or_else(|| Error::Daemon("daemon socket has no parent directory".into()))?,
    )
    .await?;
    let socket_path = resolve_socket_path(&socket_path).await?;
    if !prepare_spawn_socket(&socket_path).await? {
        return Ok(DaemonMode::Existing);
    }
    spawn_server(socket_path, false).await
}

async fn try_managed_daemon_start() -> Result<bool, Error> {
    let mut command = tokio::process::Command::new("codex");
    command.args(["app-server", "daemon", "start"]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::piped());
    transport::configure_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|error| Error::Daemon(format!("failed to start daemon manager: {error}")))?;
    if let Some(stderr) = child.stderr.take() {
        transport::spawn_stderr_task(stderr, "codex app-server daemon start");
    }

    match timeout(START_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            if !status.success() {
                tracing::debug!(%status, "managed Codex daemon start fell through");
            }
            Ok(status.success())
        }
        Ok(Err(error)) => Err(Error::Daemon(format!(
            "failed waiting for daemon manager: {error}"
        ))),
        Err(_) => {
            transport::terminate_child_group(&mut child).await;
            tracing::debug!("managed Codex daemon start timed out and fell through");
            Ok(false)
        }
    }
}

async fn spawn_server(socket_path: PathBuf, private: bool) -> Result<DaemonMode, Error> {
    validate_socket_path(&socket_path)?;

    let mut command = tokio::process::Command::new("codex");
    command.arg("app-server");
    command.arg("--listen");
    command.arg(format!("unix://{}", socket_path.display()));
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::piped());
    transport::configure_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|error| Error::Daemon(format!("failed to spawn app-server: {error}")))?;
    if let Some(stderr) = child.stderr.take() {
        transport::spawn_stderr_task(stderr, "codex app-server daemon");
    }
    let process = supervise(child, socket_path.clone());

    if !wait_until_ready(&socket_path).await? {
        process.shutdown().await;
        return Err(Error::Daemon(format!(
            "app-server did not become ready at {}",
            socket_path.display()
        )));
    }

    if private {
        Ok(DaemonMode::Private(process))
    } else {
        Ok(DaemonMode::Spawned(process))
    }
}

fn supervise(mut child: Child, socket_path: PathBuf) -> DaemonProcess {
    let process_group = child.id().expect("spawned child has no process id") as i32;
    let running = Arc::new(AtomicBool::new(true));
    let waiter_running = Arc::clone(&running);
    let cancel = CancellationToken::new();
    let waiter_cancel = cancel.clone();
    let waiter = tokio::spawn(async move {
        tokio::select! {
            _ = waiter_cancel.cancelled() => transport::terminate_child_group(&mut child).await,
            _ = child.wait() => {},
        }
        waiter_running.store(false, Ordering::Release);
    });
    DaemonProcess {
        socket_path,
        process_group,
        running,
        cancel,
        waiter: Some(waiter),
    }
}

async fn resolve_socket_path(socket_path: &Path) -> Result<PathBuf, Error> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| Error::Daemon("daemon socket has no parent directory".into()))?;
    tokio::fs::create_dir_all(parent).await?;
    let parent = tokio::fs::canonicalize(parent).await?;
    let file_name = socket_path
        .file_name()
        .ok_or_else(|| Error::Daemon("daemon socket has no file name".into()))?;
    let resolved = parent.join(file_name);
    validate_socket_path(&resolved)?;
    Ok(resolved)
}

fn validate_socket_path(socket_path: &Path) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;

    let length = socket_path.as_os_str().as_bytes().len();
    if length > MAX_SOCKET_PATH_BYTES {
        return Err(Error::Daemon(format!(
            "Unix socket path is {length} bytes; it must be at most {MAX_SOCKET_PATH_BYTES}: {}",
            socket_path.display()
        )));
    }
    Ok(())
}

/// Returns `false` when another healthy server won the spawn race.
async fn prepare_spawn_socket(socket_path: &Path) -> Result<bool, Error> {
    match probe_socket(socket_path).await? {
        SocketState::Ready => Ok(false),
        SocketState::Occupied(message) => Err(Error::Daemon(message)),
        SocketState::Stale => {
            tokio::fs::remove_file(socket_path).await?;
            Ok(true)
        }
        SocketState::Missing => Ok(true),
    }
}

async fn wait_until_ready(socket_path: &Path) -> Result<bool, Error> {
    for _ in 0..READY_RETRIES {
        match probe_socket(socket_path).await? {
            SocketState::Ready => return Ok(true),
            SocketState::Occupied(message) => return Err(Error::Daemon(message)),
            SocketState::Missing | SocketState::Stale => sleep(READY_RETRY_DELAY).await,
        }
    }
    Ok(false)
}

enum SocketState {
    Missing,
    Stale,
    Ready,
    Occupied(String),
}

async fn probe_socket(socket_path: &Path) -> Result<SocketState, Error> {
    let stream = match timeout(PROBE_TIMEOUT, UnixStream::connect(socket_path)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SocketState::Missing);
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Ok(SocketState::Stale);
        }
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            return Ok(SocketState::Occupied(format!(
                "timed out connecting to live socket {}",
                socket_path.display()
            )));
        }
    };

    match timeout(PROBE_TIMEOUT, client_async("ws://localhost/rpc", stream)).await {
        Ok(Ok((mut websocket, _))) => {
            let _ = websocket.close(None).await;
            Ok(SocketState::Ready)
        }
        Ok(Err(error)) => Ok(SocketState::Occupied(format!(
            "socket {} accepts connections but rejected the WebSocket handshake: {error}",
            socket_path.display()
        ))),
        Err(_) => Ok(SocketState::Occupied(format!(
            "socket {} accepts connections but its WebSocket handshake timed out",
            socket_path.display()
        ))),
    }
}

async fn websocket_bridge(
    mut websocket: WebSocketStream<UnixStream>,
    mut inbound: tokio::io::DuplexStream,
    mut outbound: BufReader<tokio::io::DuplexStream>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            message = websocket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if inbound.write_all(text.as_bytes()).await.is_err()
                        || inbound.write_all(b"\n").await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if websocket.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            read = outbound.read_line(&mut line) => match read {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = line.trim_end_matches(['\r', '\n']);
                    if websocket.send(Message::Text(text.to_owned().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}
