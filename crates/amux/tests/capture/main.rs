//! The maintained, opt-in real-Claude E2E suite from `docs/CHAT.md` §H.
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
//! `h-all` runs H.1–H.9 (H.5 has three independently reported legs). `all`
//! additionally runs the stable capture regressions retained from Phases 0
//! and 3; `probes` runs deliberately non-gating investigations of currently
//! unverified Claude menus. Every scenario has one row in [`SCENARIOS`]: id,
//! requirement, model policy, timeout, and runner cannot drift apart.
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
//! A failure always prints the assertion and this capture path. Taxonomy
//! drift is written beside the run as data and never changes its exit code.

mod graduate;
mod harness;
mod redact;
mod structure;
mod taxonomy;

use std::path::{Path, PathBuf};
use std::time::Duration;

use amux::claude_io::ClaudePtyTranscriptV1Action as Act;
use anyhow::{Context, Result, bail};
use harness::{CaptureSession, DaemonEnv, Scratch, ScratchDaemon, claude_version};

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const TURN_TIMEOUT: Duration = Duration::from_secs(240);
const ASK_TIMEOUT: Duration = Duration::from_secs(180);

/// Pause between a menu appearing (its hook row arriving) and answering it:
/// the hook fires as the dialog renders, and keystrokes that race the render
/// are dropped by claude's TUI.
const MENU_SETTLE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Pong,
    Tools,
    Permission,
    QuestionSingle,
    QuestionMulti,
    Interrupt,
    PlanManual,
    PlanChanges,
    Compact,
    PermissionSession,
    PermissionDenyFeedback,
    QuestionTabs,
    QuestionOtherSingle,
    QuestionMixed,
    PlanAuto,
    ModeCycle,
    PromptMultiline,
    StaleSeq,
    Subscriptions,
    ExternalReadonly,
    PermissionMultiProbe,
    QuestionChatProbe,
}

#[derive(Clone, Copy)]
struct ScenarioSpec {
    name: &'static str,
    requirement: &'static str,
    kind: Scenario,
    default_model: &'static str,
    timeout: Duration,
    in_h_suite: bool,
    probe: bool,
}

const fn scenario(
    name: &'static str,
    requirement: &'static str,
    kind: Scenario,
    in_h_suite: bool,
) -> ScenarioSpec {
    ScenarioSpec {
        name,
        requirement,
        kind,
        default_model: "haiku",
        timeout: Duration::from_secs(360),
        in_h_suite,
        probe: false,
    }
}

/// The one scenario grammar. Supporting captures are kept in the same table
/// as H.1–H.9 so selection, provenance, timeout policy, and dispatch share a
/// single source of truth.
const SCENARIOS: &[ScenarioSpec] = &[
    scenario("pong", "H.1", Scenario::Pong, true),
    scenario("question_single", "H.2", Scenario::QuestionSingle, true),
    scenario("question_multi", "H.3", Scenario::QuestionMulti, true),
    scenario("permission", "H.4", Scenario::Permission, true),
    scenario("plan_auto", "H.5/approve-auto", Scenario::PlanAuto, true),
    scenario(
        "plan_approve",
        "H.5/approve-manual",
        Scenario::PlanManual,
        true,
    ),
    scenario(
        "plan_reject",
        "H.5/request-changes",
        Scenario::PlanChanges,
        true,
    ),
    scenario("interrupt", "H.6", Scenario::Interrupt, true),
    scenario("stale_seq", "H.7", Scenario::StaleSeq, true),
    scenario("subscriptions", "H.8", Scenario::Subscriptions, true),
    ScenarioSpec {
        name: "external_readonly",
        requirement: "H.9",
        kind: Scenario::ExternalReadonly,
        default_model: "not-applicable",
        timeout: Duration::from_secs(120),
        in_h_suite: true,
        probe: false,
    },
    scenario("tools", "support/tool-pairing", Scenario::Tools, false),
    scenario(
        "compact",
        "support/manual-compact",
        Scenario::Compact,
        false,
    ),
    scenario(
        "permission_session",
        "support/permission-scope",
        Scenario::PermissionSession,
        false,
    ),
    scenario(
        "permission_deny_feedback",
        "support/permission-feedback",
        Scenario::PermissionDenyFeedback,
        false,
    ),
    scenario(
        "question_tabs",
        "support/question-tabs",
        Scenario::QuestionTabs,
        false,
    ),
    scenario(
        "question_other_single",
        "support/question-other",
        Scenario::QuestionOtherSingle,
        false,
    ),
    scenario(
        "question_mixed",
        "support/question-mixed",
        Scenario::QuestionMixed,
        false,
    ),
    scenario("mode_cycle", "support/D4", Scenario::ModeCycle, false),
    scenario(
        "prompt_multiline",
        "support/B1-multiline",
        Scenario::PromptMultiline,
        false,
    ),
    ScenarioSpec {
        name: "permission_multi_probe",
        requirement: "probe/permission-suggestions>=2",
        kind: Scenario::PermissionMultiProbe,
        default_model: "haiku",
        timeout: Duration::from_secs(360),
        in_h_suite: false,
        probe: true,
    },
    ScenarioSpec {
        name: "question_chat_probe",
        requirement: "probe/question-chat-about-this",
        kind: Scenario::QuestionChatProbe,
        default_model: "haiku",
        timeout: Duration::from_secs(360),
        in_h_suite: false,
        probe: true,
    },
];

fn main() -> Result<()> {
    validate_scenario_grammar()?;
    let scenario_names: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    if scenario_names.first().is_some_and(|arg| arg == "tooling") {
        return run_tooling(&scenario_names[1..]);
    }
    if scenario_names.is_empty() {
        println!(
            "capture: no scenarios named; skipping (opt-in real-Claude harness — \
             pass `h-all`, `all`, `probes`, or scenario names)"
        );
        return Ok(());
    }
    let selected: Vec<&ScenarioSpec> = if scenario_names.iter().any(|name| name == "all") {
        SCENARIOS.iter().filter(|spec| !spec.probe).collect()
    } else if scenario_names.iter().any(|name| name == "h-all") {
        SCENARIOS.iter().filter(|spec| spec.in_h_suite).collect()
    } else if scenario_names.iter().any(|name| name == "probes") {
        SCENARIOS.iter().filter(|spec| spec.probe).collect()
    } else {
        let mut selected = Vec::new();
        for name in &scenario_names {
            let Some(known) = SCENARIOS.iter().find(|known| known.name == name) else {
                let names: Vec<_> = SCENARIOS.iter().map(|spec| spec.name).collect();
                bail!("unknown scenario '{name}'; known: {names:?}");
            };
            selected.push(known);
        }
        selected
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(selected))
}

fn validate_scenario_grammar() -> Result<()> {
    let mut names = std::collections::BTreeSet::new();
    for spec in SCENARIOS {
        if !names.insert(spec.name) {
            bail!("duplicate scenario id in grammar: {}", spec.name);
        }
        if spec.timeout.is_zero() || spec.default_model.is_empty() {
            bail!("scenario {} has incomplete execution policy", spec.name);
        }
    }
    for requirement in 1..=9 {
        let prefix = format!("H.{requirement}");
        if !SCENARIOS
            .iter()
            .any(|spec| spec.in_h_suite && spec.requirement.starts_with(&prefix))
        {
            bail!("scenario grammar has no {prefix} leg");
        }
    }
    Ok(())
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chat-v1")
}

fn semantics_markdown() -> &'static str {
    include_str!("../../../../notes/chat-v1/transcript-semantics.md")
}

fn run_tooling(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("tooling requires verify, drift, or graduate");
    };
    let Some(run_dir) = args.get(1).map(structure::workspace_path) else {
        bail!("tooling {command} requires RUN_DIR");
    };
    match command {
        "drift" => {
            let report = taxonomy::write_report(&run_dir, &fixture_dir(), semantics_markdown())?;
            print!("{}", report.render());
        }
        "verify" | "graduate" => {
            let scenarios = tooling_scenarios(&run_dir, &args[2..])?;
            graduate::verify_run(&run_dir, &scenarios)?;
            if command == "graduate" {
                graduate::graduate(&run_dir, &fixture_dir(), &scenarios)?;
                println!("graduated fixtures: {scenarios:?}");
            } else {
                println!("verified fixture candidates: {scenarios:?}");
            }
        }
        other => bail!("unknown tooling command '{other}' (expected verify, drift, graduate)"),
    }
    Ok(())
}

fn tooling_scenarios(run_dir: &Path, requested: &[String]) -> Result<Vec<String>> {
    if !requested.is_empty() {
        return Ok(requested.to_vec());
    }
    let mut scenarios = Vec::new();
    for entry in std::fs::read_dir(run_dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        if let Some(name) = name.strip_suffix(".meta.json") {
            scenarios.push(name.to_string());
        }
    }
    scenarios.sort();
    if scenarios.is_empty() {
        bail!("no scenario metadata found in {}", run_dir.display());
    }
    Ok(scenarios)
}

async fn run(scenarios: Vec<&ScenarioSpec>) -> Result<()> {
    let out = match std::env::var("AMUX_CAPTURE_OUT") {
        Ok(dir) => structure::workspace_path(dir),
        Err(_) => structure::workspace_path("target/capture").join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        )),
    };
    let model_override = std::env::var("AMUX_CAPTURE_MODEL").ok();
    let poisoned = std::env::var("AMUX_CAPTURE_POISON").as_deref() != Ok("0");

    let scratch = Scratch::create(out.clone())?;
    let daemon = harness::start_daemon(&scratch, &DaemonEnv { poisoned }).await?;
    let version = claude_version();
    println!("capture: daemon up (poisoned={poisoned}), claude: {version}");
    println!("capture: output dir {}", out.display());

    let mut failures = Vec::new();
    for spec in scenarios {
        let model = model_override.as_deref().unwrap_or(spec.default_model);
        println!(
            "=== {} {} (model={model}, timeout={}s) ===",
            spec.requirement,
            spec.name,
            spec.timeout.as_secs()
        );
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            spec.timeout,
            run_scenario(&daemon, &scratch, spec.kind, spec.name, model),
        )
        .await
        .map_err(|_| anyhow::anyhow!("scenario timeout after {:?}", spec.timeout))
        .and_then(|result| result);
        // A timeout drops the scenario future, so it cannot call
        // CaptureSession::close itself. Abort and join its recorder tasks and
        // delete its agent before finalization reads the candidate files or
        // the next scenario starts. This is also a safety net for any early
        // structural-assertion error.
        scratch
            .cancel_active_session(&daemon.client)
            .await
            .context("clean up canceled scenario")?;
        match result {
            Ok(notes) => {
                finalize(&scratch, spec, model, &version, poisoned, notes, None)?;
                println!(
                    "=== {} {} PASS ({:.0}s) capture={} ===",
                    spec.requirement,
                    spec.name,
                    started.elapsed().as_secs_f64(),
                    scratch.out.display()
                );
            }
            Err(error) => {
                let message = format!("{error:#}");
                println!(
                    "=== {} {} FAIL: {message}; capture={} ===",
                    spec.requirement,
                    spec.name,
                    scratch.out.display()
                );
                finalize(
                    &scratch,
                    spec,
                    model,
                    &version,
                    poisoned,
                    serde_json::json!({}),
                    Some(message.clone()),
                )?;
                failures.push((spec.requirement, spec.name, message));
            }
        }
    }

    match taxonomy::write_report(&scratch.out, &fixture_dir(), semantics_markdown()) {
        Ok(report) => println!(
            "capture: taxonomy drift report {} ({})",
            scratch.out.join("taxonomy-drift.txt").display(),
            if report.changed() {
                "differences recorded"
            } else {
                "no differences"
            }
        ),
        Err(error) => println!("capture: taxonomy report unavailable: {error:#}"),
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
    spec: &ScenarioSpec,
    model: &str,
    version: &str,
    poisoned: bool,
    notes: serde_json::Value,
    failure: Option<String>,
) -> Result<()> {
    let scenario = spec.name;
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
        "requirement": spec.requirement,
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "claude_version": version,
        "model": model,
        "harness": format!("cargo test -p amux --test capture -- {scenario}"),
        "poisoned_daemon_env": poisoned,
        "notes": notes,
        "failure": failure,
    });
    let meta = serde_json::to_string_pretty(&meta)?;
    let meta = redact::redact(&meta, &scratch.root)?;
    std::fs::write(scratch.out.join(format!("{scenario}.meta.json")), meta)?;
    Ok(())
}

async fn run_scenario(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    scenario: Scenario,
    name: &str,
    model: &str,
) -> Result<serde_json::Value> {
    match scenario {
        Scenario::Pong => pong(daemon, scratch, model).await,
        Scenario::Tools => tools(daemon, scratch, model).await,
        Scenario::Permission => permission(daemon, scratch, model).await,
        Scenario::QuestionSingle => question_single(daemon, scratch, model).await,
        Scenario::QuestionMulti => question_multi(daemon, scratch, model).await,
        Scenario::Interrupt => interrupt(daemon, scratch, model).await,
        Scenario::PlanManual => plan(daemon, scratch, model, true).await,
        Scenario::PlanChanges => plan(daemon, scratch, model, false).await,
        Scenario::Compact => compact(daemon, scratch, model).await,
        Scenario::PermissionSession => permission_session(daemon, scratch, model).await,
        Scenario::PermissionDenyFeedback => permission_deny_feedback(daemon, scratch, model).await,
        Scenario::QuestionTabs => question_tabs(daemon, scratch, model).await,
        Scenario::QuestionOtherSingle => question_other_single(daemon, scratch, model).await,
        Scenario::QuestionMixed => question_mixed(daemon, scratch, model).await,
        Scenario::PlanAuto => plan_auto(daemon, scratch, model).await,
        Scenario::ModeCycle => mode_cycle(daemon, scratch, model).await,
        Scenario::PromptMultiline => prompt_multiline(daemon, scratch, model).await,
        Scenario::StaleSeq => stale_seq(daemon, scratch, model).await,
        Scenario::Subscriptions => subscriptions(daemon, scratch, model).await,
        Scenario::ExternalReadonly => external_readonly(daemon, scratch).await,
        Scenario::PermissionMultiProbe => permission_multi_probe(daemon, scratch, model).await,
        Scenario::QuestionChatProbe => question_chat_probe(daemon, scratch, model).await,
    }
    .with_context(|| format!("{name} structural assertion"))
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

fn tool_use_ids(rows: &[harness::Row], name: &str) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.tool_use_id(name))
        .collect()
}

fn tool_result_block<'a>(
    rows: &'a [harness::Row],
    tool_use_id: &str,
) -> Option<(&'a harness::Row, &'a serde_json::Value)> {
    rows.iter().find_map(|row| {
        let blocks = row
            .json
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)?;
        blocks
            .iter()
            .find(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                    && block.get("tool_use_id").and_then(serde_json::Value::as_str)
                        == Some(tool_use_id)
            })
            .map(|block| (row, block))
    })
}

async fn wait_for_plan_resolution(
    session: &CaptureSession,
    from_index: usize,
    outcome: structure::PlanOutcome,
) -> Result<(usize, String)> {
    let what = match outcome {
        structure::PlanOutcome::Approved => "ExitPlanMode approval row",
        structure::PlanOutcome::Rejected => "ExitPlanMode rejection row",
    };
    let index = session
        .wait_for_row(from_index, ASK_TIMEOUT, what, |row| {
            row.plan_resolution()
                .is_some_and(|(_, observed)| observed == outcome)
        })
        .await?;
    let snapshot = session.snapshot().await;
    let parsed: Vec<_> = snapshot.iter().map(|row| row.json.clone()).collect();
    let (_, id) = structure::find_plan_resolution(&parsed, from_index, outcome)
        .ok_or_else(|| anyhow::anyhow!("{what} vanished after structural wait"))?;
    let id = id.to_string();
    Ok((index, id))
}

fn question_tool_input(rows: &[harness::Row]) -> Option<&serde_json::Value> {
    rows.iter().find_map(|row| {
        row.json
            .pointer("/message/content")?
            .as_array()?
            .iter()
            .find(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                    && block.get("name").and_then(serde_json::Value::as_str)
                        == Some("AskUserQuestion")
            })
            .and_then(|block| block.get("input"))
    })
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
    session
        .wait_for_row(index, TURN_TIMEOUT, "system/turn_duration", |row| {
            row.row_type() == "system"
                && row.json.get("subtype").and_then(serde_json::Value::as_str)
                    == Some("turn_duration")
        })
        .await?;
    let rows = session.snapshot().await;
    let prompt = rows.iter().any(|row| {
        row.row_type() == "user"
            && row
                .json
                .pointer("/message/content")
                .and_then(serde_json::Value::as_str)
                .is_some()
    });
    let final_assistant = rows.iter().any(|row| {
        row.row_type() == "assistant"
            && row
                .json
                .pointer("/message/stop_reason")
                .and_then(serde_json::Value::as_str)
                == Some("end_turn")
            && row
                .json
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| !blocks.is_empty())
    });
    if !prompt || !final_assistant {
        bail!("complete-turn assertion failed: prompt={prompt} final_assistant={final_assistant}");
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({
        "keys": keys,
        "assertions": { "human_prompt": prompt, "final_assistant": final_assistant, "turn_duration": true }
    }))
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
    let allow_suggestions = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find(|row| row.row_type() == "hook.permission_request")
        .and_then(|row| row.json.get("permission_suggestions"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    if allow_suggestions != 1 {
        bail!("H.4 verified menu requires one permission suggestion, saw {allow_suggestions}");
    }
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
    let deny_suggestions = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find(|row| row.row_type() == "hook.permission_request")
        .and_then(|row| row.json.get("permission_suggestions"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let deny_digit = deny_suggestions + 2;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            &format!("deny: generated last digit {deny_digit}"),
            vec![Act::Write(deny_digit.to_string().into_bytes())],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "user-rejected denial row", |row| {
            row.json
                .get("toolDenialKind")
                .and_then(serde_json::Value::as_str)
                == Some("user-rejected")
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;

    let rows = session.snapshot().await;
    let ids = tool_use_ids(&rows, "Bash");
    if ids.len() != 2 {
        bail!("expected two Bash tool_use ids, saw {ids:?}");
    }
    let (_, allowed_result) = tool_result_block(&rows, &ids[0])
        .ok_or_else(|| anyhow::anyhow!("allowed Bash has no paired result"))?;
    let (denied_row, denied_result) = tool_result_block(&rows, &ids[1])
        .ok_or_else(|| anyhow::anyhow!("denied Bash has no paired result"))?;
    if allowed_result
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || denied_result
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || denied_row
            .json
            .get("toolDenialKind")
            .and_then(serde_json::Value::as_str)
            != Some("user-rejected")
    {
        bail!("permission resolution facts malformed for tool ids {ids:?}");
    }
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
        "assertions": {
            "suggestion_counts": [allow_suggestions, deny_suggestions],
            "paired_tool_ids": ids,
            "denial_kind": "user-rejected"
        }
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
            row.is_permission_request_for("AskUserQuestion")
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "select option 1 (digit auto-submits)",
            vec![Act::Write(b"1".to_vec())],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "question answers row", |row| {
            row.has_question_answers()
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;
    let rows = session.snapshot().await;
    let input = question_tool_input(&rows)
        .ok_or_else(|| anyhow::anyhow!("missing AskUserQuestion tool input"))?;
    let questions = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("questions is not an array"))?;
    if questions.len() != 1
        || questions[0].get("multiSelect").and_then(|v| v.as_bool()) == Some(true)
    {
        bail!("expected exactly one single-select question, got {questions:?}");
    }
    let question = questions[0]
        .get("question")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("question text missing"))?;
    let option_labels: Vec<_> = questions[0]
        .get("options")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("options is not an array"))?
        .iter()
        .filter_map(|option| option.get("label").and_then(serde_json::Value::as_str))
        .collect();
    let answers = latest_answers(&rows).ok_or_else(|| anyhow::anyhow!("answers object missing"))?;
    let answer = answers.get(question).and_then(serde_json::Value::as_str);
    if answers.len() != 1 || !answer.is_some_and(|answer| option_labels.contains(&answer)) {
        bail!("answer must be keyed by question text and equal one option label: {answers:?}");
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys, "answers": answers }))
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
            row.is_permission_request_for("AskUserQuestion")
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
            row.has_question_answers()
        })
        .await?;
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;

    // Structural assertion: a genuine multi-select-with-Other must record BOTH
    // real selections and the Other free-text value in the parsed answers —
    // otherwise the fixture is a single-select mislabelled as multi.
    let rows = session.snapshot().await;
    let input = question_tool_input(&rows)
        .ok_or_else(|| anyhow::anyhow!("missing AskUserQuestion tool input"))?;
    let questions = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("questions is not an array"))?;
    if questions.len() != 1
        || questions[0]
            .get("multiSelect")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        bail!("expected exactly one multi-select question, got {questions:?}");
    }
    let question = questions[0]
        .get("question")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("question text missing"))?;
    let answers = latest_answers(&rows)
        .ok_or_else(|| anyhow::anyhow!("no toolUseResult.answers found in the capture"))?;
    let joined = answers
        .get(question)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("answers not keyed by question text: {answers:?}"))?;
    if answers.len() != 1 || joined.split(", ").count() < 3 {
        bail!("multi-select must be one comma-joined string with >=3 selections: {answers:?}");
    }
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
    Ok(serde_json::json!({ "keys": keys, "answers": answers, "joined_selection": joined }))
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
            row.json.get("interruptedMessageId").is_some()
        })
        .await?;
    let rows = session.snapshot().await;
    let flushed_ids: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.row_type() == "assistant"
                && row
                    .json
                    .pointer("/message/stop_reason")
                    .is_some_and(serde_json::Value::is_null)
        })
        .filter_map(|row| {
            row.json
                .pointer("/message/id")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    let interrupted_id = rows.iter().find_map(|row| {
        row.json
            .get("interruptedMessageId")
            .and_then(serde_json::Value::as_str)
    });
    if flushed_ids.is_empty() || !interrupted_id.is_some_and(|id| flushed_ids.contains(&id)) {
        bail!(
            "interrupt must pair a null-stop_reason flush with interruptedMessageId: \
             flushed={flushed_ids:?} interrupted={interrupted_id:?}"
        );
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({
        "keys": keys,
        "assertions": { "null_stop_reason_message_ids": flushed_ids, "interrupted_message_id": interrupted_id }
    }))
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
            row.is_permission_request_for("ExitPlanMode")
        })
        .await?;
    let original_plan_input = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find(|row| row.is_permission_request_for("ExitPlanMode"))
        .and_then(|row| row.json.get("tool_input"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ExitPlanMode hook tool_input missing"))?;
    // The permission hook is the menu-readiness fact. Claude is allowed to
    // withhold the assistant tool_use transcript row until the menu resolves;
    // waiting for that row before answering deadlocks. The typed result after
    // input supplies the correlation id, and the final assertions pair it to
    // the assistant row once the transcript flushes.
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
        let _ = wait_for_plan_resolution(&session, index, structure::PlanOutcome::Approved).await?;
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
        let (index, _) =
            wait_for_plan_resolution(&session, index, structure::PlanOutcome::Rejected).await?;
        // Request-changes is not a turn end. End only after the rejection
        // facts and a subsequent, newly emitted ask are both observed. The
        // next ask can be a revised plan or a clarification question.
        session
            .wait_for_row(index, ASK_TIMEOUT, "new ask after plan rejection", |row| {
                row.row_type() == "hook.permission_request"
                    && (row
                        .json
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        != Some("ExitPlanMode")
                        || row.json.get("tool_input") != Some(&original_plan_input))
            })
            .await?;
        session
            .send_keys("end scenario: Esc", vec![Act::Write(b"\x1b".to_vec())])
            .await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let rows = session.snapshot().await;
    let plan_ids = tool_use_ids(&rows, "ExitPlanMode");
    let expected_outcome = if approve {
        structure::PlanOutcome::Approved
    } else {
        structure::PlanOutcome::Rejected
    };
    let first_id = rows
        .iter()
        .find_map(|row| {
            row.plan_resolution()
                .and_then(|(id, outcome)| (outcome == expected_outcome).then_some(id))
        })
        .ok_or_else(|| anyhow::anyhow!("ExitPlanMode typed result missing"))?;
    if !plan_ids.iter().any(|id| id == first_id) {
        bail!("ExitPlanMode assistant tool_use did not pair to result {first_id}");
    }
    let (result_row, result) = tool_result_block(&rows, first_id)
        .ok_or_else(|| anyhow::anyhow!("ExitPlanMode result did not pair to {first_id}"))?;
    if approve {
        if result.get("is_error").and_then(serde_json::Value::as_bool) == Some(true)
            || !result_row
                .json
                .get("toolUseResult")
                .is_some_and(serde_json::Value::is_object)
        {
            bail!("manual approval result is not a successful typed result");
        }
    } else if result.get("is_error").and_then(serde_json::Value::as_bool) != Some(true)
        || result_row
            .json
            .get("toolDenialKind")
            .and_then(serde_json::Value::as_str)
            != Some("user-rejected")
        || result_row
            .json
            .get("userFeedback")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || !rows
            .iter()
            .skip_while(|row| !row.is_tool_result_for(first_id))
            .skip(1)
            .any(|row| {
                row.row_type() == "hook.permission_request"
                    && (row
                        .json
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        != Some("ExitPlanMode")
                        || row.json.get("tool_input") != Some(&original_plan_input))
            })
    {
        bail!("request-changes must carry denial+feedback and lead to a new ask");
    }
    let new_ask_tool = (!approve).then(|| {
        rows.iter()
            .skip_while(|row| !row.is_tool_result_for(first_id))
            .skip(1)
            .find(|row| {
                row.row_type() == "hook.permission_request"
                    && (row
                        .json
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        != Some("ExitPlanMode")
                        || row.json.get("tool_input") != Some(&original_plan_input))
            })
            .and_then(|row| row.json.get("tool_name"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    let keys = session.close().await?;
    Ok(serde_json::json!({
        "keys": keys,
        "assertions": {
            "exit_plan_mode_ids": plan_ids,
            "approved": approve,
            "rejection_facts_observed": !approve,
            "new_ask_after_rejection": new_ask_tool.flatten()
        }
    }))
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
            row.row_type() == "system"
                && row.json.get("subtype").and_then(serde_json::Value::as_str)
                    == Some("compact_boundary")
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
    let command_permissions = rows.iter().any(|row| {
        row.row_type() == "attachment"
            && row
                .json
                .pointer("/attachment/type")
                .and_then(serde_json::Value::as_str)
                == Some("command_permissions")
    });
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
    let suggestion_count = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find(|row| row.row_type() == "hook.permission_request")
        .and_then(|row| row.json.get("permission_suggestions"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    if suggestion_count != 1 {
        bail!(
            "verified feedback program requires one permission suggestion, saw {suggestion_count}"
        );
    }
    let deny_digit = suggestion_count + 2;
    let mut paste = Vec::new();
    paste.extend_from_slice(b"\x1b[200~");
    paste.extend_from_slice(feedback.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    session
        .send_keys(
            "deny with feedback, one program: generated last digit, settle, paste feedback, Enter",
            vec![
                Act::Write(deny_digit.to_string().into_bytes()),
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
                |row| row.has_question_answers(),
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
            row.is_permission_request_for("AskUserQuestion")
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
            row.is_permission_request_for("AskUserQuestion")
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
            row.is_permission_request_for("AskUserQuestion")
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
            row.is_permission_request_for("ExitPlanMode")
        })
        .await?;
    // Do not wait for the assistant row here: current Claude builds can hold
    // it until the blocking plan menu receives an answer.
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "plan approve (auto): digit 1",
            vec![Act::Write(b"1".to_vec())],
        )
        .await?;
    let (index, plan_tool_id) =
        wait_for_plan_resolution(&session, index, structure::PlanOutcome::Approved).await?;
    // Under auto acceptance claude proceeds to write README.md WITHOUT a
    // permission ask; the turn end closes the scenario and its hook.stop
    // payload carries the effective permission_mode.
    let _ = session.wait_for_turn_end(index, TURN_TIMEOUT).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let rows = session.snapshot().await;
    if !rows
        .iter()
        .any(|row| row.tool_use_id("ExitPlanMode").as_deref() == Some(&plan_tool_id))
    {
        bail!("ExitPlanMode assistant tool_use did not pair to result {plan_tool_id}");
    }
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
    let (approval_row, approval) = tool_result_block(&rows, &plan_tool_id)
        .ok_or_else(|| anyhow::anyhow!("ExitPlanMode approval did not pair to {plan_tool_id}"))?;
    let sidecar = approval_row
        .json
        .get("toolUseResult")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("ExitPlanMode approval sidecar missing"))?;
    if approval
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || !["filePath", "isAgent", "plan"]
            .iter()
            .all(|field| sidecar.contains_key(*field))
    {
        bail!("auto approval result/sidecar has the wrong structural shape");
    }
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
        "assertions": { "exit_plan_mode_id": plan_tool_id, "typed_approval_sidecar": true },
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

async fn wait_runtime(
    runtime: &mut amux_ui::Runtime,
    timeout: Duration,
    what: &str,
    ready: impl Fn(&amux_ui::Model) -> bool,
) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    while !ready(runtime.model()) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("timed out after {timeout:?} waiting for runtime {what}");
        }
        if !tokio::time::timeout(remaining, runtime.next())
            .await
            .context("runtime wait timeout")?
        {
            bail!("runtime stopped while waiting for {what}");
        }
    }
    Ok(())
}

/// H.7 — make the reducer's cursor genuinely stale against a real daemon,
/// then prove the typed SequenceNumberMismatch becomes a stated, retryable
/// ask instead of a panic or stuck optimistic marker.
async fn stale_seq(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    use amux_ui::claude::AskState;
    use amux_ui::claude::encoding::{AskAnswer, PermissionAnswer};
    use amux_ui::{Command, OpOutcome, Runtime, RuntimeOptions};

    let (session, index) = open(
        daemon,
        scratch,
        "stale_seq",
        &[],
        model,
        "Use the Bash tool to run exactly: echo stale-race > stale.txt. Then stop.",
    )
    .await?;
    session
        .wait_for_row(index, ASK_TIMEOUT, "permission request", |row| {
            row.row_type() == "hook.permission_request"
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;

    let agent = session.agent_id();
    let mut runtime = Runtime::start_with_client(daemon.client.clone(), RuntimeOptions::default());
    wait_runtime(&mut runtime, READY_TIMEOUT, "agent inventory", |model| {
        model.agent(agent).is_some()
    })
    .await?;
    runtime.note_attached(agent);
    wait_runtime(
        &mut runtime,
        READY_TIMEOUT,
        "pending permission ask",
        |model| {
            model
                .claude(agent)
                .and_then(|layer| layer.ask_head())
                .is_some()
        },
    )
    .await?;
    let (ask, captured_seq) = {
        let layer = runtime.model().claude(agent).expect("waited for layer");
        (layer.ask_head().expect("waited for ask").id, layer.cursor())
    };

    // Advance the daemon's structured seq through the same hook CLI seam,
    // while deliberately not folding the queued Stream Msg in Runtime.
    let hook_row = session
        .snapshot()
        .await
        .into_iter()
        .rev()
        .find(|row| row.row_type() == "hook.permission_request")
        .ok_or_else(|| anyhow::anyhow!("permission hook vanished"))?;
    let notification = serde_json::json!({
        "hook_event_name": "Notification",
        "notification_type": "h7_stale_seq_probe",
        "message": "structural probe",
        "session_id": hook_row.json.get("session_id"),
        "transcript_path": hook_row.json.get("transcript_path"),
        "cwd": hook_row.json.get("cwd"),
    });
    daemon.deliver_hook(scratch, Some(agent), &notification)?;
    session
        .wait_for_row(0, ASK_TIMEOUT, "stale-seq notification row", |row| {
            row.row_type() == "hook.notification"
                && row
                    .json
                    .get("notification_type")
                    .and_then(serde_json::Value::as_str)
                    == Some("h7_stale_seq_probe")
        })
        .await?;
    let advanced_seq = session.current_seq().await;
    if advanced_seq <= captured_seq {
        bail!("daemon seq did not advance: captured={captured_seq} current={advanced_seq}");
    }

    let first = runtime.dispatch(Command::AnswerAsk {
        agent,
        ask,
        answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
    });
    wait_runtime(&mut runtime, ASK_TIMEOUT, "stale answer outcome", |model| {
        model.finished_op(first).is_some()
    })
    .await?;
    let error = match &runtime
        .model()
        .finished_op(first)
        .expect("waited for outcome")
        .outcome
    {
        OpOutcome::Error { error } => error.message.clone(),
        other => bail!("genuinely stale positional answer unexpectedly succeeded: {other:?}"),
    };
    if !error.contains("input raced the session") || !error.contains("sequence number mismatch") {
        bail!("typed SequenceNumberMismatch was not stated clearly: {error}");
    }
    let resurfaced = runtime
        .model()
        .claude(agent)
        .and_then(|layer| layer.ask_head())
        .ok_or_else(|| anyhow::anyhow!("stale failure did not resurface the ask"))?;
    if !matches!(&resurfaced.state, AskState::SendFailed { message } if message == &error) {
        bail!(
            "resurfaced ask did not retain the failure: {:?}",
            resurfaced.state
        );
    }

    // The daemon-side hook delivery and Runtime stream folding are independent
    // tasks. Do not dispatch the retry until the advanced batch is folded:
    // otherwise this scenario can reuse captured_seq and race into a second
    // stale failure even though the production retry behavior is correct.
    wait_runtime(
        &mut runtime,
        ASK_TIMEOUT,
        "advanced stale-seq batch",
        |model| {
            model
                .claude(agent)
                .is_some_and(|layer| layer.cursor() >= advanced_seq)
        },
    )
    .await?;

    // The retry is a fresh typed command using the folded notification's
    // cursor. It must be accepted and resolve the real permission menu.
    let retry = runtime.dispatch(Command::AnswerAsk {
        agent,
        ask,
        answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
    });
    wait_runtime(&mut runtime, ASK_TIMEOUT, "fresh retry outcome", |model| {
        model.finished_op(retry).is_some()
    })
    .await?;
    if runtime.model().finished_op(retry).map(|op| &op.outcome) != Some(&OpOutcome::InputSent) {
        bail!("fresh answer retry was not accepted");
    }
    session
        .wait_for_row(0, TURN_TIMEOUT, "allowed Bash result", |row| {
            row.is_tool_result()
                && row
                    .json
                    .pointer("/message/content/0/is_error")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
        })
        .await?;
    let _ = session.wait_for_turn_end(0, TURN_TIMEOUT).await;
    let exists = scratch.projects.join("stale_seq/stale.txt").exists();
    if !exists {
        bail!("world assertion failed: retry did not create stale.txt");
    }
    drop(runtime);
    let keys = session.close().await?;
    Ok(serde_json::json!({
        "keys": keys,
        "world": { "stale.txt": exists },
        "assertions": {
            "captured_seq": captured_seq,
            "advanced_seq": advanced_seq,
            "typed_mismatch": error,
            "ask_resurfaced": true,
            "fresh_retry": "input_sent"
        }
    }))
}

/// Wait specifically for the transcript authority, not the earlier
/// arrival-ordered hook.stop.
async fn wait_for_turn_duration(session: &CaptureSession, from: usize) -> Result<usize> {
    session
        .wait_for_row(from, TURN_TIMEOUT, "system/turn_duration", |row| {
            row.row_type() == "system"
                && row.json.get("subtype").and_then(serde_json::Value::as_str)
                    == Some("turn_duration")
        })
        .await
}

/// H.8 — keep raw and structured subscriptions open while each input path
/// drives a turn; both observers must continue advancing.
async fn subscriptions(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "subscriptions",
        &[],
        model,
        "Reply with exactly FIRST and nothing else.",
    )
    .await?;
    let mut cursor = wait_for_turn_duration(&session, index).await?;
    let raw_after_first = session.raw_len().await;
    let seq_after_first = session.current_seq().await;

    let raw_prompt = "Reply with exactly RAW and nothing else.";
    session
        .send_raw(format!("{raw_prompt}\r").as_bytes())
        .await?;
    cursor = session
        .wait_for_row(cursor, TURN_TIMEOUT, "raw-path prompt row", |row| {
            row.row_type() == "user"
                && row
                    .json
                    .pointer("/message/content")
                    .and_then(serde_json::Value::as_str)
                    == Some(raw_prompt)
        })
        .await?;
    cursor = wait_for_turn_duration(&session, cursor).await?;
    let raw_after_raw = session.raw_len().await;
    let seq_after_raw = session.current_seq().await;

    let chat_prompt = "Reply with exactly CHAT and nothing else.";
    session.send_prompt(chat_prompt).await?;
    cursor = session
        .wait_for_row(cursor, TURN_TIMEOUT, "chat-path prompt row", |row| {
            row.row_type() == "user"
                && row
                    .json
                    .pointer("/message/content")
                    .and_then(serde_json::Value::as_str)
                    == Some(chat_prompt)
        })
        .await?;
    wait_for_turn_duration(&session, cursor).await?;
    let raw_after_chat = session.raw_len().await;
    let seq_after_chat = session.current_seq().await;
    let streams_open = session.streams_open();
    if !(streams_open
        && raw_after_first < raw_after_raw
        && raw_after_raw < raw_after_chat
        && seq_after_first < seq_after_raw
        && seq_after_raw < seq_after_chat)
    {
        bail!(
            "subscription disturbance: open={streams_open} raw={raw_after_first}/{raw_after_raw}/{raw_after_chat} \
             seq={seq_after_first}/{seq_after_raw}/{seq_after_chat}"
        );
    }
    let keys = session.close().await?;
    Ok(serde_json::json!({
        "keys": keys,
        "assertions": {
            "streams_open": streams_open,
            "raw_bytes": [raw_after_first, raw_after_raw, raw_after_chat],
            "structured_seq": [seq_after_first, seq_after_raw, seq_after_chat]
        }
    }))
}

/// H.9 — exercise external hook discovery without launching or configuring
/// Claude. The transcript and hook CLI both live under the scratch daemon;
/// no file beneath the user's real ~/.claude is read or written.
async fn external_readonly(daemon: &ScratchDaemon, scratch: &Scratch) -> Result<serde_json::Value> {
    use amux::claude_io::{PTY_TRANSCRIPT_V1, decode_pty_transcript_v1_output};
    use amux::{
        AgentIdentifier, ClientError, ProtocolError, SendInputRequest, SubscribeSessionEvent,
        SubscribeSessionRequest,
    };
    use uuid::Uuid;

    let cwd = scratch.project_dir("external_readonly")?;
    let external_dir = scratch.root.join("external-session");
    std::fs::create_dir_all(&external_dir)?;
    let transcript = external_dir.join("transcript.jsonl");
    let session_id = Uuid::new_v4();
    let row_id = Uuid::new_v4();
    let transcript_row = serde_json::json!({
        "type": "user",
        "uuid": row_id,
        "parentUuid": null,
        "sessionId": session_id,
        "isSidechain": false,
        "cwd": cwd,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "version": "h9-synthetic",
        "message": { "role": "user", "content": "H9 scratch observation" }
    });
    std::fs::write(&transcript, format!("{}\n", transcript_row))?;
    let start = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "transcript_path": transcript,
        "cwd": cwd,
    });
    daemon.deliver_hook(scratch, None, &start)?;

    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let external = loop {
        if let Some(agent) = daemon
            .client
            .list_agents()
            .await?
            .into_iter()
            .find(|agent| agent.id == session_id)
        {
            break agent;
        }
        if std::time::Instant::now() > deadline {
            bail!("hook-discovered external agent never entered inventory");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if !external.readonly
        || !external
            .io_protocols
            .iter()
            .any(|protocol| protocol == PTY_TRANSCRIPT_V1)
        || external
            .io_protocols
            .iter()
            .any(|protocol| protocol == amux::claude_io::RAW_V1)
    {
        bail!("external inventory shape is not structured-only readonly: {external:?}");
    }

    let mut stream = daemon
        .client
        .subscribe_session(SubscribeSessionRequest {
            agent: AgentIdentifier::Id(session_id),
            io_protocol: PTY_TRANSCRIPT_V1.to_string(),
            args: None,
        })
        .await?;
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut captured = Vec::new();
    let mut ready = false;
    let mut observed_prompt = false;
    let mut latest_seq = 0;
    while !(ready && observed_prompt) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("readonly stream did not replay ready+prompt");
        }
        let event = tokio::time::timeout(remaining, stream.recv()).await??;
        match event {
            SubscribeSessionEvent::Output { payload } => {
                let output = decode_pty_transcript_v1_output(&payload)?;
                latest_seq = output.seq_id;
                let raw = String::from_utf8(output.payload)?;
                let row: serde_json::Value = serde_json::from_str(&raw)?;
                ready |= row.get("type").and_then(serde_json::Value::as_str)
                    == Some("amux.transcript_ready");
                observed_prompt |= row
                    .pointer("/message/content")
                    .and_then(serde_json::Value::as_str)
                    == Some("H9 scratch observation");
                captured.push(raw);
            }
            SubscribeSessionEvent::Closed { reason } => {
                bail!("readonly stream closed during replay: {reason:?}")
            }
            _ => {}
        }
    }
    std::fs::write(
        scratch.out.join("external_readonly.rows.jsonl"),
        format!("{}\n", captured.join("\n")),
    )?;

    let payload = amux::claude_io::encode_pty_transcript_v1_input(
        amux::claude_io::ClaudePtyTranscriptV1Input {
            expected_seq: latest_seq,
            actions: vec![Act::Write(b"x".to_vec())],
        },
    );
    let readonly_error = daemon
        .client
        .send_input(SendInputRequest {
            agent: AgentIdentifier::Id(session_id),
            io_protocol: PTY_TRANSCRIPT_V1.to_string(),
            payload: payload.into(),
        })
        .await
        .expect_err("readonly session accepted input");
    if !matches!(
        &readonly_error,
        ClientError::Protocol(ProtocolError::ServerError { message }) if message == "session is readonly"
    ) {
        bail!("readonly input returned the wrong typed error: {readonly_error}");
    }

    let end = serde_json::json!({
        "hook_event_name": "SessionEnd",
        "session_id": session_id,
        "transcript_path": transcript,
        "cwd": cwd,
    });
    daemon.deliver_hook(scratch, None, &end)?;
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        if !daemon
            .client
            .list_agents()
            .await?
            .iter()
            .any(|agent| agent.id == session_id)
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            bail!("SessionEnd did not withdraw readonly external agent");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(serde_json::json!({
        "keys": [],
        "assertions": {
            "scratch_only_setup": true,
            "readonly": true,
            "structured_replay": true,
            "raw_absent": true,
            "input_refused": readonly_error.to_string(),
            "session_end_withdrew": true
        }
    }))
}

/// Non-gating capture probe for the permission digit table's open >=2-
/// suggestion shape. A successful capture is the prerequisite for extending
/// amux-ui's encoder; failure preserves the capture and changes no encoding.
async fn permission_multi_probe(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "permission_multi_probe",
        &[],
        model,
        "Use one Bash tool call to run exactly: git status && echo multi-probe > probe.txt. Then stop.",
    )
    .await?;
    session
        .wait_for_row(index, ASK_TIMEOUT, "permission request", |row| {
            row.row_type() == "hook.permission_request"
        })
        .await?;
    let suggestions = session
        .snapshot()
        .await
        .iter()
        .rev()
        .find(|row| row.row_type() == "hook.permission_request")
        .and_then(|row| row.json.get("permission_suggestions"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if suggestions.len() < 2 {
        bail!(
            "probe produced {} suggestion(s), need >=2",
            suggestions.len()
        );
    }
    tokio::time::sleep(MENU_SETTLE).await;
    session
        .send_keys(
            "deny generated >=2-suggestion menu",
            vec![Act::Write((suggestions.len() + 2).to_string().into_bytes())],
        )
        .await?;
    session
        .wait_for_row(index, ASK_TIMEOUT, "typed denial", |row| {
            row.json
                .get("toolDenialKind")
                .and_then(serde_json::Value::as_str)
                == Some("user-rejected")
        })
        .await?;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys, "permission_suggestions": suggestions }))
}

/// Non-gating probe for Claude's appended `Chat about this` question option.
/// It records the typed tool result shape; no client intent is added until a
/// successful, reviewed capture provides provenance.
async fn question_chat_probe(
    daemon: &ScratchDaemon,
    scratch: &Scratch,
    model: &str,
) -> Result<serde_json::Value> {
    let (mut session, index) = open(
        daemon,
        scratch,
        "question_chat_probe",
        &[],
        model,
        "Use AskUserQuestion to ask one single-select question with exactly two options. Header: Color. Question: Which color? Options: Red, Blue.",
    )
    .await?;
    session
        .wait_for_row(index, ASK_TIMEOUT, "AskUserQuestion request", |row| {
            row.row_type() == "hook.permission_request"
                && row
                    .json
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    == Some("AskUserQuestion")
        })
        .await?;
    tokio::time::sleep(MENU_SETTLE).await;
    let discussion = "I want to discuss the tradeoffs before choosing.";
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend_from_slice(discussion.as_bytes());
    paste.extend_from_slice(b"\x1b[201~");
    session
        .send_keys(
            "Chat about this: digit options+2, then a discussion prompt",
            vec![
                Act::Write(b"4".to_vec()),
                Act::DelayMs(1200),
                Act::Write(paste),
                Act::DelayMs(400),
                Act::Write(b"\r".to_vec()),
            ],
        )
        .await?;
    let index = session
        .wait_for_row(index, ASK_TIMEOUT, "AskUserQuestion result", |row| {
            row.json.get("toolUseResult").is_some() && row.is_tool_result()
        })
        .await?;
    let rows = session.snapshot().await;
    let result = rows
        .iter()
        .skip(index.saturating_sub(1))
        .find_map(|row| row.json.get("toolUseResult"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("question result shape vanished"))?;
    let keys = session.close().await?;
    Ok(serde_json::json!({ "keys": keys, "toolUseResult": result }))
}
