use std::path::Path;
use std::sync::Arc;

use amux::{Client, ClientError, Config, CredentialProvider, DebugFormat, Server, TransportError};
use anyhow::{Result, anyhow};

use crate::auth::{DeviceFlowProvider, auth_file_path};
use crate::client_common::{
    get_client, get_client_with_executable, open_daemon, print_update_banner, remove_stale_socket,
    server_unavailable_error, spawn_daemon,
};
use crate::update::MarkerFileReporter;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerMode {
    Local,
    Cloud,
}

impl ServerMode {
    pub(crate) const fn is_cloud(self) -> bool {
        matches!(self, Self::Cloud)
    }
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
            run_server_foreground(config.clone(), options.mode.is_cloud()).await?;
            Ok(())
        }
        StartStyle::Background => {
            spawn_daemon(config, options.mode.is_cloud()).await?;
            println!("Server started.");
            Ok(())
        }
    }
}

pub(crate) async fn run_server_foreground(config: Config, cloud: bool) -> Result<()> {
    let update_reporter = Arc::new(MarkerFileReporter::from_state_path(&config.state_path));
    let credentials = if cloud {
        None
    } else {
        Some(Arc::new(DeviceFlowProvider::new(
            auth_file_path(&config.state_path),
            config.cloud_url.clone(),
        )) as Arc<dyn CredentialProvider>)
    };
    let builder = Server::builder()
        .config(config)
        .update_reporter(update_reporter);
    match credentials {
        Some(credentials) => builder.credentials(credentials).run().await?,
        None => builder.as_cloud_relay().run().await?,
    }
    Ok(())
}

/// Kill all agents and shut down the server
pub async fn stop_server(config: &Config) -> Result<()> {
    let Some(client) = existing_server(config).await? else {
        println!("No server running.");
        return Ok(());
    };

    if let Err(error) = client.shutdown().await {
        tracing::warn!(%error, "error reading shutdown response");
    }

    println!("Server shutting down.");
    print_update_banner(&config.state_path);
    Ok(())
}

/// Suspend all agents and stop the server.
pub async fn suspend_server(config: &Config) -> Result<()> {
    let Some(client) = existing_server(config).await? else {
        println!("No server running.");
        return Ok(());
    };
    let _ = suspend_connected_server(client, SuspendIntent::User).await?;
    Ok(())
}

/// Suspend all agents for an update if the server is currently running.
pub async fn suspend_server_for_update_if_running(config: &Config) -> Result<bool> {
    let Some(client) = existing_server(config).await? else {
        return Ok(false);
    };
    suspend_connected_server(client, SuspendIntent::Update).await
}

/// Resume any previously suspended agents, spawning the daemon if needed.
pub async fn resume_server(config: &Config) -> Result<()> {
    let executable =
        std::env::current_exe().map_err(|e| anyhow!("failed to determine executable: {e}"))?;
    resume_server_with_executable(config, executable.as_path()).await
}

/// Resume any previously suspended agents, spawning the daemon from `executable` if needed.
pub async fn resume_server_with_executable(config: &Config, executable: &Path) -> Result<()> {
    let client = get_client_with_executable(config, executable).await?;
    let summary = client.resume().await?;
    print!("Resumed {} agent(s).", summary.resumed_count);
    if summary.failed_count > 0 {
        print!(" ({} failed to resume)", summary.failed_count);
    }
    println!();
    Ok(())
}

/// Connect to a remote amux server.
pub async fn connect_remote(address: &str, config: &Config) -> Result<()> {
    let client = get_client(config).await?;
    client.connect_to_server(address.to_string()).await?;
    println!("Connected to {}", address);
    Ok(())
}

/// Get server debug information as a pre-rendered string in the requested format.
pub async fn debug(config: &Config, verbose: bool, format: DebugFormat) -> Result<String> {
    let Some(client) = existing_server(config).await? else {
        return Err(anyhow!("No server running"));
    };

    Ok(client.debug_dump_verbose(verbose, format).await?)
}

/// Probe whether a local amux server is running. Also removes a stale socket
/// file if one is found, as a side effect of the underlying `existing_server`
/// check. Any connect error that isn't "server not running" is treated as a
/// non-probe; we conservatively return `false` so callers don't block on
/// ambiguous state.
pub(crate) async fn server_is_running(config: &Config) -> bool {
    matches!(existing_server(config).await, Ok(Some(_)))
}

async fn existing_server(config: &Config) -> Result<Option<Client>> {
    match open_daemon(config).await {
        Ok(client) => Ok(Some(client)),
        Err(error) if server_unavailable_error(config, &error) => {
            remove_stale_socket(config, &error);
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn suspend_connected_server(client: Client, intent: SuspendIntent) -> Result<bool> {
    let result = match intent {
        SuspendIntent::User => client.suspend().await,
        SuspendIntent::Update => client.suspend_for_update().await,
    };
    match result {
        Ok(summary) => {
            println!("Suspended {} agent(s).", summary.suspended_count);
            Ok(true)
        }
        Err(ClientError::Transport(TransportError::Io(_))) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

impl From<bool> for ServerMode {
    fn from(cloud: bool) -> Self {
        if cloud { Self::Cloud } else { Self::Local }
    }
}
