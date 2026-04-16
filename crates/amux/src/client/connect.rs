use crate::client::connection::Connection;
use crate::config::Config;
use crate::config::ConfigError;
use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::protocol::message::ProtocolError;
use crate::protocol::route::generate_terminal_link;
use crate::server::Server;
use crate::transport::{LocalTransport, Transport, TransportError};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

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
    #[error("{0}")]
    Start(String),
}

impl From<bool> for ServerMode {
    fn from(cloud: bool) -> Self {
        if cloud { Self::Cloud } else { Self::Local }
    }
}

/// Policy for how `connect()` reaches the amux server.
pub enum ConnectPolicy {
    /// Connect to existing server, spawn a managed `amux server start` daemon if needed.
    SpawnDaemon(DaemonOptions),
    /// Start server in-process on a background task, connect via socket.
    Embedded,
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
        ConnectPolicy::Embedded => connect_embedded(config).await,
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
    let link_name = connect_handshake(&mut transport, generate_terminal_link).await?;
    tracing::info!(link = %link_name, "connected");
    Ok(Connection::new(transport, link_name))
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

/// Start server in-process on a background task, then connect.
async fn connect_embedded(config: &Config) -> Result<Connection> {
    // Validate before spawning so callers see the real error, not a timeout
    config.validate(false)?;

    #[cfg(unix)]
    if config.socket_path.exists() {
        let _ = std::fs::remove_file(&config.socket_path);
    }

    let mut server =
        Server::with_config(config.clone()).map_err(|e| ConnectError::Start(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = server.run(false).await {
            tracing::error!(error = %e, "embedded server exited with error");
        }
    });

    for _ in 0..50 {
        match connect_existing(config).await {
            Ok(conn) => return Ok(conn),
            Err(ConnectError::Transport(TransportError::Io(_))) => {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(e) => return Err(e),
        }
    }

    Err(ConnectError::Start(
        "embedded server failed to start within 5s".to_string(),
    ))
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

    let mut cmd = daemon_command(options, mode);
    let mut child = cmd
        .spawn()
        .map_err(|e| ConnectError::Start(format!("failed to start server: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config_yaml.as_bytes());
    }

    wait_for_server_connection(config).await
}

fn daemon_command(options: &DaemonOptions, mode: ServerMode) -> std::process::Command {
    let mut cmd = std::process::Command::new(&options.executable);
    cmd.arg("server")
        .arg("start")
        .arg("--foreground")
        .arg("--config-from-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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

async fn wait_for_server_connection(config: &Config) -> Result<Connection> {
    for _ in 0..50 {
        match connect_existing(config).await {
            Ok(conn) => return Ok(conn),
            Err(ConnectError::Transport(TransportError::Io(_))) => {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(e) => return Err(e),
        }
    }

    let log_path = std::env::var("AMUX_LOG")
        .unwrap_or_else(|_| crate::config::default_log_path().display().to_string());
    Err(ConnectError::Start(format!(
        "server failed to start within 5s — check {} for details",
        log_path
    )))
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn connect_handshake<T, F>(
    transport: &mut T,
    mut generate_link: F,
) -> Result<String>
where
    T: Transport,
    F: FnMut() -> String,
{
    for attempt in 0..5 {
        let proposed_link = generate_link();

        let connect = Connect {
            link_name: proposed_link.clone(),
            token: None,
            version: PROTOCOL_VERSION,
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        let payload = connect
            .encode()
            .map_err(TransportError::SerializationEncode)?;
        transport.write_frame(&payload).await?;

        let payload = tokio::time::timeout(HANDSHAKE_TIMEOUT, transport.read_frame())
            .await
            .map_err(|_| ConnectError::HandshakeTimeout)??;
        let response = ConnectResult::decode(&payload).map_err(|e| {
            ConnectError::InvalidHandshake(format!("expected ConnectResult during handshake: {e}"))
        })?;

        match response.error {
            None => return Ok(proposed_link),
            Some(ProtocolError::LinkNameTaken) => {
                tracing::debug!(link = %proposed_link, attempt = attempt + 1, "link name taken, retrying");
                continue;
            }
            Some(other) => return Err(ConnectError::Protocol(other)),
        }
    }

    Err(ConnectError::Start(
        "handshake failed after 5 link-name collisions — this is usually transient, retry the command".to_string(),
    ))
}
