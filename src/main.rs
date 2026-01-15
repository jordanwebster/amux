#[macro_use]
mod log;
mod buffer;
mod client;
mod config;
mod error;
mod message;
mod server;
mod session;
mod transport;

use clap::{Parser, Subcommand};
use config::Config;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Agent multiplexer - terminal multiplexer for AI agents
#[derive(Parser)]
#[command(name = "amux")]
#[command(about = "Terminal multiplexer for AI agents (Claude, Codex, etc.)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file (YAML format)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Hidden server mode (used internally for forking)
    #[arg(long, hide = true)]
    server: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new agent session
    #[command(name = "new-agent")]
    NewAgent {
        /// Command to run (e.g., claude, codex)
        command: String,

        /// Session name
        #[arg(short = 't', long, default_value = "default")]
        target: String,
    },

    /// Attach to an existing agent session
    Attach {
        /// Target session name (default: first available)
        #[arg(short = 't', long)]
        target: Option<String>,
    },

    /// List all running agent sessions
    ListAgents,

    /// Kill all agents and shut down the server
    KillServer,

    /// Connect to a remote amux server
    Connect {
        /// Remote server address (host:port)
        address: String,
    },
}

#[tokio::main]
async fn main() {
    log::init();

    let cli = Cli::parse();

    // Load config from file or use defaults
    let config = match &cli.config {
        Some(path) => match Config::from_file(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: failed to load config from {:?}: {}", path, e);
                std::process::exit(1);
            }
        },
        None => Config::new(),
    };

    // Hidden server mode
    if cli.server {
        let mut server = server::Server::with_config(config);
        if let Err(e) = server.run().await {
            log!("server error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let result = match cli.command {
        None => {
            // Default: attach to first available agent
            ensure_server_running(&config, cli.config.as_deref()).await;
            client::attach(None, &config).await
        }
        Some(Commands::NewAgent { command, target }) => {
            ensure_server_running(&config, cli.config.as_deref()).await;
            client::new_agent(&target, &command, &config).await
        }
        Some(Commands::Attach { target }) => {
            ensure_server_running(&config, cli.config.as_deref()).await;
            client::attach(target.as_deref(), &config).await
        }
        Some(Commands::ListAgents) => client::list_agents(&config).await,
        Some(Commands::KillServer) => client::kill_server(&config).await,
        Some(Commands::Connect { address }) => {
            ensure_server_running(&config, cli.config.as_deref()).await;
            client::connect(&address, &config).await
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

/// Ensure the server is running, start it if not
async fn ensure_server_running(config: &Config, config_path: Option<&Path>) {
    let socket_path = &config.socket_path;

    // Check if socket exists and server is actually responding
    if socket_path.exists() {
        // Try to connect to verify server is alive
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(_) => return, // Server is running
            Err(e) => {
                // Stale socket - server died without cleanup
                log!("stale socket detected ({}), removing", e);
                let _ = std::fs::remove_file(socket_path);
            }
        }
    }

    log!("starting server");

    // Spawn server as background process
    let exe = std::env::current_exe().expect("Failed to get current exe");
    let mut cmd = Command::new(&exe);
    cmd.arg("--server");

    // Pass config file path if we have one
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start server");

    // Wait for server to be ready
    for _ in 0..50 {
        if socket_path.exists() {
            // Verify server is actually listening
            if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    eprintln!("error: Server failed to start");
    std::process::exit(1);
}
