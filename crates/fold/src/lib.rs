//! Pure observation and transcript folds shared by the daemon and clients.
//!
//! This crate owns deterministic, I/O-free derivation and the value vocabulary
//! exchanged with the SQLite-backed store. Persisted values use postcard's
//! non-self-describing representation; provider JSON crosses this boundary as
//! bytes and is never retained as a JSON value tree.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use model::{
    Agent, AgentId, AgentKind, AgentParent, AgentPhase, Attention, Capabilities, ClaudeDriver,
    ContextMeter, ContextMeterSource, HostEntry, HostId, HostTrustStatus, Progress, Seq,
    StructuredProtocol, Summary, SummaryEnvelope, SummaryField, SupportedAgentType, TodoProgress,
    Why, WorkingOn,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The largest complete encoded entry key accepted by the fold and store.
pub const ENTRY_KEY_MAX_BYTES: usize = 512;

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
pub struct Order {
    pub seq: Seq,
    pub slot: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Revision {
    pub seq: Seq,
    pub fence: ChatRevision,
    pub ordinal: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Patch<T> {
    #[default]
    Unchanged,
    Set(T),
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Promotion {
    ToolToTask,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeDefect {
    EqualRevisionDisagreement { field: String, revision: Revision },
    AliasCycle { from: EntryKey, to: EntryKey },
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
    fn from_partial(patch: &Self::Partial) -> Self;
    fn clip(&mut self, budget: usize);
    fn bytes(&self) -> usize;
}

#[derive(Clone, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AgentFold {}

/// Opaque provider JSON in a persisted value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonBytes(pub Vec<u8>);

#[derive(Clone, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<E> {
    pub entries: Vec<Stored<E>>,
    pub boundaries: Vec<BoundaryAt>,
    pub next: Option<PageToken>,
    pub content_revision: ChatRevision,
}

#[derive(Clone, Serialize, Deserialize)]
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

#[derive(Clone, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreError {
    Busy,
    DiskFull,
    Permission,
    Io,
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
    },
    Reachability {
        host_id: HostId,
        online: bool,
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

leaf_safe!(
    u8,
    u16,
    u32,
    u64,
    usize,
    i32,
    bool,
    String,
    DateTime<Utc>,
    HostId,
    PathBuf,
    Baseline,
    Promotion,
    AgentFold,
    Boundary,
    BaselineReason,
    StoreError,
    Membership,
);

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
composite_safe!(MergeDefect => [String, Revision, EntryKey]);
composite_safe!(JsonBytes => [Vec<u8>]);
composite_safe!(Generations => [u64]);
composite_safe!(AttemptId => [u64]);
composite_safe!(OpId => [u64]);
composite_safe!(StreamAttempt => [u64]);
composite_safe!(SegmentTransition => [Option<u32>, u32, Baseline, u64, Option<u64>, DateTime<Utc>]);
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
    }
}
impl<T: PostcardSafe> PostcardSafe for Patch<T> {}

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
        text: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct TestPartial {
        text: Patch<String>,
    }

    leaf_safe!(TestEntry, TestPartial);

    impl Entry for TestEntry {
        type Partial = TestPartial;

        fn kind(&self) -> &'static str {
            "test"
        }

        fn text(&self) -> Option<&str> {
            Some(&self.text)
        }

        fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect> {
            match &patch.text {
                Patch::Unchanged => {}
                Patch::Set(text) => self.text.clone_from(text),
                Patch::Clear => self.text.clear(),
            }
            Ok(())
        }

        fn from_partial(patch: &Self::Partial) -> Self {
            let mut entry = Self::default();
            entry.merge(patch).expect("test merge cannot fail");
            entry
        }

        fn clip(&mut self, budget: usize) {
            self.text.truncate(budget.min(self.text.len()));
        }

        fn bytes(&self) -> usize {
            self.text.len()
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
        roundtrip(&Patch::Set("value".to_string()));
        roundtrip(&Patch::<String>::Clear);
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
                    text: Patch::Set("hello".into()),
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
            entry: TestEntry {
                text: "hello".into(),
            },
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
            },
            FleetDelta::Reachability {
                host_id: HostId::from_u128(2),
                online: false,
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
        assert_safe::<Mutation<TestEntry>>();
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
}
