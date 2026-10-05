//! Codex: one `codex app-server` per agent, spoken to over JSON-RPC on
//! stdio. The facts are the messages the server writes: notifications about
//! the thread, its turns and their items, requests that ask the person
//! something, and responses to the requests this interpreter sent.
//!
//! The agent process performs the handshake (`initialize`, then
//! `thread/start` or `thread/resume`); the interpreter reads the thread from
//! that response or from `thread/started`, and from then on writes every
//! request and response itself with ids it chooses (`amux-<n>`), so each
//! acknowledgement is matched to what it acknowledges.
//!
//! What the agent offers comes from the server, never from a client
//! catalogue: once the handshake's thread answer arrives, the interpreter
//! asks `model/list` (following `nextCursor` to the last page; hidden
//! models left out) and `skills/list` once per server, and the snapshot
//! carries the models with their reasoning efforts and the skills as
//! commands. A refused or missing answer leaves its list empty.
//!
//! An agent message goes to Codex with `thread/inject_items`. Codex reports
//! nothing about an injected item, so the interpreter writes the
//! agent-message item itself. When the message leaves the pending set
//! depends on whether a running turn drains injected items or parks them
//! for the next turn: [`CODEX_INJECT_CONSUMPTION`].

mod facts;
mod recording;

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wire::{
    AgentSpec, AskClosed, AskItem, BackgroundProcesses, CodexAnswer, CodexAsk, CodexItem,
    CodexSnapshot, ContextMeter, Decision, DecisionOutcome, Envelope, EnvelopeKind, FormAction,
    Input, OfferedCommand, OfferedModel, QueuedInput, Step, TaskList, TaskListEntry, ToolDecision,
    ToolServerHealth, UsageLimits, Work, codex_answer, codex_ask, codex_input, codex_item, input,
    sender, work,
};

use crate::claude_common::{clip, describe_tasks, or_dash};
use crate::{
    Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView, RedactTarget,
    Shared, SnapshotView, Stepped, agent_message_body, agent_message_key, ask_item, codex_input,
    human, reason, serde_pb, unknown,
};

/// When an agent message injected into Codex counts as consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InjectConsumption {
    /// A running turn does not see injected items; they wait for the next
    /// turn. At turn end, while messages are parked, the interpreter starts
    /// an empty turn, and the messages leave the pending set at that turn's
    /// acknowledgement.
    ParkedUntilNextTurn,
    /// A running turn samples again with injected items in context before
    /// it ends, so a message injected mid-turn leaves the pending set at the
    /// inject's acknowledgement and no turn is started for it. An inject
    /// acknowledged after its turn ended was recorded into an idle thread,
    /// so it is kicked as an idle inject is.
    DrainedMidTurn,
}

/// Decided by probing codex-cli 0.157.0: a running turn drains items
/// injected into it and answers them under the same turn id. An inject into
/// an idle thread still needs an empty turn to be answered, and is consumed
/// at that turn's acknowledgement under either arm.
pub const CODEX_INJECT_CONSUMPTION: InjectConsumption = InjectConsumption::DrainedMidTurn;

/// Which consumption arm an interpreter type runs.
pub trait Arm {
    const CONSUMPTION: InjectConsumption;
}

/// The arm the probe decided.
pub struct Probed;
impl Arm for Probed {
    const CONSUMPTION: InjectConsumption = CODEX_INJECT_CONSUMPTION;
}

/// The parked arm, whatever the probe decided; for its own goldens.
pub struct Parked;
impl Arm for Parked {
    const CONSUMPTION: InjectConsumption = InjectConsumption::ParkedUntilNextTurn;
}

/// The drained arm, whatever the probe decided; for its own goldens.
pub struct Drained;
impl Arm for Drained {
    const CONSUMPTION: InjectConsumption = InjectConsumption::DrainedMidTurn;
}

/// The Codex interpreter with a fixed consumption arm.
pub struct CodexWith<A: Arm>(PhantomData<A>);

/// The interpreter for kind `codex`.
pub type Codex = CodexWith<Probed>;

/// A request this interpreter sent, awaiting its response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    /// `turn/start`. Agent messages that leave the pending set at its
    /// acknowledgement ride along: a kick, or the next turn in the parked
    /// arm.
    Turn {
        #[serde(with = "serde_pb::bytes_vec")]
        consumes: Vec<Vec<u8>>,
    },
    /// `turn/steer` for the queued prompt with this input id.
    Steer {
        #[serde(with = "serde_pb::bytes")]
        input_id: Vec<u8>,
    },
    Interrupt,
    Compact,
    Inject {
        #[serde(with = "serde_pb::bytes")]
        envelope_id: Vec<u8>,
        /// A turn was running when it was sent.
        during_turn: bool,
        /// That turn ended before the acknowledgement came.
        turn_over: bool,
    },
    /// A page of `model/list`, with the models earlier pages listed.
    Models {
        page: u32,
        #[serde(with = "serde_pb::msgs")]
        listed: Vec<OfferedModel>,
    },
    /// `skills/list`.
    Skills,
}

/// What answering an open ask needs beyond the wire ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    /// The server's request id, as it sent it.
    id: String,
    method: String,
    /// For approvals: the response each offered decision sends, in the
    /// order of the ask's decisions.
    responses: Vec<String>,
    /// For questions: each question's id and option labels.
    questions: Vec<(String, Vec<String>)>,
    /// When it opened, for the item of an ask that is the work.
    at_ms: i64,
}

/// A unit of work as last emitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WorkState {
    at_ms: i64,
    #[serde(with = "serde_pb::opt_msg")]
    work: Option<Work>,
    text: String,
    turn: String,
}

/// A text item streaming from Codex: a message, a working note, a plan or
/// reasoning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Streamed {
    Message,
    WorkingNote,
    Reasoning(Vec<String>),
}

/// Overrides the next `turn/start` carries; Codex keeps them for later
/// turns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Overrides {
    model: Option<String>,
    effort: Option<Option<String>>,
    approval: Option<(String, String)>,
}

/// Everything the Codex interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<CodexAsk>,
    consumption: InjectConsumption,
    /// The spec's incarnation: a later one's first thread is a resume.
    #[serde(default)]
    incarnation: u32,
    thread_id: Option<String>,
    version: Option<String>,
    model: Option<String>,
    launch_model: Option<String>,
    effort: Option<String>,
    approval: Option<String>,
    sandbox: Option<String>,
    /// What `model/list` and `skills/list` answered, whole.
    #[serde(with = "serde_pb::msgs")]
    models: Vec<OfferedModel>,
    #[serde(with = "serde_pb::msgs")]
    commands: Vec<OfferedCommand>,
    /// This server was asked what it offers.
    offers_asked: bool,
    overrides: Overrides,
    /// The turn Codex reports running.
    active_turn: Option<String>,
    /// An interrupt asked for before Codex named the turn.
    interrupt_pending: bool,
    requests: BTreeMap<String, Request>,
    next_request: u64,
    next_boundary: u64,
    /// Agent messages accepted while no thread was running.
    #[serde(with = "serde_pb::msgs")]
    held: Vec<Envelope>,
    /// Agent messages injected mid-turn, waiting for the next turn (parked
    /// arm only).
    #[serde(with = "serde_pb::bytes_vec")]
    parked: Vec<Vec<u8>>,
    works: BTreeMap<String, WorkState>,
    /// When each amux send call started, so its completion keeps the time.
    sent: BTreeMap<String, i64>,
    streamed: BTreeMap<String, Streamed>,
    asks: BTreeMap<String, AskMeta>,
    /// Keys emitted since the turn began, and the newest created: text is
    /// appended only to the newest item, else the item is re-emitted whole.
    created: BTreeSet<String>,
    newest: String,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<UsageLimits>,
    #[serde(with = "serde_pb::opt_msg")]
    servers: Option<ToolServerHealth>,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<wire::SignIn>,
    /// Reviewer verdicts on work Codex has not reported yet.
    #[serde(with = "serde_pb::msg_map")]
    reviewed: BTreeMap<String, ToolDecision>,
    /// The whole-turn diff last emitted, by key.
    last_diff: Option<(String, String)>,
    /// The turn whose final error is already an item.
    final_error: Option<String>,
    /// Commands still running after the turn that started them ended.
    background: Option<BTreeSet<String>>,
    plan: Option<Vec<(String, i32)>>,
}

fn item_body(kind: codex_item::Kind) -> Vec<u8> {
    CodexItem { kind: Some(kind) }.encode_to_vec()
}

/// A request id as an ask key: the JSON the server sent.
fn ask_key(id: &codex_protocol::RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<CodexAsk> {
        &self.shared
    }

    /// The arm this state runs.
    pub fn consumption(&self) -> InjectConsumption {
        self.consumption
    }

    fn new(spec: &AgentSpec, producer_version: &str, consumption: InjectConsumption) -> Self {
        Self {
            shared: Shared::new(spec, KIND, producer_version),
            consumption,
            incarnation: spec.incarnation,
            thread_id: None,
            version: (!spec.provider_version.is_empty()).then(|| spec.provider_version.clone()),
            model: None,
            launch_model: None,
            effort: None,
            approval: None,
            sandbox: None,
            models: Vec::new(),
            commands: Vec::new(),
            offers_asked: false,
            overrides: Overrides::default(),
            active_turn: None,
            interrupt_pending: false,
            requests: BTreeMap::new(),
            next_request: 0,
            next_boundary: 0,
            held: Vec::new(),
            parked: Vec::new(),
            works: BTreeMap::new(),
            sent: BTreeMap::new(),
            streamed: BTreeMap::new(),
            asks: BTreeMap::new(),
            created: BTreeSet::new(),
            newest: String::new(),
            context_tokens: None,
            context_window: None,
            usage: None,
            servers: None,
            sign_in: None,
            reviewed: BTreeMap::new(),
            final_error: None,
            last_diff: None,
            background: None,
            plan: None,
        }
    }

    fn body(&self) -> Vec<u8> {
        CodexSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            context: Some(match self.context_tokens {
                None => unknown::context_meter(),
                Some(used_tokens) => ContextMeter {
                    known: true,
                    used_tokens,
                    window_tokens: self.context_window,
                    breakdown: Vec::new(),
                },
            }),
            model: self.model.clone(),
            approval_policy: self.approval.clone(),
            sandbox: self.sandbox.clone(),
            active_turn: self.active_turn.clone(),
            servers: Some(
                self.servers
                    .clone()
                    .unwrap_or_else(unknown::tool_server_health),
            ),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::usage_limits)),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            background_processes: Some(match &self.background {
                None => unknown::background_processes(),
                Some(running) => BackgroundProcesses {
                    known: true,
                    running: running.len() as u32,
                },
            }),
            plan: Some(match &self.plan {
                None => unknown::task_list(),
                Some(steps) => TaskList {
                    known: true,
                    entries: steps
                        .iter()
                        .enumerate()
                        .map(|(index, (step, status))| TaskListEntry {
                            id: (index + 1).to_string(),
                            subject: step.clone(),
                            status: *status,
                            active_form: String::new(),
                        })
                        .collect(),
                },
            }),
            effort: self.effort.clone(),
            thread_id: self.thread_id.clone(),
            models: self.models.clone(),
            commands: self.commands.clone(),
        }
        .encode_to_vec()
    }

    // --- writing to Codex ------------------------------------------------

    fn request(&mut self, emit: &mut Emit, method: &str, params: Value, request: Request) {
        emit.effect(Effect::ProviderWrite(
            self.request_bytes(method, params, request),
        ));
    }

    fn request_bytes(&mut self, method: &str, params: Value, request: Request) -> Vec<u8> {
        self.next_request += 1;
        let id = format!("amux-{}", self.next_request);
        self.request_as(id, method, params, request)
    }

    /// A request under an id of its own choosing, outside the numbered run
    /// that turns and their controls take.
    fn request_as(&mut self, id: String, method: &str, params: Value, request: Request) -> Vec<u8> {
        self.requests.insert(id.clone(), request);
        serde_json::to_vec(&json!({ "id": id, "method": method, "params": params })).expect("json")
    }

    fn respond(&self, emit: &mut Emit, id: &str, result: Value) {
        let id = serde_json::from_str::<Value>(id).unwrap_or(Value::Null);
        emit.effect(Effect::ProviderWrite(
            serde_json::to_vec(&json!({ "id": id, "result": result })).expect("json"),
        ));
    }

    fn thread(&self) -> Value {
        json!(self.thread_id.clone().unwrap_or_default())
    }

    // --- items -----------------------------------------------------------

    /// Emits an item, remembering which key was created last.
    fn emit_item(&mut self, emit: &mut Emit, draft: ItemDraft) {
        if self.created.insert(draft.key.clone()) {
            self.newest = draft.key.clone();
        }
        self.shared.item(emit, draft);
    }

    /// Extends an open item's text: an append when it is the newest item,
    /// else the whole item again.
    fn extend(&mut self, emit: &mut Emit, key: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.newest == key && self.shared.append(emit, key, delta) {
            return;
        }
        if let Some(open) = self.shared.open_item(key).cloned() {
            self.shared.item(
                emit,
                ItemDraft {
                    key: key.to_owned(),
                    text: open.text + delta,
                    attachments: open.attachments,
                    input_id: open.input_id,
                    body: open.body,
                    at_ms: Some(open.at_ms),
                    complete: false,
                },
            );
        }
    }

    fn emit_work(&mut self, emit: &mut Emit, key: &str) {
        let Some(state) = self.works.get(key).cloned() else {
            return;
        };
        let Some(work) = state.work else {
            return;
        };
        let complete = work_complete(&work);
        self.emit_item(
            emit,
            ItemDraft {
                key: key.to_owned(),
                text: state.text,
                body: item_body(codex_item::Kind::Work(work)),
                at_ms: Some(state.at_ms),
                complete,
                ..Default::default()
            },
        );
    }

    /// Writes the item of an ask that is the work, open or closed; asks that
    /// point at a unit of work have none.
    fn emit_ask(&mut self, emit: &mut Emit, ask: &CodexAsk, at_ms: i64, closed: Option<AskClosed>) {
        let Some(item) = work_ask(ask) else {
            return;
        };
        let item = match closed {
            Some(closed) => ask_item::close(item, closed),
            None => item,
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: ask.item_key.clone(),
                body: item_body(codex_item::Kind::Ask(item)),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Closes an ask the server resolved without saying how: the decision
    /// on its unit of work, or its own item, reads dismissed.
    fn dismiss(&mut self, emit: &mut Emit, ask: &CodexAsk, meta: Option<AskMeta>) {
        self.decide(
            emit,
            &ask.item_key,
            ToolDecision {
                outcome: DecisionOutcome::Unknown as i32,
                ..Default::default()
            },
        );
        if let Some(meta) = meta {
            self.emit_ask(emit, ask, meta.at_ms, Some(ask_item::dismissed()));
        }
    }

    /// Puts a decision on the work item an ask pointed at.
    fn decide(&mut self, emit: &mut Emit, item_key: &str, decision: ToolDecision) {
        let Some(work) = self
            .works
            .get_mut(item_key)
            .and_then(|state| state.work.as_mut())
        else {
            return;
        };
        work.decision = Some(decision);
        self.emit_work(emit, item_key);
    }

    /// Cancels the running turn; one still starting is cancelled as soon
    /// as Codex names it.
    fn interrupt(&mut self, emit: &mut Emit) {
        match self.active_turn.clone() {
            Some(turn) => self.request(
                emit,
                "turn/interrupt",
                json!({ "threadId": self.thread(), "turnId": turn }),
                Request::Interrupt,
            ),
            None if self.shared.is_busy() => self.interrupt_pending = true,
            None => {}
        }
    }

    fn boundary(&mut self, emit: &mut Emit, kind: wire::BoundaryKind, cause: String) {
        self.next_boundary += 1;
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("boundary:{}", self.next_boundary),
                body: item_body(codex_item::Kind::Boundary(wire::Boundary {
                    kind: kind as i32,
                    provider_session: self.thread_id.clone().unwrap_or_default(),
                    provider_version: self.version.clone().unwrap_or_default(),
                    cause,
                    keymap: String::new(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- inputs ----------------------------------------------------------

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        let arm = match input.of {
            Some(input::Of::AgentMessage(envelope)) => {
                return self.agent_message(emit, &id, envelope);
            }
            Some(input::Of::Codex(wire::CodexInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, reason::UNSUPPORTED),
        };
        match arm {
            codex_input::Of::Prompt(prompt) => {
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry, Vec::new());
                }
            }
            codex_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            codex_input::Of::SendNow(send) => {
                let turn = self.active_turn.clone();
                if let Some(entry) =
                    self.shared
                        .send_now(emit, &id, &send.queued_input_id, turn.is_some())
                    && let Some(turn) = turn
                {
                    self.steer(emit, &turn, entry);
                }
            }
            codex_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Approval(approval) => {
                self.approval = Some(approval.approval_policy.clone());
                self.sandbox = Some(approval.sandbox.clone());
                self.overrides.approval = Some((approval.approval_policy, approval.sandbox));
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Model(model) => {
                let model = model.model.or_else(|| self.launch_model.clone());
                self.model = model.clone();
                self.overrides.model = model;
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Effort(effort) => {
                self.effort = effort.effort.clone();
                self.overrides.effort = Some(effort.effort);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Approve(approve) => self.approve(emit, &id, approve),
            codex_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// Starts a turn for a prompt. Its item is written now, keyed by its
    /// input id; Codex's reflection of it is recognised and left out.
    fn submit(&mut self, emit: &mut Emit, entry: QueuedInput, mut consumes: Vec<Vec<u8>>) {
        let key = prompt_key(&entry.input_id);
        if entry.text.trim() == "/compact" {
            // Codex compacts on request rather than from a message; the
            // compaction runs as a turn of its own and reflects nothing.
            self.shared.reflect_prompt();
            self.request(
                emit,
                "thread/compact/start",
                json!({ "threadId": self.thread() }),
                Request::Compact,
            );
        } else {
            consumes.append(&mut self.parked);
            let params = self.turn_params(&entry.text);
            let request = self.request_bytes("turn/start", params, Request::Turn { consumes });
            emit.effect(if entry.attachments.is_empty() {
                Effect::ProviderWrite(request)
            } else {
                Effect::CodexTurnInput {
                    request,
                    attachments: entry.attachments.clone(),
                }
            });
        }
        self.emit_item(
            emit,
            ItemDraft {
                key,
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(codex_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn turn_params(&mut self, text: &str) -> Value {
        let input = if text.is_empty() {
            json!([])
        } else {
            json!([{ "type": "text", "text": text, "text_elements": [] }])
        };
        let mut params = json!({ "threadId": self.thread(), "input": input });
        let overrides = std::mem::take(&mut self.overrides);
        if let Some(model) = overrides.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = overrides.effort {
            params["effort"] = json!(effort);
        }
        if let Some((policy, sandbox)) = overrides.approval {
            params["approvalPolicy"] = json!(policy);
            params["sandboxPolicy"] = sandbox_policy(&sandbox);
        }
        params
    }

    /// Delivers a queued prompt into the running turn. Its item waits for
    /// Codex's reflection; a refused steer returns it to the queue.
    fn steer(&mut self, emit: &mut Emit, turn: &str, entry: QueuedInput) {
        let params = json!({
            "threadId": self.thread(),
            "expectedTurnId": turn,
            "input": [{ "type": "text", "text": entry.text, "text_elements": [] }],
        });
        let request = self.request_bytes(
            "turn/steer",
            params,
            Request::Steer {
                input_id: entry.input_id,
            },
        );
        emit.effect(if entry.attachments.is_empty() {
            Effect::ProviderWrite(request)
        } else {
            Effect::CodexTurnInput {
                request,
                attachments: entry.attachments,
            }
        });
    }

    /// An agent message: its item now, since Codex reports nothing about an
    /// injected item, then the inject.
    fn agent_message(&mut self, emit: &mut Emit, id: &[u8], envelope: Envelope) {
        self.shared.message_accepted(&envelope.id);
        self.emit_item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: envelope.text.clone(),
                input_id: envelope.id.clone(),
                body: item_body(codex_item::Kind::AgentMessage(agent_message_body(
                    &envelope,
                ))),
                complete: true,
                ..Default::default()
            },
        );
        if !self.shared.is_started() {
            self.held.push(envelope);
            return self.shared.accept(emit, id, true);
        }
        self.inject(emit, &envelope);
        self.shared.accept(emit, id, false);
    }

    fn inject(&mut self, emit: &mut Emit, envelope: &Envelope) {
        let during_turn = self.shared.is_busy();
        self.inject_items(emit, envelope, during_turn);
        if !during_turn {
            // An idle thread records the item and waits; an empty turn
            // answers it under either arm.
            self.kick(emit, vec![envelope.id.clone()]);
        } else if self.consumption == InjectConsumption::ParkedUntilNextTurn {
            self.parked.push(envelope.id.clone());
        }
    }

    /// Records an agent message in the thread; whatever turn runs next
    /// sees it.
    fn inject_items(&mut self, emit: &mut Emit, envelope: &Envelope, during_turn: bool) {
        self.request(
            emit,
            "thread/inject_items",
            json!({
                "threadId": self.thread(),
                "items": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": injected_text(envelope) }],
                }],
            }),
            Request::Inject {
                envelope_id: envelope.id.clone(),
                during_turn,
                turn_over: false,
            },
        );
    }

    /// Starts an empty turn; the messages it carries are consumed at its
    /// acknowledgement.
    fn kick(&mut self, emit: &mut Emit, consumes: Vec<Vec<u8>>) {
        self.shared.turn_started();
        let params = self.turn_params("");
        self.request(emit, "turn/start", params, Request::Turn { consumes });
    }

    /// Inject what was held until a thread was running. A prompt queued
    /// for the thread (a spawn's task) carries the held messages in its own
    /// turn, so the model reads its task with them rather than answering
    /// them first in an empty turn of their own.
    fn release_held(&mut self, emit: &mut Emit) {
        let held = std::mem::take(&mut self.held);
        if held.is_empty() {
            return;
        }
        // A compaction runs as a turn that carries no input.
        let compacts = self
            .shared
            .queue()
            .entries()
            .iter()
            .find(|entry| !entry.steer)
            .is_some_and(|entry| entry.text.trim() == "/compact");
        let Some(entry) = (!compacts).then(|| self.shared.next_queued()).flatten() else {
            for envelope in &held {
                self.inject(emit, envelope);
            }
            return;
        };
        for envelope in &held {
            self.inject_items(emit, envelope, false);
        }
        let consumes = held.into_iter().map(|envelope| envelope.id).collect();
        self.submit(emit, entry, consumes);
    }

    /// At turn end in the parked arm: parked messages need a turn. A queued
    /// prompt's turn carries them; else an empty one is started.
    fn turn_over_parked(&mut self, emit: &mut Emit) {
        if self.parked.is_empty() || !self.shared.queue().is_empty() {
            return;
        }
        let parked = std::mem::take(&mut self.parked);
        self.kick(emit, parked);
    }

    fn approve(&mut self, emit: &mut Emit, id: &[u8], approve: wire::Approve) {
        let key = approve.request_id;
        let Some(ask) = self.shared.asks().get(&key).cloned() else {
            return self.shared.reject(emit, id, reason::CLOSED_ASK);
        };
        let Some(meta) = self.asks.get(&key).cloned() else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        let Some(response) = ask
            .decisions
            .iter()
            .position(|offered| *offered == approve.decision)
            .and_then(|at| meta.responses.get(at))
            .and_then(|response| serde_json::from_str::<Value>(response).ok())
        else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        self.shared.answer(emit, id, &key);
        self.asks.remove(&key);
        self.respond(emit, &meta.id, response);
        let decision = Decision::try_from(approve.decision).unwrap_or_default();
        let (outcome, scope) = match decision {
            Decision::Approve => (DecisionOutcome::Allowed, ""),
            Decision::ApproveSession => (DecisionOutcome::Allowed, "session"),
            Decision::ApproveSimilar => (DecisionOutcome::Allowed, "similar"),
            Decision::ApproveNetwork => (DecisionOutcome::Allowed, "network"),
            Decision::Deny => (DecisionOutcome::Denied, ""),
            Decision::Abort => (DecisionOutcome::Denied, "abort"),
            Decision::Unspecified => (DecisionOutcome::Unknown, ""),
        };
        self.decide(
            emit,
            &ask.item_key,
            ToolDecision {
                outcome: outcome as i32,
                scope: scope.into(),
                note: String::new(),
                elsewhere: false,
            },
        );
        self.shared.accept(emit, id, false);
    }

    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: wire::AnswerInput) {
        let key = answer.ask_key;
        let Some(ask) = self.shared.asks().get(&key).cloned() else {
            return self.shared.reject(emit, id, reason::CLOSED_ASK);
        };
        let parsed = CodexAnswer::decode(answer.body.as_slice())
            .ok()
            .and_then(|answer| answer.of);
        let Some((meta, parsed)) = self.asks.get(&key).cloned().zip(parsed) else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        let Some((response, closed)) = codex_answer_response(&ask, &meta, parsed) else {
            return self.shared.reject(emit, id, reason::UNSUPPORTED);
        };
        self.shared.answer(emit, id, &key);
        self.asks.remove(&key);
        self.respond(emit, &meta.id, response);
        self.emit_ask(emit, &ask, meta.at_ms, Some(closed));
        self.shared.accept(emit, id, false);
    }
}

/// The text the model sees for an agent message.
fn injected_text(envelope: &Envelope) -> String {
    let from = match envelope.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(sender::Value::Agent(agent)) if !agent.name.is_empty() => {
            format!("agent {}", agent.name)
        }
        Some(sender::Value::Agent(_)) => "another agent".to_owned(),
        _ => return envelope.text.clone(),
    };
    match EnvelopeKind::try_from(envelope.kind).unwrap_or_default() {
        EnvelopeKind::Finished => format!("[{from} finished its turn]\n{}", envelope.text),
        EnvelopeKind::Failed => format!("[{from} stopped: {}]", envelope.text),
        _ => format!("[message from {from}]\n{}", envelope.text),
    }
}

fn prompt_key(input_id: &[u8]) -> String {
    format!("prompt:{}", serde_pb::to_hex(input_id))
}

/// A sandbox mode as `turn/start` wants it.
fn sandbox_policy(sandbox: &str) -> Value {
    let kind = match sandbox {
        "read-only" => "readOnly",
        "workspace-write" => "workspaceWrite",
        "danger-full-access" => "dangerFullAccess",
        other => other,
    };
    json!({ "type": kind })
}

fn work_complete(work: &Work) -> bool {
    !matches!(
        wire::ToolState::try_from(work.state).unwrap_or_default(),
        wire::ToolState::Pending | wire::ToolState::Running | wire::ToolState::Unspecified
    )
}

/// How Codex marks a note among a question's answers.
pub(crate) const USER_NOTE: &str = "user_note: ";

/// The response an answer sends, and how the ask's own item closes; None
/// when the answer does not fit the ask.
fn codex_answer_response(
    ask: &CodexAsk,
    meta: &AskMeta,
    answer: codex_answer::Of,
) -> Option<(Value, AskClosed)> {
    match (ask.body.as_ref()?, answer) {
        (codex_ask::Body::Question(asked), codex_answer::Of::Question(answer)) => {
            if answer.answers.len() != meta.questions.len() {
                return None;
            }
            let mut answers = serde_json::Map::new();
            let last = meta.questions.len().saturating_sub(1);
            for (at, ((question, labels), response)) in
                meta.questions.iter().zip(&answer.answers).enumerate()
            {
                let mut picked = Vec::new();
                for index in &response.selected {
                    picked.push(labels.get(*index as usize)?.clone());
                }
                picked.extend(response.other.clone());
                if picked.is_empty() {
                    return None;
                }
                // Codex's own form appends a question's notes to its answers
                // this way; the one note the person wrote goes on the last.
                if at == last && !answer.note.is_empty() {
                    picked.push(format!("{USER_NOTE}{}", answer.note));
                }
                answers.insert(question.clone(), json!({ "answers": picked }));
            }
            Some((
                json!({ "answers": answers }),
                ask_item::answered(asked, &answer)?,
            ))
        }
        (codex_ask::Body::McpForm(_), codex_answer::Of::Form(form)) => {
            let action = form_action(form.action)?;
            let content = if action == "accept" {
                serde_json::from_slice(&form.content_json).unwrap_or(json!({}))
            } else {
                Value::Null
            };
            Some((
                json!({ "action": action, "content": content }),
                ask_item::form_sent(&form),
            ))
        }
        (codex_ask::Body::McpLink(_), codex_answer::Of::Link(link)) => Some((
            json!({ "action": form_action(link.action)?, "content": null }),
            ask_item::link_answered(link.action),
        )),
        (codex_ask::Body::Access(asked), codex_answer::Of::Grant(grant)) => {
            let subset = |granted: &[String], asked: &[String]| {
                granted.iter().all(|path| asked.contains(path))
            };
            if !subset(&grant.read, &asked.read)
                || !subset(&grant.write, &asked.write)
                || (grant.network && !asked.network)
            {
                return None;
            }
            let mut permissions = serde_json::Map::new();
            if !grant.read.is_empty() || !grant.write.is_empty() {
                permissions.insert(
                    "fileSystem".into(),
                    json!({ "read": grant.read, "write": grant.write }),
                );
            }
            if grant.network {
                permissions.insert("network".into(), json!({ "enabled": true }));
            }
            let scope = if grant.for_session { "session" } else { "turn" };
            Some((
                json!({ "permissions": permissions, "scope": scope }),
                ask_item::granted(&grant),
            ))
        }
        _ => None,
    }
}

/// The item an ask that is the work opens with; None for an approval,
/// whose decision lands on the unit of work it points at.
fn work_ask(ask: &CodexAsk) -> Option<AskItem> {
    let asked = match ask.body.as_ref()? {
        codex_ask::Body::Question(question) => wire::ask_item::Ask::Question(question.clone()),
        codex_ask::Body::McpForm(form) => wire::ask_item::Ask::Form(form.clone()),
        codex_ask::Body::McpLink(link) => wire::ask_item::Ask::Link(link.clone()),
        codex_ask::Body::Access(access) => wire::ask_item::Ask::Access(access.clone()),
        codex_ask::Body::Command(_)
        | codex_ask::Body::FileChange(_)
        | codex_ask::Body::McpTool(_) => return None,
    };
    Some(ask_item::opened(asked))
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

const KIND: &str = "codex";

impl<A: Arm> Interpreter for CodexWith<A> {
    type State = State;
    const KIND: &'static str = KIND;

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version, A::CONSUMPTION);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        state.shared.reincarnate(spec, producer_version);
        state.incarnation = spec.incarnation;
        if !spec.provider_version.is_empty() {
            state.version = Some(spec.provider_version.clone());
        }
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
            state.submit(&mut emit, entry, Vec::new());
        }
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<wire::CodexItem, wire::CodexSnapshot, wire::CodexAnswer, State>(
            target,
        )
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::codex().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        codex_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_work(work: &Work) -> String {
    let mut text = match &work.of {
        Some(work::Of::Command(command)) => format!(
            "command {} cwd={} action={}{}{}",
            clip(&command.command, 60),
            or_dash(&command.cwd),
            or_dash(&command.action),
            command
                .exit_code
                .map_or(String::new(), |code| format!(" exit={code}")),
            if command.background {
                " background"
            } else {
                ""
            }
        ),
        Some(work::Of::FileChange(change)) => format!(
            "file_change [{}]",
            change
                .changes
                .iter()
                .map(|file| format!(
                    "{} {}{} patch={}",
                    wire::FileChangeKind::try_from(file.kind)
                        .map_or("?", |kind| kind.as_str_name()),
                    file.path,
                    if file.move_to.is_empty() {
                        String::new()
                    } else {
                        format!(" -> {}", file.move_to)
                    },
                    clip(&file.patch, 40)
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Some(work::Of::Mcp(call)) => format!(
            "mcp {}·{} args={} result={}{}",
            call.server,
            call.tool,
            clip(&String::from_utf8_lossy(&call.arguments_json), 40),
            clip(&String::from_utf8_lossy(&call.result_json), 40),
            if call.error.is_empty() {
                String::new()
            } else {
                format!(" error={}", Value::String(call.error.clone()))
            }
        ),
        Some(work::Of::WebSearch(search)) => {
            format!("web_search {}", Value::String(search.query.clone()))
        }
        Some(work::Of::Image(image)) => format!(
            "image {} {}",
            if image.generated {
                "generated"
            } else {
                "viewed"
            },
            image.path
        ),
        Some(work::Of::Collab(collab)) => format!(
            "collab {} threads=[{}] prompt={}",
            collab.tool,
            collab.thread_ids.join(","),
            clip(&collab.prompt, 40)
        ),
        None => "none".to_owned(),
    };
    text.push_str(&format!(
        " {}",
        wire::ToolState::try_from(work.state).map_or("?", |state| state.as_str_name())
    ));
    text.push_str(crate::claude_common::describe_class(work.class));
    if let Some(decision) = &work.decision {
        text.push_str(&format!(
            " decision={}{}{}",
            DecisionOutcome::try_from(decision.outcome)
                .map_or("?", |outcome| outcome.as_str_name()),
            if decision.scope.is_empty() {
                String::new()
            } else {
                format!(" scope={}", decision.scope)
            },
            if decision.elsewhere { " elsewhere" } else { "" }
        ));
    }
    if let Some(ended) = work.ended_at_ms {
        text.push_str(&format!(" ended={ended}"));
    }
    text
}

fn describe_item(body: &[u8]) -> ItemView {
    use codex_item::Kind;
    let item = CodexItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::WorkingNote(text)) => ("working_note", text.complete, String::new()),
        Some(Kind::Reasoning(reasoning)) => (
            "reasoning",
            reasoning.complete,
            format!(
                "summary=[{}]",
                reasoning
                    .summary
                    .iter()
                    .map(|part| clip(part, 60))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        Some(Kind::Work(work)) => ("work", work_complete(&work), describe_work(&work)),
        Some(Kind::McpStartup(startup)) => (
            "mcp_startup",
            true,
            format!(
                "{} {} {}",
                startup.server,
                wire::ToolServerStatus::try_from(startup.status)
                    .map_or("?", |status| status.as_str_name()),
                Value::String(startup.error)
            ),
        ),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms,
            ),
        ),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
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
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::Error(error)) => (
            "error",
            true,
            format!(
                "{} {} retry={} {}/{}",
                error.error_kind,
                clip(&error.message, 80),
                error.will_retry,
                error.attempt,
                error.max_attempts,
            ),
        ),
        Some(Kind::Reroute(switch)) => (
            "reroute",
            true,
            format!(
                "{} -> {} {}",
                switch.from,
                switch.to,
                Value::String(switch.reason)
            ),
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
        Some(Kind::TurnDiff(diff)) => (
            "turn_diff",
            true,
            format!("patch={}", clip(&diff.patch, 60)),
        ),
        Some(Kind::Ask(item)) => ("ask", true, ask_item::describe(&item)),
        Some(Kind::Verdict(verdict)) => (
            "verdict",
            true,
            format!(
                "{} risk={} on={} {}",
                verdict.decision,
                or_dash(&verdict.risk),
                or_dash(&verdict.item_key),
                clip(&verdict.rationale, 60)
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

fn describe_ask(ask: &CodexAsk) -> String {
    let body = match &ask.body {
        Some(codex_ask::Body::Command(command)) => format!(
            "command {} reason={}{}{}",
            clip(&command.command, 50),
            Value::String(command.reason.clone()),
            if command.allow_prefix.is_empty() {
                String::new()
            } else {
                format!(" prefix={:?}", command.allow_prefix)
            },
            if command.network_hosts.is_empty() {
                String::new()
            } else {
                format!(" hosts={:?}", command.network_hosts)
            }
        ),
        Some(codex_ask::Body::FileChange(change)) => format!(
            "file_change files={} reason={}{}",
            change.changes.len(),
            Value::String(change.reason.clone()),
            if change.grant_root.is_empty() {
                String::new()
            } else {
                format!(" root={}", change.grant_root)
            }
        ),
        Some(codex_ask::Body::McpTool(tool)) => format!(
            "mcp_tool {}·{} args={}",
            tool.server,
            tool.tool,
            clip(&String::from_utf8_lossy(&tool.arguments_json), 40)
        ),
        Some(codex_ask::Body::McpForm(form)) => format!(
            "form {} {} schema={}",
            form.server,
            clip(&form.message, 50),
            clip(&String::from_utf8_lossy(&form.schema_json), 40)
        ),
        Some(codex_ask::Body::McpLink(link)) => {
            format!(
                "link {} {} {}",
                link.server,
                clip(&link.message, 50),
                link.url
            )
        }
        Some(codex_ask::Body::Access(access)) => format!(
            "access reason={} read={:?} write={:?} network={}{}",
            Value::String(access.reason.clone()),
            access.read,
            access.write,
            access.network,
            if access.network_hosts.is_empty() {
                String::new()
            } else {
                format!(" hosts={:?}", access.network_hosts)
            }
        ),
        Some(codex_ask::Body::Question(question)) => format!(
            "question [{}]",
            question
                .questions
                .iter()
                .map(|question| format!(
                    "{}{}{}{} ({})",
                    Value::String(question.question.clone()),
                    if question.multi_select { " multi" } else { "" },
                    if question.allow_other { " other" } else { "" },
                    if question.secret { " secret" } else { "" },
                    question
                        .options
                        .iter()
                        .map(|option| format!(
                            "{}{}",
                            option.label,
                            if option.recommended { "*" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join("|")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        None => "none".into(),
    };
    format!(
        "{}→{} {} decisions=[{}]",
        ask.key,
        or_dash(&ask.item_key),
        body,
        ask.decisions
            .iter()
            .map(|decision| Decision::try_from(*decision)
                .map_or("?", |decision| decision.as_str_name())
                .trim_start_matches("DECISION_"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = CodexSnapshot::decode(body).unwrap_or_default();
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
            "asks=[{}] thread={} turn={} model={} effort={} approval={} sandbox={} models=[{}] commands={} context={} plan={} usage={} servers={} sign_in={} background={}",
            snapshot
                .asks
                .iter()
                .map(describe_ask)
                .collect::<Vec<_>>()
                .join("; "),
            snapshot.thread_id.as_deref().unwrap_or("?"),
            snapshot.active_turn.as_deref().unwrap_or("-"),
            snapshot.model.as_deref().unwrap_or("?"),
            snapshot.effort.as_deref().unwrap_or("?"),
            snapshot.approval_policy.as_deref().unwrap_or("?"),
            snapshot.sandbox.as_deref().unwrap_or("?"),
            crate::claude_common::describe_models(&snapshot.models),
            crate::claude_common::describe_commands(&snapshot.commands),
            if context.known {
                format!(
                    "{}/{}",
                    context.used_tokens,
                    context
                        .window_tokens
                        .map_or("?".to_owned(), |window| window.to_string())
                )
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.plan.unwrap_or_default()),
            match wire::UsageState::try_from(usage.state).unwrap_or_default() {
                wire::UsageState::Unknown => "?".to_owned(),
                state => format!(
                    "{}{}{}",
                    state.as_str_name(),
                    usage
                        .windows
                        .iter()
                        .map(|window| format!(" {}:{:.0}%", window.name, window.used_percent))
                        .collect::<String>(),
                    usage
                        .credits
                        .as_ref()
                        .map_or(String::new(), |credits| format!(" credits={credits}"))
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
                state => format!(
                    "{} {}{}",
                    state.as_str_name(),
                    or_dash(&sign_in.account),
                    if sign_in.message.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", clip(&sign_in.message, 60))
                    }
                ),
            },
            if background.known {
                background.running.to_string()
            } else {
                "?".into()
            }
        ),
    }
}
