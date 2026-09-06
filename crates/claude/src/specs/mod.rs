//! Executable specifications for the Claude protocol surface.
//!
//! A specification is one async function that drives a [`SpecSession`] and states
//! what must hold. The same function runs in two modes: against a live Claude
//! Code process it records the conversation, and against that recording it
//! verifies the claim. Nothing is authored in between, which is what stops a
//! specification describing traffic that was never observed.
//!
//! The division of labour matters. A specification asserts only what survives
//! a re-capture — the acknowledged mode, the completed text, the result
//! subtype. Everything transcript-shaped (token counts, message ids,
//! durations) stays in the recording and is enforced instead by strict replay,
//! which requires every recorded frame to be consumed. Claims stay readable;
//! evidence stays exact.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream;
use replay_support::{ReplayAdvance, SpecEntry, StrictReplay};
use semver::Version;
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::sdk::init::ContextUsage;
use crate::sdk::{
    CompactBoundaryMessage, ContentBlock, Error, InitializationResult, McpServerStatus, Message,
    MessageContent, PermissionDeniedMessage, PermissionMode, ProcessExit, QueryOptions,
    ResultMessage, SdkEvent, StreamDelta, StreamEvent, TaskNotificationMessage, TaskStartedMessage,
    Usage,
};

pub mod agents;
pub mod commands;
pub mod configured;
pub mod control;
pub mod history;
#[cfg(feature = "pty")]
pub mod probe;
#[cfg(feature = "pty")]
pub mod pty;
pub mod results;
pub mod session;
pub mod tools;

/// Specifications reach for Haiku first: shorter turns make smaller, more
/// stable recordings, and nothing below needs a larger model to be true.
pub(crate) const HAIKU: &str = "claude-haiku-4-5-20251001";
pub const MINIMUM_SUPPORTED: &str = "2.1.247";
pub const ALLOWED_MODELS: &[&str] = &[HAIKU, "claude-sonnet-5"];

/// How long a draining session must stay silent before it counts as finished.
/// Replay reaches the end of its recording instead and never waits this out.
const QUIET: std::time::Duration = std::time::Duration::from_secs(3);

/// Assert a specification claim. The message states the claim in the words a
/// reader needs, because it is the whole of what a failure reports.
#[macro_export]
macro_rules! expect {
    ($condition:expr, $claim:expr) => {
        assert!($condition, "unmet claim: {}", $claim)
    };
    ($condition:expr, $claim:expr, $($argument:tt)+) => {
        assert!($condition, "unmet claim: {}", format!($claim, $($argument)+))
    };
}

/// How a session opens. Identical in both modes, so a recording cannot be
/// replayed under options that differ from the ones that produced it.
pub struct SessionSetup {
    pub prompt: String,
    pub options: QueryOptions,
    answer_permission: bool,
    question_answer: Option<String>,
    plan_reviews: VecDeque<PlanReview>,
    hook_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    elicitation_content: Option<serde_json::Value>,
    dialog_result: Option<serde_json::Value>,
    defer_prompt: bool,
}

#[derive(Clone)]
pub(crate) enum PlanReview {
    ApproveAuto,
    ApproveManual,
    RequestChanges(String),
}

impl SessionSetup {
    /// The common case: one text prompt against a named model.
    pub fn new(model: &str, prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            options: QueryOptions::new(model),
            answer_permission: false,
            question_answer: None,
            plan_reviews: VecDeque::new(),
            hook_log: None,
            elicitation_content: None,
            dialog_result: None,
            defer_prompt: false,
        }
    }

    /// A session that stays open for more than one turn.
    ///
    /// A single text prompt closes Claude Code's stdin as soon as it has been
    /// answered, which is right for a one-shot question and fatal for a
    /// conversation: a second message would have nowhere to go. Opening with a
    /// stream instead leaves the input open until the session is closed.
    pub fn conversation(model: &str, first: impl Into<String>) -> Self {
        let mut setup = Self::new(model, first);
        setup.defer_prompt = true;
        setup
    }

    pub(crate) fn allow_permissions(&mut self) {
        self.answer_permission = true;
    }

    pub(crate) fn answer_question(&mut self, answer: impl Into<String>) {
        self.question_answer = Some(answer.into());
    }

    pub(crate) fn review_plans(&mut self, reviews: impl IntoIterator<Item = PlanReview>) {
        self.plan_reviews = reviews.into_iter().collect();
    }

    pub(crate) fn answer_hooks(&mut self, log: Option<Arc<std::sync::Mutex<Vec<String>>>>) {
        self.hook_log = Some(log.unwrap_or_default());
    }

    pub(crate) fn accept_elicitation(&mut self, content: serde_json::Value) {
        self.elicitation_content = Some(content);
    }

    pub(crate) fn complete_dialog(&mut self, result: serde_json::Value) {
        self.dialog_result = Some(result);
    }
}

/// One recorded session's identity and I/O, for replay to hand back.
pub struct RecordedTransport {
    pub session_id: String,
    pub reader: Box<dyn AsyncBufRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
}

enum Source {
    /// Open real Claude Code processes, applying this to every session's
    /// options so that capture's environment reaches all of them.
    Live(Box<dyn FnMut(&mut QueryOptions) + Send>),
    /// Hand back the recorded sessions in the order they were opened.
    Recorded(VecDeque<RecordedTransport>),
}

/// Where a specification's sessions come from.
///
/// A specification that resumes or forks needs to open a second session, and
/// the two modes differ only here: capture spawns another process, replay hands
/// back the next transport from the recording. Everything else about a session
/// - what it is asked, what it claims - is the specification's alone.
#[derive(Clone)]
pub struct Sessions {
    source: Arc<tokio::sync::Mutex<Source>>,
    /// The identity of every session opened so far, in order. Capture writes
    /// these into the manifest so replay can reuse them.
    opened: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Sessions {
    /// Every session opened so far, in the order the specification opened them.
    pub fn opened(&self) -> Vec<String> {
        self.opened
            .lock()
            .expect("the session ledger is not poisoned")
            .clone()
    }

    /// Open real processes, patching each session's options with the capture
    /// environment: which binary to talk to, where to work, and the sandbox.
    pub fn live(environment: impl FnMut(&mut QueryOptions) + Send + 'static) -> Self {
        Self::from(Source::Live(Box::new(environment)))
    }

    /// Replay the recorded sessions, in the order the recording opened them.
    pub fn recorded(transports: Vec<RecordedTransport>) -> Self {
        Self::from(Source::Recorded(transports.into()))
    }

    fn from(source: Source) -> Self {
        Self {
            source: Arc::new(tokio::sync::Mutex::new(source)),
            opened: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub async fn open(&self, mut setup: SessionSetup) -> Result<SpecSession, Error> {
        let session = match &mut *self.source.lock().await {
            Source::Live(environment) => {
                environment(&mut setup.options);
                crate::sdk::spawn(setup.options).await?
            }
            Source::Recorded(transports) => {
                let transport = transports.pop_front().ok_or_else(|| {
                    Error::InvalidOptions(
                        "the specification opened more sessions than the recording holds".into(),
                    )
                })?;
                // Reuse the identity the recording was made under, unless the
                // specification chose one itself: an explicit id or a resumed
                // session is a claim, and silently overwriting it would hide a
                // specification that no longer opens what it recorded.
                if setup.options.session_id.is_none() && setup.options.resume.is_none() {
                    setup.options.session_id = Some(transport.session_id);
                }
                crate::sdk::from_io(transport.reader, transport.writer, setup.options).await?
            }
        };
        if setup.defer_prompt {
            let control = session.control.clone();
            let prompt = setup.prompt;
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                control.prompt(crate::sdk::UserMessage::text(prompt)).await
            });
        } else {
            session
                .control
                .prompt(crate::sdk::UserMessage::text(setup.prompt))
                .await?;
        }
        self.opened
            .lock()
            .expect("the session ledger is not poisoned")
            .push(session.control.session_id().to_owned());
        Ok(SpecSession {
            events: session.events,
            control: Some(session.control),
            sessions: self.clone(),
            pending: Vec::new(),
            answer_permission: setup.answer_permission,
            question_answer: setup.question_answer,
            plan_reviews: setup.plan_reviews,
            permission_requests: Vec::new(),
            hook_log: setup.hook_log,
            elicitation_content: setup.elicitation_content,
            dialog_result: setup.dialog_result,
            dialog_requests: Vec::new(),
            exit: None,
        })
    }
}

/// A driven Claude session. The transport behind it is the only thing that
/// differs between recording and replay.
pub struct SpecSession {
    events: crate::sdk::EventStream,
    control: Option<crate::sdk::Control>,
    /// So a specification can open a further session - to resume or fork the
    /// one it is already driving - without knowing which mode it is running in.
    sessions: Sessions,
    /// Messages read by [`SpecSession::advance_to`] that the turn they belong
    /// to has not been given yet.
    pending: Vec<Message>,
    answer_permission: bool,
    question_answer: Option<String>,
    plan_reviews: VecDeque<PlanReview>,
    permission_requests: Vec<(String, serde_json::Value)>,
    hook_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    elicitation_content: Option<serde_json::Value>,
    dialog_result: Option<serde_json::Value>,
    dialog_requests: Vec<crate::sdk::UserDialogRequest>,
    exit: Option<ProcessExit>,
}

impl SpecSession {
    /// Open a further session, from the same place this one came from.
    ///
    /// This is how a specification resumes or forks: the session it is already
    /// driving stays alive, and the new one is opened beside it.
    pub async fn open(&self, setup: SessionSetup) -> Result<SpecSession, Error> {
        self.sessions.open(setup).await
    }

    pub fn session_id(&self) -> &str {
        self.control().session_id()
    }

    /// What Claude Code answered the SDK's initialize control with: the model,
    /// the tools, the slash commands, the account. A session cannot open
    /// without it, so it is not optional here.
    pub fn initialization(&self) -> &InitializationResult {
        self.control()
            .initialization_result()
            .expect("an opened session has been initialized")
    }

    pub async fn context_usage(&self) -> Result<ContextUsage, Error> {
        self.control().get_context_usage().await
    }

    pub async fn mcp_server_status(&self) -> Result<Vec<McpServerStatus>, Error> {
        self.control().mcp_server_status().await
    }

    pub async fn background_tasks(&self, tool_use_id: Option<&str>) -> Result<bool, Error> {
        self.control().background_tasks(tool_use_id).await
    }

    pub async fn stop_task(&self, task_id: &str) -> Result<(), Error> {
        self.control().stop_task(task_id).await
    }

    pub async fn reinitialize(&self) -> Result<InitializationResult, Error> {
        self.control().reinitialize().await
    }

    pub async fn reconnect_mcp_server(&self, name: &str) -> Result<(), Error> {
        self.control().reconnect_mcp_server(name).await
    }

    pub async fn set_mcp_servers(
        &self,
        servers: std::collections::HashMap<String, crate::sdk::McpServerConfig>,
    ) -> Result<crate::sdk::McpSetServersResult, Error> {
        self.control().set_mcp_servers(servers).await
    }

    pub async fn set_mcp_permission_mode_override(
        &self,
        name: &str,
        mode: Option<crate::sdk::McpPermissionMode>,
    ) -> Result<crate::sdk::McpPermissionModeOverrideResult, Error> {
        self.control()
            .set_mcp_permission_mode_override(name, mode)
            .await
    }

    pub async fn toggle_mcp_server(&self, name: &str, enabled: bool) -> Result<(), Error> {
        self.control().toggle_mcp_server(name, enabled).await
    }

    pub async fn reload_skills(&self) -> Result<crate::sdk::ReloadSkillsResult, Error> {
        self.control().reload_skills().await
    }

    pub async fn reload_plugins(&self) -> Result<crate::sdk::ReloadPluginsResult, Error> {
        self.control().reload_plugins().await
    }

    pub async fn apply_flag_settings(&self, settings: serde_json::Value) -> Result<(), Error> {
        self.control().apply_flag_settings(settings).await
    }

    pub async fn seed_read_state(&self, path: &str, mtime: u64) -> Result<(), Error> {
        self.control().seed_read_state(path, mtime).await
    }

    /// Send another user message into a session that is already open.
    pub async fn say(&self, text: &str) -> Result<(), Error> {
        self.control()
            .prompt(crate::sdk::UserMessage::text(text))
            .await
    }

    pub async fn set_permission_mode(
        &self,
        mode: PermissionMode,
    ) -> Result<Option<PermissionMode>, Error> {
        self.control().set_permission_mode(mode).await
    }

    pub async fn set_model(&self, model: &str) -> Result<(), Error> {
        self.control().set_model(Some(model)).await
    }

    pub async fn interrupt(&self) -> Result<(), Error> {
        self.control().interrupt().await.map(|_| ())
    }

    pub(crate) fn permission_request_count(&self, tool_name: &str) -> usize {
        self.permission_requests
            .iter()
            .filter(|(seen, _)| seen == tool_name)
            .count()
    }

    pub(crate) fn dialog_requests(&self) -> &[crate::sdk::UserDialogRequest] {
        &self.dialog_requests
    }

    /// Read forward until the session has produced a message of this shape,
    /// keeping what was read for whoever takes it.
    ///
    /// This is how a specification says "once the turn is under way" without
    /// saying "after three seconds". A duration would be a guess when
    /// recording and pure waste when replaying, where the answer is already
    /// known.
    pub async fn advance_to(&mut self, kind: &str) {
        let wanted = kind.to_owned();
        self.advance_until(move |message| message.kind() == wanted)
            .await;
    }

    pub(crate) async fn advance_to_permission_request(&mut self, tool_name: &str, count: usize) {
        while self.permission_request_count(tool_name) < count {
            let Some(message) = self.next_message().await else {
                panic!(
                    "the session ended before reaching permission request {count} for {tool_name}"
                );
            };
            self.pending.push(message);
        }
    }

    /// Read the stream to the end of the turn in flight.
    pub async fn turn(&mut self) -> Turn {
        self.advance_until(|message| matches!(message, Message::Result(_)))
            .await;
        self.take()
    }

    /// Read until the session goes quiet, then hand over everything read.
    ///
    /// Work the session delegated can outlive the turn that asked for it, so a
    /// specification about delegation has to be able to say "and then whatever
    /// else this session had to say", rather than stopping at the result and
    /// leaving the rest of the conversation unexamined.
    pub async fn drain(&mut self) -> Turn {
        while let Ok(Some(message)) = tokio::time::timeout(QUIET, self.next_message()).await {
            self.pending.push(message);
        }
        self.take()
    }

    /// Everything read since the last turn was taken.
    pub fn take(&mut self) -> Turn {
        Turn {
            messages: std::mem::take(&mut self.pending),
        }
    }

    async fn advance_until(&mut self, mut reached: impl FnMut(&Message) -> bool) {
        if self.pending.iter().any(&mut reached) {
            return;
        }
        loop {
            let Some(message) = self.next_message().await else {
                panic!("the session ended before reaching what was waited for");
            };
            let done = reached(&message);
            self.pending.push(message);
            if done {
                return;
            }
        }
    }

    async fn next_message(&mut self) -> Option<Message> {
        loop {
            let item = std::future::poll_fn(|cx| Pin::new(&mut self.events).poll_next(cx)).await?;
            match item.expect("the session stream failed mid-turn") {
                SdkEvent::Message(message) => return Some(message),
                SdkEvent::PermissionRequest {
                    id,
                    input,
                    suggestions: _,
                    blocked_path: _,
                    tool_name,
                } => {
                    self.permission_requests
                        .push((tool_name.clone(), input.clone()));
                    let result = if tool_name == "AskUserQuestion" && self.question_answer.is_some()
                    {
                        crate::sdk::PermissionResult::Allow {
                            updated_input: Some(question_input_with_answer(
                                input,
                                self.question_answer
                                    .as_deref()
                                    .expect("question answer checked"),
                            )),
                            updated_permissions: None,
                            tool_use_id: None,
                        }
                    } else if tool_name == "ExitPlanMode" && !self.plan_reviews.is_empty() {
                        match self.plan_reviews.pop_front().expect("plan review checked") {
                            PlanReview::ApproveAuto => crate::sdk::PermissionResult::Allow {
                                updated_input: Some(input),
                                updated_permissions: Some(vec![
                                    crate::sdk::PermissionUpdate::SetMode {
                                        mode: PermissionMode::AcceptEdits,
                                        destination:
                                            crate::sdk::PermissionUpdateDestination::Session,
                                    },
                                ]),
                                tool_use_id: None,
                            },
                            // A bare allow leaves plan mode into accept-edits
                            // on the CLI's own initiative, so approving
                            // manually has to say which mode it wants.
                            PlanReview::ApproveManual => crate::sdk::PermissionResult::Allow {
                                updated_input: Some(input),
                                updated_permissions: Some(vec![
                                    crate::sdk::PermissionUpdate::SetMode {
                                        mode: PermissionMode::Default,
                                        destination:
                                            crate::sdk::PermissionUpdateDestination::Session,
                                    },
                                ]),
                                tool_use_id: None,
                            },
                            PlanReview::RequestChanges(message) => {
                                crate::sdk::PermissionResult::Deny {
                                    message,
                                    interrupt: Some(false),
                                    tool_use_id: None,
                                }
                            }
                        }
                    } else if self.answer_permission {
                        crate::sdk::PermissionResult::Allow {
                            updated_input: Some(input),
                            updated_permissions: None,
                            tool_use_id: None,
                        }
                    } else {
                        crate::sdk::PermissionResult::Deny {
                            message: "the specification did not authorize this request".into(),
                            interrupt: Some(false),
                            tool_use_id: None,
                        }
                    };
                    self.control()
                        .answer_permission(id, result)
                        .await
                        .expect("the permission request is answered through Control");
                }
                SdkEvent::HookCallback { id, input, .. } => {
                    if let Some(log) = &self.hook_log {
                        log.lock()
                            .expect("the hook log is not poisoned")
                            .push(hook_event_name(&input));
                    }
                    self.control()
                        .answer_hook(id, allow_hook())
                        .await
                        .expect("the hook request is answered through Control");
                }
                SdkEvent::Elicitation { id, request: _ } => {
                    let result = match self.elicitation_content.clone() {
                        Some(content) => crate::sdk::ElicitationResult::Accept {
                            content: Some(content),
                            extensions: Default::default(),
                        },
                        None => crate::sdk::ElicitationResult::Decline {
                            extensions: Default::default(),
                        },
                    };
                    self.control()
                        .answer_elicitation(id, result)
                        .await
                        .expect("the elicitation is answered through Control");
                }
                SdkEvent::UserDialog { id, request } => {
                    self.dialog_requests.push(request);
                    let result = match self.dialog_result.clone() {
                        Some(result) => crate::sdk::UserDialogResult::Completed {
                            result,
                            extensions: Default::default(),
                        },
                        None => crate::sdk::UserDialogResult::Cancelled {
                            extensions: Default::default(),
                        },
                    };
                    self.control()
                        .answer_user_dialog(id, result)
                        .await
                        .expect("the dialog request is answered through Control");
                }
                SdkEvent::Exited(exit) => {
                    self.exit = Some(exit);
                    return None;
                }
            }
        }
    }

    fn control(&self) -> &crate::sdk::Control {
        self.control.as_ref().expect("the session is still open")
    }

    pub async fn close(mut self) -> ProcessExit {
        match self.control.take() {
            Some(control) => control.close().await,
            None => self.exit.unwrap_or(ProcessExit {
                success: true,
                code: Some(0),
                stderr: String::new(),
                termination: crate::sdk::Termination::Exited,
            }),
        }
    }
}

fn question_input_with_answer(mut input: serde_json::Value, answer: &str) -> serde_json::Value {
    let answers = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| question.get("question").and_then(serde_json::Value::as_str))
        .map(|question| {
            (
                question.to_owned(),
                serde_json::Value::String(answer.to_owned()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    input["answers"] = serde_json::Value::Object(answers);
    input
}

fn allow_hook() -> crate::sdk::HookOutput {
    crate::sdk::HookOutput::Sync(crate::sdk::SyncHookOutput {
        r#continue: Some(true),
        suppress_output: None,
        stop_reason: None,
        decision: None,
        system_message: None,
        reason: None,
        hook_specific_output: None,
    })
}

fn hook_event_name(input: &crate::sdk::HookInput) -> String {
    match &input.event {
        crate::sdk::HookEventData::UserPromptSubmit { .. } => "UserPromptSubmit",
        crate::sdk::HookEventData::PreToolUse { .. } => "PreToolUse",
        crate::sdk::HookEventData::PostToolUse { .. } => "PostToolUse",
        crate::sdk::HookEventData::Stop { .. } => "Stop",
        other => return format!("{other:?}"),
    }
    .to_string()
}

/// Everything one turn produced, with accessors for the parts a claim is
/// usually about.
pub struct Turn {
    messages: Vec<Message>,
}

impl Turn {
    /// The completed assistant text, in order, concatenated.
    pub fn text(&self) -> String {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Every permission mode the session stated during this turn, in
    /// order: the `system.status` rows that follow a mode change.
    pub fn permission_modes(&self) -> Vec<PermissionMode> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::Status(status) => status.permission_mode.clone(),
                _ => None,
            })
            .collect()
    }

    /// The completed assistant thinking, in order, concatenated.
    pub fn thinking(&self) -> String {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The names of the tools this turn asked to use, in order.
    pub fn tools_used(&self) -> Vec<&str> {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The tool calls this turn made, as `(id, name)`. The id is what
    /// everything else about a tool call correlates against.
    pub fn tool_uses(&self) -> Vec<(&str, &str)> {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
                _ => None,
            })
            .collect()
    }

    /// The completion news for delegated tasks, which can arrive after the
    /// turn that spawned them has already ended.
    pub fn task_notifications(&self) -> Vec<&TaskNotificationMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::TaskNotification(task) => Some(task),
                _ => None,
            })
            .collect()
    }

    /// The compaction boundaries this turn crossed, with what was reclaimed.
    pub fn compactions(&self) -> Vec<&CompactBoundaryMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::CompactBoundary(boundary) => Some(boundary),
                _ => None,
            })
            .collect()
    }

    /// Tool uses this turn refused.
    pub fn permission_denials(&self) -> Vec<&PermissionDeniedMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::PermissionDenied(denial) => Some(denial),
                _ => None,
            })
            .collect()
    }

    /// History the session re-stated rather than took anew.
    pub fn replayed(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| matches!(message, Message::UserReplay(_)))
            .count()
    }

    /// The delegated tasks this turn announced.
    pub fn tasks_started(&self) -> Vec<&TaskStartedMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::TaskStarted(task) => Some(task),
                _ => None,
            })
            .collect()
    }

    /// The tool results that came back, as the text they carried. These arrive
    /// as user messages, because a tool result is input to the next assistant
    /// turn however it was produced.
    pub fn tool_results(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::User(user) => Some(&user.message.content),
                _ => None,
            })
            .flat_map(|content| match content {
                MessageContent::Text(text) => vec![text.clone()],
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult { content, .. } => Some(
                            content
                                .iter()
                                .map(|part| match part {
                                    crate::sdk::ToolResultContent::Text { text, .. } => {
                                        text.clone()
                                    }
                                    other => serde_json::to_string(other).unwrap_or_default(),
                                })
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect(),
            })
            .collect()
    }

    /// The incremental events this turn streamed, in the order they arrived.
    /// Empty unless the session opted into partial messages.
    pub fn stream_events(&self) -> Vec<&StreamEvent> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::StreamEvent(event) => Some(&event.event),
                _ => None,
            })
            .collect()
    }

    /// The text assembled from the deltas, which must agree with the completed
    /// text: streaming is a view of the same answer, not a second one.
    pub fn streamed_text(&self) -> String {
        self.deltas()
            .filter_map(|delta| match delta {
                StreamDelta::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn streamed_thinking(&self) -> String {
        self.deltas()
            .filter_map(|delta| match delta {
                StreamDelta::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect()
    }

    /// What Claude Code reported when a turn ended badly.
    pub fn errors(&self) -> Vec<&str> {
        match self.result() {
            Some(
                ResultMessage::ErrorDuringExecution(error)
                | ResultMessage::ErrorMaxTurns(error)
                | ResultMessage::ErrorMaxBudgetUsd(error)
                | ResultMessage::ErrorMaxStructuredOutputRetries(error),
            ) => error.errors.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }

    pub fn usage(&self) -> Option<&Usage> {
        match self.result()? {
            ResultMessage::Success(success) => Some(&success.common.usage),
            ResultMessage::ErrorDuringExecution(error)
            | ResultMessage::ErrorMaxTurns(error)
            | ResultMessage::ErrorMaxBudgetUsd(error)
            | ResultMessage::ErrorMaxStructuredOutputRetries(error) => Some(&error.common.usage),
            ResultMessage::Unknown(_) => None,
        }
    }

    pub fn result(&self) -> Option<&ResultMessage> {
        self.messages.iter().find_map(|message| match message {
            Message::Result(result) => Some(result),
            _ => None,
        })
    }

    pub fn succeeded(&self) -> bool {
        matches!(self.result(), Some(ResultMessage::Success(_)))
    }

    /// True when the turn carried a message of this shape, named by the same
    /// discriminant string the protocol manifest uses.
    pub fn saw(&self, kind: &str) -> bool {
        self.messages.iter().any(|message| message.kind() == kind)
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn deltas(&self) -> impl Iterator<Item = &StreamDelta> {
        self.stream_events()
            .into_iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn blocks(&self) -> impl Iterator<Item = &ContentBlock> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::Assistant(assistant) => Some(&assistant.message.content),
                _ => None,
            })
            .flatten()
    }
}

/// A specification, and the recording that is its evidence.
pub struct SpecDef {
    /// Capability path, e.g. `control/permission_mode`.
    pub name: &'static str,
    /// The fixture directory holding this specification's recording. One
    /// recording per specification: a specification with none cannot run, and
    /// a recording no specification claims is an orphan.
    pub fixture: &'static str,
    /// Opens the session. Called identically when recording and replaying.
    pub setup: fn() -> SessionSetup,
    /// Drives the session and states the claims.
    pub run: for<'a> fn(&'a mut SpecSession) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
}

static DEFINITIONS: &[&SpecDef] = &[
    &session::TEXT_TURN,
    &session::STREAMED_TURN,
    &session::MULTI_TURN,
    &commands::COMPACTED,
    &commands::CLEARED,
    &control::PERMISSION_MODE_AND_MODEL,
    &control::SESSION_INTROSPECTION,
    &control::SESSION_MAINTENANCE,
    &control::CONNECTED_MCP_SERVERS,
    &tools::PERMISSION_CALLBACK,
    &tools::QUESTION_ASKED,
    &tools::PLAN_REVIEWED,
    &tools::IN_PROCESS_MCP,
    &tools::ELICITATION_ACCEPTED,
    &tools::DIALOG_REQUESTED,
    &tools::HOOK_LIFECYCLE,
    &configured::CONFIGURED_TURN,
    &configured::EVERY_HOOK_EVENT,
    &configured::EFFORTFUL_TURN,
    &agents::SUBAGENT_TASK,
    &history::RESUMED,
    &history::FORKED,
    &history::RESUMED_AT,
    &results::MAX_TURNS,
    &results::MAX_BUDGET,
    &results::INTERRUPTED,
];

const fn entry(name: &'static str, recording: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording,
        allowed_models: ALLOWED_MODELS,
    }
}

static SDK_REGISTRY: &[SpecEntry] = &[
    entry("session/text_turn", "text_turn"),
    entry("session/streamed_turn", "streamed_turn"),
    entry("session/multi_turn", "multi_turn"),
    entry("commands/compacted", "compacted"),
    entry("commands/cleared", "cleared"),
    entry("control/permission_mode_and_model", "controls"),
    entry("control/session_introspection", "introspection"),
    entry("control/session_maintenance", "session_maintenance"),
    entry("control/connected_mcp_servers", "connected_mcp_servers"),
    entry("tools/permission_callback", "permission_callback"),
    entry("tools/question_asked", "question_asked"),
    entry("tools/plan_reviewed", "plan_reviewed"),
    entry("tools/in_process_mcp", "in_process_mcp"),
    entry("tools/elicitation_accepted", "elicitation_accepted"),
    entry("tools/hook_lifecycle", "hook_lifecycle"),
    entry("options/configured_turn", "configured_turn"),
    entry("options/every_hook_event", "every_hook_event"),
    entry("configured/effortful_turn", "effortful_turn"),
    entry("agents/subagent_task", "subagent_task"),
    entry("history/resumed", "resumed"),
    entry("history/forked", "forked"),
    entry("history/resumed_at", "resumed_at"),
    entry("results/max_turns", "max_turns"),
    entry("results/max_budget", "max_budget"),
    entry("results/interrupted", "interrupted"),
];

/// The donor's executable SDK specifications in stable reading order.
pub fn sdk_registry() -> &'static [SpecEntry] {
    SDK_REGISTRY
}

#[cfg(feature = "pty")]
pub fn pty_registry() -> &'static [SpecEntry] {
    pty::registry()
}

pub fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sdk")
}

pub enum SpecSource {
    Live {
        binary: std::path::PathBuf,
        cwd: std::path::PathBuf,
        environment: Option<std::collections::HashMap<String, String>>,
    },
    Recorded {
        replay: StrictReplay,
        transport_order: Vec<String>,
        session_ids: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub provider_version: Option<Version>,
    pub model: String,
    pub session_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("specification {spec} failed: {claim}")]
pub struct SpecFailure {
    pub spec: String,
    pub claim: String,
}

pub async fn run(spec: &SpecEntry, source: SpecSource) -> Result<(), SpecFailure> {
    execute(spec, source).await.map(|_| ())
}

/// Runs one SDK specification and returns capture metadata for `claude-probe`.
#[doc(hidden)]
pub async fn execute(spec: &SpecEntry, source: SpecSource) -> Result<RunReport, SpecFailure> {
    let definition = DEFINITIONS
        .iter()
        .copied()
        .find(|definition| definition.name == spec.name && definition.fixture == spec.recording)
        .ok_or_else(|| failure(spec, "specification is not in the Claude SDK registry"))?;
    let setup = (definition.setup)();
    let model = setup
        .options
        .model
        .clone()
        .ok_or_else(|| failure(spec, "specification does not name a model"))?;
    if !spec.allowed_models.contains(&model.as_str()) {
        return Err(failure(spec, format!("model {model} is not allowed")));
    }

    let (sessions, replay_driver) = match source {
        SpecSource::Live {
            binary,
            cwd,
            environment,
        } => (
            Sessions::live(move |options| {
                options.cli_path = Some(binary.clone());
                options.cwd = Some(cwd.clone());
                options.env = environment.clone();
                let sandbox = serde_json::json!({"enabled": false});
                if let Some(crate::sdk::SettingsConfig::Inline(serde_json::Value::Object(
                    settings,
                ))) = &mut options.settings
                {
                    settings.insert("sandbox".to_owned(), sandbox);
                } else {
                    options.settings = Some(crate::sdk::SettingsConfig::Inline(
                        serde_json::json!({"sandbox": sandbox}),
                    ));
                }
            }),
            None,
        ),
        SpecSource::Recorded {
            mut replay,
            transport_order,
            session_ids,
        } => {
            if transport_order.len() != session_ids.len() {
                return Err(failure(
                    spec,
                    format!(
                        "recording has {} transports but {} session ids",
                        transport_order.len(),
                        session_ids.len()
                    ),
                ));
            }
            let mut transports = Vec::with_capacity(transport_order.len());
            for (transport_id, session_id) in transport_order.into_iter().zip(session_ids) {
                let transport = replay.transports.remove(&transport_id).ok_or_else(|| {
                    failure(spec, format!("recording omits transport {transport_id}"))
                })?;
                transports.push(RecordedTransport {
                    session_id,
                    reader: transport.reader,
                    writer: transport.writer,
                });
            }
            if !replay.transports.is_empty() {
                return Err(failure(spec, "recording has undeclared extra transports"));
            }
            let controller = replay.controller;
            let driver = tokio::spawn(async move {
                while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                    controller.advance_one().await
                {
                    tokio::task::yield_now().await;
                }
            });
            (Sessions::recorded(transports), Some(driver))
        }
    };

    let ledger = sessions.clone();
    let driven = tokio::spawn(async move {
        let mut session = sessions
            .open(setup)
            .await
            .map_err(|error| error.to_string())?;
        (definition.run)(&mut session).await;
        session.close().await;
        Ok::<(), String>(())
    });
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), driven)
        .await
        .map_err(|_| failure(spec, "specification did not finish within 300 seconds"))?
        .map_err(|error| failure(spec, format!("claim task failed: {error}")))?
        .map_err(|claim| failure(spec, claim));
    if let Some(mut driver) = replay_driver
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut driver)
            .await
            .is_err()
    {
        driver.abort();
    }
    outcome?;

    Ok(RunReport {
        provider_version: None,
        model,
        session_ids: ledger.opened(),
    })
}

fn failure(spec: &SpecEntry, claim: impl ToString) -> SpecFailure {
    SpecFailure {
        spec: spec.name.to_string(),
        claim: claim.to_string(),
    }
}
