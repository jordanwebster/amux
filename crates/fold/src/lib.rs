//! Pure observation and transcript folds shared by the daemon and clients.
//!
//! This crate owns deterministic, I/O-free derivation and the value vocabulary
//! exchanged with the SQLite-backed store. Persisted values use postcard's
//! non-self-describing representation; provider JSON crosses this boundary as
//! bytes and is never retained as a JSON value tree.

#![forbid(unsafe_code)]

mod algebra;
pub mod claude_pty;
pub mod claude_sdk;
mod claude_tasks;
pub mod codex;
pub mod diff;
mod oracle;

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

pub use algebra::{
    Component, ComponentSource, Components, FieldPatch, MutationGroup, VersionedField, coalesce,
};
use chrono::{DateTime, Utc};
use model::{
    Agent, AgentId, AgentKind, AgentParent, AgentPhase, Attention, Capabilities, ClaudeDriver,
    ContextMeter, ContextMeterSource, HostEntry, HostId, HostTrustStatus, Progress, Seq,
    StructuredProtocol, Summary, SummaryEnvelope, SummaryField, SupportedAgentType, TodoProgress,
    Why, WorkingOn,
};
pub use oracle::{LifecycleRevisions, MutationOracle, RedirectState};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The largest complete encoded entry key accepted by the fold and store.
pub const ENTRY_KEY_MAX_BYTES: usize = 512;
pub const ORDER_SLOT_MAX: u16 = 1023;
pub const TIP_MAX_BYTES: usize = 1024 * 1024;
pub const STREAMING_BLOCK_MAX_BYTES: usize = 64 * 1024;
pub const TIP_MAX_OPEN_ENTRIES: usize = 256;
pub const ENTRY_MAX_COMPONENTS: usize = 256;
pub const DESKTOP_ENTRY_MAX_BYTES: usize = 512 * 1024;
pub const PHONE_ENTRY_MAX_BYTES: usize = 256 * 1024;

/// A segment number within one agent. Zero means that no segment exists yet.
pub type SegmentId = u32;
pub type HeadVersion = u64;
pub type ChatRevision = u64;
pub type Generation = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generations {
    pub fleet: Generation,
    pub chat: Generation,
    pub provider: Generation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttemptId(pub u64);

/// Store-operation identity. This is intentionally distinct from command ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamAttempt(pub u64);

/// Everything a provider fold consumes.
///
/// Row payloads are borrowed JSON bytes. They are transient input, not part of
/// the persisted graph; a provider parses only the fields it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input<'a> {
    Row {
        seq: Seq,
        published_at: DateTime<Utc>,
        activity_at: Option<DateTime<Utc>>,
        historical: bool,
        #[serde(borrow)]
        payload: &'a [u8],
    },
    ReplayComplete {
        through: Seq,
        at: DateTime<Utc>,
    },
    ProcessExited {
        exit_code: Option<i32>,
        at: DateTime<Utc>,
    },
    ObserverLost {
        at: DateTime<Utc>,
    },
    Tick {
        now: DateTime<Utc>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Baseline {
    Start,
    Truncated { from: Seq },
    Gap { after: Seq },
    VersionGap { after: Seq },
}

/// A provider-native entry identity, including its kind namespace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EntryKey(String);

impl EntryKey {
    pub fn new(value: impl Into<String>) -> Result<Self, EntryKeyTooLong> {
        let value = value.into();
        let encoded_bytes = value.len();
        if encoded_bytes > ENTRY_KEY_MAX_BYTES {
            return Err(EntryKeyTooLong { encoded_bytes });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for EntryKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for EntryKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for EntryKey {
    type Error = EntryKeyTooLong;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for EntryKey {
    type Error = EntryKeyTooLong;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EntryKey> for String {
    fn from(value: EntryKey) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryKeyTooLong {
    pub encoded_bytes: usize,
}

impl fmt::Display for EntryKeyTooLong {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "entry key is {} bytes; the maximum is {ENTRY_KEY_MAX_BYTES}",
            self.encoded_bytes
        )
    }
}

impl Error for EntryKeyTooLong {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "OrderRepr", into = "OrderRepr")]
pub struct Order {
    seq: Seq,
    slot: u16,
}

impl Order {
    pub fn new(seq: Seq, slot: u16) -> Result<Self, OrderSlotTooLarge> {
        if slot > ORDER_SLOT_MAX {
            return Err(OrderSlotTooLarge { slot });
        }
        Ok(Self { seq, slot })
    }

    pub fn seq(self) -> Seq {
        self.seq
    }

    pub fn slot(self) -> u16 {
        self.slot
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OrderRepr {
    seq: Seq,
    slot: u16,
}

impl TryFrom<OrderRepr> for Order {
    type Error = OrderSlotTooLarge;

    fn try_from(value: OrderRepr) -> Result<Self, Self::Error> {
        Self::new(value.seq, value.slot)
    }
}

impl From<Order> for OrderRepr {
    fn from(value: Order) -> Self {
        Self {
            seq: value.seq,
            slot: value.slot,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderSlotTooLarge {
    pub slot: u16,
}

impl fmt::Display for OrderSlotTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "entry slot is {}; the maximum is {ORDER_SLOT_MAX}",
            self.slot
        )
    }
}

impl Error for OrderSlotTooLarge {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Revision {
    pub seq: Seq,
    pub fence: ChatRevision,
    pub ordinal: u32,
}

impl Revision {
    pub const fn row(seq: Seq) -> Self {
        Self {
            seq,
            fence: 0,
            ordinal: 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Patch<T> {
    #[default]
    Unchanged,
    Set {
        value: T,
        revision: Revision,
    },
    Clear {
        revision: Revision,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Promotion {
    ToolToTask,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeDefect {
    EqualRevisionDisagreement {
        field: String,
        revision: Revision,
    },
    ComponentDisagreement {
        source: String,
    },
    AliasCycle {
        from: EntryKey,
        to: EntryKey,
    },
    InvalidLifecycleFence,
    RevisionExhausted,
    EntryOverBudget {
        key: EntryKey,
        encoded_bytes: usize,
        budget: usize,
    },
}

impl fmt::Display for MergeDefect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EqualRevisionDisagreement { field, revision } => write!(
                formatter,
                "field {field} disagrees at revision {}:{}:{}",
                revision.seq, revision.fence, revision.ordinal
            ),
            Self::AliasCycle { from, to } => {
                write!(formatter, "alias from {from} to {to} would form a cycle")
            }
            Self::ComponentDisagreement { source } => {
                write!(
                    formatter,
                    "component {source} disagrees with its retransmission"
                )
            }
            Self::InvalidLifecycleFence => {
                formatter.write_str("lifecycle revisions require an accepted nonzero fence")
            }
            Self::RevisionExhausted => formatter.write_str("revision ordinal exhausted"),
            Self::EntryOverBudget {
                key,
                encoded_bytes,
                budget,
            } => write!(
                formatter,
                "entry {key} remains {encoded_bytes} bytes after clipping to {budget} bytes"
            ),
        }
    }
}

impl Error for MergeDefect {}

/// Marker for values audited as part of the persisted postcard graph.
///
/// The trait is sealed: notably, it has no implementation for
/// `serde_json::Value`. Provider entry and tip types must satisfy this bound
/// before they can implement [`Entry`] or [`ProviderFold`].
pub trait PostcardSafe: private::Sealed {}

pub trait Entry: Serialize + DeserializeOwned + Clone + PostcardSafe {
    type Partial: Serialize + DeserializeOwned + Clone + PostcardSafe;

    fn kind(&self) -> &'static str;
    fn text(&self) -> Option<&str>;
    fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect>;
    fn from_partial(patch: &Self::Partial) -> Result<Self, MergeDefect>;
    /// Merge an aliased source into this target. Target fields win when both
    /// are present unless the named promotion defines a provider exception.
    fn merge_alias(
        &mut self,
        source: &Self,
        promotion: Option<Promotion>,
    ) -> Result<(), MergeDefect>;
    /// Apply a promotion when only one side of an alias currently exists.
    fn promote(&mut self, promotion: Option<Promotion>) -> Result<(), MergeDefect>;
    fn clip(&mut self, budget: usize);
    fn bytes(&self) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub enum Mutation<E: Entry> {
    Upsert {
        key: EntryKey,
        order: Order,
        revision: Revision,
        entry: E::Partial,
    },
    Delete {
        key: EntryKey,
        revision: Revision,
    },
    Alias {
        from: EntryKey,
        to: EntryKey,
        revision: Revision,
        promote: Option<Promotion>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Changes<E: Entry> {
    pub summary: Option<Summary>,
    pub through: Seq,
    pub mutations: Vec<Mutation<E>>,
}

pub trait ProviderFold: Default + Serialize + DeserializeOwned + PostcardSafe {
    type Entry: Entry;

    const PROTOCOL: StructuredProtocol;
    const ENTRY_VERSION: u32;
    const TIP_VERSION: u32;
    const TIP_BUDGET: usize;

    fn begin(&mut self, segment: SegmentId, baseline: Baseline);
    fn apply(&mut self, input: Input<'_>) -> Changes<Self::Entry>;
    fn summary(&self) -> Summary;
    fn tip_bytes(&self) -> usize;
}

/// The closed provider fold. Provider variants are added only as they land.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentFold {
    Claude(claude_pty::ClaudeFold),
    ClaudeSdk(claude_sdk::ClaudeSdkFold),
    Codex(codex::CodexFold),
}

/// Provider-erased output used by observers that retain only standing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryChanges {
    pub through: Seq,
    pub changed: bool,
    pub summary: Summary,
}

/// The summary a client should present after comparing its open-chat fold
/// with the daemon's advisory envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub through: Seq,
    pub summary: Summary,
    /// The time to use when the selected summary has no known activity time.
    pub observed_at: Option<DateTime<Utc>>,
    /// The selected daemon envelope has fallen behind its source.
    pub stale: bool,
    /// A daemon envelope was present but produced by an incompatible fold,
    /// and there was no client-local candidate to replace it.
    pub incompatible: bool,
}

/// Select one whole summary, then fill only fields that winner explicitly
/// marks unknown from the losing candidate.
pub fn select_summary(
    host: Option<&SummaryEnvelope>,
    local: Option<&(Seq, Summary)>,
    producer_version: u32,
) -> Option<Effective> {
    let compatible_host = host.filter(|candidate| candidate.producer_version == producer_version);

    let (through, mut summary, observed_at, stale) = match (compatible_host, local) {
        (Some(host), Some((local_through, local_summary))) if *local_through > host.through => (
            *local_through,
            local_summary.clone(),
            local_summary.last_activity,
            false,
        ),
        (Some(host), Some((_, local_summary))) => (
            host.through,
            fill_unknowns(host.summary.clone(), local_summary),
            Some(host.observed_at),
            host.stale,
        ),
        (Some(host), None) => (
            host.through,
            host.summary.clone(),
            Some(host.observed_at),
            host.stale,
        ),
        (None, Some((through, summary))) => {
            (*through, summary.clone(), summary.last_activity, false)
        }
        (None, None) => {
            let excluded = host?;
            return Some(Effective {
                through: excluded.through,
                summary: unknown_summary(),
                observed_at: Some(excluded.observed_at),
                stale: false,
                incompatible: true,
            });
        }
    };

    // A local winner still gets knowledge that it explicitly lacks from the
    // compatible host. The guarded arm above handles the opposite direction.
    if let (Some(host), Some((local_through, _))) = (compatible_host, local)
        && *local_through > host.through
    {
        summary = fill_unknowns(summary, &host.summary);
    }

    Some(Effective {
        through,
        summary,
        observed_at,
        stale,
        incompatible: false,
    })
}

fn fill_unknowns(mut winner: Summary, loser: &Summary) -> Summary {
    for field in winner.unknown.clone() {
        if loser.unknown.contains(&field) {
            continue;
        }
        match field {
            SummaryField::Attention => winner.attention = loser.attention,
            SummaryField::Phase => winner.phase = loser.phase.clone(),
            SummaryField::LastActivity => winner.last_activity = loser.last_activity,
            SummaryField::Todo => winner.todo = loser.todo.clone(),
            SummaryField::Context => winner.context = loser.context.clone(),
            SummaryField::Model => winner.model = loser.model.clone(),
            SummaryField::Outstanding => {}
        }
        winner.unknown.retain(|unknown| *unknown != field);
    }
    winner
}

fn unknown_summary() -> Summary {
    Summary {
        attention: Attention::Unknown,
        phase: AgentPhase::Running,
        last_activity: None,
        todo: None,
        context: None,
        model: None,
        unknown: vec![
            SummaryField::Attention,
            SummaryField::Phase,
            SummaryField::LastActivity,
            SummaryField::Todo,
            SummaryField::Context,
            SummaryField::Model,
            SummaryField::Outstanding,
        ],
    }
}

impl AgentFold {
    pub fn for_protocol(protocol: StructuredProtocol) -> Self {
        match protocol {
            StructuredProtocol::ClaudePtyTranscript => Self::Claude(Default::default()),
            StructuredProtocol::ClaudeSdk => Self::ClaudeSdk(Default::default()),
            StructuredProtocol::Codex => Self::Codex(Default::default()),
        }
    }

    pub fn begin(&mut self, segment: SegmentId, baseline: Baseline) {
        match self {
            Self::Claude(fold) => fold.begin(segment, baseline),
            Self::ClaudeSdk(fold) => fold.begin(segment, baseline),
            Self::Codex(fold) => fold.begin(segment, baseline),
        }
    }

    pub fn apply_summary(&mut self, input: Input<'_>) -> SummaryChanges {
        macro_rules! apply {
            ($fold:expr) => {{
                let changes = $fold.apply(input);
                SummaryChanges {
                    through: changes.through,
                    changed: changes.summary.is_some(),
                    summary: $fold.summary(),
                }
            }};
        }
        match self {
            Self::Claude(fold) => apply!(fold),
            Self::ClaudeSdk(fold) => apply!(fold),
            Self::Codex(fold) => apply!(fold),
        }
    }

    pub fn summary(&self) -> Summary {
        match self {
            Self::Claude(fold) => fold.summary(),
            Self::ClaudeSdk(fold) => fold.summary(),
            Self::Codex(fold) => fold.summary(),
        }
    }

    pub fn tip_version(&self) -> u32 {
        match self {
            Self::Claude(_) => claude_pty::ClaudeFold::TIP_VERSION,
            Self::ClaudeSdk(_) => claude_sdk::ClaudeSdkFold::TIP_VERSION,
            Self::Codex(_) => codex::CodexFold::TIP_VERSION,
        }
    }

    /// Retained heap bytes in the provider tip, erased across protocols.
    pub fn tip_bytes(&self) -> usize {
        match self {
            Self::Claude(fold) => fold.tip_bytes(),
            Self::ClaudeSdk(fold) => fold.tip_bytes(),
            Self::Codex(fold) => fold.tip_bytes(),
        }
    }
}

/// Opaque provider JSON in a persisted value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonBytes(pub Vec<u8>);

/// One bounded provider row retained only to rebuild a pending live
/// obligation after the durable cursor has skipped its creating row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRow {
    pub seq: Seq,
    pub payload: JsonBytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Head<F: ProviderFold> {
    pub segment: SegmentId,
    pub baseline: Baseline,
    pub through: Seq,
    pub tip_version: u32,
    pub entry_version: u32,
    pub tip: F,
    pub summary: Summary,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentTransition {
    pub predecessor: Option<SegmentId>,
    pub successor: SegmentId,
    pub baseline: Baseline,
    pub previous_through: Seq,
    /// The first selected replay row. None represents an empty covered interval.
    pub selected_from: Option<Seq>,
    /// The replay watermark at which this segment was opened.
    pub replay_through: Seq,
    pub opened_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedHead {
    Absent {
        fence: ChatRevision,
    },
    Present {
        fence: ChatRevision,
        version: HeadVersion,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Boundary {
    Truncated,
    Gap,
    VersionGap,
    Evicted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryAt {
    pub segment: SegmentId,
    /// The entry before which the boundary renders. None means segment start.
    pub before: Option<(Order, EntryKey)>,
    pub boundary: Boundary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored<E> {
    pub key: EntryKey,
    pub segment: SegmentId,
    pub order: Order,
    pub revision: Revision,
    pub entry: E,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageToken {
    pub generations: Generations,
    pub content_revision: ChatRevision,
    pub view_epoch: u64,
    pub before: (SegmentId, Order, EntryKey),
}

/// Bounds the remembered window returned by a store load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowBudget {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub view_epoch: u64,
}

impl WindowBudget {
    pub const fn desktop(view_epoch: u64) -> Self {
        Self {
            max_entries: 400,
            max_bytes: 16 * 1024 * 1024,
            view_epoch,
        }
    }
}

/// The part of a visible window for which a commit must return canonical state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInterest {
    pub held_keys: Vec<EntryKey>,
    pub feed_from: Option<(SegmentId, Order, EntryKey)>,
    pub view_epoch: u64,
    pub result_max_bytes: usize,
}

impl WindowInterest {
    pub const fn all(view_epoch: u64, result_max_bytes: usize) -> Self {
        Self {
            held_keys: Vec::new(),
            feed_from: None,
            view_epoch,
            result_max_bytes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<E> {
    pub entries: Vec<Stored<E>>,
    pub boundaries: Vec<BoundaryAt>,
    pub next: Option<PageToken>,
    pub content_revision: ChatRevision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub enum HeadState<F: ProviderFold> {
    Usable(HeadVersion, Head<F>),
    NeedsBaseline {
        previous_through: Seq,
        reason: BaselineReason,
    },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselineReason {
    TipVersion,
    Corrupt,
    First,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Loaded<F: ProviderFold> {
    pub generations: Generations,
    pub fence: ChatRevision,
    pub content_revision: ChatRevision,
    pub segment_high_water: SegmentId,
    pub head: HeadState<F>,
    pub window: Vec<Stored<F::Entry>>,
    pub boundaries: Vec<BoundaryAt>,
    pub first_page: Option<PageToken>,
    pub aliases: Vec<(EntryKey, EntryKey)>,
    pub host: Option<SummaryEnvelope>,
    pub progress: Option<Progress>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub key: EntryKey,
    pub segment: SegmentId,
    pub order: Order,
    pub revision: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResult {
    pub expected: ExpectedHead,
    pub content_revision: ChatRevision,
    pub placed: Vec<Placement>,
    pub bodies: Vec<Stored<JsonBytes>>,
    pub deleted: Vec<EntryKey>,
    pub redirected: Vec<(EntryKey, EntryKey)>,
    pub boundaries: Vec<BoundaryAt>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
#[allow(
    clippy::large_enum_variant,
    reason = "a conflict carries the fresh loaded state by value as the store contract requires"
)]
pub enum CommitOutcome<F: ProviderFold> {
    Committed(CommitResult),
    Conflict(Loaded<F>),
    Refused(StoreError),
}

/// A store-backed fleet snapshot shared with I/O-free reducers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fleet {
    pub hosts: Vec<FleetHost>,
    pub agents: Vec<FleetAgent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetHost {
    pub host: HostEntry,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetAgent {
    pub agent: Agent,
    pub membership: Membership,
    pub absent_since: Option<DateTime<Utc>>,
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreError {
    Busy,
    DiskFull,
    Permission,
    Io,
    Invalid,
    Corrupt,
    UnsupportedFormat,
    GenerationMoved,
    RecoveryRequired,
    OverBudget,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "store is busy",
            Self::DiskFull => "store is full",
            Self::Permission => "store permission denied",
            Self::Io => "store I/O failed",
            Self::Invalid => "store operation is invalid",
            Self::Corrupt => "store is corrupt",
            Self::UnsupportedFormat => "store format is newer or unsupported",
            Self::GenerationMoved => "store generation moved",
            Self::RecoveryRequired => "durable store recovery is required",
            Self::OverBudget => "store remains over budget",
        })
    }
}

impl Error for StoreError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub host_id: HostId,
    pub through_revision: u64,
    pub agents: Vec<(Agent, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FleetDelta {
    Host {
        host: HostEntry,
        revision: u64,
    },
    Reachability {
        host_id: HostId,
        online: bool,
    },
    /// The host left the paired set: nothing this device remembered about it
    /// is a member any more.
    HostRemoved {
        host_id: HostId,
    },
    Snapshot(FleetSnapshot),
    AgentUp {
        agent: Agent,
        revision: u64,
    },
    AgentUpdated {
        agent: Agent,
        revision: u64,
    },
    AgentDown {
        host_id: HostId,
        agent_id: AgentId,
        revision: u64,
        reason: Option<String>,
    },
    Summary {
        host_id: HostId,
        agent_id: AgentId,
        envelope: SummaryEnvelope,
    },
    Progress {
        host_id: HostId,
        agent_id: AgentId,
        progress: Progress,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Membership {
    Cached,
    Absent,
}

mod private {
    pub trait Sealed {
        fn assert_fields_are_postcard_safe();
    }
}

macro_rules! leaf_safe {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl private::Sealed for $ty {
                fn assert_fields_are_postcard_safe() {}
            }
            impl PostcardSafe for $ty {}
        )+
    };
}

macro_rules! composite_safe {
    ($ty:ty => [$($field:ty),* $(,)?]) => {
        impl private::Sealed for $ty {
            fn assert_fields_are_postcard_safe() {
                $(assert_postcard_safe::<$field>();)*
            }
        }
        impl PostcardSafe for $ty {}
    };
}

fn assert_postcard_safe<T: PostcardSafe>() {
    T::assert_fields_are_postcard_safe();
}

leaf_safe!(u8, u16, u32, u64, usize, i32, i64, bool, String,);

// External scalar wrappers have no provider-owned generic field graph to
// audit. Keep them visibly separate from the primitive allowlist so adding a
// persisted project type always requires an exhaustive implementation below.
macro_rules! opaque_safe {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl private::Sealed for $ty {
                fn assert_fields_are_postcard_safe() {}
            }
            impl PostcardSafe for $ty {}
        )+
    };
}
opaque_safe!(DateTime<Utc>, HostId, PathBuf);

macro_rules! fieldless_enum_safe {
    ($ty:ty, $value:ident => $body:expr) => {
        impl private::Sealed for $ty {
            fn assert_fields_are_postcard_safe() {
                let _ = |$value: $ty| $body;
            }
        }
        impl PostcardSafe for $ty {}
    };
}

impl private::Sealed for Baseline {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: Baseline| match value {
            Baseline::Start => {}
            Baseline::Truncated { from } => assert_value_safe(&from),
            Baseline::Gap { after } | Baseline::VersionGap { after } => assert_value_safe(&after),
        };
    }
}
impl PostcardSafe for Baseline {}
fieldless_enum_safe!(Promotion, value => match value {
    Promotion::ToolToTask => {}
});
impl private::Sealed for AgentFold {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: AgentFold| match value {
            AgentFold::Claude(fold) => assert_value_safe(&fold),
            AgentFold::ClaudeSdk(fold) => assert_value_safe(&fold),
            AgentFold::Codex(fold) => assert_value_safe(&fold),
        };
    }
}
impl PostcardSafe for AgentFold {}
fieldless_enum_safe!(Boundary, value => match value {
    Boundary::Truncated | Boundary::Gap | Boundary::VersionGap | Boundary::Evicted => {}
});
fieldless_enum_safe!(BaselineReason, value => match value {
    BaselineReason::TipVersion | BaselineReason::Corrupt | BaselineReason::First => {}
});
fieldless_enum_safe!(StoreError, value => match value {
    StoreError::Busy
    | StoreError::DiskFull
    | StoreError::Permission
    | StoreError::Io
    | StoreError::Invalid
    | StoreError::Corrupt
    | StoreError::UnsupportedFormat
    | StoreError::GenerationMoved
    | StoreError::RecoveryRequired
    | StoreError::OverBudget => {}
});
fieldless_enum_safe!(Membership, value => match value {
    Membership::Cached | Membership::Absent => {}
});

fn assert_value_safe<T: PostcardSafe>(_: &T) {}

macro_rules! model_struct_safe {
    ($ty:ty, $pattern:pat => [$($field:ident),* $(,)?]) => {
        impl private::Sealed for $ty {
            fn assert_fields_are_postcard_safe() {
                let _ = |$pattern: $ty| {
                    $(assert_value_safe(&$field);)*
                };
            }
        }
        impl PostcardSafe for $ty {}
    };
}

impl private::Sealed for ClaudeDriver {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ClaudeDriver| match value {
            ClaudeDriver::Pty | ClaudeDriver::Sdk => {}
        };
    }
}
impl PostcardSafe for ClaudeDriver {}

impl private::Sealed for AgentKind {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: AgentKind| match value {
            AgentKind::Claude { driver } => assert_value_safe(&driver),
            AgentKind::Codex | AgentKind::TestAgent => {}
        };
    }
}
impl PostcardSafe for AgentKind {}

model_struct_safe!(
    AgentParent,
    AgentParent { agent_id, host_id } => [agent_id, host_id]
);
model_struct_safe!(
    WorkingOn,
    WorkingOn { text, updated_at } => [text, updated_at]
);
model_struct_safe!(
    Agent,
    Agent {
        id,
        host_id,
        name,
        command,
        working_dir,
        kind,
        readonly,
        args,
        created_at,
        parent,
        working_on,
        summary,
        progress,
        inventory_revision,
    } => [
        id,
        host_id,
        name,
        command,
        working_dir,
        kind,
        readonly,
        args,
        created_at,
        parent,
        working_on,
        summary,
        progress,
        inventory_revision,
    ]
);

model_struct_safe!(
    SupportedAgentType,
    SupportedAgentType { agent_type } => [agent_type]
);
model_struct_safe!(
    Capabilities,
    Capabilities {
        features,
        supported_agent_types,
    } => [features, supported_agent_types]
);

impl private::Sealed for HostTrustStatus {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: HostTrustStatus| match value {
            HostTrustStatus::Trusted | HostTrustStatus::UntrustedButOnline => {}
        };
    }
}
impl PostcardSafe for HostTrustStatus {}

model_struct_safe!(
    HostEntry,
    HostEntry {
        id,
        name,
        online,
        version,
        capabilities,
        trust_status,
        last_dial_error,
        platform,
    } => [
        id,
        name,
        online,
        version,
        capabilities,
        trust_status,
        last_dial_error,
        platform,
    ]
);

impl private::Sealed for Why {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: Why| match value {
            Why::Permission | Why::Question | Why::Finished => {}
        };
    }
}
impl PostcardSafe for Why {}

impl private::Sealed for Attention {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: Attention| match value {
            Attention::Unknown | Attention::Idle | Attention::Working => {}
            Attention::NeedsYou { why } => assert_value_safe(&why),
        };
    }
}
impl PostcardSafe for Attention {}

impl private::Sealed for AgentPhase {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: AgentPhase| match value {
            AgentPhase::Running => {}
            AgentPhase::Exited { exit_code } => assert_value_safe(&exit_code),
        };
    }
}
impl PostcardSafe for AgentPhase {}

model_struct_safe!(
    TodoProgress,
    TodoProgress {
        done,
        total,
        current,
    } => [done, total, current]
);

impl private::Sealed for ContextMeterSource {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ContextMeterSource| match value {
            ContextMeterSource::AssistantUsage
            | ContextMeterSource::ResultUsage
            | ContextMeterSource::AssistantContextUsage
            | ContextMeterSource::CompactBoundary => {}
        };
    }
}
impl PostcardSafe for ContextMeterSource {}

model_struct_safe!(
    ContextMeter,
    ContextMeter {
        used_tokens,
        window_tokens,
        source,
    } => [used_tokens, window_tokens, source]
);

impl private::Sealed for SummaryField {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: SummaryField| match value {
            SummaryField::Attention
            | SummaryField::Phase
            | SummaryField::LastActivity
            | SummaryField::Todo
            | SummaryField::Context
            | SummaryField::Model
            | SummaryField::Outstanding => {}
        };
    }
}
impl PostcardSafe for SummaryField {}

model_struct_safe!(
    Summary,
    Summary {
        attention,
        phase,
        last_activity,
        todo,
        context,
        model,
        unknown,
    } => [
        attention,
        phase,
        last_activity,
        todo,
        context,
        model,
        unknown,
    ]
);
model_struct_safe!(
    SummaryEnvelope,
    SummaryEnvelope {
        through,
        producer_version,
        observed_at,
        stale,
        revision,
        summary,
    } => [
        through,
        producer_version,
        observed_at,
        stale,
        revision,
        summary,
    ]
);
model_struct_safe!(
    Progress,
    Progress {
        through,
        at,
        revision,
    } => [through, at, revision]
);

composite_safe!(EntryKey => [String]);
composite_safe!(Order => [u64, u16]);
composite_safe!(Revision => [u64, u64, u32]);
composite_safe!(MergeDefect => [String, Revision, EntryKey, usize]);
composite_safe!(JsonBytes => [Vec<u8>]);
composite_safe!(RestoreRow => [Seq, JsonBytes]);
composite_safe!(Generations => [u64]);
composite_safe!(AttemptId => [u64]);
composite_safe!(OpId => [u64]);
composite_safe!(StreamAttempt => [u64]);
composite_safe!(SegmentTransition => [Option<u32>, u32, Baseline, u64, Option<u64>, DateTime<Utc>]);
composite_safe!(WindowBudget => [usize, usize, u64]);
composite_safe!(WindowInterest => [Vec<EntryKey>, Option<(u32, Order, EntryKey)>, u64, usize]);
composite_safe!(ExpectedHead => [u64]);
composite_safe!(BoundaryAt => [u32, Option<(Order, EntryKey)>, Boundary]);
composite_safe!(PageToken => [Generations, u64, (u32, Order, EntryKey)]);
composite_safe!(Placement => [EntryKey, u32, Order, Revision]);
composite_safe!(CommitResult => [
    ExpectedHead,
    u64,
    Vec<Placement>,
    Vec<Stored<JsonBytes>>,
    Vec<EntryKey>,
    Vec<(EntryKey, EntryKey)>,
    Vec<BoundaryAt>,
]);
composite_safe!(FleetSnapshot => [HostId, u64, Vec<(Agent, u64)>]);
composite_safe!(FleetDelta => [
    HostEntry,
    HostId,
    bool,
    FleetSnapshot,
    Agent,
    u64,
    AgentId,
    Option<String>,
    SummaryEnvelope,
    Progress,
]);

impl<T: PostcardSafe> private::Sealed for Option<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<T>();
    }
}
impl<T: PostcardSafe> PostcardSafe for Option<T> {}

impl<T: PostcardSafe> private::Sealed for Vec<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<T>();
    }
}
impl<T: PostcardSafe> PostcardSafe for Vec<T> {}

impl<A: PostcardSafe, B: PostcardSafe> private::Sealed for (A, B) {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<A>();
        assert_postcard_safe::<B>();
    }
}
impl<A: PostcardSafe, B: PostcardSafe> PostcardSafe for (A, B) {}

impl<A: PostcardSafe, B: PostcardSafe, C: PostcardSafe> private::Sealed for (A, B, C) {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<A>();
        assert_postcard_safe::<B>();
        assert_postcard_safe::<C>();
    }
}
impl<A: PostcardSafe, B: PostcardSafe, C: PostcardSafe> PostcardSafe for (A, B, C) {}

impl<T: PostcardSafe> private::Sealed for Patch<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<T>();
        assert_postcard_safe::<Revision>();
    }
}
impl<T: PostcardSafe> PostcardSafe for Patch<T> {}

impl<T: PostcardSafe> private::Sealed for VersionedField<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<T>();
        assert_postcard_safe::<Revision>();
    }
}
impl<T: PostcardSafe> PostcardSafe for VersionedField<T> {}

composite_safe!(ComponentSource => [u64, u16, String]);

impl<T: PostcardSafe> private::Sealed for Component<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<ComponentSource>();
        assert_postcard_safe::<u64>();
        assert_postcard_safe::<Vec<ComponentSource>>();
        assert_postcard_safe::<T>();
    }
}
impl<T: PostcardSafe> PostcardSafe for Component<T> {}

impl<T: PostcardSafe> private::Sealed for Components<T> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<Component<T>>();
        assert_postcard_safe::<Revision>();
        assert_postcard_safe::<u64>();
        assert_postcard_safe::<bool>();
    }
}
impl<T: PostcardSafe> PostcardSafe for Components<T> {}

impl<E: Entry> private::Sealed for Mutation<E> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<EntryKey>();
        assert_postcard_safe::<Order>();
        assert_postcard_safe::<Revision>();
        assert_postcard_safe::<E::Partial>();
        assert_postcard_safe::<Promotion>();
    }
}
impl<E: Entry> PostcardSafe for Mutation<E> {}

impl<E: Entry> private::Sealed for MutationGroup<E> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<EntryKey>();
        assert_postcard_safe::<Mutation<E>>();
    }
}
impl<E: Entry> PostcardSafe for MutationGroup<E> {}

impl<E: Entry> private::Sealed for Changes<E> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<Summary>();
        assert_postcard_safe::<Mutation<E>>();
    }
}
impl<E: Entry> PostcardSafe for Changes<E> {}

impl<E: PostcardSafe> private::Sealed for Stored<E> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<EntryKey>();
        assert_postcard_safe::<Order>();
        assert_postcard_safe::<Revision>();
        assert_postcard_safe::<E>();
    }
}
impl<E: PostcardSafe> PostcardSafe for Stored<E> {}

impl<E: PostcardSafe> private::Sealed for Page<E> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<Stored<E>>();
        assert_postcard_safe::<BoundaryAt>();
        assert_postcard_safe::<PageToken>();
    }
}
impl<E: PostcardSafe> PostcardSafe for Page<E> {}

impl<F: ProviderFold> private::Sealed for Head<F> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<F>();
        assert_postcard_safe::<Summary>();
        assert_postcard_safe::<Baseline>();
    }
}
impl<F: ProviderFold> PostcardSafe for Head<F> {}

impl<F: ProviderFold> private::Sealed for HeadState<F> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<Head<F>>();
        assert_postcard_safe::<BaselineReason>();
    }
}
impl<F: ProviderFold> PostcardSafe for HeadState<F> {}

impl<F: ProviderFold> private::Sealed for Loaded<F> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<Generations>();
        assert_postcard_safe::<HeadState<F>>();
        assert_postcard_safe::<Stored<F::Entry>>();
        assert_postcard_safe::<BoundaryAt>();
        assert_postcard_safe::<PageToken>();
        assert_postcard_safe::<SummaryEnvelope>();
        assert_postcard_safe::<Progress>();
    }
}
impl<F: ProviderFold> PostcardSafe for Loaded<F> {}

impl<F: ProviderFold> private::Sealed for CommitOutcome<F> {
    fn assert_fields_are_postcard_safe() {
        assert_postcard_safe::<CommitResult>();
        assert_postcard_safe::<Loaded<F>>();
        assert_postcard_safe::<StoreError>();
    }
}
impl<F: ProviderFold> PostcardSafe for CommitOutcome<F> {}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{TimeZone, Utc};
    use model::{
        AgentKind, ClaudeDriver, ContextMeter, ContextMeterSource, HostTrustStatus, SummaryField,
        TodoProgress,
    };
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct TestEntry {
        description: VersionedField<String>,
        status: VersionedField<String>,
        components: Components<String>,
        promoted: bool,
        clipped: bool,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct TestPartial {
        description: FieldPatch<String>,
        status: FieldPatch<String>,
        components: Vec<Component<String>>,
        final_components: Option<(Seq, Revision, Vec<Component<String>>)>,
    }

    composite_safe!(TestEntry => [
        VersionedField<String>,
        Components<String>,
        bool,
    ]);
    composite_safe!(TestPartial => [
        FieldPatch<String>,
        Vec<Component<String>>,
        Option<(Seq, Revision, Vec<Component<String>>)>,
    ]);

    impl Entry for TestEntry {
        type Partial = TestPartial;

        fn kind(&self) -> &'static str {
            "test"
        }

        fn text(&self) -> Option<&str> {
            self.description.value().map(String::as_str)
        }

        fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect> {
            self.description.merge("description", &patch.description)?;
            self.status.merge("status", &patch.status)?;
            for component in &patch.components {
                self.components.merge(component.clone())?;
            }
            if let Some((through, revision, replacement)) = &patch.final_components {
                self.components
                    .replace_final(*through, *revision, replacement.clone())?;
            }
            Ok(())
        }

        fn from_partial(patch: &Self::Partial) -> Result<Self, MergeDefect> {
            let mut entry = Self::default();
            entry.merge(patch)?;
            Ok(entry)
        }

        fn merge_alias(
            &mut self,
            source: &Self,
            promotion: Option<Promotion>,
        ) -> Result<(), MergeDefect> {
            self.description.fill_unknown_from(&source.description);
            self.status.fill_unknown_from(&source.status);
            for component in source.components.values() {
                self.components.merge(component.clone())?;
            }
            self.promote(promotion)
        }

        fn promote(&mut self, promotion: Option<Promotion>) -> Result<(), MergeDefect> {
            self.promoted |= promotion == Some(Promotion::ToolToTask);
            Ok(())
        }

        fn clip(&mut self, budget: usize) {
            self.components.clip_by(
                ENTRY_MAX_COMPONENTS,
                budget.saturating_sub(32),
                |component| component.value.len() + 24,
            );
            self.clipped |= self.components.is_clipped();
            while self.bytes() > budget {
                if self
                    .description
                    .value_mut()
                    .is_some_and(|description| description.pop().is_some())
                {
                    self.clipped = true;
                    continue;
                }
                if self
                    .status
                    .value_mut()
                    .is_some_and(|status| status.pop().is_some())
                {
                    self.clipped = true;
                    continue;
                }
                break;
            }
        }

        fn bytes(&self) -> usize {
            32 + self.description.value().map_or(0, String::len)
                + self.status.value().map_or(0, String::len)
                + self
                    .components
                    .values()
                    .iter()
                    .map(|component| component.value.len() + 24)
                    .sum::<usize>()
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct TestFold {
        through: Seq,
    }

    leaf_safe!(TestFold);

    impl ProviderFold for TestFold {
        type Entry = TestEntry;

        const PROTOCOL: StructuredProtocol = StructuredProtocol::ClaudePtyTranscript;
        const ENTRY_VERSION: u32 = 1;
        const TIP_VERSION: u32 = 1;
        const TIP_BUDGET: usize = 1024;

        fn begin(&mut self, _segment: SegmentId, _baseline: Baseline) {}

        fn apply(&mut self, input: Input<'_>) -> Changes<Self::Entry> {
            if let Input::Row { seq, .. } = input {
                self.through = seq;
            }
            Changes {
                summary: None,
                through: self.through,
                mutations: Vec::new(),
            }
        }

        fn summary(&self) -> Summary {
            sample_summary()
        }

        fn tip_bytes(&self) -> usize {
            std::mem::size_of::<Self>()
        }
    }

    fn at(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, second)
            .single()
            .expect("valid timestamp")
    }

    fn key(value: &str) -> EntryKey {
        EntryKey::new(value).expect("short test key")
    }

    fn sample_summary() -> Summary {
        Summary {
            attention: model::Attention::NeedsYou {
                why: model::Why::Question,
            },
            phase: model::AgentPhase::Running,
            last_activity: Some(at(1)),
            todo: Some(TodoProgress {
                done: 1,
                total: 2,
                current: Some("verify".into()),
            }),
            context: Some(ContextMeter {
                used_tokens: 25,
                window_tokens: Some(100),
                source: ContextMeterSource::AssistantUsage,
            }),
            model: Some("test-model".into()),
            unknown: vec![SummaryField::Outstanding],
        }
    }

    #[test]
    fn agent_fold_exposes_tip_bytes_for_every_protocol() {
        for protocol in [
            StructuredProtocol::ClaudePtyTranscript,
            StructuredProtocol::ClaudeSdk,
            StructuredProtocol::Codex,
        ] {
            let fold = AgentFold::for_protocol(protocol);
            assert!(fold.tip_bytes() <= TIP_MAX_BYTES, "{protocol:?}");
        }
    }

    #[test]
    fn summary_selection_uses_position_host_ties_and_unknown_fill_only() {
        let mut host_summary = sample_summary();
        host_summary.attention = Attention::Working;
        host_summary.todo = None;
        host_summary.unknown = vec![SummaryField::Todo, SummaryField::Model];
        let host = SummaryEnvelope {
            through: 10,
            producer_version: 7,
            observed_at: at(10),
            stale: true,
            revision: 12,
            summary: host_summary,
        };

        let mut local_summary = sample_summary();
        local_summary.attention = Attention::Idle;
        local_summary.todo = Some(TodoProgress {
            done: 0,
            total: 0,
            current: None,
        });
        local_summary.model = None;
        local_summary.unknown = vec![SummaryField::Context, SummaryField::Model];

        let tied = select_summary(Some(&host), Some(&(10, local_summary.clone())), 7)
            .expect("host tie is selected");
        assert_eq!(tied.through, 10);
        assert_eq!(tied.summary.attention, Attention::Working);
        assert_eq!(tied.summary.todo, local_summary.todo);
        assert!(
            !tied.summary.unknown.contains(&SummaryField::Todo),
            "a known empty todo is knowledge and fills the host"
        );
        assert!(tied.summary.unknown.contains(&SummaryField::Model));
        assert!(tied.stale);
        assert!(!tied.incompatible);

        let ahead = select_summary(Some(&host), Some(&(11, local_summary.clone())), 7)
            .expect("local fold is selected when ahead");
        assert_eq!(ahead.through, 11);
        assert_eq!(ahead.summary.attention, Attention::Idle);
        assert_eq!(ahead.summary.context, host.summary.context);
        assert!(!ahead.summary.unknown.contains(&SummaryField::Context));
        assert!(!ahead.stale);
    }

    #[test]
    fn incompatible_host_summary_is_unknown_with_envelope_age() {
        let host = SummaryEnvelope {
            through: 20,
            producer_version: 8,
            observed_at: at(20),
            stale: true,
            revision: 30,
            summary: sample_summary(),
        };

        let effective = select_summary(Some(&host), None, 7).expect("excluded summary is visible");
        assert_eq!(effective.summary.attention, Attention::Unknown);
        assert_eq!(effective.observed_at, Some(at(20)));
        assert!(effective.incompatible);
        assert!(!effective.stale);

        let local = (19, sample_summary());
        let effective = select_summary(Some(&host), Some(&local), 7)
            .expect("compatible local summary replaces excluded host");
        assert_eq!(effective.through, 19);
        assert_eq!(effective.summary, local.1);
        assert!(!effective.incompatible);
    }

    fn sample_agent() -> Agent {
        Agent {
            id: AgentId::from_u128(1),
            host_id: HostId::from_u128(2),
            name: Some("agent".into()),
            command: "test".into(),
            working_dir: PathBuf::from("/tmp/amux-fold-test"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            readonly: false,
            args: vec!["--fixture".into()],
            created_at: at(2),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        }
    }

    fn sample_host() -> HostEntry {
        HostEntry {
            id: HostId::from_u128(2),
            name: "host".into(),
            online: true,
            version: Some("1.0.0".into()),
            capabilities: None,
            trust_status: HostTrustStatus::Trusted,
            last_dial_error: None,
            platform: Some("macos".into()),
        }
    }

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + DeserializeOwned,
    {
        let encoded = postcard::to_allocvec(value).expect("postcard encode");
        let decoded: T = postcard::from_bytes(&encoded).expect("postcard decode");
        let reencoded = postcard::to_allocvec(&decoded).expect("postcard re-encode");
        assert_eq!(encoded, reencoded);
    }

    fn roundtrip_input(value: &Input<'_>) {
        let encoded = postcard::to_allocvec(value).expect("postcard encode");
        let decoded: Input<'_> = postcard::from_bytes(&encoded).expect("postcard decode");
        assert_eq!(*value, decoded);
    }

    fn revision(seq: Seq) -> Revision {
        Revision::row(seq)
    }

    fn order(seq: Seq, slot: u16) -> Order {
        Order::new(seq, slot).expect("valid test order")
    }

    fn component(seq: Seq, text: &str) -> Component<String> {
        Component {
            source: ComponentSource::Sequence { seq, slot: 0 },
            observed_at: seq,
            after: Vec::new(),
            value: text.to_owned(),
        }
    }

    fn upsert(key_value: &str, seq: Seq, entry: TestPartial) -> Mutation<TestEntry> {
        Mutation::Upsert {
            key: key(key_value),
            order: order(seq, 0),
            revision: revision(seq),
            entry,
        }
    }

    #[test]
    fn complete_vocabulary_round_trips_through_postcard() {
        let segment: SegmentId = 1;
        roundtrip(&segment);

        for input in [
            Input::Row {
                seq: 1,
                published_at: at(1),
                activity_at: Some(at(0)),
                historical: true,
                payload: br#"{"type":"test"}"#,
            },
            Input::ReplayComplete {
                through: 1,
                at: at(2),
            },
            Input::ProcessExited {
                exit_code: Some(0),
                at: at(3),
            },
            Input::ObserverLost { at: at(4) },
            Input::Tick { now: at(5) },
        ] {
            roundtrip_input(&input);
        }

        for baseline in [
            Baseline::Start,
            Baseline::Truncated { from: 2 },
            Baseline::Gap { after: 3 },
            Baseline::VersionGap { after: 4 },
        ] {
            roundtrip(&baseline);
        }

        roundtrip(&key("msg:native"));
        roundtrip(&Order { seq: 4, slot: 2 });
        roundtrip(&Revision {
            seq: 4,
            fence: 5,
            ordinal: 6,
        });
        roundtrip(&Patch::<String>::Unchanged);
        roundtrip(&Patch::set("value".to_string(), revision(3)));
        roundtrip(&Patch::<String>::clear(revision(3)));
        roundtrip(&FieldPatch::set("value".to_string(), revision(4)));
        roundtrip(&FieldPatch::<String>::clear(revision(5)));
        roundtrip(&VersionedField::<String>::default());
        roundtrip(&component(4, "delta"));
        let mut components = Components::default();
        components
            .merge(component(4, "delta"))
            .expect("component merge");
        roundtrip(&components);
        roundtrip(&Promotion::ToolToTask);
        roundtrip(&MergeDefect::EqualRevisionDisagreement {
            field: "status".into(),
            revision: Revision {
                seq: 1,
                fence: 2,
                ordinal: 3,
            },
        });

        let revision = Revision {
            seq: 9,
            fence: 10,
            ordinal: 0,
        };
        let mutations = vec![
            Mutation::<TestEntry>::Upsert {
                key: key("msg:1"),
                order: Order { seq: 9, slot: 0 },
                revision,
                entry: TestPartial {
                    description: FieldPatch::set("hello".into(), revision),
                    ..TestPartial::default()
                },
            },
            Mutation::Delete {
                key: key("msg:2"),
                revision,
            },
            Mutation::Alias {
                from: key("tool:1"),
                to: key("task:1"),
                revision,
                promote: Some(Promotion::ToolToTask),
            },
        ];
        for mutation in &mutations {
            roundtrip(mutation);
        }
        let groups = coalesce(&mutations, &[]).expect("coalesced sample mutations");
        for group in &groups {
            roundtrip(group);
        }
        roundtrip(&Changes::<TestEntry> {
            summary: Some(sample_summary()),
            through: 9,
            mutations,
        });

        // The closed fold has no inhabitant until a provider lands.
        roundtrip(&Option::<AgentFold>::None);
        roundtrip(&JsonBytes(br#"{"opaque":true}"#.to_vec()));

        let generations = Generations {
            fleet: 1,
            chat: 2,
            provider: 3,
        };
        roundtrip(&generations);
        roundtrip(&AttemptId(1));
        roundtrip(&OpId(2));
        roundtrip(&StreamAttempt(3));

        let head = Head::<TestFold> {
            segment: 1,
            baseline: Baseline::Start,
            through: 9,
            tip_version: 1,
            entry_version: 1,
            tip: TestFold { through: 9 },
            summary: sample_summary(),
            observed_at: at(6),
        };
        roundtrip(&head);
        roundtrip(&SegmentTransition {
            predecessor: Some(1),
            successor: 2,
            baseline: Baseline::Gap { after: 9 },
            previous_through: 9,
            selected_from: Some(12),
            replay_through: 20,
            opened_at: at(7),
        });
        for expected in [
            ExpectedHead::Absent { fence: 1 },
            ExpectedHead::Present {
                fence: 1,
                version: 2,
            },
        ] {
            roundtrip(&expected);
        }
        for boundary in [
            Boundary::Truncated,
            Boundary::Gap,
            Boundary::VersionGap,
            Boundary::Evicted,
        ] {
            roundtrip(&boundary);
        }

        let boundary = BoundaryAt {
            segment: 1,
            before: Some((Order { seq: 9, slot: 0 }, key("msg:1"))),
            boundary: Boundary::Gap,
        };
        let stored = Stored {
            key: key("msg:1"),
            segment: 1,
            order: Order { seq: 9, slot: 0 },
            revision,
            entry: TestEntry::from_partial(&TestPartial {
                description: FieldPatch::set("hello".into(), revision),
                ..TestPartial::default()
            })
            .expect("valid sample entry"),
        };
        roundtrip(&boundary);
        roundtrip(&stored);

        let token = PageToken {
            generations,
            content_revision: 4,
            view_epoch: 5,
            before: (1, Order { seq: 9, slot: 0 }, key("msg:1")),
        };
        roundtrip(&token);
        roundtrip(&Page {
            entries: vec![stored.clone()],
            boundaries: vec![boundary.clone()],
            next: Some(token.clone()),
            content_revision: 4,
        });
        roundtrip(&WindowBudget {
            max_entries: 400,
            max_bytes: 16 * 1024 * 1024,
            view_epoch: 5,
        });
        roundtrip(&WindowInterest {
            held_keys: vec![key("msg:1")],
            feed_from: Some((1, Order { seq: 9, slot: 0 }, key("msg:1"))),
            view_epoch: 5,
            result_max_bytes: 8 * 1024 * 1024,
        });

        roundtrip(&HeadState::Usable(1, head));
        for reason in [
            BaselineReason::TipVersion,
            BaselineReason::Corrupt,
            BaselineReason::First,
        ] {
            roundtrip(&reason);
            roundtrip(&HeadState::<TestFold>::NeedsBaseline {
                previous_through: 9,
                reason,
            });
        }
        roundtrip(&HeadState::<TestFold>::None);

        let envelope = SummaryEnvelope {
            through: 9,
            producer_version: 1,
            observed_at: at(8),
            stale: false,
            revision: 10,
            summary: sample_summary(),
        };
        let progress = Progress {
            through: 9,
            at: at(8),
            revision: 11,
        };
        let loaded = Loaded::<TestFold> {
            generations,
            fence: 3,
            content_revision: 4,
            segment_high_water: 1,
            head: HeadState::None,
            window: vec![stored],
            boundaries: vec![boundary.clone()],
            first_page: Some(token),
            aliases: vec![(key("old:1"), key("msg:1"))],
            host: Some(envelope.clone()),
            progress: Some(progress.clone()),
        };
        roundtrip(&loaded);

        let placement = Placement {
            key: key("msg:1"),
            segment: 1,
            order: Order { seq: 9, slot: 0 },
            revision,
        };
        roundtrip(&placement);
        let result = CommitResult {
            expected: ExpectedHead::Present {
                fence: 3,
                version: 2,
            },
            content_revision: 5,
            placed: vec![placement],
            bodies: vec![Stored {
                key: key("msg:1"),
                segment: 1,
                order: Order { seq: 9, slot: 0 },
                revision,
                entry: JsonBytes(b"body".to_vec()),
            }],
            deleted: vec![key("msg:2")],
            redirected: vec![(key("old:1"), key("msg:1"))],
            boundaries: vec![boundary],
        };
        roundtrip(&result);
        roundtrip(&CommitOutcome::<TestFold>::Committed(result));
        roundtrip(&CommitOutcome::<TestFold>::Conflict(loaded));
        for error in [
            StoreError::Busy,
            StoreError::DiskFull,
            StoreError::Permission,
            StoreError::Io,
            StoreError::Invalid,
            StoreError::Corrupt,
            StoreError::UnsupportedFormat,
            StoreError::GenerationMoved,
            StoreError::RecoveryRequired,
            StoreError::OverBudget,
        ] {
            roundtrip(&error);
            roundtrip(&CommitOutcome::<TestFold>::Refused(error));
        }

        let snapshot = FleetSnapshot {
            host_id: HostId::from_u128(2),
            through_revision: 12,
            agents: vec![(sample_agent(), 11)],
        };
        roundtrip(&snapshot);
        for delta in [
            FleetDelta::Host {
                host: sample_host(),
                revision: 10,
            },
            FleetDelta::Reachability {
                host_id: HostId::from_u128(2),
                online: false,
            },
            FleetDelta::HostRemoved {
                host_id: HostId::from_u128(2),
            },
            FleetDelta::Snapshot(snapshot),
            FleetDelta::AgentUp {
                agent: sample_agent(),
                revision: 13,
            },
            FleetDelta::AgentUpdated {
                agent: sample_agent(),
                revision: 14,
            },
            FleetDelta::AgentDown {
                host_id: HostId::from_u128(2),
                agent_id: AgentId::from_u128(1),
                revision: 15,
                reason: Some("finished".into()),
            },
            FleetDelta::Summary {
                host_id: HostId::from_u128(2),
                agent_id: AgentId::from_u128(1),
                envelope,
            },
            FleetDelta::Progress {
                host_id: HostId::from_u128(2),
                agent_id: AgentId::from_u128(1),
                progress,
            },
        ] {
            roundtrip(&delta);
        }
        roundtrip(&Membership::Cached);
        roundtrip(&Membership::Absent);
    }

    #[test]
    fn persisted_graph_is_compile_time_postcard_safe() {
        fn assert_safe<T: PostcardSafe>() {}

        assert_safe::<TestEntry>();
        assert_safe::<TestPartial>();
        assert_safe::<TestFold>();
        assert_safe::<FieldPatch<String>>();
        assert_safe::<VersionedField<String>>();
        assert_safe::<Component<String>>();
        assert_safe::<Components<String>>();
        assert_safe::<Mutation<TestEntry>>();
        assert_safe::<MutationGroup<TestEntry>>();
        assert_safe::<Changes<TestEntry>>();
        assert_safe::<AgentFold>();
        assert_safe::<JsonBytes>();
        assert_safe::<Head<TestFold>>();
        assert_safe::<SegmentTransition>();
        assert_safe::<ExpectedHead>();
        assert_safe::<BoundaryAt>();
        assert_safe::<Stored<TestEntry>>();
        assert_safe::<PageToken>();
        assert_safe::<Page<TestEntry>>();
        assert_safe::<WindowBudget>();
        assert_safe::<WindowInterest>();
        assert_safe::<HeadState<TestFold>>();
        assert_safe::<Loaded<TestFold>>();
        assert_safe::<Placement>();
        assert_safe::<CommitResult>();
        assert_safe::<CommitOutcome<TestFold>>();
        assert_safe::<StoreError>();
        assert_safe::<FleetSnapshot>();
        assert_safe::<FleetDelta>();
        assert_safe::<Membership>();
    }

    #[test]
    fn entry_key_rejects_more_than_512_encoded_bytes() {
        let max_ascii = "a".repeat(ENTRY_KEY_MAX_BYTES);
        assert_eq!(
            EntryKey::new(max_ascii.clone()).unwrap().as_str(),
            max_ascii
        );

        let over_ascii = "a".repeat(ENTRY_KEY_MAX_BYTES + 1);
        assert_eq!(
            EntryKey::new(over_ascii).unwrap_err(),
            EntryKeyTooLong {
                encoded_bytes: ENTRY_KEY_MAX_BYTES + 1,
            }
        );

        let unicode = "é".repeat(ENTRY_KEY_MAX_BYTES / 2 + 1);
        assert_eq!(
            EntryKey::new(unicode).unwrap_err(),
            EntryKeyTooLong { encoded_bytes: 514 }
        );
    }

    #[test]
    fn order_rejects_slots_beyond_the_row_budget_in_construction_and_postcard() {
        assert_eq!(order(1, ORDER_SLOT_MAX).slot(), ORDER_SLOT_MAX);
        assert_eq!(
            Order::new(1, ORDER_SLOT_MAX + 1).unwrap_err(),
            OrderSlotTooLarge {
                slot: ORDER_SLOT_MAX + 1
            }
        );

        let encoded = postcard::to_allocvec(&OrderRepr {
            seq: 1,
            slot: ORDER_SLOT_MAX + 1,
        })
        .expect("encode unchecked representation");
        assert!(postcard::from_bytes::<Order>(&encoded).is_err());
    }

    #[test]
    fn fields_merge_independently_by_revision_and_clear_records_knowledge() {
        let mut entry = TestEntry::default();
        entry
            .merge(&TestPartial {
                description: FieldPatch::set("draft".into(), revision(10)),
                ..TestPartial::default()
            })
            .unwrap();
        entry
            .merge(&TestPartial {
                status: FieldPatch::set("done".into(), revision(30)),
                ..TestPartial::default()
            })
            .unwrap();
        entry
            .merge(&TestPartial {
                description: FieldPatch::set("revised".into(), revision(20)),
                ..TestPartial::default()
            })
            .unwrap();

        assert_eq!(
            entry.description.value().map(String::as_str),
            Some("revised")
        );
        assert_eq!(entry.description.revision(), Some(revision(20)));
        assert_eq!(entry.status.value().map(String::as_str), Some("done"));
        assert_eq!(entry.status.revision(), Some(revision(30)));

        let clear = TestPartial {
            description: FieldPatch::clear(revision(40)),
            ..TestPartial::default()
        };
        entry.merge(&clear).unwrap();
        entry.merge(&clear).unwrap();
        assert!(entry.description.is_known());
        assert_eq!(entry.description.value(), None);

        let defect = entry
            .merge(&TestPartial {
                description: FieldPatch::set("disagrees".into(), revision(40)),
                ..TestPartial::default()
            })
            .unwrap_err();
        assert_eq!(
            defect,
            MergeDefect::EqualRevisionDisagreement {
                field: "description".into(),
                revision: revision(40),
            }
        );
    }

    #[test]
    fn components_union_idempotently_and_final_replacement_supersedes_deltas() {
        let mut components = Components::default();
        assert!(components.merge(component(10, "hel")).unwrap());
        assert!(!components.merge(component(10, "hel")).unwrap());
        assert_eq!(components.values().len(), 1);

        let final_component = Component {
            source: ComponentSource::Native {
                id: "final-row".into(),
                slot: 0,
            },
            observed_at: 20,
            after: Vec::new(),
            value: "hello".into(),
        };
        assert!(
            components
                .replace_final(20, revision(20), vec![final_component.clone()])
                .unwrap()
        );
        assert!(
            !components
                .replace_final(20, revision(20), vec![final_component])
                .unwrap()
        );
        assert_eq!(components.values().len(), 1);
        assert_eq!(components.values()[0].value, "hello");
        assert!(!components.merge(component(12, "stale")).unwrap());
    }

    #[test]
    fn coalescing_keeps_compound_alias_work_in_one_ordered_group() {
        let mutations = vec![
            upsert(
                "tool:1",
                10,
                TestPartial {
                    description: FieldPatch::set("launch".into(), revision(10)),
                    ..TestPartial::default()
                },
            ),
            Mutation::Alias {
                from: key("tool:1"),
                to: key("task:1"),
                revision: revision(20),
                promote: Some(Promotion::ToolToTask),
            },
            upsert(
                "task:1",
                20,
                TestPartial {
                    status: FieldPatch::set("running".into(), revision(20)),
                    ..TestPartial::default()
                },
            ),
        ];

        let groups = coalesce(&mutations, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical, key("task:1"));
        assert_eq!(groups[0].mutations.len(), 3);
    }

    #[test]
    fn oracle_enforces_tombstones_identity_alias_placement_and_cycles() {
        let mut oracle = MutationOracle::<TestEntry>::default();
        oracle
            .apply(&[upsert(
                "tool:1",
                10,
                TestPartial {
                    description: FieldPatch::set("launch".into(), revision(10)),
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        oracle
            .apply(&[upsert(
                "task:1",
                20,
                TestPartial {
                    status: FieldPatch::set("running".into(), revision(20)),
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        oracle
            .apply(&[Mutation::Alias {
                from: key("tool:1"),
                to: key("task:1"),
                revision: revision(30),
                promote: Some(Promotion::ToolToTask),
            }])
            .unwrap();

        let entries = oracle.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, key("task:1"));
        assert_eq!(entries[0].order, order(20, 0), "target placement wins");
        assert_eq!(
            entries[0].entry.description.value().map(String::as_str),
            Some("launch")
        );
        assert_eq!(
            entries[0].entry.status.value().map(String::as_str),
            Some("running")
        );
        assert!(entries[0].entry.promoted);

        let before_cycle = oracle.entries();
        assert_eq!(
            oracle
                .apply(&[Mutation::Alias {
                    from: key("task:1"),
                    to: key("tool:1"),
                    revision: revision(31),
                    promote: None,
                }])
                .unwrap_err(),
            MergeDefect::AliasCycle {
                from: key("task:1"),
                to: key("tool:1"),
            }
        );
        assert_eq!(
            oracle.entries(),
            before_cycle,
            "defects roll back atomically"
        );

        oracle
            .apply(&[Mutation::Delete {
                key: key("task:1"),
                revision: revision(40),
            }])
            .unwrap();
        oracle
            .apply(&[upsert(
                "tool:1",
                40,
                TestPartial {
                    status: FieldPatch::set("stale".into(), revision(40)),
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        assert!(oracle.entries().is_empty(), "delete wins at equal revision");
        oracle
            .apply(&[upsert(
                "tool:1",
                41,
                TestPartial {
                    status: FieldPatch::set("revived".into(), revision(41)),
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        assert_eq!(oracle.entries().len(), 1, "strictly newer upsert revives");

        let mut delete_before_alias = MutationOracle::<TestEntry>::default();
        delete_before_alias
            .apply(&[upsert(
                "tool:deleted",
                10,
                TestPartial {
                    description: FieldPatch::set("launch".into(), revision(10)),
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        delete_before_alias
            .apply(&[Mutation::Delete {
                key: key("task:deleted"),
                revision: revision(20),
            }])
            .unwrap();
        delete_before_alias
            .apply(&[Mutation::Alias {
                from: key("tool:deleted"),
                to: key("task:deleted"),
                revision: revision(20),
                promote: Some(Promotion::ToolToTask),
            }])
            .unwrap();
        assert!(
            delete_before_alias.entries().is_empty(),
            "a target delete wins over an equal-revision alias"
        );
    }

    #[test]
    fn oracle_continuation_matches_uninterrupted_materialisation() {
        let prefix = vec![upsert(
            "msg:stable",
            10,
            TestPartial {
                description: FieldPatch::set("first".into(), revision(10)),
                components: vec![component(10, "a")],
                ..TestPartial::default()
            },
        )];
        let suffix = vec![upsert(
            "msg:stable",
            30,
            TestPartial {
                status: FieldPatch::set("complete".into(), revision(30)),
                description: FieldPatch::set("late".into(), revision(20)),
                components: vec![component(10, "a"), component(30, "b")],
                ..TestPartial::default()
            },
        )];

        let mut continued = MutationOracle::<TestEntry>::default();
        continued.apply(&prefix).unwrap();
        let mut restored = continued.clone();
        restored.apply(&suffix).unwrap();

        let mut uninterrupted = MutationOracle::<TestEntry>::default();
        uninterrupted.apply(&prefix).unwrap();
        uninterrupted.apply(&suffix).unwrap();

        assert_eq!(restored.entries(), uninterrupted.entries());
        assert_eq!(restored.redirects(), uninterrupted.redirects());
        assert_eq!(restored.tombstones(), uninterrupted.tombstones());
        assert_eq!(restored.entries()[0].order, order(10, 0));
        assert_eq!(restored.entries()[0].entry.components.values().len(), 2);
    }

    #[test]
    fn lifecycle_revisions_follow_rows_and_keep_accepted_fence_provenance() {
        assert_eq!(
            LifecycleRevisions::new(30, 0).unwrap_err(),
            MergeDefect::InvalidLifecycleFence
        );
        let mut lifecycle = LifecycleRevisions::new(30, 7).unwrap();
        let first = lifecycle.next_revision().unwrap();
        let second = lifecycle.next_revision().unwrap();
        assert!(first > revision(30));
        assert_eq!(first.fence, 7);
        assert_eq!(first.ordinal, 0);
        assert_eq!(second.ordinal, 1);
    }

    #[test]
    fn oracle_clips_whole_entries_and_component_counts_to_the_configured_budget() {
        let mut oracle = MutationOracle::<TestEntry>::new(1, 256);
        let components = (0..300)
            .map(|seq| component(seq, &"x".repeat(32)))
            .collect();
        oracle
            .apply(&[upsert(
                "msg:large",
                1,
                TestPartial {
                    description: FieldPatch::set("y".repeat(512), revision(1)),
                    components,
                    ..TestPartial::default()
                },
            )])
            .unwrap();
        let entry = &oracle.entries()[0].entry;
        assert!(entry.bytes() <= 256);
        assert!(entry.components.values().len() <= ENTRY_MAX_COMPONENTS);
        assert!(entry.clipped);
    }
}
