use crate::config::Config;
use crate::connection::Connection;
use crate::error::{AmuxError, Result};
use crate::route::generate_terminal_link;
use crate::server;
use crate::transport::UnixTransport;
use std::time::Duration;
use tokio::net::UnixStream;

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

/// Connect to an existing server via Unix socket and perform handshake.
async fn connect_existing(config: &Config) -> Result<Connection> {
    let stream = UnixStream::connect(&config.socket_path).await?;
    let mut transport = UnixTransport::new(stream);
    let link_name = server::connect_handshake(&mut transport, generate_terminal_link).await?;
    tracing::info!(link = %link_name, "connected");
    Ok(Connection::new(transport, link_name))
}

/// Try connecting to existing server; if not running, spawn `amux serve` and retry.
async fn connect_daemon(config: &Config) -> Result<Connection> {
    let socket_path = &config.socket_path;

    // Check if socket exists and server is responding
    if socket_path.exists() {
        match UnixStream::connect(socket_path).await {
            Ok(_stream) => {
                // Server is running, do normal connect with handshake
                drop(_stream);
                return connect_existing(config).await;
            }
            Err(e) => {
                // Stale socket — server died without cleanup
                tracing::warn!(error = %e, "stale socket detected, removing");
                let _ = std::fs::remove_file(socket_path);
            }
        }
    }

    tracing::info!("starting server");

    // Spawn server as background process, passing config via stdin
    let exe = std::env::current_exe()
        .map_err(|e| AmuxError::Config(format!("failed to get current exe: {e}")))?;

    let config_yaml = serde_yaml::to_string(config)
        .map_err(|e| AmuxError::Config(format!("failed to serialize config: {e}")))?;

    let mut child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--config-from-stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| AmuxError::Config(format!("failed to start server: {e}")))?;

    // Write config to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config_yaml.as_bytes());
    }

    // Wait for server to be ready
    for _ in 0..50 {
        if socket_path.exists() && UnixStream::connect(socket_path).await.is_ok() {
            return connect_existing(config).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
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
    let socket_path = config.socket_path.clone();

    // Clean stale socket if present
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let mut server = server::Server::with_config(config.clone());
    tokio::spawn(async move {
        if let Err(e) = server.run(false).await {
            tracing::error!(error = %e, "embedded server exited with error");
        }
    });

    // Wait for server to be ready
    for _ in 0..50 {
        if socket_path.exists() && UnixStream::connect(&socket_path).await.is_ok() {
            return connect_existing(config).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Err(AmuxError::Config(
        "embedded server failed to start within 5s".to_string(),
    ))
}
