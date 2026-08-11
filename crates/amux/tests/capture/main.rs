//! The real-Claude capture harness — the seed of the CHAT.md §H suite.
//!
//! Opt-in: does nothing (exits 0 with a note) unless scenario names are
//! passed. Run one scenario at a time, always under `timeout`:
//!
//! ```text
//! cargo build -p amux-cli
//! AMUX_CAPTURE_OUT=target/capture timeout 600 \
//!     cargo test -p amux --test capture -- pong
//! ```
//!
//! Scenarios: pong, tools, permission, question_single, question_multi,
//! interrupt, plan_approve, plan_reject, compact (or `all`).
//!
//! Environment:
//! - `AMUX_CAPTURE_OUT`   output dir (default `target/capture/<unix-secs>`)
//! - `AMUX_CAPTURE_MODEL` claude model (default `haiku`; owner directive —
//!   rerun a tool-unreliable scenario with `sonnet` and the meta records it)
//! - `AMUX_CAPTURE_POISON=0` disables the poisoned-daemon env (default ON:
//!   every run doubles as a live check of the child-session-marker scrub)
//!
//! Each scenario writes `<name>.rows.jsonl` (raw stream capture),
//! `<name>.raw.log` (PTY bytes, debugging), `<name>.redacted.jsonl`
//! (fixture candidate) and `<name>.meta.json` (provenance + keystroke log).

mod harness;
mod redact;

use std::time::Duration;

use amux::claude_io::ClaudePtyTranscriptV1Action as Act;
use anyhow::{Result, bail};
use harness::{CaptureSession, DaemonEnv, Scratch, ScratchDaemon, claude_version};

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const TURN_TIMEOUT: Duration = Duration::from_secs(240);
const ASK_TIMEOUT: Duration = Duration::from_secs(180);

/// Pause between a menu appearing (its hook row arriving) and answering it:
/// the hook fires as the dialog renders, and keystrokes that race the render
/// are dropped by claude's TUI.
const MENU_SETTLE: Duration = Duration::from_millis(1500);

fn main() -> Result<()> {
    let scenario_names: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    if scenario_names.is_empty() {
        println!(
            "capture: no scenarios named; skipping (opt-in real-Claude harness — \
             pass scenario names or `all`, see tests/capture/main.rs)"
        );
        return Ok(());
    }
    let all = [
        "pong",
        "tools",
        "permission",
        "question_single",
        "question_multi",
        "interrupt",
        "plan_approve",
        "plan_reject",
        "compact",
    ];
    let selected: Vec<&str> = if scenario_names.iter().any(|name| name == "all") {
        all.to_vec()
    } else {
        let mut selected = Vec::new();
        for name in &scenario_names {
            let Some(known) = all.iter().find(|known| **known == name.as_str()) else {
                bail!("unknown scenario '{name}'; known: {all:?}");
            };
            selected.push(*known);
        }
        selected
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(selected))
}

async fn run(scenarios: Vec<&str>) -> Result<()> {
    let out = match std::env::var("AMUX_CAPTURE_OUT") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => std::path::PathBuf::from("target/capture").join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        )),
    };
    let model = std::env::var("AMUX_CAPTURE_MODEL").unwrap_or_else(|_| "haiku".to_string());
    let poisoned = std::env::var("AMUX_CAPTURE_POISON").as_deref() != Ok("0");

    let scratch = Scratch::create(out.clone())?;
    let daemon = harness::start_daemon(&scratch, &DaemonEnv { poisoned }).await?;
    let version = claude_version();
    println!("capture: daemon up (poisoned={poisoned}), claude: {version}, model: {model}");
    println!("capture: output dir {}", out.display());

    let mut failures = Vec::new();
    for scenario in scenarios {
        println!("=== scenario {scenario} ===");
        let started = std::time::Instant::now();
        match run_scenario(&daemon, &scratch, scenario, &model).await {
            Ok(notes) => {
                finalize(&scratch, scenario, &model, &version, poisoned, notes, None)?;
                println!(
                    "=== scenario {scenario} OK ({:.0}s) ===",
                    started.elapsed().as_secs_f64()
                );
            }
            Err(error) => {
                let message = format!("{error:#}");
                println!("=== scenario {scenario} FAILED: {message} ===");
                finalize(
                    &scratch,
                    scenario,
                    &model,
                    &version,
                    poisoned,
                    serde_json::json!({}),
                    Some(message.clone()),
                )?;
                failures.push((scenario, message));
            }
        }
    }

    let _ = daemon.client.shutdown().await;
    if failures.is_empty() {
        println!("capture: all scenarios OK");
        Ok(())
    } else {
        bail!(
            "capture: {} scenario(s) failed: {failures:?}",
            failures.len()
        )
    }
}

/// Write the redacted fixture candidate and the provenance meta sidecar.
fn finalize(
    scratch: &Scratch,
    scenario: &str,
    model: &str,
    version: &str,
    poisoned: bool,
    notes: serde_json::Value,
    failure: Option<String>,
) -> Result<()> {
    let rows_path = scratch.out.join(format!("{scenario}.rows.jsonl"));
    if rows_path.exists() {
        let raw = std::fs::read_to_string(&rows_path)?;
        let redacted = redact::redact(&raw, &scratch.root)?;
        std::fs::write(
            scratch.out.join(format!("{scenario}.redacted.jsonl")),
            redacted,
        )?;
    }
    let meta = serde_json::json!({
        "scenario": scenario,
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "claude_version": version,
        "model": model,
        "harness": format!("cargo test -p amux --test capture -- {scenario}"),
        "poisoned_daemon_env": poisoned,
        "notes": notes,
        "failure": failure,
    });
    std::fs::write(
        scratch.out.join(format!("{scenario}.meta.json")),
        serde_json::to_string_pretty(&meta)?,
    )?;
    Ok(())
}

async fn run_scenario(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    scenario: &str,
    model: &str,
) -> Result<serde_json::Value> {
    match scenario {
        "pong" => pong(daemon, scratch, model).await,
        "tools" => tools(daemon, scratch, model).await,
        "permission" => permission(daemon, scratch, model).await,
        "question_single" => question_single(daemon, scratch, model).await,
        "question_multi" => question_multi(daemon, scratch, model).await,
        "interrupt" => interrupt(daemon, scratch, model).await,
        "plan_approve" => plan(daemon, scratch, model, true).await,
        "plan_reject" => plan(daemon, scratch, model, false).await,
        "compact" => compact(daemon, scratch, model).await,
        _ => unreachable!("scenario names validated in main"),
    }
}

/// Open a scenario session and drive its first prompt.
///
/// Sequencing (see `prepare_for_first_prompt`): claude creates the transcript
/// file lazily on the first turn, so the first prompt must be sent *before*
/// `amux.transcript_ready` can appear. This returns the index just past the
/// `transcript_ready` boundary — which is the live proof the spawned claude
/// actually persisted its transcript (the Phase 0 bug: a poisoned daemon
/// would suppress persistence and this wait would time out).
async fn open(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    scenario: &str,
    extra_args: &[&str],
    model: &str,
    first_prompt: &str,
) -> Result<(CaptureSession, usize)> {
    let dir = scratch.project_dir(scenario)?;
    let mut session =
        CaptureSession::open(daemon, scratch, scenario, dir, extra_args, model).await?;
    session.prepare_for_first_prompt(READY_TIMEOUT).await?;
    session.send_prompt(first_prompt).await?;
    let index = session.wait_for_transcript_ready(READY_TIMEOUT).await?;
    Ok((session, index))
}

/// The `toolUseResult.answers` object of the most recent AskUserQuestion
/// result row in the capture, if any.
fn latest_answers(rows: &[harness::Row]) -> Option<serde_json::Map<String, serde_json::Value>> {
    rows.iter()
        .rev()
        .find_map(|row| row.json.pointer("/toolUseResult/answers"))
        .and_then(|v| v.as_object().cloned())
}

/// H.1 — prompt round trip.
async fn pong(daemon: &ScratchDaemon, scratch: &Scratch, model: &str) -> Result<serde_json::Value> {
    let (session, index) = open(
        daemon,
        scratch,
        "pong",
        &[],
        model,
        "Reply with exactly PONG and nothing else.",
    )
    .await?;
    session.wait_for_turn_end(index, TURN_TIMEOUT).await?;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys }))
}

/// Tool use: an Edit and a Bash command, friction-free via
/// `--dangerously-skip-permissions` (permission flows are scenario 3).
async fn tools(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (session, index) = open(
        daemon,
        scratch,
        "tools",
        &["--dangerously-skip-permissions"],
        model,
        "In config.txt use the Edit tool to change VALUE=1 to VALUE=2. \
         Then use the Bash tool to run exactly: cat config.txt. Then stop.",
    )
    .await?;
    session.wait_for_turn_end(index, TURN_TIMEOUT).await?;
    // World assertion: the edit really landed.
    let content = std::fs::read_to_string(scratch.projects.join("tools/config.txt"))?;
    // Structural assertion: the capture must actually contain both an Edit and
    // a Bash tool_use, each with a paired tool_result — a run that skipped
    // either tool would otherwise pass on the file content alone.
    let rows = session.snapshot().await;
    let has_edit = rows.iter().any(|r| r.is_tool_use("Edit"));
    let has_bash = rows.iter().any(|r| r.is_tool_use("Bash"));
    let result_count = rows.iter().filter(|r| r.is_tool_result()).count();
    let keys = session.close().await?;
    if !content.contains("VALUE=2") {
        bail!("world assertion failed: config.txt does not contain VALUE=2 (got {content:?})");
    }
    if !has_edit {
        bail!("capture assertion failed: no Edit tool_use row recorded");
    }
    if !has_bash {
        bail!("capture assertion failed: no Bash tool_use row recorded");
    }
    if result_count < 2 {
        bail!("capture assertion failed: expected ≥2 tool_result rows, saw {result_count}");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "config.txt": content },
        "assertions": { "edit_tool_use": has_edit, "bash_tool_use": has_bash, "tool_results": result_count },
    }))
}

/// Permission allow AND deny in default permission mode.
async fn permission(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    // The first prompt is the allow-leg trigger.
    let (mut session, index) = open(
        daemon,
        scratch,
        "permission",
        &[],
        model,
        "Use the Bash tool to run exactly: echo allowed-probe > allowed.txt",
    )
    .await?;

    // Allow leg.
    let index = session
        .wait_for_row(
            index,
            ASK_TIMEOUT,
            "permission request (allow leg)",
            |row| row.row_type() == "hook.permission_request",
        )
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys("allow once: digit 1", vec![Act::Write(b"1".to_vec())])
        .await?;
    let index = session.wait_for_turn_end(index, TURN_TIMEOUT).await?;

    // Deny leg.
    session
        .send_prompt("Use the Bash tool to run exactly: echo denied-probe > denied.txt")
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "permission request (deny leg)", |row| {
            row.row_type() == "hook.permission_request"
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys("deny: Esc", vec![Act::Write(b"\x1b".to_vec())])
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "user-rejected denial row", |row| {
            row.raw.contains("toolDenialKind") || row.raw.contains("user-rejected")
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;

    let allowed = scratch.projects.join("permission/allowed.txt").exists();
    let denied = scratch.projects.join("permission/denied.txt").exists();
    let keys = session.close().await?;
    if !allowed {
        bail!("world assertion failed: allowed.txt missing after allow");
    }
    if denied {
        bail!("world assertion failed: denied.txt exists after deny");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "allowed.txt": allowed, "denied.txt": denied },
    }))
}

/// AskUserQuestion, single-select.
async fn question_single(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_single",
        &[],
        model,
        "Use the AskUserQuestion tool to ask me exactly one single-select question. \
         Header: Color. Question: Which color do you prefer? Options: Red, Blue. \
         After I answer, reply with the answer and stop.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "AskUserQuestion request", |row| {
            row.row_type() == "hook.permission_request" && row.raw.contains("AskUserQuestion")
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "select option 1 (digit), then Enter",
            vec![
                Act::Write(b"1".to_vec()),
                Act::DelayMs(400),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "question answers row", |row| {
            row.raw.contains("\"answers\"")
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys }))
}

/// AskUserQuestion, multi-select with an Other free-text answer.
async fn question_multi(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_multi",
        &[],
        model,
        "Use the AskUserQuestion tool to ask me exactly one question with \
         multiSelect true. Header: Tools. Question: Which tools should I use? \
         Options: Hammer, Saw, Drill. After I answer, reply with the answer and stop.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "AskUserQuestion request", |row| {
            row.row_type() == "hook.permission_request" && row.raw.contains("AskUserQuestion")
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    // Observed multi-select layout + interaction (claude 2.1.228, verified via
    // the raw PTY and empirical probing): a numbered checkbox list
    // `1.[] Hammer` … with an appended `N. Type something` (the Other entry)
    // and an in-list `Submit` row; ↑/↓ navigate, Space toggles a checkbox.
    // The Other is the fiddly bit: Enter opens an inline editor, typing +
    // Enter *saves the text but does NOT check the box* — a following **Space
    // commits (checks) the custom option**. Without that Space the Other value
    // is silently dropped from the submitted answers (empirically verified:
    // Other-alone without the Space submits `""`; with it, the text lands).
    session
        .send_keys(
            "space-toggle Hammer; ↓ space-toggle Saw; ↓↓ Other: Enter, type, Enter, Space-check",
            vec![
                Act::Write(b" ".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\x1b[B".to_vec()),
                Act::DelayMs(400),
                Act::Write(b" ".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\x1b[B".to_vec()),
                Act::DelayMs(300),
                Act::Write(b"\x1b[B".to_vec()),
                Act::DelayMs(400),
                Act::Write(b"\r".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"a torque wrench".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\r".to_vec()),
                Act::DelayMs(600),
                Act::Write(b" ".to_vec()),
            ],
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // Submit via the Submit tab: Tab, Enter → "Review your answers", Enter
    // confirms the preselected "Submit answers".
    session
        .send_keys(
            "Tab to Submit tab, Enter to review, Enter to confirm submit",
            vec![
                Act::Write(b"\t".to_vec()),
                Act::DelayMs(900),
                Act::Write(b"\r".to_vec()),
                Act::DelayMs(1000),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "question answers row", |row| {
            row.raw.contains("\"answers\"")
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;

    // Structural assertion: a genuine multi-select-with-Other must record BOTH
    // real selections and the Other free-text value in the parsed answers —
    // otherwise the fixture is a single-select mislabelled as multi.
    let answers = latest_answers(&session.snapshot().await)
        .ok_or_else(|| anyhow::anyhow!("no toolUseResult.answers found in the capture"))?;
    let joined = answers
        .values()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    for expected in ["Hammer", "Saw", "a torque wrench"] {
        if !joined.contains(expected) {
            bail!(
                "multi-select assertion failed: expected '{expected}' in answers, \
                 got {joined:?} (keystroke encoding did not register the full \
                 multi-selection + Other)"
            );
        }
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys, "answers": answers }))
}

/// Interrupt mid-turn via Esc.
async fn interrupt(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "interrupt",
        &[],
        model,
        "Without using any tools, write a very detailed 800-word essay \
         about the history of rivers. Take your time.",
    )
    .await?;
    // Let the turn get going, then interrupt while the message is still
    // being generated (main files burst-write, so nothing arrives before
    // the interrupt flush — the wait here is wall-clock, not row-driven).
    tokio::time::sleep(Duration::from_secs(6)).await;
    session
        .send_keys("interrupt: Esc", vec![Act::Write(b"\x1b".to_vec())])
        .await?;
    session
        .wait_for_row(index, ASK_TIMEOUT, "interrupt artifact row", |row| {
            row.raw.contains("Request interrupted by user")
                || row.raw.contains("interruptedMessageId")
        })
        .await?;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys }))
}

/// Plan mode: approve or request-changes — the UNOBSERVED ExitPlanMode rows.
async fn plan(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
    approve: bool,
) -> Result<serde_json::Value> {
    let scenario = if approve {
        "plan_approve"
    } else {
        "plan_reject"
    };
    let (mut session, index) = open(
        daemon,
        scratch,
        scenario,
        &["--permission-mode", "plan"],
        model,
        "Make a short plan for adding a README.md that documents config.txt. \
         Do not ask any clarifying questions — make reasonable assumptions. \
         When the plan is ready, use the ExitPlanMode tool to present it \
         directly.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "ExitPlanMode request", |row| {
            row.row_type() == "hook.permission_request" && row.raw.contains("ExitPlanMode")
        })
        .await?;
    // The ExitPlanMode tool_use id, so we can wait for *its* resolution row
    // precisely rather than any tool_result.
    let plan_tool_id = session
        .snapshot()
        .await
        .iter()
        .find_map(|row| row.tool_use_id("ExitPlanMode"));
    tokio::time::sleep(MENU_SETTLE).await;
    if approve {
        // Option 2: approve with manual edit approval — captures the
        // ExitPlanMode tool_result success + the following permission-mode row.
        session
            .send_keys(
                "plan approve (manual): digit 2",
                vec![Act::Write(b"2".to_vec())],
            )
            .await?;
        // Wait for the ExitPlanMode resolution row, then stop. We do NOT wait
        // for turn end: after a manual approve, claude proceeds and blocks on
        // the next Write permission, so wait_for_turn_end would burn the full
        // timeout for a row we've already captured.
        session
            .wait_for_row(index, ASK_TIMEOUT, "ExitPlanMode resolution row", |row| {
                match &plan_tool_id {
                    Some(id) => row.is_tool_result_for(id),
                    None => row.row_type() == "user" && row.raw.contains("tool_use_id"),
                }
            })
            .await?;
        // Brief settle so the trailing permission-mode row flushes into the
        // capture, then stop the scenario.
        tokio::time::sleep(Duration::from_secs(2)).await;
    } else {
        // Option 3: reject — keep planning, with feedback text.
        session
            .send_keys(
                "plan reject: digit 3, then feedback text + Enter",
                vec![
                    Act::Write(b"3".to_vec()),
                    Act::DelayMs(800),
                    Act::Write(b"Please also document the meaning of VALUE.".to_vec()),
                    Act::DelayMs(400),
                    Act::Write(b"\r".to_vec()),
                ],
            )
            .await?;
        let index = session
            .wait_for_row(index, ASK_TIMEOUT, "ExitPlanMode rejection row", |row| {
                row.row_type() == "user" && row.raw.contains("tool_use_id")
            })
            .await?;
        // The agent goes back to planning; give it a beat then interrupt so
        // the session ends deterministically.
        let _ = session
            .wait_for_turn_end(index, Duration::from_secs(60))
            .await;
        session
            .send_keys("end scenario: Esc", vec![Act::Write(b"\x1b".to_vec())])
            .await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys }))
}

/// /compact after a couple of turns.
async fn compact(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "compact",
        &[],
        model,
        "Reply with a two-paragraph description of what a config file is.",
    )
    .await?;
    let index = session.wait_for_turn_end(index, TURN_TIMEOUT).await?;
    session
        .send_prompt("Now reply with a haiku about configuration.")
        .await?;
    let index = session.wait_for_turn_end(index, TURN_TIMEOUT).await?;
    session
        .send_keys(
            "slash command: /compact + Enter",
            vec![
                Act::Write(b"/compact".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    session
        .wait_for_row(index, TURN_TIMEOUT, "compact_boundary row", |row| {
            row.raw.contains("compact_boundary")
        })
        .await?;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys }))
}
