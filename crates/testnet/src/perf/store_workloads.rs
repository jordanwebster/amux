use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::{TimeZone, Utc};
use fold::claude_sdk::ClaudeSdkFold;
use fold::{
    Baseline, CommitOutcome, ExpectedHead, Head, Input, ProviderFold, SegmentTransition,
    WindowInterest,
};
use model::{
    Agent, AgentIdentifier, AgentKind, ClaudeDriver, HostEntry, HostTrustStatus, ReplayOutcome,
    ReplayQuery, SessionArgs, SessionOutput, SubscribeSessionEvent, SubscribeSessionRequest,
};
use node::{ColorSetting, Config, ThemeSetting, UiSettings};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use store::Store;
use tempfile::TempDir;
use tui::{ChatView, FrameContext, Theme, ViewState, render};
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::Model;
use ui_state::store::{ChatState, WINDOW_MAX_BYTES, WINDOW_MAX_ENTRIES};
use uuid::Uuid;

use super::{Metric, MetricRun, Sample, Statistic, Unit, Workload};
use crate::TestNet;

const SEED: u64 = 0xA6_2026_0917;
const VIEWPORT: (u16, u16) = (120, 40);

const COLD_WORKLOAD: Workload = Workload {
    description: "exec to flushed fleet frame, daemon unreachable, 10 MiB stored chat",
    seed: SEED,
    identity_growth: "40 or 200 stored fleet agents and 5,000 chat entries",
    warm_up: "one unmeasured child exec",
};

const ATTACH_WORKLOAD: Workload = Workload {
    description: "client runtime opens 5,000 stored entries during a live 2,000 rows/s stream",
    seed: SEED,
    identity_growth: "5,000 stored and fresh SDK prompt ids throughout the open",
    warm_up: "daemon ring and store seeded before the measured open command",
};

const DELTA_WORKLOAD: Workload = Workload {
    description: "real client subscription to a testnet daemon after an exact stored cursor",
    seed: SEED,
    identity_growth: "5,000 retained rows followed by fresh row payloads per reconnect",
    warm_up: "client runtime persists the initial daemon cursor",
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
        let mut runs = reconnect_delta().await?;
        runs.extend(cold_start().await?);
        runs.extend(attach_during_flood().await?);
        runs.push(commit_latency().await?);
        runs.extend(scroll_and_sweep().await?);
        Ok(runs)
    })
}

async fn cold_start() -> Result<Vec<MetricRun>> {
    let mut runs = Vec::new();
    for agents in [40_usize, 200] {
        let temp = tempfile::Builder::new()
            .prefix("amux-cold-start-")
            .tempdir_in("/tmp")
            .context("cold-start installation directory")?;
        let installation_root = temp.path().join("installation");
        std::fs::create_dir_all(&installation_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&installation_root, std::fs::Permissions::from_mode(0o700))?;
        }
        let profile_id = node::installation::ProfileId::new();
        let paths = node::installation::ProfilePaths::for_id(&installation_root, profile_id)?;
        let data_dir = paths.data_dir.clone();
        let path = data_dir.join("store.sqlite");
        let store = Store::open(&path).await?;
        seed_fleet(&store, agents).await?;
        seed_chat(&store, agent_id(0), 5_000, 2_100).await?;
        store.close().await;
        let database_bytes = std::fs::metadata(&path)?.len();
        if database_bytes < 10 * 1024 * 1024 {
            bail!("cold-start fixture is only {database_bytes} bytes; expected at least 10 MiB");
        }

        node::ensure_device_files_in(&data_dir)?;
        let installation_path = installation_root.join("installation.yaml");
        let installation = node::InstallationConfig {
            root: installation_root.clone(),
            host_name: "perf-mac".to_owned(),
            front_door_socket: installation_root.join("front-door-unreachable.sock"),
            prevent_idle_sleep: Some(false),
            ui: UiSettings {
                theme: ThemeSetting::Dark,
                color: ColorSetting::Ansi,
                ..UiSettings::default()
            },
            ..node::InstallationConfig::default()
        };
        std::fs::write(&installation_path, serde_yaml::to_string(&installation)?)?;
        let config_path = paths.config_path.context("profile config path")?;
        std::fs::write(
            &config_path,
            serde_yaml::to_string(&node::ProfileConfig {
                installation_config: installation_path,
                socket_path: paths.socket_path,
                data_dir,
                state_path: paths.state_path,
                cloud_url: Config::default().cloud_url,
                tcp_port: None,
            })?,
        )?;
        let record = node::installation::ProfileRecord {
            id: profile_id,
            label: node::installation::ProfileLabel {
                override_name: Some("Performance".into()),
                ..Default::default()
            },
            binding: None,
            paused: false,
            revision: 1,
        };
        std::fs::write(
            installation_root.join("registry.yaml"),
            serde_yaml::to_string(&serde_json::json!({ "profiles": [record] }))?,
        )?;
        std::fs::create_dir_all(installation_root.join("state"))?;
        std::fs::write(
            installation_root.join("state/last-profile"),
            profile_id.to_string(),
        )?;

        let executable = std::env::current_exe()
            .context("resolve performance executable")?
            .parent()
            .context("performance executable has no parent")?
            .join(if cfg!(windows) { "amux.exe" } else { "amux" });
        ensure!(
            executable.is_file(),
            "release amux binary is missing at {}; run through `just perf`",
            executable.display()
        );
        run_release_tui(&executable, &config_path)?;
        let started_at = Utc::now();
        let mut samples = Vec::with_capacity(7);
        for _ in 0..7 {
            samples.push(run_release_tui(&executable, &config_path)?);
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

fn run_release_tui(executable: &Path, config_path: &Path) -> Result<f64> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: VIEWPORT.1,
            cols: VIEWPORT.0,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open cold-start pseudo-terminal")?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("clone cold-start pseudo-terminal reader")?;
    let mut writer = pair
        .master
        .take_writer()
        .context("open cold-start pseudo-terminal writer")?;
    let (sender, receiver) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 4_096];
        let mut terminal_query = Vec::new();
        let mut answered_attributes = false;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    terminal_query.extend_from_slice(&buffer[..length]);
                    if !answered_attributes
                        && terminal_query
                            .windows(b"\x1b[?u\x1b[c".len())
                            .any(|window| window == b"\x1b[?u\x1b[c")
                    {
                        // A pseudo-terminal is only the transport half of a terminal.
                        // Answer the release client's capability probe as the generic
                        // xterm named below would, otherwise crossterm waits its full
                        // two-second missing-emulator timeout before the first frame.
                        if writer.write_all(b"\x1b[?1;2c").is_err() || writer.flush().is_err() {
                            break;
                        }
                        answered_attributes = true;
                    }
                    if terminal_query.len() > 64 {
                        terminal_query.drain(..terminal_query.len() - 64);
                    }
                    if sender.send(buffer[..length].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut command = CommandBuilder::new(executable);
    command.arg("--config");
    command.arg(config_path);
    command.arg("ui");
    command.env("TERM", "xterm-256color");
    command.env_remove("AMUX_CONFIG");

    let began = Instant::now();
    let mut child = pair
        .slave
        .spawn_command(command)
        .context("exec release amux for cold-start measurement")?;
    drop(pair.slave);

    let deadline = began + Duration::from_secs(5);
    let mut output = Vec::new();
    let observed = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(anyhow::anyhow!(
                "release amux did not paint a seeded fleet row within five seconds; output: {}",
                String::from_utf8_lossy(&output)
            ));
        }
        match receiver.recv_timeout(remaining) {
            Ok(bytes) => {
                output.extend_from_slice(&bytes);
                if output
                    .windows(b"perf-agent-".len())
                    .any(|window| window == b"perf-agent-")
                {
                    break Ok(began.elapsed());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(anyhow::anyhow!(
                    "release amux exited before painting a seeded fleet row; output: {}",
                    String::from_utf8_lossy(&output)
                ));
            }
        }
    };

    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader_thread.join();
    Ok(observed?.as_secs_f64() * 1_000.0)
}

async fn reconnect_delta() -> Result<Vec<MetricRun>> {
    let temp = TempDir::new().context("reconnect delta store directory")?;
    let net = TestNet::builder().daemon("perf-delta").start().await;
    let daemon = net.daemon("perf-delta");
    daemon.script_sdk_sessions(sdk_script()).await;
    let agent = daemon
        .spawn_scripted_sdk_agent("perf-delta", temp.path())
        .await?;
    let store_path = temp.path().join("store.sqlite");
    let store = Store::open(&store_path).await?;
    seed_live_fleet(&store, &agent).await?;
    seed_chat(&store, agent.id, 5_000, 160).await?;
    store.close().await;
    publish_sdk_rows(&daemon, agent.id, 1, 5_000, 160).await?;
    let client = daemon.admin_client().await;
    wait_for_daemon_through(&client, agent.id, 5_000).await?;

    let mut runtime = Runtime::start_with_client(
        client.clone(),
        RuntimeOptions {
            local_host_id: Some(daemon.host_id()),
            store_path: Some(store_path.clone()),
            ..RuntimeOptions::default()
        },
    );
    wait_for_runtime(&mut runtime, "reconnect agent inventory", |runtime| {
        runtime.model().is_synchronized() && runtime.model().agent(agent.id).is_some()
    })
    .await?;
    runtime.open_chat(agent.id);
    wait_for_runtime(&mut runtime, "initial stored cursor", |runtime| {
        runtime
            .model()
            .chat(agent.id)
            .is_some_and(|chat| chat.state == ChatState::Live && chat.pending_bytes() == 0)
    })
    .await?;
    ensure!(
        store_path.is_file(),
        "runtime did not create its reconnect store"
    );

    let started_at = Utc::now();
    let mut runs = Vec::new();
    let mut identity = 5_001_u64;
    for rows in [10_usize, 100, 1_000] {
        let cursor = runtime
            .model()
            .chat(agent.id)
            .and_then(|chat| chat.head_through())
            .context("runtime has no stored reconnect cursor")?;
        publish_sdk_rows(&daemon, agent.id, identity, rows, 160).await?;
        identity += rows as u64;
        let expected_through = cursor + rows as u64;
        let published = wait_for_daemon_through(&client, agent.id, expected_through).await?;
        ensure!(published.through == expected_through);

        let (received_bytes, payload_bytes) =
            measure_reconnect(&client, agent.id, cursor, rows, expected_through).await?;
        let name = match rows {
            10 => "reconnect delta (10 rows)",
            100 => "reconnect delta (100 rows)",
            1_000 => "reconnect delta (1,000 rows)",
            _ => unreachable!("contract pins reconnect row counts"),
        };
        runs.push(metric_run(
            name,
            Statistic::Median,
            payload_bytes as f64 * 1.2 + 4_096.0,
            Unit::Bytes,
            DELTA_WORKLOAD,
            started_at,
            &[received_bytes as f64],
        ));

        wait_for_runtime(&mut runtime, "persisted reconnect delta", |runtime| {
            runtime.model().chat(agent.id).is_some_and(|chat| {
                chat.state == ChatState::Live
                    && chat.pending_bytes() == 0
                    && chat.head_through() == Some(expected_through)
            })
        })
        .await?;
    }
    drop(runtime);
    net.shutdown().await;
    Ok(runs)
}

async fn measure_reconnect(
    client: &client::Client,
    agent: model::AgentId,
    cursor: u64,
    expected_rows: usize,
    expected_through: u64,
) -> Result<(u64, usize)> {
    let mut stream = client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(agent),
            args: SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
                replay_query: Some(ReplayQuery::After {
                    after: cursor,
                    tail_bound: Some(ui_state::REPLAY_TAIL),
                }),
            }),
        })
        .await?;
    let mut rows = 0_usize;
    let mut payload_bytes = 0_usize;
    let mut opened = false;
    loop {
        match stream.recv().await? {
            SubscribeSessionEvent::Opened {
                replay: Some(facts),
            } => {
                ensure!(!opened, "reconnect subscription opened twice");
                ensure!(
                    matches!(facts.outcome, ReplayOutcome::Continuous),
                    "reconnect subscription reported a gap: {:?}",
                    facts.outcome
                );
                ensure!(facts.selected_from == cursor + 1);
                ensure!(facts.through == expected_through);
                opened = true;
            }
            SubscribeSessionEvent::Opened { replay: None } => {
                bail!("structured reconnect omitted opening facts")
            }
            SubscribeSessionEvent::Output(SessionOutput::ClaudeSdkV1(row)) => {
                ensure!(opened, "reconnect delivered a row before opening facts");
                ensure!(
                    row.seq > cursor,
                    "reconnect redelivered row {} at or before stored cursor {cursor}",
                    row.seq
                );
                rows += 1;
                payload_bytes += row.payload.len();
            }
            SubscribeSessionEvent::Output(_) => bail!("reconnect delivered the wrong protocol"),
            SubscribeSessionEvent::ReplayComplete => break,
            SubscribeSessionEvent::Closed { reason } => {
                bail!("reconnect subscription closed during replay: {reason}")
            }
        }
    }
    ensure!(opened, "reconnect completed without opening facts");
    ensure!(
        rows == expected_rows,
        "reconnect delivered {rows} rows, expected {expected_rows}"
    );
    Ok((stream.received_encoded_bytes(), payload_bytes))
}

async fn wait_for_daemon_through(
    client: &client::Client,
    agent: model::AgentId,
    minimum: u64,
) -> Result<model::ReplayFacts> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut stream = client
                .subscribe_session(SubscribeSessionRequest {
                    agent: AgentIdentifier::Id(agent),
                    args: SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
                        replay_query: Some(ReplayQuery::TailCount {
                            count: 1,
                            tail_bound: Some(1),
                        }),
                    }),
                })
                .await?;
            let facts = match stream.recv().await? {
                SubscribeSessionEvent::Opened {
                    replay: Some(facts),
                } => facts,
                other => bail!("daemon probe opened with {other:?}"),
            };
            if facts.through >= minimum {
                return Ok::<_, anyhow::Error>(facts);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .with_context(|| format!("daemon did not publish through row {minimum}"))?
}

async fn attach_during_flood() -> Result<Vec<MetricRun>> {
    let temp = TempDir::new().context("attach store directory")?;
    let path = temp.path().join("store.sqlite");
    let store = Store::open(&path).await?;
    let net = TestNet::builder().daemon("perf-attach").start().await;
    let daemon = net.daemon("perf-attach");
    daemon.script_sdk_sessions(sdk_script()).await;
    let agent = daemon
        .spawn_scripted_sdk_agent("perf-attach", temp.path())
        .await?;
    seed_live_fleet(&store, &agent).await?;
    seed_chat(&store, agent.id, 5_000, 80).await?;
    let started_at = Utc::now();
    store.close().await;

    publish_sdk_rows(&daemon, agent.id, 1, 5_000, 80).await?;
    let client = daemon.admin_client().await;
    wait_for_daemon_through(&client, agent.id, 5_000).await?;

    let mut runtime = Runtime::start_with_client(
        client,
        RuntimeOptions {
            local_host_id: Some(daemon.host_id()),
            store_path: Some(path),
            ..RuntimeOptions::default()
        },
    );
    wait_for_runtime(&mut runtime, "synchronized live agent", |runtime| {
        runtime.model().is_synchronized() && runtime.model().agent(agent.id).is_some()
    })
    .await?;

    let stop = Arc::new(AtomicBool::new(false));
    let published = Arc::new(AtomicU64::new(0));
    let publisher = {
        let daemon = daemon.clone();
        let stop = Arc::clone(&stop);
        let published = Arc::clone(&published);
        tokio::spawn(async move {
            let mut first = 5_001_u64;
            while !stop.load(Ordering::Acquire) {
                let pulse = Instant::now();
                publish_sdk_rows(&daemon, agent.id, first, 100, 80).await?;
                first += 100;
                published.fetch_add(100, Ordering::Release);
                tokio::time::sleep(Duration::from_millis(50).saturating_sub(pulse.elapsed())).await;
            }
            Ok::<(), anyhow::Error>(())
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while published.load(Ordering::Acquire) < 100 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("scripted attach flood did not begin")?;

    let opened = Instant::now();
    runtime.open_chat(agent.id);
    let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
    let mut first_paint = None;
    let caught_up = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            ensure!(runtime.next().await, "client runtime closed during attach");
            if runtime
                .model()
                .chat(agent.id)
                .is_some_and(|chat| chat.is_painted() && !chat.entries.is_empty())
            {
                paint_runtime_chat(runtime.model(), agent.id, &mut terminal)?;
                first_paint.get_or_insert_with(|| opened.elapsed());
            }
            if runtime.model().chat(agent.id).is_some_and(|chat| {
                chat.state == ChatState::Live
                    && chat.pending_bytes() == 0
                    && chat.head_through().is_some_and(|through| through > 5_000)
                    && published.load(Ordering::Acquire) >= 2_000
            }) {
                return Ok::<Duration, anyhow::Error>(opened.elapsed());
            }
        }
    })
    .await
    .context("attach did not catch up through the runtime")??;
    stop.store(true, Ordering::Release);
    publisher.await.context("join attach flood publisher")??;
    ensure!(
        published.load(Ordering::Acquire) >= 2_000,
        "attach flood stopped before one second at 2,000 rows/s"
    );
    let first_paint = first_paint.context("stored chat never painted")?;
    drop(runtime);
    net.shutdown().await;
    Ok(vec![
        metric_run(
            "attach first painted window",
            Statistic::Median,
            50.0,
            Unit::Milliseconds,
            ATTACH_WORKLOAD,
            started_at,
            &[first_paint.as_secs_f64() * 1_000.0],
        ),
        metric_run(
            "attach caught up",
            Statistic::Worst,
            2_000.0,
            Unit::Milliseconds,
            ATTACH_WORKLOAD,
            started_at,
            &[caught_up.as_secs_f64() * 1_000.0],
        ),
    ])
}

fn paint_runtime_chat(
    model: &Model,
    agent: model::AgentId,
    terminal: &mut Terminal<TestBackend>,
) -> Result<()> {
    let mut chat = ChatView::open(model, agent, 'a', false).context("open stored chat view")?;
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
    terminal.draw(|frame| render(&model, &view, &context, frame))?;
    Ok(())
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

    let mut runtime = Runtime::start(
        Box::new(|| Box::pin(std::future::pending())),
        RuntimeOptions {
            local_host_id: Some(host_id()),
            store_path: Some(path.clone()),
            ..RuntimeOptions::default()
        },
    );
    wait_for_runtime(&mut runtime, "stored scroll fleet", |runtime| {
        !runtime.remembered_fleet_pending() && runtime.model().agent(agent_id(0)).is_some()
    })
    .await?;
    runtime.open_chat(agent_id(0));
    wait_for_runtime(&mut runtime, "stored scroll chat", |runtime| {
        runtime
            .model()
            .chat(agent_id(0))
            .is_some_and(|chat| chat.is_painted() && !chat.entries.is_empty())
    })
    .await?;
    let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
    paint_runtime_chat(runtime.model(), agent_id(0), &mut terminal)?;
    ensure_runtime_window(runtime.model(), agent_id(0))?;

    let before = super::sample_memory(std::process::id())?.bytes as f64;
    let started_at = Utc::now();
    let mut pages = 0_usize;
    loop {
        let chat = runtime
            .model()
            .chat(agent_id(0))
            .context("scroll chat disappeared")?;
        let page_epoch = chat.view_epoch;
        if chat.first_page.is_none() {
            break;
        }
        runtime.page_chat_older(agent_id(0));
        wait_for_runtime(&mut runtime, "older stored page", |runtime| {
            runtime
                .model()
                .chat(agent_id(0))
                .is_some_and(|chat| chat.view_epoch > page_epoch)
        })
        .await?;
        ensure_runtime_window(runtime.model(), agent_id(0))?;
        paint_runtime_chat(runtime.model(), agent_id(0), &mut terminal)?;
        pages += 1;
    }
    let oldest = runtime
        .model()
        .chat(agent_id(0))
        .and_then(|chat| chat.entries.first())
        .map(|entry| entry.position().1.seq())
        .context("oldest scroll window is empty")?;
    ensure!(
        oldest == 1,
        "scroll stopped at row {oldest}, expected row 1"
    );
    ensure!(pages > 0, "scroll workload issued no page operations");

    let oldest_epoch = runtime
        .model()
        .chat(agent_id(0))
        .context("scroll chat disappeared before following tip")?
        .view_epoch;
    runtime.follow_chat_tip(agent_id(0));
    wait_for_runtime(&mut runtime, "newest stored page", |runtime| {
        runtime.model().chat(agent_id(0)).is_some_and(|chat| {
            chat.view_epoch > oldest_epoch
                && chat
                    .entries
                    .last()
                    .is_some_and(|entry| entry.position().1.seq() == 50_000)
        })
    })
    .await?;
    ensure_runtime_window(runtime.model(), agent_id(0))?;
    paint_runtime_chat(runtime.model(), agent_id(0), &mut terminal)?;
    let after = super::sample_memory(std::process::id())?.bytes as f64;
    let memory_ratio = after / before;
    drop(runtime);

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

fn ensure_runtime_window(model: &Model, agent: model::AgentId) -> Result<()> {
    let chat = model.chat(agent).context("runtime has no scroll chat")?;
    ensure!(
        chat.entries.len() <= WINDOW_MAX_ENTRIES,
        "client window retained {} entries, budget is {WINDOW_MAX_ENTRIES}",
        chat.entries.len()
    );
    let bytes = chat.encoded_window_bytes();
    ensure!(
        bytes <= WINDOW_MAX_BYTES,
        "client window retained {bytes} encoded bytes, budget is {WINDOW_MAX_BYTES}"
    );
    Ok(())
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

async fn seed_live_fleet(store: &Store, agent: &Agent) -> Result<()> {
    let generations = generations(store);
    store
        .apply_fleet(
            generations,
            fold::FleetDelta::Host {
                host: HostEntry {
                    id: agent.host_id,
                    name: "perf-daemon".to_owned(),
                    online: true,
                    version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                    capabilities: Some(Default::default()),
                    trust_status: HostTrustStatus::Trusted,
                    last_dial_error: None,
                    platform: None,
                },
                revision: 1,
            },
        )
        .await?;
    store
        .apply_fleet(
            generations,
            fold::FleetDelta::AgentUp {
                agent: agent.clone(),
                revision: 2,
            },
        )
        .await?;
    Ok(())
}

async fn publish_sdk_rows(
    daemon: &crate::Daemon,
    agent: model::AgentId,
    first_identity: u64,
    count: usize,
    text: usize,
) -> Result<()> {
    for offset in (0..count).step_by(100) {
        let batch = (offset..(offset + 100).min(count))
            .map(|offset| sdk_live_prompt(agent, first_identity + offset as u64, text))
            .collect();
        daemon.emit_scripted_sdk_rows(agent, batch).await?;
    }
    Ok(())
}

fn sdk_live_prompt(agent: model::AgentId, identity: u64, text: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "uuid": Uuid::from_u128(0xA600_0000_0000_0000_0000_0000_0000_0000 + identity as u128),
        "session_id": agent,
        "parent_tool_use_id": null,
        "message": {
            "role": "user",
            "content": format!("live row {identity}: {}", "x".repeat(text)),
        },
    })
}

fn sdk_script() -> crate::sdk::Script {
    serde_json::from_value(serde_json::json!({
        "initialization": {
            "commands": [],
            "agents": [],
            "models": [],
            "account": {},
            "output_style": "default",
            "available_output_styles": []
        },
        "reply": "unused performance reply"
    }))
    .expect("static SDK performance script")
}

async fn wait_for_runtime(
    runtime: &mut Runtime,
    what: &str,
    ready: impl Fn(&Runtime) -> bool,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready(runtime) {
            ensure!(runtime.next().await, "runtime closed waiting for {what}");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| format!("timed out waiting for {what}"))?
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
    let observation_count = values.len();
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
        observation_count,
        started_at,
        ended_at: Utc::now(),
    }
}
