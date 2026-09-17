use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fold::{AgentFold, Baseline, ExpectedHead, Input, TIP_MAX_BYTES};
use model::StructuredProtocol;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use tui::chrome::{Chrome, ChromeConfig, InputEvent, KeyRecord, TraceEvent};
use tui::fixtures::{canonical_commit_msg, long_feed, long_feed_batch};
use tui::{FrameContext, Theme, render};
use ui_runtime::MAX_STREAM_BATCH;
use ui_state::{AgentId, ChatStreamMsg, Msg, StreamMsg, update};

use super::{Metric, MetricRun, Sample, Statistic, Unit, Workload};

const VIEWPORT: (u16, u16) = (120, 40);
const SEED: u64 = 0xA6_2026_0917;
const FLOOD_ROWS_PER_SECOND: usize = 2_000;
const FLOOD_SECONDS: usize = 30;

const FRAME_WORKLOAD: Workload = Workload {
    description: "three provider chats with 1,000 stored entries",
    seed: SEED,
    identity_growth: "fixed 1,000-entry windows",
    warm_up: "one complete frame per provider",
};

const FLOOD_WORKLOAD: Workload = Workload {
    description: "release-loop reducer, chrome and terminal flush; 2,000 rows/s in production-sized batches for 30 seconds",
    seed: SEED,
    identity_growth: "60,000 fresh Codex item ids",
    warm_up: "one flushed test-backend frame",
};

const TIP_WORKLOAD: Workload = Workload {
    description: "all provider corpora, sampled after every input",
    seed: SEED,
    identity_growth: "provider-native corpus ids",
    warm_up: "none",
};

const SUMMARIZER_WORKLOAD: Workload = Workload {
    description: "shipping ring readers and summarizer tasks: 200 idle for at least 10 s, then 20 active consuming 1,000 rows each",
    seed: SEED,
    identity_growth: "fresh active row ids",
    warm_up: "initial publications drained; one ring row per active summarizer",
};

pub fn run_fast() -> Result<Vec<MetricRun>> {
    let mut runs = Vec::new();
    runs.push(steady_state_frame()?);
    runs.extend(frame_under_flood()?);
    runs.push(tip_bound());
    runs.extend(summarizer_cost()?);
    runs.extend(super::store_workloads::run_store()?);
    Ok(runs)
}

fn steady_state_frame() -> Result<MetricRun> {
    let started_at = Utc::now();
    let mut harnesses = protocols()
        .map(|protocol| {
            let fixture = long_feed(protocol, 1_000);
            let terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
            Ok::<_, anyhow::Error>((fixture, terminal))
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    for (fixture, terminal) in &mut harnesses {
        draw_fixture(fixture, terminal)?;
    }
    let mut samples = Vec::with_capacity(50);
    for _ in 0..50 {
        let mut worst = 0.0_f64;
        for (fixture, terminal) in &mut harnesses {
            let began = Instant::now();
            draw_fixture(fixture, terminal)?;
            worst = worst.max(began.elapsed().as_secs_f64() * 1_000.0);
        }
        samples.push(sample("steady-state frame", worst, Unit::Milliseconds));
    }
    Ok(run(
        "steady-state frame",
        Statistic::Median,
        8.0,
        Unit::Milliseconds,
        FRAME_WORKLOAD,
        started_at,
        samples,
    ))
}

fn draw_fixture(
    fixture: &mut tui::fixtures::Fixture,
    terminal: &mut Terminal<TestBackend>,
) -> Result<()> {
    let context = FrameContext {
        viewport: VIEWPORT,
        theme: Theme::default(),
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .context("draw performance fixture")?;
    Ok(())
}

fn frame_under_flood() -> Result<Vec<MetricRun>> {
    let started_at = Utc::now();
    let fixture = long_feed(StructuredProtocol::Codex, 1);
    let mut model = fixture.model;
    let mut chrome = Chrome::new(
        fixture.view,
        ChromeConfig {
            theme: Theme::default(),
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))?;
    paint_chrome(&mut chrome, &model, &mut terminal)?;
    let batches = flood_batch_sizes();
    let pulse = Duration::from_secs(1) / u32::try_from(batches.len())?;
    let mut frame_samples = Vec::with_capacity(FLOOD_SECONDS * batches.len());
    let mut key_samples = Vec::with_capacity(FLOOD_SECONDS);
    let mut next_row = 1_usize;
    let mut typed = String::new();

    for second in 0..FLOOD_SECONDS {
        let interval = Instant::now();
        for (pulse_index, &rows) in batches.iter().enumerate() {
            let due = interval + pulse * u32::try_from(pulse_index)?;
            std::thread::sleep(due.saturating_duration_since(Instant::now()));

            // Build the message before the input arrives: the runtime has
            // already decoded messages queued ahead of a terminal event.
            let first_row = next_row;
            let last_row = first_row + rows - 1;
            let message = long_feed_batch(StructuredProtocol::Codex, first_row, rows);
            next_row += rows;
            let key_arrived = (pulse_index + 1 == batches.len()).then(Instant::now);

            // This is the release loop's turn order: the runtime folds its
            // queued batch, chrome reconciles after the drain, then a ready
            // input is stepped before the dirty frame is rendered and
            // Terminal::draw applies changed cells and flushes the backend.
            apply_store_backed_batch(&mut model, message)?;
            chrome.step(&model, &TraceEvent::Drained);
            if key_arrived.is_some() {
                let key = char::from(b'a' + u8::try_from(second % 26)?);
                typed.push(key);
                let now = Utc::now();
                chrome.step(&model, &TraceEvent::InputArrival { at: now });
                let effects = chrome.step(
                    &model,
                    &TraceEvent::Input {
                        event: InputEvent::Key(KeyRecord::from_event(KeyEvent::new(
                            KeyCode::Char(key),
                            KeyModifiers::NONE,
                        ))),
                        viewport: VIEWPORT,
                        now,
                    },
                );
                anyhow::ensure!(effects.is_empty(), "composer key emitted shell effects");
                let composer = chrome
                    .view
                    .chat
                    .as_mut()
                    .context("flood fixture has no open chat")?
                    .composer_mut()
                    .text();
                anyhow::ensure!(composer == typed, "keypress did not update the composer");
            }

            let frame_started = Instant::now();
            paint_chrome(&mut chrome, &model, &mut terminal)?;
            anyhow::ensure!(
                rendered_text(&terminal).contains(&codex_long_feed_marker(last_row)),
                "flood pulse left the painted transcript behind row {last_row}"
            );
            frame_samples.push(sample(
                "frame under flood",
                frame_started.elapsed().as_secs_f64() * 1_000.0,
                Unit::Milliseconds,
            ));
            if let Some(arrived) = key_arrived {
                anyhow::ensure!(
                    rendered_text(&terminal).contains(&typed),
                    "flushed frame did not reflect the keypress"
                );
                key_samples.push(sample(
                    "keypress to draw",
                    arrived.elapsed().as_secs_f64() * 1_000.0,
                    Unit::Milliseconds,
                ));
            }
        }
    }
    anyhow::ensure!(
        next_row == FLOOD_ROWS_PER_SECOND * FLOOD_SECONDS + 1,
        "flood emitted the wrong row count"
    );
    Ok(vec![
        run(
            "frame under flood",
            Statistic::P99,
            16.667,
            Unit::Milliseconds,
            FLOOD_WORKLOAD,
            started_at,
            frame_samples,
        ),
        run(
            "keypress to draw",
            Statistic::P99,
            50.0,
            Unit::Milliseconds,
            FLOOD_WORKLOAD,
            started_at,
            key_samples,
        ),
    ])
}

fn apply_store_backed_batch(model: &mut ui_state::Model, message: Msg) -> Result<()> {
    let Msg::Stream {
        agent,
        event: StreamMsg::Batch { at, entries },
    } = message
    else {
        anyhow::bail!("long-feed pulse was not a structured stream batch");
    };
    let (attempt, expected, content_revision, before_tip) = {
        let chat = model
            .chat(agent)
            .context("flood fixture has no stored chat")?;
        (
            chat.stream_attempt,
            next_store_head(chat.expected),
            chat.content_revision.saturating_add(1),
            stored_tip(model, agent)?,
        )
    };
    let effects = update(
        model,
        Msg::ChatStream {
            agent,
            attempt,
            event: ChatStreamMsg::Batch { at, entries },
        },
    );
    let committed = canonical_commit_msg(agent, effects, expected, content_revision);
    update(model, committed);
    let after_tip = stored_tip(model, agent)?;
    anyhow::ensure!(
        after_tip > before_tip,
        "flood pulse did not advance the stored transcript tip"
    );
    Ok(())
}

fn next_store_head(expected: ExpectedHead) -> ExpectedHead {
    match expected {
        ExpectedHead::Absent { fence } => ExpectedHead::Present {
            fence: fence.saturating_add(1),
            version: 1,
        },
        ExpectedHead::Present { fence, version } => ExpectedHead::Present {
            fence: fence.saturating_add(1),
            version: version.saturating_add(1),
        },
    }
}

fn stored_tip(model: &ui_state::Model, agent: AgentId) -> Result<u64> {
    model
        .chat(agent)
        .and_then(|chat| chat.entries.last())
        .map(|entry| entry.position().1.seq())
        .context("flood fixture stored chat has no transcript tip")
}

fn codex_long_feed_marker(index: usize) -> String {
    match index % 3 {
        0 => format!("Investigate retry case {index}."),
        1 => format!("Retry case {index} is covered by the focused test."),
        _ => format!("cargo test retry_case_{index}"),
    }
}

fn flood_batch_sizes() -> Vec<usize> {
    let mut remaining = FLOOD_ROWS_PER_SECOND;
    let mut batches = Vec::new();
    while remaining > 0 {
        let rows = remaining.min(MAX_STREAM_BATCH);
        batches.push(rows);
        remaining -= rows;
    }
    batches
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn paint_chrome(
    chrome: &mut Chrome,
    model: &ui_state::Model,
    terminal: &mut Terminal<TestBackend>,
) -> Result<()> {
    chrome.step(
        model,
        &TraceEvent::Draw {
            viewport: VIEWPORT,
            now: Utc::now(),
        },
    );
    let lines = chrome.take_frame().context("chrome owed a frame")?;
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .context("draw flood frame")?;
    Ok(())
}

fn tip_bound() -> MetricRun {
    let started_at = Utc::now();
    let corpora = [
        (
            StructuredProtocol::ClaudePtyTranscript,
            include_str!("../../../claude-specs/fixtures/claude-pty/tools.rows.jsonl"),
        ),
        (
            StructuredProtocol::ClaudeSdk,
            include_str!("../../../ui-state/tests/spec/fixtures/claude_sdk/converse.rows.jsonl"),
        ),
        (
            StructuredProtocol::Codex,
            include_str!("../../../codex-specs/fixtures/codex/turn_round_trip.rows.jsonl"),
        ),
    ];
    let mut samples = Vec::new();
    for (protocol, corpus) in corpora {
        let mut fold = AgentFold::for_protocol(protocol);
        fold.begin(1, Baseline::Start);
        for (offset, payload) in corpus.lines().filter(|line| !line.is_empty()).enumerate() {
            fold.apply_summary(Input::Row {
                seq: offset as u64 + 1,
                published_at: Utc::now(),
                activity_at: None,
                historical: false,
                payload: payload.as_bytes(),
            });
            samples.push(sample("tip bound", fold.tip_bytes() as f64, Unit::Bytes));
        }
    }
    run(
        "tip bound",
        Statistic::Peak,
        TIP_MAX_BYTES as f64,
        Unit::Bytes,
        TIP_WORKLOAD,
        started_at,
        samples,
    )
}

fn summarizer_cost() -> Result<[MetricRun; 2]> {
    const IDLE_WALL_TIME: Duration = Duration::from_secs(10);
    const IDLE_SUMMARIZERS: usize = 200;
    const ACTIVE_SUMMARIZERS: usize = 20;
    const ACTIVE_ROWS_PER_SUMMARIZER: u64 = 1_000;

    let started_at = Utc::now();
    let idle_runtime = summarizer_runtime()?;
    let mut idle = agent_runtime::test_support::DaemonMemoryHarness::new();
    idle_runtime.block_on(idle.add_idle_summarizers(IDLE_SUMMARIZERS));
    let idle_wall_started = Instant::now();
    let idle_cpu_started = cpu_time()?;
    block_on_sleep(&idle_runtime, IDLE_WALL_TIME);
    let idle_cpu = cpu_time()?.saturating_sub(idle_cpu_started);
    let idle_wall = idle_wall_started.elapsed();
    anyhow::ensure!(
        idle_wall >= IDLE_WALL_TIME,
        "summarizer idle sample ended before its 10 second wall-clock floor"
    );
    let idle_percent = idle_cpu.as_secs_f64() / idle_wall.as_secs_f64() * 100.0;
    drop(idle);
    drop(idle_runtime);

    let active_runtime = summarizer_runtime()?;
    let mut active = agent_runtime::test_support::DaemonMemoryHarness::new();
    active_runtime.block_on(active.add_active(ACTIVE_SUMMARIZERS));
    active_runtime.block_on(active.consume_active_rows(1));
    let active_cpu_started = cpu_time()?;
    let rows = active_runtime.block_on(active.consume_active_rows(ACTIVE_ROWS_PER_SUMMARIZER));
    let active_cpu = cpu_time()?.saturating_sub(active_cpu_started);
    let per_row_us = active_cpu.as_secs_f64() * 1_000_000.0 / rows as f64;

    Ok([
        run(
            "summarizer CPU per row",
            Statistic::Median,
            50.0,
            Unit::Microseconds,
            SUMMARIZER_WORKLOAD,
            started_at,
            vec![sample(
                "summarizer CPU per row",
                per_row_us,
                Unit::Microseconds,
            )],
        ),
        run(
            "summarizer idle core",
            Statistic::Median,
            1.0,
            Unit::Percent,
            SUMMARIZER_WORKLOAD,
            started_at,
            vec![sample("summarizer idle core", idle_percent, Unit::Percent)],
        ),
    ])
}

fn summarizer_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("build summarizer performance runtime")
}

fn block_on_sleep(runtime: &tokio::runtime::Runtime, duration: Duration) {
    runtime.block_on(async { tokio::time::sleep(duration).await });
}

fn protocols() -> [StructuredProtocol; 3] {
    [
        StructuredProtocol::ClaudePtyTranscript,
        StructuredProtocol::ClaudeSdk,
        StructuredProtocol::Codex,
    ]
}

#[cfg(unix)]
fn cpu_time() -> Result<Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes one initialized rusage on success.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("getrusage(RUSAGE_SELF)");
    }
    // SAFETY: the successful call initialized the complete value.
    let usage = unsafe { usage.assume_init() };
    let timeval = |value: libc::timeval| {
        Duration::from_secs(value.tv_sec as u64)
            + Duration::from_micros(value.tv_usec.try_into().unwrap())
    };
    Ok(timeval(usage.ru_utime) + timeval(usage.ru_stime))
}

#[cfg(not(unix))]
fn cpu_time() -> Result<Duration> {
    anyhow::bail!("summarizer process CPU measurement is supported only on Unix")
}

fn sample(metric: &'static str, value: f64, unit: Unit) -> Sample {
    Sample {
        metric,
        value,
        unit,
    }
}

fn run(
    name: &'static str,
    statistic: Statistic,
    budget: f64,
    unit: Unit,
    workload: Workload,
    started_at: chrono::DateTime<Utc>,
    samples: Vec<Sample>,
) -> MetricRun {
    let observation_count = samples.len();
    MetricRun {
        metric: Metric {
            name,
            statistic,
            budget,
            unit,
            workload,
        },
        samples,
        observation_count,
        started_at,
        ended_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizer_runtime_enters_timer_context() {
        let runtime = summarizer_runtime().unwrap();
        block_on_sleep(&runtime, Duration::from_millis(1));
    }

    #[test]
    fn flood_second_uses_runtime_sized_batches() {
        let batches = flood_batch_sizes();
        assert_eq!(batches, vec![256, 256, 256, 256, 256, 256, 256, 208]);
        assert_eq!(batches.iter().sum::<usize>(), FLOOD_ROWS_PER_SECOND);
        assert!(batches.iter().all(|rows| *rows <= MAX_STREAM_BATCH));
    }

    #[test]
    fn flood_pulse_advances_and_paints_the_stored_chat() {
        let fixture = long_feed(StructuredProtocol::Codex, 1);
        let mut model = fixture.model;
        let agent = AgentId::from_u128(8);
        let before_tip = stored_tip(&model, agent).expect("initial stored tip");
        apply_store_backed_batch(&mut model, long_feed_batch(StructuredProtocol::Codex, 1, 3))
            .expect("apply flood pulse");
        assert!(
            stored_tip(&model, agent).expect("advanced stored tip") > before_tip,
            "the pulse reaches the canonical chat window"
        );

        let mut chrome = Chrome::new(
            fixture.view,
            ChromeConfig {
                theme: Theme::default(),
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1)).unwrap();
        chrome.step(&model, &TraceEvent::Drained);
        paint_chrome(&mut chrome, &model, &mut terminal).expect("paint pulse");
        assert!(
            rendered_text(&terminal).contains(&codex_long_feed_marker(3)),
            "the frame paints the pulse's newest transcript row"
        );
    }
}
