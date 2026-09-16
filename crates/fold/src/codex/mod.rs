//! Codex stream observation shared by clients and the daemon.

mod fold;

use std::collections::{BTreeMap, VecDeque};

use model::AgentMessageKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The native structured protocol owned by this layer.
pub const PROTOCOL: &str = "codex_sdk_v1";

/// The source tail and the layer retain the same number of entries.
pub const FEED_RETAINED: usize = 1000;
/// Pending obligations live outside the feed window.  This cap matches the
/// backend's complete Codex row ring rather than the smaller UI tail.
pub const ASKS_RETAINED: usize = 8192;
/// A compact command/patch preview; full content remains behind the stream.
pub const OUTPUT_HEAD_MAX: usize = 4096;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry<C> {
    pub id: u64,
    /// Stream sequence of the row that created this entry.
    pub seq: u64,
    pub kind: FeedEntryKind<C>,
}

/// Ten Codex-native entry kinds.  Work subtypes express Codex's broad item
/// vocabulary without leaking a generic cross-agent representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum FeedEntryKind<C> {
    Prompt(PromptEntry<C>),
    Message(MessageEntry<C>),
    Reasoning(ReasoningEntry),
    Work(WorkEntry),
    McpStartup(McpStartupEntry),
    /// A message another amux agent sent to this one, from the row the
    /// daemon writes because the native thread shows nothing.
    AgentMessage(AgentMessageEntry),
    Turn(TurnEntry),
    Boundary(BoundaryEntry),
    Error(ErrorEntry),
    Unrecognized(UnrecognizedEntry),
}

/// A message delivered by amux, as the daemon recorded accepting it.
/// Structurally unlike Claude's: the Codex carrier injects the message
/// into a thread rather than into text, so the fields are the daemon's own
/// rather than whatever a transcript could recover — and the carrier that
/// took it is a fact worth keeping, since three are possible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageEntry {
    pub id: Option<String>,
    pub context: Option<String>,
    /// Who sent it: `name/host`, or `human`.
    pub from: String,
    pub kind: AgentMessageKind,
    pub text: String,
    /// Which carrier accepted it: `inject_queued`, `inject_started`, or
    /// the `turn_started` fallback.
    pub delivery: Option<String>,
}

/// Clients aggregate these per-server statuses themselves today. When two
/// clients need the same counts, add a counts projection beside this type
/// instead of writing a second aggregation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpStartupEntry {
    pub servers: BTreeMap<String, McpServerStartup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerStartup {
    pub status: McpStartupStatus,
    pub error: Option<String>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptEntry<C> {
    pub item_id: String,
    pub source: PromptSource,
    pub parts: Vec<PromptPart>,
    /// Text parts split into prose and attachment mentions.
    pub content: C,
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
pub struct MessageEntry<C> {
    pub item_id: String,
    pub text: String,
    /// Message text split into prose and attachment mentions.
    pub content: C,
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
    AmuxSend {
        to: String,
        text: String,
        success: Option<bool>,
    },
    /// One of amux's own agent tools, reached through the MCP server amux
    /// runs for the thread. Separated from `McpTool` because these are the
    /// fleet acting on itself — spawning, stopping and messaging agents the
    /// human can see — and reading them as calls to some anonymous server
    /// would bury the only work a chat can explain in the fleet's own words.
    AmuxTool {
        tool: String,
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
    /// Input tokens the provider served from its cache. Counted inside
    /// `input_tokens`, not beside it, so a breakdown states it as a share
    /// rather than adding it in again.
    pub cached_input_tokens: Option<u64>,
    /// Input tokens the provider wrote into its cache this turn. Also a
    /// share of `input_tokens`; zero in every recording so far, and
    /// stated so the breakdown reports what the app-server reports.
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub model_context_window: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum BoundaryEntry {
    Resumed,
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
    /// Choices interpreted for V1. Dynamic tool calls are the explicit
    /// exception because upstream sends `null` while the backend accepts the
    /// layer-supplied binary decisions.
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
        proposed_execpolicy_amendment: Option<Vec<String>>,
        proposed_network_policy_amendments: Vec<NetworkPolicyAmendment>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkPolicyAction {
    Allow,
    Deny,
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
    /// Retained as the opaque provider fact for dumps and agreement checks;
    /// renderers and answer dispatch use only typed `meaning`.
    pub wire: Value,
    pub meaning: AskActionMeaning,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "meaning", rename_all = "snake_case")]
pub enum AskActionMeaning {
    Scalar {
        decision: CodexDecision,
    },
    AcceptWithExecpolicyAmendment {
        matches_proposal: bool,
    },
    ApplyNetworkPolicyAmendment {
        amendment: NetworkPolicyAmendment,
        proposed: bool,
    },
    EmptyObject,
    UnknownObject {
        kind: String,
        scalar_details: Vec<String>,
    },
    UnknownScalar {
        detail: String,
    },
}

impl AskAction {
    pub fn decision(&self) -> Option<CodexDecision> {
        match self.meaning {
            AskActionMeaning::Scalar { decision } => Some(decision),
            AskActionMeaning::AcceptWithExecpolicyAmendment { .. }
            | AskActionMeaning::ApplyNetworkPolicyAmendment { .. }
            | AskActionMeaning::EmptyObject
            | AskActionMeaning::UnknownObject { .. }
            | AskActionMeaning::UnknownScalar { .. } => None,
        }
    }

    pub(crate) fn from_wire(wire: Value, context: &AskContext) -> Self {
        let meaning = classify_ask_action(&wire, context);
        Self { wire, meaning }
    }
}

fn classify_ask_action(wire: &Value, context: &AskContext) -> AskActionMeaning {
    if let Some(decision) = wire.as_str().and_then(CodexDecision::from_wire) {
        return AskActionMeaning::Scalar { decision };
    }
    let Some(object) = wire.as_object() else {
        return AskActionMeaning::UnknownScalar {
            detail: scalar_details(wire).join(" · "),
        };
    };
    let Some((kind, body)) = object.iter().next() else {
        return AskActionMeaning::EmptyObject;
    };
    if object.len() == 1 {
        match kind.as_str() {
            "acceptWithExecpolicyAmendment" => {
                let amendment = execpolicy_amendment(body);
                let matches_proposal = match context {
                    AskContext::Command {
                        proposed_execpolicy_amendment: Some(proposed),
                        ..
                    } => amendment.as_ref() == Some(proposed),
                    _ => false,
                };
                return AskActionMeaning::AcceptWithExecpolicyAmendment { matches_proposal };
            }
            "applyNetworkPolicyAmendment" => {
                let amendment = network_policy_amendment(body);
                if let AskContext::Command {
                    proposed_network_policy_amendments,
                    ..
                } = context
                {
                    if let Some(amendment) = amendment.as_ref() {
                        let proposed = proposed_network_policy_amendments.contains(amendment);
                        return AskActionMeaning::ApplyNetworkPolicyAmendment {
                            amendment: amendment.clone(),
                            proposed,
                        };
                    }
                    return AskActionMeaning::UnknownObject {
                        kind: sanitize_decision_text(kind),
                        scalar_details: Vec::new(),
                    };
                }
                if let Some(amendment) = amendment {
                    let action = match amendment.action {
                        NetworkPolicyAction::Allow => "allow",
                        NetworkPolicyAction::Deny => "deny",
                    };
                    return AskActionMeaning::UnknownObject {
                        kind: sanitize_decision_text(kind),
                        scalar_details: vec![amendment.host, action.to_string()],
                    };
                }
            }
            _ => {}
        }
    }
    AskActionMeaning::UnknownObject {
        kind: sanitize_decision_text(kind),
        scalar_details: scalar_details(body),
    }
}

fn execpolicy_amendment(value: &Value) -> Option<Vec<String>> {
    value
        .get("execpolicy_amendment")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .map(|value| value.map(str::to_owned))
        .collect()
}

fn network_policy_amendment(value: &Value) -> Option<NetworkPolicyAmendment> {
    let amendment = value.get("network_policy_amendment")?;
    let host = amendment.get("host")?.as_str()?.to_string();
    let action = match amendment.get("action")?.as_str()? {
        "allow" => NetworkPolicyAction::Allow,
        "deny" => NetworkPolicyAction::Deny,
        _ => return None,
    };
    Some(NetworkPolicyAmendment { host, action })
}

fn scalar_details(value: &Value) -> Vec<String> {
    fn collect(value: &Value, scalars: &mut Vec<String>) {
        match value {
            Value::Null => {}
            Value::Bool(value) => scalars.push(value.to_string()),
            Value::Number(value) => scalars.push(value.to_string()),
            Value::String(value) => {
                let value = sanitize_decision_text(value);
                if !value.is_empty() {
                    scalars.push(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, scalars);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    collect(value, scalars);
                }
            }
        }
    }

    let mut scalars = Vec::new();
    collect(value, &mut scalars);
    scalars
}

fn sanitize_decision_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '{' | '}' | '"' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

/// The reducer-owned, bounded visible feed.
///
/// This is deliberately part of the serializable model rather than renderer
/// state. Recorder replay, terminal paint watermarks, and mobile projection
/// all read the same entries and eviction offset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleWindow<E, const MAX: usize> {
    entries: VecDeque<E>,
    evicted: u64,
}

impl<E, const MAX: usize> Default for VisibleWindow<E, MAX> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            evicted: 0,
        }
    }
}

impl<E, const MAX: usize> VisibleWindow<E, MAX> {
    pub fn iter(&self) -> impl Iterator<Item = &E> {
        self.entries.iter()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut E> {
        self.entries.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    fn front(&self) -> Option<&E> {
        self.entries.front()
    }

    fn back(&self) -> Option<&E> {
        self.entries.back()
    }

    fn push(&mut self, entry: E) -> Option<E> {
        self.entries.push_back(entry);
        if self.entries.len() > MAX {
            self.evicted = self.evicted.saturating_add(1);
            self.entries.pop_front()
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invariant {
    RetentionOverflow {
        store: &'static str,
        len: usize,
        cap: usize,
    },
    FeedOrder,
    IndexAhead {
        index: &'static str,
        entry: u64,
        next: u64,
    },
    DuplicateAsk,
}

/// Provider-derived Codex state for one agent.
///
/// `C` is the client's presentation content for prompt/message text. The
/// shared fold receives a pure text adapter, allowing UI attachment segments
/// without a dependency on `ui-state`; daemon observers can use a cheaper
/// representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation<C> {
    truncated_start: bool,
    history_loss: bool,
    ready_count: u64,
    replay_complete: bool,
    gap: bool,
    read_only: bool,
    thread_closed: bool,
    window: VisibleWindow<FeedEntry<C>, FEED_RETAINED>,
    next_entry_id: u64,
    item_entries: BTreeMap<String, u64>,
    turn_entries: BTreeMap<String, u64>,
    asks: VecDeque<Ask>,
    pending_approval_context: Option<AskContext>,
    turn: TurnState,
    accumulators: Accumulators,
    latest_usage: Option<TokenUsage>,
}

impl<C> Default for Observation<C> {
    fn default() -> Self {
        Self {
            truncated_start: false,
            history_loss: false,
            ready_count: 0,
            replay_complete: false,
            gap: false,
            read_only: false,
            thread_closed: false,
            window: VisibleWindow::default(),
            next_entry_id: 0,
            item_entries: BTreeMap::new(),
            turn_entries: BTreeMap::new(),
            asks: VecDeque::new(),
            pending_approval_context: None,
            turn: TurnState::default(),
            accumulators: Accumulators::default(),
            latest_usage: None,
        }
    }
}

impl<C> Observation<C> {
    pub fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            truncated_start: truncated,
            ..Self::default()
        };
    }

    pub fn observe(&mut self, seq: u64, row: &Value, content: impl Fn(&str) -> C) {
        fold::observe(self, seq, row, &content);
    }

    pub fn observe_replay_complete(&mut self) {
        self.replay_complete = true;
    }

    pub fn observe_exit(&mut self) {
        self.asks.clear();
        self.accumulators = Accumulators::default();
        self.turn.active_id = None;
    }

    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry<C>> {
        self.window.iter()
    }

    pub fn entry_count(&self) -> usize {
        self.window.len()
    }

    pub fn evicted_entries(&self) -> u64 {
        self.window.evicted()
    }

    pub fn history_truncated(&self) -> bool {
        self.truncated_start || self.window.evicted() > 0 || self.history_loss
    }

    pub fn token_usage(&self) -> Option<&TokenUsage> {
        self.latest_usage.as_ref()
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

    pub fn activity(&self) -> Activity {
        if self.thread_closed {
            Activity::Closed
        } else if self.gap {
            Activity::Unknown
        } else if self.read_only {
            Activity::ReadOnly
        } else if !(self.ready_count > 0 || self.truncated_start && self.replay_complete) {
            Activity::Replaying
        } else if let Some(ask) = self.asks.front() {
            Activity::AwaitingApproval {
                request_id: ask.request_id.clone(),
            }
        } else if let Some(item_id) = self.accumulators.unsupported.front() {
            Activity::BlockedUnsupported {
                item_id: item_id.clone(),
            }
        } else if self.turn.active_id.is_some() {
            match self.accumulators.active_items.back() {
                Some(ActiveItem {
                    item_id,
                    kind: ActiveItemKind::Message,
                }) => Activity::Responding {
                    item_id: item_id.clone(),
                },
                Some(ActiveItem {
                    item_id,
                    kind: ActiveItemKind::Work,
                }) => Activity::Executing {
                    item_id: item_id.clone(),
                },
                _ => Activity::Working,
            }
        } else if self.turn.status == ThreadStatus::Active {
            Activity::Working
        } else if self.truncated_start
            && self.turn.last.is_none()
            && self.turn.status == ThreadStatus::Unknown
        {
            Activity::Unknown
        } else {
            match self.turn.last {
                Some(LastTurn::Completed | LastTurn::Failed) => Activity::Finished,
                Some(LastTurn::Interrupted) | None => Activity::Idle,
            }
        }
    }

    pub fn set_work_state(&mut self, item_id: &str, state: WorkState) {
        fold::set_work_state(self, item_id, state);
    }

    pub fn invariants(&self) -> Vec<Invariant> {
        let mut out = Vec::new();
        for (store, len, cap) in [
            ("feed", self.window.len(), FEED_RETAINED),
            ("asks", self.asks.len(), ASKS_RETAINED),
        ] {
            if len > cap {
                out.push(Invariant::RetentionOverflow { store, len, cap });
            }
        }
        let coherent = self.window.evicted() + self.window.len() as u64 == self.next_entry_id
            && self
                .window
                .front()
                .is_none_or(|entry| entry.id == self.window.evicted())
            && self
                .window
                .back()
                .is_none_or(|entry| entry.id + 1 == self.next_entry_id);
        if !coherent {
            out.push(Invariant::FeedOrder);
        }
        for (index, entry) in self
            .item_entries
            .values()
            .map(|entry| ("items", *entry))
            .chain(self.turn_entries.values().map(|entry| ("turns", *entry)))
        {
            if entry >= self.next_entry_id {
                out.push(Invariant::IndexAhead {
                    index,
                    entry,
                    next: self.next_entry_id,
                });
            }
        }
        if self.asks.iter().enumerate().any(|(i, ask)| {
            self.asks
                .iter()
                .skip(i + 1)
                .any(|other| ask.request_id == other.request_id)
        }) {
            out.push(Invariant::DuplicateAsk);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn visible_window_keeps_a_bounded_model_feed_and_monotone_offset() {
        let mut window = VisibleWindow::<u64, 3>::default();
        assert_eq!(window.push(10), None);
        assert_eq!(window.push(11), None);
        assert_eq!(window.push(12), None);
        assert_eq!(window.push(13), Some(10));
        assert_eq!(window.push(14), Some(11));
        assert_eq!(window.iter().copied().collect::<Vec<_>>(), [12, 13, 14]);
        assert_eq!(window.evicted(), 2);
    }

    #[test]
    fn observation_owns_codex_feed_and_activity_without_ui_state() {
        let mut observation = Observation::<String>::default();
        observation.observe(1, &json!({"type":"amux.codex_ready"}), str::to_owned);
        observation.observe(
            2,
            &json!({"type":"turn/started","turn":{"id":"turn-1"}}),
            str::to_owned,
        );
        observation.observe(
            3,
            &json!({
                "type":"item/agentMessage/delta",
                "itemId":"message-1",
                "delta":"hello"
            }),
            str::to_owned,
        );

        assert_eq!(observation.entry_count(), 1);
        assert!(matches!(
            observation.entries().next().map(|entry| &entry.kind),
            Some(FeedEntryKind::Message(MessageEntry { text, .. })) if text == "hello"
        ));
        assert_eq!(
            observation.activity(),
            Activity::Responding {
                item_id: "message-1".to_string()
            }
        );
    }

    #[test]
    fn detects_retention_and_feed_arithmetic_failures() {
        let mut observation = Observation::<String>::default();
        for id in 0..=FEED_RETAINED as u64 {
            observation.window.entries.push_back(FeedEntry {
                id,
                seq: id,
                kind: FeedEntryKind::Unrecognized(UnrecognizedEntry {
                    method: "test".to_string(),
                    detail: None,
                }),
            });
        }
        observation.next_entry_id = FEED_RETAINED as u64 + 1;
        assert!(matches!(
            observation.invariants().as_slice(),
            [Invariant::RetentionOverflow { store: "feed", .. }]
        ));

        observation.next_entry_id += 1;
        let invariants = observation.invariants();
        assert!(
            invariants
                .iter()
                .any(|invariant| matches!(invariant, Invariant::FeedOrder))
        );
    }

    #[test]
    fn detects_an_index_ahead_of_the_feed() {
        let mut observation = Observation::<String>::default();
        observation.item_entries.insert("ghost".to_string(), 9);

        assert!(observation.invariants().iter().any(|invariant| matches!(
            invariant,
            Invariant::IndexAhead {
                index: "items",
                entry: 9,
                next: 0,
            }
        )));
    }

    #[test]
    fn detects_duplicate_ask_identity() {
        let mut observation = Observation::<String>::default();
        for _ in 0..2 {
            observation.asks.push_back(Ask {
                seq: 1,
                request_id: json!("same"),
                context: AskContext::Command {
                    item_id: "item".to_string(),
                    command: "true".to_string(),
                    cwd: None,
                    reason: None,
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: Vec::new(),
                },
                actions: Vec::new(),
            });
        }

        assert!(
            observation
                .invariants()
                .iter()
                .any(|invariant| matches!(invariant, Invariant::DuplicateAsk))
        );
    }

    #[test]
    fn network_amendment_edge_cases_preserve_contextual_fallback_facts() {
        let command = AskContext::Command {
            item_id: "command".to_string(),
            command: "cargo test".to_string(),
            cwd: None,
            reason: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: Vec::new(),
        };
        let malformed = json!({
            "applyNetworkPolicyAmendment": {"network_policy_amendment": {"host": 7}}
        });
        assert_eq!(
            classify_ask_action(&malformed, &command),
            AskActionMeaning::UnknownObject {
                kind: "applyNetworkPolicyAmendment".to_string(),
                scalar_details: Vec::new(),
            }
        );

        let file_change = AskContext::FileChange {
            item_id: "patch".to_string(),
            reason: None,
            changes: Vec::new(),
        };
        let parseable = json!({
            "applyNetworkPolicyAmendment": {
                "network_policy_amendment": {"host": "crates.io", "action": "allow"}
            }
        });
        assert_eq!(
            classify_ask_action(&parseable, &file_change),
            AskActionMeaning::UnknownObject {
                kind: "applyNetworkPolicyAmendment".to_string(),
                scalar_details: vec!["crates.io".to_string(), "allow".to_string()],
            }
        );
    }
}
