use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use replay_support::{ReplayAdvance, SpecEntry, StrictReplay};
use semver::Version;

use crate::{
    ApprovalPolicy, ApprovalResponse, Codex, CodexConfig, DynamicToolCallResponse,
    FunctionDynamicToolSpec, ListThreadsParams, MessagePhase, SandboxMode, Thread, ThreadConfig,
    ThreadEvent, ThreadEventStream, ThreadItem, TurnEvent, TurnStatus,
};

pub const MINIMUM_SUPPORTED: &str = "0.150.1";
pub const CAPTURE_MODEL: &str = "gpt-5.6-luna";
const ALLOWED_MODELS: &[&str] = &[CAPTURE_MODEL];
const EVENT_TIMEOUT: Duration = Duration::from_secs(300);
const LIVE_IO_FILE: &str = "spec.io.jsonl";

const REGISTRY: &[SpecEntry] = &[
    entry("initialize_and_start"),
    entry("turn_round_trip"),
    entry("approval_allow"),
    entry("approval_deny"),
    entry("interrupt"),
    entry("thread_list_and_resume"),
    entry("dynamic_tools"),
    entry("inject_idle"),
    entry("inject_busy"),
    entry("two_assistant_messages"),
];

const fn entry(name: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording: name,
        allowed_models: ALLOWED_MODELS,
    }
}

pub fn registry() -> &'static [SpecEntry] {
    REGISTRY
}

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

pub fn live_io_path(codex_home: &Path) -> PathBuf {
    codex_home.join(LIVE_IO_FILE)
}

pub enum SpecSource {
    Live { codex_home: PathBuf, model: String },
    Recorded(StrictReplay),
}

#[derive(Debug, thiserror::Error)]
#[error("specification {spec} failed: {claim}")]
pub struct SpecFailure {
    pub spec: String,
    pub claim: String,
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub provider_version: Option<Version>,
    pub server_model: String,
    pub session_ids: Vec<String>,
    pub observed: replay_support::Observed,
}

struct ScenarioReport {
    server_model: String,
    session_ids: Vec<String>,
}

struct Runtime {
    codex: Codex,
    model: String,
    project: PathBuf,
    live_io: Option<PathBuf>,
    replay_driver: Option<tokio::task::JoinHandle<()>>,
}

pub async fn run(spec: &SpecEntry, source: SpecSource) -> Result<(), SpecFailure> {
    execute(spec, source).await.map(|_| ())
}

/// Runs a specification and returns capture metadata used by `codex-probe`.
#[doc(hidden)]
pub async fn execute(spec: &SpecEntry, source: SpecSource) -> Result<RunReport, SpecFailure> {
    if !REGISTRY.contains(spec) {
        return Err(failure(spec, "specification is not in the Codex registry"));
    }

    let mut runtime = open_runtime(spec, source).await?;
    let initialization = runtime.codex.initialization_result().cloned();
    let scenario = run_scenario(spec.name, &runtime.codex, &runtime.model, &runtime.project).await;
    let replay_exhausted = if let Some(mut driver) = runtime.replay_driver.take() {
        let exhausted = tokio::time::timeout(Duration::from_secs(5), &mut driver)
            .await
            .is_ok();
        if !exhausted {
            driver.abort();
        }
        exhausted
    } else {
        true
    };
    runtime.codex.close().await;
    if !replay_exhausted {
        return Err(failure(
            spec,
            "strict replay driver did not reach exhaustion",
        ));
    }

    let scenario = scenario.map_err(|claim| failure(spec, claim))?;
    let observed = runtime
        .live_io
        .as_deref()
        .filter(|path| path.is_file())
        .map(replay_support::load_script)
        .map(|events| replay_support::observe(&events))
        .unwrap_or_default();
    let provider_version = initialization
        .as_ref()
        .and_then(|result| version_from_user_agent(&result.user_agent));

    Ok(RunReport {
        provider_version,
        server_model: scenario.server_model,
        session_ids: scenario.session_ids,
        observed,
    })
}

async fn open_runtime(spec: &SpecEntry, source: SpecSource) -> Result<Runtime, SpecFailure> {
    match source {
        SpecSource::Live { codex_home, model } => {
            if !spec.allowed_models.contains(&model.as_str()) {
                return Err(failure(
                    spec,
                    format!("model {model} is not allowed; expected {CAPTURE_MODEL}"),
                ));
            }
            let project = codex_home.parent().unwrap_or(&codex_home).join("project");
            std::fs::create_dir_all(&project).map_err(|error| failure(spec, error))?;
            let io_path = live_io_path(&codex_home);
            if io_path.exists() {
                std::fs::remove_file(&io_path).map_err(|error| failure(spec, error))?;
            }
            let mut env = HashMap::new();
            env.insert(
                "CODEX_HOME".to_string(),
                codex_home.to_string_lossy().into_owned(),
            );
            let codex = crate::connect(CodexConfig {
                model: Some(model.clone()),
                cwd: Some(project.clone()),
                env: Some(env),
                record_io: Some(io_path.clone()),
                client_name: "amux-codex-spec".to_string(),
                ..CodexConfig::default()
            })
            .await
            .map_err(|error| failure(spec, format!("model {model}: {error}")))?;
            Ok(Runtime {
                codex,
                model,
                project,
                live_io: Some(io_path),
                replay_driver: None,
            })
        }
        SpecSource::Recorded(mut replay) => {
            let transport = if replay.transports.len() == 1 {
                replay
                    .transports
                    .pop_first()
                    .map(|(_, transport)| transport)
            } else {
                replay.transports.remove("<default>")
            }
            .ok_or_else(|| failure(spec, "recording has no default app-server transport"))?;
            let controller = replay.controller.clone();
            let driver = tokio::spawn(async move {
                while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                    controller.advance_one().await
                {
                    tokio::task::yield_now().await;
                }
            });
            let codex = Codex::from_io(
                transport.reader,
                transport.writer,
                CodexConfig {
                    model: Some(CAPTURE_MODEL.to_string()),
                    client_name: "amux-codex-spec".to_string(),
                    ..CodexConfig::default()
                },
            )
            .await
            .map_err(|error| failure(spec, error))?;
            Ok(Runtime {
                codex,
                model: CAPTURE_MODEL.to_string(),
                project: PathBuf::from("<MACHINE_PATH>"),
                live_io: None,
                replay_driver: Some(driver),
            })
        }
    }
}

async fn run_scenario(
    name: &str,
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    match name {
        "initialize_and_start" => initialize_and_start(codex, model, project).await,
        "turn_round_trip" => turn_round_trip(codex, model, project).await,
        "approval_allow" => approval(codex, model, project, true).await,
        "approval_deny" => approval(codex, model, project, false).await,
        "interrupt" => interrupt(codex, model, project).await,
        "thread_list_and_resume" => thread_list_and_resume(codex, model, project).await,
        "dynamic_tools" => dynamic_tools(codex, model, project).await,
        "inject_idle" => inject_idle(codex, model, project).await,
        "inject_busy" => inject_busy(codex, model, project).await,
        "two_assistant_messages" => two_assistant_messages(codex, model, project).await,
        other => Err(format!("unknown registered specification {other}")),
    }
}

fn thread_config(model: &str, project: &Path) -> ThreadConfig {
    ThreadConfig {
        model: Some(model.to_string()),
        cwd: Some(project.to_string_lossy().into_owned()),
        approval_policy: Some(ApprovalPolicy::OnRequest),
        sandbox: Some(SandboxMode::WorkspaceWrite),
        ..ThreadConfig::default()
    }
}

async fn start_thread(codex: &Codex, model: &str, project: &Path) -> Result<Thread, String> {
    codex
        .start_thread(thread_config(model, project))
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))
}

fn report(thread: &Thread) -> ScenarioReport {
    ScenarioReport {
        server_model: thread.session_info().model.clone(),
        session_ids: vec![thread.id().to_string()],
    }
}

async fn initialize_and_start(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    Ok(report(&thread))
}

async fn turn_round_trip(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .start_turn("Reply with exactly CODEX_SPEC_PONG and nothing else.")
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if !messages
        .iter()
        .any(|(text, _)| text.contains("CODEX_SPEC_PONG"))
    {
        return Err("turn completed without CODEX_SPEC_PONG".to_string());
    }
    Ok(report(&thread))
}

async fn approval(
    codex: &Codex,
    model: &str,
    project: &Path,
    allow: bool,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.sandbox = Some(SandboxMode::ReadOnly);
    let thread = codex
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    let mut events = thread.events().await.map_err(stringify)?;
    let file = project.join(if allow {
        "approval-allowed.txt"
    } else {
        "approval-denied.txt"
    });
    let command = if project == Path::new("<MACHINE_PATH>") {
        // The sanitizer replaces the complete path token, including its trailing
        // punctuation, so replay must produce the canonical recorded sentence.
        "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE."
            .to_string()
    } else {
        format!(
            "Run this exact shell command and no substitute: /usr/bin/touch {}. Then say DONE.",
            file.display()
        )
    };
    thread.start_turn(command).await.map_err(stringify)?;

    loop {
        let event = next_event(&mut events, "approval request").await?;
        match event.event {
            TurnEvent::ApprovalRequired(request) => {
                thread
                    .respond_approval(
                        request.request_id(),
                        if allow {
                            ApprovalResponse::Accept
                        } else {
                            ApprovalResponse::Decline
                        },
                    )
                    .await
                    .map_err(stringify)?;
                break;
            }
            TurnEvent::TurnCompleted { .. } => {
                return Err("turn completed before requesting approval".to_string());
            }
            _ => {}
        }
    }
    wait_for_completion(&mut events).await?;
    if project.is_absolute() && file.exists() != allow {
        return Err(format!(
            "approval world assertion failed for {}",
            file.display()
        ));
    }
    Ok(report(&thread))
}

async fn interrupt(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    let turn_id = thread
        .start_turn("Count slowly from one to one hundred, one number per line.")
        .await
        .map_err(stringify)?;
    loop {
        if matches!(
            next_event(&mut events, "turn start before interrupt")
                .await?
                .event,
            TurnEvent::TurnStarted { .. }
        ) {
            break;
        }
    }
    thread.interrupt(&turn_id).await.map_err(stringify)?;
    wait_for_completion(&mut events).await?;
    Ok(report(&thread))
}

async fn thread_list_and_resume(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let original = report(&thread);
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .start_turn("Reply with exactly CODEX_SPEC_RESUME and nothing else.")
        .await
        .map_err(stringify)?;
    wait_for_completion(&mut events).await?;
    drop(events);
    codex
        .rename_thread(thread.id(), "codex-spec-resume")
        .await
        .map_err(stringify)?;
    let listed = codex
        .list_threads(ListThreadsParams::default())
        .await
        .map_err(stringify)?;
    if !listed.data.iter().any(|item| item.id == thread.id()) {
        return Err("thread/list omitted the newly started thread".to_string());
    }
    let id = thread.id().to_string();
    drop(thread);
    let resumed = codex
        .resume_thread(&id, thread_config(model, project))
        .await
        .map_err(stringify)?;
    if resumed.id() != id {
        return Err("thread/resume changed thread identity".to_string());
    }
    Ok(ScenarioReport {
        server_model: resumed.session_info().model.clone(),
        session_ids: original.session_ids,
    })
}

async fn dynamic_tools(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.dynamic_tools = Some(vec![FunctionDynamicToolSpec {
        name: "send".to_string(),
        description: "Send a short message to another agent.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"to": {"type": "string"}, "text": {"type": "string"}},
            "required": ["to", "text"]
        }),
        defer_loading: None,
    }]);
    let thread = codex.start_thread(config).await.map_err(stringify)?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .start_turn("Call the send tool exactly once with to=probe and text=CODEX_SPEC_SENT. Do not use any other tool.")
        .await
        .map_err(stringify)?;
    let mut called = false;
    loop {
        let event = next_event(&mut events, "dynamic tool call and completion").await?;
        match event.event {
            TurnEvent::ToolCallRequired(request) if request.tool == "send" => {
                if request.arguments
                    != serde_json::json!({"to": "probe", "text": "CODEX_SPEC_SENT"})
                {
                    return Err(format!(
                        "dynamic tool arguments differed: {}",
                        request.arguments
                    ));
                }
                called = true;
                thread
                    .respond_tool_call(
                        request.request_id,
                        DynamicToolCallResponse {
                            content_items: vec![
                                serde_json::json!({"type": "inputText", "text": "sent"}),
                            ],
                            success: true,
                        },
                    )
                    .await
                    .map_err(stringify)?;
            }
            TurnEvent::TurnCompleted { turn } => {
                if turn.status != TurnStatus::Completed || !called {
                    return Err("dynamic tool turn completed without the required call".to_string());
                }
                break;
            }
            _ => {}
        }
    }
    Ok(report(&thread))
}

fn injected_item(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

async fn inject_idle(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .inject_items(vec![injected_item(
            "Reply with exactly CODEX_SPEC_INJECT_IDLE and nothing else.",
        )])
        .await
        .map_err(stringify)?;
    thread.start_empty_turn().await.map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if !messages
        .iter()
        .any(|(text, _)| text.contains("CODEX_SPEC_INJECT_IDLE"))
    {
        return Err("idle injected item was not reflected in the response".to_string());
    }
    Ok(report(&thread))
}

async fn inject_busy(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .start_turn("Think briefly, then reply exactly CODEX_SPEC_INITIAL.")
        .await
        .map_err(stringify)?;
    loop {
        if matches!(
            next_event(&mut events, "busy turn start").await?.event,
            TurnEvent::TurnStarted { .. }
        ) {
            break;
        }
    }
    thread
        .inject_items(vec![injected_item(
            "Reply with exactly CODEX_SPEC_INJECT_BUSY and nothing else.",
        )])
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    let texts = messages
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>();
    if texts != ["CODEX_SPEC_INITIAL", "CODEX_SPEC_INJECT_BUSY"] {
        return Err(format!("busy injected message order differed: {texts:?}"));
    }
    Ok(report(&thread))
}

async fn two_assistant_messages(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .start_turn("Send two separate assistant messages in this turn: first exactly CODEX_SPEC_FIRST in commentary, then exactly CODEX_SPEC_SECOND as the final answer.")
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if messages
        != [
            (
                "CODEX_SPEC_FIRST".to_string(),
                Some(MessagePhase::Commentary),
            ),
            (
                "CODEX_SPEC_SECOND".to_string(),
                Some(MessagePhase::FinalAnswer),
            ),
        ]
    {
        return Err(format!(
            "assistant message order or phases differed: {messages:?}"
        ));
    }
    Ok(report(&thread))
}

async fn wait_for_completion(
    events: &mut ThreadEventStream,
) -> Result<Vec<(String, Option<MessagePhase>)>, String> {
    let mut messages = Vec::new();
    loop {
        let event = next_event(events, "turn completion").await?;
        match event.event {
            TurnEvent::ItemCompleted(ThreadItem::AgentMessage { text, phase, .. }) => {
                messages.push((text, phase));
            }
            TurnEvent::TurnCompleted { turn } => {
                if turn.status != TurnStatus::Completed && turn.status != TurnStatus::Interrupted {
                    return Err(format!("turn completed with status {:?}", turn.status));
                }
                return Ok(messages);
            }
            TurnEvent::Error { message, .. } => return Err(format!("turn error: {message}")),
            _ => {}
        }
    }
}

async fn next_event(events: &mut ThreadEventStream, what: &str) -> Result<ThreadEvent, String> {
    tokio::time::timeout(EVENT_TIMEOUT, events.next())
        .await
        .map_err(|_| format!("timed out waiting for {what}"))?
        .map_err(stringify)?
        .ok_or_else(|| format!("event stream closed while waiting for {what}"))
}

fn version_from_user_agent(user_agent: &str) -> Option<Version> {
    user_agent
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|part| part.matches('.').count() >= 2)
        .find_map(|part| Version::parse(part).ok())
}

fn stringify(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn failure(spec: &SpecEntry, claim: impl std::fmt::Display) -> SpecFailure {
    SpecFailure {
        spec: spec.name.to_string(),
        claim: claim.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_the_provider_side_of_the_c_suite() {
        assert_eq!(registry().len(), 10);
        assert_eq!(registry()[0].name, "initialize_and_start");
        assert_eq!(registry()[9].name, "two_assistant_messages");
        assert!(
            registry()
                .iter()
                .all(|entry| entry.allowed_models == [CAPTURE_MODEL])
        );
    }

    #[test]
    fn user_agent_version_is_semantic() {
        assert_eq!(
            version_from_user_agent("codex-cli/0.150.1"),
            Some(Version::parse("0.150.1").unwrap())
        );
    }
}
