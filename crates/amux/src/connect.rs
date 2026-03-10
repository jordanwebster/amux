use crate::config::Config;
use crate::connection::Connection;
use crate::error::{AmuxError, Result};
use crate::route::generate_terminal_link;
use crate::server;
use crate::transport::LocalTransport;
use std::time::Duration;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

/// Policy for how `connect()` reaches the amux server.
pub enum ConnectPolicy {
    /// Connect to existing server, spawn `amux serve` if not running.
    Daemon,
    /// Start server in-process on a background task, connect via socket.
    Embedded,
    /// Connect to existing server only, fail if not running.
    ExistingOnly,
}

/// Connect to an amux server according to the given policy.
///
/// Returns a [`Connection`] that can send and receive messages.
pub async fn connect(config: &Config, policy: ConnectPolicy) -> Result<Connection> {
    match policy {
        ConnectPolicy::ExistingOnly => connect_existing(config).await,
        ConnectPolicy::Daemon => connect_daemon(config).await,
        ConnectPolicy::Embedded => connect_embedded(config).await,
    }
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
    let mut transport = connect_local_transport(config).await?;
    let link_name = server::connect_handshake(&mut transport, generate_terminal_link).await?;
    tracing::info!(link = %link_name, "connected");
    Ok(Connection::new(transport, link_name))
}

/// Try connecting to existing server; if not running, spawn `amux serve` and retry.
async fn connect_daemon(config: &Config) -> Result<Connection> {
    match connect_existing(config).await {
        Ok(conn) => return Ok(conn),
        #[cfg(unix)]
        Err(AmuxError::Io(e))
            if config.socket_path.exists()
                && (e.kind() == std::io::ErrorKind::ConnectionRefused
                    || e.kind() == std::io::ErrorKind::NotFound) =>
        {
            tracing::warn!(error = %e, "stale local socket detected, removing");
            let _ = std::fs::remove_file(&config.socket_path);
        }
        Err(AmuxError::Io(_)) => {}
        Err(e) => return Err(e),
    }

    tracing::info!("starting server");

    let exe = std::env::current_exe()
        .map_err(|e| AmuxError::Config(format!("failed to get current exe: {e}")))?;

    let config_yaml = serde_yaml::to_string(config)
        .map_err(|e| AmuxError::Config(format!("failed to serialize config: {e}")))?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve")
        .arg("--config-from-stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008); // DETACHED_PROCESS
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| AmuxError::Config(format!("failed to start server: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config_yaml.as_bytes());
    }

    for _ in 0..50 {
        match connect_existing(config).await {
            Ok(conn) => return Ok(conn),
            Err(AmuxError::Io(_)) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(e) => return Err(e),
        }
    }

    let log_path = std::env::var("AMUX_LOG")
        .unwrap_or_else(|_| crate::config::default_log_path().display().to_string());
    Err(AmuxError::Config(format!(
        "server failed to start within 5s — check {} for details",
        log_path
    )))
}

/// Start server in-process on a background task, then connect.
async fn connect_embedded(config: &Config) -> Result<Connection> {
    #[cfg(unix)]
    if config.socket_path.exists() {
        let _ = std::fs::remove_file(&config.socket_path);
    }

    let mut server = server::Server::with_config(config.clone());
    tokio::spawn(async move {
        if let Err(e) = server.run(false).await {
            tracing::error!(error = %e, "embedded server exited with error");
        }
    });

    for _ in 0..50 {
        match connect_existing(config).await {
            Ok(conn) => return Ok(conn),
            Err(AmuxError::Io(_)) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(e) => return Err(e),
        }
    }

    Err(AmuxError::Config(
        "embedded server failed to start within 5s".to_string(),
    ))
}
