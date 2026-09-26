//! Claude through the SDK: headless `claude -p` speaking stream-JSON. The
//! facts are the lines Claude writes to stdout, events and control
//! requests alike, and the process's exit. Text, thinking and tool input
//! stream; asks arrive as named control requests, so nothing is inferred.
//!
//! Every user message the interpreter hands to Claude carries a uuid made
//! from its input or envelope id. Claude echoes it on the message's replay,
//! on its `command_lifecycle` frames and as its transcript row's uuid, so a
//! reflection is matched to what was sent by id ([`CORRELATION`]); several
//! messages written while a turn runs fold into that turn, so arrival order
//! would mismatch them.

mod facts;
mod recording;

use std::collections::BTreeMap;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wire::{
    AgentSpec, Ask, BackgroundProcesses, ClaudeAnswer, ClaudeSdkItem, ClaudeSdkSnapshot,
    ContextMeter, ContextShare, DecisionOutcome, FormAction, Input, SignIn, Step, ToolCall,
    ToolDecision, ToolServerHealth, UsageLimits, claude_answer, claude_sdk_input, claude_sdk_item,
    input, permission_answer, plan_answer,
};

use crate::claude_common::{
    Task, describe_asks, describe_tasks, describe_tool, or_dash, task_list,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    RedactTarget, Shared, SnapshotView, Stepped, agent_message_body, agent_message_key,
    claude_sdk_input, human, reason, serde_pb, unknown,
};

/// The interpreter for kind `claude_sdk`.
pub struct ClaudeSdk;

/// How a reflection is matched to the input it reflects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Correlation {
    /// By the uuid the interpreter put on the message.
    ClientId,
    /// By arrival order, one message per turn.
    Fifo,
}

/// Decided by probing Claude 2.1.282: the uuid on a stdin user message is
/// echoed exactly, and messages written during a turn fold into it, so
/// arrival order cannot be trusted.
pub const CORRELATION: Correlation = Correlation::ClientId;

/// The uuid a user message for this input or envelope id carries. Claude
/// wants a well-formed uuid, so the id's first sixteen bytes (zero padded)
/// get version 4 and the RFC 4122 variant; the interpreter remembers the
/// mapping rather than reversing it.
pub fn client_uuid(id: &[u8]) -> String {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(id) {
        *slot = *byte;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::to_hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A message handed to Claude, awaiting or past its reflection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Client {
    #[serde(with = "serde_pb::bytes")]
    id: Vec<u8>,
    /// An agent message, which leaves the pending set when Claude takes it.
    message: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tool {
    at_ms: i64,
    name: String,
    server: String,
    input: String,
    state: i32,
    outcome_text: String,
    outcome_json: String,
    class: i32,
    background: bool,
    decision: Option<ToolDecisionState>,
    ended_at_ms: Option<i64>,
    parent_key: String,
    hidden: bool,
    #[serde(with = "serde_pb::item_body")]
    emitted: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ToolDecisionState {
    outcome: i32,
    scope: String,
    note: String,
}

/// What answering an ask needs beyond the wire Ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    tool_use_id: String,
    input: String,
    /// Each offered scope choice as Claude sent it, to send back.
    suggestions: Vec<String>,
    shape: AskShape,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum AskShape {
    Permission,
    Plan,
    /// The question texts, and each question's option labels.
    Question(Vec<(String, Vec<String>, bool)>),
    Form,
    Link,
}

/// A control request the interpreter sent, awaiting its response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    Interrupt,
    Model(Option<String>),
    Mode(String),
}

/// A subagent or background task Claude reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TaskState {
    at_ms: i64,
    description: String,
    state: i32,
    tool_count: u32,
    last_tool: String,
    tool_key: String,
    tokens: u64,
}

/// Everything the Claude SDK interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<wire::Ask>,
    incarnation: u32,
    session: Option<String>,
    version: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    inits: u32,
    exited: bool,
    clients: BTreeMap<String, Client>,
    /// The message being streamed, and its open blocks by index.
    stream: Option<String>,
    stream_tools: BTreeMap<u32, String>,
    /// Content blocks seen per assistant message, so whole blocks and
    /// streamed ones land on the same key.
    blocks: BTreeMap<String, u32>,
    tools: BTreeMap<String, Tool>,
    asks: BTreeMap<String, AskMeta>,
    requests: BTreeMap<String, Request>,
    next_request: u64,
    next_boundary: u64,
    tasks: Option<Vec<Task>>,
    active_tasks: BTreeMap<String, TaskState>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    context_breakdown: Vec<(String, u64)>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<UsageLimits>,
    #[serde(with = "serde_pb::opt_msg")]
    servers: Option<ToolServerHealth>,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<SignIn>,
    background: Option<u32>,
    /// The newest slash command sent, which a local command's output
    /// belongs to.
    slash: Option<String>,
    /// An interrupt was sent in the running turn.
    interrupted: bool,
}

fn item_body(kind: claude_sdk_item::Kind) -> Vec<u8> {
    ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()
}

fn control_response(request_id: &str, response: Value) -> Effect {
    Effect::ProviderWrite(
        serde_json::to_vec(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        }))
        .expect("json"),
    )
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<Ask> {
        &self.shared
    }

    fn new(spec: &AgentSpec, producer_version: &str) -> Self {
        Self {
            shared: Shared::new(spec, ClaudeSdk::KIND, producer_version),
            incarnation: spec.incarnation,
            session: None,
            version: (!spec.provider_version.is_empty()).then(|| spec.provider_version.clone()),
            model: None,
            effort: None,
            permission_mode: None,
            inits: 0,
            exited: false,
            clients: BTreeMap::new(),
            stream: None,
            stream_tools: BTreeMap::new(),
            blocks: BTreeMap::new(),
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            requests: BTreeMap::new(),
            next_request: 0,
            next_boundary: 0,
            tasks: None,
            active_tasks: BTreeMap::new(),
            context_tokens: None,
            context_window: None,
            context_breakdown: Vec::new(),
            usage: None,
            servers: None,
            sign_in: None,
            background: None,
            slash: None,
            interrupted: false,
        }
    }

    fn body(&self) -> Vec<u8> {
        let context = match self.context_tokens {
            None => unknown::context_meter(),
            Some(used_tokens) => ContextMeter {
                known: true,
                used_tokens,
                window_tokens: self.context_window,
                breakdown: self
                    .context_breakdown
                    .iter()
                    .map(|(category, tokens)| ContextShare {
                        category: category.clone(),
                        tokens: *tokens,
                    })
                    .collect(),
            },
        };
        ClaudeSdkSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(task_list(&self.tasks)),
            context: Some(context),
            model: self.model.clone(),
            effort: self.effort.clone(),
            permission_mode: self.permission_mode.clone(),
            active_tasks: self
                .active_tasks
                .iter()
                .filter(|(_, task)| task.state == wire::TaskState::Running as i32)
                .map(|(id, task)| task.to_wire(id))
                .collect(),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::usage_limits)),
            servers: Some(
                self.servers
                    .clone()
                    .unwrap_or_else(unknown::tool_server_health),
            ),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            background_processes: Some(match self.background {
                None => unknown::background_processes(),
                Some(running) => BackgroundProcesses {
                    known: true,
                    running,
                },
            }),
            provider_session: self.session.clone(),
        }
        .encode_to_vec()
    }

    fn next_request_id(&mut self, request: Request) -> String {
        self.next_request += 1;
        let id = format!("amux-{}", self.next_request);
        self.requests.insert(id.clone(), request);
        id
    }

    fn control_request(&mut self, emit: &mut Emit, request: Request, body: Value) {
        let id = self.next_request_id(request);
        emit.effect(Effect::ProviderWrite(
            serde_json::to_vec(
                &json!({ "type": "control_request", "request_id": id, "request": body }),
            )
            .expect("json"),
        ));
    }

    // --- inputs ----------------------------------------------------------

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        let arm = match input.of {
            Some(input::Of::AgentMessage(envelope)) => {
                // An immediate hand-off to Claude's own queue: a message
                // sent while a turn runs folds into it.
                self.shared.message_accepted(&envelope.id);
                self.clients.insert(
                    client_uuid(&envelope.id),
                    Client {
                        id: envelope.id.clone(),
                        message: true,
                    },
                );
                emit.effect(Effect::Inject {
                    envelope: envelope.clone(),
                    via: Carrier::Stdin,
                });
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: agent_message_key(&envelope.id),
                        text: envelope.text.clone(),
                        input_id: envelope.id.clone(),
                        body: item_body(claude_sdk_item::Kind::AgentMessage(agent_message_body(
                            &envelope,
                        ))),
                        complete: true,
                        ..Default::default()
                    },
                );
                self.shared.accept(emit, &id, true);
                return;
            }
            Some(input::Of::ClaudeSdk(wire::ClaudeSdkInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, reason::UNSUPPORTED),
        };
        match arm {
            claude_sdk_input::Of::Prompt(prompt) => {
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry);
                }
            }
            claude_sdk_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            claude_sdk_input::Of::Interrupt(_) => {
                if self.shared.is_busy() || !self.shared.asks().is_empty() {
                    self.interrupted = true;
                    self.control_request(
                        emit,
                        Request::Interrupt,
                        json!({ "subtype": "interrupt" }),
                    );
                }
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Clear(_) => {
                let entry = wire::QueuedInput {
                    input_id: id.clone(),
                    text: "/clear".into(),
                    ..Default::default()
                };
                let prompt = wire::PromptInput {
                    text: entry.text,
                    ..Default::default()
                };
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry);
                }
            }
            claude_sdk_input::Of::Mode(mode) => {
                self.control_request(
                    emit,
                    Request::Mode(mode.mode.clone()),
                    json!({ "subtype": "set_permission_mode", "mode": mode.mode }),
                );
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Model(model) => {
                let mut body = json!({ "subtype": "set_model" });
                if let Some(name) = &model.model {
                    body["model"] = json!(name);
                }
                self.control_request(emit, Request::Model(model.model), body);
                self.shared.accept(emit, &id, false);
            }
            // Headless Claude takes its effort at launch only.
            claude_sdk_input::Of::Effort(_) => self.shared.reject(emit, &id, reason::UNSUPPORTED),
            claude_sdk_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// Hands a prompt to Claude. Its item is written now: the uuid it
    /// carries is the key Claude's own record of it uses.
    fn submit(&mut self, emit: &mut Emit, entry: wire::QueuedInput) {
        let uuid = client_uuid(&entry.input_id);
        if entry.text.starts_with('/') {
            self.slash = Some(entry.text.clone());
        }
        self.clients.insert(
            uuid.clone(),
            Client {
                id: entry.input_id.clone(),
                message: false,
            },
        );
        emit.effect(Effect::UserMessage {
            uuid: uuid.clone(),
            text: entry.text.clone(),
            attachments: entry.attachments.clone(),
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: uuid,
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(claude_sdk_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// An answer through amux: checked against the ask, then sent as the
    /// control response. The ask closes with the outcome it carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: wire::AnswerInput) {
        let key = answer.ask_key;
        if self.shared.asks().get(&key).is_none() {
            self.shared.answer(emit, id, &key);
            return;
        }
        let parsed = ClaudeAnswer::decode(answer.body.as_slice())
            .ok()
            .and_then(|answer| answer.of);
        let Some((response, decision)) = self
            .asks
            .get(&key)
            .and_then(|meta| sdk_answer(meta, parsed?))
        else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        self.shared.answer(emit, id, &key);
        let meta = self.asks.remove(&key);
        emit.effect(control_response(&key, response));
        if let (Some(meta), Some(decision)) = (meta, decision) {
            self.decide(emit, &meta.tool_use_id, decision);
        }
        self.shared.accept(emit, id, false);
    }

    fn decide(&mut self, emit: &mut Emit, tool_use_id: &str, decision: ToolDecisionState) {
        if let Some(tool) = self.tools.get_mut(tool_use_id) {
            tool.decision = Some(decision);
            self.emit_tool(emit, tool_use_id);
        }
    }

    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if tool.hidden {
            return;
        }
        let body = item_body(claude_sdk_item::Kind::Tool(ToolCall {
            name: tool.name.clone(),
            input_json: tool.input.clone().into_bytes(),
            state: tool.state,
            outcome_text: tool.outcome_text.clone(),
            outcome_json: tool.outcome_json.clone().into_bytes(),
            attachments: Vec::new(),
            class: tool.class,
            decision: tool.decision.as_ref().map(|decision| ToolDecision {
                outcome: decision.outcome,
                scope: decision.scope.clone(),
                note: decision.note.clone(),
                elsewhere: false,
            }),
            background: tool.background,
            parent_key: tool.parent_key.clone(),
            subagent: None,
            server: tool.server.clone(),
            exit_code: None,
            ended_at_ms: tool.ended_at_ms,
        }));
        if body == tool.emitted {
            return;
        }
        tool.emitted = body.clone();
        let at_ms = tool.at_ms;
        self.shared.item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                body,
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn boundary(&mut self, emit: &mut Emit, kind: wire::BoundaryKind, cause: String) {
        self.next_boundary += 1;
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("boundary:{}", self.next_boundary),
                body: item_body(claude_sdk_item::Kind::Boundary(wire::Boundary {
                    kind: kind as i32,
                    provider_session: self.session.clone().unwrap_or_default(),
                    provider_version: self.version.clone().unwrap_or_default(),
                    cause,
                    keymap: String::new(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }
}

impl TaskState {
    fn to_wire(&self, id: &str) -> wire::Task {
        wire::Task {
            task_id: id.to_owned(),
            description: self.description.clone(),
            state: self.state,
            tool_count: self.tool_count,
            last_tool: self.last_tool.clone(),
            tool_key: self.tool_key.clone(),
            tokens: self.tokens,
        }
    }
}

/// The control response an answer sends and the decision it puts on the
/// call's row, or None when the answer does not fit the ask.
fn sdk_answer(
    meta: &AskMeta,
    answer: claude_answer::Of,
) -> Option<(Value, Option<ToolDecisionState>)> {
    let input = serde_json::from_str::<Value>(&meta.input).unwrap_or(Value::Null);
    let decided = |outcome: DecisionOutcome, scope: &str, note: &str| {
        Some(ToolDecisionState {
            outcome: outcome as i32,
            scope: scope.to_owned(),
            note: note.to_owned(),
        })
    };
    let deny = |note: &str, stop: bool| {
        json!({
            "behavior": "deny",
            "message": if note.is_empty() { "The person denied this." } else { note },
            "interrupt": stop,
        })
    };
    match (&meta.shape, answer) {
        (AskShape::Permission, claude_answer::Of::Permission(permission)) => match permission.of? {
            permission_answer::Of::Allow(allow) => {
                let mut response = json!({ "behavior": "allow", "updatedInput": input });
                let mut scope = String::new();
                if let Some(index) = allow.scope {
                    let suggestion =
                        serde_json::from_str::<Value>(meta.suggestions.get(index as usize)?)
                            .ok()?;
                    scope = suggestion
                        .get("destination")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    response["updatedPermissions"] = json!([suggestion]);
                }
                Some((response, decided(DecisionOutcome::Allowed, &scope, "")))
            }
            permission_answer::Of::Deny(no) => Some((
                deny(&no.note, no.stop),
                decided(DecisionOutcome::Denied, "", &no.note),
            )),
        },
        (AskShape::Plan, claude_answer::Of::Plan(plan)) => match plan.of? {
            plan_answer::Of::Approve(approve) => Some((
                json!({
                    "behavior": "allow",
                    "updatedInput": input,
                    "updatedPermissions": [{
                        "type": "setMode",
                        "mode": if approve.auto_accept_edits { "acceptEdits" } else { "default" },
                        "destination": "session",
                    }],
                }),
                decided(DecisionOutcome::Allowed, "", ""),
            )),
            plan_answer::Of::SendBack(send_back) => Some((
                deny(&send_back.note, false),
                decided(DecisionOutcome::Denied, "", &send_back.note),
            )),
        },
        (AskShape::Question(questions), claude_answer::Of::Question(answer)) => {
            if answer.answers.len() != questions.len() {
                return None;
            }
            let mut answers = serde_json::Map::new();
            for ((question, labels, multi), response) in questions.iter().zip(&answer.answers) {
                let mut picked = Vec::new();
                for index in &response.selected {
                    picked.push(labels.get(*index as usize)?.clone());
                }
                picked.extend(response.other.clone());
                if picked.is_empty() || (!multi && picked.len() > 1) {
                    return None;
                }
                answers.insert(question.clone(), json!(picked.join(", ")));
            }
            let mut updated = input;
            updated["answers"] = Value::Object(answers);
            Some((
                json!({ "behavior": "allow", "updatedInput": updated }),
                decided(DecisionOutcome::Allowed, "", &answer.note),
            ))
        }
        (AskShape::Form, claude_answer::Of::Form(form)) => {
            let action = form_action(form.action)?;
            let mut response = json!({ "action": action });
            if action == "accept" {
                response["content"] =
                    serde_json::from_slice(&form.content_json).unwrap_or(json!({}));
            }
            Some((response, None))
        }
        (AskShape::Link, claude_answer::Of::Link(link)) => {
            Some((json!({ "action": form_action(link.action)? }), None))
        }
        _ => None,
    }
}

fn form_action(action: i32) -> Option<&'static str> {
    Some(match FormAction::try_from(action).ok()? {
        FormAction::Accept => "accept",
        FormAction::Decline => "decline",
        FormAction::Cancel => "cancel",
        FormAction::Unspecified => return None,
    })
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

impl Interpreter for ClaudeSdk {
    type State = State;
    const KIND: &'static str = "claude_sdk";

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn step(state: &mut State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            Event::ProviderExit { code } => state.exited(&mut emit, code),
            // Draining and stopping are the agent process's; the provider's
            // facts report what follows.
            Event::DaemonLost | Event::StopRequested(_) => {}
        }
        if let Some(entry) = state.shared.next_queued() {
            state.submit(&mut emit, entry);
        }
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<
            wire::ClaudeSdkItem,
            wire::ClaudeSdkSnapshot,
            wire::ClaudeAnswer,
            State,
        >(target)
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude_sdk().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_sdk_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_item(body: &[u8]) -> ItemView {
    use claude_sdk_item::Kind;
    let item = ClaudeSdkItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thinking(thinking)) => ("thinking", thinking.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", true, describe_tool(&tool)),
        Some(Kind::Task(task)) => (
            "task",
            true,
            format!(
                "{} {} {} tools={} last={} tokens={} call={}",
                task.task_id,
                wire::TaskState::try_from(task.state).map_or("?", |state| state.as_str_name()),
                Value::String(task.description),
                task.tool_count,
                or_dash(&task.last_tool),
                task.tokens,
                or_dash(&task.tool_key)
            ),
        ),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={} cost={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms,
                turn.cost_usd
                    .map_or("-".to_owned(), |cost| format!("{cost:.6}"))
            ),
        ),
        Some(Kind::Status(status)) => ("status", true, status.status),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{} session={} version={}{}",
                wire::BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                or_dash(&boundary.provider_session),
                or_dash(&boundary.provider_version),
                if boundary.cause.is_empty() {
                    String::new()
                } else {
                    format!(" cause={}", Value::String(boundary.cause))
                }
            ),
        ),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            format!("envelope={}", crate::to_hex(&message.envelope_id)),
        ),
        Some(Kind::ApiError(error)) => (
            "api_error",
            true,
            format!(
                "{} {} retry={} {}/{}{}",
                error.error_kind,
                Value::String(error.message),
                error.will_retry,
                error.attempt,
                error.max_attempts,
                error
                    .retry_at_ms
                    .map_or(String::new(), |at| format!(" at={at}"))
            ),
        ),
        Some(Kind::Slash(slash)) => (
            "slash",
            true,
            format!("{} args={}", slash.command, Value::String(slash.args)),
        ),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                Value::String(unrecognized.summary)
            ),
        ),
        Some(Kind::ModelSwitch(switch)) => (
            "model_switch",
            true,
            format!(
                "{} -> {} {}",
                switch.from,
                switch.to,
                Value::String(switch.reason)
            ),
        ),
        Some(Kind::Compaction(compaction)) => (
            "compaction",
            true,
            format!(
                "{}→{}{}",
                compaction
                    .tokens_before
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                compaction
                    .tokens_after
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                if compaction.automatic { " auto" } else { "" }
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = ClaudeSdkSnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    let usage = snapshot.usage.unwrap_or_default();
    let servers = snapshot.servers.unwrap_or_default();
    let sign_in = snapshot.sign_in.unwrap_or_default();
    let background = snapshot.background_processes.unwrap_or_default();
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "asks=[{}] session={} model={} effort={} mode={} context={} tasks={} active=[{}] usage={} servers={} sign_in={} background={}",
            describe_asks(&snapshot.asks),
            snapshot.provider_session.as_deref().unwrap_or("?"),
            snapshot.model.as_deref().unwrap_or("?"),
            snapshot.effort.as_deref().unwrap_or("?"),
            snapshot.permission_mode.as_deref().unwrap_or("?"),
            if context.known {
                format!(
                    "{}/{}{}",
                    context.used_tokens,
                    context
                        .window_tokens
                        .map_or("?".to_owned(), |window| window.to_string()),
                    if context.breakdown.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} categories)", context.breakdown.len())
                    }
                )
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.tasks.unwrap_or_default()),
            snapshot
                .active_tasks
                .iter()
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>()
                .join(","),
            match wire::UsageState::try_from(usage.state).unwrap_or_default() {
                wire::UsageState::Unknown => "?".to_owned(),
                state => format!(
                    "{}{}",
                    state.as_str_name(),
                    usage
                        .windows
                        .iter()
                        .map(|window| format!(" {}:{:.0}%", window.name, window.used_percent))
                        .collect::<String>()
                ),
            },
            match wire::HealthState::try_from(servers.state).unwrap_or_default() {
                wire::HealthState::Unknown => "?".to_owned(),
                state => format!(
                    "{}[{}]",
                    state.as_str_name(),
                    servers
                        .servers
                        .iter()
                        .map(|server| format!(
                            "{}:{}",
                            server.name,
                            wire::ToolServerStatus::try_from(server.status)
                                .map_or("?", |status| status.as_str_name())
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            },
            match wire::SignInState::try_from(sign_in.state).unwrap_or_default() {
                wire::SignInState::Unknown => "?".to_owned(),
                state => format!("{} {}", state.as_str_name(), or_dash(&sign_in.account)),
            },
            if background.known {
                background.running.to_string()
            } else {
                "?".into()
            }
        ),
    }
}
