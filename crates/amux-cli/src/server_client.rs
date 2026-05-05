use std::io;
use std::path::Path;

use amux::protocol::DebugFormat;
use amux::{
    Config, ConnectError, ConnectPolicy, Connection, DaemonOptions, RpcClient, RpcClientError,
    ServerMode, TransportError, connect, run_server, spawn_daemon,
};
use anyhow::{Result, anyhow};

use crate::client_common::{cli_daemon_policy, daemon_options, print_update_banner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartStyle {
    Foreground,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StartOptions {
    mode: ServerMode,
    style: StartStyle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuspendIntent {
    User,
    Update,
}

impl StartOptions {
    pub(crate) fn from_flags(cloud: bool, foreground: bool) -> Self {
        let mode = ServerMode::from(cloud);
        let style = if foreground {
            StartStyle::Foreground
        } else {
            StartStyle::Background
        };
        Self { mode, style }
    }
}

/// Start the local server, daemonizing unless `foreground` is requested.
pub async fn start_server(config: &Config, options: StartOptions) -> Result<()> {
    if existing_server(config).await?.is_some() {
        println!("Server already running.");
        return Ok(());
    }

    config.validate(options.mode.is_cloud())?;

    match options.style {
        StartStyle::Foreground => {
            run_server(config.clone(), options.mode.is_cloud()).await?;
            Ok(())
        }
        StartStyle::Background => {
            spawn_daemon(config, &daemon_options()?, options.mode).await?;
            println!("Server started.");
            Ok(())
        }
    }
}

/// Kill all agents and shut down the server
pub async fn stop_server(config: &Config) -> Result<()> {
    let Some(conn) = existing_server(config).await? else {
        println!("No server running.");
        return Ok(());
    };

    let mut client = RpcClient::new(conn);
    if let Err(error) = client.shutdown().await {
        tracing::warn!(%error, "error reading shutdown response");
    }

    println!("Server shutting down.");
    print_update_banner(&config.state_path);
    Ok(())
}

/// Suspend all agents and stop the server.
pub async fn suspend_server(config: &Config) -> Result<()> {
    let Some(conn) = existing_server(config).await? else {
        println!("No server running.");
        return Ok(());
    };
    let _ = suspend_connected_server(conn, SuspendIntent::User).await?;
    Ok(())
}

/// Suspend all agents for an update if the server is currently running.
pub async fn suspend_server_for_update_if_running(config: &Config) -> Result<bool> {
    let Some(conn) = existing_server(config).await? else {
        return Ok(false);
    };
    suspend_connected_server(conn, SuspendIntent::Update).await
}

/// Resume any previously suspended agents, spawning the daemon if needed.
pub async fn resume_server(config: &Config) -> Result<()> {
    let executable = daemon_options()?.executable;
    resume_server_with_executable(config, executable.as_path()).await
}

/// Resume any previously suspended agents, spawning the daemon from `executable` if needed.
pub async fn resume_server_with_executable(config: &Config, executable: &Path) -> Result<()> {
    let conn = connect(
        config,
        ConnectPolicy::SpawnDaemon(DaemonOptions::new(executable.to_path_buf())),
    )
    .await?;

    let mut client = RpcClient::new(conn);
    let summary = client.resume().await?;
    print!("Resumed {} agent(s).", summary.resumed_count);
    if summary.failed_count > 0 {
        print!(" ({} failed to resume)", summary.failed_count);
    }
    println!();
    Ok(())
}

/// Connect to a remote amux server
pub async fn connect_remote(address: &str, config: &Config) -> Result<()> {
    let conn = connect(config, cli_daemon_policy()?).await?;

    let mut client = RpcClient::new(conn);
    client.connect_to_server(address.to_string()).await?;
    println!("Connected to {}", address);
    Ok(())
}

/// Get server debug information as a pre-rendered string in the requested format.
pub async fn debug(config: &Config, verbose: bool, format: DebugFormat) -> Result<String> {
    let Some(conn) = existing_server(config).await? else {
        return Err(anyhow!("No server running"));
    };

    let mut client = RpcClient::new(conn);
    Ok(client.debug(verbose, format).await?)
}

/// Probe whether a local amux server is running. Also removes a stale socket
/// file if one is found, as a side effect of the underlying `existing_server`
/// check. Any connect error that isn't "server not running" is treated as a
/// non-probe; we conservatively return `false` so callers don't block on
/// ambiguous state.
pub(crate) async fn server_is_running(config: &Config) -> bool {
    matches!(existing_server(config).await, Ok(Some(_)))
}

async fn existing_server(config: &Config) -> Result<Option<Connection>> {
    match connect(config, ConnectPolicy::ExistingOnly).await {
        Ok(conn) => Ok(Some(conn)),
        #[cfg(unix)]
        Err(ConnectError::Transport(TransportError::Io(e)))
            if config.socket_path.exists() && is_server_unavailable(&e) =>
        {
            tracing::warn!(error = %e, "stale local socket detected, removing");
            let _ = std::fs::remove_file(&config.socket_path);
            Ok(None)
        }
        Err(ConnectError::Transport(TransportError::Io(e))) if is_server_unavailable(&e) => {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

async fn suspend_connected_server(conn: Connection, intent: SuspendIntent) -> Result<bool> {
    let mut client = RpcClient::new(conn);
    let result = match intent {
        SuspendIntent::User => client.suspend().await,
        SuspendIntent::Update => client.suspend_for_update().await,
    };
    match result {
        Ok(summary) => {
            println!("Suspended {} agent(s).", summary.suspended_count);
            Ok(true)
        }
        Err(RpcClientError::Transport(TransportError::Io(_))) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

fn is_server_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}
