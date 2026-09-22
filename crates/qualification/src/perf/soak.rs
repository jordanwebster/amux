use std::ffi::OsStr;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use testnet::script::{Provider, Script, ScriptAsk, Step};
use testnet::{TestNet, connect_user};
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::store::ChatState;
use uuid::Uuid;

use super::{Baselines, Machine, Metric, MetricRun, Report, Sample, Statistic, Unit, Workload};

const QUALIFICATION_DURATION: Duration = Duration::from_secs(10 * 60);
const MIN_DIAGNOSTIC_DURATION: Duration = Duration::from_secs(4 * 60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const PULSE_INTERVAL: Duration = Duration::from_millis(50);
const WARM_UP: Duration = Duration::from_secs(2 * 60);
const RESET_AFTER: Duration = Duration::from_secs(4 * 60);
const RESET_REOPEN_MARGIN: Duration = Duration::from_secs(60);
const MIB: f64 = 1024.0 * 1024.0;
const SEED: u64 = 0xA6_2026_0917;

#[derive(Clone, Copy)]
struct ClientSoakTiming {
    stall_after: Duration,
    stall_for: Duration,
    reset_after: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SoakRunConfig {
    duration: Duration,
    diagnostic: bool,
}

impl SoakRunConfig {
    fn from_override(value: Option<&OsStr>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self {
                duration: QUALIFICATION_DURATION,
                diagnostic: false,
            });
        };
        let value = value
            .to_str()
            .context("AMUX_PERF_SOAK_SECONDS must be Unicode")?;
        let seconds = value
            .parse::<u64>()
            .context("AMUX_PERF_SOAK_SECONDS must be a whole number of seconds")?;
        let duration = Duration::from_secs(seconds);
        ensure!(
            duration >= MIN_DIAGNOSTIC_DURATION && duration < QUALIFICATION_DURATION,
            "AMUX_PERF_SOAK_SECONDS is for shortened repair runs from {} through {} seconds",
            MIN_DIAGNOSTIC_DURATION.as_secs(),
            QUALIFICATION_DURATION.as_secs() - 1
        );
        Ok(Self {
            duration,
            diagnostic: true,
        })
    }

    fn from_env() -> Result<Self> {
        Self::from_override(std::env::var_os("AMUX_PERF_SOAK_SECONDS").as_deref())
    }

    fn client_timing(self) -> ClientSoakTiming {
        ClientSoakTiming {
            stall_after: Duration::from_secs(10),
            stall_for: Duration::from_secs(5),
            reset_after: RESET_AFTER.min(self.duration - RESET_REOPEN_MARGIN),
        }
    }
}

const CLIENT_WORKLOAD: Workload = Workload {
    description: "10 open chats replaying fresh-identity corpora for 10 minutes",
    seed: SEED,
    identity_growth: "fresh ids; oversized row, 100 asks, 5 s persistence stall, reset",
    warm_up: "exclude the first two minutes from slope",
};

const CLIENT_DIAGNOSTIC_WORKLOAD: Workload = Workload {
    description: "10 open chats replaying fresh-identity corpora for the diagnostic duration named in the report header",
    seed: SEED,
    identity_growth: "fresh ids; oversized row, 100 asks, 5 s persistence stall, reset",
    warm_up: "exclude the first two minutes from slope",
};

const DAEMON_WORKLOAD: Workload = Workload {
    description: "200 idle and 20 active daemon rings with live summarizers for 10 minutes",
    seed: SEED,
    identity_growth: "fresh ids; oversized row, 100 asks and semantic reset",
    warm_up: "exclude the first two minutes from slope",
};

const DAEMON_DIAGNOSTIC_WORKLOAD: Workload = Workload {
    description: "200 idle and 20 active daemon rings for the diagnostic duration named in the report header",
    seed: SEED,
    identity_growth: "fresh ids; oversized row, 100 asks and semantic reset",
    warm_up: "exclude the first two minutes from slope",
};

pub fn run_soak(machine: Machine, recording: bool) -> Result<()> {
    let config = SoakRunConfig::from_env()?;
    Report::validate_recording(recording, config.diagnostic)?;
    if config.diagnostic {
        println!(
            "run: diagnostic memory soak shortened to {} s (qualification remains {} s; warm-up remains {} s)",
            config.duration.as_secs(),
            QUALIFICATION_DURATION.as_secs(),
            WARM_UP.as_secs()
        );
    } else {
        println!(
            "run: qualification memory soak {} s (warm-up {} s)",
            config.duration.as_secs(),
            WARM_UP.as_secs()
        );
    }
    let client_workload = if config.diagnostic {
        CLIENT_DIAGNOSTIC_WORKLOAD
    } else {
        CLIENT_WORKLOAD
    };
    let daemon_workload = if config.diagnostic {
        DAEMON_DIAGNOSTIC_WORKLOAD
    } else {
        DAEMON_WORKLOAD
    };
    let root = tempfile::tempdir().context("memory soak control directory")?;
    let executable = std::env::current_exe().context("resolve performance executable")?;

    let daemon_dir = root.path().join("daemon");
    let client_dir = root.path().join("client");
    std::fs::create_dir_all(&daemon_dir)?;
    std::fs::create_dir_all(&client_dir)?;

    let mut daemon = ChildGuard::spawn(&executable, "daemon", &daemon_dir)?;
    wait_marker(&mut daemon, &daemon_dir.join("baseline"))?;
    let daemon_baseline = super::sample_memory(daemon.id())?;
    std::fs::write(daemon_dir.join("baseline.ack"), b"ok\n")?;

    wait_marker(&mut daemon, &daemon_dir.join("idle"))?;
    let daemon_idle = super::sample_memory(daemon.id())?;
    std::fs::write(daemon_dir.join("idle.ack"), b"ok\n")?;

    wait_marker(&mut daemon, &daemon_dir.join("ready"))?;
    let daemon_active = super::sample_memory(daemon.id())?;
    std::fs::write(daemon_dir.join("ready.ack"), b"ok\n")?;

    let client_daemon_dir = root.path().join("client-daemon");
    std::fs::create_dir_all(&client_daemon_dir)?;
    let mut client_daemon = ChildGuard::spawn(&executable, "client-daemon", &client_daemon_dir)?;
    wait_marker(&mut client_daemon, &client_daemon_dir.join("ready"))?;
    std::fs::copy(
        client_daemon_dir.join("connection.json"),
        client_dir.join("connection.json"),
    )?;

    let mut client = ChildGuard::spawn(&executable, "client", &client_dir)?;
    wait_marker(&mut client, &client_dir.join("ready"))?;
    std::fs::write(client_dir.join("start"), b"start\n")?;
    std::fs::write(client_daemon_dir.join("start"), b"start\n")?;
    wait_marker(
        &mut client_daemon,
        &client_daemon_dir.join("workload-ready"),
    )?;

    let started_at = Utc::now();
    let started = Instant::now();
    let sample_count = config.duration.as_secs() / SAMPLE_INTERVAL.as_secs() + 1;
    let mut client_samples = Vec::with_capacity(sample_count as usize);
    let mut client_samples_before_reset = Vec::with_capacity(sample_count as usize);
    let mut client_samples_after_reset = Vec::with_capacity(sample_count as usize);
    let mut daemon_samples = Vec::with_capacity(sample_count as usize);
    for index in 0..sample_count {
        ensure_running(&mut client, "client")?;
        ensure_running(&mut client_daemon, "client daemon")?;
        ensure_running(&mut daemon, "daemon")?;
        let at = started.elapsed().as_secs_f64();
        let client_sample = TimedSample {
            seconds: at,
            bytes: super::sample_memory(client.id())?.bytes,
        };
        if !client_daemon_dir.join("reset-started").is_file() {
            client_samples_before_reset.push(client_sample);
        } else if client_dir.join("reset-reopened").is_file() {
            client_samples_after_reset.push(client_sample);
        }
        client_samples.push(client_sample);
        daemon_samples.push(TimedSample {
            seconds: at,
            bytes: super::sample_memory(daemon.id())?.bytes,
        });
        if index * SAMPLE_INTERVAL.as_secs() < config.duration.as_secs() {
            let deadline = started + SAMPLE_INTERVAL * (index as u32 + 1);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
    std::fs::write(client_dir.join("stop"), b"stop\n")?;
    std::fs::write(client_daemon_dir.join("stop"), b"stop\n")?;
    std::fs::write(daemon_dir.join("stop"), b"stop\n")?;
    client.wait_success()?;
    client_daemon.wait_success()?;
    daemon.wait_success()?;
    let ended_at = Utc::now();
    let observation_count = client_samples.len();
    let client_slope = if config.diagnostic {
        let before = slope_mib_per_minute(&client_samples_before_reset);
        let after = slope_mib_per_minute(&client_samples_after_reset);
        println!(
            "diagnostic client slope: max steady-state trend around the planned reset/reopen discontinuity (before {before:.3} MiB/min; after {after:.3} MiB/min); the one-time allocator plateau change remains included in peak"
        );
        before.max(after)
    } else {
        slope_mib_per_minute(&client_samples)
    };

    let memory_name = daemon_active.name();
    println!("memory measure: {memory_name} (sampled every 5 s)");
    let runs = vec![
        memory_run(
            "bounded client memory slope",
            client_slope,
            1.0,
            Unit::MegabytesPerMinute,
            client_workload,
            Statistic::Worst,
            observation_count,
            started_at,
            ended_at,
        ),
        memory_run(
            "bounded client memory peak",
            peak_mib(&client_samples),
            300.0,
            Unit::Megabytes,
            client_workload,
            Statistic::Peak,
            observation_count,
            started_at,
            ended_at,
        ),
        memory_run(
            "bounded daemon memory slope",
            slope_mib_per_minute(&daemon_samples),
            1.0,
            Unit::MegabytesPerMinute,
            daemon_workload,
            Statistic::Worst,
            observation_count,
            started_at,
            ended_at,
        ),
        memory_run(
            "daemon memory per idle agent",
            daemon_idle.bytes.saturating_sub(daemon_baseline.bytes) as f64 / MIB / 200.0,
            2.0,
            Unit::Megabytes,
            daemon_workload,
            Statistic::Worst,
            observation_count,
            started_at,
            ended_at,
        ),
        memory_run(
            "daemon memory per active agent",
            daemon_samples
                .iter()
                .map(|sample| sample.bytes)
                .max()
                .unwrap_or(daemon_active.bytes)
                .saturating_sub(daemon_idle.bytes) as f64
                / MIB
                / 20.0,
            40.0,
            Unit::Megabytes,
            daemon_workload,
            Statistic::Peak,
            observation_count,
            started_at,
            ended_at,
        ),
    ];
    let baseline_path = machine.soak_baseline_path();
    let recorded = if config.diagnostic {
        None
    } else {
        Baselines::read(&baseline_path, &machine, None)?
    };
    let report = if config.diagnostic {
        Report::evaluate_diagnostic(machine, runs)?
    } else {
        Report::evaluate_soak(machine, runs, recorded.as_ref(), recording)?
    };
    report.print();
    if recording {
        report.write_baseline(&baseline_path)?;
        println!("baseline: wrote {}", baseline_path.display());
    }
    if !report.passed() {
        bail!("one or more memory soak metrics missed their budget");
    }
    Ok(())
}

pub fn soak_child(kind: &str, directory: &Path) -> Result<()> {
    let config = SoakRunConfig::from_env()?;
    match kind {
        "client" => client_child(directory, config),
        "client-daemon" => client_daemon_child(directory, config),
        "daemon" => daemon_child(directory, config),
        _ => bail!("unknown memory soak child {kind:?}"),
    }
}

fn client_child(directory: &Path, config: SoakRunConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("build client soak runtime")?;
    runtime.block_on(client_runtime(
        directory,
        config.client_timing(),
        config.duration,
    ))
}

#[derive(Serialize, Deserialize)]
struct ClientDaemonConnection {
    cloud_url: String,
    relay: SocketAddr,
    token: String,
    qr: String,
    agents: Vec<Uuid>,
}

async fn client_runtime(
    directory: &Path,
    timing: ClientSoakTiming,
    soak_duration: Duration,
) -> Result<()> {
    let connection: ClientDaemonConnection = serde_json::from_slice(
        &std::fs::read(directory.join("connection.json")).context("read client soak connection")?,
    )
    .context("parse client soak connection")?;
    let client = connect_user(&connection.cloud_url, connection.relay, connection.token)
        .await
        .context("connect client soak runtime")?;
    let qr = node::parse_qr_pairing_payload(&connection.qr).context("parse soak pairing QR")?;
    let admin = client.admin();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let hosts = admin
                .list_pairing_hosts()
                .await
                .context("list soak pairing candidates")?;
            if hosts
                .iter()
                .any(|host| host.host.id == qr.host_id && host.host.online)
            {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("timed out waiting for soak daemon cloud presence")??;
    let pending = admin
        .begin_pair_qr(&qr)
        .await
        .context("authenticate soak daemon")?;
    admin
        .confirm_pair(pending)
        .await
        .context("pair soak client to daemon")?;

    let store_path = directory.join("store.sqlite");
    let resets = Arc::new(AtomicUsize::new(0));
    let tapped_resets = Arc::clone(&resets);
    let mut runtime = Runtime::start_with_client(
        (*client).clone(),
        RuntimeOptions {
            store_path: Some(store_path.clone()),
            msg_tap: Some(Box::new(move |msg| {
                if matches!(
                    msg,
                    ui_state::Msg::ChatStream {
                        event: ui_state::ChatStreamMsg::Closed {
                            reason: ui_state::StreamCloseReason::Reset,
                            ..
                        },
                        ..
                    }
                ) {
                    tapped_resets.fetch_add(1, Ordering::Relaxed);
                }
            })),
            ..RuntimeOptions::default()
        },
    );
    wait_for_client(&mut runtime, "ten scripted agents", |runtime| {
        runtime.model().is_synchronized()
            && connection
                .agents
                .iter()
                .all(|agent| runtime.model().agent(*agent).is_some())
    })
    .await?;
    for agent in &connection.agents {
        runtime.open_chat(*agent);
    }
    wait_for_client(&mut runtime, "ten live stored chats", |runtime| {
        connection.agents.iter().all(|agent| {
            runtime
                .model()
                .chat(*agent)
                .is_some_and(|chat| chat.state == ChatState::Live)
        })
    })
    .await?;
    ensure!(
        store_path.is_file(),
        "client runtime did not create its on-disk store"
    );
    std::fs::write(directory.join("ready"), b"ready\n")?;
    wait_path(directory, "start", Duration::from_secs(30)).await?;

    let stalled_store = store_path.clone();
    let stall = tokio::task::spawn_blocking(move || {
        stall_store_worker(&stalled_store, timing.stall_after, timing.stall_for)
    });
    let started = Instant::now();
    let mut reset_observed = false;
    let mut reopening = false;
    let mut reopened = false;
    while !directory.join("stop").is_file() {
        let _ = tokio::time::timeout(PULSE_INTERVAL, runtime.next()).await;
        if !reset_observed && resets.load(Ordering::Relaxed) >= connection.agents.len() {
            std::fs::write(directory.join("reset-observed"), b"reset\n")?;
            reset_observed = true;
            reopening = true;
            for agent in &connection.agents {
                runtime.close_chat(*agent);
            }
        }
        if reopening
            && connection.agents.iter().all(|agent| {
                runtime
                    .model()
                    .chat(*agent)
                    .is_some_and(|chat| chat.state == ChatState::Absent)
            })
        {
            for agent in &connection.agents {
                runtime.open_chat(*agent);
            }
            reopening = false;
        }
        if reset_observed
            && !reopening
            && !reopened
            && connection.agents.iter().all(|agent| {
                runtime.model().chat(*agent).is_some_and(|chat| {
                    chat.state == ChatState::Live && !chat.boundaries.is_empty()
                })
            })
        {
            std::fs::write(directory.join("reset-reopened"), b"live\n")?;
            reopened = true;
        }
        if started.elapsed() > soak_duration + Duration::from_secs(120) {
            bail!("client soak parent did not stop the child");
        }
    }
    stall.await.context("join store stall")??;
    ensure!(
        runtime.model().chats().count() == connection.agents.len(),
        "client runtime lost an open soak chat"
    );
    ensure!(
        resets.load(Ordering::Relaxed) >= connection.agents.len(),
        "client runtime did not observe every daemon-side reset"
    );
    ensure!(reopened, "client runtime did not reopen every reset chat");
    drop(runtime);
    drop(client);
    Ok(())
}

async fn wait_for_client(
    runtime: &mut Runtime,
    what: &str,
    ready: impl Fn(&Runtime) -> bool,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready(runtime) {
            ensure!(
                runtime.next().await,
                "client runtime closed waiting for {what}"
            );
        }
        Ok(())
    })
    .await
    .with_context(|| format!("timed out waiting for {what}"))?
}

async fn wait_path(directory: &Path, name: &str, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, async {
        while !directory.join(name).is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for client soak marker {name}"))?;
    Ok(())
}

fn stall_store_worker(path: &Path, after: Duration, for_duration: Duration) -> Result<()> {
    std::thread::sleep(after);
    let connection = rusqlite::Connection::open(path).context("open client store stall handle")?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .context("set client store stall timeout")?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .context("begin client store stall")?;
    std::thread::sleep(for_duration);
    connection
        .execute_batch("COMMIT")
        .context("finish client store stall")?;
    Ok(())
}

fn client_daemon_child(directory: &Path, config: SoakRunConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("build client-daemon soak runtime")?;
    runtime.block_on(client_daemon_runtime(
        directory,
        config.client_timing(),
        config.duration,
    ))
}

async fn client_daemon_runtime(
    directory: &Path,
    timing: ClientSoakTiming,
    soak_duration: Duration,
) -> Result<()> {
    const USER: &str = "soak-user";
    const DAEMON: &str = "soak-daemon";
    let net = TestNet::builder()
        .cloud_url("https://soak.testnet.example")
        .daemon(DAEMON)
        .cloud_user(USER)
        .start()
        .await;
    let daemon = net.daemon(DAEMON);
    let mut agents = Vec::with_capacity(10);
    let mut providers = Vec::with_capacity(10);
    for index in 0..10 {
        let (agent, provider) = daemon
            .spawn_scripted_agent(
                &format!("soak-{index:02}"),
                std::env::temp_dir(),
                Script::default(),
                None,
            )
            .await
            .with_context(|| format!("spawn soak agent {index}"))?;
        agents.push(agent.id);
        providers.push(provider);
    }
    let (_, token) = net.user_credentials(USER);
    let qr = daemon.try_start_qr_pairing().await?;
    let connection = ClientDaemonConnection {
        cloud_url: net.cloud_url().to_owned(),
        relay: net.relay_addr(),
        token,
        qr: qr.encoded(),
        agents,
    };
    std::fs::write(
        directory.join("connection.json"),
        serde_json::to_vec(&connection)?,
    )?;
    std::fs::write(directory.join("ready"), b"ready\n")?;
    wait_path(directory, "start", Duration::from_secs(30)).await?;

    seed_client_workload(&providers).await?;
    std::fs::write(directory.join("workload-ready"), b"ready\n")?;
    let started = Instant::now();
    let mut iteration = 0;
    let mut reset = false;
    while !directory.join("stop").is_file() {
        let pulse = Instant::now();
        let chat = iteration as usize % providers.len();
        providers[chat]
            .emit(vec![corpus_row(chat, iteration / providers.len() as u64)])
            .await
            .with_context(|| format!("replay client corpus for chat {chat}"))?;
        if !reset && started.elapsed() >= timing.reset_after {
            std::fs::write(directory.join("reset-started"), b"reset\n")?;
            for provider in &providers {
                provider
                    .play(vec![Step::Compaction])
                    .await
                    .context("reset client stream through provider relink")?;
            }
            std::fs::write(directory.join("reset"), b"reset\n")?;
            reset = true;
        }
        iteration += 1;
        tokio::time::sleep(PULSE_INTERVAL.saturating_sub(pulse.elapsed())).await;
        if started.elapsed() > soak_duration + Duration::from_secs(30) {
            bail!("client-daemon soak parent did not stop the child");
        }
    }
    net.shutdown().await;
    Ok(())
}

async fn seed_client_workload(providers: &[Provider]) -> Result<()> {
    providers[0]
        .play(vec![Step::Markdown {
            text: "x".repeat(5 * 1024 * 1024),
        }])
        .await
        .context("publish oversized client message")?;
    for (chat, provider) in providers.iter().enumerate() {
        for ask in 0..10 {
            provider
                .raise_ask(ScriptAsk::Permission {
                    tool: "Write".to_owned(),
                    invocation: serde_json::json!({
                        "file_path": format!("/tmp/soak-{chat}-{ask}")
                    }),
                    scoped_directories: Vec::new(),
                })
                .await
                .with_context(|| format!("publish unresolved ask {ask} for chat {chat}"))?;
        }
    }
    Ok(())
}

fn daemon_child(directory: &Path, config: SoakRunConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("build daemon soak runtime")?;
    runtime.block_on(async {
        let mut harness = agent_runtime::test_support::DaemonMemoryHarness::new();
        std::fs::write(directory.join("baseline"), b"baseline\n")?;
        wait_ack(directory, "baseline")?;
        harness.add_idle(200).await;
        std::fs::write(directory.join("idle"), b"idle\n")?;
        wait_ack(directory, "idle")?;
        harness.add_active(20).await;
        std::fs::write(directory.join("ready"), b"ready\n")?;
        wait_ack(directory, "ready")?;
        let started = Instant::now();
        let mut iteration = 0;
        while !directory.join("stop").is_file() {
            let pulse = Instant::now();
            harness.pulse(iteration).await;
            iteration += 1;
            tokio::time::sleep(PULSE_INTERVAL.saturating_sub(pulse.elapsed())).await;
            if started.elapsed() > config.duration + Duration::from_secs(30) {
                bail!("daemon soak parent did not stop the child");
            }
        }
        Ok(())
    })
}

fn wait_ack(directory: &Path, stage: &str) -> Result<()> {
    let path = directory.join(format!("{stage}.ack"));
    let started = Instant::now();
    while !path.is_file() {
        if started.elapsed() > Duration::from_secs(10) {
            bail!("timed out waiting for parent acknowledgement of {stage}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn corpus_row(chat: usize, iteration: u64) -> serde_json::Value {
    let id = format!("{chat:02}-{iteration:010}");
    match iteration % 3 {
        0 => serde_json::json!({
            "type": "user",
            "uuid": format!("user-{id}"),
            "message": {"content": format!("Investigate memory case {id}")},
        }),
        1 => serde_json::json!({
            "type": "assistant",
            "uuid": format!("assistant-{id}"),
            "message": {"id": format!("message-{id}"), "content": [{"type":"text", "text": format!("Memory case {id} is bounded.")}]},
        }),
        _ => serde_json::json!({
            "type": "assistant",
            "uuid": format!("tool-{id}"),
            "message": {"id": format!("message-tool-{id}"), "content": [{"type":"tool_use", "id": format!("toolu-{id}"), "name":"Read", "input":{"file_path":format!("/tmp/{id}")}}]},
        }),
    }
}

#[derive(Clone, Copy)]
struct TimedSample {
    seconds: f64,
    bytes: u64,
}

fn slope_mib_per_minute(samples: &[TimedSample]) -> f64 {
    let samples = samples
        .iter()
        .filter(|sample| sample.seconds >= WARM_UP.as_secs_f64())
        .collect::<Vec<_>>();
    if samples.len() < 2 {
        return f64::INFINITY;
    }
    let mean_x = samples.iter().map(|sample| sample.seconds).sum::<f64>() / samples.len() as f64;
    let mean_y = samples
        .iter()
        .map(|sample| sample.bytes as f64)
        .sum::<f64>()
        / samples.len() as f64;
    let numerator = samples
        .iter()
        .map(|sample| (sample.seconds - mean_x) * (sample.bytes as f64 - mean_y))
        .sum::<f64>();
    let denominator = samples
        .iter()
        .map(|sample| (sample.seconds - mean_x).powi(2))
        .sum::<f64>();
    (numerator / denominator * 60.0 / MIB).max(0.0)
}

fn peak_mib(samples: &[TimedSample]) -> f64 {
    samples.iter().map(|sample| sample.bytes).max().unwrap_or(0) as f64 / MIB
}

#[allow(
    clippy::too_many_arguments,
    reason = "a derived memory row carries the complete measurement contract"
)]
fn memory_run(
    name: &'static str,
    value: f64,
    budget: f64,
    unit: Unit,
    workload: Workload,
    statistic: Statistic,
    observation_count: usize,
    started_at: chrono::DateTime<Utc>,
    ended_at: chrono::DateTime<Utc>,
) -> MetricRun {
    MetricRun {
        metric: Metric {
            name,
            statistic,
            budget,
            unit,
            ceiling_only: false,
            workload,
        },
        samples: vec![Sample {
            metric: name,
            value,
            unit,
        }],
        observation_count,
        started_at,
        ended_at,
    }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(executable: &Path, kind: &str, directory: &Path) -> Result<Self> {
        let child = Command::new(executable)
            .arg("--soak-child")
            .arg(kind)
            .arg(directory)
            .spawn()
            .with_context(|| format!("spawn {kind} memory child"))?;
        Ok(Self { child: Some(child) })
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("live child").id()
    }

    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.as_mut().expect("live child").try_wait()?)
    }

    fn wait_success(&mut self) -> Result<()> {
        let status = self.child.as_mut().expect("live child").wait()?;
        if !status.success() {
            bail!("memory child exited with {status}");
        }
        self.child = None;
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_marker(child: &mut ChildGuard, path: &Path) -> Result<()> {
    let started = Instant::now();
    while !path.is_file() {
        if let Some(status) = child.try_wait()? {
            bail!("memory child exited before {}: {status}", path.display());
        }
        if started.elapsed() > Duration::from_secs(60) {
            bail!(
                "timed out waiting for memory child marker {}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn ensure_running(child: &mut ChildGuard, name: &str) -> Result<()> {
    if let Some(status) = child.try_wait()? {
        bail!("{name} memory child exited early with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RETENTION_FIRST_ROWS_PER_CHAT: u64 = 400;
    const RETENTION_SECOND_ROWS_PER_CHAT: u64 = 900;

    async fn emit_retention_rows(providers: &[Provider], from: u64, through: u64) -> Result<()> {
        for (chat, provider) in providers.iter().enumerate() {
            provider
                .emit(
                    (from..through)
                        .map(|iteration| corpus_row(chat, iteration))
                        .collect(),
                )
                .await
                .with_context(|| format!("emit retention rows for chat {chat}"))?;
        }
        Ok(())
    }

    async fn wait_for_retention_cut(
        runtime: &mut Runtime,
        agents: &[Uuid],
        base: &[u64],
        rows_per_chat: u64,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(120), async {
            while !agents.iter().zip(base).all(|(agent, base)| {
                runtime
                    .model()
                    .chat(*agent)
                    .and_then(|chat| chat.head_through())
                    .is_some_and(|through| through >= base + rows_per_chat)
            }) {
                ensure!(
                    runtime.next().await,
                    "client runtime closed waiting for retention measurement cut"
                );
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("timed out waiting for retention measurement cut")??;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let report = runtime.retention_report();
                if report.store_queued_ops == 0
                    && report.chats.iter().all(|chat| chat.pending_commits == 0)
                {
                    return Ok::<(), anyhow::Error>(());
                }
                let _ = tokio::time::timeout(PULSE_INTERVAL, runtime.next()).await;
            }
        })
        .await
        .context("timed out draining retention measurement cut")??;
        Ok(())
    }

    fn component_delta(after: usize, before: usize, rows: usize) -> f64 {
        (after as f64 - before as f64) / rows as f64
    }

    fn print_retention_delta(
        label: &str,
        delivered_rows: usize,
        before: &ui_runtime::RuntimeRetentionReport,
        after: &ui_runtime::RuntimeRetentionReport,
    ) {
        println!("\n{label}: {delivered_rows} delivered rows\n{after}");
        println!(
            "per delivered row: visible={:.1} B, canonical={:.1} B, pending mutations={:.1} B, store queue={:.1} B, SQLite={:.1} B, reducer effects={:.1} B, reducer subscriptions={:.1} B, runtime subscriptions={:.1} B, provider state={:.1} B, ask registry={:.1} B, recorder messages={:.1} B, recorder checkpoint={:.1} B, accounted={:.1} B",
            component_delta(
                after.visible_entry_bytes(),
                before.visible_entry_bytes(),
                delivered_rows,
            ),
            component_delta(
                after.canonical_entry_bytes(),
                before.canonical_entry_bytes(),
                delivered_rows,
            ),
            component_delta(
                after.pending_mutation_bytes(),
                before.pending_mutation_bytes(),
                delivered_rows,
            ),
            component_delta(
                after.store_queued_bytes,
                before.store_queued_bytes,
                delivered_rows,
            ),
            component_delta(after.sqlite_bytes, before.sqlite_bytes, delivered_rows),
            component_delta(
                after.reducer_effect_bytes,
                before.reducer_effect_bytes,
                delivered_rows,
            ),
            component_delta(
                after.reducer_subscription_bytes,
                before.reducer_subscription_bytes,
                delivered_rows,
            ),
            component_delta(
                after.runtime_subscription_bytes,
                before.runtime_subscription_bytes,
                delivered_rows,
            ),
            component_delta(
                after.provider_state_bytes,
                before.provider_state_bytes,
                delivered_rows,
            ),
            component_delta(after.ask_bytes, before.ask_bytes, delivered_rows),
            component_delta(
                after.recorder_entry_bytes,
                before.recorder_entry_bytes,
                delivered_rows,
            ),
            component_delta(
                after.recorder_checkpoint_bytes,
                before.recorder_checkpoint_bytes,
                delivered_rows,
            ),
            component_delta(
                after.accounted_bytes(),
                before.accounted_bytes(),
                delivered_rows,
            ),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn client_retention_per_delivered_row() {
        const USER: &str = "retention-user";
        const DAEMON: &str = "retention-daemon";
        let root = tempfile::tempdir().unwrap();
        let store_path = root.path().join("store.sqlite");
        let net = TestNet::builder()
            .cloud_url("https://retention.testnet.example")
            .daemon(DAEMON)
            .cloud_user(USER)
            .start()
            .await;
        let daemon = net.daemon(DAEMON);
        let mut agents = Vec::with_capacity(10);
        let mut providers = Vec::with_capacity(10);
        for chat in 0..10 {
            let (agent, provider) = daemon
                .spawn_scripted_agent(
                    &format!("retention-{chat:02}"),
                    root.path(),
                    Script::default(),
                    None,
                )
                .await
                .unwrap();
            agents.push(agent.id);
            providers.push(provider);
        }

        let (_, token) = net.user_credentials(USER);
        let client = connect_user(net.cloud_url(), net.relay_addr(), token)
            .await
            .unwrap();
        let qr = daemon.try_start_qr_pairing().await.unwrap();
        let qr = node::parse_qr_pairing_payload(&qr.encoded()).unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if client
                    .admin()
                    .list_pairing_hosts()
                    .await
                    .unwrap()
                    .iter()
                    .any(|host| host.host.id == qr.host_id && host.host.online)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let admin = client.admin();
        let pending = admin.begin_pair_qr(&qr).await.unwrap();
        admin.confirm_pair(pending).await.unwrap();

        let mut runtime = Runtime::start_with_client(
            (*client).clone(),
            RuntimeOptions {
                store_path: Some(store_path.clone()),
                ..RuntimeOptions::default()
            },
        );
        wait_for_client(&mut runtime, "ten retention agents", |runtime| {
            runtime.model().is_synchronized()
                && agents
                    .iter()
                    .all(|agent| runtime.model().agent(*agent).is_some())
        })
        .await
        .unwrap();
        for agent in &agents {
            runtime.open_chat(*agent);
        }
        wait_for_client(&mut runtime, "ten retained chats", |runtime| {
            agents.iter().all(|agent| {
                runtime
                    .model()
                    .chat(*agent)
                    .is_some_and(|chat| chat.state == ChatState::Live)
            })
        })
        .await
        .unwrap();
        assert!(store_path.is_file());

        let before_seed = agents
            .iter()
            .map(|agent| {
                runtime
                    .model()
                    .chat(*agent)
                    .and_then(|chat| chat.head_through())
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        seed_client_workload(&providers).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while !agents
                .iter()
                .zip(&before_seed)
                .enumerate()
                .all(|(chat, (agent, base))| {
                    runtime
                        .model()
                        .chat(*agent)
                        .and_then(|window| window.head_through())
                        .is_some_and(|through| through >= base + if chat == 0 { 11 } else { 10 })
                })
            {
                assert!(runtime.next().await);
            }
        })
        .await
        .unwrap();
        wait_for_retention_cut(&mut runtime, &agents, &before_seed, 10)
            .await
            .unwrap();
        assert_eq!(runtime.retention_report().ask_count, 100);
        let base = agents
            .iter()
            .map(|agent| {
                runtime
                    .model()
                    .chat(*agent)
                    .and_then(|chat| chat.head_through())
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        let baseline = runtime.retention_report();

        emit_retention_rows(&providers, 0, RETENTION_FIRST_ROWS_PER_CHAT)
            .await
            .unwrap();
        wait_for_retention_cut(&mut runtime, &agents, &base, RETENTION_FIRST_ROWS_PER_CHAT)
            .await
            .unwrap();
        let first = runtime.retention_report();
        let first_rows = RETENTION_FIRST_ROWS_PER_CHAT as usize * agents.len();
        print_retention_delta("first retained interval", first_rows, &baseline, &first);

        emit_retention_rows(
            &providers,
            RETENTION_FIRST_ROWS_PER_CHAT,
            RETENTION_SECOND_ROWS_PER_CHAT,
        )
        .await
        .unwrap();
        wait_for_retention_cut(&mut runtime, &agents, &base, RETENTION_SECOND_ROWS_PER_CHAT)
            .await
            .unwrap();
        let second = runtime.retention_report();
        let second_rows = (RETENTION_SECOND_ROWS_PER_CHAT - RETENTION_FIRST_ROWS_PER_CHAT) as usize
            * agents.len();
        print_retention_delta("second retained interval", second_rows, &first, &second);
        println!(
            "window plateau: one drawable store window caps at {} entries per chat ({} s at the soak's 2 rows/s per chat); provider layers and row-identity sets retain no entries",
            ui_state::WINDOW_MAX_ENTRIES,
            ui_state::WINDOW_MAX_ENTRIES / 2,
        );

        assert_eq!(second.chats.len(), agents.len());
        assert!(second.chats.iter().all(|chat| {
            chat.visible_entries <= ui_state::WINDOW_MAX_ENTRIES && chat.canonical_entries == 0
        }));
        assert_eq!(second.runtime_subscription_tasks, agents.len());
        assert_eq!(second.store_page_cache_bytes, 0);
        assert_eq!(second.store_write_cache_bytes, 0);

        drop(runtime);
        drop(client);
        net.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn client_soak_uses_runtime_store_and_provider_reset() {
        let root = tempfile::tempdir().unwrap();
        let daemon_dir = root.path().join("daemon");
        let client_dir = root.path().join("client");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        std::fs::create_dir_all(&client_dir).unwrap();
        let timing = ClientSoakTiming {
            stall_after: Duration::from_millis(50),
            stall_for: Duration::from_millis(100),
            reset_after: Duration::from_millis(50),
        };

        let daemon_path = daemon_dir.clone();
        let daemon = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(client_daemon_runtime(
                &daemon_path,
                timing,
                Duration::from_secs(30),
            ));
            if let Err(error) = &result {
                eprintln!("client soak daemon failed: {error:#}");
            }
            result
        });
        wait_path(&daemon_dir, "ready", Duration::from_secs(30))
            .await
            .unwrap();
        std::fs::copy(
            daemon_dir.join("connection.json"),
            client_dir.join("connection.json"),
        )
        .unwrap();
        let client_path = client_dir.clone();
        let client = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(client_runtime(
                &client_path,
                timing,
                Duration::from_secs(30),
            ));
            if let Err(error) = &result {
                eprintln!("client soak runtime failed: {error:#}");
            }
            result
        });
        wait_path(&client_dir, "ready", Duration::from_secs(30))
            .await
            .unwrap();
        std::fs::write(client_dir.join("start"), b"start\n").unwrap();
        std::fs::write(daemon_dir.join("start"), b"start\n").unwrap();
        wait_path(&daemon_dir, "reset", Duration::from_secs(30))
            .await
            .unwrap();
        wait_path(&client_dir, "reset-observed", Duration::from_secs(30))
            .await
            .unwrap();
        wait_path(&client_dir, "reset-reopened", Duration::from_secs(30))
            .await
            .unwrap();
        std::fs::write(client_dir.join("stop"), b"stop\n").unwrap();
        std::fs::write(daemon_dir.join("stop"), b"stop\n").unwrap();

        client.join().unwrap().unwrap();
        daemon.join().unwrap().unwrap();
        assert!(client_dir.join("store.sqlite").is_file());
    }

    #[test]
    fn memory_slope_ignores_warm_up_and_reports_mib_per_minute() {
        let samples = (0..=10)
            .map(|minute| TimedSample {
                seconds: minute as f64 * 60.0,
                bytes: if minute < 2 {
                    500 * 1024 * 1024
                } else {
                    (100 + minute) * 1024 * 1024
                },
            })
            .collect::<Vec<_>>();
        assert!((slope_mib_per_minute(&samples) - 1.0).abs() < 0.001);
    }

    #[test]
    fn diagnostic_slope_separates_the_planned_reset_plateau() {
        let before = (0..=36)
            .map(|sample| TimedSample {
                seconds: sample as f64 * 5.0,
                bytes: (20.0 * MIB + sample as f64 * 5.0 / 60.0 * 0.25 * MIB) as u64,
            })
            .collect::<Vec<_>>();
        let after = (37..=48)
            .map(|sample| TimedSample {
                seconds: sample as f64 * 5.0,
                bytes: (27.0 * MIB + (sample as f64 * 5.0 - 185.0) / 60.0 * 0.25 * MIB) as u64,
            })
            .collect::<Vec<_>>();
        let combined = before.iter().chain(&after).copied().collect::<Vec<_>>();

        assert!(slope_mib_per_minute(&combined) > 4.0);
        let diagnostic = slope_mib_per_minute(&before).max(slope_mib_per_minute(&after));
        assert!((diagnostic - 0.25).abs() < 0.001);
    }

    #[test]
    fn diagnostic_slope_still_reports_growth_on_each_side_of_reset() {
        let growing = |start: u64, end: u64, plateau_mib: f64| {
            (start..=end)
                .map(|sample| TimedSample {
                    seconds: sample as f64 * 5.0,
                    bytes: (plateau_mib * MIB + sample as f64 * 5.0 / 60.0 * 4.0 * MIB) as u64,
                })
                .collect::<Vec<_>>()
        };
        let before = growing(0, 36, 20.0);
        let after = growing(37, 48, 27.0);

        let diagnostic = slope_mib_per_minute(&before).max(slope_mib_per_minute(&after));
        assert!((diagnostic - 4.0).abs() < 0.001);
    }

    #[test]
    fn unset_duration_keeps_the_ten_minute_qualification_schedule() {
        let config = SoakRunConfig::from_override(None).unwrap();
        assert_eq!(config.duration, Duration::from_secs(10 * 60));
        assert!(!config.diagnostic);
        assert_eq!(
            config.client_timing().reset_after,
            Duration::from_secs(4 * 60)
        );
    }

    #[test]
    fn four_minute_diagnostic_moves_reset_before_the_slope_window_closes() {
        let config = SoakRunConfig::from_override(Some(OsStr::new("240"))).unwrap();
        assert_eq!(config.duration, Duration::from_secs(4 * 60));
        assert!(config.diagnostic);
        let reset_at = config.client_timing().reset_after;
        assert_eq!(reset_at, Duration::from_secs(3 * 60));
        assert!(reset_at >= WARM_UP);
        assert!(reset_at + RESET_REOPEN_MARGIN <= config.duration);
    }

    #[test]
    fn diagnostic_duration_refuses_a_weaker_or_full_qualification_window() {
        assert!(SoakRunConfig::from_override(Some(OsStr::new("239"))).is_err());
        assert!(SoakRunConfig::from_override(Some(OsStr::new("600"))).is_err());
        assert!(SoakRunConfig::from_override(Some(OsStr::new("four"))).is_err());
    }

    #[test]
    fn derived_memory_metric_preserves_observation_contract() {
        let started_at = Utc::now();
        let ended_at = Utc::now();
        let run = memory_run(
            "fixture slope",
            0.25,
            1.0,
            Unit::MegabytesPerMinute,
            CLIENT_WORKLOAD,
            Statistic::Worst,
            121,
            started_at,
            ended_at,
        );
        assert_eq!(run.samples.len(), 1);
        assert_eq!(run.observation_count, 121);
        assert_eq!(run.started_at, started_at);
        assert_eq!(run.ended_at, ended_at);
    }
}
