use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fold::{AgentFold, Baseline, Input, TIP_MAX_BYTES};
use model::StructuredProtocol;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use tui::chrome::{Chrome, ChromeConfig, InputEvent, KeyRecord, TraceEvent};
use tui::fixtures::{long_feed, long_feed_batch};
use tui::{FrameContext, Theme, render};
use ui_runtime::MAX_STREAM_BATCH;
use ui_state::update;

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
    description: "200 idle and 20 active summary-only folds",
    seed: SEED,
    identity_growth: "fresh active row ids",
    warm_up: "one row per active fold",
};

pub fn run_fast() -> Result<Vec<MetricRun>> {
    let mut runs = Vec::new();
    runs.push(steady_state_frame()?);
    runs.extend(frame_under_flood()?);
    runs.push(tip_bound());
    runs.extend(summarizer_cost());
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
            let message = long_feed_batch(StructuredProtocol::Codex, next_row, rows);
            next_row += rows;
            let key_arrived = (pulse_index + 1 == batches.len()).then(Instant::now);

            // This is the release loop's turn order: the runtime folds its
            // queued batch, chrome reconciles after the drain, then a ready
            // input is stepped before the dirty frame is rendered and
            // Terminal::draw applies changed cells and flushes the backend.
            update(&mut model, message);
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

fn summarizer_cost() -> [MetricRun; 2] {
    let started_at = Utc::now();
    let mut active = (0..20)
        .map(|_| {
            let mut fold = AgentFold::for_protocol(StructuredProtocol::ClaudeSdk);
            fold.begin(1, Baseline::Start);
            fold
        })
        .collect::<Vec<_>>();
    let warm = br#"{"type":"user","uuid":"00000000-0000-4000-8000-000000000000","message":{"content":"warm"}}"#;
    for fold in &mut active {
        fold.apply_summary(Input::Row {
            seq: 1,
            published_at: Utc::now(),
            activity_at: None,
            historical: false,
            payload: warm,
        });
    }
    let began = cpu_time();
    let rows = 20 * 1_000;
    for sequence in 2..=1_001_u64 {
        let payload = format!(
            r#"{{"type":"user","uuid":"00000000-0000-4000-8000-{sequence:012}","message":{{"content":"active {sequence}"}}}}"#
        );
        for fold in &mut active {
            fold.apply_summary(Input::Row {
                seq: sequence,
                published_at: Utc::now(),
                activity_at: None,
                historical: false,
                payload: payload.as_bytes(),
            });
        }
    }
    let per_row_us = (cpu_time() - began).as_secs_f64() * 1_000_000.0 / rows as f64;

    let mut idle = (0..200)
        .map(|_| {
            let mut fold = AgentFold::for_protocol(StructuredProtocol::ClaudeSdk);
            fold.begin(1, Baseline::Start);
            fold
        })
        .collect::<Vec<_>>();
    let began = cpu_time();
    for _ in 0..10 {
        for fold in &mut idle {
            fold.apply_summary(Input::Tick { now: Utc::now() });
        }
    }
    let idle_percent = (cpu_time() - began).as_secs_f64() / 10.0 * 100.0;
    [
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
    ]
}

fn protocols() -> [StructuredProtocol; 3] {
    [
        StructuredProtocol::ClaudePtyTranscript,
        StructuredProtocol::ClaudeSdk,
        StructuredProtocol::Codex,
    ]
}

#[cfg(unix)]
fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes one initialized rusage on success.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage(RUSAGE_SELF) failed");
    // SAFETY: the successful call initialized the complete value.
    let usage = unsafe { usage.assume_init() };
    let timeval = |value: libc::timeval| {
        Duration::from_secs(value.tv_sec as u64)
            + Duration::from_micros(value.tv_usec.try_into().unwrap())
    };
    timeval(usage.ru_utime) + timeval(usage.ru_stime)
}

#[cfg(not(unix))]
fn cpu_time() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
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
    fn flood_second_uses_runtime_sized_batches() {
        let batches = flood_batch_sizes();
        assert_eq!(batches, vec![256, 256, 256, 256, 256, 256, 256, 208]);
        assert_eq!(batches.iter().sum::<usize>(), FLOOD_ROWS_PER_SECOND);
        assert!(batches.iter().all(|rows| *rows <= MAX_STREAM_BATCH));
    }
}
