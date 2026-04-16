use std::io;
use std::path::Path;

use amux::protocol::{Command, DebugFormat, Message, ShutdownReason};
use amux::{
    Config, ConnectError, ConnectPolicy, Connection, DaemonOptions, ServerMode, TransportError,
    connect, run_server, spawn_daemon,
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

    conn.send(&Message::Command {
        command: Command::Shutdown,
    })
    .await?;

    match conn.recv().await {
        Ok(Message::Command {
            command:
                Command::ShutdownNotification {
                    reason: ShutdownReason::UserRequested,
                },
        }) => {}
        Ok(other) => tracing::warn!(?other, "unexpected shutdown response"),
        Err(e) => tracing::warn!(error = %e, "error reading shutdown response"),
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
    let _ = suspend_connected_server(conn).await?;
    Ok(())
}

/// Suspend all agents and stop the server if it is currently running.
pub async fn suspend_server_if_running(config: &Config) -> Result<bool> {
    let Some(conn) = existing_server(config).await? else {
        return Ok(false);
    };
    suspend_connected_server(conn).await
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

    conn.send(&Message::Command {
        command: Command::Resume,
    })
    .await?;

    match conn.recv().await? {
        Message::Command {
            command:
                Command::ResumeResult {
                    resumed_count,
                    failed_count,
                    error,
                },
        } => {
            if let Some(error) = error {
                return Err(anyhow!("resume failed: {error}"));
            }
            print!("Resumed {resumed_count} agent(s).");
            if failed_count > 0 {
                print!(" ({failed_count} failed to resume)");
            }
            println!();
            Ok(())
        }
        other => Err(anyhow!(
            "unexpected response to Resume: {}",
            other.type_label()
        )),
    }
}

/// Connect to a remote amux server
pub async fn connect_remote(address: &str, config: &Config) -> Result<()> {
    let conn = connect(config, cli_daemon_policy()?).await?;

    conn.send(&Message::Command {
        command: Command::ConnectToServer {
            address: address.to_string(),
        },
    })
    .await?;

    match conn.recv().await? {
        Message::Command {
            command: Command::ConnectToServerResult { error: None },
        } => {
            println!("Connected to {}", address);
            Ok(())
        }
        Message::Command {
            command: Command::ConnectToServerResult { error: Some(e) },
        } => Err(anyhow!("failed to connect to {address}: {e}")),
        other => Err(anyhow!(
            "expected ConnectToServerResult, got {}",
            other.type_label()
        )),
    }
}

/// Get server debug information as a pre-rendered string in the requested format.
pub async fn debug(config: &Config, verbose: bool, format: DebugFormat) -> Result<String> {
    let Some(conn) = existing_server(config).await? else {
        return Err(anyhow!("No server running"));
    };

    conn.send(&Message::Command {
        command: Command::Debug { verbose, format },
    })
    .await?;

    match conn.recv().await? {
        Message::Command {
            command: Command::DebugResult { dump },
        } => Ok(dump),
        other => Err(anyhow!("expected DebugResult, got {}", other.type_label())),
    }
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

async fn suspend_connected_server(conn: Connection) -> Result<bool> {
    conn.send(&Message::Command {
        command: Command::Suspend,
    })
    .await?;

    match conn.recv().await {
        Ok(Message::Command {
            command:
                Command::SuspendResult {
                    suspended_count,
                    error,
                },
        }) => {
            if let Some(error) = error {
                return Err(anyhow!("suspend failed: {error}"));
            }
            println!("Suspended {suspended_count} agent(s).");
            Ok(true)
        }
        Err(TransportError::Io(_)) => Ok(true),
        Err(e) => Err(e.into()),
        Ok(other) => Err(anyhow!(
            "unexpected response to Suspend: {}",
            other.type_label()
        )),
    }
}

fn is_server_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}
