//! Owned JSON DTOs over the shared reducer's read surface.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use model::{DisconnectReason, RelayConnection};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use ui_state::{
    Agent, AgentId, AgentPhase, Attention, Command, HostState, Model, OpId, OpOutcome, StreamPhase,
    StructuredProtocol, Why, claude, claude_sdk, codex, review,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCardDto {
    pub agent: Agent,
    pub display_name: String,
    pub attention: Attention,
    pub phase: AgentPhase,
    pub last_activity: DateTime<Utc>,
    /// What the agent's last finished turn changed. A finished turn is
    /// reported to a reader in words rather than as a tick, and the words are
    /// only worth reading if they carry the numbers.
    ///
    /// The shared inventory does not yet report aggregate changed-file
    /// counts. Absent means "not known", never "no changes", so a reader
    /// states the outcome without arithmetic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TurnOutcome>,
    /// Remembered from the last run and not yet confirmed by the machine that
    /// owns it.
    ///
    /// The fleet as a whole is reconciled only once every host has answered,
    /// but a host answers on its own schedule, so a card is confirmed the
    /// moment its own machine has been heard from. A reader can then stop
    /// treating that one row as a memory without waiting for the slowest
    /// machine on the account.
    #[serde(default, skip_serializing_if = "is_false")]
    pub awaiting: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// What one finished turn changed, as the provider counted it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub files: u32,
    pub insertions: u64,
    pub deletions: u64,
    /// Anything else the provider said about the turn in one short phrase,
    /// such as "3 tests added".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Native row vocabularies stay distinct even where their fields coincide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "row", rename_all = "snake_case")]
pub enum FeedEntryDto {
    ClaudePty(claude::FeedEntry),
    ClaudeSdk(claude_sdk::FeedEntry),
    Codex(codex::FeedEntry),
    /// A place where a stored conversation's history is not continuous. It
    /// belongs to the store rather than to any provider, so it keeps a
    /// vocabulary of its own.
    History(HistoryRow),
}

impl FeedEntryDto {
    fn seq(&self) -> u64 {
        match self {
            Self::ClaudePty(row) => row.seq,
            Self::Codex(row) => row.seq,
            Self::ClaudeSdk(row) => row.seq,
            Self::History(row) => row.seq,
        }
    }
}

/// A break in a stored conversation, drawn between the rows it separates so a
/// reader never takes the rows on either side for one continuous exchange.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryRow {
    pub id: u64,
    /// Always 0: a break is not folded from any stream row.
    pub seq: u64,
    pub boundary: HistoryBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryBoundary {
    /// Rows this device never received, because the machine no longer held
    /// them when it reconnected or the history was cut short.
    Missing,
    /// The rows after this were written in a different entry version.
    VersionChanged,
    /// Older rows were evicted from this device's store.
    Evicted,
}

impl From<ui_state::Boundary> for HistoryBoundary {
    fn from(boundary: ui_state::Boundary) -> Self {
        match boundary {
            ui_state::Boundary::Truncated | ui_state::Boundary::Gap => Self::Missing,
            ui_state::Boundary::VersionGap => Self::VersionChanged,
            ui_state::Boundary::Evicted => Self::Evicted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum GateDto {
    ClaudePty(claude::SendGate),
    ClaudeSdk(claude_sdk::SendGate),
    Codex(codex::SendGate),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum PhaseDto {
    ClaudePty(claude::ChatPhase),
    ClaudeSdk(claude_sdk::SdkPhase),
    Codex(codex::CodexPhase),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "value", rename_all = "snake_case")]
pub enum AskDto {
    ClaudePty(claude::Ask),
    ClaudeSdk(claude_sdk::Ask),
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
        session: claude_sdk::SessionFacts,
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
    pub provider: Box<ui_state::ProviderFacts>,
    pub settings_gate: ui_state::provider::SettingsGate,
    pub queue: Option<Box<ui_state::QueuedMessage>>,
    pub family: Vec<FamilyMemberDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDto {
    Connecting,
    Connected,
    Disconnected,
}

/// Why the phone is offline. The screen owns the words; this owns the kinds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineReasonDto {
    Unreachable,
    Rejected,
    TimedOut,
    Ended,
    Stopped,
    Suspended,
}

impl From<DisconnectReason> for OfflineReasonDto {
    fn from(reason: DisconnectReason) -> Self {
        match reason {
            DisconnectReason::Unreachable => Self::Unreachable,
            DisconnectReason::Rejected => Self::Rejected,
            DisconnectReason::TimedOut => Self::TimedOut,
            DisconnectReason::Ended => Self::Ended,
            DisconnectReason::Stopped => Self::Stopped,
            DisconnectReason::Suspended => Self::Suspended,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpOutcomeDto {
    Shared(Box<OpOutcome>),
    Subscription(SubscriptionOutcome),
    Pairing(PairingOutcome),
    Connection(ConnectionOutcome),
    Devices(DevicesOutcome),
    Creation(CreationOutcome),
    Accounts(AccountsOutcome),
}

/// How putting another account on screen ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AccountsOutcome {
    /// The account named is now the one being read. What was on screen is
    /// gone rather than hidden: the screens start from nothing, and every
    /// result the previous account still had in flight is refused.
    Selected { account: String },
    /// No account of that name is signed in on this phone — a stale switcher,
    /// or an account signed out from somewhere else on the screen.
    Unknown { account: String },
}

/// How asking a machine what it has to offer ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CreationOutcome {
    /// What the machine says it has: what it was used in recently, the
    /// repositories under its roots, and the roots themselves. Kept apart
    /// because a directory somebody worked in yesterday is a different kind of
    /// suggestion from one that merely exists.
    Repositories {
        host: model::HostId,
        recent: Vec<ProjectDto>,
        repositories: Vec<ProjectDto>,
        roots: Vec<String>,
    },
    /// The machine could not be asked, or would not answer. It carries no
    /// detail: a screen that cannot list directories offers a typed path, and
    /// which RPC failed does not change that.
    RepositoriesUnavailable { host: model::HostId },
}

/// One directory a new agent could be started in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectDto {
    pub path: String,
    pub name: String,
    /// When an agent last ran here, or nothing where none has. It is what
    /// makes a directory "recent" rather than merely present.
    pub last_used: Option<DateTime<Utc>>,
}

/// How withdrawing trust from a machine ended.
///
/// Revoking is not a request the machine can decline: this device stops
/// trusting its key and closes every link it holds to it, with the reason on
/// the close so the machine knows why. What ends is what this phone could
/// reach; the machine's own record of this device is the machine's to remove,
/// and a machine that is away is revoked here anyway.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DevicesOutcome {
    Revoked {
        host: model::HostId,
        name: String,
    },
    /// The machine is not one this device trusts, so there was nothing to
    /// withdraw — a second tap on the same row, or a stale screen.
    RevokeRefused,
}

/// How something asked of the phone's own link to the relay ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ConnectionOutcome {
    /// The connection has been asked to dial now. Whether that shortened
    /// anything is the connection's to decide, and whether the relay answers
    /// arrives as a connection state rather than as an answer to this.
    RetryRequested,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubscriptionOutcome {
    Subscribed { agent: AgentId },
    Unsubscribed { agent: AgentId },
}

/// How one step of a two-phase pairing ended.
///
/// The first step never grants trust: it returns the machine's own account of
/// itself — its name, the fingerprint of the key it will be trusted by, and
/// when the offer runs out — for a person to look at. Only `Paired` means a
/// trust store was written.
///
/// Every way a secret can be wrong is `PairingRefused` carrying nothing.
/// Whether the code was mistyped, already used, expired or never issued is
/// exactly what a caller guessing codes would want to learn, so the answer is
/// the same shape in all four cases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PairingOutcome {
    /// Authenticated and waiting for a person to say yes.
    PairingPending {
        /// The handle this attempt is confirmed or abandoned by. The capability
        /// the host issued never leaves the runtime.
        pending: String,
        host: model::HostId,
        name: String,
        fingerprint: String,
        expires_at: DateTime<Utc>,
    },
    /// Trust written, on this device and on the machine.
    Paired { host: model::HostId, name: String },
    /// Abandoned by the person, with nothing written anywhere.
    PairingAbandoned,
    /// The secret did not authenticate, in the one shape every such failure has.
    PairingRefused,
    /// The attempt this refers to is not one this runtime is holding — it was
    /// already answered, or the app was restarted since.
    PairingLost,
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
    Session(Box<SessionDto>),
    OpResult {
        op: OpId,
        outcome: OpOutcomeDto,
    },
    Diff {
        agent: AgentId,
        /// The artifact the patch is, so a review sent later names the same
        /// diff the person read.
        diff: model::ArtifactId,
        /// One document per file, numbered, with the repository state it was
        /// computed from. The phone reads the frozen review document rather
        /// than a flat run of hunks, because a page that has to collapse a
        /// file, name it, or say which file a comment is about cannot do any
        /// of that from hunks alone.
        document: review::ReviewDocument,
    },
    /// Machines on the account's relay that this device has not paired with.
    ///
    /// Deliberately not part of the fleet. An untrusted machine is an offer,
    /// not a host: nothing runs on it, no agent of its is readable, and it
    /// exists on the phone only so a person can point at it and start pairing.
    /// It is the pairing screens' only source of a machine to name, because
    /// authenticating a six-digit code is done against one machine and the
    /// phone has to know which.
    Discovered {
        hosts: Vec<model::HostEntry>,
    },
    Connection {
        state: ConnectionDto,
        /// Why, in the closed set a screen can word. Absent while connecting
        /// or connected.
        reason: Option<OfflineReasonDto>,
    },
    TokenRequest {
        request_id: u64,
        /// Which signed-in account the token is wanted for.
        account: String,
    },
    /// How many agents are waiting on an account nobody is looking at.
    ///
    /// Read from that account's own live subscription while the app is in
    /// front of somebody, so it is a count of real work rather than a badge
    /// somebody guessed. It is the only thing an unselected account reports.
    Attention {
        account: String,
        waiting: usize,
    },
    Invariant {
        detail: String,
    },
    /// This phone's own identity and every machine it trusts.
    ///
    /// Apart from the fleet because it answers a different question. The fleet
    /// says which machines are answering and what runs on them; this says
    /// which keys this device has granted access to, including machines that
    /// are away and would still be trusted the moment they came back. It is
    /// what a person reads before revoking one.
    Devices {
        identity: DeviceIdentityDto,
        devices: Vec<PairedDeviceDto>,
    },
}

/// What this phone is, as the machines it pairs with see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceIdentityDto {
    pub host: model::HostId,
    pub name: String,
    /// The fingerprint of this device's public key, in the same spelling the
    /// pairing confirmation shows on the other side of the exchange.
    pub fingerprint: String,
}

/// One machine this phone trusts, as the This-phone section lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairedDeviceDto {
    pub host: model::HostId,
    pub name: String,
    pub fingerprint: String,
    pub paired_at: DateTime<Utc>,
}

impl Event {
    pub fn connection(connection: &RelayConnection) -> Self {
        let (state, reason) = match connection {
            RelayConnection::Connecting => (ConnectionDto::Connecting, None),
            RelayConnection::Connected => (ConnectionDto::Connected, None),
            RelayConnection::Disconnected { reason } => {
                (ConnectionDto::Disconnected, Some((*reason).into()))
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
    /// The machine these rows came from, remembered from the last time this
    /// agent was known, so a machine that has gone away can be told apart
    /// from an agent that has really been removed.
    host: Option<model::HostId>,
    stored: StoredFeed,
}

/// How a chat's store window maps onto the append-only positions a feed
/// reports.
///
/// A store window slides: new entries join its tail, the oldest leave its
/// head, and a merge rewrites an entry in place. Each entry, and each break in
/// the history between entries, keeps the position it was first given for as
/// long as the window stays one continuous run of the items already
/// projected. A window that stops being that run (paging toward older history,
/// or a reload after a gap) starts a new run, which the feed reports as a new
/// window rather than as rows edited out of order.
#[derive(Default)]
struct StoredFeed {
    run: u64,
    base: u64,
    slots: Vec<Slot>,
    converted: BTreeMap<ui_state::EntryKey, (ui_state::StoredDto, Restored)>,
}

/// What holds a position in a stored feed. A break is identified by where it
/// sits rather than by its kind, so a break whose kind the store revises is
/// redrawn in place.
#[derive(PartialEq)]
enum Slot {
    Entry(ui_state::EntryKey),
    Boundary(u32, Option<(ui_state::Order, ui_state::EntryKey)>),
}

enum StoredRow<'a> {
    Entry(&'a Restored),
    Boundary(ui_state::Boundary),
}

enum Restored {
    Claude(claude::FeedEntry),
    ClaudeSdk(claude_sdk::FeedEntry),
    Codex(codex::FeedEntry),
}

impl StoredFeed {
    /// The window's entries and breaks as renderer rows with their stable
    /// positions, and the identity of the run they belong to.
    fn rows(&mut self, window: &ui_state::ChatWindow) -> (u64, Vec<(u64, StoredRow<'_>)>) {
        let history = window.history();
        let slots: Vec<_> = history
            .iter()
            .map(|item| match item {
                ui_state::WindowItem::Entry(entry) => Slot::Entry(entry.key().clone()),
                ui_state::WindowItem::Boundary(boundary) => {
                    Slot::Boundary(boundary.segment, boundary.before.clone())
                }
            })
            .collect();
        let continues = slots.first().and_then(|first| {
            let start = self.slots.iter().position(|slot| slot == first)?;
            let held = &self.slots[start..];
            (held.len() <= slots.len() && held.iter().zip(&slots).all(|(a, b)| a == b))
                .then_some(start)
        });
        match continues {
            Some(start) => self.base += start as u64,
            None => {
                self.run += 1;
                self.base = 0;
            }
        }
        self.slots = slots;
        let mut converted = BTreeMap::new();
        for entry in &window.entries {
            let key = entry.key().clone();
            let restored = match self.converted.remove(&key) {
                Some((held, restored)) if held == *entry => restored,
                _ => match entry {
                    ui_state::StoredDto::Claude(stored) => {
                        Restored::Claude(ui_state::restored::claude::feed_entry(0, &stored.entry))
                    }
                    ui_state::StoredDto::ClaudeSdk(stored) => Restored::ClaudeSdk(
                        ui_state::restored::claude_sdk::feed_entry(0, &stored.entry),
                    ),
                    ui_state::StoredDto::Codex(stored) => {
                        Restored::Codex(ui_state::restored::codex::feed_entry(0, &stored.entry))
                    }
                },
            };
            converted.insert(key, (entry.clone(), restored));
        }
        self.converted = converted;
        let rows = history
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let row = match item {
                    ui_state::WindowItem::Entry(entry) => {
                        StoredRow::Entry(&self.converted[entry.key()].1)
                    }
                    ui_state::WindowItem::Boundary(boundary) => {
                        StoredRow::Boundary(boundary.boundary)
                    }
                };
                (self.base + index as u64, row)
            })
            .collect();
        (self.run, rows)
    }
}

// Compare borrowed native rows; clone only rows that will cross the callback.
enum RowRef<'a> {
    Claude(&'a claude::FeedEntry),
    ClaudeSdk(&'a claude_sdk::FeedEntry),
    Codex(&'a codex::FeedEntry),
    History(&'a HistoryRow),
}
impl<'a> RowRef<'a> {
    fn of(row: &'a FeedEntryDto) -> Self {
        match row {
            FeedEntryDto::ClaudePty(row) => Self::Claude(row),
            FeedEntryDto::ClaudeSdk(row) => Self::ClaudeSdk(row),
            FeedEntryDto::Codex(row) => Self::Codex(row),
            FeedEntryDto::History(row) => Self::History(row),
        }
    }

    fn id(&self) -> u64 {
        match self {
            Self::Claude(row) => row.id,
            Self::ClaudeSdk(row) => row.id,
            Self::Codex(row) => row.id,
            Self::History(row) => row.id,
        }
    }
    fn seq(&self) -> u64 {
        match self {
            Self::Claude(row) => row.seq,
            Self::ClaudeSdk(row) => row.seq,
            Self::Codex(row) => row.seq,
            Self::History(row) => row.seq,
        }
    }
    fn same(&self, previous: &FeedEntryDto) -> bool {
        match (self, previous) {
            (Self::Claude(row), FeedEntryDto::ClaudePty(old)) => *row == old,
            (Self::ClaudeSdk(row), FeedEntryDto::ClaudeSdk(old)) => *row == old,
            (Self::Codex(row), FeedEntryDto::Codex(old)) => *row == old,
            (Self::History(row), FeedEntryDto::History(old)) => *row == old,
            _ => false,
        }
    }
    fn owned(&self) -> FeedEntryDto {
        match self {
            Self::Claude(row) => FeedEntryDto::ClaudePty((*row).clone()),
            Self::ClaudeSdk(row) => FeedEntryDto::ClaudeSdk((*row).clone()),
            Self::Codex(row) => FeedEntryDto::Codex((*row).clone()),
            Self::History(row) => FeedEntryDto::History((*row).clone()),
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
    /// The machines last reported as discovered. A plain list rather than the
    /// event, so a phone that has discovered nothing — which is every phone
    /// until one is found — never sends an event saying so.
    discovered: Vec<model::HostEntry>,
    synchronized: bool,
    remote_inventories: BTreeMap<model::HostId, BTreeSet<AgentId>>,
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
            // A frozen patch reaches the phone as the review document the
            // rest of the workspace already reads, identity and all. A patch
            // that will not parse into one is dropped rather than half-drawn:
            // a page whose row numbers or file boundaries were guessed would
            // put a comment on the wrong line.
            let document = match (&result.command, &result.outcome) {
                (Command::RequestDiff { agent, .. }, OpOutcome::DiffReady { response }) => {
                    review::parse_patch(&response.patch, response.identity.clone(), &response.files)
                        .ok()
                        .map(|document| (*agent, response.artifact.id.clone(), document))
                }
                // A patch fetched back from a stored review is not projected
                // here. The identity it was taken against lives in the review
                // mention that referenced it, not in this outcome, and a
                // document assembled with an invented identity would claim
                // the phone knew what it had been diffed against. Reading a
                // review somebody else sent is its own path.
                _ => continue,
            };
            let Some((agent, diff, document)) = document else {
                events.push(Event::Invariant {
                    detail: "a diff arrived that would not parse as a review document".into(),
                });
                continue;
            };
            events.push(Event::Diff {
                agent,
                diff,
                document,
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
                // A remembered row belongs to a machine this device still
                // trusts; one whose machine was unpaired is not drawn while
                // the connection that will drop it is on its way.
                .filter(|card| {
                    !card.remembered
                        || model.host(card.agent.host_id).is_some_and(|host| {
                            host.entry.trust_status == model::HostTrustStatus::Trusted
                        })
                })
                .map(|card| AgentCardDto {
                    agent: card.agent.clone(),
                    display_name: card.display_name(),
                    attention: model.fleet_attention(card),
                    phase: model.effective_phase(card),
                    last_activity: model.effective_summary_age(card),
                    outcome: None,
                    // A row the store remembered stays unconfirmed until the
                    // machine that owns it has answered for it.
                    awaiting: card.remembered,
                })
                .collect(),
            hosts: model
                .hosts()
                // This device is not one of the machines. It is trusted by
                // itself and so arrives in its own inventory, but nothing runs
                // on a phone: the Hosts tab lists places agents live and says
                // what this phone is in a section of its own.
                .filter(|host| model.is_local(host.entry.id) != Some(true))
                .filter(|host| host.entry.trust_status == model::HostTrustStatus::Trusted)
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
        let discovered: Vec<_> = model
            .hosts()
            .filter(|host| model.is_local(host.entry.id) != Some(true))
            .filter(|host| host.entry.trust_status == model::HostTrustStatus::UntrustedButOnline)
            .map(|host| host.entry.clone())
            .collect();
        if self.discovered != discovered {
            self.discovered = discovered.clone();
            events.push(Event::Discovered { hosts: discovered });
        }
        for agent in &self.subscribed {
            let session = session(model, *agent);
            if self.sessions.get(agent) != Some(&session) {
                self.sessions.insert(*agent, session.clone());
                events.push(Event::Session(Box::new(session)));
            }
            let state = self.feeds.entry(*agent).or_default();
            if let Some(card) = model.agent(*agent) {
                state.host = Some(card.agent.host_id);
            }
            let stored = model.chat(*agent).filter(|chat| !chat.entries.is_empty());
            let feed = if let Some(chat) = stored {
                // The chat's store window is what this device knows of the
                // conversation, remembered rows and live ones alike, so it is
                // what the reader sees whenever it holds anything.
                let protocol = chat.protocol;
                let (run, rows) = state.stored.rows(chat);
                let evicted = rows.first().map_or(0, |(id, _)| *id);
                let rows: Vec<_> = rows
                    .into_iter()
                    .map(|(id, row)| match row {
                        StoredRow::Entry(Restored::Claude(entry)) => {
                            FeedEntryDto::ClaudePty(claude::FeedEntry {
                                id,
                                ..entry.clone()
                            })
                        }
                        StoredRow::Entry(Restored::ClaudeSdk(entry)) => {
                            let mut entry = entry.clone();
                            entry.id = id;
                            FeedEntryDto::ClaudeSdk(entry)
                        }
                        StoredRow::Entry(Restored::Codex(entry)) => {
                            FeedEntryDto::Codex(codex::FeedEntry {
                                id,
                                ..entry.clone()
                            })
                        }
                        StoredRow::Boundary(boundary) => FeedEntryDto::History(HistoryRow {
                            id,
                            seq: 0,
                            boundary: boundary.into(),
                        }),
                    })
                    .collect();
                state.project(
                    *agent,
                    (protocol, Some(format!("store:{run}"))),
                    evicted,
                    rows.iter().map(RowRef::of),
                )
            } else if let Some(layer) = model.claude(*agent) {
                // A fold that has not begun replaces nothing: an agent whose
                // machine has just come back holds an empty layer until its
                // replay arrives, and a transcript that emptied itself for
                // those seconds would be reporting the reconnection rather
                // than the conversation. A session that was really cleared
                // names a new session and evicts through the window rule
                // below.
                if layer.entry_count() == 0
                    && layer.session_id().is_none()
                    && !state.rows.is_empty()
                {
                    None
                } else {
                    state.project(
                        *agent,
                        (
                            StructuredProtocol::ClaudePtyTranscript,
                            layer.session_id().map(str::to_owned),
                        ),
                        layer.evicted_entries(),
                        layer.entries().map(RowRef::Claude),
                    )
                }
            } else if let Some(layer) = model.claude_sdk(*agent) {
                if layer.entry_count() == 0 && !state.rows.is_empty() {
                    None
                } else {
                    state.project(
                        *agent,
                        (StructuredProtocol::ClaudeSdk, None),
                        layer.evicted_entries(),
                        layer.entries().map(RowRef::ClaudeSdk),
                    )
                }
            } else if let Some(layer) = model.codex(*agent) {
                if layer.entry_count() == 0 && !state.rows.is_empty() {
                    None
                } else {
                    state.project(
                        *agent,
                        (StructuredProtocol::Codex, None),
                        layer.evicted_entries(),
                        layer.entries().map(RowRef::Codex),
                    )
                }
            } else if keeps_its_rows(model, *agent, state.host) {
                None
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

/// Whether an agent with no readable layer keeps the rows it already had.
///
/// A transcript is the only account of a conversation there is, and losing the
/// fold it was projected from is not evidence that the account was wrong. So
/// the rows stay until something replaces them — the machine's own replay when
/// it answers again — rather than emptying the screen of a reader who is being
/// told at the same time that the machine cannot be reached. Only two things
/// really end a transcript: an agent removed from a machine that is still
/// answering, and an agent whose provider this build cannot read at all.
fn keeps_its_rows(model: &Model, agent: AgentId, host: Option<model::HostId>) -> bool {
    match model.agent(agent) {
        // Here, and running a provider with a transcript — it simply has not
        // folded one yet, which is the state a machine passes through between
        // coming back and replaying what it holds.
        Some(card) => matches!(
            card.structured_protocol(),
            Some(
                StructuredProtocol::ClaudePtyTranscript
                    | StructuredProtocol::ClaudeSdk
                    | StructuredProtocol::Codex
            )
        ),
        // The machine that owned it has stopped answering. Nothing has said
        // this agent is gone, only that nobody can be asked about it.
        None => host.is_some_and(|host| !model.host_online(host)),
    }
}

fn session(model: &Model, agent: AgentId) -> SessionDto {
    let protocol = model
        .agent(agent)
        .and_then(|card| card.structured_protocol());
    let (gate, phase, asks, facts) = match protocol {
        Some(StructuredProtocol::ClaudePtyTranscript) => (
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
            GateDto::ClaudeSdk(claude_sdk::send_gate(model, agent)),
            PhaseDto::ClaudeSdk(claude_sdk::phase(model, agent)),
            model
                .claude_sdk(agent)
                .map(|layer| layer.asks().cloned().map(AskDto::ClaudeSdk).collect())
                .unwrap_or_default(),
            FactsDto::ClaudeSdk {
                session: model
                    .claude_sdk(agent)
                    .map(|layer| layer.session().clone())
                    .unwrap_or_default(),
            },
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
        provider: Box::new(ui_state::provider::facts(model, agent)),
        settings_gate: ui_state::provider::settings_gate(model, agent),
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
