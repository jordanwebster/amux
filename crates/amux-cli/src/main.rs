mod client;
mod hooks;
mod init;
mod plugin;

use amux::Config;
use amux::protocol;
use amux::run_server;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use clap::Subcommand;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

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

        /// Session name (optional human-readable name)
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

        /// Read config from stdin (YAML format). Used by ConnectPolicy::Daemon.
        #[arg(long, hide = true)]
        config_from_stdin: bool,
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
    SessionStart,
    SessionEnd,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    UserPromptSubmit,
    Notification,
    Stop,
    SubagentStart,
    SubagentStop,
    PreCompact,
    Setup,
}

fn is_handled_hook_event(event: &ClaudeHookEvent) -> bool {
    matches!(
        event,
        ClaudeHookEvent::SessionStart | ClaudeHookEvent::PermissionRequest | ClaudeHookEvent::Stop
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let _log_guard = init_tracing();

    let cli = Cli::parse();

    // Fast-path exit for unhandled hook events (no stdin, no config, no socket)
    if let Some(Commands::Hooks {
        provider: HooksProvider::Claude { event },
    }) = &cli.command
        && !is_handled_hook_event(event)
    {
        return Ok(());
    }

    // Special case: serve --config-from-stdin reads config from stdin before
    // anything else (used by ConnectPolicy::Daemon)
    if let Some(Commands::Serve {
        cloud,
        config_from_stdin: true,
    }) = &cli.command
    {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("failed to read config from stdin")?;
        let config: Config =
            serde_yaml::from_str(&input).context("failed to parse config from stdin")?;
        return run_server(config, *cloud).await.map_err(Into::into);
    }

    let config = load_config(cli.config)?;

    match cli.command {
        None => {
            // Default: attach to first available agent
            ensure_initialized(&config).await?;
            client::attach(None, &config).await?;
        }
        Some(Commands::NewAgent { agent_type, target }) => {
            let agent_type = parse_agent_type(&agent_type)?;
            ensure_initialized(&config).await?;
            match agent_type {
                protocol::AgentType::Claude => {
                    plugin::ensure_plugin_installed().await;
                }
                #[cfg(any(debug_assertions, test))]
                protocol::AgentType::TestAgent(_) => {}
            };
            client::new_agent(target.as_deref(), agent_type, &config).await?;
        }
        Some(Commands::Attach { target }) => {
            ensure_initialized(&config).await?;
            client::attach(target.as_deref(), &config).await?;
        }
        Some(Commands::ListAgents) => client::list_agents(&config).await?,
        Some(Commands::KillServer) => client::kill_server(&config).await?,
        Some(Commands::Connect { address }) => {
            ensure_initialized(&config).await?;
            client::connect_remote(&address, &config).await?;
        }
        Some(Commands::Init { reset }) => init::run_init(&config, reset).await?,
        Some(Commands::Serve { cloud, .. }) => {
            run_server(config, cloud).await?;
        }
        Some(Commands::Debug) => {
            let info = client::debug(&config).await?;
            print!("{}", serde_yaml::to_string(&info)?);
        }
        Some(Commands::Hooks { provider }) => match provider {
            HooksProvider::Claude { .. } => {
                hooks::handle_claude_hook(&config);
            }
        },
    }

    Ok(())
}

/// Ensure initialization is complete (cloud mode, authentication)
async fn ensure_initialized(config: &Config) -> Result<()> {
    if init::needs_init(config) {
        println!("First-time setup required.\n");
        init::run_init(config, false)
            .await
            .context("initialization failed")?;
    }
    Ok(())
}

// TODO: Once E2E executor can call amux/test-agent binaries directly (without
// path substitution), switch to Clap's ValueEnum for proper enum argument parsing.
fn parse_agent_type(s: &str) -> Result<protocol::AgentType> {
    match s.to_lowercase().as_str() {
        "claude" => Ok(protocol::AgentType::Claude),
        #[cfg(any(debug_assertions, test))]
        "test-agent" => Ok(protocol::AgentType::TestAgent(s.to_string())),
        #[cfg(any(debug_assertions, test))]
        _ if s.ends_with("test-agent") => {
            // Accept full path for E2E tests (e.g., /abs/path/test-agent)
            Ok(protocol::AgentType::TestAgent(s.to_string()))
        }
        #[cfg(not(any(debug_assertions, test)))]
        _ => Err(anyhow!("Unknown agent type: '{}'. Valid: claude", s)),
        #[cfg(any(debug_assertions, test))]
        _ => Err(anyhow!(
            "Unknown agent type: '{}'. Valid: claude, test-agent",
            s
        )),
    }
}

fn init_tracing() -> WorkerGuard {
    let log_path = std::env::var("AMUX_LOG")
        .unwrap_or_else(|_| amux::default_log_path().display().to_string());
    let log_path_buf = PathBuf::from(&log_path);
    if let Some(parent) = log_path_buf.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let (writer, guard) = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => tracing_appender::non_blocking(file),
        Err(e) => {
            eprintln!(
                "warning: failed to open log file {}: {}, falling back to stderr",
                log_path, e
            );
            tracing_appender::non_blocking(std::io::stderr())
        }
    };

    let default_level = if cfg!(debug_assertions) {
        "amux=debug"
    } else {
        "amux=info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(filter)
        .init();

    guard
}

fn load_config(input_path: Option<PathBuf>) -> Result<Config> {
    // Resolve config path: explicit --config flag, or default path if it exists
    let config_path: Option<PathBuf> = match &input_path {
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
    Ok(match &config_path {
        Some(path) => match Config::from_file(path) {
            Ok(c) => c,
            Err(e) => match input_path {
                Some(_) => Err(anyhow!("failed to load config from {:?}: {}", path, e))?,
                None => {
                    eprintln!(
                        "warning: failed to load config from {:?}: {}, using defaults",
                        path, e
                    );
                    Config::new()
                }
            },
        },
        None => Config::new(),
    })
}
