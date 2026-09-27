//! Claude in a terminal. The facts are rows of Claude's transcript file,
//! hook payloads on the agent's hooks socket, the agent process's own launch
//! fact and the PTY's exit. Rows arrive whole, so nothing here streams.
//!
//! Terminal Claude has no protocol for asks, so this is the one interpreter
//! that infers them, and every inference rule lives in this module:
//!
//! - A tool call is keyed by its tool-use id. PreToolUse and PostToolUse
//!   hooks usually land before the transcript row for the same call; the
//!   item is emitted from whichever arrives first and revised, same key,
//!   when the other lands.
//! - An ask opens on the PermissionRequest hook, which names no tool-use id.
//!   It points at the running call with the same tool and input when a
//!   PreToolUse hook announced one, and otherwise at the call whose row
//!   lands later with that tool and input.
//! - An ask closes on an answer sent through amux (outcome known), on the
//!   call's PostToolUse hook or its tool result (answered in the terminal),
//!   or on a later fact that proves the call is over without saying how: a
//!   new prompt, an interruption, the turn's end, a session change, the
//!   provider exiting, or a row from a later assistant message (outcome
//!   unknown, drawn dismissed). Nothing else closes one; a tick never does.
//! - A subagent's calls reach the hooks socket carrying its agent id; they
//!   are its steps, counted on the Agent row that started it, never rows of
//!   their own. An ask a subagent raises points at that Agent row and closes
//!   on the subagent's own result for the call. A subagent Claude runs in the
//!   background answers the Agent call at once with launch metadata; its row
//!   stays running, across the end of the turn that launched it, until a
//!   user row of task-notification origin carries its result.
//! - A tool server's form or link that Claude shows in its own terminal is
//!   announced only by a Notification hook, which no hook can answer. It
//!   opens an unanswerable ask with an item of its own, pointing at the call
//!   that was running (a tool server's, when one was). An interrupt through
//!   amux closes it cancelled; that call's result, a row from a later
//!   assistant message, and the facts that close every ask close it
//!   dismissed.

mod facts;
mod recording;

use std::collections::BTreeMap;
use std::fmt;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, Attachment, BackgroundProcesses, Boundary, BoundaryKind,
    ClaudeAnswer, ClaudePtyItem, ClaudePtySnapshot, ContextMeter, DecisionOutcome, Input, KeyName,
    Step, SubagentProgress, ToolCall, ToolDecision, claude_answer, claude_pty_input,
    claude_pty_item, input, permission_answer, plan_answer,
};

use crate::claude_common::{
    Task, describe_asks, describe_tasks, describe_tool, or_dash, same_json, split_tool_name,
    task_list,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    RedactTarget, SendOutcome, Shared, SnapshotView, Stepped, agent_message_body,
    agent_message_key, ask_item, claude_pty_input, human, is_send_tool, reason, sent_message,
    serde_pb, unknown,
};

/// The interpreter for kind `claude_pty`.
pub struct ClaudePty;

/// A semantic input for the agent process to type into Claude's terminal.
/// The agent process turns it into bytes with the keymap it resolved for
/// the Claude version it launched; the shapes here are what that keymap's
/// programs need to know about the ask being answered.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalInput {
    /// Paste the text and submit it.
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// Stop the running turn.
    Interrupt,
    /// Start a new conversation: /clear.
    Clear,
    Key(KeyName),
    /// Answer the permission menu. `suggestions` is how many scope choices
    /// the menu offers.
    Permission {
        suggestions: u32,
        choice: PermissionChoice,
    },
    /// Answer the plan menu.
    Plan(PlanChoice),
    /// Fill in the question form, one answer per question in order.
    Question {
        questions: Vec<QuestionShape>,
        answers: Vec<QuestionChoice>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionChoice {
    AllowOnce,
    /// The scope choice at this index of the ask's offer.
    AllowScoped {
        suggestion: u32,
    },
    /// Terminal Claude always ends the turn on a deny.
    Deny {
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanChoice {
    ApproveAutoAcceptEdits,
    Approve,
    SendBack { note: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionShape {
    pub options: u32,
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionChoice {
    pub selected: Vec<u32>,
    pub other: Option<String>,
}

impl fmt::Display for TerminalInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prompt { text, attachments } => {
                write!(f, "prompt {}", Value::String(text.clone()))?;
                if !attachments.is_empty() {
                    write!(f, " attachments={}", attachments.len())?;
                }
                Ok(())
            }
            Self::Interrupt => f.write_str("interrupt"),
            Self::Clear => f.write_str("clear"),
            Self::Key(key) => write!(f, "key {}", key.as_str_name()),
            Self::Permission {
                suggestions,
                choice,
            } => write!(f, "permission of {suggestions} {choice:?}"),
            Self::Plan(choice) => write!(f, "plan {choice:?}"),
            Self::Question { questions, answers } => {
                write!(f, "question")?;
                for (shape, answer) in questions.iter().zip(answers) {
                    write!(
                        f,
                        " [{} of {}{}{}]",
                        answer
                            .selected
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                        shape.options,
                        if shape.multi_select { " multi" } else { "" },
                        answer
                            .other
                            .as_ref()
                            .map(|other| format!(" other={}", Value::String(other.clone())))
                            .unwrap_or_default()
                    )?;
                }
                Ok(())
            }
        }
    }
}

/// The launch fact the agent process sends on the agent channel before the
/// provider's first fact, and again after it relaunches the provider.
pub fn launch_fact(version: &str, keymap: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "type": "launch",
        "version": version,
        "keymap": keymap,
    }))
    .expect("json")
}

/// The fact the agent process sends when Claude's terminal first draws with
/// bracketed paste on: its input is live, so a prompt can be typed. Some
/// versions report their session start only after the first prompt, so
/// nothing else says a new terminal takes input.
pub fn ready_fact() -> Vec<u8> {
    br#"{"type":"ready"}"#.to_vec()
}

/// What the interpreter knows about the Claude process and its session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Provider {
    session: Option<String>,
    transcript: Option<String>,
    version: Option<String>,
    keymap: String,
    launches: u32,
    /// Launched again after an earlier launch; the next session start is a
    /// restart whatever its source says.
    relaunched: bool,
    model: Option<String>,
    permission_mode: Option<String>,
}

/// A tool call as it stands, whichever of hook or row reported it first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tool {
    /// Arrival order, so the newest running call can be found.
    seq: u64,
    at_ms: i64,
    name: String,
    server: String,
    input: String,
    state: i32,
    outcome_text: String,
    outcome_json: String,
    class: i32,
    background: bool,
    decision: Option<Decision>,
    ended_at_ms: Option<i64>,
    /// The assistant message the call's row belongs to, once it landed.
    message_id: Option<String>,
    /// A result arrived: a tool result row or a PostToolUse hook.
    finished: bool,
    /// Drawn elsewhere than as a tool row: the status tool sets working_on
    /// and the task tools the task list.
    hidden: bool,
    /// An Agent call's subagent: the steps it has taken so far.
    subagent: Option<Subagent>,
    /// A subagent launched in the background: the call returned, and the
    /// task notification carries the answer.
    awaiting_notification: bool,
    /// The body last emitted, so an unchanged revision is not re-emitted.
    /// Images the tool read, by the blobs that hold them.
    #[serde(with = "serde_pb::msgs")]
    images: Vec<Attachment>,
    #[serde(with = "serde_pb::item_body")]
    emitted: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Subagent {
    tool_count: u32,
    last_tool: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Decision {
    outcome: i32,
    scope: String,
    note: String,
    elsewhere: bool,
}

impl Decision {
    fn elsewhere(outcome: DecisionOutcome) -> Self {
        Self {
            outcome: outcome as i32,
            scope: String::new(),
            note: String::new(),
            elsewhere: true,
        }
    }

    fn unknown() -> Self {
        Self {
            outcome: DecisionOutcome::Unknown as i32,
            scope: String::new(),
            note: String::new(),
            elsewhere: false,
        }
    }

    fn to_wire(&self) -> ToolDecision {
        ToolDecision {
            outcome: self.outcome,
            scope: self.scope.clone(),
            note: self.note.clone(),
            elsewhere: self.elsewhere,
        }
    }
}

/// What the interpreter keeps about an ask beside the wire Ask: how to
/// find its tool call and how the terminal's menu is shaped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    seq: u64,
    tool_name: String,
    input: String,
    shape: AskShape,
    /// The tool-use id of the call it points at.
    bound: Option<String>,
    /// Closed before its call's row landed: the decision waits for it.
    closed: Option<Decision>,
    /// Raised inside the subagent with this agent id: never bound to a
    /// call of the main conversation.
    agent: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum AskShape {
    /// The destination of each scope choice, in offer order.
    Permission {
        scopes: Vec<String>,
    },
    Plan,
    Question {
        questions: Vec<QuestionShape>,
    },
    /// A dialog only Claude's own terminal can answer, raised while this
    /// call ran.
    Unanswerable {
        call: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PendingMessage {
    #[serde(with = "serde_pb::bytes")]
    id: Vec<u8>,
    text: String,
}

/// The newest slash command's item, which its output row fills in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Slash {
    key: String,
    command: String,
    args: String,
    #[serde(with = "serde_pb::bytes")]
    input_id: Vec<u8>,
    at_ms: i64,
    output: Option<String>,
}

/// Everything the Claude PTY interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<Ask>,
    provider: Provider,
    tools: BTreeMap<String, Tool>,
    asks: BTreeMap<String, AskMeta>,
    seq: u64,
    next_ask: u64,
    next_boundary: u64,
    tasks: Option<Vec<Task>>,
    context_tokens: Option<u64>,
    background: Option<u32>,
    messages: Vec<PendingMessage>,
    /// Each subagent's agent id and the tool-use id of its Agent call.
    agents: BTreeMap<String, String>,
    slash: Option<Slash>,
    /// The running turn began with a local command, not a model request.
    local_turn: bool,
}

fn item_body(kind: claude_pty_item::Kind) -> Vec<u8> {
    ClaudePtyItem { kind: Some(kind) }.encode_to_vec()
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<Ask> {
        &self.shared
    }

    fn new(spec: &AgentSpec, producer_version: &str) -> Self {
        Self {
            shared: Shared::new(spec, ClaudePty::KIND, producer_version),
            provider: Provider::default(),
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            seq: 0,
            next_ask: 0,
            next_boundary: 0,
            tasks: None,
            context_tokens: None,
            background: None,
            messages: Vec::new(),
            agents: BTreeMap::new(),
            slash: None,
            local_turn: false,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn body(&self) -> Vec<u8> {
        let tasks = task_list(&self.tasks);
        let context = match self.context_tokens {
            None => unknown::context_meter(),
            Some(used_tokens) => ContextMeter {
                known: true,
                used_tokens,
                window_tokens: None,
                breakdown: Vec::new(),
            },
        };
        let background_processes = match self.background {
            None => unknown::background_processes(),
            Some(running) => BackgroundProcesses {
                known: true,
                running,
            },
        };
        ClaudePtySnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(tasks),
            context: Some(context),
            model: self.provider.model.clone(),
            permission_mode: self.provider.permission_mode.clone(),
            provider_session: self.provider.session.clone(),
            background_processes: Some(background_processes),
            ..unknown::claude_pty()
        }
        .encode_to_vec()
    }

    // --- tools -----------------------------------------------------------

    /// Emits a tool call's item if it is drawn and changed since the last
    /// emission.
    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if tool.hidden {
            return;
        }
        if is_send_tool(&tool.server, &tool.name) {
            let (text, message) = sent_message(
                tool.input.as_bytes(),
                SendOutcome::of(
                    wire::ToolState::try_from(tool.state).unwrap_or_default(),
                    &tool.outcome_text,
                ),
            );
            let body = item_body(claude_pty_item::Kind::AgentMessage(message));
            if body == tool.emitted {
                return;
            }
            tool.emitted = body.clone();
            let at_ms = tool.at_ms;
            return self.shared.item(
                emit,
                ItemDraft {
                    key: id.to_owned(),
                    text,
                    body,
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        let body = item_body(claude_pty_item::Kind::Tool(ToolCall {
            name: tool.name.clone(),
            input_json: tool.input.clone().into_bytes(),
            state: tool.state,
            outcome_text: tool.outcome_text.clone(),
            outcome_json: tool.outcome_json.clone().into_bytes(),
            attachments: tool.images.clone(),
            class: tool.class,
            decision: tool.decision.as_ref().map(Decision::to_wire),
            background: tool.background,
            parent_key: String::new(),
            subagent: tool.subagent.as_ref().map(|subagent| SubagentProgress {
                tool_count: subagent.tool_count,
                last_tool: subagent.last_tool.clone(),
                finished: tool.finished,
            }),
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

    // --- asks ------------------------------------------------------------

    /// Points an ask at a call: the wire ask gains its item key, or a
    /// decision reached before the call's row landed goes onto the call.
    fn bind(&mut self, emit: &mut Emit, ask_key: &str, tool_id: &str) {
        let Some(meta) = self.asks.get_mut(ask_key) else {
            return;
        };
        meta.bound = Some(tool_id.to_owned());
        match meta.closed.clone() {
            Some(decision) => {
                self.asks.remove(ask_key);
                self.decide(emit, tool_id, decision);
            }
            None => {
                if let Some(open) = self.shared.asks().get(ask_key) {
                    let ask = Ask {
                        item_key: tool_id.to_owned(),
                        ..open.clone()
                    };
                    self.shared.open_ask(ask);
                }
            }
        }
    }

    fn decide(&mut self, emit: &mut Emit, tool_id: &str, decision: Decision) {
        if let Some(tool) = self.tools.get_mut(tool_id) {
            tool.decision = Some(decision);
            self.emit_tool(emit, tool_id);
        }
    }

    /// Closes an ask with the outcome the closing fact carries.
    fn close(&mut self, emit: &mut Emit, ask_key: &str, decision: Decision) {
        if let Some(AskShape::Unanswerable { .. }) = self.asks.get(ask_key).map(|meta| &meta.shape)
        {
            return self.close_unanswerable(emit, ask_key, ask_item::dismissed());
        }
        self.shared.close_ask(ask_key);
        let Some(meta) = self.asks.get_mut(ask_key) else {
            return;
        };
        match meta.bound.clone() {
            Some(tool_id) => {
                self.asks.remove(ask_key);
                self.decide(emit, &tool_id, decision);
            }
            None if meta.agent.is_some() => {
                self.asks.remove(ask_key);
            }
            None => meta.closed = Some(decision),
        }
    }

    fn open_ask_keys(&self) -> Vec<String> {
        self.shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .collect()
    }

    /// A fact that proves every open ask is over without saying how.
    fn close_all_unknown(&mut self, emit: &mut Emit) {
        for key in self.open_ask_keys() {
            self.close(emit, &key, Decision::unknown());
        }
    }

    /// Writes an unanswerable ask's own item, open or closed.
    fn emit_unanswerable(&mut self, emit: &mut Emit, ask: &Ask, closed: Option<AskClosed>) {
        let Some(wire::ask::Body::Unanswerable(unanswerable)) = &ask.body else {
            return;
        };
        let item = ask_item::opened(wire::ask_item::Ask::Unanswerable(unanswerable.clone()));
        let item = match closed {
            Some(closed) => ask_item::close(item, closed),
            None => item,
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: ask.item_key.clone(),
                body: item_body(claude_pty_item::Kind::Ask(item)),
                at_ms: Some(ask.opened_at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn close_unanswerable(&mut self, emit: &mut Emit, ask_key: &str, closed: AskClosed) {
        self.asks.remove(ask_key);
        if let Some(ask) = self.shared.close_ask(ask_key) {
            self.emit_unanswerable(emit, &ask, Some(closed));
        }
    }

    /// The open unanswerable asks and the call each was raised under.
    fn unanswerable_asks(&self) -> Vec<(String, Option<String>)> {
        self.asks
            .iter()
            .filter_map(|(key, meta)| match &meta.shape {
                AskShape::Unanswerable { call } => Some((key.clone(), call.clone())),
                _ => None,
            })
            .collect()
    }

    /// A call's result ends the dialog raised while it ran, or one raised
    /// while no call ran.
    fn close_unanswerable_for_tool(&mut self, emit: &mut Emit, tool_id: &str) {
        for (key, call) in self.unanswerable_asks() {
            if call.as_deref().is_none_or(|call| call == tool_id) {
                self.close_unanswerable(emit, &key, ask_item::dismissed());
            }
        }
    }

    /// The call's own result closes the asks that point at it.
    fn close_for_tool(&mut self, emit: &mut Emit, tool_id: &str, outcome: DecisionOutcome) {
        for key in self.open_ask_keys() {
            if self.asks.get(&key).and_then(|meta| meta.bound.as_deref()) == Some(tool_id) {
                self.close(emit, &key, Decision::elsewhere(outcome));
            }
        }
    }

    /// The running, undecided call an ask for this tool and input points
    /// at: the newest with equal input, else the newest with the tool.
    fn tool_for_ask(&self, name: &str, input: &Value) -> Option<String> {
        let bound = self
            .asks
            .values()
            .filter_map(|meta| meta.bound.as_deref())
            .collect::<Vec<_>>();
        let candidates = self
            .tools
            .iter()
            .filter(|(id, tool)| {
                tool.name_matches(name)
                    && !tool.hidden
                    && !tool.finished
                    && tool.decision.is_none()
                    && !bound.contains(&id.as_str())
            })
            .collect::<Vec<_>>();
        let newest = |exact: bool| {
            candidates
                .iter()
                .filter(|(_, tool)| !exact || same_json(&tool.input, input))
                .max_by_key(|(_, tool)| tool.seq)
                .map(|(id, _)| (*id).clone())
        };
        newest(true).or_else(|| newest(false))
    }

    /// The unbound ask a newly seen call answers to: the oldest with equal
    /// input, else the oldest for the tool.
    fn ask_for_tool(&self, name: &str, input: &Value) -> Option<String> {
        let candidates = self
            .asks
            .iter()
            .filter(|(_, meta)| {
                meta.bound.is_none() && meta.agent.is_none() && meta.tool_name == name
            })
            .collect::<Vec<_>>();
        let oldest = |exact: bool| {
            candidates
                .iter()
                .filter(|(_, meta)| !exact || same_json(&meta.input, input))
                .min_by_key(|(_, meta)| meta.seq)
                .map(|(key, _)| (*key).clone())
        };
        oldest(true).or_else(|| oldest(false))
    }

    // --- items -----------------------------------------------------------

    /// Cancels the running turn with the interrupt key, which also
    /// dismisses an open ask's dialog.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.shared.is_busy() || !self.shared.asks().is_empty() {
            emit.effect(Effect::Terminal(TerminalInput::Interrupt));
        }
        // The interrupt key cancels a dialog Claude shows in its terminal
        // whether or not the turn goes on, and nothing else reports that.
        for (key, _) in self.unanswerable_asks() {
            self.close_unanswerable(emit, &key, ask_item::outcome(wire::AskOutcome::Cancelled));
        }
    }

    fn boundary(&mut self, emit: &mut Emit, kind: BoundaryKind, cause: String) {
        self.next_boundary += 1;
        let key = format!("boundary:{}", self.next_boundary);
        self.shared.item(
            emit,
            ItemDraft {
                key,
                body: item_body(claude_pty_item::Kind::Boundary(Boundary {
                    kind: kind as i32,
                    provider_session: self.provider.session.clone().unwrap_or_default(),
                    provider_version: self.provider.version.clone().unwrap_or_default(),
                    cause,
                    keymap: self.provider.keymap.clone(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn emit_slash(&mut self, emit: &mut Emit) {
        let Some(slash) = &self.slash else {
            return;
        };
        let draft = ItemDraft {
            key: slash.key.clone(),
            text: slash.output.clone().unwrap_or_default(),
            input_id: slash.input_id.clone(),
            body: item_body(claude_pty_item::Kind::Slash(wire::SlashOutput {
                command: slash.command.clone(),
                args: slash.args.clone(),
            })),
            at_ms: Some(slash.at_ms),
            complete: true,
            ..Default::default()
        };
        self.shared.item(emit, draft);
    }

    // --- inputs ----------------------------------------------------------

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        let arm = match input.of {
            Some(input::Of::AgentMessage(envelope)) => {
                self.shared.message_accepted(&envelope.id);
                self.messages.push(PendingMessage {
                    id: envelope.id.clone(),
                    text: envelope.text.clone(),
                });
                emit.effect(Effect::Inject {
                    envelope: envelope.clone(),
                    via: Carrier::MessagingSocket,
                });
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: agent_message_key(&envelope.id),
                        text: envelope.text.clone(),
                        input_id: envelope.id.clone(),
                        body: item_body(claude_pty_item::Kind::AgentMessage(agent_message_body(
                            &envelope,
                        ))),
                        complete: true,
                        ..Default::default()
                    },
                );
                self.shared.accept(emit, &id, true);
                return;
            }
            Some(input::Of::ClaudePty(wire::ClaudePtyInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, reason::UNSUPPORTED),
        };
        match arm {
            claude_pty_input::Of::Prompt(prompt) => {
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    emit.effect(Effect::Terminal(TerminalInput::Prompt {
                        text: entry.text,
                        attachments: entry.attachments,
                    }));
                }
            }
            claude_pty_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            // Typed while the turn runs, a prompt joins it at the next tool
            // boundary, as Codex's steer and headless Claude's mid-turn
            // message do. Claude's send-now chord is not used: it moves a
            // running command to the background or cancels the reply being
            // written. Nothing is typed over an open menu.
            claude_pty_input::Of::SendNow(send) => {
                let running = self.shared.is_busy() && self.shared.asks().is_empty();
                if let Some(entry) = self
                    .shared
                    .send_now(emit, &id, &send.queued_input_id, running)
                {
                    emit.effect(Effect::Terminal(TerminalInput::Prompt {
                        text: entry.text,
                        attachments: entry.attachments,
                    }));
                }
            }
            claude_pty_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            claude_pty_input::Of::Clear(_) => {
                emit.effect(Effect::Terminal(TerminalInput::Clear));
                self.shared.accept(emit, &id, false);
            }
            claude_pty_input::Of::Key(key) => match KeyName::try_from(key.key) {
                Ok(name) if name != KeyName::Unspecified => {
                    emit.effect(Effect::Terminal(TerminalInput::Key(name)));
                    self.shared.accept(emit, &id, false);
                }
                _ => self.shared.reject(emit, &id, reason::UNSUPPORTED),
            },
            claude_pty_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// An answer through amux: checked against the open ask's shape, then
    /// typed into the terminal's menu. The ask closes with the outcome the
    /// answer carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: wire::AnswerInput) {
        let key = answer.ask_key;
        if self.shared.asks().get(&key).is_none() {
            self.shared.answer(emit, id, &key);
            return;
        }
        let parsed = ClaudeAnswer::decode(answer.body.as_slice())
            .ok()
            .and_then(|answer| answer.of);
        let Some((terminal, decision)) = self
            .asks
            .get(&key)
            .and_then(|meta| terminal_answer(&meta.shape, parsed?))
        else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        self.shared.answer(emit, id, &key);
        self.close(emit, &key, decision);
        emit.effect(Effect::Terminal(terminal));
        self.shared.accept(emit, id, false);
    }
}

impl Tool {
    fn name_matches(&self, name: &str) -> bool {
        let (server, tool) = split_tool_name(name);
        self.name == tool && self.server == server
    }
}

/// The keystrokes an answer stands for and the decision it records, or
/// None when the answer does not fit the ask.
fn terminal_answer(
    shape: &AskShape,
    answer: claude_answer::Of,
) -> Option<(TerminalInput, Decision)> {
    let decision = |outcome: DecisionOutcome, scope: String, note: String| Decision {
        outcome: outcome as i32,
        scope,
        note,
        elsewhere: false,
    };
    match (shape, answer) {
        (AskShape::Permission { scopes }, claude_answer::Of::Permission(permission)) => {
            let suggestions = scopes.len() as u32;
            match permission.of? {
                permission_answer::Of::Allow(allow) => match allow.scope {
                    None => Some((
                        TerminalInput::Permission {
                            suggestions,
                            choice: PermissionChoice::AllowOnce,
                        },
                        decision(DecisionOutcome::Allowed, String::new(), String::new()),
                    )),
                    Some(index) => Some((
                        TerminalInput::Permission {
                            suggestions,
                            choice: PermissionChoice::AllowScoped { suggestion: index },
                        },
                        decision(
                            DecisionOutcome::Allowed,
                            scopes.get(index as usize)?.clone(),
                            String::new(),
                        ),
                    )),
                },
                permission_answer::Of::Deny(deny) => Some((
                    TerminalInput::Permission {
                        suggestions,
                        choice: PermissionChoice::Deny {
                            note: deny.note.clone(),
                        },
                    },
                    decision(DecisionOutcome::Denied, String::new(), deny.note),
                )),
            }
        }
        (AskShape::Plan, claude_answer::Of::Plan(plan)) => match plan.of? {
            plan_answer::Of::Approve(approve) => Some((
                TerminalInput::Plan(if approve.auto_accept_edits {
                    PlanChoice::ApproveAutoAcceptEdits
                } else {
                    PlanChoice::Approve
                }),
                decision(DecisionOutcome::Allowed, String::new(), String::new()),
            )),
            plan_answer::Of::SendBack(send_back) => Some((
                TerminalInput::Plan(PlanChoice::SendBack {
                    note: send_back.note.clone(),
                }),
                decision(DecisionOutcome::Denied, String::new(), send_back.note),
            )),
        },
        (AskShape::Question { questions }, claude_answer::Of::Question(answer)) => {
            if answer.answers.len() != questions.len() {
                return None;
            }
            let mut answers = Vec::new();
            for (shape, response) in questions.iter().zip(answer.answers) {
                let picks = response.selected.len() + usize::from(response.other.is_some());
                let fits = response.selected.iter().all(|index| *index < shape.options)
                    && picks > 0
                    && (shape.multi_select || picks == 1);
                if !fits {
                    return None;
                }
                answers.push(QuestionChoice {
                    selected: response.selected,
                    other: response.other,
                });
            }
            Some((
                TerminalInput::Question {
                    questions: questions.clone(),
                    answers,
                },
                decision(DecisionOutcome::Allowed, String::new(), answer.note),
            ))
        }
        _ => None,
    }
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

impl Interpreter for ClaudePty {
    type State = State;
    const KIND: &'static str = "claude_pty";

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        state.shared.reincarnate(spec, producer_version);
        // The launch fact of the new process is its first, not a relaunch:
        // the session it resumes is a resume, not a restart.
        state.provider.launches = 0;
        state.provider.relaunched = false;
        // The new process's transcript is followed anew, from wherever the
        // agent process last read it.
        state.provider.transcript = None;
        state.resume()
    }

    fn step(state: &mut State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            Event::ProviderExit { code } => state.exited(&mut emit, crate::exit_cause(code)),
            Event::Exiting { cause } => state.exited(&mut emit, cause),
            Event::DaemonLost => {
                state.boundary(&mut emit, wire::BoundaryKind::DaemonLost, String::new())
            }
            Event::StopRequested(wire::StopMode::Abort) => state.interrupt(&mut emit),
            Event::StopRequested(_) => {}
        }
        if let Some(entry) = state.shared.next_queued() {
            emit.effect(Effect::Terminal(TerminalInput::Prompt {
                text: entry.text,
                attachments: entry.attachments,
            }));
        }
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<
            wire::ClaudePtyItem,
            wire::ClaudePtySnapshot,
            wire::ClaudeAnswer,
            State,
        >(target)
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude_pty().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_pty_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_item(body: &[u8]) -> ItemView {
    use claude_pty_item::Kind;
    let item = ClaudePtyItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thinking(thinking)) => ("thinking", thinking.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", true, describe_tool(&tool)),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms
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
        Some(Kind::CompactSummary(_)) => ("compact_summary", true, String::new()),
        Some(Kind::Task(task)) => (
            "task",
            true,
            format!("{} {} {}", task.task_id, task.status, task.summary),
        ),
        Some(Kind::Interruption(_)) => ("interruption", true, String::new()),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::ApiError(error)) => (
            "api_error",
            true,
            format!(
                "{} {} retry={} {}/{}",
                error.error_kind,
                Value::String(error.message),
                error.will_retry,
                error.attempt,
                error.max_attempts
            ),
        ),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{} session={} version={} keymap={}{}",
                BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                or_dash(&boundary.provider_session),
                or_dash(&boundary.provider_version),
                or_dash(&boundary.keymap),
                if boundary.cause.is_empty() {
                    String::new()
                } else {
                    format!(" cause={}", Value::String(boundary.cause))
                }
            ),
        ),
        Some(Kind::Slash(slash)) => (
            "slash",
            true,
            format!("{} args={}", slash.command, Value::String(slash.args)),
        ),
        Some(Kind::Ask(item)) => ("ask", true, ask_item::describe(&item)),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                Value::String(unrecognized.summary)
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
    let snapshot = ClaudePtySnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    let background = snapshot.background_processes.unwrap_or_default();
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "asks=[{}] session={} model={} mode={} context={} tasks={} background={}",
            describe_asks(&snapshot.asks),
            snapshot.provider_session.as_deref().unwrap_or("?"),
            snapshot.model.as_deref().unwrap_or("?"),
            snapshot.permission_mode.as_deref().unwrap_or("?"),
            if context.known {
                context.used_tokens.to_string()
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.tasks.unwrap_or_default()),
            if background.known {
                background.running.to_string()
            } else {
                "?".into()
            }
        ),
    }
}
