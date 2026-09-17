//! The Model: everything a renderer may read, all derivations included.
//!
//! Views format, never decide (`docs/UI.md`): fleet ordering, display-name
//! fallback, status labels are computed here, once. Reducer-visible
//! collections are `BTreeMap`s so iteration order is canonical.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use model::{Agent, AgentId, AgentParent, HostEntry, HostId, WorkingOn};
pub use model::{AgentPhase, Attention, StructuredProtocol, Why};
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudeLayer, ClaudeViolation};
pub use crate::claude_sdk::ClaudeSdkLayer;
use crate::codex::{CodexLayer, CodexViolation};
use crate::msg::{Command, DisconnectReason, OpId, OpOutcome, StreamCloseReason};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalSummary {
    pub fold: fold::AgentFold,
    pub through: model::Seq,
}

/// How many finished ops the Model retains (retention is explicitly bounded;
/// old outcomes age out, pending obligations never do — they live in
/// `pending_ops` until resolved).
pub(crate) const FINISHED_OPS_RETAINED: usize = 64;

/// Connection state of the daemon link, epoch-scoped. `Connected` starts in
/// catch-up; the two synchronized flags flip as each snapshot completes, so
/// renderers can tell "loading" from "empty".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "connection", rename_all = "snake_case")]
pub enum Connection {
    Connecting,
    Connected {
        hosts_synchronized: bool,
        agents_synchronized: bool,
    },
    Disconnected {
        reason: DisconnectReason,
    },
}

/// Typed per-agent state. Exhaustive dispatch is deliberate: a new agent
/// adds an enum arm and keeps its native vocabulary intact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "state", rename_all = "snake_case")]
pub enum AgentLayer {
    Claude(ClaudeLayer),
    ClaudeSdk(ClaudeSdkLayer),
    Codex(CodexLayer),
}

impl AgentLayer {
    /// The layer an agent's kind determines. A kind with no native chat in
    /// this build has none; nothing is inferred from what a stream carries.
    pub fn from_kind(kind: &model::AgentKind) -> Option<Self> {
        match kind {
            model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            } => Some(Self::Claude(ClaudeLayer::default())),
            model::AgentKind::Claude {
                driver: model::ClaudeDriver::Sdk,
            } => Some(Self::ClaudeSdk(ClaudeSdkLayer::default())),
            model::AgentKind::Codex => Some(Self::Codex(CodexLayer::default())),
            model::AgentKind::TestAgent => None,
        }
    }

    pub(crate) fn protocol(&self) -> StructuredProtocol {
        match self {
            Self::Claude(_) => StructuredProtocol::ClaudePtyTranscript,
            Self::ClaudeSdk(_) => StructuredProtocol::ClaudeSdk,
            Self::Codex(_) => StructuredProtocol::Codex,
        }
    }

    pub fn claude(&self) -> Option<&ClaudeLayer> {
        match self {
            Self::Claude(layer) => Some(layer),
            Self::ClaudeSdk(_) | Self::Codex(_) => None,
        }
    }

    pub(crate) fn claude_mut(&mut self) -> Option<&mut ClaudeLayer> {
        match self {
            Self::Claude(layer) => Some(layer),
            Self::ClaudeSdk(_) | Self::Codex(_) => None,
        }
    }

    pub fn claude_sdk(&self) -> Option<&ClaudeSdkLayer> {
        match self {
            Self::ClaudeSdk(layer) => Some(layer),
            Self::Claude(_) | Self::Codex(_) => None,
        }
    }

    pub fn codex(&self) -> Option<&CodexLayer> {
        match self {
            Self::Claude(_) | Self::ClaudeSdk(_) => None,
            Self::Codex(layer) => Some(layer),
        }
    }

    pub(crate) fn codex_mut(&mut self) -> Option<&mut CodexLayer> {
        match self {
            Self::Claude(_) | Self::ClaudeSdk(_) => None,
            Self::Codex(layer) => Some(layer),
        }
    }

    pub(crate) fn begin_window(&mut self, truncated: bool) {
        match self {
            Self::Claude(layer) => layer.begin_window(truncated),
            Self::ClaudeSdk(layer) => layer.begin_window(truncated),
            Self::Codex(layer) => layer.begin_window(truncated),
        }
    }

    pub(crate) fn restore_head(&mut self, head: Option<&crate::store::HeadDto>) {
        match (self, head) {
            (Self::Claude(layer), Some(crate::store::HeadDto::Claude(head))) => {
                layer.restore_head(&head.tip)
            }
            (Self::ClaudeSdk(layer), Some(crate::store::HeadDto::ClaudeSdk(head))) => {
                layer.restore_head(&head.tip)
            }
            (Self::Codex(layer), Some(crate::store::HeadDto::Codex(head))) => {
                layer.restore_head(&head.tip)
            }
            (layer, None) => layer.invalidate(),
            (layer, Some(_)) => layer.invalidate(),
        }
    }

    pub(crate) fn observe(&mut self, seq: u64, at: DateTime<Utc>, payload: &serde_json::Value) {
        match self {
            Self::Claude(layer) => layer.observe(seq, at, payload),
            Self::ClaudeSdk(layer) => layer.observe(seq, at, payload),
            Self::Codex(layer) => layer.observe(seq, at, payload),
        }
    }

    pub(crate) fn discard_feed(&mut self) {
        match self {
            Self::Claude(layer) => layer.discard_feed(),
            Self::ClaudeSdk(layer) => layer.discard_feed(),
            Self::Codex(layer) => layer.discard_feed(),
        }
    }

    pub(crate) fn observe_replay_complete(&mut self) {
        match self {
            Self::Claude(layer) => layer.observe_replay_complete(),
            Self::ClaudeSdk(_) => {}
            Self::Codex(layer) => layer.observe_replay_complete(),
        }
    }

    pub(crate) fn observe_exit(&mut self) {
        match self {
            Self::Claude(layer) => layer.observe_exit(),
            Self::ClaudeSdk(layer) => layer.observe_exit(),
            Self::Codex(layer) => layer.observe_exit(),
        }
    }

    pub(crate) fn invalidate(&mut self) {
        match self {
            Self::Claude(layer) => layer.invalidate(),
            Self::ClaudeSdk(layer) => layer.invalidate(),
            Self::Codex(layer) => layer.invalidate(),
        }
    }

    /// Derive cached fleet attention from each layer's one stream-aware
    /// classification. Cached Claude attention is deliberately time-free;
    /// read-time degradation remains in [`Model::effective_attention`].
    pub(crate) fn attention(&self, stream_phase: Option<&StreamPhase>) -> Attention {
        match self {
            Self::Claude(layer) => crate::claude::cached_attention(layer, stream_phase),
            Self::ClaudeSdk(layer) => layer.attention(stream_phase),
            Self::Codex(layer) => crate::codex::projected_attention(layer, stream_phase),
        }
    }

    pub(crate) fn working_is_stale(&self, now: Option<DateTime<Utc>>) -> bool {
        match self {
            Self::Claude(layer) => layer.working_is_stale(now),
            Self::ClaudeSdk(_) => false,
            Self::Codex(layer) => layer.working_is_stale(now),
        }
    }

    pub(crate) fn check_invariants(&self, agent: AgentId, out: &mut Vec<Violation>) {
        match self {
            Self::Claude(layer) => layer.check_invariants(agent, out),
            Self::ClaudeSdk(_) => {}
            Self::Codex(layer) => layer.check_invariants(agent, out),
        }
    }
}

/// One agent in the fleet: wire facts plus UI-layer derived state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCard {
    pub agent: Agent,
    /// Adapter-translated provider label. Nothing populates it in V1 (the
    /// naming translation cleanup is deferred); the display fallback already
    /// consults it.
    pub provider_label: Option<String>,
    pub attention: Attention,
    /// Recency for fleet ranking: stream activity when observed, otherwise
    /// the creation time.
    pub last_activity: DateTime<Utc>,
    pub phase: AgentPhase,
    /// Loaded from the local store and not yet confirmed by this connection's
    /// authoritative host snapshot.
    #[serde(default)]
    pub remembered: bool,
    /// The shared provider fold for this device's currently open stream.
    /// Fleet selection compares this whole summary with the host envelope.
    pub(crate) local_summary: Option<LocalSummary>,
    /// Typed native layer state. `None` until the structured stream
    /// produces evidence; unsupported agents honestly stay `Unknown`.
    pub(crate) layer: Option<AgentLayer>,
    /// Epoch of the last upsert; entities from older epochs are pruned when
    /// a reconnect snapshot completes.
    pub(crate) epoch: u64,
}

impl AgentCard {
    /// The native structured protocol selected from this agent's advertised
    /// protocols, when it is one this UI knows how to render.
    pub fn structured_protocol(&self) -> Option<StructuredProtocol> {
        AgentLayer::from_kind(&self.agent.kind).map(|layer| layer.protocol())
    }

    /// The Claude chat layer's feed facts, when the agent's structured
    /// stream has produced any.
    pub fn claude(&self) -> Option<&ClaudeLayer> {
        self.layer.as_ref().and_then(AgentLayer::claude)
    }

    pub fn claude_sdk(&self) -> Option<&ClaudeSdkLayer> {
        self.layer.as_ref().and_then(AgentLayer::claude_sdk)
    }

    /// The Codex chat layer's native view state, when available.
    pub fn codex(&self) -> Option<&CodexLayer> {
        self.layer.as_ref().and_then(AgentLayer::codex)
    }

    /// The agent that spawned this one, when it has one. A wire fact: the
    /// owning daemon records the edge, so a card knows its parent even when
    /// the parent lives on another host and is not in this inventory.
    pub fn parent(&self) -> Option<AgentParent> {
        self.agent.parent
    }

    /// What this agent says it is working on, and when it last said so.
    pub fn working_on(&self) -> Option<&WorkingOn> {
        self.agent.working_on.as_ref()
    }

    /// Display name fallback: user-assigned name, then provider label, then
    /// short id.
    pub fn display_name(&self) -> String {
        display_name_fallback(
            self.agent.name.as_deref(),
            self.provider_label.as_deref(),
            &self.agent.id,
        )
    }

    /// The fleet status word for the given EFFECTIVE attention — the Model
    /// applies read-time policy (host reachability, staleness) before
    /// calling, so the label and the badge are always the same fact.
    /// Attention takes precedence; phase shows when not running. Readonly
    /// rows (A3: they open in chat only) state `read-only` as their
    /// resting word — an inventory fact, more informative than idle/`–` —
    /// while live attention words still win over it.
    ///
    /// Keeping these words in the Model is a deliberate exception while the
    /// terminal fleet is their only consumer. When a second consumer needs
    /// status labels, precedence stays here and each renderer owns its words.
    pub(crate) fn status_label(&self, attention: Attention, phase: &AgentPhase) -> String {
        match (&attention, phase) {
            (_, AgentPhase::Exited { exit_code }) => match exit_code {
                Some(code) => format!("exited({code})"),
                None => "exited".to_string(),
            },
            (
                Attention::NeedsYou {
                    why: Why::Permission,
                },
                _,
            ) => "permission".to_string(),
            (Attention::NeedsYou { why: Why::Question }, _) => "question".to_string(),
            (Attention::NeedsYou { why: Why::Finished }, _) => "finished".to_string(),
            (Attention::Working, _) => "working".to_string(),
            (Attention::Idle, _) | (Attention::Unknown, _) if self.agent.readonly => {
                "read-only".to_string()
            }
            (Attention::Idle, _) => "idle".to_string(),
            (Attention::Unknown, _) => "–".to_string(),
        }
    }
}

/// Fleet ranking: `NeedsYou` first by urgency, everything else by recency.
fn attention_rank(attention: Attention) -> u8 {
    match attention {
        Attention::NeedsYou {
            why: Why::Permission,
        } => 0,
        Attention::NeedsYou { why: Why::Question } => 1,
        Attention::NeedsYou { why: Why::Finished } => 2,
        _ => 3,
    }
}

/// How loudly one attention speaks for a whole family, most urgent first.
/// Finer than `attention_rank`, which only has to order rows: a collapsed
/// family shows ONE badge for several agents, so the summary needs to
/// separate the three values the row order lumps together. `Unknown`
/// outranks `Idle` deliberately — a family holding a member we cannot see
/// is not a family we may call idle (summaries are honest about
/// incompleteness; degradation is to `Unknown`, never to a wrong badge).
fn attention_severity(attention: Attention) -> u8 {
    match attention {
        Attention::NeedsYou {
            why: Why::Permission,
        } => 0,
        Attention::NeedsYou { why: Why::Question } => 1,
        Attention::NeedsYou { why: Why::Finished } => 2,
        Attention::Working => 3,
        Attention::Unknown => 4,
        Attention::Idle => 5,
    }
}

/// Display name fallback shared by every client: `name`, then
/// `provider_label`, then the first eight hex digits of the id.
pub fn display_name_fallback(
    name: Option<&str>,
    provider_label: Option<&str>,
    id: &AgentId,
) -> String {
    if let Some(name) = name
        && !name.is_empty()
    {
        return name.to_string();
    }
    if let Some(label) = provider_label
        && !label.is_empty()
    {
        return label.to_string();
    }
    id.simple().to_string()[..8].to_string()
}

/// One known host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub entry: HostEntry,
    pub(crate) epoch: u64,
}

/// A dispatched, unresolved operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingOp {
    pub op: OpId,
    /// Dispatch order (monotonic per Model); canonical ordering for pending
    /// rows and failure recency.
    pub seq: u64,
    pub command: Command,
}

/// A resolved operation. Outcomes are state — a lost outcome must not leave
/// a spinner lying — retained bounded (`FINISHED_OPS_RETAINED`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinishedOp {
    pub op: OpId,
    pub seq: u64,
    pub command: Command,
    pub outcome: OpOutcome,
}

/// Session-stream bookkeeping for one agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamState {
    pub phase: StreamPhase,
    /// Replay started past the beginning of the stream; summaries over it
    /// must degrade to `Unknown` when the window lacks evidence.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stream_phase", rename_all = "snake_case")]
pub enum StreamPhase {
    /// Requested by effect, not yet opened.
    Opening,
    /// Opened, replaying history.
    Replaying,
    Live,
    Closed {
        reason: StreamCloseReason,
    },
}

pub use ::model::{AgentMessageKind, AgentMessagePresentation};

/// The sender named by an amux message envelope.
///
/// Parsed addresses retain the original text beside their typed parts so a
/// client can pass through an address whose host is not in its inventory
/// without normalizing or shortening it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMessageSender<'a> {
    Address {
        name: &'a str,
        host: HostId,
        raw: &'a str,
    },
    Raw(&'a str),
}

impl<'a> AgentMessageSender<'a> {
    pub fn raw(self) -> &'a str {
        match self {
            Self::Address { raw, .. } | Self::Raw(raw) => raw,
        }
    }
}

/// The closed form of a message body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageDigest<'a> {
    /// The line a closed body shows: its first line with anything on it.
    pub head: &'a str,
    /// How many further lines have anything on them. Source lines, not
    /// screen rows — how a line wraps is the renderer's business, and the
    /// count exists to say whether closing hides anything at all.
    pub hidden_lines: usize,
}

/// Close a message body to one line. One derivation, so every client that
/// draws a chat closes a body at the same place, and an empty body is
/// honestly nothing rather than a blank line pretending to be content.
pub fn message_digest(text: &str) -> MessageDigest<'_> {
    let mut lines = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty());
    let head = lines.next().unwrap_or_default();
    MessageDigest {
        head,
        hidden_lines: lines.filter(|line| !line.is_empty()).count(),
    }
}

/// One agent below a family's top row, with the generations between them
/// so a renderer can indent without walking parent edges itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyMember<'a> {
    pub card: &'a AgentCard,
    /// 1 for a direct child, 2 for a grandchild, and so on.
    pub depth: usize,
}

/// A child asking for the human, addressed for its parent's chat.
///
/// Nothing is copied out of the child here. The ask itself stays where the
/// child folded it, in the child's own layer, and a renderer draws it by
/// asking that layer for it under the child's id — so the parent's chat
/// decides where the ask is drawn while the child's layer decides what it
/// looks like, and answering from either place is the same act.
///
/// The one-line ask detail is renderer wording while only one surface draws
/// family banners. When a second surface draws them, add a typed per-agent ask
/// digest so two banners cannot describe the same ask differently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyNeed<'a> {
    pub card: &'a AgentCard,
    /// Generations below the parent, counted as [`FamilyMember`] counts
    /// them: a grandchild's ask surfaces too, and says how far down it is.
    pub depth: usize,
    /// Why the human is wanted, in the same three-word vocabulary the
    /// fleet badge uses. A message from another agent is not one of them:
    /// only states that need a person raise attention at all.
    pub why: Why,
}

impl FamilyNeed<'_> {
    /// The child to address — the id a command carries and a layer is
    /// looked up by.
    pub fn agent(&self) -> AgentId {
        self.card.agent.id
    }

    /// Which layer knows how to draw this ask, when the child advertises
    /// one this build renders.
    pub fn layer(&self) -> Option<StructuredProtocol> {
        self.card.structured_protocol()
    }
}

/// One row of the ranked fleet.
#[derive(Clone, Debug, PartialEq)]
pub enum FleetItem<'a> {
    Agent(&'a AgentCard),
    /// An agent that spawned others. The family occupies ONE row and is
    /// ranked as one thing; `children` travels with it so a renderer that
    /// expands the row has the descendants already ranked, and one that
    /// leaves it collapsed shows only the count and the summary badge.
    Family {
        parent: &'a AgentCard,
        /// Every descendant, depth-first in family rank order.
        children: Vec<FamilyMember<'a>>,
        /// How many agents the collapsed row is standing in for — the
        /// whole subtree, not just the direct children, because that is
        /// what stays hidden while the row is collapsed.
        child_count: usize,
        /// The loudest effective attention anywhere in the family,
        /// including the parent's own.
        highest_attention: Attention,
    },
    /// An optimistic row for an in-flight create.
    PendingCreate {
        op: OpId,
        name: &'a str,
        agent_type: &'a model::AgentType,
        host: Option<HostId>,
    },
}

/// The parent edges of one inventory read, resolved once.
struct Topology<'a> {
    /// Direct children per parent, in family rank order.
    children: BTreeMap<AgentId, Vec<&'a AgentCard>>,
    /// The agents no parent in this inventory claims, in family rank order.
    roots: Vec<&'a AgentCard>,
}

/// The client Model. One per daemon connection; renderers borrow it and
/// format, never derive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub(crate) connection: Connection,
    pub(crate) epoch: u64,
    pub(crate) local_host_id: Option<HostId>,
    /// Cloud auth expired or missing: render the degraded banner, keep local
    /// agents fully usable. Never a blocking screen.
    pub(crate) cloud_auth_required: bool,
    /// Cloud subscription missing: render the degraded banner while local
    /// agents remain fully usable.
    pub(crate) cloud_subscription_required: bool,
    /// The runtime observed structural incoherence this session. This is a
    /// sticky renderer fact, not an invariant derivation: views may format
    /// the warning but must never recompute the violation.
    pub(crate) invariant_warning: bool,
    pub(crate) hosts: BTreeMap<HostId, HostState>,
    pub(crate) agents: BTreeMap<AgentId, AgentCard>,
    /// Last authoritative remote membership; disconnection does not mean deletion.
    pub(crate) remote_inventories: BTreeMap<HostId, BTreeSet<AgentId>>,
    pub(crate) streams: BTreeMap<AgentId, StreamState>,
    /// User-opened conversations outlive the temporary inventory removal of
    /// an unreachable host. Its next inventory re-establishes these streams.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) attached: BTreeMap<AgentId, HostId>,
    pub(crate) queues: BTreeMap<AgentId, crate::QueuedMessage>,
    pub(crate) pending_ops: BTreeMap<OpId, PendingOp>,
    pub(crate) finished_ops: Vec<FinishedOp>,
    pub(crate) op_seq: u64,
    /// Last observed time (enters via `Msg::Tick`).
    pub(crate) now: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(crate) store: crate::store::StoreState,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            connection: Connection::Connecting,
            epoch: 0,
            local_host_id: None,
            cloud_auth_required: false,
            cloud_subscription_required: false,
            invariant_warning: false,
            hosts: BTreeMap::new(),
            agents: BTreeMap::new(),
            remote_inventories: BTreeMap::new(),
            streams: BTreeMap::new(),
            attached: BTreeMap::new(),
            queues: BTreeMap::new(),
            pending_ops: BTreeMap::new(),
            finished_ops: Vec::new(),
            op_seq: 0,
            now: None,
            store: crate::store::StoreState::default(),
        }
    }
}

impl Model {
    /// Serialized sizes of reducer-owned state used by runtime retention
    /// diagnostics. Store-backed chat windows are reported separately.
    pub fn retention(&self) -> ModelRetention {
        fn json_bytes<T: Serialize + ?Sized>(value: &T) -> usize {
            serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
        }

        let effect_state_bytes = json_bytes(&(&self.queues, &self.pending_ops, &self.finished_ops));
        let subscription_state_bytes = json_bytes(&(&self.streams, &self.attached));
        let mut provider_state_count = 0;
        let mut provider_state_bytes = 0usize;
        let mut ask_count = 0usize;
        let mut ask_bytes = 0usize;
        for card in self.agents.values() {
            let Some(layer) = card.layer.as_ref() else {
                continue;
            };
            provider_state_count += 1;
            provider_state_bytes = provider_state_bytes.saturating_add(json_bytes(layer));
            match layer {
                AgentLayer::Claude(layer) => {
                    ask_count += layer.ask_count();
                    ask_bytes = ask_bytes.saturating_add(
                        layer
                            .asks()
                            .map(json_bytes)
                            .fold(0usize, usize::saturating_add),
                    );
                }
                AgentLayer::ClaudeSdk(layer) => {
                    ask_count += layer.ask_count();
                    ask_bytes = ask_bytes.saturating_add(
                        layer
                            .asks()
                            .map(json_bytes)
                            .fold(0usize, usize::saturating_add),
                    );
                }
                AgentLayer::Codex(layer) => {
                    ask_count += layer.ask_count();
                    ask_bytes = ask_bytes.saturating_add(
                        layer
                            .asks()
                            .map(json_bytes)
                            .fold(0usize, usize::saturating_add),
                    );
                }
            }
        }

        ModelRetention {
            effect_state_count: self.queues.len()
                + self.pending_ops.len()
                + self.finished_ops.len(),
            effect_state_bytes,
            subscription_state_count: self.streams.len() + self.attached.len(),
            subscription_state_bytes,
            provider_state_count,
            provider_state_bytes,
            ask_count,
            ask_bytes,
        }
    }

    pub fn queued(&self, agent: AgentId) -> Option<&crate::QueuedMessage> {
        self.queues.get(&agent)
    }

    /// Every held message, whichever agent holds it.
    pub fn queued_messages(&self) -> impl Iterator<Item = (&AgentId, &crate::QueuedMessage)> {
        self.queues.iter()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, Connection::Connected { .. })
    }

    /// False during snapshot catch-up: renderers show "loading", not
    /// "empty".
    pub fn is_synchronized(&self) -> bool {
        matches!(
            self.connection,
            Connection::Connected {
                hosts_synchronized: true,
                agents_synchronized: true,
            }
        )
    }

    pub fn remote_inventories(&self) -> &BTreeMap<HostId, BTreeSet<AgentId>> {
        &self.remote_inventories
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn cloud_auth_required(&self) -> bool {
        self.cloud_auth_required
    }

    pub fn cloud_subscription_required(&self) -> bool {
        self.cloud_subscription_required
    }

    /// Whether the runtime has observed any Model invariant violation this
    /// session. Sticky once set; renderers use this read-only fact for the
    /// persistent diagnostic banner.
    pub fn has_invariant_warning(&self) -> bool {
        self.invariant_warning
    }

    /// Records that the runtime observed a reducer invariant failure before it
    /// persists the diagnostic snapshot.
    pub fn note_invariant_violation(&mut self) {
        self.invariant_warning = true;
    }

    pub fn local_host_id(&self) -> Option<HostId> {
        self.local_host_id
    }

    /// Whether an agent owned by `host` lives on the machine this client
    /// runs on. `None` until the connection has told us which host that
    /// is: unknown locality is a real state, and callers that must choose
    /// an entry mode treat it as "assume local" rather than guessing
    /// remote and hiding the terminal an agent may well have.
    pub fn is_local(&self, host: HostId) -> Option<bool> {
        self.local_host_id.map(|local| local == host)
    }

    pub fn hosts(&self) -> impl Iterator<Item = &HostState> {
        self.hosts.values()
    }

    pub fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub fn host(&self, id: HostId) -> Option<&HostState> {
        self.hosts.get(&id)
    }

    pub fn host_name(&self, id: HostId) -> Option<&str> {
        self.hosts.get(&id).map(|host| host.entry.name.as_str())
    }

    /// Interpret the sender field from amux's own envelope vocabulary while
    /// retaining malformed, human, and otherwise non-address values verbatim.
    pub fn agent_message_sender(from: &str) -> AgentMessageSender<'_> {
        let Some((name, host)) = from.rsplit_once('/') else {
            return AgentMessageSender::Raw(from);
        };
        let Ok(host) = host.parse::<HostId>() else {
            return AgentMessageSender::Raw(from);
        };
        AgentMessageSender::Address {
            name,
            host,
            raw: from,
        }
    }

    /// A host we know nothing about counts as offline.
    pub fn host_online(&self, id: HostId) -> bool {
        self.hosts.get(&id).is_some_and(|host| host.entry.online)
    }

    pub fn agents(&self) -> impl Iterator<Item = &AgentCard> {
        self.agents.values()
    }

    pub fn agent(&self, id: AgentId) -> Option<&AgentCard> {
        self.agents.get(&id)
    }

    /// Agents the fleet shows; the header, empty state, and tickers key on
    /// this so counts never disagree with the list.
    pub fn fleet_agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Whether a conversation is open: the user asked for this agent and has
    /// not closed it. The subscription policy widens for exactly these.
    pub fn is_attached(&self, id: AgentId) -> bool {
        self.attached.contains_key(&id)
    }

    pub fn stream(&self, id: AgentId) -> Option<&StreamState> {
        self.streams.get(&id)
    }

    pub fn chat(&self, id: AgentId) -> Option<&crate::store::ChatWindow> {
        self.store.chats.get(&id)
    }

    pub fn chats(&self) -> impl Iterator<Item = (&AgentId, &crate::store::ChatWindow)> {
        self.store.chats.iter()
    }

    pub fn remembered_chat(&self) -> Option<AgentId> {
        self.store.remembered_chat
    }

    pub fn store_unavailable(&self) -> Option<fold::StoreError> {
        self.store.unavailable
    }

    /// A remembered row of a paired remote host that has not answered this
    /// connection. The connection's own snapshot cannot disprove it: only that
    /// host's inventory, or the host leaving the paired set (removed, or no
    /// longer trusted), can.
    pub(crate) fn awaits_own_host(&self, card: &AgentCard) -> bool {
        let host = card.agent.host_id;
        card.remembered
            && self
                .hosts
                .get(&host)
                .is_some_and(|state| state.entry.trust_status == model::HostTrustStatus::Trusted)
            && self.local_host_id != Some(host)
            && !self.remote_inventories.contains_key(&host)
    }

    /// Whether the store's remembered fleet has been installed, or can no
    /// longer be. Until then an empty fleet means "not read yet", not "none".
    pub fn remembered_fleet_settled(&self) -> bool {
        self.store.fleet_settled || self.store.unavailable.is_some()
    }

    /// The Claude chat layer for an agent (the chat view's read surface).
    pub fn claude(&self, id: AgentId) -> Option<&ClaudeLayer> {
        self.agents.get(&id).and_then(AgentCard::claude)
    }

    pub fn claude_sdk(&self, id: AgentId) -> Option<&ClaudeSdkLayer> {
        self.agents.get(&id).and_then(AgentCard::claude_sdk)
    }

    pub fn codex(&self, id: AgentId) -> Option<&CodexLayer> {
        self.agents.get(&id).and_then(AgentCard::codex)
    }

    pub fn pending_ops(&self) -> impl Iterator<Item = &PendingOp> {
        self.pending_ops.values()
    }

    pub fn finished_ops(&self) -> &[FinishedOp] {
        &self.finished_ops
    }

    pub fn finished_op(&self, op: OpId) -> Option<&FinishedOp> {
        self.finished_ops.iter().find(|finished| finished.op == op)
    }

    /// The most recently finished failed op, for the status line. The
    /// renderer's ViewState decides dismissal (by `seq`); the Model only
    /// reports.
    pub fn latest_op_failure(&self) -> Option<&FinishedOp> {
        self.finished_ops
            .iter()
            .filter(|finished| finished.outcome.is_error())
            .max_by_key(|finished| finished.seq)
    }

    pub fn now(&self) -> Option<DateTime<Utc>> {
        self.now
    }

    /// What a fleet consumer should show for this card's attention: an
    /// offline host means our knowledge is stale, so it degrades to
    /// `Unknown` — computed here, once, for every renderer. The E1
    /// staleness cap applies at read time too: a crashed claude leaves the
    /// cached `Working` stuck, and degrading it here keeps the fleet badge
    /// in agreement with the chat phase (E3) while the folded state stays
    /// time-free.
    pub fn effective_attention(&self, card: &AgentCard) -> Attention {
        if !self.host_online(card.agent.host_id) {
            return Attention::Unknown;
        }
        self.apply_working_staleness_cap(card, card.attention)
    }

    fn apply_working_staleness_cap(&self, card: &AgentCard, attention: Attention) -> Attention {
        if attention == Attention::Working
            && card
                .layer
                .as_ref()
                .is_some_and(|layer| layer.working_is_stale(self.now))
        {
            return Attention::Unknown;
        }
        attention
    }

    /// Attention specifically for fleet presentation. Unlike chat gates and
    /// native projections, this resolves the daemon's advisory envelope.
    pub fn fleet_attention(&self, card: &AgentCard) -> Attention {
        if !self.host_online(card.agent.host_id) {
            return Attention::Unknown;
        }
        if let Some(effective) = self.effective_summary(card) {
            if effective.stale || effective.incompatible {
                return Attention::Unknown;
            }
            return self.apply_working_staleness_cap(card, effective.summary.attention);
        }
        self.effective_attention(card)
    }

    /// The advisory host fold and this client's open-chat fold resolved by
    /// position. Agents without either retain the legacy projection.
    pub fn effective_summary(&self, card: &AgentCard) -> Option<fold::Effective> {
        let protocol = card.structured_protocol()?;
        let producer_version = fold::AgentFold::for_protocol(protocol).tip_version();
        let local = card
            .local_summary
            .as_ref()
            .map(|local| (local.through, local.fold.summary()));
        fold::select_summary(
            card.agent.summary.as_ref(),
            local.as_ref(),
            producer_version,
        )
    }

    pub fn effective_phase(&self, card: &AgentCard) -> AgentPhase {
        self.effective_summary(card)
            .map(|effective| effective.summary.phase)
            .unwrap_or_else(|| card.phase.clone())
    }

    /// Age drawn by the fleet. Activity time wins; an incompatible envelope
    /// has no trusted activity but still retains its observation age.
    pub fn effective_summary_age(&self, card: &AgentCard) -> DateTime<Utc> {
        self.effective_summary(card)
            .and_then(|effective| effective.summary.last_activity.or(effective.observed_at))
            .unwrap_or(card.last_activity)
    }

    /// The fleet status word with read-time policy applied. Live offline rows
    /// show `–`; remembered rows retain the stored standing that makes an
    /// offline launch useful. A live word derives from the SAME effective
    /// attention as its badge, so a staleness-degraded Unknown badge can never
    /// sit beside a stale "working" label (views format, never decide).
    pub fn status_label_for(&self, card: &AgentCard) -> String {
        if !card.remembered && !self.host_online(card.agent.host_id) {
            return "–".to_string();
        }
        if let Some(effective) = self.effective_summary(card) {
            if effective.incompatible {
                return "unknown".to_string();
            }
            if effective.stale {
                return "stale".to_string();
            }
            let attention = if card.remembered {
                effective.summary.attention
            } else {
                self.fleet_attention(card)
            };
            return card.status_label(attention, &effective.summary.phase);
        }
        let attention = self.fleet_attention(card);
        card.status_label(attention, &card.phase)
    }

    /// Every descendant of an agent, ranked exactly as the fleet ranks a
    /// family's children. Empty when the agent has spawned nobody — which
    /// is also the answer for an agent this inventory does not hold.
    pub fn family_of(&self, agent: AgentId) -> Vec<FamilyMember<'_>> {
        let topology = self.topology();
        let mut placed = std::collections::BTreeSet::new();
        self.descendants(&topology, agent, 1, &mut placed)
    }

    /// The agent heading the family this one belongs to — itself when no
    /// parent in this inventory claims it, and `None` when the inventory
    /// does not hold the agent at all.
    ///
    /// The walk stops at an edge naming an agent we cannot see, exactly
    /// as [`Model::fleet`] does, so a family we only half know still has
    /// a top row to hang from and a looping edge still terminates.
    pub fn family_root(&self, agent: AgentId) -> Option<AgentId> {
        let mut seen = std::collections::BTreeSet::new();
        let mut at = self.agents.get(&agent)?;
        while seen.insert(at.agent.id) {
            let Some(parent) = at.parent().and_then(|edge| self.agents.get(&edge.agent_id)) else {
                break;
            };
            at = parent;
        }
        Some(at.agent.id)
    }

    /// Which of an agent's descendants are asking for the human, loudest
    /// first.
    ///
    /// Composed, never synthesized: no record is written into the parent's
    /// stream and no state is stored anywhere, so a child's ask reaches
    /// its parent's chat by the same derivation that puts it on the fleet
    /// badge — and leaves by that derivation too. Answering it in the
    /// child's own view, or on another device, empties this list on the
    /// next fold with nothing to clear.
    ///
    /// The order is over the flattened family, not over its branches: a
    /// consumer that shows one need and counts the rest — the parent's
    /// chat banner does exactly that — must be handed the most urgent one
    /// wherever in the subtree it sits. Tree order alone would hand it the
    /// first need encountered, which is only the loudest by luck: a
    /// grandchild blocked on permission hides under whichever branch the
    /// tree happens to walk later, and the banner would name a finished
    /// sibling while a permission went unanswered.
    ///
    /// The parent's own asks are absent: those belong to its own chat,
    /// which is already showing them.
    pub fn family_needs(&self, parent: AgentId) -> Vec<FamilyNeed<'_>> {
        let mut needs: Vec<FamilyNeed<'_>> = self
            .family_of(parent)
            .into_iter()
            .filter_map(|member| match self.fleet_attention(member.card) {
                // Read-time policy applies here too: a child on an offline
                // host degrades to Unknown, and a banner for an ask we can
                // no longer see would be a promise the chat cannot keep.
                Attention::NeedsYou { why } => Some(FamilyNeed {
                    card: member.card,
                    depth: member.depth,
                    why,
                }),
                _ => None,
            })
            .collect();
        // Stable, so urgency is the only thing this reorders: two needs of
        // the same kind keep the order the family gave them — nearest
        // branch first, siblings in the fleet's own rank.
        needs.sort_by_key(|need| attention_severity(Attention::NeedsYou { why: need.why }));
        needs
    }

    /// Direct-child edges of the current inventory, plus the agents no
    /// parent in this inventory claims. Computed once per read: an edge
    /// naming an agent we cannot see (a parent on an unreachable host)
    /// leaves the child a root, so a family we only half know still
    /// renders every agent it has.
    fn topology(&self) -> Topology<'_> {
        let mut children: BTreeMap<AgentId, Vec<&AgentCard>> = BTreeMap::new();
        let mut roots: Vec<&AgentCard> = Vec::new();
        for card in self.agents.values() {
            match card.parent() {
                Some(parent) if self.agents.contains_key(&parent.agent_id) => {
                    children.entry(parent.agent_id).or_default().push(card);
                }
                _ => roots.push(card),
            }
        }
        for siblings in children.values_mut() {
            siblings.sort_by(|a, b| self.rank_order(a, b));
        }
        roots.sort_by(|a, b| self.rank_order(a, b));
        Topology { children, roots }
    }

    /// Depth-first descendants of one agent, marking each as placed so a
    /// looping edge cannot walk forever.
    fn descendants<'m>(
        &'m self,
        topology: &Topology<'m>,
        of: AgentId,
        depth: usize,
        placed: &mut std::collections::BTreeSet<AgentId>,
    ) -> Vec<FamilyMember<'m>> {
        placed.insert(of);
        let mut members = Vec::new();
        for child in topology.children.get(&of).into_iter().flatten() {
            if !placed.insert(child.agent.id) {
                continue;
            }
            members.push(FamilyMember { card: child, depth });
            members.extend(self.descendants(topology, child.agent.id, depth + 1, placed));
        }
        members
    }

    /// One top-level row and the keys it sorts by: a family ranks as a
    /// unit, on its loudest attention and its most recent activity
    /// anywhere, so a working child never sinks under the idle parent that
    /// hides it.
    fn family_row<'m>(
        &'m self,
        parent: &'m AgentCard,
        children: Vec<FamilyMember<'m>>,
    ) -> (u8, DateTime<Utc>, AgentId, FleetItem<'m>) {
        let key = parent.agent.id;
        if children.is_empty() {
            let attention = self.fleet_attention(parent);
            return (
                attention_rank(attention),
                self.effective_summary_age(parent),
                key,
                FleetItem::Agent(parent),
            );
        }
        let highest_attention = children
            .iter()
            .map(|member| self.fleet_attention(member.card))
            .chain(std::iter::once(self.fleet_attention(parent)))
            .min_by_key(|attention| attention_severity(*attention))
            .unwrap_or(Attention::Unknown);
        let recency = children
            .iter()
            .map(|member| self.effective_summary_age(member.card))
            .chain(std::iter::once(self.effective_summary_age(parent)))
            .max()
            .unwrap_or_else(|| self.effective_summary_age(parent));
        (
            attention_rank(highest_attention),
            recency,
            key,
            FleetItem::Family {
                parent,
                child_count: children.len(),
                highest_attention,
                children,
            },
        )
    }

    /// The order two cards take among their siblings: the same rule the
    /// fleet applies at top level, so an expanded family reads like the
    /// list it sits in.
    fn rank_order(&self, a: &AgentCard, b: &AgentCard) -> std::cmp::Ordering {
        attention_rank(self.fleet_attention(a))
            .cmp(&attention_rank(self.fleet_attention(b)))
            .then(
                self.effective_summary_age(b)
                    .cmp(&self.effective_summary_age(a)),
            )
            .then(a.agent.id.cmp(&b.agent.id))
    }

    /// The fleet: ONE flat list, globally ranked. `NeedsYou` first
    /// (permission, question, finished), then recency; host is a column, not
    /// a grouping. Pending creates render as optimistic rows at the bottom
    /// in dispatch order.
    /// Readonly agents (externally captured sessions the chrome cannot
    /// drive) surface like any other row now that the chat renders them
    /// (A3: they open in chat only — the entry keys enforce it); their
    /// resting status word is `read-only`.
    /// An agent that spawned others occupies one `Family` row carrying its
    /// descendants: the list stays flat and globally ranked, and the family
    /// is ranked as one thing.
    pub fn fleet(&self) -> Vec<FleetItem<'_>> {
        let topology = self.topology();
        let mut placed: std::collections::BTreeSet<AgentId> = std::collections::BTreeSet::new();
        let mut rows: Vec<(u8, DateTime<Utc>, AgentId, FleetItem<'_>)> = Vec::new();

        for root in &topology.roots {
            let children = self.descendants(&topology, root.agent.id, 1, &mut placed);
            rows.push(self.family_row(root, children));
        }
        // A parent edge that loops has no root to hang from. The agents are
        // real and must still be reachable, so each stands alone rather
        // than vanishing into a cycle nobody can expand.
        for card in self.agents.values() {
            if !placed.contains(&card.agent.id) {
                placed.insert(card.agent.id);
                rows.push(self.family_row(card, Vec::new()));
            }
        }

        rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let mut items: Vec<FleetItem<'_>> = rows.into_iter().map(|row| row.3).collect();

        let mut creates: Vec<&PendingOp> = self
            .pending_ops
            .values()
            .filter(|pending| matches!(pending.command, Command::CreateAgent { .. }))
            .collect();
        creates.sort_by_key(|pending| pending.seq);
        for pending in creates {
            if let Command::CreateAgent {
                host,
                name,
                agent_type,
                ..
            } = &pending.command
            {
                items.push(FleetItem::PendingCreate {
                    op: pending.op,
                    name,
                    agent_type,
                    host: *host,
                });
            }
        }
        items
    }
}

/// Encoded reducer ownership excluding the store-backed chat windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelRetention {
    pub effect_state_count: usize,
    pub effect_state_bytes: usize,
    pub subscription_state_count: usize,
    pub subscription_state_bytes: usize,
    /// Full live provider layers; `ask_bytes` is a named subset of this value.
    pub provider_state_count: usize,
    pub provider_state_bytes: usize,
    pub ask_count: usize,
    pub ask_bytes: usize,
}

/// A broken Model invariant, typed and entity-addressed so a release-mode
/// dump names the exact entity that went incoherent.
#[derive(Clone, Debug, PartialEq)]
pub enum Violation {
    /// A `streams` key with no corresponding agent card.
    StreamWithoutAgent {
        agent: AgentId,
    },
    /// An agent card stamped with an epoch the Model has not reached.
    CardEpochAhead {
        agent: AgentId,
        card_epoch: u64,
        model_epoch: u64,
    },
    /// A host stamped with an epoch the Model has not reached.
    HostEpochAhead {
        host: HostId,
        host_epoch: u64,
        model_epoch: u64,
    },
    /// A stale-epoch agent card survived snapshot pruning.
    CardEpochStale {
        agent: AgentId,
        card_epoch: u64,
        model_epoch: u64,
    },
    /// A stale-epoch host survived snapshot pruning.
    HostEpochStale {
        host: HostId,
        host_epoch: u64,
        model_epoch: u64,
    },
    /// `finished_ops` exceeded its explicit retention bound.
    FinishedOpsOverflow {
        len: usize,
        cap: usize,
    },
    /// An agent's ancestry loops, so it belongs to no family the fleet can
    /// expand. The fleet still lists it, standing alone.
    ParentCycle {
        agent: AgentId,
    },
    /// A card's cached attention disagrees with its provider projection.
    /// Codex includes the kernel stream phase; Claude is layer-only.
    AttentionMismatch {
        agent: AgentId,
        card: Attention,
        derived: Attention,
    },
    /// A typed layer's own structural invariant failed.
    ClaudeSdkProjection {
        agent: AgentId,
        phase: crate::claude_sdk::SdkPhase,
        attention: Attention,
        gate: crate::claude_sdk::SendGate,
    },
    Claude(ClaudeViolation),
    Codex(CodexViolation),
}

impl Violation {
    /// Stable per-class key, used by the shell to throttle release-mode
    /// dumps to once per kind per session.
    pub fn kind(&self) -> &'static str {
        match self {
            Violation::StreamWithoutAgent { .. } => "stream-without-agent",
            Violation::CardEpochAhead { .. } => "card-epoch-ahead",
            Violation::HostEpochAhead { .. } => "host-epoch-ahead",
            Violation::CardEpochStale { .. } => "card-epoch-stale",
            Violation::HostEpochStale { .. } => "host-epoch-stale",
            Violation::FinishedOpsOverflow { .. } => "finished-ops-overflow",
            Violation::ParentCycle { .. } => "parent-cycle",
            Violation::AttentionMismatch { .. } => "attention-mismatch",
            Violation::ClaudeSdkProjection { .. } => "claude-sdk-projection-disagreement",
            Violation::Claude(violation) => violation.kind(),
            Violation::Codex(violation) => violation.kind(),
        }
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::StreamWithoutAgent { agent } => {
                write!(f, "stream for {agent} has no agent card")
            }
            Violation::CardEpochAhead {
                agent,
                card_epoch,
                model_epoch,
            } => write!(
                f,
                "agent {agent} card epoch {card_epoch} is ahead of model epoch {model_epoch}"
            ),
            Violation::HostEpochAhead {
                host,
                host_epoch,
                model_epoch,
            } => write!(
                f,
                "host {host} epoch {host_epoch} is ahead of model epoch {model_epoch}"
            ),
            Violation::CardEpochStale {
                agent,
                card_epoch,
                model_epoch,
            } => write!(
                f,
                "agent {agent} card epoch {card_epoch} survived pruning at model epoch \
                 {model_epoch}"
            ),
            Violation::HostEpochStale {
                host,
                host_epoch,
                model_epoch,
            } => write!(
                f,
                "host {host} epoch {host_epoch} survived pruning at model epoch {model_epoch}"
            ),
            Violation::FinishedOpsOverflow { len, cap } => {
                write!(
                    f,
                    "finished_ops holds {len} entries over the bound of {cap}"
                )
            }
            Violation::ParentCycle { agent } => {
                write!(f, "agent {agent} has a looping parent edge")
            }
            Violation::AttentionMismatch {
                agent,
                card,
                derived,
            } => write!(
                f,
                "agent {agent} attention {card:?} disagrees with its layer's {derived:?}"
            ),
            Violation::ClaudeSdkProjection {
                agent,
                phase,
                attention,
                gate,
            } => write!(
                f,
                "agent {agent} Claude projections disagree: {phase:?}, {attention:?}, {gate:?}"
            ),
            Violation::Claude(violation) => violation.fmt(f),
            Violation::Codex(violation) => violation.fmt(f),
        }
    }
}

impl Model {
    /// Structural coherence of the folded state, checked by the shell at
    /// the fold seam after every Msg (loud and non-fatal by default,
    /// dump-once-per-kind). Distinct from input tripwires, which refuse impossible
    /// *inputs* at the receiving reducer arm — this checks that the fold
    /// itself left the Model coherent.
    ///
    /// Discipline rule: invariants range over the structural index — ids,
    /// counts, phases, epochs — NEVER over content. `check_invariants`
    /// stays O(entities) forever; anything that would re-derive content
    /// (re-running folds over entries, inspecting payloads) belongs in the
    /// spec suite, not here.
    pub fn check_invariants(&self) -> Vec<Violation> {
        let mut violations = Vec::new();
        let synchronized = self.is_synchronized();

        for agent in self.streams.keys() {
            if !self.agents.contains_key(agent) {
                violations.push(Violation::StreamWithoutAgent { agent: *agent });
            }
        }

        for (id, card) in &self.agents {
            if card.epoch > self.epoch {
                violations.push(Violation::CardEpochAhead {
                    agent: *id,
                    card_epoch: card.epoch,
                    model_epoch: self.epoch,
                });
            } else if synchronized && card.epoch != self.epoch && !self.awaits_own_host(card) {
                violations.push(Violation::CardEpochStale {
                    agent: *id,
                    card_epoch: card.epoch,
                    model_epoch: self.epoch,
                });
            }
            if let Some(layer) = &card.layer {
                let stream_phase = self.streams.get(id).map(|stream| &stream.phase);
                let derived_attention = layer.attention(stream_phase);
                if card.attention != derived_attention {
                    violations.push(Violation::AttentionMismatch {
                        agent: *id,
                        card: card.attention,
                        derived: derived_attention,
                    });
                }
                layer.check_invariants(*id, &mut violations);
                match layer {
                    AgentLayer::Claude(_) => {
                        crate::claude::check_projection_invariant(self, *id, &mut violations);
                    }
                    AgentLayer::ClaudeSdk(_) => {
                        crate::claude_sdk::check_projection_invariant(self, *id, &mut violations);
                    }
                    AgentLayer::Codex(_) => {
                        crate::codex::check_projection_invariant(
                            self,
                            *id,
                            card.attention,
                            &mut violations,
                        );
                    }
                }
            }
        }

        // Parent edges must form a forest: every agent reachable from some
        // agent no parent claims. A loop would strand its members outside
        // every family, so it is named here rather than silently flattened.
        let topology = self.topology();
        let mut reachable = std::collections::BTreeSet::new();
        for root in &topology.roots {
            self.descendants(&topology, root.agent.id, 1, &mut reachable);
        }
        for id in self.agents.keys() {
            if !reachable.contains(id) {
                violations.push(Violation::ParentCycle { agent: *id });
            }
        }

        for (id, host) in &self.hosts {
            if host.epoch > self.epoch {
                violations.push(Violation::HostEpochAhead {
                    host: *id,
                    host_epoch: host.epoch,
                    model_epoch: self.epoch,
                });
            } else if synchronized && host.epoch != self.epoch {
                violations.push(Violation::HostEpochStale {
                    host: *id,
                    host_epoch: host.epoch,
                    model_epoch: self.epoch,
                });
            }
        }

        if self.finished_ops.len() > FINISHED_OPS_RETAINED {
            violations.push(Violation::FinishedOpsOverflow {
                len: self.finished_ops.len(),
                cap: FINISHED_OPS_RETAINED,
            });
        }

        violations
    }
}

/// Short human label for a typed agent kind (pending rows; live rows carry
/// the wire's `agent_type` string).
pub fn agent_type_label(agent_type: &model::AgentType) -> &'static str {
    match agent_type {
        model::AgentType::Claude { .. } => "claude",
        model::AgentType::Codex { .. } => "codex",
        #[allow(unreachable_patterns)]
        _ => "test-agent",
    }
}

/// Compact relative age ("12s", "2m", "3h", "2d") for fleet rows. Pure:
/// `now` comes from the renderer's `FrameContext`.
pub fn format_relative_age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let seconds = (now - then).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// `check_invariants` is proven two ways: the wire_free differential spec
/// asserts no public fold sequence ever violates it, and these tests prove
/// each invariant class actually FIRES — a coherent Model is built through
/// public folds, then one field is corrupted directly (private access) per
/// class.
#[cfg(test)]
mod tests {
    use model::{Agent, Capabilities, HostEntry, HostTrustStatus};
    use uuid::Uuid;

    use super::*;
    use crate::msg::{Msg, OpOutcome, ServerMsg, StreamMsg};
    use crate::update::update;

    fn host_id() -> HostId {
        Uuid::from_u128(1)
    }

    fn agent_id() -> AgentId {
        Uuid::from_u128(2)
    }

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture epoch")
    }

    fn a_host() -> HostEntry {
        HostEntry {
            id: host_id(),
            name: "nova".to_string(),
            online: true,
            version: Some("0.4.0".to_string()),
            capabilities: Some(Capabilities::default()),
            trust_status: HostTrustStatus::Trusted,
            last_dial_error: None,
            platform: None,
        }
    }

    fn an_agent() -> Agent {
        Agent {
            id: agent_id(),
            host_id: host_id(),
            name: Some("fix-auth-bug".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            kind: model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: t0(),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        }
    }

    /// A synchronized Model — one host, one local agent with a folded
    /// claude layer — built exclusively through public folds, and coherent
    /// by construction.
    fn coherent_model() -> Model {
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host_id()),
            }),
            Msg::Server(ServerMsg::HostUpserted { host: a_host() }),
            Msg::Server(ServerMsg::AgentUpserted { agent: an_agent() }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
        ] {
            update(&mut model, msg);
        }
        let violations = model.check_invariants();
        assert!(
            violations.is_empty(),
            "fixture must start coherent: {violations:?}"
        );
        assert!(
            model.agents[&agent_id()].layer.is_some(),
            "fixture must carry a claude layer for the attention class"
        );
        model
    }

    #[test]
    fn detects_stream_without_agent() {
        let mut model = coherent_model();
        model.agents.clear();
        assert!(
            model
                .check_invariants()
                .iter()
                .any(|violation| matches!(violation, Violation::StreamWithoutAgent { .. })),
            "clearing agents must orphan the stream key"
        );
    }

    #[test]
    fn detects_epochs_ahead_of_the_model() {
        let mut model = coherent_model();
        model.agents.get_mut(&agent_id()).unwrap().epoch = model.epoch + 1;
        model.hosts.get_mut(&host_id()).unwrap().epoch = model.epoch + 1;
        let violations = model.check_invariants();
        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, Violation::CardEpochAhead { .. })),
            "card epoch past the model must be reported: {violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, Violation::HostEpochAhead { .. })),
            "host epoch past the model must be reported: {violations:?}"
        );
    }

    #[test]
    fn detects_stale_epochs_while_synchronized() {
        let mut model = coherent_model();
        model.agents.get_mut(&agent_id()).unwrap().epoch = model.epoch - 1;
        model.hosts.get_mut(&host_id()).unwrap().epoch = model.epoch - 1;
        let violations = model.check_invariants();
        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, Violation::CardEpochStale { .. })),
            "stale card epoch under a synchronized model must be reported: {violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, Violation::HostEpochStale { .. })),
            "stale host epoch under a synchronized model must be reported: {violations:?}"
        );
    }

    #[test]
    fn detects_finished_ops_over_retention() {
        let mut model = coherent_model();
        for seq in 0..=FINISHED_OPS_RETAINED as u64 {
            model.finished_ops.push(FinishedOp {
                op: crate::msg::OpId(Uuid::from_u128(u128::from(seq) + 100)),
                seq,
                command: crate::msg::Command::DeleteAgent { agent: agent_id() },
                outcome: OpOutcome::AgentDeleted,
            });
        }
        assert!(
            model
                .check_invariants()
                .iter()
                .any(|violation| matches!(violation, Violation::FinishedOpsOverflow { .. })),
            "finished_ops past the retention bound must be reported"
        );
    }

    #[test]
    fn detects_attention_disagreeing_with_the_layer() {
        let mut model = coherent_model();
        model.agents.get_mut(&agent_id()).unwrap().attention = Attention::Working;
        assert!(
            model
                .check_invariants()
                .iter()
                .any(|violation| matches!(violation, Violation::AttentionMismatch { .. })),
            "a cached attention that disagrees with its layer's derivation must be reported"
        );
    }

    #[test]
    fn detects_codex_projection_disagreement() {
        let mut model = coherent_model();
        let card = model.agents.get_mut(&agent_id()).unwrap();
        card.agent.kind = model::AgentKind::Codex;
        card.layer = Some(AgentLayer::Codex(CodexLayer::default()));
        for event in [
            StreamMsg::Opened { truncated: false },
            StreamMsg::Batch {
                at: t0(),
                entries: vec![crate::msg::StreamEntry::observed(
                    1,
                    t0(),
                    serde_json::json!({"type":"amux.codex_ready"}),
                )],
            },
            StreamMsg::ReplayComplete,
        ] {
            update(
                &mut model,
                Msg::Stream {
                    agent: agent_id(),
                    event,
                },
            );
        }
        assert!(model.check_invariants().is_empty(), "fixture is coherent");

        // Corrupt only the kernel exit fact: layer attention remains its
        // coherent Idle projection, but the write gate now refuses Exited.
        model.agents.get_mut(&agent_id()).unwrap().phase =
            AgentPhase::Exited { exit_code: Some(1) };
        assert!(
            model.check_invariants().iter().any(|violation| matches!(
                violation,
                Violation::Codex(CodexViolation::ProjectionDisagreement { .. })
            )),
            "an Idle attention beside an Exited gate must be reported"
        );
    }
}
