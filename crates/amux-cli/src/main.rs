mod client_common;
mod hooks;
mod init;
mod plugin;
mod server_client;
mod session_client;
mod update;

use std::fs::OpenOptions;
use std::io::Read;
use std::path::PathBuf;

use amux::{Config, ServerMode, protocol, run_server, setup};
use anyhow::{Context, Result, anyhow};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Agent multiplexer - terminal multiplexer for AI agents
#[derive(Debug, Parser)]
#[command(name = "amux")]
#[command(about = "Terminal multiplexer for AI agents (Claude, Codex, etc.)", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file (YAML format)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a new agent session
    New {
        /// Agent type: claude or test-agent (test-agent only in dev builds)
        agent_type: String,

        /// Session name (optional human-readable name)
        #[arg(long)]
        name: Option<String>,

        /// Extra arguments passed to the agent (after --)
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Attach to an existing agent session
    Attach {
        /// Session name (default: first available)
        name: Option<String>,
    },

    /// List all running agent sessions
    #[command(alias = "ls")]
    List,

    /// Manage the amux server lifecycle and topology
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },

    /// Initialize amux (cloud mode, authentication)
    Init {
        /// Clear existing state and re-initialize
        #[arg(long)]
        reset: bool,
    },

    /// Internal: Handle hooks from AI coding assistants
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        provider: HooksProvider,
    },

    /// Update amux to the latest version
    Update,

    /// Internal: Show server debug information
    #[command(hide = true)]
    Debug {
        /// Dump per-user, per-host, per-route, and per-agent details
        #[arg(long)]
        verbose: bool,
        /// Output format (default: yaml)
        #[arg(long, value_enum, default_value_t = CliDebugFormat::Yaml)]
        format: CliDebugFormat,
    },
}

#[derive(Debug, Subcommand)]
enum ServerCommands {
    /// Start the amux server
    Start {
        /// Run as cloud server (requires TLS, validates tokens)
        #[arg(long)]
        cloud: bool,

        /// Run in the foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,

        /// Read config from stdin (YAML format). Used by ConnectPolicy::SpawnDaemon.
        #[arg(long, hide = true)]
        config_from_stdin: bool,
    },

    /// Shut down the server and all running agent sessions
    Stop,

    /// Connect the local server to a remote amux server
    Connect {
        /// Remote server address (host:port)
        address: String,
    },

    /// Internal: Suspend all agents and stop the server
    #[command(hide = true)]
    Suspend,

    /// Internal: Resume suspended agents
    #[command(hide = true)]
    Resume,
}

/// CLI-side mirror of `protocol::DebugFormat` so we can derive `clap::ValueEnum`
/// without pulling clap into the `amux` library crate.
#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum CliDebugFormat {
    Yaml,
    Json,
}

impl From<CliDebugFormat> for protocol::DebugFormat {
    fn from(value: CliDebugFormat) -> Self {
        match value {
            CliDebugFormat::Yaml => protocol::DebugFormat::Yaml,
            CliDebugFormat::Json => protocol::DebugFormat::Json,
        }
    }
}

#[derive(Debug, Subcommand)]
enum HooksProvider {
    /// Claude Code hooks
    Claude {
        #[command(subcommand)]
        event: ClaudeHookEvent,
    },
}

#[derive(Debug, Subcommand)]
enum ClaudeHookEvent {
    SessionStart,
    SessionEnd,
    PermissionRequest,
    Stop,
    Notification,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _log_guard = init_tracing();

    let cli = Cli::parse();
    let Some(command) = cli.command else {
        print_top_level_help()?;
        return Ok(());
    };

    if handle_server_start_from_stdin(&command).await? {
        return Ok(());
    }

    let config = load_validated_config(&command, cli.config)?;

    run_command(command, config).await
}

fn print_top_level_help() -> Result<()> {
    let mut command = Cli::command();
    command.print_help()?;
    Ok(())
}

async fn handle_server_start_from_stdin(command: &Commands) -> Result<bool> {
    let Commands::Server {
        command:
            ServerCommands::Start {
                cloud,
                config_from_stdin: true,
                ..
            },
    } = command
    else {
        return Ok(false);
    };

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read config from stdin")?;
    let config: Config =
        serde_yaml::from_str(&input).context("failed to parse config from stdin")?;
    config
        .validate(*cloud)
        .map_err(|e| anyhow!("invalid config: {e}"))?;
    run_server(config, *cloud).await?;
    Ok(true)
}

fn load_validated_config(command: &Commands, path: Option<PathBuf>) -> Result<Config> {
    let config = load_config(path)?;
    config
        .validate(command_server_mode(command).is_cloud())
        .map_err(|e| anyhow!("invalid config: {e}"))?;
    Ok(config)
}

fn command_server_mode(command: &Commands) -> ServerMode {
    match command {
        Commands::Server {
            command: ServerCommands::Start { cloud: true, .. },
        } => ServerMode::Cloud,
        _ => ServerMode::Local,
    }
}

async fn run_command(command: Commands, mut config: Config) -> Result<()> {
    match command {
        Commands::New {
            agent_type,
            name,
            args,
        } => {
            let agent_type = parse_agent_type(&agent_type)?;
            ensure_initialized(&mut config).await?;
            check_upgrade_required(&config);
            match agent_type {
                protocol::AgentType::Claude => {
                    plugin::ensure_plugin_installed().await;
                }
                #[cfg(any(debug_assertions, test))]
                protocol::AgentType::TestAgent { .. } => {}
                protocol::AgentType::Unknown => {
                    unreachable!("CLI parser only constructs known agent types")
                }
            };
            session_client::new_agent(name.as_deref(), agent_type, args, &config).await?;
        }
        Commands::Attach { name } => {
            ensure_initialized(&mut config).await?;
            session_client::attach(name.as_deref(), &config).await?;
        }
        Commands::List => session_client::list_agents(&config).await?,
        Commands::Server { command } => match command {
            ServerCommands::Start {
                cloud, foreground, ..
            } => {
                ensure_initialized(&mut config).await?;
                let options = server_client::StartOptions::from_flags(cloud, foreground);
                server_client::start_server(&config, options).await?
            }
            ServerCommands::Stop => server_client::stop_server(&config).await?,
            ServerCommands::Connect { address } => {
                ensure_initialized(&mut config).await?;
                server_client::connect_remote(&address, &config).await?;
            }
            ServerCommands::Suspend => server_client::suspend_server(&config).await?,
            ServerCommands::Resume => server_client::resume_server(&config).await?,
        },
        Commands::Init { reset } => {
            let was_running = server_client::server_is_running(&config).await;
            init::run_init(&mut config, init::InitContext::explicit(), reset).await?;
            if was_running {
                println!();
                println!(
                    "Note: a server is running with the previous configuration. \
                     Restart it (`amux server stop` then `amux server start`) to apply any changes."
                );
            }
        }
        Commands::Update => update::run_update(&config).await?,
        Commands::Debug { verbose, format } => {
            let dump = server_client::debug(&config, verbose, format.into()).await?;
            print!("{dump}");
        }
        Commands::Hooks { provider } => match provider {
            HooksProvider::Claude { .. } => {
                hooks::handle_claude_hook(&config);
            }
        },
    }

    Ok(())
}

/// Ensure any pending init steps run before the current command executes.
///
/// Fast path: `init::needs_init` is a pure check against the in-memory config
/// plus a single `state.yaml` read — no extra IO when init is already done.
///
/// If init is needed but a server is already running, its config was frozen at
/// startup — prompting the user now would persist answers the running server
/// will never honour. Skip in that case; the command proceeds against the
/// already-running server.
async fn ensure_initialized(config: &mut Config) -> Result<()> {
    if !init::needs_init(config) {
        return Ok(());
    }
    if server_client::server_is_running(config).await {
        tracing::info!("init incomplete but server is running; skipping prompts");
        return Ok(());
    }
    println!("First-time setup required.\n");
    init::run_init(config, init::InitContext::implicit(), false)
        .await
        .context("initialization failed")?;
    Ok(())
}

// TODO: Once E2E executor can call amux/test-agent binaries directly (without
// path substitution), switch to Clap's ValueEnum for proper enum argument parsing.
fn parse_agent_type(s: &str) -> Result<protocol::AgentType> {
    #[cfg(any(debug_assertions, test))]
    let looks_like_test_agent_path = std::path::Path::new(s)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("test-agent"))
        .unwrap_or(false);

    match s.to_lowercase().as_str() {
        "claude" => Ok(protocol::AgentType::Claude),
        #[cfg(any(debug_assertions, test))]
        "test-agent" => Ok(protocol::AgentType::TestAgent {
            command: s.to_string(),
        }),
        #[cfg(any(debug_assertions, test))]
        _ if looks_like_test_agent_path => {
            // Accept full path for E2E tests (e.g., /abs/path/test-agent or test-agent.exe)
            Ok(protocol::AgentType::TestAgent {
                command: s.to_string(),
            })
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

/// Show a blocking warning if the cloud server requires a newer version.
/// Only shown when cloud mode is enabled and the user hasn't dismissed this version.
fn check_upgrade_required(config: &Config) {
    // Only relevant if cloud mode is enabled
    if !setup::cloud_enabled(config) {
        return;
    }

    let minimum_version = match amux::update::read_upgrade_required(&config.state_path) {
        Some(v) => v,
        None => return,
    };

    if amux::update::is_upgrade_dismissed(&config.state_path, &minimum_version) {
        return;
    }

    let current = env!("CARGO_PKG_VERSION");
    eprintln!("┌ Update required ──────────────────────────────────────────┐");
    eprintln!("│                                                           │");
    eprintln!("│  Your version ({current}) is below the minimum required");
    eprintln!("│  version ({minimum_version}) for cloud connectivity.");
    eprintln!("│                                                           │");
    eprintln!("│  Run 'amux update' to update.                             │");
    eprintln!("│                                                           │");
    eprintln!("│  Press Enter to continue, 'd' to dismiss permanently      │");
    eprintln!("│  (until next update), or Ctrl-C to exit.                  │");
    eprintln!("└───────────────────────────────────────────────────────────┘");

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_ok() && input.trim().eq_ignore_ascii_case("d") {
        amux::update::dismiss_upgrade(&config.state_path, &minimum_version);
    }
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
        Some(path) => Config::from_file(path)
            .map_err(|e| anyhow!("failed to load config from {:?}: {}", path, e))?,
        None => Config::new(),
    })
}
