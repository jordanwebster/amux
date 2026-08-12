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
//! interrupt, plan_approve, plan_reject, compact (or `all`), plus the
//! Phase 3 encoding-verification set: permission_session,
//! permission_deny_feedback, question_tabs, plan_auto, mode_cycle,
//! prompt_multiline.
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
        "permission_session",
        "permission_deny_feedback",
        "question_tabs",
        "question_other_single",
        "question_mixed",
        "plan_auto",
        "mode_cycle",
        "prompt_multiline",
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
        "permission_session" => permission_session(daemon, scratch, model).await,
        "permission_deny_feedback" => permission_deny_feedback(daemon, scratch, model).await,
        "question_tabs" => question_tabs(daemon, scratch, model).await,
        "question_other_single" => question_other_single(daemon, scratch, model).await,
        "question_mixed" => question_mixed(daemon, scratch, model).await,
        "plan_auto" => plan_auto(daemon, scratch, model).await,
        "mode_cycle" => mode_cycle(daemon, scratch, model).await,
        "prompt_multiline" => prompt_multiline(daemon, scratch, model).await,
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

// --- Phase 3 encoding-verification scenarios --------------------------------
//
// Each scenario exists to confirm one C6 keystroke table empirically before
// the amux-ui encoding module states it. Assertions are structural (world +
// row shapes), never prose; the `notes` object records the observed evidence
// the phase report cites.

/// Permission menu digit 2 — "allow for this session"/"don't ask again".
/// Verifies: digit 2 resolves the ask as allowed AND the session-scope fact
/// (`command_permissions` attachment / no re-ask for the same command shape).
async fn permission_session(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "permission_session",
        &[],
        model,
        "Use the Bash tool to run exactly: echo probe-one > one.txt. Then, in a \
         second separate Bash tool call, run exactly: echo probe-two > two.txt. \
         Then stop.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "permission request", |row| {
            row.row_type() == "hook.permission_request"
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "allow for session: digit 2",
            vec![Act::Write(b"2".to_vec())],
        )
        .await?;
    // The second Bash call may be covered by the session allowance (no second
    // menu) or re-ask (then we allow once). Watch for either a second
    // permission request or the turn end.
    let mut cursor = index;
    let mut second_ask = false;
    loop {
        let rows = session.snapshot().await;
        if rows.iter().skip(cursor).any(|row| {
            row.row_type() == "system"
                && row.json.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration")
        }) {
            break;
        }
        if let Some(pos) = rows
            .iter()
            .enumerate()
            .skip(cursor)
            .find(|(_, row)| row.row_type() == "hook.permission_request")
            .map(|(i, _)| i + 1)
        {
            second_ask = true;
            cursor = pos;
            tokio::time::sleep(MENU_SETTLE).await;
            session
                .send_keys(
                    "allow once (second ask): digit 1",
                    vec![Act::Write(b"1".to_vec())],
                )
                .await?;
        }
        if rows.len() > cursor {
            cursor = rows.len();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let one = scratch.projects.join("permission_session/one.txt").exists();
    let two = scratch.projects.join("permission_session/two.txt").exists();
    let rows = session.snapshot().await;
    let command_permissions = rows
        .iter()
        .any(|row| row.raw.contains("command_permissions"));
    let keys = session.close().await?;
    if !one || !two {
        bail!("world assertion failed: one.txt={one} two.txt={two} after digit-2 allow");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "one.txt": one, "two.txt": two },
        "observed": {
            "command_permissions_attachment": command_permissions,
            "second_permission_ask": second_ask,
        },
    }))
}

/// Permission menu digit 3 — deny with feedback. Verifies whether digit 3
/// opens a feedback field (like the plan menu's request-changes) and how the
/// feedback lands (`userFeedback` on the denial row vs a separate prompt).
async fn permission_deny_feedback(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "permission_deny_feedback",
        &[],
        model,
        "Use the Bash tool to run exactly: echo denied-probe > denied.txt. Then stop.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "permission request", |row| {
            row.row_type() == "hook.permission_request"
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    // First run of this scenario (claude 2.1.228): the Bash permission
    // menu's last option is a bare `No` — digit 3 denies IMMEDIATELY
    // (typed denial + `[Request interrupted by user for tool use]` +
    // turn_duration; no feedback field, unlike the plan menu). The
    // deny-with-feedback composition is therefore digit 3, then the
    // feedback as a follow-up prompt — verified here as the ONE program
    // the C6 encoder emits (the settle delay covers the denial flush and
    // the composer regaining focus; bracketed paste keeps the text
    // literal).
    let feedback = "Do not create that file; it is not needed. Acknowledge and stop.";
    let mut paste = Vec::new();
    paste.extend_from_slice(b"\x1b[200~");
    paste.extend_from_slice(feedback.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    session
        .send_keys(
            "deny with feedback, one program: digit 3, settle, paste feedback, Enter",
            vec![
                Act::Write(b"3".to_vec()),
                Act::DelayMs(1500),
                Act::Write(paste),
                Act::DelayMs(400),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "typed denial row", |row| {
            row.json.get("toolDenialKind").is_some()
        })
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "feedback prompt row", |row| {
            row.row_type() == "user"
                && row
                    .json
                    .pointer("/message/content")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("Do not create that file"))
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;

    let rows = session.snapshot().await;
    let denial_feedback = rows
        .iter()
        .rev()
        .find_map(|row| row.json.get("userFeedback").and_then(|f| f.as_str()))
        .map(str::to_string);
    let denied = scratch
        .projects
        .join("permission_deny_feedback/denied.txt")
        .exists();
    let keys = session.close().await?;
    if denied {
        bail!("world assertion failed: denied.txt exists after deny");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "denied.txt": denied },
        "observed": {
            "denial_userFeedback": denial_feedback,
            "deny_is_immediate_no_feedback_field": true,
        },
    }))
}

/// Wait for the question form's answers row; while it has not landed a
/// review/submit step is still up — confirm it with Enter (up to three).
/// Returns how many extra Enters were needed, recorded per scenario as
/// `observed.extra_submit_steps` (evidence for the C6 submit tables: zero
/// means the scenario's own program submitted the form).
async fn confirm_question_submit(
    session: &mut CaptureSession,
    index: usize,
    flow: &str,
) -> Result<u32> {
    let mut extra_submit_steps = 0u32;
    loop {
        let answered = session
            .wait_for_row(
                index,
                Duration::from_secs(15),
                "question answers row",
                |row| row.raw.contains("\"answers\""),
            )
            .await
            .is_ok();
        if answered {
            return Ok(extra_submit_steps);
        }
        if extra_submit_steps >= 3 {
            bail!("no answers row after the {flow} flow (+{extra_submit_steps} submit Enters)");
        }
        extra_submit_steps += 1;
        session
            .send_keys("submit step: Enter", vec![Act::Write(b"\r".to_vec())])
            .await?;
    }
}

/// AskUserQuestion with TWO single-select questions: verifies the
/// multi-question tab flow — digit selects, Enter advances to the next
/// question tab, the final submit step confirms all answers.
async fn question_tabs(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_tabs",
        &[],
        model,
        "Use the AskUserQuestion tool to ask me exactly two single-select \
         questions in ONE tool call. Question 1: header Color, question \
         'Which color do you prefer?', options Red, Blue. Question 2: header \
         Size, question 'Which size fits best?', options Small, Large. After \
         I answer, reply with both answers and stop.",
    )
    .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "AskUserQuestion request", |row| {
            row.row_type() == "hook.permission_request" && row.raw.contains("AskUserQuestion")
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    // Observed model (first run of this scenario, claude 2.1.228): on a
    // single-select question list a DIGIT selects that option and advances
    // to the next question tab immediately — no Enter. Answering the last
    // question advances to the review step (`1. Submit answers` /
    // `2. Cancel`, Submit preselected), where Enter confirms. (The first
    // run pressed digit+Enter per question and the surplus keys walked the
    // review onto Cancel — captured as the decline denial artifacts.)
    session
        .send_keys(
            "Q1: digit 1 (Red, selects+advances); Q2: digit 2 (Large); review: Enter submits",
            vec![
                Act::Write(b"1".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"2".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let extra_submit_steps = confirm_question_submit(&mut session, index, "tab").await?;
    let _ = session
        .wait_for_turn_end(index, Duration::from_secs(60))
        .await;

    let answers = latest_answers(&session.snapshot().await)
        .ok_or_else(|| anyhow::anyhow!("no toolUseResult.answers found in the capture"))?;
    let keys = session.close().await?;
    if answers.len() != 2 {
        bail!("expected answers for BOTH questions, got {answers:?}");
    }
    let joined = answers
        .values()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    for expected in ["Red", "Large"] {
        if !joined.contains(expected) {
            bail!("expected '{expected}' among answers, got {joined:?}");
        }
    }
    Ok(serde_json::json!({
        "keys": keys,
        "answers": answers,
        "observed": { "extra_submit_steps": extra_submit_steps },
    }))
}

/// The Other ("Type something") flow on a SINGLE-select question: the
/// appended Other option's digit opens an inline editor; typing + Enter
/// commits the custom answer (multi-select needs a trailing Space —
/// verified separately in question_multi).
async fn question_other_single(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_other_single",
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
    // Two predefined options, so the appended `Type something.` (Other) is
    // digit 3. Open it, type, Enter to save; if the flow lands on the
    // review step, Enter confirms Submit.
    session
        .send_keys(
            "Other: digit 3 opens the editor; type; Enter saves",
            vec![
                Act::Write(b"3".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"a warm ochre".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let extra_submit_steps = confirm_question_submit(&mut session, index, "Other").await?;
    let _ = session
        .wait_for_turn_end(index, Duration::from_secs(60))
        .await;
    let answers = latest_answers(&session.snapshot().await)
        .ok_or_else(|| anyhow::anyhow!("no toolUseResult.answers found in the capture"))?;
    let keys = session.close().await?;
    let joined = answers
        .values()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    if !joined.contains("ochre") {
        bail!("the Other free-text did not land in the answers: {joined:?}");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "answers": answers,
        "observed": { "extra_submit_steps": extra_submit_steps },
    }))
}

/// A MIXED multi-question form (multi-select first, single-select second):
/// verifies the one remaining navigation hop — Tab advancing from a
/// multi-select question to the NEXT question tab (not straight to
/// Submit), composing with the digit auto-advance and the review Enter.
async fn question_mixed(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_mixed",
        &[],
        model,
        "Use the AskUserQuestion tool to ask me exactly two questions in ONE \
         tool call. Question 1: header Tools, question 'Which tools should I \
         use?', options Hammer, Saw, Drill, with multiSelect true. Question 2: \
         header Size, question 'Which size fits best?', options Small, Large, \
         single-select. After I answer, reply with both answers and stop.",
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
            "Q1 (multi): Space Hammer, down, Space Saw, Tab to Q2; Q2: digit 2 (Large); review: Enter",
            vec![
                Act::Write(b" ".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\x1b[B".to_vec()),
                Act::DelayMs(400),
                Act::Write(b" ".to_vec()),
                Act::DelayMs(500),
                Act::Write(b"\t".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"2".to_vec()),
                Act::DelayMs(800),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let extra_submit_steps = confirm_question_submit(&mut session, index, "mixed").await?;
    let _ = session
        .wait_for_turn_end(index, Duration::from_secs(60))
        .await;
    let answers = latest_answers(&session.snapshot().await)
        .ok_or_else(|| anyhow::anyhow!("no toolUseResult.answers found in the capture"))?;
    let keys = session.close().await?;
    if answers.len() != 2 {
        bail!("expected answers for BOTH questions, got {answers:?}");
    }
    let joined = answers
        .values()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    for expected in ["Hammer", "Saw", "Large"] {
        if !joined.contains(expected) {
            bail!("expected '{expected}' among answers, got {joined:?}");
        }
    }
    Ok(serde_json::json!({
        "keys": keys,
        "answers": answers,
        "observed": { "extra_submit_steps": extra_submit_steps },
    }))
}

/// Plan review — approve with AUTO edit acceptance (menu digit 1): the H.5
/// sub-capture. Verifies the digit AND whether auto-approval flips the
/// `permission-mode` row (manual approval does not — Phase 0).
async fn plan_auto(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "plan_auto",
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
    let plan_tool_id = session
        .snapshot()
        .await
        .iter()
        .find_map(|row| row.tool_use_id("ExitPlanMode"));
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "plan approve (auto): digit 1",
            vec![Act::Write(b"1".to_vec())],
        )
        .await?;
    let index =
        session
            .wait_for_row(index, ASK_TIMEOUT, "ExitPlanMode resolution row", |row| {
                match &plan_tool_id {
                    Some(id) => row.is_tool_result_for(id),
                    None => row.row_type() == "user" && row.raw.contains("tool_use_id"),
                }
            })
            .await?;
    // Under auto acceptance claude proceeds to write README.md WITHOUT a
    // permission ask; the turn end closes the scenario and its hook.stop
    // payload carries the effective permission_mode.
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let rows = session.snapshot().await;
    let mode_rows: Vec<String> = rows
        .iter()
        .filter(|row| row.row_type() == "permission-mode")
        .filter_map(|row| {
            row.json
                .get("permissionMode")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .collect();
    let hook_modes: Vec<String> = rows
        .iter()
        .filter(|row| row.row_type().starts_with("hook."))
        .filter_map(|row| {
            row.json
                .get("permission_mode")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .collect();
    let ask_count = rows
        .iter()
        .filter(|row| row.row_type() == "hook.permission_request")
        .count();
    let readme = scratch.projects.join("plan_auto/README.md").exists();
    let keys = session.close().await?;
    if !readme {
        bail!("world assertion failed: README.md missing — auto-approved edits did not land");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "README.md": readme },
        "observed": {
            "permission_mode_rows": mode_rows,
            "hook_permission_modes": hook_modes,
            "permission_request_rows": ask_count,
        },
    }))
}

/// Shift+Tab permission-mode cycling — the OPEN D4 question: does a
/// mid-session cycle re-emit the `permission-mode` row with the new value?
/// A follow-up permission prompt captures the hook payload's
/// `permission_mode` as the fallback source either way.
async fn mode_cycle(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, mut index) = open(
        daemon,
        scratch,
        "mode_cycle",
        &[],
        model,
        "Reply with exactly OK and nothing else.",
    )
    .await?;
    index = session.wait_for_turn_end(index, TURN_TIMEOUT).await?;
    let rows_before = session.snapshot().await.len();

    // The probe pattern: cycle N times, then run a trivial turn whose
    // arrival-ordered hook.stop payload states the EFFECTIVE
    // permission_mode — the D4 fallback source, and the proof the cycle
    // registered even if no `permission-mode` row is ever written. One
    // press (default → acceptEdits) is probed directly; the wrap probe
    // presses twice more (acceptEdits → plan → default) WITHOUT prompting
    // in between — a prompt in plan mode triggers the whole plan flow
    // (observed on the first run of this scenario).
    let mut hook_modes_by_probe: Vec<Option<String>> = Vec::new();
    for (probe, presses) in [(1u32, 1u32), (2, 2)] {
        for _ in 0..presses {
            session
                .send_keys(
                    "cycle permission mode: Shift+Tab (CSI Z)",
                    vec![Act::Write(b"\x1b[Z".to_vec())],
                )
                .await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        session
            .send_prompt("Reply with exactly OK and nothing else.")
            .await?;
        index = session
            .wait_for_row(index, TURN_TIMEOUT, "hook.stop after cycle", |row| {
                row.row_type() == "hook.stop"
            })
            .await?;
        let mode = session
            .snapshot()
            .await
            .iter()
            .rev()
            .find(|row| row.row_type() == "hook.stop")
            .and_then(|row| {
                row.json
                    .get("permission_mode")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            });
        println!("capture: mode after probe {probe}: {mode:?}");
        hook_modes_by_probe.push(mode);
        // Let the transcript tail (turn_duration + any session-state
        // re-emission) land before the next press.
        let _ = session
            .wait_for_row(index, Duration::from_secs(30), "turn_duration", |row| {
                row.row_type() == "system"
                    && row.json.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration")
            })
            .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        index = session.snapshot().await.len();
    }

    // Every permission-mode row written after the first cycle keystroke —
    // the D4 row-emission verdict.
    let rows = session.snapshot().await;
    let mode_rows_all: Vec<String> = rows
        .iter()
        .skip(rows_before)
        .filter(|row| row.row_type() == "permission-mode")
        .filter_map(|row| {
            row.json
                .get("permissionMode")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .collect();
    let keys = session.close().await?;
    if hook_modes_by_probe.iter().any(|mode| mode.is_none()) {
        bail!("hook.stop after a cycle carried no permission_mode: {hook_modes_by_probe:?}");
    }
    if hook_modes_by_probe[0].as_deref() == Some("default") {
        bail!("the first Shift+Tab did not register (mode still default)");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "observed": {
            "hook_permission_mode_by_probe": hook_modes_by_probe,
            "permission_mode_rows_after_cycling": mode_rows_all,
        },
    }))
}

/// Multiline prompt submit via bracketed paste (ESC[200~ … ESC[201~), plus
/// the B1 echo-correlation evidence: the transcript user row's string
/// content vs the injected text.
async fn prompt_multiline(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "prompt_multiline",
        &[],
        model,
        "Reply with exactly PONG and nothing else.",
    )
    .await?;
    let index = session.wait_for_turn_end(index, TURN_TIMEOUT).await?;

    let text = "Reply with exactly DONE and nothing else.\nThis second line is part of one prompt.";
    let mut payload = Vec::new();
    payload.extend_from_slice(b"\x1b[200~");
    payload.extend_from_slice(text.as_bytes());
    payload.extend_from_slice(b"\x1b[201~");
    session
        .send_keys(
            "multiline prompt: bracketed paste, then Enter",
            vec![
                Act::Write(payload),
                Act::DelayMs(400),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "multiline prompt user row", |row| {
            row.row_type() == "user"
                && row
                    .json
                    .pointer("/message/content")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("DONE"))
        })
        .await?;
    let echoed = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find_map(|row| {
            if row.row_type() != "user" {
                return None;
            }
            row.json
                .pointer("/message/content")
                .and_then(|c| c.as_str())
                .filter(|c| c.contains("DONE"))
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("multiline prompt row vanished from the snapshot"))?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;
    let keys = session.close().await?;
    if !echoed.contains('\n') {
        bail!("the newline did not survive bracketed paste: {echoed:?}");
    }
    Ok(serde_json::json!({
        "keys": keys,
        "observed": {
            "sent_text": text,
            "row_content": echoed,
            "content_equals_sent": echoed == text,
        },
    }))
}
