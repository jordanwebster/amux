use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::store::ChatState;
use uuid::Uuid;

use super::{Machine, Metric, MetricRun, Report, Sample, Statistic, Unit, Workload};
use crate::script::{Provider, Script, ScriptAsk, Step};
use crate::{TestNet, connect_user};

const SOAK_DURATION: Duration = Duration::from_secs(10 * 60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const PULSE_INTERVAL: Duration = Duration::from_millis(50);
const WARM_UP: Duration = Duration::from_secs(2 * 60);
const MIB: f64 = 1024.0 * 1024.0;
const SEED: u64 = 0xA6_2026_0917;

#[derive(Clone, Copy)]
struct ClientSoakTiming {
    stall_after: Duration,
    stall_for: Duration,
    reset_iteration: u64,
}

const CLIENT_SOAK_TIMING: ClientSoakTiming = ClientSoakTiming {
    stall_after: Duration::from_secs(10),
    stall_for: Duration::from_secs(5),
    reset_iteration: 4_800,
};

const CLIENT_WORKLOAD: Workload = Workload {
    description: "10 open chats replaying fresh-identity corpora for 10 minutes",
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

pub fn run_soak(machine: Machine) -> Result<()> {
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
    let mut client_samples = Vec::with_capacity(121);
    let mut daemon_samples = Vec::with_capacity(121);
    for index in 0..=SOAK_DURATION.as_secs() / SAMPLE_INTERVAL.as_secs() {
        ensure_running(&mut client, "client")?;
        ensure_running(&mut client_daemon, "client daemon")?;
        ensure_running(&mut daemon, "daemon")?;
        let at = started.elapsed().as_secs_f64();
        client_samples.push(TimedSample {
            seconds: at,
            bytes: super::sample_memory(client.id())?.bytes,
        });
        daemon_samples.push(TimedSample {
            seconds: at,
            bytes: super::sample_memory(daemon.id())?.bytes,
        });
        if index * SAMPLE_INTERVAL.as_secs() < SOAK_DURATION.as_secs() {
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

    let memory_name = daemon_active.name();
    println!("memory measure: {memory_name} (sampled every 5 s)");
    let runs = vec![
        memory_run(
            "bounded client memory slope",
            slope_mib_per_minute(&client_samples),
            1.0,
            Unit::MegabytesPerMinute,
            CLIENT_WORKLOAD,
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
            CLIENT_WORKLOAD,
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
            DAEMON_WORKLOAD,
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
            DAEMON_WORKLOAD,
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
            DAEMON_WORKLOAD,
            Statistic::Peak,
            observation_count,
            started_at,
            ended_at,
        ),
    ];
    let report = Report::evaluate(machine, runs, None, false)?;
    report.print();
    if !report.passed() {
        bail!("one or more memory soak metrics missed their budget");
    }
    Ok(())
}

pub fn soak_child(kind: &str, directory: &Path) -> Result<()> {
    match kind {
        "client" => client_child(directory),
        "client-daemon" => client_daemon_child(directory),
        "daemon" => daemon_child(directory),
        _ => bail!("unknown memory soak child {kind:?}"),
    }
}

fn client_child(directory: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("build client soak runtime")?;
    runtime.block_on(client_runtime(directory, CLIENT_SOAK_TIMING))
}

#[derive(Serialize, Deserialize)]
struct ClientDaemonConnection {
    cloud_url: String,
    relay: SocketAddr,
    token: String,
    qr: String,
    agents: Vec<Uuid>,
}

async fn client_runtime(directory: &Path, timing: ClientSoakTiming) -> Result<()> {
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
                .any(|host| host.id == qr.host_id && host.online)
            {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("timed out waiting for soak daemon cloud presence")??;
    admin
        .pair_qr_cloud_peer(qr.host_id, qr.secret)
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
                .is_some_and(|chat| chat.state == ChatState::Live && !chat.live_only)
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
    while !directory.join("stop").is_file() {
        let _ = tokio::time::timeout(PULSE_INTERVAL, runtime.next()).await;
        if !reset_observed && resets.load(Ordering::Relaxed) >= connection.agents.len() {
            std::fs::write(directory.join("reset-observed"), b"reset\n")?;
            reset_observed = true;
        }
        if started.elapsed() > SOAK_DURATION + Duration::from_secs(120) {
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

fn client_daemon_child(directory: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("build client-daemon soak runtime")?;
    runtime.block_on(client_daemon_runtime(directory, CLIENT_SOAK_TIMING))
}

async fn client_daemon_runtime(directory: &Path, timing: ClientSoakTiming) -> Result<()> {
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
    while !directory.join("stop").is_file() {
        let pulse = Instant::now();
        for (chat, provider) in providers.iter().enumerate() {
            provider
                .emit(vec![corpus_row(chat, iteration)])
                .await
                .with_context(|| format!("replay client corpus for chat {chat}"))?;
        }
        if iteration == timing.reset_iteration {
            for provider in &providers {
                provider
                    .play(vec![Step::Compaction])
                    .await
                    .context("reset client stream through provider relink")?;
            }
            std::fs::write(directory.join("reset"), b"reset\n")?;
        }
        iteration += 1;
        tokio::time::sleep(PULSE_INTERVAL.saturating_sub(pulse.elapsed())).await;
        if started.elapsed() > SOAK_DURATION + Duration::from_secs(30) {
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

fn daemon_child(directory: &Path) -> Result<()> {
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
            if started.elapsed() > SOAK_DURATION + Duration::from_secs(30) {
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
            reset_iteration: 1,
        };

        let daemon_path = daemon_dir.clone();
        let daemon = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(client_daemon_runtime(&daemon_path, timing));
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
            let result = runtime.block_on(client_runtime(&client_path, timing));
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
