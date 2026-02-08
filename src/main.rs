#[macro_use]
mod log;
mod buffer;
mod client;
mod cloud;
mod config;
mod error;
mod hooks;
mod init;
mod jwt;
mod message;
mod multiplex_log_buffer;
mod oauth;
mod route;
mod server;
mod session;
mod state;
mod structured_log;
mod transcript;
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
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new agent session
    #[command(name = "new-agent")]
    NewAgent {
        /// Agent type: claude or test-agent (test-agent only in dev builds)
        agent_type: String,

        /// Session alias (optional human-readable name)
        #[arg(short = 't', long)]
        target: Option<String>,
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

    /// Initialize amux (cloud mode, authentication)
    Init {
        /// Clear existing state and re-initialize
        #[arg(long)]
        reset: bool,
    },

    /// Start the amux server directly (usually auto-started)
    Serve {
        /// Run as cloud server (requires TLS, validates tokens)
        #[arg(long)]
        cloud: bool,
    },

    /// Internal: Handle hooks from AI coding assistants
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        provider: HooksProvider,
    },

    /// Internal: Show server debug information
    #[command(hide = true)]
    Debug,
}

#[derive(Subcommand)]
enum HooksProvider {
    /// Claude Code hooks
    Claude {
        #[command(subcommand)]
        event: ClaudeHookEvent,
    },
}

#[derive(Subcommand)]
enum ClaudeHookEvent {
    /// Called when a Claude Code session starts
    SessionStart,
    /// Called when Claude Code requests permission for a tool
    PermissionRequest,
}

#[tokio::main]
async fn main() {
    log::init();

    let cli = Cli::parse();

    // Resolve config path: explicit --config flag, or default path if it exists
    let config_path: Option<PathBuf> = match &cli.config {
        Some(path) => Some(path.clone()),
        None => {
            let default_path = Config::default_path();
            if default_path.exists() {
                Some(default_path)
            } else {
                None
            }
        }
    };

    // Load config from file or use defaults
    let config = match &config_path {
        Some(path) => match Config::from_file(path) {
            Ok(c) => c,
            Err(e) => {
                if cli.config.is_some() {
                    // Explicit --config: failure is fatal
                    eprintln!("error: failed to load config from {:?}: {}", path, e);
                    std::process::exit(1);
                } else {
                    // Default path: failure is a warning, fall back to defaults
                    eprintln!(
                        "warning: failed to load config from {:?}: {}, using defaults",
                        path, e
                    );
                    Config::new()
                }
            }
        },
        None => Config::new(),
    };

    // Handle hooks commands first (no server needed)
    if let Some(Commands::Hooks { provider }) = &cli.command {
        match provider {
            HooksProvider::Claude { .. } => {
                hooks::handle_claude_hook(&config);
                return;
            }
        }
    }

    let result = match cli.command {
        None => {
            // Default: attach to first available agent
            ensure_initialized(&config).await;
            ensure_server_running(&config, config_path.as_deref()).await;
            client::attach(None, &config).await
        }
        Some(Commands::NewAgent { agent_type, target }) => {
            let agent_type = match parse_agent_type(&agent_type) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            };
            ensure_initialized(&config).await;
            ensure_server_running(&config, config_path.as_deref()).await;
            client::new_agent(target.as_deref(), agent_type, &config).await
        }
        Some(Commands::Attach { target }) => {
            ensure_initialized(&config).await;
            ensure_server_running(&config, config_path.as_deref()).await;
            client::attach(target.as_deref(), &config).await
        }
        Some(Commands::ListAgents) => client::list_agents(&config).await,
        Some(Commands::KillServer) => client::kill_server(&config).await,
        Some(Commands::Connect { address }) => {
            ensure_initialized(&config).await;
            ensure_server_running(&config, config_path.as_deref()).await;
            client::connect(&address, &config).await
        }
        Some(Commands::Init { reset }) => {
            if let Err(e) = init::run_init(&config, reset).await {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
            return;
        }
        Some(Commands::Serve { cloud }) => {
            let mut server = server::Server::with_config(config);
            if let Err(e) = server.run(cloud).await {
                log!("server error: {}", e);
                std::process::exit(1);
            }
            return;
        }
        Some(Commands::Debug) => {
            match client::debug(&config).await {
                Ok(info) => {
                    print!(
                        "{}",
                        serde_yaml::to_string(&info).unwrap_or_else(|e| format!("error: {}", e))
                    );
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
            return;
        }
        Some(Commands::Hooks { .. }) => unreachable!("hooks handled above"),
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

/// Ensure initialization is complete (cloud mode, authentication)
async fn ensure_initialized(config: &Config) {
    if init::needs_init(config) {
        println!("First-time setup required.\n");
        if let Err(e) = init::run_init(config, false).await {
            eprintln!("error: initialization failed: {}", e);
            std::process::exit(1);
        }
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
    cmd.arg("serve");

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

// TODO: Once E2E executor can call amux/test-agent binaries directly (without
// path substitution), switch to Clap's ValueEnum for proper enum argument parsing.
fn parse_agent_type(s: &str) -> Result<message::AgentType, String> {
    match s.to_lowercase().as_str() {
        "claude" => Ok(message::AgentType::Claude),
        #[cfg(any(debug_assertions, test))]
        "test-agent" => Ok(message::AgentType::TestAgent(s.to_string())),
        #[cfg(any(debug_assertions, test))]
        _ if s.ends_with("test-agent") => {
            // Accept full path for E2E tests (e.g., /abs/path/test-agent)
            Ok(message::AgentType::TestAgent(s.to_string()))
        }
        #[cfg(not(any(debug_assertions, test)))]
        _ => Err(format!("Unknown agent type: '{}'. Valid: claude", s)),
        #[cfg(any(debug_assertions, test))]
        _ => Err(format!(
            "Unknown agent type: '{}'. Valid: claude, test-agent",
            s
        )),
    }
}
