mod auth;
mod client_common;
mod hooks;
mod init;
mod plugin;
mod server_client;
mod session_client;
mod update;

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use amux::{AgentType, Config, DebugFormat, PairingSecret, PairingStart, default_data_dir, setup};
use anyhow::{Context, Result, anyhow};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use qrcode::QrCode;
use qrcode::render::unicode;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::server_client::ServerMode;
use crate::update::MarkerFileReporter;

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

    /// Manage the amux server lifecycle
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

    /// Pair this device with another amux daemon
    Pair {
        /// Display a QR pairing payload, or consume a scanned QR JSON payload
        #[arg(long, value_name = "PAYLOAD", num_args = 0..=1, conflicts_with_all = ["listen", "connect", "via_ssh"])]
        qr: Option<Option<String>>,

        /// Require LAN-direct responder mode; errors when tcp_port is unset
        #[arg(long, conflicts_with_all = ["qr", "connect", "via_ssh"])]
        listen: bool,

        /// Initiate PIN pairing to a direct target or online cloud host
        #[arg(long, value_name = "TARGET", num_args = 0..=1, conflicts_with_all = ["qr", "listen", "via_ssh"])]
        connect: Option<Option<String>>,

        /// Pair through SSH and store the target for future SSH runtime links
        #[cfg(unix)]
        #[arg(long = "via-ssh", value_name = "TARGET", conflicts_with_all = ["qr", "listen", "connect"])]
        via_ssh: Option<String>,
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

/// CLI-side mirror of `DebugFormat` so we can derive `clap::ValueEnum`
/// without pulling clap into the `amux` library crate.
#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum CliDebugFormat {
    Yaml,
    Json,
}

impl From<CliDebugFormat> for DebugFormat {
    fn from(value: CliDebugFormat) -> Self {
        match value {
            CliDebugFormat::Yaml => DebugFormat::Yaml,
            CliDebugFormat::Json => DebugFormat::Json,
        }
    }
}

#[derive(Debug, Subcommand)]
enum HooksProvider {
    /// Claude Code hooks
    Claude,
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
    server_client::run_server_foreground(config, *cloud).await?;
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
            check_update_required(&config);
            match agent_type {
                AgentType::Claude => {
                    plugin::ensure_plugin_installed().await;
                }
                #[cfg(any(debug_assertions, test))]
                AgentType::TestAgent { .. } => {}
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
                // Cloud servers run non-interactively (e.g. under systemd) and
                // have no user-facing prompts to answer; the defaults via
                // `unwrap_or(...)` at consumption sites are sufficient.
                if !cloud {
                    ensure_initialized(&mut config).await?;
                }
                let options = server_client::StartOptions::from_flags(cloud, foreground);
                server_client::start_server(&config, options).await?
            }
            ServerCommands::Stop => server_client::stop_server(&config).await?,
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
        Commands::Pair {
            qr,
            listen,
            connect,
            #[cfg(unix)]
            via_ssh,
        } => {
            if let Some(payload) = qr.clone().flatten() {
                ensure_initialized(&mut config).await?;
                let payload =
                    amux::parse_qr_pairing_payload_for_cloud(&payload, &config.cloud_url)?;
                let client = client_common::require_running_client(
                    &config,
                    Some("amux pair --qr <payload>"),
                )
                .await?;
                let peer = client
                    .pair_qr_cloud_peer(payload.host_id, payload.pubkey, payload.one_shot_token)
                    .await?;
                println!("Paired with {} ({}) via QR.", peer.name, peer.host_id);
                return Ok(());
            }
            if let Some(connect_target) = connect {
                match parse_pair_connect_target(connect_target) {
                    PairConnectTarget::Picker => {
                        ensure_initialized(&mut config).await?;
                        let client = client_common::require_running_client(
                            &config,
                            Some("amux pair --connect"),
                        )
                        .await?;
                        let hosts = sorted_pairing_hosts(client.list_pairing_hosts().await?);
                        let host = prompt_pairing_host(&hosts)?;
                        let peer = pair_cloud_host(&client, &host).await?;
                        println!("Paired with {} ({}) via cloud.", peer.name, peer.host_id);
                        return Ok(());
                    }
                    PairConnectTarget::CloudName(target) => {
                        ensure_initialized(&mut config).await?;
                        let retry_command = format!("amux pair --connect {target}");
                        let client =
                            client_common::require_running_client(&config, Some(&retry_command))
                                .await?;
                        let hosts = sorted_pairing_hosts(client.list_pairing_hosts().await?);
                        let host = resolve_pairing_host_by_name(&hosts, &target)?;
                        let peer = pair_cloud_host(&client, &host).await?;
                        println!("Paired with {} ({}) via cloud.", peer.name, peer.host_id);
                        return Ok(());
                    }
                    PairConnectTarget::Direct(addr) => {
                        ensure_initialized(&mut config).await?;
                        let retry_command = format!("amux pair --connect {addr}");
                        let client =
                            client_common::require_running_client(&config, Some(&retry_command))
                                .await?;
                        let pin = prompt_pairing_pin()?;
                        let peer = amux::pair_via_pin_direct_tcp(
                            default_data_dir(),
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
                        return Ok(());
                    }
                }
            }

            ensure_initialized(&mut config).await?;
            #[cfg(unix)]
            if let Some(target) = via_ssh {
                let retry_command = format!("amux pair --via-ssh {target}");
                let client =
                    client_common::require_running_client(&config, Some(&retry_command)).await?;
                let peer = amux::pair_via_ssh_target(
                    default_data_dir(),
                    &config.host_name,
                    target,
                    &client,
                )
                .await?;
                println!("Paired with {} ({}) via SSH.", peer.name, peer.host_id);
                return Ok(());
            }

            let retry_command = pair_start_retry_command(qr.is_some(), listen);
            let client =
                client_common::require_running_client(&config, Some(retry_command)).await?;
            if listen && config.tcp_port.is_none() {
                return Err(anyhow!(
                    "set `tcp_port` in your config, or use cloud / SSH pairing"
                ));
            }
            let pairing = if qr.is_some() {
                client.start_qr_pairing().await?
            } else if listen {
                client.start_lan_pin_pairing().await?
            } else {
                client.start_pin_pairing().await?
            };
            if let Err(error) = print_pairing_start(&pairing) {
                client.cancel_pairing().await?;
                return Err(error);
            }
            wait_for_pairing_mode_to_end(&client, pairing.ttl_seconds).await?;
        }
        Commands::Peer { command } => {
            ensure_initialized(&mut config).await?;
            let client = client_common::require_running_client(&config, None).await?;
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
            let client = client_common::require_running_client(&config, None).await?;
            let entry = client.get_peer(peer.as_str()).await?;
            if !force && !confirm_unpair(&entry)? {
                println!("Unpair cancelled.");
                return Ok(());
            }
            let removed = client.unpair(peer.as_str(), "user").await?;
            println!("Unpaired {} ({}).", removed.name, removed.host_id);
        }
        #[cfg(unix)]
        Commands::PairRecv => {
            let client = client_common::require_running_client(&config, None).await?;
            amux::pair_via_ssh_responder_stdio(default_data_dir(), &config.host_name, &client)
                .await?;
        }
        #[cfg(unix)]
        Commands::Relay => {
            amux::relay_stdio_to_unix_socket(&config.socket_path).await?;
        }
        Commands::Update => update::run_update(&config).await?,
        Commands::Debug { verbose, format } => {
            let dump = server_client::debug(&config, verbose, format.into()).await?;
            print!("{dump}");
        }
        Commands::Hooks { provider } => match provider {
            HooksProvider::Claude => {
                hooks::handle_claude_hook(&config);
            }
        },
    }

    Ok(())
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

fn pair_start_retry_command(qr: bool, listen: bool) -> &'static str {
    if qr {
        "amux pair --qr"
    } else if listen {
        "amux pair --listen"
    } else {
        "amux pair"
    }
}

async fn pair_cloud_host(
    client: &amux::Client,
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

fn print_pairing_start(pairing: &PairingStart) -> Result<()> {
    match &pairing.secret {
        PairingSecret::Pin(pin) => {
            println!("Pairing PIN: {pin}");
            if let Some(port) = pairing.tcp_port {
                println!("LAN direct listener: tcp_port {port}");
            }
        }
        PairingSecret::OneShotToken(token) => {
            let payload = qr_pairing_payload(pairing, token)?;
            println!("{}", terminal_qr_code(&payload)?);
        }
    }
    println!("Pairing mode active for {} seconds.", pairing.ttl_seconds);
    Ok(())
}

fn qr_pairing_payload(pairing: &PairingStart, token: &[u8]) -> Result<String> {
    amux::encode_qr_pairing_payload(pairing, token).context("failed to encode QR pairing payload")
}

fn terminal_qr_code(payload: &str) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).context("failed to encode terminal QR")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

async fn wait_for_pairing_mode_to_end(client: &amux::Client, ttl_seconds: u64) -> Result<()> {
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
        "claude" => Ok(AgentType::Claude),
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
fn check_update_required(config: &Config) {
    // Only relevant if cloud mode is enabled
    if !setup::cloud_enabled(config) {
        return;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

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
        let Some(Commands::Pair { qr, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert_eq!(qr, Some(None));
    }

    #[test]
    fn pair_qr_with_payload_parses_as_initiator() {
        let cli = Cli::try_parse_from(["amux", "pair", "--qr", "{\"host_id\":\"x\"}"]).unwrap();
        let Some(Commands::Pair { qr, .. }) = cli.command else {
            panic!("expected pair command");
        };
        assert_eq!(qr, Some(Some("{\"host_id\":\"x\"}".to_string())));
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
            secret: PairingSecret::OneShotToken(vec![9; 32]),
        };
        let PairingSecret::OneShotToken(token) = &pairing.secret else {
            panic!("expected QR token");
        };

        let payload = qr_pairing_payload(&pairing, token).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let qr = terminal_qr_code(&payload).unwrap();

        assert_eq!(value["host_id"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(value["pubkey"].as_array().unwrap().len(), 32);
        assert!(value.get("name").is_none());
        assert_eq!(value["cloud_url"], "https://relay.example");
        assert_eq!(value["one_shot_token"].as_array().unwrap().len(), 32);
        assert!(qr.lines().count() > 4);
    }

    #[test]
    fn qr_pairing_payload_parser_validates_payload_shape() {
        let mut value = serde_json::json!({
            "host_id": "00000000-0000-0000-0000-000000000001",
            "pubkey": vec![7_u8; 32],
            "cloud_url": "https://relay.example",
            "one_shot_token": vec![9_u8; 32],
        });
        let payload = value.to_string();
        let parsed = amux::parse_qr_pairing_payload(&payload).unwrap();
        assert_eq!(parsed.host_id, uuid::Uuid::from_u128(1));
        assert_eq!(parsed.pubkey, vec![7; 32]);
        assert_eq!(parsed.cloud_url, "https://relay.example");
        assert_eq!(parsed.one_shot_token, vec![9; 32]);

        value["pubkey"] = serde_json::json!([7_u8]);
        let bad = value.to_string();
        assert!(
            amux::parse_qr_pairing_payload(&bad)
                .unwrap_err()
                .to_string()
                .contains("pubkey must be 32 bytes")
        );
        assert!(amux::validate_qr_payload_cloud_url("https://a", "https://b").is_err());
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
            reachability_status: None,
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
