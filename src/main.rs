#[macro_use]
mod log;
mod client;
mod server;
mod session;

use clap::{Parser, Subcommand};
use server::SOCKET_PATH;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Agent multiplexer - terminal multiplexer for Claude
#[derive(Parser)]
#[command(name = "amux")]
#[command(about = "Terminal multiplexer for Claude agents", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Hidden server mode (used internally for forking)
    #[arg(long, hide = true)]
    server: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Attach to an agent session (default if no command given)
    Attach {
        /// Target session name (default: agent1)
        #[arg(short = 't', long, default_value = "agent1")]
        target: String,
    },
    /// List all running agent sessions
    ListAgents,
    /// Kill all agents and shut down the server
    KillServer,
}

#[tokio::main]
async fn main() {
    log::init();

    let cli = Cli::parse();

    // Hidden server mode
    if cli.server {
        let server = server::Server::new();
        if let Err(e) = server.run().await {
            log!("server error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let result = match cli.command {
        None => {
            // Default: attach to agent1
            ensure_server_running().await;
            client::attach("agent1").await
        }
        Some(Commands::Attach { target }) => {
            ensure_server_running().await;
            client::attach(&target).await
        }
        Some(Commands::ListAgents) => client::list_agents().await,
        Some(Commands::KillServer) => client::kill_server().await,
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

/// Ensure the server is running, start it if not
async fn ensure_server_running() {
    // Check if socket exists and server is actually responding
    if Path::new(SOCKET_PATH).exists() {
        // Try to connect to verify server is alive
        match tokio::net::UnixStream::connect(SOCKET_PATH).await {
            Ok(_) => return, // Server is running
            Err(e) => {
                // Stale socket - server died without cleanup
                log!("stale socket detected ({}), removing", e);
                let _ = std::fs::remove_file(SOCKET_PATH);
            }
        }
    }

    log!("starting server");

    // Spawn server as background process
    let exe = std::env::current_exe().expect("Failed to get current exe");
    Command::new(&exe)
        .arg("--server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start server");

    // Wait for server to be ready
    for _ in 0..50 {
        if Path::new(SOCKET_PATH).exists() {
            // Verify server is actually listening
            if tokio::net::UnixStream::connect(SOCKET_PATH).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    eprintln!("error: Server failed to start");
    std::process::exit(1);
}
