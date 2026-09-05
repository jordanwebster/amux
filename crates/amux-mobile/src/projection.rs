//! Owned JSON DTOs over the shared reducer's read surface.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use amux::RelayConnection;
use amux_ui::{
    Agent, AgentId, AgentPhase, Attention, Command, HostState, Model, OpId, OpOutcome, StreamPhase,
    StructuredProtocol, Why, claude, codex, diff,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCardDto {
    pub agent: Agent,
    pub display_name: String,
    pub attention: Attention,
    pub phase: AgentPhase,
    pub last_activity: DateTime<Utc>,
}

/// Native row vocabularies stay distinct even where their fields coincide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "row", rename_all = "snake_case")]
pub enum FeedEntryDto {
    ClaudePty(claude::FeedEntry),
    ClaudeSdk(ClaudeSdkEntryDto),
    Codex(codex::FeedEntry),
}

impl FeedEntryDto {
    fn seq(&self) -> u64 {
        match self {
            Self::ClaudePty(row) => row.seq,
            Self::Codex(row) => row.seq,
            Self::ClaudeSdk(row) => match *row {},
        }
    }
}

/// This checkout's SDK layer folds no rows. Keep its type separate until the
/// shared SDK reducer supplies its vocabulary; a PTY row is never an SDK row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClaudeSdkEntryDto {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum GateDto {
    ClaudePty(claude::SendGate),
    Codex(codex::SendGate),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum PhaseDto {
    ClaudePty(claude::ChatPhase),
    Codex(codex::CodexPhase),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum AskDto {
    ClaudePty(claude::Ask),
    Codex(codex::Ask),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", rename_all = "snake_case")]
pub enum FactsDto {
    ClaudePty {
        session: claude::SessionFacts,
        accepted_plans: Vec<claude::AcceptedPlan>,
        echoes: Vec<claude::PromptEcho>,
    },
    Codex {
        active_turn_id: Option<String>,
    },
    ClaudeSdk {
        supported: bool,
    },
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FamilyMemberDto {
    pub agent: AgentId,
    pub depth: usize,
    pub needs: Option<Why>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDto {
    pub agent: AgentId,
    pub gate: GateDto,
    pub phase: PhaseDto,
    pub stream: Option<StreamPhase>,
    pub asks: Vec<AskDto>,
    pub facts: FactsDto,
    pub provider: Box<amux_ui::ProviderFacts>,
    pub settings_gate: amux_ui::provider::SettingsGate,
    pub queue: Option<Box<amux_ui::QueuedMessage>>,
    pub family: Vec<FamilyMemberDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDto {
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpOutcomeDto {
    Shared(Box<OpOutcome>),
    Subscription(SubscriptionOutcome),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubscriptionOutcome {
    Subscribed { agent: AgentId },
    Unsubscribed { agent: AgentId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Fleet {
        epoch: u64,
        agents: Vec<AgentCardDto>,
        hosts: Vec<HostState>,
        reconciled: bool,
    },
    Feed {
        agent: AgentId,
        /// Absolute position of the first appended row, independent of native IDs.
        base: u64,
        append: Vec<FeedEntryDto>,
        replace: Vec<(u64, FeedEntryDto)>,
        /// Remove all positions below this absolute prefix before applying ranges.
        evicted: u64,
    },
    Session(SessionDto),
    OpResult {
        op: OpId,
        outcome: OpOutcomeDto,
    },
    Diff {
        agent: AgentId,
        document: diff::Document,
    },
    Connection {
        state: ConnectionDto,
        reason: Option<String>,
    },
    TokenRequest {
        request_id: u64,
    },
    Invariant {
        detail: String,
    },
}

impl Event {
    pub fn connection(connection: &RelayConnection) -> Self {
        let (state, reason) = match connection {
            RelayConnection::Connecting => (ConnectionDto::Connecting, None),
            RelayConnection::Connected => (ConnectionDto::Connected, None),
            RelayConnection::Disconnected { reason } => {
                (ConnectionDto::Disconnected, Some(reason.clone()))
            }
        };
        Self::Connection { state, reason }
    }
}

#[derive(Default)]
struct FeedState {
    window: Option<(StructuredProtocol, Option<String>)>,
    offset: u64,
    next: u64,
    evicted: u64,
    rows: BTreeMap<u64, FeedEntryDto>,
}

// Compare borrowed native rows; clone only rows that will cross the callback.
enum RowRef<'a> {
    Claude(&'a claude::FeedEntry),
    Codex(&'a codex::FeedEntry),
}
impl RowRef<'_> {
    fn id(&self) -> u64 {
        match self {
            Self::Claude(row) => row.id,
            Self::Codex(row) => row.id,
        }
    }
    fn seq(&self) -> u64 {
        match self {
            Self::Claude(row) => row.seq,
            Self::Codex(row) => row.seq,
        }
    }
    fn same(&self, previous: &FeedEntryDto) -> bool {
        match (self, previous) {
            (Self::Claude(row), FeedEntryDto::ClaudePty(old)) => *row == old,
            (Self::Codex(row), FeedEntryDto::Codex(old)) => *row == old,
            _ => false,
        }
    }
    fn owned(&self) -> FeedEntryDto {
        match self {
            Self::Claude(row) => FeedEntryDto::ClaudePty((*row).clone()),
            Self::Codex(row) => FeedEntryDto::Codex((*row).clone()),
        }
    }
}

impl FeedState {
    fn project<'a>(
        &mut self,
        agent: AgentId,
        window: (StructuredProtocol, Option<String>),
        source_evicted: u64,
        rows: impl Iterator<Item = RowRef<'a>>,
    ) -> Option<Event> {
        let rows: Vec<_> = rows.collect();
        let source_next = source_evicted + rows.len() as u64;
        let reused = rows.iter().any(|row| {
            self.rows
                .get(&(self.offset + row.id()))
                .is_some_and(|old| row.seq() != old.seq())
        });
        // A fold is independent of how often it was replayed. Detect changed
        // source identity, a rewound retained window, or reused native IDs from
        // the existing facts, without putting renderer history in the reducer.
        if self.window.as_ref() != Some(&window)
            || self.offset + source_next < self.next
            || self.offset + source_evicted < self.evicted
            || reused
        {
            self.window = Some(window);
            self.offset = self.next;
            self.rows.clear();
        }
        let evicted = self.offset + source_evicted;
        self.rows.retain(|id, _| *id >= evicted);
        let base = self.next.max(evicted);
        let mut append = Vec::new();
        let mut replace = Vec::new();
        for row in rows {
            let index = self.offset + row.id();
            if self.rows.get(&index).is_some_and(|old| row.same(old)) {
                continue;
            }
            let dto = row.owned();
            self.rows.insert(index, dto.clone());
            if index >= base {
                debug_assert_eq!(index, base + append.len() as u64);
                append.push(dto);
            } else {
                replace.push((index, dto));
            }
        }
        self.next = base + append.len() as u64;
        let changed = !append.is_empty() || !replace.is_empty() || evicted != self.evicted;
        self.evicted = evicted;
        changed.then_some(Event::Feed {
            agent,
            base,
            append,
            replace,
            evicted,
        })
    }
}

#[derive(Default)]
pub struct Projection {
    fleet: Option<Event>,
    synchronized: bool,
    remote_inventories: BTreeMap<amux::HostId, BTreeSet<AgentId>>,
    feeds: BTreeMap<AgentId, FeedState>,
    sessions: BTreeMap<AgentId, SessionDto>,
    finished: BTreeSet<OpId>,
    subscribed: BTreeSet<AgentId>,
}

impl Projection {
    pub fn subscribe(&mut self, agent: AgentId) {
        self.subscribed.insert(agent);
    }

    pub fn unsubscribe(&mut self, agent: AgentId) {
        self.subscribed.remove(&agent);
        self.feeds.remove(&agent);
        self.sessions.remove(&agent);
    }

    /// Called after every folded input, before the reducer can evict an outcome.
    pub fn outcomes(&mut self, model: &Model, events: &mut Vec<Event>) {
        self.finished.retain(|op| model.finished_op(*op).is_some());
        for result in model.finished_ops() {
            if !self.finished.insert(result.op) {
                continue;
            }
            events.push(Event::OpResult {
                op: result.op,
                outcome: OpOutcomeDto::Shared(Box::new(result.outcome.clone())),
            });
            let (agent, patch) = match (&result.command, &result.outcome) {
                (Command::RequestDiff { agent, .. }, OpOutcome::DiffReady { response }) => {
                    (*agent, response.patch.as_str())
                }
                (Command::FetchDiff { agent, .. }, OpOutcome::DiffFetched { patch, .. }) => {
                    (*agent, patch.as_str())
                }
                _ => continue,
            };
            events.push(Event::Diff {
                agent,
                document: diff::parse_unified_patch(patch, false),
            });
        }
    }

    pub fn collect(
        &mut self,
        model: &Model,
        connection: &RelayConnection,
        events: &mut Vec<Event>,
    ) {
        let fleet = Event::Fleet {
            epoch: model.epoch(),
            agents: model
                .agents()
                .map(|card| AgentCardDto {
                    agent: card.agent.clone(),
                    display_name: card.display_name(),
                    attention: model.effective_attention(card),
                    phase: card.phase.clone(),
                    last_activity: card.last_activity,
                })
                .collect(),
            hosts: model
                .hosts()
                .filter(|host| host.entry.trust_status == amux::HostTrustStatus::Trusted)
                .cloned()
                .collect(),
            reconciled: model.is_synchronized() && *connection == RelayConnection::Connected,
        };
        // Cache membership also depends on local synchronization, even while a
        // disconnected relay keeps the displayed Fleet unreconciled.
        if self.fleet.as_ref() != Some(&fleet)
            || self.synchronized != model.is_synchronized()
            || &self.remote_inventories != model.remote_inventories()
        {
            self.synchronized = model.is_synchronized();
            self.remote_inventories = model.remote_inventories().clone();
            self.fleet = Some(fleet.clone());
            events.push(fleet);
        }
        for agent in &self.subscribed {
            let session = session(model, *agent);
            if self.sessions.get(agent) != Some(&session) {
                self.sessions.insert(*agent, session.clone());
                events.push(Event::Session(session));
            }
            let state = self.feeds.entry(*agent).or_default();
            let feed = if let Some(layer) = model.claude(*agent) {
                state.project(
                    *agent,
                    (
                        StructuredProtocol::Claude,
                        layer.session_id().map(str::to_owned),
                    ),
                    layer.evicted_entries(),
                    layer.entries().map(RowRef::Claude),
                )
            } else if let Some(layer) = model.codex(*agent) {
                state.project(
                    *agent,
                    (StructuredProtocol::Codex, None),
                    layer.evicted_entries(),
                    layer.entries().map(RowRef::Codex),
                )
            } else {
                // Removal or a switch to an unsupported layer evicts readable rows.
                state.project(
                    *agent,
                    (StructuredProtocol::ClaudeSdk, None),
                    0,
                    std::iter::empty(),
                )
            };
            if let Some(feed) = feed {
                events.push(feed);
            }
        }
    }
}

fn session(model: &Model, agent: AgentId) -> SessionDto {
    let protocol = model
        .agent(agent)
        .and_then(|card| card.structured_protocol());
    let (gate, phase, asks, facts) = match protocol {
        Some(StructuredProtocol::Claude) => (
            GateDto::ClaudePty(claude::send_gate(model, agent)),
            PhaseDto::ClaudePty(claude::phase(model, agent)),
            model
                .claude(agent)
                .map(|l| l.asks().cloned().map(AskDto::ClaudePty).collect())
                .unwrap_or_default(),
            FactsDto::ClaudePty {
                session: model
                    .claude(agent)
                    .map(|l| l.session().clone())
                    .unwrap_or_default(),
                accepted_plans: model
                    .claude(agent)
                    .map(|l| l.accepted_plans().to_vec())
                    .unwrap_or_default(),
                echoes: model
                    .claude(agent)
                    .map(|l| l.pending_echoes().to_vec())
                    .unwrap_or_default(),
            },
        ),
        Some(StructuredProtocol::Codex) => (
            GateDto::Codex(codex::send_gate(model, agent)),
            PhaseDto::Codex(codex::phase(model, agent)),
            model
                .codex(agent)
                .map(|l| l.asks().cloned().map(AskDto::Codex).collect())
                .unwrap_or_default(),
            FactsDto::Codex {
                active_turn_id: model
                    .codex(agent)
                    .and_then(|l| l.active_turn_id().map(str::to_owned)),
            },
        ),
        Some(StructuredProtocol::ClaudeSdk) => (
            GateDto::Unavailable,
            PhaseDto::Unavailable,
            vec![],
            FactsDto::ClaudeSdk { supported: false },
        ),
        None => (
            GateDto::Unavailable,
            PhaseDto::Unavailable,
            vec![],
            FactsDto::Unavailable,
        ),
    };
    let needs = model.family_needs(agent);
    SessionDto {
        agent,
        gate,
        phase,
        stream: model.stream(agent).map(|s| s.phase.clone()),
        asks,
        facts,
        provider: Box::new(amux_ui::provider::facts(model, agent)),
        settings_gate: amux_ui::provider::settings_gate(model, agent),
        queue: model.queued(agent).cloned().map(Box::new),
        family: model
            .family_of(agent)
            .into_iter()
            .map(|member| FamilyMemberDto {
                agent: member.card.agent.id,
                depth: member.depth,
                needs: needs
                    .iter()
                    .find(|n| n.agent() == member.card.agent.id)
                    .map(|n| n.why),
            })
            .collect(),
    }
}

/// Deadlines follow actual emission time, avoiding timer catch-up bursts after
/// a blocked callback. An idle worker has no periodic wakeup.
pub struct Cadence {
    interval: Duration,
    last: Option<Instant>,
}
impl Cadence {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }
    pub fn deadline(&self) -> Instant {
        self.last
            .map(|last| last + self.interval)
            .unwrap_or_else(Instant::now)
    }
    pub fn emitted(&mut self) {
        self.last = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests;
