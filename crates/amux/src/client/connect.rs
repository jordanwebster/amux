use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

use super::Connection;
use crate::config::{Config, ConfigError};
use crate::protocol::message::ProtocolError;
use crate::protocol::route::generate_terminal_link;
use crate::transport::{HandshakeError, LocalTransport, TransportError, connect_handshake};

#[derive(Clone, Debug)]
pub struct DaemonOptions {
    pub executable: PathBuf,
}

impl DaemonOptions {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerMode {
    Local,
    Cloud,
}

impl ServerMode {
    pub const fn is_cloud(self) -> bool {
        matches!(self, Self::Cloud)
    }
}

type Result<T> = std::result::Result<T, ConnectError>;

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("invalid handshake message: {0}")]
    InvalidHandshake(String),
    #[error("server rejected connection: {0}")]
    Protocol(ProtocolError),
    #[error(
        "handshake failed after 5 link-name collisions — this is usually transient, retry the command"
    )]
    HandshakeTooManyAttempts,
    #[error("{0}")]
    Start(String),
}

impl From<bool> for ServerMode {
    fn from(cloud: bool) -> Self {
        if cloud { Self::Cloud } else { Self::Local }
    }
}

impl From<HandshakeError> for ConnectError {
    fn from(error: HandshakeError) -> Self {
        match error {
            HandshakeError::Transport(error) => Self::Transport(error),
            HandshakeError::Timeout => Self::HandshakeTimeout,
            HandshakeError::InvalidMessage(message) => Self::InvalidHandshake(message),
            HandshakeError::Protocol(error) => Self::Protocol(error),
            HandshakeError::TooManyAttempts => Self::HandshakeTooManyAttempts,
        }
    }
}

/// Policy for how `connect()` reaches the amux server.
pub enum ConnectPolicy {
    /// Connect to existing server, spawn a managed `amux server start` daemon if needed.
    SpawnDaemon(DaemonOptions),
    /// Connect to existing server only, fail if not running.
    ExistingOnly,
}

/// Connect to an amux server according to the given policy.
///
/// Returns a [`Connection`] that can send and receive messages.
pub async fn connect(
    config: &Config,
    policy: ConnectPolicy,
) -> std::result::Result<Connection, ConnectError> {
    match policy {
        ConnectPolicy::ExistingOnly => connect_existing(config).await,
        ConnectPolicy::SpawnDaemon(options) => connect_daemon(config, options).await,
    }
}

pub async fn spawn_daemon(
    config: &Config,
    options: &DaemonOptions,
    mode: ServerMode,
) -> std::result::Result<(), ConnectError> {
    let _ = spawn_daemon_and_connect(config, options, mode).await?;
    Ok(())
}

#[cfg(unix)]
async fn connect_local_transport(config: &Config) -> std::io::Result<LocalTransport> {
    let stream = tokio::net::UnixStream::connect(&config.socket_path).await?;
    Ok(LocalTransport::new(stream))
}

#[cfg(windows)]
async fn connect_local_transport(config: &Config) -> std::io::Result<LocalTransport> {
    let pipe_name = config.socket_path.to_string_lossy().into_owned();
    for _ in 0..20 {
        match ClientOptions::new().open(&pipe_name) {
            Ok(client) => return Ok(LocalTransport::new(client)),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
    ClientOptions::new()
        .open(&pipe_name)
        .map(LocalTransport::new)
}

/// Connect to an existing server via the local control-plane transport and perform handshake.
async fn connect_existing(config: &Config) -> Result<Connection> {
    let mut transport = connect_local_transport(config)
        .await
        .map_err(TransportError::from)?;
    let outcome = connect_handshake(&mut transport, generate_terminal_link)
        .await
        .map_err(ConnectError::from)?;
    let link = outcome.link;
    tracing::info!(link = %link, "connected");
    Ok(Connection::new(transport, link))
}

/// Try connecting to existing server; if not running, spawn `amux server start` and retry.
async fn connect_daemon(config: &Config, options: DaemonOptions) -> Result<Connection> {
    match connect_existing(config).await {
        Ok(conn) => return Ok(conn),
        #[cfg(unix)]
        Err(ConnectError::Transport(TransportError::Io(e)))
            if config.socket_path.exists()
                && (e.kind() == std::io::ErrorKind::ConnectionRefused
                    || e.kind() == std::io::ErrorKind::NotFound) =>
        {
            tracing::warn!(error = %e, "stale local socket detected, removing");
            let _ = std::fs::remove_file(&config.socket_path);
        }
        Err(ConnectError::Transport(TransportError::Io(_))) => {}
        Err(e) => return Err(e),
    }

    spawn_daemon_and_connect(config, &options, ServerMode::Local).await
}

async fn spawn_daemon_and_connect(
    config: &Config,
    options: &DaemonOptions,
    mode: ServerMode,
) -> Result<Connection> {
    config.validate(mode.is_cloud())?;

    tracing::info!(?mode, "starting server");

    let config_yaml = serde_yaml::to_string(config)
        .map_err(|e| ConnectError::Start(format!("failed to serialize config: {e}")))?;

    let startup_stderr_path = startup_stderr_path(config);
    let mut cmd = daemon_command(options, mode, Some(&startup_stderr_path));
    let mut child = cmd
        .spawn()
        .map_err(|e| ConnectError::Start(format!("failed to start server: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config_yaml.as_bytes());
    }

    let connection = wait_for_server_connection(config, &mut child, &startup_stderr_path).await?;
    let _ = std::fs::remove_file(&startup_stderr_path);
    Ok(connection)
}

fn daemon_command(
    options: &DaemonOptions,
    mode: ServerMode,
    startup_stderr_path: Option<&Path>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(&options.executable);
    let stderr = startup_stderr_path
        .and_then(open_startup_stderr_file)
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    cmd.arg("server")
        .arg("start")
        .arg("--foreground")
        .arg("--config-from-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr);
    if mode.is_cloud() {
        cmd.arg("--cloud");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008); // DETACHED_PROCESS
    }
    cmd
}

fn startup_stderr_path(config: &Config) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("amux-startup-stderr.log")
}

fn open_startup_stderr_file(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to create startup stderr directory");
        return None;
    }

    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => Some(file),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to open startup stderr log");
            None
        }
    }
}

async fn wait_for_server_connection(
    config: &Config,
    child: &mut std::process::Child,
    startup_stderr_path: &Path,
) -> Result<Connection> {
    for _ in 0..50 {
        match connect_existing(config).await {
            Ok(conn) => return Ok(conn),
            Err(ConnectError::Transport(TransportError::Io(_))) => {
                if let Some(status) = child.try_wait().map_err(|e| {
                    ConnectError::Start(format!("failed to inspect server process: {e}"))
                })? {
                    return Err(startup_exit_error(status, startup_stderr_path));
                }
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(e) => return Err(e),
        }
    }

    if let Some(status) = child
        .try_wait()
        .map_err(|e| ConnectError::Start(format!("failed to inspect server process: {e}")))?
    {
        return Err(startup_exit_error(status, startup_stderr_path));
    }

    let log_path = std::env::var("AMUX_LOG")
        .unwrap_or_else(|_| crate::default_log_path().display().to_string());
    Err(ConnectError::Start(startup_timeout_error(
        &log_path,
        startup_stderr_path,
    )))
}

fn startup_exit_error(
    status: std::process::ExitStatus,
    startup_stderr_path: &Path,
) -> ConnectError {
    let mut message = format!(
        "server exited before accepting connections ({})",
        exit_status_summary(status)
    );
    if let Some(stderr) = read_startup_stderr(startup_stderr_path) {
        message.push_str(":\n");
        message.push_str(&stderr);
    }
    ConnectError::Start(message)
}

fn startup_timeout_error(log_path: &str, startup_stderr_path: &Path) -> String {
    let mut message = format!(
        "server failed to start within 5s — check {} for details",
        log_path
    );
    if let Some(stderr) = read_startup_stderr(startup_stderr_path) {
        message.push_str("\n\nstartup stderr:\n");
        message.push_str(&stderr);
    }
    message
}

fn exit_status_summary(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    }
}

fn read_startup_stderr(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    format_startup_diagnostics(&contents)
}

fn format_startup_diagnostics(contents: &str) -> Option<String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectError, HandshakeError};

    #[test]
    fn maps_handshake_collision_to_dedicated_connect_error() {
        let error = ConnectError::from(HandshakeError::TooManyAttempts);

        assert!(matches!(error, ConnectError::HandshakeTooManyAttempts));
    }

    #[test]
    fn trims_empty_startup_diagnostics() {
        assert_eq!(super::format_startup_diagnostics(" \n\t "), None);
        assert_eq!(
            super::format_startup_diagnostics("\nError: startup failed\n"),
            Some("Error: startup failed".to_string())
        );
    }
}
