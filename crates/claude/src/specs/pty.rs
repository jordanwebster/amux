//! Executable specifications for Claude Code's interactive PTY boundary.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use replay_support::{IoDirection, IoEvent, Manifest, ReplayReport, SpecEntry, StrictReplay};
use tokio::sync::{mpsc, watch};

use super::{ALLOWED_MODELS, HAIKU, SpecFailure};
use crate::launch::Launch;
use crate::pty::keymap::KeymapSources;
use crate::pty::{
    AskAnswer, AskFacts, AskKind, Intent, PermissionAnswer, PlanAnswer, PtyEvent, QuestionAnswer,
    QuestionResponse, RelinkReason,
};

const DEFAULT_WAIT: Duration = Duration::from_secs(600);
const WAIT_ENV: &str = "CLAUDE_PTY_SPEC_TIMEOUT_SECS";
const STALL_DIAGNOSTIC_ENV: &str = "CLAUDE_PTY_STALL_DIAGNOSTIC";
const SONNET: &str = "claude-sonnet-5";
const SONNET_FALLBACK_SPECS: &[&str] = &[
    "plan_approve",
    "plan_auto",
    "plan_request_changes",
    "question_mixed",
];

type SpecFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

struct PtySpecDef {
    entry: SpecEntry,
    args: &'static [&'static str],
    run: for<'a> fn(&'a mut PtySpecSession) -> SpecFuture<'a>,
}

const fn entry(name: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording: name,
        allowed_models: ALLOWED_MODELS,
    }
}

macro_rules! definition {
    ($name:ident, $args:expr, $run:ident) => {
        PtySpecDef {
            entry: entry(stringify!($name)),
            args: $args,
            run: |session| Box::pin($run(session)),
        }
    };
}

static DEFINITIONS: &[PtySpecDef] = &[
    definition!(prompt, &[], prompt),
    definition!(prompt_multiline, &[], prompt_multiline),
    definition!(tools, &["--dangerously-skip-permissions"], tools),
    definition!(permission_allow_once, &[], permission_allow_once),
    definition!(permission_allow_scoped, &[], permission_allow_scoped),
    definition!(permission_deny_feedback, &[], permission_deny_feedback),
    definition!(plan_approve, &["--permission-mode", "plan"], plan_approve),
    definition!(plan_auto, &["--permission-mode", "plan"], plan_auto),
    definition!(
        plan_request_changes,
        &["--permission-mode", "plan"],
        plan_request_changes
    ),
    definition!(question_single, &[], question_single),
    definition!(question_multi_other, &[], question_multi_other),
    definition!(question_mixed, &[], question_mixed),
    definition!(question_tabs, &[], question_tabs),
    definition!(question_other_single, &[], question_other_single),
    definition!(interrupt, &["--dangerously-skip-permissions"], interrupt),
    definition!(mode_cycle, &[], mode_cycle),
    definition!(compact_relink, &[], compact_relink),
    definition!(clear_relink, &[], clear_relink),
];

static REGISTRY: [SpecEntry; 18] = [
    entry("prompt"),
    entry("prompt_multiline"),
    entry("tools"),
    entry("permission_allow_once"),
    entry("permission_allow_scoped"),
    entry("permission_deny_feedback"),
    entry("plan_approve"),
    entry("plan_auto"),
    entry("plan_request_changes"),
    entry("question_single"),
    entry("question_multi_other"),
    entry("question_mixed"),
    entry("question_tabs"),
    entry("question_other_single"),
    entry("interrupt"),
    entry("mode_cycle"),
    entry("compact_relink"),
    entry("clear_relink"),
];

pub fn registry() -> &'static [SpecEntry] {
    &REGISTRY
}

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/pty")
}

pub fn baked_keymap_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("keymaps/claude-2.1.toml")
}

/// Apply the launch arguments owned by a PTY specification.
pub fn prepare_launch(entry: &SpecEntry, launch: &mut Launch) -> Result<(), SpecFailure> {
    let definition = definition_for(entry)?;
    launch
        .args
        .extend(definition.args.iter().map(|arg| (*arg).to_owned()));
    if !launch.args.iter().any(|arg| arg == "--model") {
        let model = if SONNET_FALLBACK_SPECS.contains(&entry.name) {
            SONNET
        } else {
            HAIKU
        };
        launch.args.extend(["--model".to_owned(), model.to_owned()]);
    }
    Ok(())
}

pub enum Source {
    Live {
        launch: Launch,
        keymaps: KeymapSources,
        size: pty_host::PtySize,
    },
    Recorded {
        replay: StrictReplay,
        manifest: Box<Manifest>,
        keymaps: KeymapSources,
    },
}

#[derive(Debug)]
pub struct RunReport {
    pub provider_version: semver::Version,
    pub model: String,
    pub session_id: String,
    pub io: Vec<IoEvent>,
    pub replay: Option<ReplayReport>,
}

pub async fn run(entry: &SpecEntry, source: Source) -> Result<RunReport, SpecFailure> {
    let definition = definition_for(entry)?;
    let wait = claim_wait(entry)?;
    let (session, capture, controller, provider_version, model, session_id) = match source {
        Source::Live {
            mut launch,
            keymaps,
            size,
        } => {
            prepare_launch(entry, &mut launch)?;
            let model = launch
                .args
                .windows(2)
                .find_map(|pair| (pair[0] == "--model").then(|| pair[1].clone()))
                .ok_or_else(|| failure(entry, "PTY specification launch omitted --model"))?;
            if !entry.allowed_models.contains(&model.as_str()) {
                return Err(failure(entry, format!("model {model} is not allowed")));
            }
            let provider_version = crate::version::probe_version(&launch.binary)
                .await
                .map_err(|error| failure(entry, error.to_string()))?
                .0;
            let session_id = launch.session_id.to_string();
            let session = crate::pty::spawn(&launch, &keymaps, size)
                .await
                .map_err(|error| failure(entry, error.to_string()))?;
            (
                session,
                Some(Capture::new()),
                None,
                provider_version,
                model,
                session_id,
            )
        }
        Source::Recorded {
            mut replay,
            manifest,
            keymaps,
        } => {
            let manifest = *manifest;
            let controller = replay.controller.clone();
            let session = crate::pty::from_recording(&mut replay, &manifest, &keymaps)
                .map_err(|error| failure(entry, error.to_string()))?;
            if !replay.transports.is_empty() {
                return Err(failure(entry, "recording has undeclared extra transports"));
            }
            (
                session,
                None,
                Some(controller),
                manifest.recorded.version,
                manifest.recorded.model,
                manifest
                    .session_ids
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "recorded-session".to_owned()),
            )
        }
    };

    let mut session = PtySpecSession::new(session, capture.clone());
    // Keep the driver in this future so failed or cancelled specifications
    // cannot leave a detached replay task waiting for a write.
    let (claim, driver_result) = {
        let claim = tokio::time::timeout(wait, async {
            session.prepare_for_prompt().await?;
            (definition.run)(&mut session).await
        });
        let driver = async {
            if let Some(controller) = &controller {
                controller.drive().await;
            }
        };
        tokio::pin!(claim, driver);
        let mut driver_finished = false;
        let claim = tokio::select! {
            result = &mut claim => result,
            () = &mut driver => {
                driver_finished = true;
                claim.await
            }
        };
        let driver_result = if matches!(claim, Ok(Ok(()))) && !driver_finished {
            tokio::time::timeout(Duration::from_secs(5), driver)
                .await
                .map_err(|_| failure(entry, "strict replay driver did not finish"))
        } else {
            Ok(())
        };
        (claim, driver_result)
    };
    if capture.is_some() && matches!(claim, Ok(Ok(()))) {
        session.drain_quiet().await;
    }
    if capture.is_some() && claim.is_err() {
        // The timed-out future is gone, so drain anything already queued before
        // killing the provider and freezing the diagnostic capture.
        session.drain_quiet().await;
    }
    let timeout_tail = claim.is_err().then(|| session.screen_tail());
    let close_result = if let Some(controller) = &controller {
        controller
            .close_reads()
            .await
            .map_err(|error| failure(entry, format!("closing recorded streams failed: {error}")))
    } else {
        Ok(())
    };
    let shutdown = tokio::time::timeout(
        Duration::from_secs(5),
        session.control.stop(pty_host::Terminate::Kill),
    )
    .await;
    let stall_diagnostic = if claim.is_err() {
        capture
            .as_ref()
            .map(|capture| persist_stall_diagnostic(entry, &capture.events()))
    } else {
        None
    };
    claim
        .map_err(|_| {
            let diagnostic = match stall_diagnostic {
                Some(Ok(path)) => format!("; diagnostic={}", path.display()),
                Some(Err(error)) => format!("; diagnostic write failed: {error}"),
                None => "; diagnostic unavailable for recorded source".to_owned(),
            };
            failure(
                entry,
                format!(
                    "stalled after {wait:?}{diagnostic}; terminal tail={:?}",
                    timeout_tail.unwrap_or_default()
                ),
            )
        })?
        .map_err(|claim| failure(entry, claim))?;
    driver_result?;
    close_result?;
    shutdown.map_err(|_| failure(entry, "PTY session did not exit during cleanup"))?;
    let replay =
        controller.map(|controller| controller.finish().unwrap_or_else(|error| error.report));
    if replay.as_ref().is_some_and(|report| !report.is_complete()) {
        return Err(failure(
            entry,
            format!("strict replay incomplete: {replay:?}"),
        ));
    }
    Ok(RunReport {
        provider_version,
        model,
        session_id,
        io: capture.map_or_else(Vec::new, |capture| capture.events()),
        replay,
    })
}

fn claim_wait(entry: &SpecEntry) -> Result<Duration, SpecFailure> {
    let Some(raw) = std::env::var_os(WAIT_ENV) else {
        return Ok(DEFAULT_WAIT);
    };
    let raw = raw
        .to_str()
        .ok_or_else(|| failure(entry, format!("{WAIT_ENV} is not UTF-8")))?;
    let seconds = raw.parse::<u64>().map_err(|error| {
        failure(
            entry,
            format!("{WAIT_ENV} must be a positive integer: {error}"),
        )
    })?;
    if seconds == 0 {
        return Err(failure(
            entry,
            format!("{WAIT_ENV} must be a positive integer"),
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn persist_stall_diagnostic(entry: &SpecEntry, events: &[IoEvent]) -> std::io::Result<PathBuf> {
    let path = std::env::var_os(STALL_DIAGNOSTIC_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "claude-pty-{}-stall-{}.jsonl",
                entry.name,
                std::process::id()
            ))
        });
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = String::new();
    for event in events {
        let mut row = serde_json::json!({
            "us": event.us,
            "dir": match event.direction {
                IoDirection::Write => "stdin",
                IoDirection::Read => "stdout",
            },
            "line": event.line,
        });
        if let Some(transport_id) = &event.transport_id {
            row["transport_id"] = transport_id.clone().into();
        }
        if let Some(session_id) = &event.session_id {
            row["session_id"] = session_id.clone().into();
        }
        output.push_str(&serde_json::to_string(&row).map_err(std::io::Error::other)?);
        output.push('\n');
    }
    std::fs::write(&path, output)?;
    Ok(path)
}

fn definition_for(entry: &SpecEntry) -> Result<&'static PtySpecDef, SpecFailure> {
    DEFINITIONS
        .iter()
        .find(|definition| definition.entry == *entry)
        .ok_or_else(|| failure(entry, "specification is not in the Claude PTY registry"))
}

fn failure(entry: &SpecEntry, claim: impl Into<String>) -> SpecFailure {
    SpecFailure {
        spec: entry.name.to_owned(),
        claim: claim.into(),
    }
}

#[derive(Clone)]
struct Capture {
    start: Instant,
    events: Arc<Mutex<Vec<IoEvent>>>,
}

impl Capture {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push(&self, transport: &str, direction: IoDirection, line: String) {
        self.events
            .lock()
            .expect("capture mutex poisoned")
            .push(IoEvent {
                us: self.start.elapsed().as_micros() as u64,
                direction,
                line,
                transport_id: Some(transport.to_owned()),
                session_id: None,
            });
    }

    fn events(&self) -> Vec<IoEvent> {
        let mut events = self.events.lock().expect("capture mutex poisoned").clone();
        events.sort_by_key(|event| event.us);
        events
    }
}

struct PtySpecSession {
    events: crate::pty::EventStream,
    control: crate::pty::Control,
    capture: Option<Capture>,
    screen: Arc<Mutex<String>>,
    screen_changes: watch::Receiver<()>,
    pending: VecDeque<PtyEvent>,
    tool_use_ids: HashMap<String, String>,
}

impl PtySpecSession {
    fn new(session: crate::pty::Session, capture: Option<Capture>) -> Self {
        if let Some(capture) = capture.clone() {
            let (write_tx, mut write_rx) = mpsc::unbounded_channel();
            session.control.observe_writes(write_tx);
            let writes = capture.clone();
            tokio::spawn(async move {
                while let Some(bytes) = write_rx.recv().await {
                    writes.push(
                        "pty",
                        IoDirection::Write,
                        crate::pty::encode_recording_bytes(&bytes),
                    );
                }
            });
        }
        let screen = Arc::new(Mutex::new(String::new()));
        let (screen_changed, screen_changes) = watch::channel(());
        if let Some(mut output) = session.control.terminal_output() {
            let captured = capture.clone();
            let observed_screen = Arc::clone(&screen);
            tokio::spawn(async move {
                while let Some(bytes) = output.recv().await {
                    if let Some(capture) = &captured {
                        capture.push(
                            "pty",
                            IoDirection::Read,
                            crate::pty::encode_recording_bytes(&bytes),
                        );
                    }
                    let mut screen = observed_screen.lock().expect("screen mutex poisoned");
                    screen.push_str(&String::from_utf8_lossy(&bytes));
                    if screen.len() > 256 * 1024 {
                        let cut = screen.len() - 128 * 1024;
                        screen.drain(..cut);
                    }
                    screen_changed.send_replace(());
                }
            });
        }
        Self {
            events: session.events,
            control: session.control,
            capture,
            screen,
            screen_changes,
            pending: VecDeque::new(),
            tool_use_ids: HashMap::new(),
        }
    }

    async fn prepare_for_prompt(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut trust_answered = false;
        loop {
            let screen = self.screen.lock().expect("screen mutex poisoned").clone();
            let composer_up = screen.contains("for agents")
                || (screen.contains("Try") && screen.contains("shift+tab"));
            if !trust_answered
                && screen.contains("safety")
                && screen.contains("folder")
                && !composer_up
            {
                if self.capture.is_some() {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                self.control
                    .send_program(vec![
                        crate::pty::PtyInput::Bytes(b"\x1b[B".to_vec()),
                        crate::pty::PtyInput::Delay(300),
                        crate::pty::PtyInput::Bytes(b"\r".to_vec()),
                    ])
                    .await
                    .map_err(|error| error.to_string())?;
                trust_answered = true;
                continue;
            }
            if composer_up {
                if self.capture.is_some() {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("Claude composer did not appear after 60s".to_owned());
            }
            if self.capture.is_some() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            } else {
                tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    self.screen_changes.changed(),
                )
                .await
                .map_err(|_| "Claude composer did not appear after 60s".to_owned())?
                .map_err(|_| {
                    "recorded terminal output ended before the composer appeared".to_owned()
                })?;
            }
        }
    }

    fn screen_tail(&self) -> String {
        let screen = self.screen.lock().expect("screen mutex poisoned");
        let mut tail = screen.chars().rev().take(4_000).collect::<Vec<_>>();
        tail.reverse();
        tail.into_iter().collect()
    }

    async fn next(&mut self) -> Result<PtyEvent, String> {
        let event = if let Some(event) = self.pending.pop_front() {
            event
        } else {
            self.events
                .recv()
                .await
                .ok_or_else(|| "PTY event stream ended".to_owned())?
        };
        if let PtyEvent::Hook(crate::hooks::HookPayload::PreToolUse {
            tool_name, common, ..
        }) = &event
            && let Some(tool_use_id) = common
                .raw
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
        {
            self.tool_use_ids
                .insert(tool_name.clone(), tool_use_id.to_owned());
        }
        if let Some(capture) = &self.capture {
            match &event {
                PtyEvent::Hook(hook) => capture.push(
                    "hook",
                    IoDirection::Read,
                    serde_json::to_string(hook.raw()).expect("hook JSON serializes"),
                ),
                PtyEvent::Transcript { path, row } => capture.push(
                    "transcript",
                    IoDirection::Read,
                    serde_json::json!({"path": path, "row": row.as_value()}).to_string(),
                ),
                _ => {}
            }
        }
        Ok(event)
    }

    fn tool_use_id(&self, tool_name: &str) -> Option<&str> {
        self.tool_use_ids.get(tool_name).map(String::as_str)
    }

    async fn send(&self, intent: Intent) -> Result<(), String> {
        self.control
            .send(intent)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn wait_ask(&mut self, matches: impl Fn(&AskKind) -> bool) -> Result<AskFacts, String> {
        loop {
            match self.next().await? {
                PtyEvent::Ask(ask) if matches(&ask.kind) => return Ok(ask),
                PtyEvent::Hook(crate::hooks::HookPayload::Stop { .. }) => {
                    return Err("Claude stopped the turn before producing the expected ask".into());
                }
                PtyEvent::Exited(status) => {
                    return Err(format!(
                        "Claude exited before producing the expected ask: {status:?}"
                    ));
                }
                _ => {}
            }
        }
    }

    async fn wait_transcript(
        &mut self,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value, String> {
        loop {
            if let PtyEvent::Transcript { row, .. } = self.next().await?
                && matches(row.as_value())
            {
                return Ok(row.into_value());
            }
        }
    }

    async fn wait_hook(
        &mut self,
        matches: impl Fn(&crate::hooks::HookPayload) -> bool,
    ) -> Result<crate::hooks::HookPayload, String> {
        loop {
            if let PtyEvent::Hook(hook) = self.next().await?
                && matches(&hook)
            {
                return Ok(hook);
            }
        }
    }

    async fn wait_relink(&mut self, expected: RelinkReason) -> Result<(), String> {
        loop {
            if let PtyEvent::Relink { reason, .. } = self.next().await?
                && reason == expected
            {
                return Ok(());
            }
        }
    }

    async fn drain_quiet(&mut self) {
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(100), self.events.recv()).await
        {
            self.pending.push_back(event);
            while let Some(event) = self.pending.pop_front() {
                if let Some(capture) = &self.capture {
                    match event {
                        PtyEvent::Hook(hook) => capture.push(
                            "hook",
                            IoDirection::Read,
                            serde_json::to_string(hook.raw()).expect("hook JSON serializes"),
                        ),
                        PtyEvent::Transcript { path, row } => capture.push(
                            "transcript",
                            IoDirection::Read,
                            serde_json::json!({"path": path, "row": row.as_value()}).to_string(),
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn assistant_contains(row: &serde_json::Value, marker: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
        && row.to_string().contains(marker)
}

fn tool_result_contains(row: &serde_json::Value, marker: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row.to_string().contains(marker)
}

fn successful_tool_result(row: &serde_json::Value, tool_use_id: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                        && block.get("tool_use_id").and_then(serde_json::Value::as_str)
                            == Some(tool_use_id)
                        && block.get("is_error").and_then(serde_json::Value::as_bool) != Some(true)
                })
            })
}

fn structured_patch_changes(
    row: &serde_json::Value,
    tool_use_id: &str,
    old_line: &str,
    new_line: &str,
) -> bool {
    successful_tool_result(row, tool_use_id)
        && row
            .pointer("/toolUseResult/structuredPatch")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|patches| {
                patches.iter().any(|patch| {
                    patch
                        .get("lines")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|lines| {
                            lines.iter().any(|line| line.as_str() == Some(old_line))
                                && lines.iter().any(|line| line.as_str() == Some(new_line))
                        })
                })
            })
}

fn bash_stdout_equals(row: &serde_json::Value, tool_use_id: &str, expected: &str) -> bool {
    successful_tool_result(row, tool_use_id)
        && row
            .pointer("/toolUseResult/stdout")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|stdout| stdout.trim_end() == expected)
}

fn user_prompt_equals(row: &serde_json::Value, expected: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/message/content")
            .and_then(serde_json::Value::as_str)
            == Some(expected)
}

fn question_result_has_answers(row: &serde_json::Value, expected: &[(&str, &str)]) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/toolUseResult/answers")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|answers| {
                expected.iter().all(|(question, answer)| {
                    answers.get(*question).and_then(serde_json::Value::as_str) == Some(*answer)
                })
            })
}

async fn prompt(session: &mut PtySpecSession) -> Result<(), String> {
    const MARKER: &str = "PTY_SPEC_PROMPT_OK";
    session
        .send(Intent::Prompt {
            text: format!("Reply exactly {MARKER} and nothing else."),
        })
        .await?;
    session
        .wait_transcript(|row| assistant_contains(row, MARKER))
        .await?;
    Ok(())
}

async fn prompt_multiline(session: &mut PtySpecSession) -> Result<(), String> {
    const TEXT: &str = "Reply exactly PTY_SPEC_MULTILINE_OK and nothing else.\nThis second line is part of one prompt.";
    session
        .send(Intent::Prompt {
            text: TEXT.to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| user_prompt_equals(row, TEXT))
        .await?;
    session
        .wait_transcript(|row| assistant_contains(row, "PTY_SPEC_MULTILINE_OK"))
        .await?;
    Ok(())
}

async fn tools(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "First use Read to inspect config.txt. Then use Edit to change its only line from VALUE=1 to VALUE=2. Then use Bash to run exactly: cat config.txt. Do all three in this turn, in that order, then stop."
                .to_owned(),
        })
        .await?;

    // Hooks and transcript rows arrive on independent streams. A result may
    // be observed before its hook, so retain both and correlate by tool ID.
    let expected_tools = ["Read", "Edit", "Bash"];
    let mut tool_ids = Vec::new();
    let mut results = Vec::new();
    loop {
        match session.next().await? {
            PtyEvent::Hook(crate::hooks::HookPayload::PreToolUse {
                tool_name, common, ..
            }) if expected_tools.contains(&tool_name.as_str()) => {
                if expected_tools.get(tool_ids.len()).copied() != Some(tool_name.as_str()) {
                    return Err(format!(
                        "expected Read, Edit, Bash in order; observed {tool_name}"
                    ));
                }
                let id = common
                    .raw
                    .get("tool_use_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        format!("{tool_name} PreToolUse hook omitted its tool-use id")
                    })?;
                tool_ids.push(id.to_owned());
            }
            PtyEvent::Transcript { row, .. } => results.push(row.into_value()),
            PtyEvent::Exited(status) => {
                return Err(format!(
                    "Claude exited before all three tool results: {status:?}"
                ));
            }
            _ => {}
        }
        if tool_ids.len() == 3
            && results
                .iter()
                .any(|row| successful_tool_result(row, &tool_ids[0]))
            && results
                .iter()
                .any(|row| structured_patch_changes(row, &tool_ids[1], "-VALUE=1", "+VALUE=2"))
            && results
                .iter()
                .any(|row| bash_stdout_equals(row, &tool_ids[2], "VALUE=2"))
        {
            return Ok(());
        }
    }
}

async fn permission_ask(session: &mut PtySpecSession, command: &str) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: format!("Use the Bash tool to run exactly: {command}. Then stop."),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Permission { tool_name, is_plan: false, .. } if tool_name == "Bash"))
        .await
}

async fn permission_allow_once(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = permission_ask(
        session,
        "printf allow-once > allow-once.txt; printf allow-once",
    )
    .await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "allow-once"))
        .await?;
    Ok(())
}

async fn permission_allow_scoped(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = permission_ask(
        session,
        "printf allow-scoped > allow-scoped.txt; printf allow-scoped",
    )
    .await?;
    let suggestions = match ask.kind {
        AskKind::Permission { suggestions, .. } => suggestions,
        _ => 0,
    };
    if suggestions == 0 {
        return Err("scoped permission ask carried no suggestion".to_owned());
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "allow-scoped"))
        .await?;
    Ok(())
}

async fn permission_deny_feedback(session: &mut PtySpecSession) -> Result<(), String> {
    const FEEDBACK: &str = "Use a read-only command instead";
    let ask = permission_ask(session, "printf denied > denied.txt").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some(FEEDBACK.to_owned()),
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, FEEDBACK))
        .await?;
    Ok(())
}

async fn plan_ask(session: &mut PtySpecSession) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: "Plan changing README.md's only line from CURRENT to UPDATED, then call ExitPlanMode. Do not ask questions.".to_owned(),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Permission { is_plan: true, .. }))
        .await
}

async fn plan_approve(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = plan_ask(session).await?;
    let tool_use_id = session
        .tool_use_id("ExitPlanMode")
        .ok_or_else(|| "ExitPlanMode PreToolUse hook omitted its tool-use id".to_owned())?
        .to_owned();
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
        })
        .await?;
    session
        .wait_transcript(|row| successful_tool_result(row, &tool_use_id))
        .await?;
    Ok(())
}

async fn plan_auto(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = plan_ask(session).await?;
    let exit_plan_tool_use_id = session
        .tool_use_id("ExitPlanMode")
        .ok_or_else(|| "ExitPlanMode PreToolUse hook omitted its tool-use id".to_owned())?
        .to_owned();
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::ApproveAuto),
        })
        .await?;
    session
        .wait_transcript(|row| successful_tool_result(row, &exit_plan_tool_use_id))
        .await?;

    let mut edit_tool_use_id = None;
    loop {
        match session.next().await? {
            PtyEvent::Ask(ask) => {
                return Err(format!(
                    "ApproveAuto produced a further ask before the planned edit landed: {:?}",
                    ask.kind
                ));
            }
            PtyEvent::Hook(crate::hooks::HookPayload::PreToolUse { tool_name, .. })
                if tool_name == "Edit" =>
            {
                edit_tool_use_id = session.tool_use_id("Edit").map(str::to_owned);
                if edit_tool_use_id.is_none() {
                    return Err("Edit PreToolUse hook omitted its tool-use id".to_owned());
                }
            }
            PtyEvent::Transcript { row, .. }
                if edit_tool_use_id.as_ref().is_some_and(|tool_use_id| {
                    structured_patch_changes(row.as_value(), tool_use_id, "-CURRENT", "+UPDATED")
                }) =>
            {
                return Ok(());
            }
            PtyEvent::Hook(crate::hooks::HookPayload::Stop { .. }) => {
                return Err("Claude stopped before the automatically approved edit landed".into());
            }
            PtyEvent::Exited(status) => {
                return Err(format!(
                    "Claude exited before the automatically approved edit landed: {status:?}"
                ));
            }
            _ => {}
        }
    }
}

async fn plan_request_changes(session: &mut PtySpecSession) -> Result<(), String> {
    const FEEDBACK: &str = "Also explain why the change is needed";
    let ask = plan_ask(session).await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: FEEDBACK.to_owned(),
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, FEEDBACK))
        .await?;
    Ok(())
}

async fn question_ask(session: &mut PtySpecSession, prompt: &str) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: prompt.to_owned(),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Question { .. }))
        .await
}

async fn question_single(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = question_ask(session, "Use AskUserQuestion to ask one single-select question with header Color and options Red and Blue. Then repeat my answer.").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0],
                    other: None,
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "Red"))
        .await?;
    Ok(())
}

async fn question_multi_other(session: &mut PtySpecSession) -> Result<(), String> {
    const OTHER: &str = "Torque wrench";
    let ask = question_ask(session, "Use AskUserQuestion to ask one multi-select question with header Tools and options Hammer, Saw, and Drill. Then repeat my answer.").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0, 1],
                    other: Some(OTHER.to_owned()),
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, OTHER))
        .await?;
    Ok(())
}

async fn question_mixed(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = question_ask(session, "Use one AskUserQuestion call containing two questions: first a single-select Color question with Red and Blue; second a multi-select Tools question with Hammer, Saw, and Drill. Then repeat both answers.").await?;
    let count = match &ask.kind {
        AskKind::Question { questions } => questions.len(),
        _ => 0,
    };
    if count != 2 {
        return Err(format!("mixed question ask carried {count} questions"));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: vec![1],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![0, 2],
                        other: None,
                    },
                ],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "Blue"))
        .await?;
    Ok(())
}

async fn question_tabs(session: &mut PtySpecSession) -> Result<(), String> {
    const COLOR_QUESTION: &str = "Which color do you prefer?";
    const SIZE_QUESTION: &str = "Which size fits best?";
    let ask = question_ask(
        session,
        "Use one AskUserQuestion call containing exactly two single-select questions. First: header Color, question 'Which color do you prefer?', options Red and Blue. Second: header Size, question 'Which size fits best?', options Small and Large. Then repeat both answers.",
    )
    .await?;
    let questions = match &ask.kind {
        AskKind::Question { questions } => questions,
        _ => unreachable!("question_ask returned a question"),
    };
    if questions.len() != 2
        || questions
            .iter()
            .any(|question| question.multi_select || question.options != 2)
    {
        return Err(format!(
            "two-question single-select ask had unexpected shape: {questions:?}"
        ));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: vec![0],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![1],
                        other: None,
                    },
                ],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| {
            question_result_has_answers(row, &[(COLOR_QUESTION, "Red"), (SIZE_QUESTION, "Large")])
        })
        .await?;
    Ok(())
}

async fn question_other_single(session: &mut PtySpecSession) -> Result<(), String> {
    const QUESTION: &str = "Which color do you prefer?";
    const OTHER: &str = "a warm ochre";
    let ask = question_ask(
        session,
        "Use AskUserQuestion to ask exactly one single-select question with header Color, question 'Which color do you prefer?', and options Red and Blue. Then repeat my answer.",
    )
    .await?;
    let questions = match &ask.kind {
        AskKind::Question { questions } => questions,
        _ => unreachable!("question_ask returned a question"),
    };
    if questions.len() != 1 || questions[0].multi_select || questions[0].options != 2 {
        return Err(format!(
            "single-select Other ask had unexpected shape: {questions:?}"
        ));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: Vec::new(),
                    other: Some(OTHER.to_owned()),
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| question_result_has_answers(row, &[(QUESTION, OTHER)]))
        .await?;
    Ok(())
}

async fn interrupt(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use Bash to run exactly: python3 -c 'import select; select.select([], [], [], 30)'; printf SHOULD_NOT_FINISH. Do not do anything else.".to_owned(),
        })
        .await?;
    session
        .wait_hook(|hook| matches!(hook, crate::hooks::HookPayload::PreToolUse { tool_name, .. } if tool_name == "Bash"))
        .await?;
    session.send(Intent::Interrupt).await?;
    session
        .wait_transcript(|row| row.to_string().to_lowercase().contains("interrupt"))
        .await?;
    Ok(())
}

async fn mode_cycle(session: &mut PtySpecSession) -> Result<(), String> {
    session.send(Intent::CyclePermissionMode).await?;
    session
        .send(Intent::Prompt {
            text: "Reply exactly MODE_CYCLED and nothing else.".to_owned(),
        })
        .await?;
    let hook = session
        .wait_hook(|hook| matches!(hook, crate::hooks::HookPayload::Stop { .. }))
        .await?;
    if hook.common().permission_mode.as_deref() == Some("default") {
        return Err("permission mode remained default after cycle".to_owned());
    }
    Ok(())
}

async fn completed_turn(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Reply exactly READY_FOR_RELINK and nothing else.".to_owned(),
        })
        .await?;
    session
        .wait_hook(|hook| matches!(hook, crate::hooks::HookPayload::Stop { .. }))
        .await?;
    Ok(())
}

async fn compact_relink(session: &mut PtySpecSession) -> Result<(), String> {
    for _ in 0..4 {
        completed_turn(session).await?;
    }
    session
        .send(Intent::Prompt {
            text: "/compact".to_owned(),
        })
        .await?;
    session.wait_relink(RelinkReason::Compact).await
}

async fn clear_relink(session: &mut PtySpecSession) -> Result<(), String> {
    completed_turn(session).await?;
    session
        .send(Intent::Prompt {
            text: "/clear".to_owned(),
        })
        .await?;
    session.wait_relink(RelinkReason::Clear).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_every_semantic_pty_claim_in_order() {
        assert_eq!(
            registry()
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec![
                "prompt",
                "prompt_multiline",
                "tools",
                "permission_allow_once",
                "permission_allow_scoped",
                "permission_deny_feedback",
                "plan_approve",
                "plan_auto",
                "plan_request_changes",
                "question_single",
                "question_multi_other",
                "question_mixed",
                "question_tabs",
                "question_other_single",
                "interrupt",
                "mode_cycle",
                "compact_relink",
                "clear_relink",
            ]
        );
        assert_eq!(DEFINITIONS.len(), registry().len());
    }

    #[test]
    fn sonnet_fallback_is_limited_to_specs_haiku_does_not_drive_reliably() {
        assert_eq!(
            SONNET_FALLBACK_SPECS,
            &[
                "plan_approve",
                "plan_auto",
                "plan_request_changes",
                "question_mixed"
            ]
        );
        assert!(
            registry()
                .iter()
                .filter(|entry| !SONNET_FALLBACK_SPECS.contains(&entry.name))
                .all(|entry| !entry.name.starts_with("plan_"))
        );
    }
}
