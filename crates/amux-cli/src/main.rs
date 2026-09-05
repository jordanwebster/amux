mod client_common;
mod front_door;
mod hooks;
mod init;
mod keymap;
mod mcp;
mod profiles;
mod server_client;
mod session_client;
mod ui;
mod update;

use std::fs::OpenOptions;
use std::future::Future;
use std::io::{IsTerminal, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use amux::{AgentType, Config, PairingSecret, PairingStart};
#[cfg(debug_assertions)]
use amux_cli::debug_cmd::{self, DebugCommands};
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use qrcode::QrCode;
use qrcode::render::unicode;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::update::MarkerFileReporter;

const QR_PAIRING_DEEP_LINK_PREFIX: &str = "amux://pair?payload=";

/// Agent multiplexer - terminal multiplexer for AI agents
#[derive(Debug, Parser)]
#[command(name = "amux")]
#[command(about = "Terminal multiplexer for AI agents (Claude, Codex, etc.)", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to a profile config file (YAML). Also read from `AMUX_CONFIG`, so
    /// managed Claude hooks and daemons spawned by a wrapper find the same
    /// instance without adding a config flag to their command.
    #[arg(long, global = true, env = "AMUX_CONFIG")]
    config: Option<PathBuf>,

    /// Select a profile by label or UUID
    #[arg(long, global = true)]
    profile: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Open the fleet TUI (the default when run with no command)
    Ui,

    /// Create a new agent session
    New {
        /// Agent type: claude, codex, or test-agent (test-agent only in dev builds)
        agent_type: String,

        /// Claude driver (Claude agents only; defaults to pty)
        #[arg(long, value_enum)]
        driver: Option<CliClaudeDriver>,

        /// Session name (optional human-readable name)
        #[arg(long)]
        name: Option<String>,

        /// Codex model override (Codex agents only)
        #[arg(long)]
        model: Option<String>,

        /// Codex approval policy (Codex agents only)
        #[arg(long, value_enum)]
        approval_policy: Option<CliCodexApprovalPolicy>,

        /// Codex sandbox policy (Codex agents only)
        #[arg(long, value_enum)]
        sandbox_policy: Option<CliCodexSandboxPolicy>,

        /// Extra arguments passed to the agent (after --)
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Attach to an existing agent session
    Attach {
        /// Session name (default: first available)
        name: Option<String>,
    },

    /// Remove an agent session by exact name or UUID
    Rm {
        /// Exact agent name or UUID
        target: String,

        /// Remove the family even when a child is still working
        #[arg(long)]
        force: bool,
    },

    /// List running agent sessions, folding child agents into their parent
    #[command(alias = "ls")]
    List {
        /// Show child agents, indented beneath their parent
        #[arg(long)]
        all: bool,
    },

    /// Manage Claude PTY keymaps
    Keymap {
        #[command(subcommand)]
        command: KeymapCommands,
    },

    /// List profiles through the installation front door
    Profiles,

    /// Connect a cloud account, optionally to an explicit profile
    Login {
        /// Override the account label locally
        #[arg(long)]
        name: Option<String>,
    },

    /// Forget a profile's credential, preserving its device and local agents
    Logout,

    /// Manage profiles in this installation (select with --profile)
    Profile {
        #[command(subcommand)]
        command: profiles::ProfileCommands,
    },

    /// Manage the amux server lifecycle
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },

    /// Initialize local device preferences
    Init {
        /// Reset local setup preferences
        #[arg(long)]
        reset: bool,
    },

    /// Pair this device with another amux daemon
    Pair {
        /// Display a QR pairing code for this device
        #[arg(long, conflicts_with_all = ["listen", "connect"])]
        #[cfg_attr(unix, arg(conflicts_with = "via_ssh"))]
        qr: bool,

        /// Also print the QR deep link for simulator pairing
        #[arg(long, requires = "qr")]
        #[cfg_attr(not(debug_assertions), arg(hide = true))]
        link: bool,

        /// Require LAN-direct responder mode; errors when tcp_port is unset
        #[arg(long, conflicts_with_all = ["qr", "connect"])]
        #[cfg_attr(unix, arg(conflicts_with = "via_ssh"))]
        listen: bool,

        /// Initiate PIN pairing to a direct target or online cloud host
        #[arg(long, value_name = "TARGET", num_args = 0..=1, conflicts_with_all = ["qr", "listen"])]
        #[cfg_attr(unix, arg(conflicts_with = "via_ssh"))]
        connect: Option<Option<String>>,

        /// Pair through SSH and store the target for future SSH runtime links
        #[cfg(unix)]
        #[arg(long = "via-ssh", value_name = "TARGET", conflicts_with_all = ["qr", "listen", "connect", "demo"])]
        via_ssh: Option<String>,

        /// Hold a reusable fixed PIN open for unattended demos (returns immediately;
        /// the daemon keeps the session until it expires or `amux pair --cancel`)
        #[arg(long, requires_all = ["pin", "for"], conflicts_with_all = ["qr", "listen", "connect", "cancel"])]
        demo: bool,

        /// Six-digit PIN for `--demo`
        #[arg(long, value_name = "DIGITS", requires = "demo")]
        pin: Option<String>,

        /// How long the `--demo` PIN stays valid, e.g. `30d`, `12h`, `45m`
        #[arg(long = "for", value_name = "DURATION", requires = "demo", value_parser = parse_pairing_duration)]
        r#for: Option<Duration>,

        /// End any active pairing session on this daemon
        #[arg(long, conflicts_with_all = ["qr", "listen", "connect", "demo"])]
        cancel: bool,
    },

    /// Show trusted peers
    Peer {
        #[command(subcommand)]
        command: PeerCommands,
    },

    /// Remove trust for a peer
    Unpair {
        /// Peer host ID or unique display name
        peer: String,

        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },

    /// Internal: receive an SSH pairing identity exchange over stdin/stdout
    #[cfg(unix)]
    #[command(name = "pair-recv", hide = true)]
    PairRecv,

    /// Internal: bridge SSH stdin/stdout to the local daemon socket
    #[cfg(unix)]
    #[command(hide = true)]
    Relay,

    /// Internal: Handle hooks from AI coding assistants
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        provider: HooksProvider,
    },

    /// Internal: Serve agent tools over the Model Context Protocol
    #[command(hide = true)]
    Mcp {
        #[command(subcommand)]
        provider: McpProvider,
    },

    /// Update amux to the latest version
    Update,

    /// Inspect daemon state and locally captured reports
    #[cfg(debug_assertions)]
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },
}

#[derive(Debug, Subcommand)]
enum PeerCommands {
    /// List trusted peers
    List,

    /// Show trusted peer details
    Info {
        /// Peer host ID or unique display name
        peer: String,
    },
}

#[derive(Debug, Subcommand)]
enum KeymapCommands {
    /// List effective keymaps and their basis for the installed Claude version
    List,

    /// Print an effective keymap
    Show {
        /// Declared keymap name
        name: String,
    },

    /// Validate and install a user keymap
    Add {
        /// TOML keymap file
        file: PathBuf,
    },

    /// Remove an installed user keymap
    Remove {
        /// Declared keymap name
        name: String,
    },

    /// Print the user keymap directory
    Dir,
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

        /// Read config from stdin (YAML format). Used by CLI daemon spawning.
        #[arg(long, hide = true)]
        config_from_stdin: bool,

        /// Preserve the source path of config serialized over stdin.
        #[arg(long, hide = true, requires = "config_from_stdin")]
        config_path: Option<PathBuf>,
    },

    /// Shut down the server and all running agent sessions
    Stop,

    /// Internal: Suspend all agents and stop the server
    #[command(hide = true)]
    Suspend,

    /// Internal: Resume suspended agents
    #[command(hide = true)]
    Resume,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliClaudeDriver {
    Pty,
    Sdk,
}

impl From<CliClaudeDriver> for amux::ClaudeDriver {
    fn from(value: CliClaudeDriver) -> Self {
        match value {
            CliClaudeDriver::Pty => Self::Pty,
            CliClaudeDriver::Sdk => Self::Sdk,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliCodexApprovalPolicy {
    Untrusted,
    OnRequest,
    Never,
}

impl CliCodexApprovalPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliCodexSandboxPolicy {
    DangerFullAccess,
    WorkspaceWrite,
    ReadOnly,
}

impl CliCodexSandboxPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::DangerFullAccess => "danger-full-access",
            Self::WorkspaceWrite => "workspace-write",
            Self::ReadOnly => "read-only",
        }
    }
}

#[derive(Debug, Subcommand)]
enum HooksProvider {
    /// Claude Code hooks
    Claude,
}

#[derive(Debug, Subcommand)]
enum McpProvider {
    /// Agent-tool stdio server
    Agent {
        /// Exact daemon endpoint selected by the managed session.
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let _log_guard = init_tracing();

    let cli = Cli::parse();
    let Some(command) = cli.command else {
        // Bare `amux` opens the fleet TUI (init-first on a fresh machine —
        // the dispatch decides before the TUI ever starts). Without a real
        // terminal (scripts, pipes, e2e) it prints help instead, like any
        // bare CLI; explicit `amux ui` still errors honestly there.
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            Cli::command().print_help()?;
            return Ok(ExitCode::SUCCESS);
        }
        if cli.config.is_none() && !amux::InstallationConfig::default_path().exists() {
            init::initialize(None, false).await?;
        }
        let config = profiles::configuration(cli.config.as_deref(), cli.profile.as_deref()).await?;
        config
            .validate()
            .map_err(|e| anyhow!("invalid config: {e}"))?;
        ui::run(config).await?;
        return Ok(ExitCode::SUCCESS);
    };

    if matches!(command, Commands::Hooks { .. }) && std::env::var_os("CLAUDE_HOOK_SOCKET").is_some()
    {
        hooks::handle_claude_hook(None);
        return Ok(ExitCode::SUCCESS);
    }

    if handle_server_start_from_stdin(&command).await? {
        return Ok(ExitCode::SUCCESS);
    }

    if let Commands::Login { name } = command {
        if cli.config.is_none() && !amux::InstallationConfig::default_path().exists() {
            init::initialize(None, false).await?;
        }
        let installation = front_door::configuration(cli.config.as_deref())?;
        let cloud_url = match cli.config.as_deref() {
            Some(path) => profiles::load(path)?.cloud_url,
            None => Config::default().cloud_url,
        };
        profiles::login(&installation, &cloud_url, cli.profile.as_deref(), name).await?;
        return Ok(ExitCode::SUCCESS);
    }

    if let Commands::Profile { command } = command {
        let installation = front_door::configuration(cli.config.as_deref())?;
        let configured = configured_profile(cli.config.as_deref())?;
        profiles::administer(
            &installation,
            cli.profile.as_deref().or(configured.as_deref()),
            command,
        )
        .await?;
        return Ok(ExitCode::SUCCESS);
    }

    match &command {
        Commands::Logout => {
            let installation = front_door::configuration(cli.config.as_deref())?;
            let configured = configured_profile(cli.config.as_deref())?;
            profiles::logout(
                &installation,
                cli.profile.as_deref().or(configured.as_deref()),
            )
            .await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Init { reset } => {
            init::initialize(cli.config.as_deref(), *reset).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Keymap { .. } | Commands::Update => {
            let installation = front_door::configuration(cli.config.as_deref())?;
            match command {
                Commands::Keymap { command } => {
                    keymap::run(command, &installation.keymaps_dir).await?
                }
                Commands::Update => update::run_update(&installation).await?,
                _ => unreachable!(),
            }
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Profiles => {
            let config = front_door::configuration(cli.config.as_deref())?;
            front_door::list(&config).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Server {
            command:
                ServerCommands::Start {
                    cloud: false,
                    foreground,
                    ..
                },
        } => {
            let config = front_door::configuration(cli.config.as_deref())?;
            front_door::start(config, *foreground).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Server {
            command: ServerCommands::Suspend,
        } => {
            let config = front_door::configuration(cli.config.as_deref())?;
            front_door::suspend(&config).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Server {
            command: ServerCommands::Resume,
        } => {
            let config = front_door::configuration(cli.config.as_deref())?;
            front_door::resume(&config).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Server {
            command: ServerCommands::Stop,
        } => {
            let config = front_door::configuration(cli.config.as_deref())?;
            front_door::stop(&config).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Server {
            command: ServerCommands::Start { cloud: true, .. },
        } => {
            let path = cli.config.unwrap_or_else(Config::default_path);
            return run_command(command, Config::from_file(&path)?).await;
        }
        _ => {}
    }
    let config = if matches!(command, Commands::Mcp { .. }) {
        let path = cli
            .config
            .as_deref()
            .context("MCP requires AMUX_CONFIG or --config pointing at a profile config")?;
        if cli.profile.is_some() {
            return Err(anyhow!(
                "MCP uses its profile config; --profile cannot override the launch route"
            ));
        }
        profiles::load(path)?
    } else if matches!(command, Commands::Hooks { .. })
        && cli.config.is_some()
        && cli.profile.is_none()
    {
        profiles::load(cli.config.as_deref().unwrap())?
    } else {
        if matches!(command, Commands::Ui)
            && cli.config.is_none()
            && !amux::InstallationConfig::default_path().exists()
        {
            init::initialize(None, false).await?;
        }
        profiles::configuration(cli.config.as_deref(), cli.profile.as_deref()).await?
    };
    run_command(command, config).await
}

async fn handle_server_start_from_stdin(command: &Commands) -> Result<bool> {
    let Commands::Server {
        command:
            ServerCommands::Start {
                cloud,
                config_from_stdin: true,
                config_path,
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
    if !cloud {
        let mut config: amux::InstallationConfig = serde_yaml::from_str(&input)
            .context("failed to parse installation config from stdin")?;
        config.path = config_path
            .as_deref()
            .map(normalize_config_path)
            .transpose()?;
        front_door::run(config).await?;
        return Ok(true);
    }
    let mut config: Config =
        serde_yaml::from_str(&input).context("failed to parse relay config from stdin")?;
    config.path = config_path
        .as_deref()
        .map(normalize_config_path)
        .transpose()?;
    config
        .validate()
        .map_err(|e| anyhow!("invalid config: {e}"))?;
    server_client::run_relay_foreground(config).await?;
    Ok(true)
}

fn configured_profile(path: Option<&std::path::Path>) -> Result<Option<String>> {
    path.map(|path| {
        Ok(amux::load_profile_config(&std::fs::canonicalize(path)?)?
            .profile_id
            .to_string())
    })
    .transpose()
}

async fn run_command(command: Commands, mut config: Config) -> Result<ExitCode> {
    match command {
        Commands::Profiles
        | Commands::Profile { .. }
        | Commands::Login { .. }
        | Commands::Logout => unreachable!("profiles dispatches before profile configuration"),
        Commands::Ui => ui::run(config).await?,
        Commands::New {
            agent_type,
            driver,
            name,
            model,
            approval_policy,
            sandbox_policy,
            args,
        } => {
            let open_mode = config.ui.default_open_mode;
            run_new_agent_command(
                open_mode,
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
                move || async move {
                    let agent_type = configure_agent_type(
                        parse_agent_type(&agent_type)?,
                        driver,
                        model,
                        approval_policy,
                        sandbox_policy,
                    )?;
                    ensure_initialized(&mut config).await?;
                    check_update_required(&config);
                    session_client::new_agent(name.as_deref(), agent_type, args, &config).await
                },
            )
            .await?;
        }
        Commands::Attach { name } => {
            ensure_initialized(&mut config).await?;
            session_client::attach(name.as_deref(), &config).await?;
        }
        Commands::Rm { target, force } => {
            session_client::remove_agent(&target, force, &config).await?
        }
        Commands::List { all } => session_client::list_agents(all, &config).await?,
        Commands::Keymap { .. } => unreachable!("keymaps dispatches before profile configuration"),
        Commands::Server { command } => match command {
            ServerCommands::Start {
                cloud: true,
                foreground,
                ..
            } => server_client::start_relay(&config, foreground).await?,
            _ => unreachable!("installation lifecycle dispatches before profile configuration"),
        },
        Commands::Init { .. } => unreachable!("init dispatches before profile configuration"),
        Commands::Pair {
            qr,
            link,
            listen,
            connect,
            #[cfg(unix)]
            via_ssh,
            demo,
            pin,
            r#for,
            cancel,
        } => {
            validate_pair_qr_link_usage(link, cfg!(debug_assertions))?;
            if cancel {
                ensure_initialized(&mut config).await?;
                let client = front_door::profile_admin(&config, None).await?;
                client.cancel_pairing().await?;
                println!("Pairing mode cancelled.");
                return Ok(ExitCode::SUCCESS);
            }
            if demo {
                let (Some(pin), Some(ttl)) = (pin, r#for) else {
                    unreachable!("clap enforces --pin and --for with --demo");
                };
                ensure_initialized(&mut config).await?;
                let client = front_door::profile_admin(
                    &config,
                    Some("amux pair --demo --pin <DIGITS> --for <DURATION>"),
                )
                .await?;
                let pairing = client.start_demo_pin_pairing(pin, ttl).await?;
                print_pairing_start(&pairing, false)?;
                println!(
                    "Demo pairing: this PIN pairs any device that presents it, repeatedly, \
                     until it expires or `amux pair --cancel`. It does not survive a daemon restart."
                );
                return Ok(ExitCode::SUCCESS);
            }
            if let Some(connect_target) = connect {
                match parse_pair_connect_target(connect_target) {
                    PairConnectTarget::Picker => {
                        ensure_initialized(&mut config).await?;
                        let client =
                            front_door::profile_admin(&config, Some("amux pair --connect")).await?;
                        let hosts = sorted_pairing_hosts(client.list_pairing_hosts().await?);
                        let host = prompt_pairing_host(&hosts)?;
                        let peer = pair_cloud_host(&client, &host).await?;
                        println!("Paired with {} ({}) via cloud.", peer.name, peer.host_id);
                        return Ok(ExitCode::SUCCESS);
                    }
                    PairConnectTarget::CloudName(target) => {
                        ensure_initialized(&mut config).await?;
                        let retry_command = format!("amux pair --connect {target}");
                        let client =
                            front_door::profile_admin(&config, Some(&retry_command)).await?;
                        let hosts = sorted_pairing_hosts(client.list_pairing_hosts().await?);
                        let host = resolve_pairing_host_by_name(&hosts, &target)?;
                        let peer = pair_cloud_host(&client, &host).await?;
                        println!("Paired with {} ({}) via cloud.", peer.name, peer.host_id);
                        return Ok(ExitCode::SUCCESS);
                    }
                    PairConnectTarget::Direct(addr) => {
                        ensure_initialized(&mut config).await?;
                        let retry_command = format!("amux pair --connect {addr}");
                        let client =
                            front_door::profile_admin(&config, Some(&retry_command)).await?;
                        let pin = prompt_pairing_pin()?;
                        let peer = amux::pair_via_pin_direct_tcp(
                            config.data_dir.clone(),
                            &config.host_name,
                            addr,
                            &pin,
                            &client,
                        )
                        .await?;
                        println!(
                            "Paired with {} ({}) via direct TCP.",
                            peer.name, peer.host_id
                        );
                        return Ok(ExitCode::SUCCESS);
                    }
                }
            }

            ensure_initialized(&mut config).await?;
            #[cfg(unix)]
            if let Some(target) = via_ssh {
                let retry_command = format!("amux pair --via-ssh {target}");
                let client = front_door::profile_admin(&config, Some(&retry_command)).await?;
                let peer = amux::pair_via_ssh_target(
                    config.data_dir.clone(),
                    &config.host_name,
                    target,
                    &client,
                )
                .await?;
                println!("Paired with {} ({}) via SSH.", peer.name, peer.host_id);
                return Ok(ExitCode::SUCCESS);
            }

            let retry_command = pair_start_retry_command(qr, listen);
            let client = front_door::profile_admin(&config, Some(retry_command)).await?;
            if listen && config.tcp_port.is_none() {
                return Err(anyhow!(
                    "set `tcp_port` in your config, or use cloud / SSH pairing"
                ));
            }
            let pairing = if qr {
                client.start_qr_pairing().await?
            } else if listen {
                client.start_lan_pin_pairing().await?
            } else {
                client.start_pin_pairing().await?
            };
            if let Err(error) = print_pairing_start(&pairing, link) {
                client.cancel_pairing().await?;
                return Err(error);
            }
            wait_for_pairing_mode_to_end(&client, pairing.ttl_seconds).await?;
        }
        Commands::Peer { command } => {
            ensure_initialized(&mut config).await?;
            let client = front_door::profile_admin(&config, None).await?;
            match command {
                PeerCommands::List => {
                    let peers = client.list_peers().await?;
                    print!("{}", format_peer_list(&peers));
                }
                PeerCommands::Info { peer } => {
                    let peer = client.get_peer(peer.as_str()).await?;
                    print!("{}", format_peer_info(&peer));
                }
            }
        }
        Commands::Unpair { peer, force } => {
            ensure_initialized(&mut config).await?;
            let client = front_door::profile_admin(&config, None).await?;
            let entry = client.get_peer(peer.as_str()).await?;
            if !force && !confirm_unpair(&entry)? {
                println!("Unpair cancelled.");
                return Ok(ExitCode::SUCCESS);
            }
            let removed = client.unpair(peer.as_str(), "user").await?;
            println!("Unpaired {} ({}).", removed.name, removed.host_id);
        }
        #[cfg(unix)]
        Commands::PairRecv => {
            let client = front_door::profile_admin(&config, None).await?;
            amux::pair_via_ssh_responder_stdio(config.data_dir.clone(), &config.host_name, &client)
                .await?;
        }
        #[cfg(unix)]
        Commands::Relay => {
            amux::relay_stdio_to_unix_socket(&config.socket_path).await?;
        }
        Commands::Update => unreachable!("update dispatches before profile configuration"),
        #[cfg(debug_assertions)]
        Commands::Debug { command } => match command {
            DebugCommands::Daemon { verbose, format } => {
                let dump = server_client::debug(&config, verbose, format.into()).await?;
                print!("{dump}");
            }
            DebugCommands::Report { command } => {
                let output = debug_cmd::run_report(command, &config)?;
                print!("{}", output.text);
                if output.exit_code != ExitCode::SUCCESS {
                    return Ok(output.exit_code);
                }
            }
        },
        Commands::Hooks { provider } => match provider {
            HooksProvider::Claude => {
                hooks::handle_claude_hook(Some(&config));
            }
        },
        Commands::Mcp { provider } => match provider {
            McpProvider::Agent { socket_path } => {
                mcp::serve_agent(&config, socket_path.as_deref()).await?
            }
        },
    }

    Ok(ExitCode::SUCCESS)
}

async fn run_new_agent_command<Action, ActionFuture>(
    open_mode: amux::OpenMode,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    action: Action,
) -> Result<()>
where
    Action: FnOnce() -> ActionFuture,
    ActionFuture: Future<Output = Result<()>>,
{
    if new_agent_opens_interactively(open_mode) && !(stdin_is_terminal && stdout_is_terminal) {
        return Err(anyhow!(
            "`amux new` must run in an interactive terminal because it opens the new agent immediately"
        ));
    }
    action().await
}

fn new_agent_opens_interactively(open_mode: amux::OpenMode) -> bool {
    match open_mode {
        amux::OpenMode::Chat | amux::OpenMode::Raw => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PairConnectTarget {
    Picker,
    Direct(SocketAddr),
    CloudName(String),
}

fn format_peer_list(peers: &[amux::PeerEntry]) -> String {
    if peers.is_empty() {
        return "No trusted peers.\n".to_string();
    }
    let mut output = String::from("Trusted peers:\n");
    for peer in peers {
        output.push_str(&format!(
            "  {}  {}  {}\n",
            peer.host_id,
            peer.name,
            format_peer_reachabilities(&peer.reachabilities)
        ));
    }
    output
}

fn format_peer_info(peer: &amux::PeerEntry) -> String {
    format!(
        "Host ID: {}\nName: {}\nPaired at: {}\nPubkey: {}\nReachability: {}\n",
        peer.host_id,
        peer.name,
        peer.paired_at.to_rfc3339(),
        hex_encode(&peer.pubkey),
        format_peer_reachabilities(&peer.reachabilities)
    )
}

fn format_peer_reachabilities(reachabilities: &[amux::PeerReachability]) -> String {
    if reachabilities.is_empty() {
        return "none".to_string();
    }
    reachabilities
        .iter()
        .map(|reachability| match reachability {
            amux::PeerReachability::Cloud => "cloud".to_string(),
            amux::PeerReachability::Ssh { target } => format!("ssh:{target}"),
            amux::PeerReachability::DirectTcp { addr } => format!("direct-tcp:{addr}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn confirm_unpair(peer: &amux::PeerEntry) -> Result<bool> {
    print!("Unpair {} ({})? [y/N]: ", peer.name, peer.host_id);
    std::io::stdout()
        .flush()
        .context("failed to flush unpair prompt")?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read unpair confirmation")?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn parse_pair_connect_target(target: Option<String>) -> PairConnectTarget {
    match target {
        Some(target) => match target.parse::<SocketAddr>() {
            Ok(addr) => PairConnectTarget::Direct(addr),
            Err(_) => PairConnectTarget::CloudName(target),
        },
        None => PairConnectTarget::Picker,
    }
}

fn prompt_pairing_pin() -> Result<String> {
    print!("PIN: ");
    std::io::stdout()
        .flush()
        .context("failed to flush PIN prompt")?;
    let mut pin = String::new();
    std::io::stdin()
        .read_line(&mut pin)
        .context("failed to read PIN")?;
    Ok(pin.trim().to_string())
}

fn format_pairing_ttl(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 && s >= 86_400 => format!("{} days", s / 86_400),
        s if s % 3_600 == 0 && s >= 3_600 => format!("{} hours", s / 3_600),
        s if s % 60 == 0 && s >= 60 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

/// Parse `30d` / `12h` / `45m` / `90s` (a bare number is seconds).
fn parse_pairing_duration(input: &str) -> Result<Duration, String> {
    let input = input.trim();
    let (digits, unit) = match input.find(|c: char| !c.is_ascii_digit()) {
        Some(index) => input.split_at(index),
        None => (input, "s"),
    };
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid duration `{input}`: expected e.g. 30d, 12h, 45m"))?;
    let multiplier = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        other => {
            return Err(format!(
                "invalid duration unit `{other}`: use s, m, h, or d"
            ));
        }
    };
    let seconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("duration `{input}` is too large"))?;
    if seconds == 0 {
        return Err("duration must be positive".to_string());
    }
    Ok(Duration::from_secs(seconds))
}

fn pair_start_retry_command(qr: bool, listen: bool) -> &'static str {
    if qr {
        "amux pair --qr"
    } else if listen {
        "amux pair --listen"
    } else {
        "amux pair"
    }
}

fn validate_pair_qr_link_usage(link: bool, debug_build: bool) -> Result<()> {
    if link && !debug_build {
        return Err(anyhow!(
            "`amux pair --qr --link` is only available in debug builds"
        ));
    }
    Ok(())
}

async fn pair_cloud_host(
    client: &amux::installation::ProfileAdminClient,
    host: &amux::HostEntry,
) -> Result<amux::SshPairingPeer> {
    let pin = prompt_pairing_pin()?;
    client
        .pair_pin_cloud_peer(host.id, pin)
        .await
        .with_context(|| format!("failed to pair with cloud host {} ({})", host.name, host.id))
}

fn sorted_pairing_hosts(mut hosts: Vec<amux::HostEntry>) -> Vec<amux::HostEntry> {
    hosts.sort_unstable_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    hosts
}

fn resolve_pairing_host_by_name(
    hosts: &[amux::HostEntry],
    target: &str,
) -> Result<amux::HostEntry> {
    if let Ok(id) = uuid::Uuid::parse_str(target)
        && let Some(host) = hosts.iter().find(|host| host.id == id)
    {
        return Ok(host.clone());
    }

    let matches = hosts
        .iter()
        .filter(|host| host.name == target)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [host] => Ok((*host).clone()),
        [] => Err(anyhow!(
            "no online cloud host named `{target}`. Run `amux pair --connect` to choose from currently visible hosts."
        )),
        _ => Err(anyhow!(
            "multiple online cloud hosts are named {target}; use the host ID shown by `amux pair --connect`"
        )),
    }
}

fn prompt_pairing_host(hosts: &[amux::HostEntry]) -> Result<amux::HostEntry> {
    if hosts.is_empty() {
        return Err(anyhow!("no online cloud hosts are available for pairing"));
    }

    println!("Cloud hosts:");
    for (index, host) in hosts.iter().enumerate() {
        println!("  {}. {} ({})", index + 1, host.name, host.id);
    }
    print!("Select host: ");
    std::io::stdout()
        .flush()
        .context("failed to flush host selection prompt")?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read host selection")?;
    let selected = parse_pairing_host_selection(&input, hosts.len())?;
    Ok(hosts[selected].clone())
}

fn parse_pairing_host_selection(input: &str, host_count: usize) -> Result<usize> {
    if host_count == 0 {
        return Err(anyhow!("no online cloud hosts are available for pairing"));
    }
    let trimmed = input.trim();
    let selection = trimmed
        .parse::<usize>()
        .with_context(|| format!("invalid host selection {trimmed:?}"))?;
    if !(1..=host_count).contains(&selection) {
        return Err(anyhow!(
            "host selection {selection} is out of range; choose 1-{host_count}"
        ));
    }
    Ok(selection - 1)
}

fn print_pairing_start(pairing: &PairingStart, print_link: bool) -> Result<()> {
    match &pairing.secret {
        PairingSecret::Pin(pin) => {
            println!("Pairing PIN: {pin}");
            if let Some(port) = pairing.tcp_port {
                println!("LAN direct listener: tcp_port {port}");
            }
        }
        PairingSecret::QrSecret(secret) => {
            let payload = qr_pairing_payload(pairing, secret)?;
            println!("{}", terminal_qr_code(&payload)?);
            if print_link {
                println!("Pairing link: {payload}");
            }
        }
    }
    println!(
        "Pairing mode active for {}.",
        format_pairing_ttl(pairing.ttl_seconds)
    );
    Ok(())
}

fn qr_pairing_payload(pairing: &PairingStart, secret: &[u8]) -> Result<String> {
    let payload = amux::encode_qr_pairing_payload(pairing, secret)
        .context("failed to encode QR pairing payload")?;
    let encoded = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    Ok(format!("{QR_PAIRING_DEEP_LINK_PREFIX}{encoded}"))
}

fn terminal_qr_code(payload: &str) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).context("failed to encode terminal QR")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

async fn wait_for_pairing_mode_to_end(
    client: &amux::installation::ProfileAdminClient,
    ttl_seconds: u64,
) -> Result<()> {
    let ttl = Duration::from_secs(ttl_seconds);
    let poll_interval = Duration::from_secs(1);
    let started_at = tokio::time::Instant::now();
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            result = &mut ctrl_c => {
                result.context("failed to listen for Ctrl-C")?;
                client.cancel_pairing().await?;
                println!("Pairing mode cancelled.");
                return Ok(());
            }
            _ = tokio::time::sleep(poll_interval) => {
                if !client.pairing_is_active().await? {
                    println!("Pairing mode ended.");
                    return Ok(());
                }
                if started_at.elapsed() >= ttl {
                    return Ok(());
                }
            }
        }
    }
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
fn parse_agent_type(s: &str) -> Result<AgentType> {
    #[cfg(any(debug_assertions, test))]
    let looks_like_test_agent_path = std::path::Path::new(s)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("test-agent"))
        .unwrap_or(false);

    match s.to_lowercase().as_str() {
        "claude" => Ok(AgentType::Claude {
            driver: amux::ClaudeDriver::Pty,
        }),
        "codex" => Ok(AgentType::Codex {
            model: None,
            approval_policy: None,
            sandbox_policy: None,
            resume_thread_id: None,
        }),
        #[cfg(any(debug_assertions, test))]
        "test-agent" => Ok(AgentType::TestAgent {
            command: s.to_string(),
        }),
        #[cfg(any(debug_assertions, test))]
        _ if looks_like_test_agent_path => {
            // Accept full path for E2E tests (e.g., /abs/path/test-agent or test-agent.exe)
            Ok(AgentType::TestAgent {
                command: s.to_string(),
            })
        }
        #[cfg(not(any(debug_assertions, test)))]
        _ => Err(anyhow!("Unknown agent type: '{}'. Valid: claude, codex", s)),
        #[cfg(any(debug_assertions, test))]
        _ => Err(anyhow!(
            "Unknown agent type: '{}'. Valid: claude, codex, test-agent",
            s
        )),
    }
}

fn configure_agent_type(
    agent_type: AgentType,
    driver: Option<CliClaudeDriver>,
    model: Option<String>,
    approval_policy: Option<CliCodexApprovalPolicy>,
    sandbox_policy: Option<CliCodexSandboxPolicy>,
) -> Result<AgentType> {
    match agent_type {
        AgentType::Claude { .. } => {
            if model.is_some() || approval_policy.is_some() || sandbox_policy.is_some() {
                return Err(anyhow!(
                    "--model, --approval-policy, and --sandbox-policy require agent type `codex`"
                ));
            }
            Ok(AgentType::Claude {
                driver: driver.unwrap_or(CliClaudeDriver::Pty).into(),
            })
        }
        AgentType::Codex {
            resume_thread_id, ..
        } => {
            if driver.is_some() {
                return Err(anyhow!("--driver requires agent type `claude`"));
            }
            Ok(AgentType::Codex {
                model,
                approval_policy: approval_policy.map(|value| value.as_str().to_string()),
                sandbox_policy: sandbox_policy.map(|value| value.as_str().to_string()),
                resume_thread_id,
            })
        }
        #[cfg(any(debug_assertions, test))]
        AgentType::TestAgent { command } => {
            if driver.is_some() {
                return Err(anyhow!("--driver requires agent type `claude`"));
            }
            if model.is_some() || approval_policy.is_some() || sandbox_policy.is_some() {
                return Err(anyhow!(
                    "--model, --approval-policy, and --sandbox-policy require agent type `codex`"
                ));
            }
            Ok(AgentType::TestAgent { command })
        }
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
/// Only shown when the user has not dismissed this version.
fn check_update_required(config: &Config) {
    let reporter = MarkerFileReporter::from_state_path(&config.state_path);
    let current = env!("CARGO_PKG_VERSION");
    let minimum_version = match reporter.read_active_update_required(current) {
        Some(v) => v,
        None => return,
    };

    if reporter.is_update_dismissed(&minimum_version) {
        return;
    }

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
        reporter.dismiss_update_required(&minimum_version);
    }
}

fn normalize_config_path(path: &std::path::Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to determine the current directory for --config")?
            .join(path)
    };
    let normalized = std::fs::canonicalize(&absolute)
        .with_context(|| format!("failed to resolve config path {}", absolute.display()))?;
    normalized
        .to_str()
        .ok_or_else(|| anyhow!("config path is not valid UTF-8: {}", normalized.display()))?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_selector_is_global_for_ui_relay_and_administration() {
        for args in [
            vec!["amux", "ui", "--profile", "Work"],
            vec!["amux", "relay", "--profile", "Work"],
            vec!["amux", "--profile", "Work", "profile", "rename", "Office"],
            vec!["amux", "login", "--profile", "Work", "--name", "Office"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.profile.as_deref(), Some("Work"));
        }
        assert!(Cli::try_parse_from(["amux", "profile", "rename"]).is_err());
        assert!(Cli::try_parse_from(["amux", "profile", "rename", "Office", "--clear"]).is_err());
    }

    #[test]
    fn top_level_help_mentions_debug_exactly_in_debug_builds() {
        let help = Cli::command().render_long_help().to_string();
        let debug_entries = help
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line == "debug" || line.starts_with("debug ")
            })
            .count();

        assert_eq!(debug_entries, usize::from(cfg!(debug_assertions)), "{help}");
    }

    /// The config flag doubles as `AMUX_CONFIG`. Checked through the clap
    /// command model rather than by setting the variable, which would race
    /// with other tests in the same process.
    #[test]
    fn config_flag_reads_amux_config_env() {
        let command = Cli::command();
        let arg = command
            .get_arguments()
            .find(|arg| arg.get_id() == "config")
            .expect("--config arg");
        assert_eq!(arg.get_env(), Some(std::ffi::OsStr::new("AMUX_CONFIG")));
        assert!(arg.is_global_set());
    }

    /// The managed Claude hook has no config flag; it reaches the right
    /// instance through the inherited global argument environment.
    #[test]
    fn config_flag_is_accepted_after_hooks_subcommand() {
        let cli =
            Cli::try_parse_from(["amux", "hooks", "claude", "--config", "/x/cfg.yaml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/x/cfg.yaml")));
        assert!(matches!(
            cli.command,
            Some(Commands::Hooks {
                provider: HooksProvider::Claude
            })
        ));
    }

    #[test]
    fn mcp_agent_parses_an_exact_socket_and_rejects_the_retired_provider_name() {
        let cli = Cli::try_parse_from([
            "amux",
            "mcp",
            "agent",
            "--socket-path",
            "/runtime/amux.sock",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Mcp {
                provider: McpProvider::Agent { socket_path: Some(ref path) }
            }) if path == std::path::Path::new("/runtime/amux.sock")
        ));
        assert!(Cli::try_parse_from(["amux", "mcp", "claude"]).is_err());
    }

    #[test]
    fn stdin_server_config_accepts_only_explicit_source_metadata() {
        let cli = Cli::try_parse_from([
            "amux",
            "server",
            "start",
            "--foreground",
            "--config-from-stdin",
            "--config-path",
            "/checkout/amux.yaml",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Server {
                command: ServerCommands::Start {
                    config_from_stdin: true,
                    config_path: Some(ref path),
                    ..
                }
            }) if path == std::path::Path::new("/checkout/amux.yaml")
        ));
        assert!(
            Cli::try_parse_from([
                "amux",
                "server",
                "start",
                "--config-path",
                "/checkout/amux.yaml",
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_codex_agent_type() {
        assert!(matches!(
            parse_agent_type("codex").unwrap(),
            AgentType::Codex {
                model: None,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            }
        ));
    }

    #[test]
    fn parses_claude_driver_and_defaults_to_pty() {
        for (args, expected) in [
            (vec!["amux", "new", "claude"], amux::ClaudeDriver::Pty),
            (
                vec!["amux", "new", "claude", "--driver", "pty"],
                amux::ClaudeDriver::Pty,
            ),
            (
                vec!["amux", "new", "claude", "--driver", "sdk"],
                amux::ClaudeDriver::Sdk,
            ),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Commands::New {
                agent_type,
                driver,
                model,
                approval_policy,
                sandbox_policy,
                ..
            }) = cli.command
            else {
                panic!("expected new command");
            };
            let configured = configure_agent_type(
                parse_agent_type(&agent_type).unwrap(),
                driver,
                model,
                approval_policy,
                sandbox_policy,
            )
            .unwrap();
            assert_eq!(configured, AgentType::Claude { driver: expected });
        }

        assert!(Cli::try_parse_from(["amux", "new", "claude", "--driver", "unknown"]).is_err());
    }

    #[test]
    fn parses_and_applies_codex_creation_options() {
        let cli = Cli::try_parse_from([
            "amux",
            "new",
            "codex",
            "--model",
            "gpt-5.4",
            "--approval-policy",
            "on-request",
            "--sandbox-policy",
            "workspace-write",
        ])
        .unwrap();
        let Some(Commands::New {
            agent_type,
            driver,
            model,
            approval_policy,
            sandbox_policy,
            ..
        }) = cli.command
        else {
            panic!("expected new command");
        };
        let configured = configure_agent_type(
            parse_agent_type(&agent_type).unwrap(),
            driver,
            model,
            approval_policy,
            sandbox_policy,
        )
        .unwrap();
        assert!(matches!(
            configured,
            AgentType::Codex {
                model: Some(ref model),
                approval_policy: Some(ref approval),
                sandbox_policy: Some(ref sandbox),
                ..
            } if model == "gpt-5.4" && approval == "on-request" && sandbox == "workspace-write"
        ));
    }

    #[tokio::test]
    async fn new_non_tty_preflight_never_invokes_creation() {
        for open_mode in [amux::OpenMode::Chat, amux::OpenMode::Raw] {
            for (stdin_is_terminal, stdout_is_terminal) in [(false, true), (true, false)] {
                let create_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let observed_calls = create_calls.clone();
                let error = run_new_agent_command(
                    open_mode,
                    stdin_is_terminal,
                    stdout_is_terminal,
                    move || async move {
                        observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    },
                )
                .await
                .expect_err("a missing terminal must fail before creation");

                assert_eq!(
                    error.to_string(),
                    "`amux new` must run in an interactive terminal because it opens the new agent immediately"
                );
                assert_eq!(
                    create_calls.load(std::sync::atomic::Ordering::SeqCst),
                    0,
                    "creation action must not run for {open_mode:?}"
                );
            }
        }
    }

    #[test]
    fn parses_rm_with_one_positional_target_and_force_flag() {
        let cli = Cli::try_parse_from(["amux", "rm", "exact-name"]).unwrap();
        let Some(Commands::Rm { target, force }) = cli.command else {
            panic!("expected rm command");
        };
        assert_eq!(target, "exact-name");
        assert!(!force);

        let extra = Cli::try_parse_from(["amux", "rm", "exact-name", "extra"])
            .expect_err("rm accepts exactly one target");
        assert_eq!(extra.kind(), clap::error::ErrorKind::UnknownArgument);

        let cli = Cli::try_parse_from(["amux", "rm", "exact-name", "--force"]).unwrap();
        let Some(Commands::Rm { force, .. }) = cli.command else {
            panic!("expected rm command");
        };
        assert!(force);
    }

    #[test]
    fn list_all_is_an_explicit_opt_in() {
        let cli = Cli::try_parse_from(["amux", "list"]).unwrap();
        let Some(Commands::List { all }) = cli.command else {
            panic!("expected list command");
        };
        assert!(!all);

        let cli = Cli::try_parse_from(["amux", "list", "--all"]).unwrap();
        let Some(Commands::List { all }) = cli.command else {
            panic!("expected list command");
        };
        assert!(all);
    }

    #[test]
    fn keymap_subcommands_parse_their_targets() {
        assert!(matches!(
            Cli::try_parse_from(["amux", "keymap", "list"])
                .unwrap()
                .command,
            Some(Commands::Keymap {
                command: KeymapCommands::List
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["amux", "keymap", "show", "claude-2.1"])
                .unwrap()
                .command,
            Some(Commands::Keymap {
                command: KeymapCommands::Show { name }
            }) if name == "claude-2.1"
        ));
        assert!(matches!(
            Cli::try_parse_from(["amux", "keymap", "add", "custom.toml"])
                .unwrap()
                .command,
            Some(Commands::Keymap {
                command: KeymapCommands::Add { file }
            }) if file.as_os_str() == "custom.toml"
        ));
        assert!(matches!(
            Cli::try_parse_from(["amux", "keymap", "remove", "custom"])
                .unwrap()
                .command,
            Some(Commands::Keymap {
                command: KeymapCommands::Remove { name }
            }) if name == "custom"
        ));
        assert!(matches!(
            Cli::try_parse_from(["amux", "keymap", "dir"])
                .unwrap()
                .command,
            Some(Commands::Keymap {
                command: KeymapCommands::Dir
            })
        ));
    }

    #[test]
    fn codex_creation_options_reject_other_agents() {
        let error = configure_agent_type(
            AgentType::Claude {
                driver: amux::ClaudeDriver::Pty,
            },
            None,
            Some("gpt-5.4".to_string()),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("require agent type `codex`"));
    }

    #[test]
    fn claude_driver_rejects_other_agent_kinds() {
        let error = configure_agent_type(
            parse_agent_type("codex").unwrap(),
            Some(CliClaudeDriver::Sdk),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "--driver requires agent type `claude`");
    }

    #[test]
    fn pair_demo_requires_pin_and_duration() {
        assert!(Cli::try_parse_from(["amux", "pair", "--demo"]).is_err());
        assert!(Cli::try_parse_from(["amux", "pair", "--demo", "--pin", "123456"]).is_err());
        assert!(Cli::try_parse_from(["amux", "pair", "--pin", "123456", "--for", "1d"]).is_err());
        assert!(
            Cli::try_parse_from([
                "amux", "pair", "--demo", "--pin", "123456", "--for", "30d", "--qr"
            ])
            .is_err()
        );
        let cli =
            Cli::try_parse_from(["amux", "pair", "--demo", "--pin", "123456", "--for", "30d"])
                .unwrap();
        match cli.command {
            Some(Commands::Pair {
                demo, pin, r#for, ..
            }) => {
                assert!(demo);
                assert_eq!(pin.as_deref(), Some("123456"));
                assert_eq!(r#for, Some(Duration::from_secs(30 * 86_400)));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn pairing_duration_parses_units_and_rejects_garbage() {
        assert_eq!(
            parse_pairing_duration("45m"),
            Ok(Duration::from_secs(2_700))
        );
        assert_eq!(
            parse_pairing_duration("12h"),
            Ok(Duration::from_secs(43_200))
        );
        assert_eq!(parse_pairing_duration("90"), Ok(Duration::from_secs(90)));
        assert!(parse_pairing_duration("0d").is_err());
        assert!(parse_pairing_duration("3w").is_err());
        assert!(parse_pairing_duration("abc").is_err());
        assert_eq!(format_pairing_ttl(30 * 86_400), "30 days");
        assert_eq!(format_pairing_ttl(300), "5 minutes");
    }

    #[test]
    fn pair_connect_without_target_parses_as_interactive_request() {
        let cli = Cli::try_parse_from(["amux", "pair", "--connect"]).unwrap();
        let Some(Commands::Pair { connect, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert_eq!(connect, Some(None));
    }

    #[test]
    fn pair_connect_with_target_parses_value() {
        let cli = Cli::try_parse_from(["amux", "pair", "--connect", "127.0.0.1:4242"]).unwrap();
        let Some(Commands::Pair { connect, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert_eq!(connect, Some(Some("127.0.0.1:4242".to_string())));
    }

    #[test]
    fn pair_qr_without_payload_parses_as_responder() {
        let cli = Cli::try_parse_from(["amux", "pair", "--qr"]).unwrap();
        let Some(Commands::Pair { qr, link, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert!(qr);
        assert!(!link);
    }

    #[test]
    fn pair_qr_link_parses_as_debug_link_request() {
        let cli = Cli::try_parse_from(["amux", "pair", "--qr", "--link"]).unwrap();
        let Some(Commands::Pair { qr, link, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert!(qr);
        assert!(link);
    }

    #[test]
    fn pair_qr_rejects_payload_argument() {
        let error = Cli::try_parse_from(["amux", "pair", "--qr", "{\"host_id\":\"x\"}"])
            .expect_err("QR mode should not accept external payloads");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn pair_link_requires_qr() {
        let error = Cli::try_parse_from(["amux", "pair", "--link"])
            .expect_err("--link should require QR mode");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn pair_qr_link_runtime_guard_rejects_release_usage() {
        validate_pair_qr_link_usage(false, false).unwrap();
        validate_pair_qr_link_usage(true, true).unwrap();
        assert!(
            validate_pair_qr_link_usage(true, false)
                .unwrap_err()
                .to_string()
                .contains("debug builds")
        );
    }

    #[test]
    fn pair_connect_target_parser_splits_direct_cloud_and_picker() {
        assert_eq!(parse_pair_connect_target(None), PairConnectTarget::Picker);
        assert_eq!(
            parse_pair_connect_target(Some("127.0.0.1:4242".to_string())),
            PairConnectTarget::Direct("127.0.0.1:4242".parse().unwrap())
        );
        assert_eq!(
            parse_pair_connect_target(Some("phone".to_string())),
            PairConnectTarget::CloudName("phone".to_string())
        );
    }

    #[test]
    fn peer_subcommands_parse_targets() {
        let cli = Cli::try_parse_from(["amux", "peer", "list"]).unwrap();
        let Some(Commands::Peer {
            command: PeerCommands::List,
        }) = cli.command
        else {
            panic!("expected peer list command");
        };

        let cli = Cli::try_parse_from(["amux", "peer", "info", "phone"]).unwrap();
        let Some(Commands::Peer {
            command: PeerCommands::Info { peer },
        }) = cli.command
        else {
            panic!("expected peer info command");
        };
        assert_eq!(peer, "phone");
    }

    #[test]
    fn unpair_parses_force_flag() {
        let cli = Cli::try_parse_from(["amux", "unpair", "phone", "--force"]).unwrap();
        let Some(Commands::Unpair { peer, force }) = cli.command else {
            panic!("expected unpair command");
        };
        assert_eq!(peer, "phone");
        assert!(force);
    }

    #[test]
    fn peer_list_and_info_format_trusted_peers() {
        let peer = test_peer(1, "phone");
        let list = format_peer_list(std::slice::from_ref(&peer));
        assert!(list.contains("Trusted peers:"));
        assert!(list.contains("phone"));
        assert!(list.contains("cloud"));

        let info = format_peer_info(&peer);
        assert!(info.contains("Host ID: 00000000-0000-0000-0000-000000000001"));
        assert!(info.contains("Name: phone"));
        assert!(info.contains("Pubkey: 070707"));
    }

    #[test]
    fn cloud_pairing_host_resolution_matches_name_or_id() {
        let phone = test_host(1, "phone");
        let laptop = test_host(2, "laptop");
        let hosts = vec![phone.clone(), laptop.clone()];

        assert_eq!(
            resolve_pairing_host_by_name(&hosts, "phone").unwrap(),
            phone
        );
        assert_eq!(
            resolve_pairing_host_by_name(&hosts, &laptop.id.to_string()).unwrap(),
            laptop
        );
    }

    #[test]
    fn cloud_pairing_host_resolution_rejects_missing_or_ambiguous_names() {
        let hosts = vec![test_host(1, "phone"), test_host(2, "phone")];

        assert!(
            resolve_pairing_host_by_name(&hosts, "tablet")
                .unwrap_err()
                .to_string()
                .contains("no online cloud host")
        );
        assert!(
            resolve_pairing_host_by_name(&hosts, "phone")
                .unwrap_err()
                .to_string()
                .contains("multiple online cloud hosts")
        );
    }

    #[test]
    fn cloud_pairing_picker_selection_is_one_based() {
        assert_eq!(parse_pairing_host_selection("1\n", 3).unwrap(), 0);
        assert_eq!(parse_pairing_host_selection("3", 3).unwrap(), 2);
        assert!(parse_pairing_host_selection("0", 3).is_err());
        assert!(parse_pairing_host_selection("4", 3).is_err());
        assert!(parse_pairing_host_selection("x", 3).is_err());
        assert!(parse_pairing_host_selection("1", 0).is_err());
    }

    #[test]
    fn qr_pairing_output_renders_terminal_code_for_payload() {
        let pairing = PairingStart {
            identity: amux::SshPairingPeer {
                host_id: uuid::Uuid::from_u128(1),
                pubkey: vec![7; 32],
                name: "desktop".to_string(),
            },
            ttl_seconds: 300,
            tcp_port: None,
            cloud_url: "https://relay.example".to_string(),
            secret: PairingSecret::QrSecret(vec![9; 32]),
        };
        let PairingSecret::QrSecret(secret) = &pairing.secret else {
            panic!("expected QR secret");
        };

        let deep_link = qr_pairing_payload(&pairing, secret).unwrap();
        let encoded_payload = deep_link
            .strip_prefix(QR_PAIRING_DEEP_LINK_PREFIX)
            .expect("QR payload should be a pairing deep link");
        let decoded_payload = URL_SAFE_NO_PAD.decode(encoded_payload).unwrap();
        let decoded_json = std::str::from_utf8(&decoded_payload).unwrap();
        let value: serde_json::Value = serde_json::from_str(decoded_json).unwrap();
        let parsed = amux::parse_qr_pairing_payload(decoded_json).unwrap();
        let qr = terminal_qr_code(&deep_link).unwrap();

        assert!(!encoded_payload.contains('='));
        assert!(!encoded_payload.contains('+'));
        assert!(!encoded_payload.contains('/'));
        assert_eq!(value["host_id"], "00000000-0000-0000-0000-000000000001");
        assert!(value.get("pubkey").is_none());
        assert!(value.get("name").is_none());
        assert_eq!(parsed.host_id, uuid::Uuid::from_u128(1));
        assert_eq!(parsed.cloud_url, "https://relay.example");
        assert_eq!(parsed.secret, vec![9; 32]);
        assert!(qr.lines().count() > 4);
    }

    #[cfg(unix)]
    #[test]
    fn pair_via_ssh_parses_target() {
        let cli = Cli::try_parse_from(["amux", "pair", "--via-ssh", "workstation"]).unwrap();
        let Some(Commands::Pair { via_ssh, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert_eq!(via_ssh.as_deref(), Some("workstation"));
    }

    #[test]
    fn pair_rejects_conflicting_modes() {
        let error = Cli::try_parse_from(["amux", "pair", "--qr", "--connect", "host"])
            .expect_err("conflicting pair modes should fail");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    fn test_host(id: u128, name: &str) -> amux::HostEntry {
        let host = amux::Host {
            id: uuid::Uuid::from_u128(id),
            name: name.to_string(),
            version: "test".to_string(),
            capabilities: amux::Capabilities::default(),
        };
        amux::HostEntry {
            id: host.id,
            name: host.name.clone(),
            online: true,
            version: Some(host.version.clone()),
            capabilities: Some(host.capabilities.clone()),
            trust_status: amux::HostTrustStatus::UntrustedButOnline,
            last_dial_error: None,
        }
    }

    fn test_peer(id: u128, name: &str) -> amux::PeerEntry {
        amux::PeerEntry {
            host_id: uuid::Uuid::from_u128(id),
            name: name.to_string(),
            pubkey: vec![7; 32],
            paired_at: chrono::DateTime::from_timestamp(200, 0).unwrap(),
            reachabilities: vec![amux::PeerReachability::Cloud],
        }
    }
}
