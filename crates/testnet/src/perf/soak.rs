use std::collections::VecDeque;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fold::claude_sdk::{ClaudeSdkEntry, ClaudeSdkFold};
use fold::{Baseline, Input, Mutation, MutationOracle, ProviderFold};

use super::{Machine, Metric, MetricRun, Report, Sample, Statistic, Unit, Workload};

const SOAK_DURATION: Duration = Duration::from_secs(10 * 60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const PULSE_INTERVAL: Duration = Duration::from_millis(50);
const WARM_UP: Duration = Duration::from_secs(2 * 60);
const MIB: f64 = 1024.0 * 1024.0;
const SEED: u64 = 0xA6_2026_0917;

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

    let mut client = ChildGuard::spawn(&executable, "client", &client_dir)?;
    wait_marker(&mut client, &client_dir.join("ready"))?;

    let started = Instant::now();
    let mut client_samples = Vec::with_capacity(121);
    let mut daemon_samples = Vec::with_capacity(121);
    for index in 0..=SOAK_DURATION.as_secs() / SAMPLE_INTERVAL.as_secs() {
        ensure_running(&mut client, "client")?;
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
    std::fs::write(daemon_dir.join("stop"), b"stop\n")?;
    client.wait_success()?;
    daemon.wait_success()?;

    let memory_name = daemon_active.name();
    println!("memory measure: {memory_name} (sampled every 5 s)");
    let runs = vec![
        memory_run(
            "bounded client memory slope",
            slope_mib_per_minute(&client_samples),
            1.0,
            Unit::MegabytesPerMinute,
            CLIENT_WORKLOAD,
        ),
        memory_run(
            "bounded client memory peak",
            peak_mib(&client_samples),
            300.0,
            Unit::Megabytes,
            CLIENT_WORKLOAD,
        ),
        memory_run(
            "bounded daemon memory slope",
            slope_mib_per_minute(&daemon_samples),
            1.0,
            Unit::MegabytesPerMinute,
            DAEMON_WORKLOAD,
        ),
        memory_run(
            "daemon memory per idle agent",
            daemon_idle.bytes.saturating_sub(daemon_baseline.bytes) as f64 / MIB / 200.0,
            2.0,
            Unit::Megabytes,
            DAEMON_WORKLOAD,
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
        "daemon" => daemon_child(directory),
        _ => bail!("unknown memory soak child {kind:?}"),
    }
}

fn client_child(directory: &Path) -> Result<()> {
    let mut harness = ClientMemoryHarness::new();
    std::fs::write(directory.join("ready"), b"ready\n")?;
    let started = Instant::now();
    let mut iteration = 0;
    while !directory.join("stop").is_file() {
        let pulse = Instant::now();
        harness.pulse(iteration)?;
        iteration += 1;
        std::thread::sleep(PULSE_INTERVAL.saturating_sub(pulse.elapsed()));
        if started.elapsed() > SOAK_DURATION + Duration::from_secs(30) {
            bail!("client soak parent did not stop the child");
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

struct ClientMemoryHarness {
    chats: Vec<ClientChat>,
}

impl ClientMemoryHarness {
    fn new() -> Self {
        Self {
            chats: (0..10).map(|_| ClientChat::new()).collect(),
        }
    }

    fn pulse(&mut self, iteration: u64) -> Result<()> {
        for (chat_index, chat) in self.chats.iter_mut().enumerate() {
            if iteration == 0 {
                chat.observe(serde_json::json!({
                    "type": "assistant",
                    "uuid": format!("client-oversized-{chat_index}"),
                    "message": {"content": "x".repeat(if chat_index == 0 { 5 * 1024 * 1024 } else { 16 })},
                }))?;
                for ask in 0..10 {
                    chat.observe(serde_json::json!({
                        "type": "amux.claude_sdk.permission_required",
                        "request_id": format!("client-{chat_index}-ask-{ask}"),
                        "tool_name": "Write",
                        "input": {"file_path": format!("/tmp/client-{chat_index}-{ask}")},
                        "suggestions": [],
                    }))?;
                }
            }
            chat.stalled = (200..300).contains(&iteration);
            if iteration == 300 {
                chat.flush_pending()?;
            }
            if iteration == 4_800 {
                chat.segment += 1;
                chat.fold.begin(
                    chat.segment,
                    Baseline::Gap {
                        after: chat.sequence,
                    },
                );
            }
            chat.observe(corpus_row(chat_index, iteration))?;
        }
        Ok(())
    }
}

struct ClientChat {
    fold: ClaudeSdkFold,
    oracle: MutationOracle<ClaudeSdkEntry>,
    pending: VecDeque<Vec<Mutation<ClaudeSdkEntry>>>,
    sequence: u64,
    segment: u32,
    stalled: bool,
}

impl ClientChat {
    fn new() -> Self {
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        Self {
            fold,
            oracle: MutationOracle::default(),
            pending: VecDeque::new(),
            sequence: 0,
            segment: 1,
            stalled: false,
        }
    }

    fn observe(&mut self, row: serde_json::Value) -> Result<()> {
        self.sequence += 1;
        let payload = serde_json::to_vec(&row)?;
        let mutations = self
            .fold
            .apply(Input::Row {
                seq: self.sequence,
                published_at: Utc::now(),
                activity_at: None,
                historical: false,
                payload: &payload,
            })
            .mutations;
        if self.stalled {
            self.pending.push_back(mutations);
        } else {
            self.oracle.apply(&mutations)?;
        }
        self.bound_window()?;
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<()> {
        while let Some(mutations) = self.pending.pop_front() {
            self.oracle.apply(&mutations)?;
        }
        self.bound_window()
    }

    fn bound_window(&mut self) -> Result<()> {
        let entries = self.oracle.entries();
        if entries.len() > 400 {
            self.oracle = MutationOracle::from_state(
                self.segment,
                fold::DESKTOP_ENTRY_MAX_BYTES,
                entries.into_iter().rev().take(400).collect(),
                Vec::new(),
                Vec::new(),
            )?;
        }
        Ok(())
    }
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

fn memory_run(
    name: &'static str,
    value: f64,
    budget: f64,
    unit: Unit,
    workload: Workload,
) -> MetricRun {
    let at = Utc::now();
    MetricRun {
        metric: Metric {
            name,
            statistic: Statistic::Worst,
            budget,
            unit,
            workload,
        },
        samples: vec![Sample {
            metric: name,
            value,
            unit,
        }],
        started_at: at,
        ended_at: at,
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
        if started.elapsed() > Duration::from_secs(20) {
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
    fn client_memory_window_and_pending_work_stay_bounded() {
        let mut harness = ClientMemoryHarness::new();
        for iteration in 0..600 {
            harness.pulse(iteration).unwrap();
        }
        for chat in harness.chats {
            assert!(chat.oracle.entries().len() <= 400);
            assert!(chat.pending.is_empty());
            assert!(chat.fold.tip_bytes() <= fold::TIP_MAX_BYTES);
        }
    }
}
