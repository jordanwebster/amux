use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fold::{AgentFold, Baseline, Input, TIP_MAX_BYTES};
use model::StructuredProtocol;
use prost::Message;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use tui::chrome::{Chrome, ChromeConfig, InputEvent, KeyRecord, TraceEvent};
use tui::fixtures::{long_feed, long_feed_batch};
use tui::{FrameContext, Theme, render};
use ui_state::update;

use super::{Metric, MetricRun, Sample, Statistic, Unit, Workload};

const VIEWPORT: (u16, u16) = (120, 40);
const SEED: u64 = 0xA6_2026_0917;

const FRAME_WORKLOAD: Workload = Workload {
    description: "three provider chats with 1,000 retained entries",
    seed: SEED,
    identity_growth: "fixed 1,000-entry windows",
    warm_up: "one complete frame per provider",
};

const FLOOD_WORKLOAD: Workload = Workload {
    description: "production reducer and chrome, 2,000 new rows/s for 30 seconds",
    seed: SEED,
    identity_growth: "60,000 fresh Codex item ids",
    warm_up: "one input and flushed test-backend frame",
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

const DELTA_WORKLOAD: Workload = Workload {
    description: "protobuf session output after an exact cursor, no gap",
    seed: SEED,
    identity_growth: "fresh row payload per sequence",
    warm_up: "none",
};

pub fn run_fast() -> Result<Vec<MetricRun>> {
    let mut runs = Vec::new();
    runs.push(steady_state_frame()?);
    runs.push(frame_under_flood()?);
    runs.push(tip_bound());
    runs.extend(summarizer_cost());
    runs.extend([10, 100, 1_000].map(reconnect_delta));
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

fn frame_under_flood() -> Result<MetricRun> {
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
    let mut samples = Vec::with_capacity(30);
    for second in 0..30 {
        let interval = Instant::now();
        let message = long_feed_batch(StructuredProtocol::Codex, second * 2_000 + 1, 2_000);
        update(&mut model, message.clone());
        chrome.step(&model, &TraceEvent::Msg(message));
        chrome.step(&model, &TraceEvent::Drained);
        let now = Utc::now();
        chrome.step(&model, &TraceEvent::InputArrival { at: now });
        chrome.step(
            &model,
            &TraceEvent::Input {
                event: InputEvent::Key(KeyRecord::from_event(KeyEvent::new(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                ))),
                viewport: VIEWPORT,
                now,
            },
        );
        let began = Instant::now();
        paint_chrome(&mut chrome, &model, &mut terminal)?;
        samples.push(sample(
            "frame under flood",
            began.elapsed().as_secs_f64() * 1_000.0,
            Unit::Milliseconds,
        ));
        std::thread::sleep(Duration::from_secs(1).saturating_sub(interval.elapsed()));
    }
    Ok(run(
        "frame under flood",
        Statistic::P99,
        16.667,
        Unit::Milliseconds,
        FLOOD_WORKLOAD,
        started_at,
        samples,
    ))
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

fn reconnect_delta(rows: usize) -> MetricRun {
    let started_at = Utc::now();
    let mut wire_bytes = 0_usize;
    let mut payload_bytes = 0_usize;
    for sequence in 1..=rows {
        let payload = format!(
            r#"{{"type":"user","uuid":"00000000-0000-4000-8000-{sequence:012}","message":{{"content":"Reconnect row {sequence}: {}"}}}}"#,
            "x".repeat(160)
        )
        .into_bytes();
        payload_bytes += payload.len();
        let output = wire::SessionOutput {
            output: Some(wire::session_output::Output::ClaudeSdkV1(
                wire::StructuredRow {
                    seq: sequence as u64,
                    published_at_unix_ms: 1_700_000_000_000 + sequence as i64,
                    activity_at_unix_ms: None,
                    historical: false,
                    payload,
                },
            )),
        };
        wire_bytes += output.encoded_len() + 5;
    }
    let name = match rows {
        10 => "reconnect delta (10 rows)",
        100 => "reconnect delta (100 rows)",
        1_000 => "reconnect delta (1,000 rows)",
        _ => unreachable!("contract pins reconnect row counts"),
    };
    run(
        name,
        Statistic::Median,
        payload_bytes as f64 * 1.2 + 4_096.0,
        Unit::Bytes,
        DELTA_WORKLOAD,
        started_at,
        vec![sample(name, wire_bytes as f64, Unit::Bytes)],
    )
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
    MetricRun {
        metric: Metric {
            name,
            statistic,
            budget,
            unit,
            workload,
        },
        samples,
        started_at,
        ended_at: Utc::now(),
    }
}
