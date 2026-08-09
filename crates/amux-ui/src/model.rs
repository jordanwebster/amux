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

use crate::msg::{Command, DisconnectReason, OpId, OpOutcome, StreamCloseReason};
use crate::summarizers::SummarizerState;

/// How many finished ops the Model retains (retention is explicitly bounded;
/// old outcomes age out, pending obligations never do — they live in
/// `pending_ops` until resolved).
pub(crate) const FINISHED_OPS_RETAINED: usize = 64;

/// Kernel attention vocabulary: "does this agent need you". Derived by
/// per-agent summarizer folds at observation time; unsubscribed or
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
    /// Typed per-agent fold state deriving `attention`; `None` for agent
    /// types without a summarizer (their attention stays `Unknown`).
    pub(crate) summarizer: Option<SummarizerState>,
    /// Epoch of the last upsert; entities from older epochs are pruned when
    /// a reconnect snapshot completes.
    pub(crate) epoch: u64,
}

impl AgentCard {
    /// Display name fallback: user-assigned name, then provider label, then
    /// short id.
    pub fn display_name(&self) -> String {
        display_name_fallback(
            self.agent.name.as_deref(),
            self.provider_label.as_deref(),
            &self.agent.id,
        )
    }

    /// The fleet status word. Attention takes precedence; phase shows when
    /// not running.
    pub fn status_label(&self) -> String {
        match (&self.attention, &self.phase) {
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
            (Attention::Idle, _) => "idle".to_string(),
            (Attention::Unknown, _) => "–".to_string(),
        }
    }

    fn attention_rank(&self) -> u8 {
        match self.attention {
            Attention::NeedsYou {
                why: Why::Permission,
            } => 0,
            Attention::NeedsYou { why: Why::Question } => 1,
            Attention::NeedsYou { why: Why::Finished } => 2,
            _ => 3,
        }
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

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn stream(&self, id: AgentId) -> Option<&StreamState> {
        self.streams.get(&id)
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

    /// The fleet: ONE flat list, globally ranked. `NeedsYou` first
    /// (permission, question, finished), then recency; host is a column, not
    /// a grouping. Pending creates render as optimistic rows at the bottom
    /// in dispatch order.
    pub fn fleet(&self) -> Vec<FleetItem<'_>> {
        let mut cards: Vec<&AgentCard> = self.agents.values().collect();
        cards.sort_by(|a, b| {
            a.attention_rank()
                .cmp(&b.attention_rank())
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

/// Short human label for a typed agent kind (pending rows; live rows carry
/// the wire's `agent_type` string).
pub fn agent_type_label(agent_type: &amux::AgentType) -> &'static str {
    match agent_type {
        amux::AgentType::Claude => "claude",
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
