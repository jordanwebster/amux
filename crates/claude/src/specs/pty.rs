//! Executable specifications for Claude Code's interactive PTY boundary.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use replay_support::{IoDirection, IoEvent, Manifest, ReplayReport, SpecEntry, StrictReplay};
use tokio::sync::mpsc;

use crate::launch::Launch;
use crate::pty::keymap::KeymapSources;
use crate::pty::{
    AskAnswer, AskFacts, AskKind, Intent, PermissionAnswer, PlanAnswer, PtyEvent, QuestionAnswer,
    QuestionResponse, RelinkReason,
};

use super::{ALLOWED_MODELS, HAIKU, SpecFailure};

const WAIT: Duration = Duration::from_secs(120);

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
    definition!(permission_allow_once, &[], permission_allow_once),
    definition!(permission_allow_scoped, &[], permission_allow_scoped),
    definition!(permission_deny_feedback, &[], permission_deny_feedback),
    definition!(plan_approve, &["--permission-mode", "plan"], plan_approve),
    definition!(
        plan_request_changes,
        &["--permission-mode", "plan"],
        plan_request_changes
    ),
    definition!(question_single, &[], question_single),
    definition!(question_multi_other, &[], question_multi_other),
    definition!(question_mixed, &[], question_mixed),
    definition!(interrupt, &["--dangerously-skip-permissions"], interrupt),
    definition!(mode_cycle, &[], mode_cycle),
    definition!(compact_relink, &[], compact_relink),
    definition!(clear_relink, &[], clear_relink),
];

static REGISTRY: [SpecEntry; 13] = [
    entry("prompt"),
    entry("permission_allow_once"),
    entry("permission_allow_scoped"),
    entry("permission_deny_feedback"),
    entry("plan_approve"),
    entry("plan_request_changes"),
    entry("question_single"),
    entry("question_multi_other"),
    entry("question_mixed"),
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
        launch.args.extend(["--model".to_owned(), HAIKU.to_owned()]);
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

    let replay_driver = controller.as_ref().map(|controller| {
        let controller = controller.clone();
        tokio::spawn(async move {
            while let replay_support::ReplayAdvance::Advanced { .. }
            | replay_support::ReplayAdvance::BlockedOnWrite = controller.advance_one().await
            {
                tokio::task::yield_now().await;
            }
        })
    });
    let mut session = PtySpecSession::new(session, capture.clone());
    tokio::time::timeout(WAIT, (definition.run)(&mut session))
        .await
        .map_err(|_| failure(entry, format!("stalled after {WAIT:?}")))?
        .map_err(|claim| failure(entry, claim))?;
    session.drain_quiet().await;
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        session.control.stop(pty_host::Terminate::Kill),
    )
    .await;
    if let Some(driver) = replay_driver {
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .map_err(|_| failure(entry, "strict replay driver did not finish"))?
            .map_err(|error| failure(entry, format!("strict replay task failed: {error}")))?;
    }
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
    pending: VecDeque<PtyEvent>,
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
            if let Some(mut output) = session.control.terminal_output() {
                tokio::spawn(async move {
                    while let Some(bytes) = output.recv().await {
                        capture.push(
                            "pty",
                            IoDirection::Read,
                            crate::pty::encode_recording_bytes(&bytes),
                        );
                    }
                });
            }
        }
        Self {
            events: session.events,
            control: session.control,
            capture,
            pending: VecDeque::new(),
        }
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

    async fn send(&self, intent: Intent) -> Result<(), String> {
        self.control
            .send(intent)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn wait_ask(&mut self, matches: impl Fn(&AskKind) -> bool) -> Result<AskFacts, String> {
        loop {
            if let PtyEvent::Ask(ask) = self.next().await?
                && matches(&ask.kind)
            {
                return Ok(ask);
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
    let ask = permission_ask(session, "printf allow-once > allow-once.txt").await?;
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
    let ask = permission_ask(session, "printf allow-scoped > allow-scoped.txt").await?;
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
            text: "Plan a one-line README change, then call ExitPlanMode.".to_owned(),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Permission { is_plan: true, .. }))
        .await
}

async fn plan_approve(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = plan_ask(session).await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "ExitPlanMode"))
        .await?;
    Ok(())
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

async fn interrupt(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use Bash to run exactly: sleep 30; printf SHOULD_NOT_FINISH. Do not do anything else.".to_owned(),
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
    completed_turn(session).await?;
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
                "permission_allow_once",
                "permission_allow_scoped",
                "permission_deny_feedback",
                "plan_approve",
                "plan_request_changes",
                "question_single",
                "question_multi_other",
                "question_mixed",
                "interrupt",
                "mode_cycle",
                "compact_relink",
                "clear_relink",
            ]
        );
        assert_eq!(DEFINITIONS.len(), registry().len());
    }
}
