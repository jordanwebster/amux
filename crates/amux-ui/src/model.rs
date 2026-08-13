//! The Model: everything a renderer may read, all derivations included.
//!
//! Views format, never decide (`docs/UI.md`): fleet ordering, display-name
//! fallback, status labels are computed here, once. Reducer-visible
//! collections are `BTreeMap`s so iteration order is canonical.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

use std::collections::BTreeMap;

use amux::{Agent, AgentId, HostEntry, HostId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudeLayer, ClaudeViolation};
use crate::msg::{Command, DisconnectReason, OpId, OpOutcome, StreamCloseReason};

/// How many finished ops the Model retains (retention is explicitly bounded;
/// old outcomes age out, pending obligations never do — they live in
/// `pending_ops` until resolved).
pub(crate) const FINISHED_OPS_RETAINED: usize = 64;

/// Kernel attention vocabulary: "does this agent need you". Derived from
/// the per-agent layer fold at observation time (E2); unsubscribed or
/// truncated-history agents stay `Unknown` — degradation is always to
/// `Unknown`, never to a wrong badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attention", rename_all = "snake_case")]
pub enum Attention {
    Unknown,
    Idle,
    Working,
    NeedsYou { why: Why },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Permission,
    Question,
    Finished,
}

/// Lifecycle phase observed from session-stream facts. Inventory removal is
/// the authority for "gone"; this only records what a stream close reported
/// while the agent is still listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum AgentPhase {
    Running,
    Exited { exit_code: Option<i32> },
}

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
}

impl AgentLayer {
    /// Select a layer only from protocols the agent advertised.
    pub(crate) fn from_protocols(protocols: &[String]) -> Option<Self> {
        protocols
            .iter()
            .any(|protocol| protocol == crate::claude::PROTOCOL)
            .then(|| Self::Claude(ClaudeLayer::default()))
    }

    pub(crate) fn protocol(&self) -> &'static str {
        match self {
            Self::Claude(_) => crate::claude::PROTOCOL,
        }
    }

    pub fn claude(&self) -> Option<&ClaudeLayer> {
        match self {
            Self::Claude(layer) => Some(layer),
        }
    }

    pub(crate) fn claude_mut(&mut self) -> Option<&mut ClaudeLayer> {
        match self {
            Self::Claude(layer) => Some(layer),
        }
    }

    pub(crate) fn begin_window(&mut self, truncated: bool) {
        match self {
            Self::Claude(layer) => layer.begin_window(truncated),
        }
    }

    pub(crate) fn observe(&mut self, seq: u64, at: DateTime<Utc>, payload: &serde_json::Value) {
        match self {
            Self::Claude(layer) => layer.observe(seq, at, payload),
        }
    }

    pub(crate) fn observe_replay_complete(&mut self) {
        match self {
            Self::Claude(layer) => layer.observe_replay_complete(),
        }
    }

    pub(crate) fn observe_exit(&mut self) {
        match self {
            Self::Claude(layer) => layer.observe_exit(),
        }
    }

    pub(crate) fn invalidate(&mut self) {
        match self {
            Self::Claude(layer) => layer.invalidate(),
        }
    }

    pub(crate) fn attention(&self) -> Attention {
        match self {
            Self::Claude(layer) => layer.attention(),
        }
    }

    pub(crate) fn working_is_stale(&self, now: Option<DateTime<Utc>>) -> bool {
        match self {
            Self::Claude(layer) => layer.working_is_stale(now),
        }
    }

    pub(crate) fn check_invariants(&self, agent: AgentId, out: &mut Vec<Violation>) {
        match self {
            Self::Claude(layer) => layer.check_invariants(agent, out),
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
    /// Typed native layer state. `None` until the structured stream
    /// produces evidence; unsupported agents honestly stay `Unknown`.
    pub(crate) layer: Option<AgentLayer>,
    /// Epoch of the last upsert; entities from older epochs are pruned when
    /// a reconnect snapshot completes.
    pub(crate) epoch: u64,
}

impl AgentCard {
    /// The Claude chat layer's feed facts, when the agent's structured
    /// stream has produced any.
    pub fn claude(&self) -> Option<&ClaudeLayer> {
        self.layer.as_ref().and_then(AgentLayer::claude)
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
    pub(crate) fn status_label(&self, attention: Attention) -> String {
        match (&attention, &self.phase) {
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

/// One row of the ranked fleet.
#[derive(Clone, Debug, PartialEq)]
pub enum FleetItem<'a> {
    Agent(&'a AgentCard),
    /// An optimistic row for an in-flight create.
    PendingCreate {
        op: OpId,
        name: &'a str,
        agent_type: &'a amux::AgentType,
        host: Option<HostId>,
    },
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
    pub(crate) hosts: BTreeMap<HostId, HostState>,
    pub(crate) agents: BTreeMap<AgentId, AgentCard>,
    pub(crate) streams: BTreeMap<AgentId, StreamState>,
    pub(crate) pending_ops: BTreeMap<OpId, PendingOp>,
    pub(crate) finished_ops: Vec<FinishedOp>,
    pub(crate) op_seq: u64,
    /// Last observed time (enters via `Msg::Tick`).
    pub(crate) now: Option<DateTime<Utc>>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            connection: Connection::Connecting,
            epoch: 0,
            local_host_id: None,
            cloud_auth_required: false,
            hosts: BTreeMap::new(),
            agents: BTreeMap::new(),
            streams: BTreeMap::new(),
            pending_ops: BTreeMap::new(),
            finished_ops: Vec::new(),
            op_seq: 0,
            now: None,
        }
    }
}

impl Model {
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

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn cloud_auth_required(&self) -> bool {
        self.cloud_auth_required
    }

    pub fn local_host_id(&self) -> Option<HostId> {
        self.local_host_id
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

    pub fn stream(&self, id: AgentId) -> Option<&StreamState> {
        self.streams.get(&id)
    }

    /// The Claude chat layer for an agent (the chat view's read surface).
    pub fn claude(&self, id: AgentId) -> Option<&ClaudeLayer> {
        self.agents.get(&id).and_then(AgentCard::claude)
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
        if card.attention == Attention::Working
            && card
                .layer
                .as_ref()
                .is_some_and(|layer| layer.working_is_stale(self.now))
        {
            return Attention::Unknown;
        }
        card.attention
    }

    /// The fleet status word with read-time policy applied: offline rows
    /// show `–`, and the word derives from the SAME effective attention as
    /// the badge — one derivation, so a staleness-degraded Unknown badge
    /// can never sit beside a stale "working" label (views format, never
    /// decide).
    pub fn status_label_for(&self, card: &AgentCard) -> String {
        if !self.host_online(card.agent.host_id) {
            return "–".to_string();
        }
        card.status_label(self.effective_attention(card))
    }

    /// The fleet: ONE flat list, globally ranked. `NeedsYou` first
    /// (permission, question, finished), then recency; host is a column, not
    /// a grouping. Pending creates render as optimistic rows at the bottom
    /// in dispatch order.
    /// Readonly agents (externally captured sessions the chrome cannot
    /// drive) surface like any other row now that the chat renders them
    /// (A3: they open in chat only — the entry keys enforce it); their
    /// resting status word is `read-only`.
    pub fn fleet(&self) -> Vec<FleetItem<'_>> {
        let mut cards: Vec<&AgentCard> = self.agents.values().collect();
        cards.sort_by(|a, b| {
            attention_rank(self.effective_attention(a))
                .cmp(&attention_rank(self.effective_attention(b)))
                .then(b.last_activity.cmp(&a.last_activity))
                .then(a.agent.id.cmp(&b.agent.id))
        });
        let mut items: Vec<FleetItem<'_>> = cards.into_iter().map(FleetItem::Agent).collect();

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

/// A broken Model invariant, typed and entity-addressed so a release-mode
/// dump names the exact entity that went incoherent.
#[derive(Clone, Debug, PartialEq)]
pub enum Violation {
    /// A `streams` key with no corresponding agent card.
    StreamWithoutAgent { agent: AgentId },
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
    FinishedOpsOverflow { len: usize, cap: usize },
    /// A card's cached attention disagrees with the one derived from its
    /// own layer fold (E2: attention IS a projection of the layer).
    AttentionMismatch {
        agent: AgentId,
        card: Attention,
        derived: Attention,
    },
    /// A typed layer's own structural invariant failed.
    Claude(ClaudeViolation),
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
            Violation::AttentionMismatch { .. } => "attention-mismatch",
            Violation::Claude(violation) => violation.kind(),
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
            Violation::AttentionMismatch {
                agent,
                card,
                derived,
            } => write!(
                f,
                "agent {agent} attention {card:?} disagrees with its layer's {derived:?}"
            ),
            Violation::Claude(violation) => violation.fmt(f),
        }
    }
}

impl Model {
    /// Structural coherence of the folded state, checked by the shell at
    /// the fold seam after every Msg (panic in debug, dump-once-per-kind in
    /// release). Distinct from input tripwires, which refuse impossible
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
            } else if synchronized && card.epoch != self.epoch {
                violations.push(Violation::CardEpochStale {
                    agent: *id,
                    card_epoch: card.epoch,
                    model_epoch: self.epoch,
                });
            }
            if let Some(layer) = &card.layer {
                if card.attention != layer.attention() {
                    violations.push(Violation::AttentionMismatch {
                        agent: *id,
                        card: card.attention,
                        derived: layer.attention(),
                    });
                }
                layer.check_invariants(*id, &mut violations);
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
pub fn agent_type_label(agent_type: &amux::AgentType) -> &'static str {
    match agent_type {
        amux::AgentType::Claude => "claude",
        amux::AgentType::Codex { .. } => "codex",
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
    use amux::{Agent, Capabilities, HostEntry, HostTrustStatus};
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
        }
    }

    fn an_agent() -> Agent {
        Agent {
            id: agent_id(),
            host_id: host_id(),
            name: Some("fix-auth-bug".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                "terminal_v1".to_string(),
                crate::claude::PROTOCOL.to_string(),
            ],
            readonly: false,
            args: Vec::new(),
            created_at: t0(),
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
}
