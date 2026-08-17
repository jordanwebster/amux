//! The Codex chat layer: a typed child model folding native `codex_sdk_v1`
//! rows into Codex-owned view state.  The kernel sees only the layer's
//! `Attention` summary; no Claude-shaped or generic content model sits
//! between these rows and this fold.

mod fold;
pub(crate) mod update;

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{AgentPhase, Attention, Model, StreamPhase, Violation, Why};
use crate::msg::OpId;

/// The native structured protocol owned by this layer.
pub const PROTOCOL: &str = "codex_sdk_v1";

/// The source tail and the layer retain the same number of entries.
pub(crate) const FEED_RETAINED: usize = 1000;
/// Pending obligations live outside the feed window.  This cap matches the
/// backend's complete Codex row ring rather than the smaller UI tail.
pub(crate) const ASKS_RETAINED: usize = 8192;
/// Inputs awaiting their correlated `amux.input_result` row.
pub(crate) const INPUTS_RETAINED: usize = 64;
/// A compact command/patch preview; full content remains behind the stream.
pub(crate) const OUTPUT_HEAD_MAX: usize = 4096;

/// Codex-native client writes.  Prompt and steer are deliberately distinct:
/// starting a turn never silently turns into steering an existing one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "codex_command", rename_all = "snake_case")]
pub enum CodexCommand {
    Prompt {
        agent: amux::AgentId,
        text: String,
    },
    Steer {
        agent: amux::AgentId,
        text: String,
    },
    Answer {
        agent: amux::AgentId,
        request_id: Value,
        decision: CodexDecision,
    },
    Interrupt {
        agent: amux::AgentId,
    },
}

/// The four V1 decisions the frozen backend input accepts.  Object-valued
/// `availableDecisions` remain visible on an ask but are disabled in V1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodexDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

impl CodexDecision {
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::AcceptForSession => "acceptForSession",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "accept" => Some(Self::Accept),
            "acceptForSession" => Some(Self::AcceptForSession),
            "decline" => Some(Self::Decline),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// Serializable mirror of the frozen Codex protobuf input oneof.  The shell
/// maps it field-for-field to `amux::codex_io::CodexSdkV1Input`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "codex_input", rename_all = "snake_case")]
pub enum CodexInput {
    UserTurn {
        input: Vec<u8>,
    },
    Steer {
        turn_id: String,
        input: Vec<u8>,
    },
    Interrupt {
        turn_id: String,
    },
    ApprovalDecision {
        request_id: Vec<u8>,
        decision: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub id: u64,
    /// Stream sequence of the row that created this entry.
    pub seq: u64,
    pub kind: FeedEntryKind,
}

/// Eight Codex-native entry kinds.  Work subtypes express Codex's broad item
/// vocabulary without leaking a generic cross-agent representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum FeedEntryKind {
    Prompt(PromptEntry),
    Message(MessageEntry),
    Reasoning(ReasoningEntry),
    Work(WorkEntry),
    Turn(TurnEntry),
    Boundary(BoundaryEntry),
    Error(ErrorEntry),
    Unrecognized(UnrecognizedEntry),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub item_id: String,
    pub source: PromptSource,
    pub parts: Vec<PromptPart>,
    pub finality: ItemFinality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSource {
    Protocol,
    SteerEcho,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum PromptPart {
    Text {
        text: String,
    },
    Image {
        url: Option<String>,
    },
    LocalImage {
        path: Option<String>,
    },
    Other {
        item_type: Option<String>,
        raw: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemFinality {
    Open,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEntry {
    pub item_id: String,
    pub text: String,
    pub phase: MessagePhase,
    pub finality: ItemFinality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningEntry {
    pub item_id: String,
    pub text: String,
    pub summary: Vec<String>,
    pub finality: ItemFinality,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkEntry {
    pub item_id: String,
    pub kind: WorkKind,
    pub state: WorkState,
    pub stdout_head: String,
    pub stderr_head: String,
    pub output_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "work", rename_all = "snake_case")]
pub enum WorkKind {
    Command {
        command: String,
        cwd: Option<String>,
        exit_code: Option<i32>,
    },
    FileChange {
        changes: Vec<FileChange>,
        patch_head: String,
        patch_truncated: bool,
    },
    Plan {
        text: String,
        explanation: Option<String>,
        steps: Vec<PlanStep>,
    },
    McpTool {
        server: String,
        tool: String,
        arguments: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
    DynamicTool {
        tool: String,
        namespace: Option<String>,
        arguments: Value,
        success: Option<bool>,
    },
    WebSearch {
        query: String,
        action: Option<Value>,
    },
    UnsupportedUserInput {
        questions: Value,
    },
    Other {
        item_type: String,
        raw: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: Option<String>,
    pub diff: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkState {
    Proposed,
    AwaitingApproval { request_id: Value },
    Running,
    Done { outcome: WorkOutcome },
    Denied,
    Abandoned { reason: ApprovalResolution },
    BlockedUnsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOutcome {
    Succeeded,
    Failed,
    Declined,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEntry {
    pub turn_id: String,
    pub status: TurnStatus,
    pub token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub model_context_window: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum BoundaryEntry {
    Ready,
    Gap { reason: String },
    Compacted { turn_id: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub severity: ErrorSeverity,
    pub message: String,
    pub will_retry: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    Notice,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnrecognizedEntry {
    pub method: String,
    pub detail: Option<String>,
}

/// One live, answerable obligation keyed by the opaque JSON request id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub seq: u64,
    pub request_id: Value,
    pub context: AskContext,
    /// Exact wire value from `amux.codex_approval_required`.
    pub available_decisions: Value,
    /// Choices interpreted for V1. Usually these mirror the wire value;
    /// dynamic tool calls are the explicit exception because upstream sends
    /// `null` while the backend accepts the layer-supplied binary decisions.
    /// Unknown/object wire choices remain visible with `decision: None`.
    pub actions: Vec<AskAction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum AskContext {
    Command {
        item_id: String,
        command: String,
        cwd: Option<String>,
        reason: Option<String>,
    },
    FileChange {
        item_id: String,
        reason: Option<String>,
        changes: Vec<FileChange>,
    },
    Permissions {
        item_id: String,
        reason: Option<String>,
        permissions: Value,
    },
    DynamicTool {
        item_id: String,
        tool: String,
        namespace: Option<String>,
        arguments: Value,
    },
}

impl AskContext {
    pub fn item_id(&self) -> &str {
        match self {
            Self::Command { item_id, .. }
            | Self::FileChange { item_id, .. }
            | Self::Permissions { item_id, .. }
            | Self::DynamicTool { item_id, .. } => item_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskAction {
    pub wire: Value,
    pub decision: Option<CodexDecision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolution {
    Answered,
    AnsweredElsewhere,
    ResponseFailed,
    ConnectionLost,
    QueueOverflow,
    EventStreamError,
    SessionStopped,
    Unknown,
}

impl ApprovalResolution {
    fn from_wire(reason: &str) -> Self {
        match reason {
            "answered" => Self::Answered,
            "answered_elsewhere" => Self::AnsweredElsewhere,
            "response_failed" => Self::ResponseFailed,
            "connection_lost" => Self::ConnectionLost,
            "queue_overflow" => Self::QueueOverflow,
            "event_stream_error" => Self::EventStreamError,
            "session_stopped" => Self::SessionStopped,
            _ => Self::Unknown,
        }
    }

    fn proceeded(self) -> bool {
        matches!(self, Self::Answered | Self::AnsweredElsewhere)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InFlightInput {
    pub op: OpId,
    pub input_id: Vec<u8>,
    pub kind: InFlightKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InFlightKind {
    Prompt,
    /// The text is retained because the steer echo is rendered from it once
    /// the send succeeds; the turn id is not, because `op` already correlates
    /// the result and `CodexInput::Steer` already carries the id on the wire.
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
            Self::ReadOnly => Some(REFUSAL_READ_ONLY),
            Self::Unknown => Some(REFUSAL_UNKNOWN),
            Self::InputInFlight => Some(REFUSAL_INPUT_IN_FLIGHT),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CodexViolation {
    RetentionOverflow {
        agent: amux::AgentId,
        store: &'static str,
        len: usize,
        cap: usize,
    },
    FeedOrder {
        agent: amux::AgentId,
    },
    IndexAhead {
        agent: amux::AgentId,
        index: &'static str,
        entry: u64,
        next: u64,
    },
    DuplicateAsk {
        agent: amux::AgentId,
    },
    DuplicateInput {
        agent: amux::AgentId,
    },
    ProjectionDisagreement {
        agent: amux::AgentId,
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
            Self::FeedOrder { .. } => "codex-feed-order",
            Self::IndexAhead { .. } => "codex-index-ahead",
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
            Self::FeedOrder { agent } => {
                write!(f, "agent {agent} codex feed id arithmetic is incoherent")
            }
            Self::IndexAhead {
                agent,
                index,
                entry,
                next,
            } => write!(
                f,
                "agent {agent} codex {index} references entry {entry} past next id {next}"
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum ThreadStatus {
    #[default]
    Unknown,
    Active,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum LastTurn {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct TurnState {
    active_id: Option<String>,
    status: ThreadStatus,
    last: Option<LastTurn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ActiveItemKind {
    Prompt,
    Message,
    Reasoning,
    Work,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ActiveItem {
    item_id: String,
    kind: ActiveItemKind,
}

/// One ordered interpretation of the layer's phase/attention facts. Public
/// projections deliberately lose different details, so those details live
/// here instead of being independently rediscovered by each projection.
#[derive(Clone, Debug, PartialEq)]
struct Situation {
    state: SituationState,
    active_turn: bool,
    input_in_flight: bool,
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
        if self.input_in_flight
            && !matches!(
                self.state,
                SituationState::Unavailable
                    | SituationState::Exited
                    | SituationState::Closed
                    | SituationState::Replaying
                    | SituationState::ReadOnly
                    | SituationState::Unknown
            )
        {
            return SendGate::InputInFlight;
        }
        match self.state {
            SituationState::Unavailable => SendGate::Unavailable,
            SituationState::Exited => SendGate::Exited,
            SituationState::Closed => SendGate::Closed,
            SituationState::Replaying => SendGate::Replaying,
            SituationState::Responding { .. }
            | SituationState::Executing { .. }
            | SituationState::Working => SendGate::ActiveTurn,
            SituationState::AwaitingApproval { .. } | SituationState::BlockedUnsupported { .. } => {
                SendGate::NeedsYou
            }
            SituationState::ReadOnly => SendGate::ReadOnly,
            SituationState::Unknown => SendGate::Unknown,
            SituationState::Finished | SituationState::Idle => SendGate::Ready,
        }
    }

    fn with_state(mut self, state: SituationState) -> Self {
        self.state = state;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Accumulators {
    message_text: BTreeMap<String, String>,
    reasoning_text: BTreeMap<String, String>,
    reasoning_summary: BTreeMap<String, Vec<String>>,
    command_stdout: BTreeMap<String, String>,
    command_stderr: BTreeMap<String, String>,
    active_items: VecDeque<ActiveItem>,
    unsupported: VecDeque<String>,
}

/// The Codex layer state for one agent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexLayer {
    truncated_start: bool,
    evicted: u64,
    history_loss: bool,
    ready_count: u64,
    replay_complete: bool,
    stale: bool,
    gap: bool,
    read_only: bool,
    thread_closed: bool,
    exited: bool,
    entries: VecDeque<FeedEntry>,
    next_entry_id: u64,
    item_entries: BTreeMap<String, u64>,
    turn_entries: BTreeMap<String, u64>,
    asks: VecDeque<Ask>,
    pending_approval_context: Option<AskContext>,
    inputs: VecDeque<InFlightInput>,
    turn: TurnState,
    accumulators: Accumulators,
    latest_usage: Option<TokenUsage>,
}

impl CodexLayer {
    pub(crate) fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            truncated_start: truncated,
            ..Self::default()
        };
    }

    pub(crate) fn observe(&mut self, seq: u64, arrived: DateTime<Utc>, row: &Value) {
        fold::observe(self, seq, arrived, row);
    }

    pub(crate) fn observe_replay_complete(&mut self) {
        self.replay_complete = true;
    }

    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(crate) fn observe_exit(&mut self) {
        self.asks.clear();
        self.inputs.clear();
        self.accumulators = Accumulators::default();
        self.turn.active_id = None;
        self.exited = true;
    }

    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn history_truncated(&self) -> bool {
        self.truncated_start || self.evicted > 0 || self.history_loss
    }

    pub fn evicted_entries(&self) -> u64 {
        self.evicted
    }

    pub fn asks(&self) -> impl Iterator<Item = &Ask> {
        self.asks.iter()
    }

    pub fn ask_head(&self) -> Option<&Ask> {
        self.asks.front()
    }

    pub fn ask_count(&self) -> usize {
        self.asks.len()
    }

    pub fn active_turn_id(&self) -> Option<&str> {
        self.turn.active_id.as_deref()
    }

    pub fn in_flight_inputs(&self) -> impl Iterator<Item = &InFlightInput> {
        self.inputs.iter()
    }

    fn live(&self) -> bool {
        self.ready_count > 0 || self.truncated_start && self.replay_complete
    }

    /// The layer-only diagnostic projection assumes an admitted live stream.
    /// Model/card consumers use [`cached_attention`], which supplies the real
    /// kernel lifecycle to the same classifier.
    pub fn attention(&self) -> Attention {
        classify(Some(self), Some(&StreamPhase::Live), None).attention()
    }

    pub(crate) fn working_is_stale(&self, _now: Option<DateTime<Utc>>) -> bool {
        // Codex has authoritative turn lifecycle rows; unlike Claude it does
        // not need an elapsed-time inference cap.
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

    pub(crate) fn check_invariants(&self, agent: amux::AgentId, out: &mut Vec<Violation>) {
        for (store, len, cap) in [
            ("feed", self.entries.len(), FEED_RETAINED),
            ("asks", self.asks.len(), ASKS_RETAINED),
            ("inputs", self.inputs.len(), INPUTS_RETAINED),
        ] {
            if len > cap {
                out.push(Violation::Codex(CodexViolation::RetentionOverflow {
                    agent,
                    store,
                    len,
                    cap,
                }));
            }
        }

        let coherent = self.evicted + self.entries.len() as u64 == self.next_entry_id
            && self
                .entries
                .front()
                .is_none_or(|entry| entry.id == self.evicted)
            && self
                .entries
                .back()
                .is_none_or(|entry| entry.id + 1 == self.next_entry_id);
        if !coherent {
            out.push(Violation::Codex(CodexViolation::FeedOrder { agent }));
        }

        for (index, entry) in self
            .item_entries
            .values()
            .map(|entry| ("items", *entry))
            .chain(self.turn_entries.values().map(|entry| ("turns", *entry)))
        {
            if entry >= self.next_entry_id {
                out.push(Violation::Codex(CodexViolation::IndexAhead {
                    agent,
                    index,
                    entry,
                    next: self.next_entry_id,
                }));
            }
        }

        let duplicate_ask = self.asks.iter().enumerate().any(|(i, ask)| {
            self.asks
                .iter()
                .skip(i + 1)
                .any(|other| ask.request_id == other.request_id)
        });
        if duplicate_ask {
            out.push(Violation::Codex(CodexViolation::DuplicateAsk { agent }));
        }

        let duplicate_input = self.inputs.iter().enumerate().any(|(i, input)| {
            self.inputs
                .iter()
                .skip(i + 1)
                .any(|other| input.input_id == other.input_id)
        });
        if duplicate_input {
            out.push(Violation::Codex(CodexViolation::DuplicateInput { agent }));
        }
    }
}

/// Cache attention by projecting the same classification used by phase and
/// every write gate.
pub(crate) fn projected_attention(
    layer: &CodexLayer,
    stream_phase: Option<&StreamPhase>,
) -> Attention {
    classify(Some(layer), stream_phase, None).attention()
}

/// The one ordered Codex classification. This is the only Codex-layer code
/// that reads kernel `StreamPhase`; every projection consumes its lossless
/// result.
fn classify(
    layer: Option<&CodexLayer>,
    stream_phase: Option<&StreamPhase>,
    agent_phase: Option<&AgentPhase>,
) -> Situation {
    let Some(layer) = layer else {
        return Situation::unavailable();
    };
    let situation = Situation {
        state: SituationState::Unknown,
        active_turn: layer.turn.active_id.is_some(),
        input_in_flight: !layer.inputs.is_empty(),
    };
    if matches!(agent_phase, Some(AgentPhase::Exited { .. })) {
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

    let state = if layer.exited {
        SituationState::Exited
    } else if layer.thread_closed {
        SituationState::Closed
    } else if layer.stale || layer.gap {
        SituationState::Unknown
    } else if layer.read_only {
        SituationState::ReadOnly
    } else if !layer.live() {
        SituationState::Replaying
    } else if let Some(ask) = layer.asks.front() {
        SituationState::AwaitingApproval {
            request_id: ask.request_id.clone(),
        }
    } else if let Some(item_id) = layer.accumulators.unsupported.front() {
        SituationState::BlockedUnsupported {
            item_id: item_id.clone(),
        }
    } else if layer.turn.active_id.is_some() {
        match layer.accumulators.active_items.back() {
            Some(ActiveItem {
                item_id,
                kind: ActiveItemKind::Message,
            }) => SituationState::Responding {
                item_id: item_id.clone(),
            },
            Some(ActiveItem {
                item_id,
                kind: ActiveItemKind::Work,
            }) => SituationState::Executing {
                item_id: item_id.clone(),
            },
            _ => SituationState::Working,
        }
    } else if layer.turn.status == ThreadStatus::Active {
        SituationState::Working
    } else if layer.truncated_start
        && layer.turn.last.is_none()
        && layer.turn.status == ThreadStatus::Unknown
    {
        SituationState::Unknown
    } else {
        match layer.turn.last {
            Some(LastTurn::Completed | LastTurn::Failed) => SituationState::Finished,
            Some(LastTurn::Interrupted) | None => SituationState::Idle,
        }
    };
    situation.with_state(state)
}

fn classify_model(model: &Model, agent: amux::AgentId) -> Situation {
    let Some(card) = model.agent(agent) else {
        return classify(None, None, None);
    };
    classify(
        card.codex(),
        model.stream(agent).map(|stream| &stream.phase),
        Some(&card.phase),
    )
}

/// Derived Codex phase, wrapped in kernel stream lifecycle facts.
pub fn phase(model: &Model, agent: amux::AgentId) -> CodexPhase {
    classify_model(model, agent).phase()
}

pub fn send_gate(model: &Model, agent: amux::AgentId) -> SendGate {
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

/// The situation states in which the session itself can still accept SOME
/// write. `session_state` narrows into this, so which states are
/// session-level refusals is stated once, in one function, and an action rule
/// literally cannot see (or restate) that membership.
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
    agent: amux::AgentId,
    action: WriteAction,
) -> WritePermission {
    let situation = classify_model(model, agent);
    let live = match session_state(&situation.state) {
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
            LiveState::Responding | LiveState::Executing | LiveState::Working => {
                WritePermission::Refused(REFUSAL_ACTIVE)
            }
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
            LiveState::Responding
            | LiveState::Executing
            | LiveState::Working
            | LiveState::Finished
            | LiveState::Idle => WritePermission::Refused("cannot steer without an active turn"),
        },
        WriteAction::Answer => match live {
            LiveState::AwaitingApproval => WritePermission::Allowed,
            LiveState::BlockedUnsupported
            | LiveState::Responding
            | LiveState::Executing
            | LiveState::Working
            | LiveState::Finished
            | LiveState::Idle => {
                WritePermission::Refused("cannot answer without a pending Codex approval")
            }
        },
    }
}

/// The single statement of which situations refuse every write because the
/// *session* cannot accept one, versus the live states an action rule then
/// judges. Returning the narrowed `LiveState` rather than an `Option<&str>` is
/// what makes the compiler the enforcer: move a state across this boundary and
/// every action rule stops compiling, instead of reaching a runtime panic in a
/// UI reducer.
fn session_state(state: &SituationState) -> Result<LiveState, &'static str> {
    match state {
        SituationState::Unavailable => Err(REFUSAL_UNAVAILABLE),
        SituationState::Exited => Err(REFUSAL_EXITED),
        SituationState::Closed => Err(REFUSAL_CLOSED),
        SituationState::Replaying => Err(REFUSAL_REPLAYING),
        SituationState::ReadOnly => Err(REFUSAL_READ_ONLY),
        SituationState::Unknown => Err(REFUSAL_UNKNOWN),
        SituationState::AwaitingApproval { .. } => Ok(LiveState::AwaitingApproval),
        SituationState::BlockedUnsupported { .. } => Ok(LiveState::BlockedUnsupported),
        SituationState::Responding { .. } => Ok(LiveState::Responding),
        SituationState::Executing { .. } => Ok(LiveState::Executing),
        SituationState::Working => Ok(LiveState::Working),
        SituationState::Finished => Ok(LiveState::Finished),
        SituationState::Idle => Ok(LiveState::Idle),
    }
}

pub fn allows_prompt(model: &Model, agent: amux::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Prompt).is_allowed()
}

pub fn allows_steer(model: &Model, agent: amux::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Steer).is_allowed()
}

pub fn allows_interrupt(model: &Model, agent: amux::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Interrupt).is_allowed()
}

pub fn allows_answer(model: &Model, agent: amux::AgentId) -> bool {
    write_permission(model, agent, WriteAction::Answer).is_allowed()
}

pub(crate) fn check_projection_invariant(
    model: &Model,
    agent: amux::AgentId,
    attention: Attention,
    out: &mut Vec<Violation>,
) {
    let situation = classify_model(model, agent);
    let phase = situation.phase();
    let send_gate = situation.send_gate();
    let phase_agrees = phase != CodexPhase::Unknown || attention == Attention::Unknown;
    let attention_agrees = match attention {
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

    fn agent() -> amux::AgentId {
        Uuid::from_u128(77)
    }

    fn kinds(layer: &CodexLayer) -> Vec<&'static str> {
        let mut violations = Vec::new();
        layer.check_invariants(agent(), &mut violations);
        violations.iter().map(Violation::kind).collect()
    }

    #[test]
    fn detects_retention_and_feed_arithmetic_failures() {
        let mut layer = CodexLayer::default();
        for id in 0..=FEED_RETAINED as u64 {
            layer.entries.push_back(FeedEntry {
                id,
                seq: id,
                kind: FeedEntryKind::Unrecognized(UnrecognizedEntry {
                    method: "test".to_string(),
                    detail: None,
                }),
            });
        }
        layer.next_entry_id = FEED_RETAINED as u64 + 1;
        assert!(kinds(&layer).contains(&"codex-retention-overflow"));
        layer.next_entry_id += 1;
        assert!(kinds(&layer).contains(&"codex-feed-order"));
    }

    #[test]
    fn detects_an_index_ahead_of_the_feed() {
        let mut layer = CodexLayer::default();
        layer.item_entries.insert("ghost".to_string(), 9);
        assert!(kinds(&layer).contains(&"codex-index-ahead"));
    }

    #[test]
    fn detects_duplicate_ask_and_input_identity() {
        let mut layer = CodexLayer::default();
        for n in 0..2 {
            layer.asks.push_back(Ask {
                seq: 1,
                request_id: json!("same"),
                context: AskContext::Command {
                    item_id: "item".to_string(),
                    command: "true".to_string(),
                    cwd: None,
                    reason: None,
                },
                available_decisions: json!([]),
                actions: Vec::new(),
            });
            layer.inputs.push_back(InFlightInput {
                op: OpId(Uuid::from_u128(100 + n)),
                input_id: vec![1, 2, 3],
                kind: InFlightKind::Prompt,
            });
        }
        let kinds = kinds(&layer);
        assert!(kinds.contains(&"codex-duplicate-ask"));
        assert!(kinds.contains(&"codex-duplicate-input"));
    }

    #[test]
    fn classifier_covers_every_kernel_stream_branch_and_exit_attention() {
        use crate::msg::StreamCloseReason;

        let mut layer = CodexLayer {
            ready_count: 1,
            ..CodexLayer::default()
        };
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
                classify(Some(&layer), stream.as_ref(), None).state,
                expected
            );
        }

        let exited = classify(
            Some(&layer),
            Some(&StreamPhase::Live),
            Some(&AgentPhase::Exited { exit_code: Some(1) }),
        );
        assert_eq!(exited.state, SituationState::Exited);
        assert_eq!(exited.attention(), Attention::Unknown);

        layer.turn.active_id = Some("turn-live".to_string());
        layer.inputs.push_back(InFlightInput {
            op: OpId(Uuid::from_u128(200)),
            input_id: vec![2],
            kind: InFlightKind::Steer {
                text: "keep going".to_string(),
            },
        });
        let active_with_input = classify(Some(&layer), Some(&StreamPhase::Live), None);
        assert!(active_with_input.active_turn);
        assert!(active_with_input.input_in_flight);
        assert_eq!(active_with_input.attention(), Attention::Working);
        assert_eq!(active_with_input.send_gate(), SendGate::InputInFlight);
    }
}
