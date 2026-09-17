use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{TimeZone, Utc};
use fold::claude_sdk::ClaudeSdkFold;
use fold::{
    Baseline, CommitOutcome, ExpectedHead, Head, Input, ProviderFold, SegmentTransition,
    WindowBudget, WindowInterest,
};
use model::{Agent, AgentKind, ClaudeDriver, HostEntry, HostTrustStatus};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use store::Store;
use tempfile::TempDir;
use tui::{ChatView, FrameContext, Theme, ViewState, render};
use ui_state::store::{
    ChatCommand, ChatStreamMsg, LoadedDto, ProfileGeneration, ReplayFactsDto, ReplayOutcomeDto,
    StoreMsg, StoreOp,
};
use ui_state::{Effect, Model, Msg, update};

use super::{Metric, MetricRun, Sample, Statistic, Unit, Workload};

const SEED: u64 = 0xA6_2026_0917;
const PROFILE: ProfileGeneration = ProfileGeneration(0);
const VIEWPORT: (u16, u16) = (120, 40);

const COLD_WORKLOAD: Workload = Workload {
    description: "exec to flushed fleet frame, daemon unreachable, 10 MiB stored chat",
    seed: SEED,
    identity_growth: "40 or 200 stored fleet agents and 5,000 chat entries",
    warm_up: "one unmeasured child exec",
};

const ATTACH_WORKLOAD: Workload = Workload {
    description: "open 5,000-entry stored chat, then fold 2,000 rows/s",
    seed: SEED,
    identity_growth: "5,000 stored and 4,000 fresh SDK prompt ids",
    warm_up: "one load and painted frame",
};

const COMMIT_WORKLOAD: Workload = Workload {
    description: "50 mutations at 20 batches/s while a second store reads fleet at 1 Hz",
    seed: SEED,
    identity_growth: "5,000 fresh SDK prompt ids",
    warm_up: "one initial commit",
};

const SCROLL_WORKLOAD: Workload = Workload {
    description: "replace the client window while paging through 50,000 stored entries and back",
    seed: SEED,
    identity_growth: "50,000 fixed SDK prompt ids",
    warm_up: "load the newest retained window",
};

const SWEEP_WORKLOAD: Workload = Workload {
    description: "fill beyond a reduced soft budget and maintain in bounded slices",
    seed: SEED,
    identity_growth: "the same 50,000-entry chat",
    warm_up: "none",
};

pub(super) fn run_store() -> Result<Vec<MetricRun>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build store performance runtime")?;
    runtime.block_on(async {
        let mut runs = cold_start().await?;
        runs.extend(attach_during_flood().await?);
        runs.push(commit_latency().await?);
        runs.extend(scroll_and_sweep().await?);
        Ok(runs)
    })
}

async fn cold_start() -> Result<Vec<MetricRun>> {
    let mut runs = Vec::new();
    for agents in [40_usize, 200] {
        let temp = TempDir::new().context("cold-start store directory")?;
        let path = temp.path().join("store.sqlite");
        let store = Store::open(&path).await?;
        seed_fleet(&store, agents).await?;
        seed_chat(&store, agent_id(0), 5_000, 2_100).await?;
        store.close().await;
        let database_bytes = std::fs::metadata(&path)?.len();
        if database_bytes < 10 * 1024 * 1024 {
            bail!("cold-start fixture is only {database_bytes} bytes; expected at least 10 MiB");
        }

        let executable = std::env::current_exe().context("resolve performance executable")?;
        run_cold_child(&executable, &path, agents)?;
        let started_at = Utc::now();
        let mut samples = Vec::with_capacity(7);
        for _ in 0..7 {
            let began = Instant::now();
            run_cold_child(&executable, &path, agents)?;
            samples.push(began.elapsed().as_secs_f64() * 1_000.0);
        }
        let median_name = if agents == 40 {
            "TUI cold start (40 agents) median"
        } else {
            "TUI cold start (200 agents) median"
        };
        let worst_name = if agents == 40 {
            "TUI cold start (40 agents) worst"
        } else {
            "TUI cold start (200 agents) worst"
        };
        runs.push(metric_run(
            median_name,
            Statistic::Median,
            100.0,
            Unit::Milliseconds,
            COLD_WORKLOAD,
            started_at,
            &samples,
        ));
        runs.push(metric_run(
            worst_name,
            Statistic::Worst,
            200.0,
            Unit::Milliseconds,
            COLD_WORKLOAD,
            started_at,
            &samples,
        ));
    }
    Ok(runs)
}

fn run_cold_child(executable: &Path, path: &Path, agents: usize) -> Result<()> {
    let status = std::process::Command::new(executable)
        .arg("--cold-child")
        .arg(path)
        .arg(agents.to_string())
        .status()
        .context("run cold-start child")?;
    if !status.success() {
        bail!("cold-start child exited with {status}");
    }
    Ok(())
}

pub fn cold_child(path: &Path, expected_agents: usize) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build cold-start child runtime")?;
    runtime.block_on(async {
        let store = Store::open(path).await?;
        let generations = generations(&store);
        let fleet = store.fleet(generations).await?;
        if fleet.agents.len() != expected_agents {
            bail!(
                "cold-start fleet has {} agents, expected {expected_agents}",
                fleet.agents.len()
            );
        }
        let mut model = Model::default();
        let effects = update(
            &mut model,
            Msg::StoreStartup {
                profile: PROFILE,
                generations,
            },
        );
        let fleet_op = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::Store(StoreOp::FleetLoad { op, .. }) => Some(*op),
                _ => None,
            })
            .context("store startup emitted no fleet load")?;
        update(
            &mut model,
            Msg::Store(StoreMsg::FleetLoaded {
                profile: PROFILE,
                op: fleet_op,
                fleet,
            }),
        );
        let view = ViewState::default();
        let context = FrameContext {
            viewport: VIEWPORT,
            theme: Theme::default(),
            now: Utc::now(),
        };
        let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
        terminal.draw(|frame| render(&model, &view, &context, frame))?;
        store.close().await;
        Ok(())
    })
}

async fn attach_during_flood() -> Result<Vec<MetricRun>> {
    let temp = TempDir::new().context("attach store directory")?;
    let path = temp.path().join("store.sqlite");
    let store = Store::open(&path).await?;
    seed_fleet(&store, 1).await?;
    seed_chat(&store, agent_id(0), 5_000, 80).await?;
    // Warm every rendering and allocation path once outside the samples.
    let loaded = store
        .load::<ClaudeSdkFold>(agent_id(0), WindowBudget::desktop(0))
        .await?;
    paint_loaded(loaded.clone())?;
    let started_at = Utc::now();
    let mut paint_samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let began = Instant::now();
        let sample_loaded = store
            .load::<ClaudeSdkFold>(agent_id(0), WindowBudget::desktop(0))
            .await?;
        paint_loaded(sample_loaded)?;
        paint_samples.push(began.elapsed().as_secs_f64() * 1_000.0);
    }

    let (mut model, attempt) = loaded_model(loaded)?;
    let now = Utc::now();
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id(0),
            attempt,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 9_000,
                    selected_from: 5_001,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: now,
            },
        },
    );
    let caught_up = Instant::now();
    for batch in 0..2_u64 {
        let entries = (0..2_000_u64)
            .map(|offset| {
                let seq = 5_001 + batch * 2_000 + offset;
                ui_state::StreamEntry::observed(seq, now, sdk_prompt(seq, 80))
            })
            .collect();
        update(
            &mut model,
            Msg::ChatStream {
                agent: agent_id(0),
                attempt,
                event: ChatStreamMsg::Batch { at: now, entries },
            },
        );
    }
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id(0),
            attempt,
            event: ChatStreamMsg::ReplayComplete { at: now },
        },
    );
    let caught_up_ms = caught_up.elapsed().as_secs_f64() * 1_000.0;
    store.close().await;
    Ok(vec![
        metric_run(
            "attach first painted window",
            Statistic::Median,
            50.0,
            Unit::Milliseconds,
            ATTACH_WORKLOAD,
            started_at,
            &paint_samples,
        ),
        metric_run(
            "attach caught up",
            Statistic::Worst,
            2_000.0,
            Unit::Milliseconds,
            ATTACH_WORKLOAD,
            started_at,
            &[caught_up_ms],
        ),
    ])
}

fn paint_loaded(loaded: fold::Loaded<ClaudeSdkFold>) -> Result<()> {
    let (model, _) = loaded_model(loaded)?;
    let mut chat = ChatView::open(&model, agent_id(0), 'a', false).context("open stored chat")?;
    chat.reconcile(&model);
    let view = ViewState {
        chat: Some(chat),
        ..ViewState::default()
    };
    let context = FrameContext {
        viewport: VIEWPORT,
        theme: Theme::default(),
        now: Utc::now(),
    };
    let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
    terminal.draw(|frame| render(&model, &view, &context, frame))?;
    Ok(())
}

fn loaded_model(loaded: fold::Loaded<ClaudeSdkFold>) -> Result<(Model, fold::StreamAttempt)> {
    let mut model = inventory_model(1);
    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::Open { agent: agent_id(0) }),
    );
    let (attempt, op) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Load { attempt, op, .. }) => Some((*attempt, *op)),
            _ => None,
        })
        .context("chat open emitted no store load")?;
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Loaded {
            profile: PROFILE,
            attempt,
            op,
            agent: agent_id(0),
            loaded: Box::new(LoadedDto::ClaudeSdk(loaded)),
        }),
    );
    let stream_attempt = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::OpenStoreStream { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .context("loaded chat emitted no stream open")?;
    Ok((model, stream_attempt))
}

async fn commit_latency() -> Result<MetricRun> {
    let temp = TempDir::new().context("commit store directory")?;
    let path = temp.path().join("store.sqlite");
    let store = Store::open(&path).await?;
    seed_fleet(&store, 1).await?;
    let mut state = ChatSeed::new();
    let _ = commit_batch(&store, agent_id(0), &mut state, 50, 80).await?;

    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let reader = Store::open(&reader_path).await?;
            while !reader_stop.load(Ordering::Acquire) {
                let _ = reader.fleet(generations(&reader)).await?;
                std::thread::sleep(Duration::from_secs(1));
            }
            reader.close().await;
            Ok(())
        })
    });

    let started_at = Utc::now();
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let began = Instant::now();
        commit_batch(&store, agent_id(0), &mut state, 50, 80).await?;
        samples.push(began.elapsed().as_secs_f64() * 1_000.0);
        let rest = Duration::from_millis(50).saturating_sub(began.elapsed());
        std::thread::sleep(rest);
    }
    stop.store(true, Ordering::Release);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("fleet reader panicked"))??;
    store.close().await;
    Ok(metric_run(
        "commit latency",
        Statistic::P99,
        20.0,
        Unit::Milliseconds,
        COMMIT_WORKLOAD,
        started_at,
        &samples,
    ))
}

async fn scroll_and_sweep() -> Result<Vec<MetricRun>> {
    let temp = TempDir::new().context("scroll store directory")?;
    let path = temp.path().join("store.sqlite");
    let store = Store::open(&path).await?;
    seed_fleet(&store, 1).await?;
    seed_chat(&store, agent_id(0), 50_000, 48).await?;

    let loaded = store
        .load::<ClaudeSdkFold>(agent_id(0), WindowBudget::desktop(0))
        .await?;
    let before = super::sample_memory(std::process::id())?.bytes as f64;
    let started_at = Utc::now();
    let mut seen = loaded.window.len();
    let mut next = loaded.first_page;
    let mut window = loaded.window;
    while let Some(token) = next {
        let page = store.page::<ClaudeSdkFold>(agent_id(0), token, 400).await?;
        seen += page.entries.len();
        next = page.next;
        window = page.entries;
    }
    if seen != 50_000 {
        bail!("scroll workload visited {seen} entries, expected 50,000");
    }
    drop(window);
    let tip = store
        .load::<ClaudeSdkFold>(agent_id(0), WindowBudget::desktop(1))
        .await?;
    drop(tip);
    let after = super::sample_memory(std::process::id())?.bytes as f64;
    let memory_ratio = after / before;

    let filled_bytes = store_disk_bytes(&path);
    let soft_budget = filled_bytes.saturating_mul(3) / 4;
    let sweep_started = Instant::now();
    let mut longest_slice = Duration::ZERO;
    loop {
        let slice_started = Instant::now();
        let report = store
            .maintain(
                store::Budget {
                    store_target_bytes: soft_budget,
                    ..store::Budget::default()
                },
                Duration::from_millis(75),
            )
            .await?;
        longest_slice = longest_slice.max(slice_started.elapsed());
        if !report.deadline_reached || sweep_started.elapsed() >= Duration::from_secs(5) {
            break;
        }
    }
    let sweep_ms = sweep_started.elapsed().as_secs_f64() * 1_000.0;
    let final_percent = store_disk_bytes(&path) as f64 / soft_budget as f64 * 100.0;
    store.close().await;

    Ok(vec![
        metric_run(
            "scroll-back memory return",
            Statistic::Worst,
            1.10,
            Unit::Ratio,
            SCROLL_WORKLOAD,
            started_at,
            &[memory_ratio],
        ),
        metric_run(
            "growth after sweep",
            Statistic::Worst,
            90.0,
            Unit::Percent,
            SWEEP_WORKLOAD,
            started_at,
            &[final_percent],
        ),
        metric_run(
            "growth sweep duration",
            Statistic::Worst,
            5_000.0,
            Unit::Milliseconds,
            SWEEP_WORKLOAD,
            started_at,
            &[sweep_ms],
        ),
        metric_run(
            "growth longest statement upper bound",
            Statistic::Worst,
            100.0,
            Unit::Milliseconds,
            SWEEP_WORKLOAD,
            started_at,
            &[longest_slice.as_secs_f64() * 1_000.0],
        ),
    ])
}

fn store_disk_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .into_iter()
    .filter_map(|path| std::fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}

async fn seed_fleet(store: &Store, count: usize) -> Result<()> {
    let generations = generations(store);
    let host = HostEntry {
        id: host_id(),
        name: "perf-mac".to_owned(),
        online: false,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        capabilities: Some(Default::default()),
        trust_status: HostTrustStatus::Trusted,
        last_dial_error: None,
        platform: None,
    };
    store
        .apply_fleet(generations, fold::FleetDelta::Host { host, revision: 1 })
        .await?;
    for index in 0..count {
        store
            .apply_fleet(
                generations,
                fold::FleetDelta::AgentUp {
                    agent: agent(index),
                    revision: index as u64 + 2,
                },
            )
            .await?;
    }
    Ok(())
}

async fn seed_chat(
    store: &Store,
    agent: model::AgentId,
    entries: usize,
    text: usize,
) -> Result<()> {
    let mut state = ChatSeed::new();
    while state.through < entries as u64 {
        let count = (entries as u64 - state.through).min(50) as usize;
        commit_batch(store, agent, &mut state, count, text).await?;
    }
    Ok(())
}

struct ChatSeed {
    fold: ClaudeSdkFold,
    expected: ExpectedHead,
    through: u64,
}

impl ChatSeed {
    fn new() -> Self {
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        Self {
            fold,
            expected: ExpectedHead::Absent { fence: 0 },
            through: 0,
        }
    }
}

async fn commit_batch(
    store: &Store,
    agent: model::AgentId,
    state: &mut ChatSeed,
    count: usize,
    text: usize,
) -> Result<Duration> {
    let first = state.through + 1;
    let mut mutations = Vec::with_capacity(count);
    for seq in first..first + count as u64 {
        let payload = serde_json::to_vec(&sdk_prompt(seq, text))?;
        mutations.extend(
            state
                .fold
                .apply(Input::Row {
                    seq,
                    published_at: now(seq as i64),
                    activity_at: Some(now(seq as i64)),
                    historical: false,
                    payload: &payload,
                })
                .mutations,
        );
    }
    state.through += count as u64;
    let head = Head {
        segment: 1,
        baseline: Baseline::Start,
        through: state.through,
        tip_version: ClaudeSdkFold::TIP_VERSION,
        entry_version: ClaudeSdkFold::ENTRY_VERSION,
        tip: state.fold.clone(),
        summary: state.fold.summary(),
        observed_at: now(state.through as i64),
    };
    let transition = (first == 1).then_some(SegmentTransition {
        predecessor: None,
        successor: 1,
        baseline: Baseline::Start,
        previous_through: 0,
        selected_from: Some(1),
        replay_through: state.through,
        opened_at: now(0),
    });
    let began = Instant::now();
    let outcome = store
        .commit(
            agent,
            generations(store),
            state.expected,
            head,
            transition,
            mutations,
            WindowInterest::all(0, 8 * 1024 * 1024),
        )
        .await;
    state.expected = match outcome {
        CommitOutcome::Committed(result) => result.expected,
        CommitOutcome::Conflict(_) => bail!("performance seed conflicted with itself"),
        CommitOutcome::Refused(error) => return Err(error.into()),
    };
    Ok(began.elapsed())
}

fn inventory_model(count: usize) -> Model {
    let mut model = Model::default();
    update(
        &mut model,
        Msg::Server(ui_state::ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
    );
    update(
        &mut model,
        Msg::Server(ui_state::ServerMsg::HostUpserted {
            host: HostEntry {
                id: host_id(),
                name: "perf-mac".to_owned(),
                online: true,
                version: None,
                capabilities: Some(Default::default()),
                trust_status: HostTrustStatus::Trusted,
                last_dial_error: None,
                platform: None,
            },
        }),
    );
    for index in 0..count {
        update(
            &mut model,
            Msg::Server(ui_state::ServerMsg::AgentUpserted {
                agent: agent(index),
            }),
        );
    }
    update(
        &mut model,
        Msg::Server(ui_state::ServerMsg::HostsSynchronized),
    );
    update(
        &mut model,
        Msg::Server(ui_state::ServerMsg::AgentsSynchronized),
    );
    model
}

fn agent(index: usize) -> Agent {
    Agent {
        id: agent_id(index),
        host_id: host_id(),
        name: Some(format!("perf-agent-{index:03}")),
        command: "claude".to_owned(),
        working_dir: PathBuf::from(format!("/work/perf/{index}")),
        kind: AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        },
        readonly: false,
        args: Vec::new(),
        created_at: now(index as i64),
        parent: None,
        working_on: None,
        summary: None,
        progress: None,
        inventory_revision: index as u64 + 2,
    }
}

fn sdk_prompt(seq: u64, text: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "uuid": format!("00000000-0000-4000-8000-{seq:012}"),
        "message": {"content": format!("row {seq}: {}", "x".repeat(text))},
    })
}

fn generations(store: &Store) -> fold::Generations {
    store
        .generations()
        .for_provider("claude_sdk")
        .expect("Claude SDK generation")
}

fn host_id() -> model::HostId {
    model::HostId::from_u128(0xA6)
}

fn agent_id(index: usize) -> model::AgentId {
    model::AgentId::from_u128(0xA6_0000 + index as u128)
}

fn now(offset: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + offset, 0).unwrap()
}

fn metric_run(
    name: &'static str,
    statistic: Statistic,
    budget: f64,
    unit: Unit,
    workload: Workload,
    started_at: chrono::DateTime<Utc>,
    values: &[f64],
) -> MetricRun {
    MetricRun {
        metric: Metric {
            name,
            statistic,
            budget,
            unit,
            workload,
        },
        samples: values
            .iter()
            .map(|value| Sample {
                metric: name,
                value: *value,
                unit,
            })
            .collect(),
        started_at,
        ended_at: Utc::now(),
    }
}
