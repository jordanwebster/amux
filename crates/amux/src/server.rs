//! The daemon entry and the server verbs.
//!
//! `amux daemon` is the daemon itself, in the foreground: what a supervisor
//! or a service manager runs. `amux server start` starts one by hand,
//! detached from the terminal, and returns once its front door answers.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::{Launch, NoopSender, StartOptions};
use settings::InstallationConfig;
use tracing_subscriber::EnvFilter;
use wire::{GetInfoRequest, InstallationShutdownRequest};

use crate::connect;

/// The daemon log's path when the environment names none.
const DAEMON_LOG: &str = "daemon.log";
/// Names the daemon's log file, overriding the one under the data dir.
const LOG_ENV: &str = "AMUX_LOG";
/// How long `amux server start` and `stop` wait on the daemon.
const PATIENCE: Duration = Duration::from_secs(30);

fn daemon_log(config: &InstallationConfig) -> PathBuf {
    std::env::var_os(LOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.root.join(DAEMON_LOG))
}

fn log_to(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening the daemon log {}", path.display()))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    Ok(())
}

/// What agents start with: this binary as the canonical install path and
/// the installation's agent and retention settings.
fn launch(config: &InstallationConfig) -> Result<Launch> {
    let install_path = std::env::current_exe().context("finding the amux binary")?;
    Ok(Launch {
        install_path,
        agent: config.agent,
        own_budget_bytes: config.retention.own_budget_mib << 20,
        replica_budget_bytes: config.retention.replica_rows_mib << 20,
        replica_blob_budget_bytes: config.retention.replica_blobs_mib << 20,
        ..Launch::default()
    })
}

/// Runs the daemon in the foreground until it is asked to stop: by a
/// client's shutdown call, SIGTERM or SIGINT.
pub fn run_daemon(config: &InstallationConfig) -> Result<()> {
    let log = daemon_log(config);
    log_to(&log)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let options = StartOptions {
            data_dir: config.root.clone(),
            boot_id: None,
            launch: launch(config)?,
            clock: Arc::new(agent_dir::SystemClock),
            // Needs-you pushes go through the relay, which this build does
            // not connect to yet.
            push: Arc::new(NoopSender),
            daemon_log: Some(log),
            front_door: Some(config.front_door_socket.clone()),
            edge: edge_options(config),
        };
        let daemon = node::start(options, None)
            .await
            .context("starting the daemon")?;
        tracing::info!(
            version = node::VERSION,
            root = %config.root.display(),
            front_door = %config.front_door_socket.display(),
            generation = daemon.generation().counter,
            "serving"
        );
        tokio::select! {
            () = daemon.shutdown_requested() => tracing::info!("a client asked the daemon to stop"),
            () = terminated() => tracing::info!("signalled to stop"),
        }
        daemon.shutdown().await.context("shutting down")?;
        tracing::info!("stopped cleanly");
        Ok(())
    })
}

/// What each profile serves on the network: a LAN listener, discovery in
/// the installation's scope, dialling paired hosts, and the link socket SSH
/// relays use.
fn edge_options(config: &InstallationConfig) -> node::EdgeOptions {
    node::EdgeOptions {
        host_name: config.host_name.clone(),
        kinds: vec![
            wire::Kind::ClaudePty,
            wire::Kind::ClaudeSdk,
            wire::Kind::Codex,
        ],
        lan: Some(node::LanOptions::default()),
        discovery: Some(node::mdns_discovery()),
        discovery_scope: config.discovery.scope.clone(),
        dial: true,
        link_socket: true,
        cloud: node::CloudOptions::default(),
    }
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[cfg(not(unix))]
async fn terminated() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Starts a daemon detached from this terminal, unless one answers
/// already, and returns once its front door answers.
pub async fn start(config: &InstallationConfig, config_path: Option<&Path>) -> Result<()> {
    if connect::front_door_now(config).await.is_some() {
        println!("The amux daemon is already running.");
        return Ok(());
    }
    std::fs::create_dir_all(&config.root)
        .with_context(|| format!("creating {}", config.root.display()))?;
    let startup = config.root.join("daemon-startup.log");
    let stderr =
        File::create(&startup).with_context(|| format!("creating {}", startup.display()))?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    if let Some(path) = config_path {
        command.arg("--config").arg(path);
    }
    detach(&mut command);
    let mut child = command.spawn().context("starting amux daemon")?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(door) = connect::front_door_now(config).await
            && connect::installation(door)
                .get_info(GetInfoRequest {})
                .await
                .is_ok()
        {
            println!("Started the amux daemon.");
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let said = std::fs::read_to_string(&startup).unwrap_or_default();
            bail!(
                "amux daemon exited ({status}) before it answered:\n{}",
                said.trim_end()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "amux daemon did not answer within {}s; its log is {}",
                PATIENCE.as_secs(),
                daemon_log(config).display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Its own session, so the daemon outlives the terminal that started it.
#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Asks the daemon to shut down cleanly and waits until it has. Its
/// agents keep running and wait out their grace for the next daemon.
pub async fn stop(config: &InstallationConfig) -> Result<()> {
    let Some(door) = connect::front_door_now(config).await else {
        println!("The amux daemon is not running.");
        return Ok(());
    };
    connect::installation(door)
        .shutdown(InstallationShutdownRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .map_err(crate::plain)?;
    // The front door closes first; the daemon has stopped once it has
    // flushed its stores and released the installation lock, and a daemon
    // started before then would find the lock still held.
    let deadline = Instant::now() + PATIENCE;
    while installation_locked(&config.root)? {
        if Instant::now() >= deadline {
            bail!("the daemon did not stop within {}s", PATIENCE.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("Stopped the amux daemon.");
    Ok(())
}

/// Whether a daemon holds the installation lock under `root`.
fn installation_locked(root: &Path) -> Result<bool> {
    let path = root.join(node::INSTALLATION_LOCK);
    let file = match OpenOptions::new().write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("probing {}", path.display()))
        }
    }
}
