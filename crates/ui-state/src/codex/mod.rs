//! The Codex chat layer: native `codex_sdk_v1` rows become Codex-owned view
//! state without a Claude-shaped or generic content model between the provider
//! and its fold. Provider-derived feed and standing facts live in `fold`; this
//! facade keeps reducer-local input, attachment, provider-settings, connection,
//! and write-gate overlays. The kernel sees only the layer's `Attention`
//! summary.

pub(crate) mod update;

use std::collections::VecDeque;

pub use ::fold::codex::{
    Activity, AgentMessageEntry, ApprovalResolution, Ask, AskAction, AskActionMeaning, AskContext,
    BoundaryEntry, CodexDecision, ErrorEntry, ErrorSeverity, FileChange, Invariant, ItemFinality,
    McpServerStartup, McpStartupEntry, McpStartupStatus, MessagePhase, NetworkPolicyAction,
    NetworkPolicyAmendment, PlanStep, PromptPart, PromptSource, ReasoningEntry, TokenUsage,
    TurnEntry, TurnStatus, UnrecognizedEntry, WorkEntry, WorkKind, WorkOutcome, WorkState,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::attachments::{AttachmentIndex, Segment};
use crate::model::{
    AgentPhase, Attention, Model, StreamPhase, Violation, Why,
};
use crate::msg::OpId;

pub type FeedEntry = ::fold::codex::FeedEntry<Vec<Segment>>;
pub type FeedEntryKind = ::fold::codex::FeedEntryKind<Vec<Segment>>;
pub type PromptEntry = ::fold::codex::PromptEntry<Vec<Segment>>;
pub type MessageEntry = ::fold::codex::MessageEntry<Vec<Segment>>;
pub type Observation = ::fold::codex::Observation<Vec<Segment>>;

pub const PROTOCOL: &str = ::fold::codex::PROTOCOL;
pub use ::fold::codex::{ASKS_RETAINED, OUTPUT_HEAD_MAX};

pub(crate) const INPUTS_RETAINED: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "codex_command", rename_all = "snake_case")]
pub enum CodexCommand {
    Prompt {
        agent: model::AgentId,
        text: String,
    },
    Steer {
        agent: model::AgentId,
        text: String,
    },
    Answer {
        agent: model::AgentId,
        request_id: Value,
        decision: CodexDecision,
    },
    Interrupt {
        agent: model::AgentId,
    },
}

pub use model::CodexSdkInput as CodexInput;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InFlightInput {
    pub op: OpId,
    pub input_id: Vec<u8>,
    pub kind: InFlightKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InFlightKind {
    Settings,
    Prompt,
    Steer {
        text: String,
    },
    Interrupt,
    Answer {
        request_id: Value,
        decision: CodexDecision,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum CodexPhase {
    Replaying,
    Idle,
    Thinking,
    Responding { item_id: String },
    Executing { item_id: String },
    AwaitingApproval { request_id: Value },
    BlockedUnsupported { item_id: String },
    ReadOnly,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendGate {
    Ready,
    Unavailable,
    Exited,
    Closed,
    Replaying,
    ActiveTurn,
    NeedsYou,
    ObserverReadOnly,
    ReadOnly,
    Unknown,
    InputInFlight,
}

const REFUSAL_UNAVAILABLE: &str = "Codex input unavailable for this agent";
const REFUSAL_EXITED: &str = "agent exited";
const REFUSAL_CLOSED: &str = "Codex thread is closed until it becomes ready again";
const REFUSAL_REPLAYING: &str = "send gated while replaying";
const REFUSAL_ACTIVE: &str = "send gated — a Codex turn is active";
const REFUSAL_NEEDS_YOU: &str = "send gated — resolve the blocking request";
const REFUSAL_OBSERVER_READ_ONLY: &str = "agent is read-only — you are observing this session";
const REFUSAL_READ_ONLY: &str = "Codex thread is read-only until reconnect succeeds";
const REFUSAL_UNKNOWN: &str = "send gated — Codex session state unknown";
const REFUSAL_INPUT_IN_FLIGHT: &str = "send gated — a Codex input is in flight";

impl SendGate {
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Unavailable => Some(REFUSAL_UNAVAILABLE),
            Self::Exited => Some(REFUSAL_EXITED),
            Self::Closed => Some(REFUSAL_CLOSED),
            Self::Replaying => Some(REFUSAL_REPLAYING),
            Self::ActiveTurn => Some(REFUSAL_ACTIVE),
            Self::NeedsYou => Some(REFUSAL_NEEDS_YOU),
            Self::ObserverReadOnly => Some(REFUSAL_OBSERVER_READ_ONLY),
            Self::ReadOnly => Some(REFUSAL_READ_ONLY),
            Self::Unknown => Some(REFUSAL_UNKNOWN),
            Self::InputInFlight => Some(REFUSAL_INPUT_IN_FLIGHT),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CodexViolation {
    RetentionOverflow {
        agent: model::AgentId,
        store: &'static str,
        len: usize,
        cap: usize,
    },
    DuplicateAsk {
        agent: model::AgentId,
    },
    DuplicateInput {
        agent: model::AgentId,
    },
    ProjectionDisagreement {
        agent: model::AgentId,
        classification: String,
        phase: CodexPhase,
        attention: Attention,
        send_gate: SendGate,
    },
}

impl CodexViolation {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::RetentionOverflow { .. } => "codex-retention-overflow",
            Self::DuplicateAsk { .. } => "codex-duplicate-ask",
            Self::DuplicateInput { .. } => "codex-duplicate-input",
            Self::ProjectionDisagreement { .. } => "codex-projection-disagreement",
        }
    }
}

impl std::fmt::Display for CodexViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetentionOverflow {
                agent,
                store,
                len,
                cap,
            } => write!(
                f,
                "agent {agent} codex {store} holds {len} entries over the bound of {cap}"
            ),
            Self::DuplicateAsk { agent } => {
                write!(f, "agent {agent} codex asks share a request id")
            }
            Self::DuplicateInput { agent } => {
                write!(f, "agent {agent} codex inputs share an input id")
            }
            Self::ProjectionDisagreement {
                agent,
                classification,
                phase,
                attention,
                send_gate,
            } => write!(
                f,
                "agent {agent} codex classification {classification}, phase {phase:?}, attention \
                 {attention:?}, and send gate {send_gate:?} disagree"
            ),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexLayer {
    observation: ::fold::codex::Observation<Vec<Segment>>,
    provider: Box<crate::ProviderFacts>,
    attachments: AttachmentIndex,
    stale: bool,
    exited: bool,
    inputs: VecDeque<InFlightInput>,
}

impl CodexLayer {
    pub fn cursor(&self) -> u64 {
        self.observation.cursor()
    }

    pub fn provider_facts(&self) -> &crate::ProviderFacts {
        &self.provider
    }

    pub(crate) fn begin_window(&mut self, truncated: bool) {
        let exited = self.exited;
        *self = Self::default();
        self.exited = exited;
        self.observation.begin_window(truncated);
    }

    pub(crate) fn restore_head(&mut self, tip: &::fold::codex::CodexFold) {
        let attention = tip.restored_attention();
        let rows = tip
            .restored_obligations()
            .filter_map(|row| {
                serde_json::from_slice(&row.payload.0)
                    .ok()
                    .map(|payload| (row.seq, payload))
            })
            .collect::<Vec<_>>();
        let attachments = &self.attachments;
        self.observation
            .restore_condition(attention, rows, |text| attachments.segments(text));
        if !tip.restored_outstanding_known()
            || matches!(attention, Some(Attention::NeedsYou { .. }))
                && self.observation.ask_count() == 0
        {
            self.stale = true;
        }
    }

    pub(crate) fn observe(&mut self, seq: u64, _arrived: DateTime<Utc>, row: &Value) {
        self.attachments.observe_row(row);
        self.observe_provider(row);

        let local_resolution = if row.get("type").and_then(Value::as_str)
            == Some("amux.codex_approval_resolved")
        {
            let request_id = row.get("request_id").cloned().unwrap_or(Value::Null);
            let item_id = self
                .observation
                .asks()
                .find(|ask| ask.request_id == request_id)
                .map(|ask| ask.context.item_id().to_string());
            let denied = self.inputs.iter().any(|input| {
                matches!(
                    &input.kind,
                    InFlightKind::Answer { request_id: pending, decision: CodexDecision::Decline | CodexDecision::Cancel }
                        if *pending == request_id
                )
            });
            denied.then_some(item_id).flatten()
        } else {
            None
        };

        let mut adapted = None;
        if row.get("type").and_then(Value::as_str) == Some("amux.input_result") {
            let input_id = input_id(row);
            let matched = input_id.as_ref().and_then(|input_id| {
                self.inputs
                    .iter()
                    .find(|input| &input.input_id == input_id)
                    .cloned()
            });
            if row.pointer("/ok/text").is_none()
                && let Some(InFlightInput {
                    op,
                    kind: InFlightKind::Steer { text },
                    ..
                }) = matched
            {
                let mut value = row.clone();
                value["ok"]["text"] = Value::String(text);
                value["echo_id"] = Value::String(format!("steer:{}", op.0));
                adapted = Some(value);
            }
            if let Some(input_id) = input_id {
                self.inputs.retain(|input| input.input_id != input_id);
            }
        }

        let row = adapted.as_ref().unwrap_or(row);
        let attachments = &self.attachments;
        self.observation
            .observe(seq, row, |text| attachments.segments(text));
        if let Some(item_id) = local_resolution {
            self.observation.set_work_state(&item_id, WorkState::Denied);
        }
    }

    fn observe_provider(&mut self, row: &Value) {
        match row.get("type").and_then(Value::as_str) {
            Some("amux.codex_ready" | "amux.codex_settings") => {
                self.stale = false;
                if let Some(session) = row.get("session") {
                    self.provider.observe_codex(session);
                }
            }
            Some("model/rerouted") => {
                self.provider.model = row
                    .get("toModel")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.provider.efforts = self
                    .provider
                    .models
                    .iter()
                    .find(|item| Some(&item.id) == self.provider.model.as_ref())
                    .map(|item| item.efforts.clone())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }

    pub(crate) fn observe_replay_complete(&mut self) {
        self.observation.observe_replay_complete();
    }

    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(crate) fn observe_exit(&mut self) {
        self.observation.observe_exit();
        self.inputs.clear();
        self.exited = true;
    }

    pub fn token_usage(&self) -> Option<&TokenUsage> {
        self.observation.token_usage()
    }

    pub fn work(&self) -> impl Iterator<Item = &WorkEntry> {
        self.observation.work()
    }

    pub fn attachments(&self) -> &AttachmentIndex {
        &self.attachments
    }

    pub(crate) fn attachments_mut(&mut self) -> &mut AttachmentIndex {
        &mut self.attachments
    }

    pub fn asks(&self) -> impl Iterator<Item = &Ask> {
        self.observation.asks()
    }

    pub fn ask_head(&self) -> Option<&Ask> {
        self.observation.ask_head()
    }

    pub fn ask_count(&self) -> usize {
        self.observation.ask_count()
    }

    pub fn active_turn_id(&self) -> Option<&str> {
        self.observation.active_turn_id()
    }

    pub fn in_flight_inputs(&self) -> impl Iterator<Item = &InFlightInput> {
        self.inputs.iter()
    }

    pub fn attention(&self) -> Attention {
        classify(Some(self), Some(&StreamPhase::Live), None, false).attention()
    }

    pub(crate) fn working_is_stale(&self, _now: Option<DateTime<Utc>>) -> bool {
        false
    }

    pub(crate) fn note_input(&mut self, input: InFlightInput) {
        self.inputs.push_back(input);
        if self.inputs.len() > INPUTS_RETAINED {
            self.inputs.pop_front();
        }
    }

    pub(crate) fn note_input_send_failed(&mut self, op: OpId) {
        self.inputs.retain(|input| input.op != op);
    }

    pub(crate) fn check_invariants(&self, agent: model::AgentId, out: &mut Vec<Violation>) {
        for invariant in self.observation.invariants() {
            let violation = match invariant {
                Invariant::RetentionOverflow { store, len, cap } => {
                    CodexViolation::RetentionOverflow {
                        agent,
                        store,
                        len,
                        cap,
                    }
                }
                Invariant::DuplicateAsk => CodexViolation::DuplicateAsk { agent },
            };
            out.push(Violation::Codex(violation));
        }
        if self.inputs.len() > INPUTS_RETAINED {
            out.push(Violation::Codex(CodexViolation::RetentionOverflow {
                agent,
                store: "inputs",
                len: self.inputs.len(),
                cap: INPUTS_RETAINED,
            }));
        }
        if self.inputs.iter().enumerate().any(|(i, input)| {
            self.inputs
                .iter()
                .skip(i + 1)
                .any(|other| input.input_id == other.input_id)
        }) {
            out.push(Violation::Codex(CodexViolation::DuplicateInput { agent }));
        }
    }

    fn observation(&self) -> &::fold::codex::Observation<Vec<Segment>> {
        &self.observation
    }
}

fn input_id(row: &Value) -> Option<Vec<u8>> {
    row.get("input_id").and_then(Value::as_array).map(|bytes| {
        bytes
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|byte| u8::try_from(byte).ok())
            .collect()
    })
}

/// One ordered interpretation of the layer's phase and attention facts. Public
/// projections deliberately lose different details, so those details live
/// here instead of being independently rediscovered by each projection.
#[derive(Clone, Debug, PartialEq)]
struct Situation {
    state: SituationState,
    active_turn: bool,
    input_in_flight: bool,
    observer_readonly: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum SituationState {
    Unavailable,
    Exited,
    Closed,
    Unknown,
    ReadOnly,
    Replaying,
    AwaitingApproval { request_id: Value },
    BlockedUnsupported { item_id: String },
    Responding { item_id: String },
    Executing { item_id: String },
    Working,
    Finished,
    Idle,
}

impl Situation {
    fn unavailable() -> Self {
        Self {
            state: SituationState::Unavailable,
            active_turn: false,
            input_in_flight: false,
            observer_readonly: false,
        }
    }

    fn phase(&self) -> CodexPhase {
        match &self.state {
            SituationState::Unavailable | SituationState::Unknown => CodexPhase::Unknown,
            SituationState::Exited
            | SituationState::Closed
            | SituationState::Finished
            | SituationState::Idle => CodexPhase::Idle,
            SituationState::ReadOnly => CodexPhase::ReadOnly,
            SituationState::Replaying => CodexPhase::Replaying,
            SituationState::AwaitingApproval { request_id } => CodexPhase::AwaitingApproval {
                request_id: request_id.clone(),
            },
            SituationState::BlockedUnsupported { item_id } => CodexPhase::BlockedUnsupported {
                item_id: item_id.clone(),
            },
            SituationState::Responding { item_id } => CodexPhase::Responding {
                item_id: item_id.clone(),
            },
            SituationState::Executing { item_id } => CodexPhase::Executing {
                item_id: item_id.clone(),
            },
            SituationState::Working => CodexPhase::Thinking,
        }
    }

    fn attention(&self) -> Attention {
        match &self.state {
            SituationState::Unavailable
            | SituationState::Exited
            | SituationState::Closed
            | SituationState::Unknown
            | SituationState::ReadOnly
            | SituationState::Replaying => Attention::Unknown,
            _ if self.input_in_flight => Attention::Working,
            SituationState::AwaitingApproval { .. } => Attention::NeedsYou {
                why: Why::Permission,
            },
            SituationState::BlockedUnsupported { .. } => Attention::NeedsYou { why: Why::Question },
            SituationState::Responding { .. }
            | SituationState::Executing { .. }
            | SituationState::Working => Attention::Working,
            SituationState::Finished => Attention::NeedsYou { why: Why::Finished },
            SituationState::Idle => Attention::Idle,
        }
    }

    fn send_gate(&self) -> SendGate {
        match self.state {
            SituationState::Unavailable => return SendGate::Unavailable,
            SituationState::Exited => return SendGate::Exited,
            SituationState::Closed => return SendGate::Closed,
            SituationState::Replaying => return SendGate::Replaying,
            SituationState::ReadOnly => return SendGate::ReadOnly,
            SituationState::Unknown => return SendGate::Unknown,
            _ => {}
        }
        if self.observer_readonly {
            return SendGate::ObserverReadOnly;
        }
        if self.input_in_flight {
            return SendGate::InputInFlight;
        }
        match self.state {
            SituationState::Responding { .. }
            | SituationState::Executing { .. }
            | SituationState::Working => SendGate::ActiveTurn,
            SituationState::AwaitingApproval { .. } | SituationState::BlockedUnsupported { .. } => {
                SendGate::NeedsYou
            }
            SituationState::Finished | SituationState::Idle => SendGate::Ready,
            _ => unreachable!("lifecycle states returned above"),
        }
    }

    fn with_state(mut self, state: SituationState) -> Self {
        self.state = state;
        self
    }
}

/// The one ordered Codex classification. This is the only Codex-layer code
/// that reads kernel `StreamPhase`; every projection consumes its lossless
/// result.
fn classify(
    layer: Option<&CodexLayer>,
    stream_phase: Option<&StreamPhase>,
    agent_phase: Option<&AgentPhase>,
    observer_readonly: bool,
) -> Situation {
    let Some(layer) = layer else {
        return Situation::unavailable();
    };
    let situation = Situation {
        state: SituationState::Unknown,
        active_turn: layer.active_turn_id().is_some(),
        input_in_flight: !layer.inputs.is_empty(),
        observer_readonly,
    };
    if matches!(agent_phase, Some(AgentPhase::Exited { .. })) || layer.exited {
        return situation.with_state(SituationState::Exited);
    }
    match stream_phase {
        Some(StreamPhase::Opening | StreamPhase::Replaying) => {
            return situation.with_state(SituationState::Replaying);
        }
        Some(StreamPhase::Live)
        | Some(StreamPhase::Closed {
            reason:
                crate::msg::StreamCloseReason::AgentExited { .. }
                | crate::msg::StreamCloseReason::AgentDeleted,
        }) => {}
        _ => return situation,
    }
    let state = match layer.observation().activity() {
        Activity::Closed => SituationState::Closed,
        _ if layer.stale => SituationState::Unknown,
        Activity::Unknown => SituationState::Unknown,
        Activity::ReadOnly => SituationState::ReadOnly,
        Activity::Replaying => SituationState::Replaying,
        Activity::AwaitingApproval { request_id } => {
            SituationState::AwaitingApproval { request_id }
        }
        Activity::BlockedUnsupported { item_id } => SituationState::BlockedUnsupported { item_id },
        Activity::Responding { item_id } => SituationState::Responding { item_id },
        Activity::Executing { item_id } => SituationState::Executing { item_id },
        Activity::Working => SituationState::Working,
        Activity::Finished => SituationState::Finished,
        Activity::Idle => SituationState::Idle,
    };
    situation.with_state(state)
}

fn classify_model(model: &Model, agent: model::AgentId) -> Situation {
    let Some(card) = model.agent(agent) else {
        return classify(None, None, None, false);
    };
    classify(
        card.codex(),
        model.stream(agent).map(|stream| &stream.phase),
        Some(&card.phase),
        card.agent.readonly,
    )
}

/// Cache attention by projecting the same classification used by phase and
/// every write gate.
pub(crate) fn projected_attention(
    layer: &CodexLayer,
    stream_phase: Option<&StreamPhase>,
) -> Attention {
    classify(Some(layer), stream_phase, None, false).attention()
}

pub fn phase(model: &Model, agent: model::AgentId) -> CodexPhase {
    classify_model(model, agent).phase()
}

pub fn send_gate(model: &Model, agent: model::AgentId) -> SendGate {
    classify_model(model, agent).send_gate()
}

#[derive(Clone, Copy)]
pub(super) enum WriteAction {
    Prompt,
    Steer,
    Interrupt,
    Answer,
}

#[derive(Clone, Copy)]
pub(super) enum WritePermission {
    Allowed,
    Refused(&'static str),
}

impl WritePermission {
    fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// The situation states in which the session itself can still accept some
/// write. `session_state` narrows into this, so session-level refusal states
/// are stated once and an action rule cannot observe or restate them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveState {
    AwaitingApproval,
    BlockedUnsupported,
    Responding,
    Executing,
    Working,
    Finished,
    Idle,
}

pub(super) fn write_permission(
    model: &Model,
    agent: model::AgentId,
    action: WriteAction,
) -> WritePermission {
    let situation = classify_model(model, agent);
    let live = match session_state(&situation) {
        Err(message) => return WritePermission::Refused(message),
        Ok(live) => live,
    };
    match action {
        WriteAction::Interrupt if situation.active_turn => WritePermission::Allowed,
        WriteAction::Interrupt => {
            WritePermission::Refused("cannot interrupt without an active turn")
        }
        _ if situation.input_in_flight => WritePermission::Refused(REFUSAL_INPUT_IN_FLIGHT),
        WriteAction::Prompt => match live {
            LiveState::Finished | LiveState::Idle => WritePermission::Allowed,
            LiveState::AwaitingApproval | LiveState::BlockedUnsupported => {
                WritePermission::Refused(REFUSAL_NEEDS_YOU)
            }
            _ => WritePermission::Refused(REFUSAL_ACTIVE),
        },
        WriteAction::Steer => match live {
            LiveState::Responding | LiveState::Executing | LiveState::Working
                if situation.active_turn =>
            {
                WritePermission::Allowed
            }
            LiveState::AwaitingApproval | LiveState::BlockedUnsupported => {
                WritePermission::Refused(REFUSAL_NEEDS_YOU)
            }
            _ => WritePermission::Refused("cannot steer without an active turn"),
        },
        WriteAction::Answer => match live {
            LiveState::AwaitingApproval => WritePermission::Allowed,
            _ => WritePermission::Refused("cannot answer without a pending Codex approval"),
        },
    }
}

/// The single statement of which situations refuse every write because the
/// session cannot accept one, versus the live states an action rule then
/// judges. Returning the narrowed `LiveState` rather than an `Option<&str>`
/// makes the compiler enforce the boundary: move a state across it and every
/// action rule stops compiling instead of reaching a runtime panic in a UI
/// reducer.
fn session_state(situation: &Situation) -> Result<LiveState, &'static str> {
    let live = match &situation.state {
        SituationState::Unavailable => return Err(REFUSAL_UNAVAILABLE),
        SituationState::Exited => return Err(REFUSAL_EXITED),
        SituationState::Closed => return Err(REFUSAL_CLOSED),
        SituationState::Replaying => return Err(REFUSAL_REPLAYING),
        SituationState::ReadOnly => return Err(REFUSAL_READ_ONLY),
        SituationState::Unknown => return Err(REFUSAL_UNKNOWN),
        SituationState::AwaitingApproval { .. } => LiveState::AwaitingApproval,
        SituationState::BlockedUnsupported { .. } => LiveState::BlockedUnsupported,
        SituationState::Responding { .. } => LiveState::Responding,
        SituationState::Executing { .. } => LiveState::Executing,
        SituationState::Working => LiveState::Working,
        SituationState::Finished => LiveState::Finished,
        SituationState::Idle => LiveState::Idle,
    };
    if situation.observer_readonly {
        Err(REFUSAL_OBSERVER_READ_ONLY)
    } else {
        Ok(live)
    }
}

pub fn allows_prompt(model: &Model, agent: model::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Prompt).is_allowed()
}

pub fn allows_steer(model: &Model, agent: model::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Steer).is_allowed()
}

pub fn allows_interrupt(model: &Model, agent: model::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Interrupt).is_allowed()
}

pub fn allows_answer(model: &Model, agent: model::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Answer).is_allowed()
}

pub(crate) fn check_projection_invariant(
    model: &Model,
    agent: model::AgentId,
    attention: Attention,
    out: &mut Vec<Violation>,
) {
    let situation = classify_model(model, agent);
    let phase = situation.phase();
    let send_gate = situation.send_gate();
    let phase_agrees = phase != CodexPhase::Unknown || attention == Attention::Unknown;
    let attention_agrees = if situation.observer_readonly && send_gate == SendGate::ObserverReadOnly
    {
        true
    } else {
        match attention {
            Attention::Unknown => true,
            Attention::Idle => send_gate == SendGate::Ready,
            Attention::Working => matches!(
                send_gate,
                SendGate::Ready | SendGate::ActiveTurn | SendGate::InputInFlight
            ),
            Attention::NeedsYou {
                why: Why::Permission | Why::Question,
            } => send_gate == SendGate::NeedsYou,
            Attention::NeedsYou { why: Why::Finished } => send_gate == SendGate::Ready,
        }
    };
    if !phase_agrees || !attention_agrees {
        out.push(Violation::Codex(CodexViolation::ProjectionDisagreement {
            agent,
            classification: format!("{:?}", situation.state),
            phase,
            attention,
            send_gate,
        }));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::msg::StreamCloseReason;

    fn agent() -> model::AgentId {
        Uuid::from_u128(77)
    }

    fn violation_kinds(layer: &CodexLayer) -> Vec<&'static str> {
        let mut violations = Vec::new();
        layer.check_invariants(agent(), &mut violations);
        violations.iter().map(Violation::kind).collect()
    }

    #[test]
    fn authoritative_thread_close_outranks_stale_overlay() {
        let mut layer = CodexLayer::default();
        layer.observe(1, Utc::now(), &json!({"type": "amux.codex_ready"}));
        layer.observe(2, Utc::now(), &json!({"type": "thread/closed"}));
        layer.invalidate();

        let stream_phase = StreamPhase::Closed {
            reason: StreamCloseReason::AgentDeleted,
        };
        let situation = classify(Some(&layer), Some(&stream_phase), None, false);

        assert_eq!(situation.state, SituationState::Closed);
        assert_eq!(situation.phase(), CodexPhase::Idle);
        assert_eq!(situation.send_gate(), SendGate::Closed);
    }

    #[test]
    fn detects_duplicate_input_identity() {
        let mut layer = CodexLayer::default();
        for n in 0..2 {
            layer.inputs.push_back(InFlightInput {
                op: OpId(Uuid::from_u128(100 + n)),
                input_id: vec![1, 2, 3],
                kind: InFlightKind::Prompt,
            });
        }

        assert!(violation_kinds(&layer).contains(&"codex-duplicate-input"));
    }

    #[test]
    fn classifier_covers_every_kernel_stream_branch_and_exit_attention() {
        let mut layer = CodexLayer::default();
        layer.observe(1, Utc::now(), &json!({"type": "amux.codex_ready"}));
        let cases = [
            (Some(StreamPhase::Opening), SituationState::Replaying),
            (Some(StreamPhase::Replaying), SituationState::Replaying),
            (Some(StreamPhase::Live), SituationState::Idle),
            (
                Some(StreamPhase::Closed {
                    reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                }),
                SituationState::Idle,
            ),
            (
                Some(StreamPhase::Closed {
                    reason: StreamCloseReason::AgentDeleted,
                }),
                SituationState::Idle,
            ),
            (
                Some(StreamPhase::Closed {
                    reason: StreamCloseReason::HostUnreachable,
                }),
                SituationState::Unknown,
            ),
            (None, SituationState::Unknown),
        ];
        for (stream, expected) in cases {
            assert_eq!(
                classify(Some(&layer), stream.as_ref(), None, false).state,
                expected
            );
        }

        let exited = classify(
            Some(&layer),
            Some(&StreamPhase::Live),
            Some(&AgentPhase::Exited { exit_code: Some(1) }),
            false,
        );
        assert_eq!(exited.state, SituationState::Exited);
        assert_eq!(exited.attention(), Attention::Unknown);

        layer.observe(
            2,
            Utc::now(),
            &json!({"type": "turn/started", "turn": {"id": "turn-live"}}),
        );
        layer.inputs.push_back(InFlightInput {
            op: OpId(Uuid::from_u128(200)),
            input_id: vec![2],
            kind: InFlightKind::Steer {
                text: "keep going".to_string(),
            },
        });
        let active_with_input = classify(Some(&layer), Some(&StreamPhase::Live), None, false);
        assert!(active_with_input.active_turn);
        assert!(active_with_input.input_in_flight);
        assert_eq!(active_with_input.attention(), Attention::Working);
        assert_eq!(active_with_input.send_gate(), SendGate::InputInFlight);
    }
}
