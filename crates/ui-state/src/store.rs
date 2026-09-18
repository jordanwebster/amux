//! Pure store protocol and store-backed chat lifecycle.
//!
//! The runtime executes [`StoreOp`] values and returns [`StoreMsg`] values.
//! Attempts fence whole chat lifetimes, operation ids fence individual I/O,
//! and stream attempts fence subscriptions. The reducer owns all semantic
//! state; the worker is deliberately only an executor.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
pub use fold::AttemptId;
use fold::claude_pty::{ClaudeEntry, ClaudeFold};
use fold::claude_sdk::{ClaudeSdkEntry, ClaudeSdkFold};
use fold::codex::{CodexEntry, CodexFold};
use fold::{
    Baseline, BaselineReason, BoundaryAt, ChatRevision, Entry, EntryKey, ExpectedHead, Fleet,
    FleetDelta, Generations, Head, HeadState, Input, JsonBytes, Loaded, Mutation, MutationOracle,
    OpId, Page, PageToken, Placement, ProviderFold, RedirectState, Revision, SegmentId,
    SegmentTransition, StoreError, Stored, StreamAttempt, WindowBudget, WindowInterest,
};
use model::{
    AgentId, AgentMessagePresentation, ReplayFacts, ReplayOutcome, Seq, StructuredProtocol,
};
use serde::{Deserialize, Serialize};

use crate::{Effect, StreamCloseReason, StreamEntry};

pub const PENDING_COMMIT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Desktop scroll cache: roughly two to five dense terminal viewports.
pub const WINDOW_MAX_ENTRIES: usize = 96;
/// The phone does not page yet, so its existing visible-history bound stays
/// separate from the desktop cache dial.
pub const PHONE_WINDOW_MAX_ENTRIES: usize = 800;
pub const WINDOW_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const WINDOW_PAGE_ENTRIES: usize = 96;
pub const COMMIT_RESULT_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const FLUSH_DEADLINE: TimeDelta = TimeDelta::seconds(5);

/// A runtime instance's profile generation. Results from an earlier worker
/// are ignored even when their chat and operation numbers happen to match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileGeneration(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreOpKind {
    Load,
    Commit,
    Page,
    Invalidate,
    FleetLoad,
    FleetApply,
    ViewGet,
    ViewSet,
}

/// Provider-erased fold head. The enum is closed over the protocols amux can
/// render, so mismatched entries fail closed instead of crossing providers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadDto {
    Claude(Head<ClaudeFold>),
    ClaudeSdk(Head<ClaudeSdkFold>),
    Codex(Head<CodexFold>),
}

impl HeadDto {
    pub fn protocol(&self) -> StructuredProtocol {
        match self {
            Self::Claude(_) => StructuredProtocol::ClaudePtyTranscript,
            Self::ClaudeSdk(_) => StructuredProtocol::ClaudeSdk,
            Self::Codex(_) => StructuredProtocol::Codex,
        }
    }

    pub fn through(&self) -> Seq {
        match self {
            Self::Claude(head) => head.through,
            Self::ClaudeSdk(head) => head.through,
            Self::Codex(head) => head.through,
        }
    }

    pub fn summary(&self) -> &model::Summary {
        match self {
            Self::Claude(head) => &head.summary,
            Self::ClaudeSdk(head) => &head.summary,
            Self::Codex(head) => &head.summary,
        }
    }

    fn segment(&self) -> SegmentId {
        match self {
            Self::Claude(head) => head.segment,
            Self::ClaudeSdk(head) => head.segment,
            Self::Codex(head) => head.segment,
        }
    }

    pub(crate) fn agent_fold(&self) -> fold::AgentFold {
        match self {
            Self::Claude(head) => fold::AgentFold::Claude(head.tip.clone()),
            Self::ClaudeSdk(head) => fold::AgentFold::ClaudeSdk(head.tip.clone()),
            Self::Codex(head) => fold::AgentFold::Codex(head.tip.clone()),
        }
    }

    fn apply(&mut self, input: Input<'_>, observed_at: DateTime<Utc>) -> MutationBatchDto {
        macro_rules! apply {
            ($head:expr, $variant:ident) => {{
                let changes = $head.tip.apply(input);
                $head.through = changes.through;
                $head.summary = $head.tip.summary();
                $head.observed_at = observed_at;
                MutationBatchDto::$variant(changes.mutations)
            }};
        }
        match self {
            Self::Claude(head) => apply!(head, Claude),
            Self::ClaudeSdk(head) => apply!(head, ClaudeSdk),
            Self::Codex(head) => apply!(head, Codex),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredDto {
    Claude(Box<Stored<ClaudeEntry>>),
    ClaudeSdk(Box<Stored<ClaudeSdkEntry>>),
    Codex(Box<Stored<CodexEntry>>),
}

impl StoredDto {
    pub fn key(&self) -> &EntryKey {
        match self {
            Self::Claude(value) => &value.key,
            Self::ClaudeSdk(value) => &value.key,
            Self::Codex(value) => &value.key,
        }
    }

    pub fn position(&self) -> (SegmentId, fold::Order, &EntryKey) {
        match self {
            Self::Claude(value) => (value.segment, value.order, &value.key),
            Self::ClaudeSdk(value) => (value.segment, value.order, &value.key),
            Self::Codex(value) => (value.segment, value.order, &value.key),
        }
    }

    /// Provider-neutral facts needed to paint a durable window before a live
    /// provider stream exists. Rich live views may know more, but durable
    /// entries always retain these two presentation facts.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Claude(value) => value.entry.kind(),
            Self::ClaudeSdk(value) => value.entry.kind(),
            Self::Codex(value) => value.entry.kind(),
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Claude(value) => value.entry.text(),
            Self::ClaudeSdk(value) => value.entry.text(),
            Self::Codex(value) => value.entry.text(),
        }
    }

    /// Whether this durable entry is a completed agent report whose body can
    /// be collapsed beyond its first line.
    pub fn has_foldable_completion(&self) -> bool {
        let completed = match self {
            Self::Claude(value) => matches!(
                value.entry.body(),
                Some(fold::claude_pty::ClaudeBody::AgentMessage { kind, .. })
                    if kind.presentation() == AgentMessagePresentation::Finished
            ),
            Self::ClaudeSdk(value) => matches!(
                value.entry.body(),
                Some(fold::claude_sdk::ClaudeSdkBody::AgentMessage { kind, .. })
                    if kind.presentation() == AgentMessagePresentation::Finished
            ),
            Self::Codex(value) => matches!(
                value.entry.body(),
                Some(fold::codex::CodexBody::AgentMessage { kind, .. })
                    if kind.presentation() == AgentMessagePresentation::Finished
            ),
        };
        completed
            && self
                .text()
                .is_some_and(|text| crate::message_digest(text).hidden_lines > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationBatchDto {
    Claude(Vec<Mutation<ClaudeEntry>>),
    ClaudeSdk(Vec<Mutation<ClaudeSdkEntry>>),
    Codex(Vec<Mutation<CodexEntry>>),
}

impl MutationBatchDto {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Claude(values) => values.is_empty(),
            Self::ClaudeSdk(values) => values.is_empty(),
            Self::Codex(values) => values.is_empty(),
        }
    }

    fn encoded_bytes(&self) -> usize {
        postcard::to_allocvec(self).map_or(PENDING_COMMIT_MAX_BYTES + 1, |bytes| bytes.len())
    }

    fn len(&self) -> usize {
        match self {
            Self::Claude(values) => values.len(),
            Self::ClaudeSdk(values) => values.len(),
            Self::Codex(values) => values.len(),
        }
    }

    fn append(&mut self, source: Self) {
        match (self, source) {
            (Self::Claude(left), Self::Claude(mut right)) => left.append(&mut right),
            (Self::ClaudeSdk(left), Self::ClaudeSdk(mut right)) => left.append(&mut right),
            (Self::Codex(left), Self::Codex(mut right)) => left.append(&mut right),
            _ => debug_assert!(false, "cannot combine mutations from different providers"),
        }
    }

    fn apply_to(
        &self,
        entries: &mut Vec<StoredDto>,
        aliases: &mut Vec<(EntryKey, EntryKey)>,
        segment: SegmentId,
        admit_new: bool,
    ) -> Result<(), ReloadRequired> {
        macro_rules! apply {
            ($mutations:expr, $entry_variant:ident, $entry_ty:ty) => {{
                let filtered = if admit_new {
                    std::borrow::Cow::Borrowed($mutations.as_slice())
                } else {
                    std::borrow::Cow::Owned(
                        $mutations
                            .iter()
                            .filter(|mutation| match mutation {
                                Mutation::Upsert { key, .. } => {
                                    let mut current = key;
                                    for _ in 0..=aliases.len() {
                                        if entries.iter().any(|entry| entry.key() == current) {
                                            return true;
                                        }
                                        let Some((_, to)) =
                                            aliases.iter().find(|(from, _)| from == current)
                                        else {
                                            return false;
                                        };
                                        current = to;
                                    }
                                    // Preserve the oracle's fail-closed cycle detection.
                                    true
                                }
                                Mutation::Delete { .. } | Mutation::Alias { .. } => true,
                            })
                            .cloned()
                            .collect(),
                    )
                };
                // A store-backed window admits a fresh key only after SQLite
                // returns its canonical body. Most stream rows take this path;
                // avoid rebuilding the whole materialiser for a known no-op.
                if filtered.is_empty() {
                    return Ok(());
                }
                let concrete = std::mem::take(entries)
                    .into_iter()
                    .map(|stored| match stored {
                        StoredDto::$entry_variant(value) => Ok(*value),
                        _ => Err(ReloadRequired::ProviderMismatch),
                    })
                    .collect::<Result<Vec<Stored<$entry_ty>>, _>>()?;
                let redirects = aliases
                    .iter()
                    .cloned()
                    .map(|(from, to)| RedirectState {
                        from,
                        to,
                        revision: Revision {
                            seq: 0,
                            fence: 0,
                            ordinal: 0,
                        },
                        promote: None,
                    })
                    .collect();
                let mut oracle = MutationOracle::from_state(
                    segment.max(1),
                    fold::DESKTOP_ENTRY_MAX_BYTES,
                    concrete,
                    Vec::new(),
                    redirects,
                )
                .map_err(|_| ReloadRequired::MergeDefect)?;
                oracle
                    .apply(filtered.as_ref())
                    .map_err(|_| ReloadRequired::MergeDefect)?;
                *entries = oracle
                    .entries()
                    .into_iter()
                    .map(|value| StoredDto::$entry_variant(Box::new(value)))
                    .collect();
                *aliases = oracle.redirects();
                Ok(())
            }};
        }
        match self {
            Self::Claude(mutations) => apply!(mutations, Claude, ClaudeEntry),
            Self::ClaudeSdk(mutations) => apply!(mutations, ClaudeSdk, ClaudeSdkEntry),
            Self::Codex(mutations) => apply!(mutations, Codex, CodexEntry),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadedDto {
    Claude(Loaded<ClaudeFold>),
    ClaudeSdk(Loaded<ClaudeSdkFold>),
    Codex(Loaded<CodexFold>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageDto {
    Claude(Page<ClaudeEntry>),
    ClaudeSdk(Page<ClaudeSdkEntry>),
    Codex(Page<CodexEntry>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreOp {
    Load {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        protocol: StructuredProtocol,
        window: WindowBudget,
    },
    Commit {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        generations: Generations,
        expected: ExpectedHead,
        head: Box<HeadDto>,
        transition: Option<SegmentTransition>,
        mutations: MutationBatchDto,
        interest: WindowInterest,
    },
    Page {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        protocol: StructuredProtocol,
        token: PageToken,
        n: usize,
    },
    Invalidate {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        protocol: StructuredProtocol,
        generations: Generations,
        expected: ExpectedHead,
        reason: BaselineReason,
    },
    FleetLoad {
        profile: ProfileGeneration,
        op: OpId,
        generations: Generations,
    },
    FleetApply {
        profile: ProfileGeneration,
        op: OpId,
        generations: Generations,
        delta: Box<FleetDelta>,
    },
    ViewGet {
        profile: ProfileGeneration,
        op: OpId,
        kind: String,
        key: String,
    },
    ViewSet {
        profile: ProfileGeneration,
        op: OpId,
        kind: String,
        key: String,
        value: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreMsg {
    Loaded {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        loaded: Box<LoadedDto>,
    },
    Committed {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        result: fold::CommitResult,
    },
    Conflict {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        loaded: Box<LoadedDto>,
    },
    Paged {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: AgentId,
        page: PageDto,
    },
    Failed {
        profile: ProfileGeneration,
        attempt: AttemptId,
        op: OpId,
        agent: Option<AgentId>,
        kind: StoreOpKind,
        error: StoreError,
    },
    FleetLoaded {
        profile: ProfileGeneration,
        op: OpId,
        fleet: Fleet,
    },
    FleetApplied {
        profile: ProfileGeneration,
        op: OpId,
    },
    FleetChanged {
        profile: ProfileGeneration,
    },
    ViewLoaded {
        profile: ProfileGeneration,
        op: OpId,
        value: Option<String>,
    },
    ViewSet {
        profile: ProfileGeneration,
        op: OpId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatCommand {
    Open { agent: AgentId },
    PageOlder { agent: AgentId, n: usize },
    FollowTip { agent: AgentId },
    Close { agent: AgentId, now: DateTime<Utc> },
    FlushDeadline { agent: AgentId, now: DateTime<Utc> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChatStreamMsg {
    Opened {
        facts: ReplayFactsDto,
        at: DateTime<Utc>,
    },
    Batch {
        at: DateTime<Utc>,
        entries: Vec<StreamEntry>,
    },
    ReplayComplete {
        at: DateTime<Utc>,
    },
    Closed {
        at: DateTime<Utc>,
        reason: StreamCloseReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayFactsDto {
    pub retained_from: Seq,
    pub through: Seq,
    pub selected_from: Seq,
    pub reset_at: Seq,
    pub outcome: ReplayOutcomeDto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayOutcomeDto {
    Continuous,
    Truncated { missing_after: Seq },
    Reset { reason: String },
}

impl From<ReplayFacts> for ReplayFactsDto {
    fn from(value: ReplayFacts) -> Self {
        Self {
            retained_from: value.retained_from,
            through: value.through,
            selected_from: value.selected_from,
            reset_at: value.reset_at,
            outcome: match value.outcome {
                ReplayOutcome::Continuous => ReplayOutcomeDto::Continuous,
                ReplayOutcome::Truncated { missing_after } => {
                    ReplayOutcomeDto::Truncated { missing_after }
                }
                ReplayOutcome::Reset { reason } => ReplayOutcomeDto::Reset { reason },
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreStreamQuery {
    After { after: Seq, tail_bound: Option<u64> },
    TailCount { count: u64, tail_bound: Option<u64> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    Absent,
    Loading,
    Invalidating,
    Painted,
    CatchingUp,
    Live,
    Reloading,
    Flushing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReloadRequired {
    ProviderMismatch,
    MergeDefect,
    CanonicalBody,
    ResultTooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingCommit {
    op: OpId,
    head: HeadDto,
    transition: Option<SegmentTransition>,
    mutations: MutationBatchDto,
    bytes: usize,
    busy_retries: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PageRequest {
    op: OpId,
    view_epoch: u64,
    content_revision: ChatRevision,
    n: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWindow {
    pub state: ChatState,
    pub protocol: StructuredProtocol,
    pub attempt: AttemptId,
    pub stream_attempt: StreamAttempt,
    /// An open request has not answered yet. Stored rows remain painted
    /// while it is pending, so chat state alone cannot suppress a duplicate.
    #[serde(default)]
    pub(crate) stream_opening: bool,
    pub generations: Option<Generations>,
    pub expected: ExpectedHead,
    pub content_revision: ChatRevision,
    pub segment_high_water: SegmentId,
    pub head: Option<HeadDto>,
    pub entries: Vec<StoredDto>,
    pub boundaries: Vec<BoundaryAt>,
    pub aliases: Vec<(EntryKey, EntryKey)>,
    pub first_page: Option<PageToken>,
    pub host: Option<model::SummaryEnvelope>,
    pub progress: Option<model::Progress>,
    pub view_epoch: u64,
    /// The stream's opening time while a replay is still being consumed.
    /// Renderers use it to suppress a distracting catch-up flash.
    #[serde(default)]
    pub catching_up_since: Option<DateTime<Utc>>,
    pub paused: bool,
    pub abandoned_flush: bool,
    pending: Vec<PendingCommit>,
    in_flight: Option<PendingCommit>,
    page_request: Option<PageRequest>,
    transition: Option<SegmentTransition>,
    next_baseline: Option<Baseline>,
    invalidation_previous_through: Option<Seq>,
    invalidation_op: Option<OpId>,
    active_load: Option<OpId>,
    replay_through: Seq,
    flush_deadline: Option<DateTime<Utc>>,
    #[serde(default)]
    retain_oldest: bool,
    #[serde(default)]
    newest_evicted: bool,
    #[serde(default = "desktop_window_entries")]
    max_entries: usize,
}

/// One item of a chat window in reading order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowItem<'a> {
    Entry(&'a StoredDto),
    /// Where the window's history is not continuous: rows that were never
    /// received, a change of entry version, or rows evicted from the store.
    Boundary(&'a BoundaryAt),
}

impl ChatWindow {
    /// The window's entries with each history boundary placed before the
    /// entry it precedes. A boundary at a segment's start precedes that
    /// segment's first entry, and one past every entry comes last.
    pub fn history(&self) -> Vec<WindowItem<'_>> {
        let mut items = Vec::with_capacity(self.entries.len() + self.boundaries.len());
        let mut boundaries = self.boundaries.iter().peekable();
        for entry in &self.entries {
            let (segment, order, key) = entry.position();
            while let Some(boundary) = boundaries.next_if(|boundary| {
                boundary.segment < segment
                    || (boundary.segment == segment
                        && boundary
                            .before
                            .as_ref()
                            .is_none_or(|(before_order, before_key)| {
                                (before_order, before_key) <= (&order, key)
                            }))
            }) {
                items.push(WindowItem::Boundary(boundary));
            }
            items.push(WindowItem::Entry(entry));
        }
        items.extend(boundaries.map(WindowItem::Boundary));
        items
    }

    /// Encoded size governed by the visible-window memory budget.
    pub fn encoded_window_bytes(&self) -> usize {
        postcard::to_allocvec(self.entries.as_slice()).map_or(usize::MAX, |bytes| bytes.len())
    }

    fn loading(protocol: StructuredProtocol, attempt: AttemptId, max_entries: usize) -> Self {
        Self {
            state: ChatState::Loading,
            protocol,
            attempt,
            stream_attempt: StreamAttempt(0),
            stream_opening: false,
            generations: None,
            expected: ExpectedHead::Absent { fence: 0 },
            content_revision: 0,
            segment_high_water: 0,
            head: None,
            entries: Vec::new(),
            boundaries: Vec::new(),
            aliases: Vec::new(),
            first_page: None,
            host: None,
            progress: None,
            view_epoch: 0,
            catching_up_since: None,
            paused: false,
            abandoned_flush: false,
            pending: Vec::new(),
            in_flight: None,
            page_request: None,
            transition: None,
            next_baseline: None,
            invalidation_previous_through: None,
            invalidation_op: None,
            active_load: None,
            replay_through: 0,
            flush_deadline: None,
            retain_oldest: false,
            newest_evicted: false,
            max_entries: max_entries.max(1),
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending.iter().map(|batch| batch.bytes).sum::<usize>()
            + self.in_flight.as_ref().map_or(0, |batch| batch.bytes)
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Encoded ownership inside this store-backed window.
    pub fn retention(&self) -> ChatWindowRetention {
        let encoded =
            |entries: &[StoredDto]| postcard::to_allocvec(entries).map_or(0, |bytes| bytes.len());
        ChatWindowRetention {
            visible_entries: self.entries.len(),
            visible_entry_bytes: encoded(&self.entries),
            canonical_entries: 0,
            canonical_entry_bytes: 0,
            pending_commits: self.pending.len() + usize::from(self.in_flight.is_some()),
            pending_mutations: self
                .pending
                .iter()
                .map(|commit| commit.mutations.len())
                .sum::<usize>()
                + self
                    .in_flight
                    .as_ref()
                    .map_or(0, |commit| commit.mutations.len()),
            pending_mutation_bytes: self.pending_bytes(),
        }
    }

    pub fn is_painted(&self) -> bool {
        matches!(
            self.state,
            ChatState::Painted | ChatState::CatchingUp | ChatState::Live | ChatState::Flushing
        )
    }

    pub fn head_through(&self) -> Option<Seq> {
        self.head.as_ref().map(HeadDto::through)
    }
}

/// Serialized ownership inside one open store-backed chat window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatWindowRetention {
    pub visible_entries: usize,
    pub visible_entry_bytes: usize,
    pub canonical_entries: usize,
    pub canonical_entry_bytes: usize,
    pub pending_commits: usize,
    pub pending_mutations: usize,
    pub pending_mutation_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoreState {
    pub profile: ProfileGeneration,
    pub generations: Option<Generations>,
    pub chats: BTreeMap<AgentId, ChatWindow>,
    pub remembered_chat: Option<AgentId>,
    next_attempt: u64,
    next_op: u64,
    startup_fleet_op: Option<OpId>,
    startup_view_op: Option<OpId>,
    /// The startup fleet read has answered, either with rows or with a failure.
    #[serde(default)]
    pub(crate) fleet_settled: bool,
    #[serde(default = "desktop_window_entries")]
    window_max_entries: usize,
}

impl Default for StoreState {
    fn default() -> Self {
        Self {
            profile: ProfileGeneration::default(),
            generations: None,
            chats: BTreeMap::new(),
            remembered_chat: None,
            next_attempt: 0,
            next_op: 0,
            startup_fleet_op: None,
            startup_view_op: None,
            fleet_settled: false,
            window_max_entries: WINDOW_MAX_ENTRIES,
        }
    }
}

const fn desktop_window_entries() -> usize {
    WINDOW_MAX_ENTRIES
}

fn window_budget(max_entries: usize, view_epoch: u64) -> WindowBudget {
    WindowBudget {
        max_entries,
        max_bytes: WINDOW_MAX_BYTES,
        view_epoch,
    }
}

impl StoreState {
    fn attempt(&mut self) -> AttemptId {
        self.next_attempt = self.next_attempt.saturating_add(1);
        AttemptId(self.next_attempt)
    }

    fn op(&mut self) -> OpId {
        self.next_op = self.next_op.saturating_add(1);
        OpId(self.next_op)
    }

    pub(crate) fn accepts_stream(&self, agent: AgentId, attempt: StreamAttempt) -> bool {
        self.chats
            .get(&agent)
            .is_some_and(|chat| chat.stream_attempt == attempt)
    }
}

pub(crate) fn startup(
    state: &mut StoreState,
    profile: ProfileGeneration,
    generations: Generations,
    window_max_entries: usize,
) -> Vec<Effect> {
    state.profile = profile;
    state.generations = Some(generations);
    state.window_max_entries = window_max_entries.max(1);
    let fleet = state.op();
    let view = state.op();
    state.startup_fleet_op = Some(fleet);
    state.startup_view_op = Some(view);
    vec![
        Effect::Store(StoreOp::FleetLoad {
            profile,
            op: fleet,
            generations,
        }),
        Effect::Store(StoreOp::ViewGet {
            profile,
            op: view,
            kind: "ui".to_owned(),
            key: "remembered_chat".to_owned(),
        }),
    ]
}

pub(crate) fn open_chat(
    state: &mut StoreState,
    agent: AgentId,
    protocol: StructuredProtocol,
) -> Vec<Effect> {
    let attempt = state.attempt();
    let load_op = state.op();
    let view_op = state.op();
    let mut chat = ChatWindow::loading(protocol, attempt, state.window_max_entries);
    chat.active_load = Some(load_op);
    state.chats.insert(agent, chat);
    state.remembered_chat = Some(agent);
    vec![
        Effect::Store(StoreOp::Load {
            profile: state.profile,
            attempt,
            op: load_op,
            agent,
            protocol,
            window: window_budget(state.window_max_entries, 0),
        }),
        Effect::Store(StoreOp::ViewSet {
            profile: state.profile,
            op: view_op,
            kind: "ui".to_owned(),
            key: "remembered_chat".to_owned(),
            value: agent.to_string(),
        }),
    ]
}

pub(crate) fn fleet_apply(state: &mut StoreState, delta: FleetDelta) -> Vec<Effect> {
    let Some(generations) = state.generations else {
        return Vec::new();
    };
    let op = state.op();
    vec![Effect::Store(StoreOp::FleetApply {
        profile: state.profile,
        op,
        generations,
        delta: Box::new(delta),
    })]
}

pub(crate) fn update_store(state: &mut StoreState, msg: StoreMsg) -> StoreUpdate {
    if msg_profile(&msg) != state.profile {
        return StoreUpdate::default();
    }
    match msg {
        StoreMsg::Loaded {
            attempt,
            op,
            agent,
            loaded,
            ..
        } => loaded_result(state, agent, attempt, op, *loaded, false),
        StoreMsg::Conflict {
            attempt,
            op,
            agent,
            loaded,
            ..
        } => conflict_result(state, agent, attempt, op, *loaded),
        StoreMsg::Committed {
            attempt,
            op,
            agent,
            result,
            ..
        } => committed_result(state, agent, attempt, op, result),
        StoreMsg::Paged {
            attempt,
            op,
            agent,
            page,
            ..
        } => paged_result(state, agent, attempt, op, page),
        StoreMsg::Failed {
            attempt,
            op,
            agent,
            kind,
            error,
            ..
        } => failed_result(state, agent, attempt, op, kind, error),
        StoreMsg::FleetLoaded { op, fleet, .. } if state.startup_fleet_op == Some(op) => {
            state.startup_fleet_op = None;
            state.fleet_settled = true;
            StoreUpdate {
                fleet: Some(fleet),
                ..StoreUpdate::default()
            }
        }
        StoreMsg::FleetApplied { .. } => StoreUpdate::default(),
        StoreMsg::FleetChanged { .. } => {
            let Some(generations) = state.generations else {
                return StoreUpdate::default();
            };
            let op = state.op();
            state.startup_fleet_op = Some(op);
            StoreUpdate::effects(vec![Effect::Store(StoreOp::FleetLoad {
                profile: state.profile,
                op,
                generations,
            })])
        }
        StoreMsg::ViewLoaded { op, value, .. } if state.startup_view_op == Some(op) => {
            state.startup_view_op = None;
            state.remembered_chat = value.and_then(|value| value.parse().ok());
            StoreUpdate::default()
        }
        StoreMsg::ViewLoaded { .. } | StoreMsg::ViewSet { .. } => StoreUpdate::default(),
        StoreMsg::FleetLoaded { .. } => StoreUpdate::default(),
    }
}

#[derive(Default)]
pub(crate) struct StoreUpdate {
    pub effects: Vec<Effect>,
    pub fleet: Option<Fleet>,
}

impl StoreUpdate {
    fn effects(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            ..Self::default()
        }
    }
}

fn msg_profile(msg: &StoreMsg) -> ProfileGeneration {
    match msg {
        StoreMsg::Loaded { profile, .. }
        | StoreMsg::Committed { profile, .. }
        | StoreMsg::Conflict { profile, .. }
        | StoreMsg::Paged { profile, .. }
        | StoreMsg::Failed { profile, .. }
        | StoreMsg::FleetLoaded { profile, .. }
        | StoreMsg::FleetApplied { profile, .. }
        | StoreMsg::FleetChanged { profile }
        | StoreMsg::ViewLoaded { profile, .. }
        | StoreMsg::ViewSet { profile, .. } => *profile,
    }
}

fn loaded_result(
    state: &mut StoreState,
    agent: AgentId,
    attempt: AttemptId,
    op: OpId,
    loaded: LoadedDto,
    reloading: bool,
) -> StoreUpdate {
    let valid = state.chats.get(&agent).is_some_and(|chat| {
        chat.attempt == attempt
            && chat.active_load == Some(op)
            && matches!(chat.state, ChatState::Loading | ChatState::Reloading)
    });
    if !valid {
        return StoreUpdate::default();
    }
    let invalidate_op = state.op();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    chat.active_load = None;
    if reloading {
        chat.state = ChatState::Reloading;
    }
    let branch = install_loaded(chat, loaded);
    match branch {
        LoadedBranch::Usable { through } => {
            chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
            chat.state = ChatState::Painted;
            StoreUpdate::effects(vec![open_effect(
                agent,
                chat.protocol,
                chat.stream_attempt,
                StoreStreamQuery::After {
                    after: through,
                    tail_bound: Some(crate::REPLAY_TAIL),
                },
                chat.paused,
            )])
        }
        LoadedBranch::None => {
            chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
            chat.state = ChatState::Painted;
            StoreUpdate::effects(vec![open_effect(
                agent,
                chat.protocol,
                chat.stream_attempt,
                StoreStreamQuery::TailCount {
                    count: crate::REPLAY_TAIL,
                    tail_bound: Some(crate::REPLAY_TAIL),
                },
                chat.paused,
            )])
        }
        LoadedBranch::NeedsBaseline {
            previous_through,
            reason,
        } => {
            chat.state = ChatState::Invalidating;
            chat.invalidation_previous_through = Some(previous_through);
            chat.next_baseline = Some(match reason {
                BaselineReason::TipVersion => Baseline::VersionGap {
                    after: previous_through,
                },
                BaselineReason::Corrupt => Baseline::Gap {
                    after: previous_through,
                },
                BaselineReason::First => Baseline::Start,
            });
            chat.invalidation_op = Some(invalidate_op);
            let Some(generations) = chat.generations else {
                return StoreUpdate::default();
            };
            StoreUpdate::effects(vec![Effect::Store(StoreOp::Invalidate {
                profile: state.profile,
                attempt,
                op: invalidate_op,
                agent,
                protocol: chat.protocol,
                generations,
                expected: chat.expected,
                reason,
            })])
        }
    }
}

enum LoadedBranch {
    Usable {
        through: Seq,
    },
    None,
    NeedsBaseline {
        previous_through: Seq,
        reason: BaselineReason,
    },
}

fn install_loaded(chat: &mut ChatWindow, loaded: LoadedDto) -> LoadedBranch {
    macro_rules! install {
        ($loaded:expr, $head_variant:ident, $entry_variant:ident) => {{
            chat.generations = Some($loaded.generations);
            chat.expected = match &$loaded.head {
                HeadState::Usable(version, _) => ExpectedHead::Present {
                    fence: $loaded.fence,
                    version: *version,
                },
                HeadState::NeedsBaseline { .. } | HeadState::None => ExpectedHead::Absent {
                    fence: $loaded.fence,
                },
            };
            chat.content_revision = $loaded.content_revision;
            chat.segment_high_water = $loaded.segment_high_water;
            chat.boundaries = $loaded.boundaries;
            chat.first_page = $loaded.first_page;
            chat.host = $loaded.host;
            chat.progress = $loaded.progress;
            chat.aliases = $loaded.aliases;
            chat.entries = $loaded
                .window
                .into_iter()
                .map(|value| StoredDto::$entry_variant(Box::new(value)))
                .collect();
            chat.pending.clear();
            chat.in_flight = None;
            chat.transition = None;
            chat.next_baseline = None;
            chat.invalidation_op = None;
            chat.invalidation_previous_through = None;
            chat.page_request = None;
            chat.paused = false;
            chat.catching_up_since = None;
            chat.retain_oldest = false;
            chat.newest_evicted = false;
            chat.view_epoch = chat.view_epoch.saturating_add(1);
            match $loaded.head {
                HeadState::Usable(_, head) => {
                    let through = head.through;
                    chat.head = Some(HeadDto::$head_variant(head));
                    LoadedBranch::Usable { through }
                }
                HeadState::NeedsBaseline {
                    previous_through,
                    reason,
                } => {
                    chat.head = None;
                    LoadedBranch::NeedsBaseline {
                        previous_through,
                        reason,
                    }
                }
                HeadState::None => {
                    chat.head = None;
                    LoadedBranch::None
                }
            }
        }};
    }
    match loaded {
        LoadedDto::Claude(loaded) => install!(loaded, Claude, Claude),
        LoadedDto::ClaudeSdk(loaded) => install!(loaded, ClaudeSdk, ClaudeSdk),
        LoadedDto::Codex(loaded) => install!(loaded, Codex, Codex),
    }
}

/// Whether the authoritative host has observed rows beyond this device's
/// stored head. Missing progress or a missing head cannot claim a lag.
pub fn behind(progress: Option<&model::Progress>, head_through: Option<Seq>) -> bool {
    progress
        .zip(head_through)
        .is_some_and(|(progress, through)| progress.through > through)
}

fn conflict_result(
    state: &mut StoreState,
    agent: AgentId,
    attempt: AttemptId,
    op: OpId,
    loaded: LoadedDto,
) -> StoreUpdate {
    let valid = state.chats.get(&agent).is_some_and(|chat| {
        chat.attempt == attempt
            && (chat.in_flight.as_ref().is_some_and(|batch| batch.op == op)
                || (chat.state == ChatState::Invalidating && chat.invalidation_op == Some(op)))
    });
    if !valid {
        return StoreUpdate::default();
    }
    if state
        .chats
        .get(&agent)
        .is_some_and(|chat| chat.state == ChatState::Flushing)
    {
        abandon_flush(state, agent);
        return StoreUpdate::default();
    }
    let new_attempt = state.attempt();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    chat.state = ChatState::Reloading;
    chat.attempt = new_attempt;
    chat.pending.clear();
    chat.in_flight = None;
    chat.entries.clear();
    chat.active_load = Some(op);
    let mut result = loaded_result(state, agent, new_attempt, op, loaded, true);
    result.effects.insert(0, Effect::CloseStream { agent });
    result
}

fn committed_result(
    state: &mut StoreState,
    agent: AgentId,
    attempt: AttemptId,
    op: OpId,
    result: fold::CommitResult,
) -> StoreUpdate {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    if chat.attempt != attempt {
        return StoreUpdate::default();
    }

    if chat.state == ChatState::Invalidating {
        if chat.invalidation_op != Some(op) {
            return StoreUpdate::default();
        }
        chat.invalidation_op = None;
        chat.expected = result.expected;
        chat.content_revision = result.content_revision;
        chat.boundaries = merge_boundaries(&chat.boundaries, &result.boundaries);
        chat.state = ChatState::Painted;
        chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
        let after = chat.invalidation_previous_through.unwrap_or(0);
        return StoreUpdate::effects(vec![open_effect(
            agent,
            chat.protocol,
            chat.stream_attempt,
            StoreStreamQuery::After {
                after,
                tail_bound: Some(crate::REPLAY_TAIL),
            },
            chat.paused,
        )]);
    }

    let Some(acknowledged) = chat.in_flight.take() else {
        return StoreUpdate::default();
    };
    if acknowledged.op != op {
        chat.in_flight = Some(acknowledged);
        return StoreUpdate::default();
    }
    if reconcile(chat, &result).is_err() {
        if chat.state == ChatState::Flushing {
            abandon_flush(state, agent);
            return StoreUpdate::default();
        }
        return reload(state, agent);
    }
    chat.expected = result.expected;
    chat.content_revision = result.content_revision;
    chat.boundaries = merge_boundaries(&chat.boundaries, &result.boundaries);

    let mut effects = Vec::new();
    if chat.paused && chat.pending_bytes() <= PENDING_COMMIT_MAX_BYTES {
        chat.paused = false;
        effects.push(Effect::ResumeStream(agent));
    }
    effects.extend(dispatch_next(state, agent));
    finish_flush_if_clean(state, agent, &mut effects);
    StoreUpdate::effects(effects)
}

/// Install a canonical acknowledgement into the visible window, then replay
/// the speculative suffix that followed it.
///
/// Commit results contain the canonical bodies for every changed held key, so
/// a second full copy of the window is unnecessary. Replacing those bodies in
/// place and replaying only later pending mutations preserves the same result
/// while retaining each unchanged scrollback entry once.
pub fn reconcile(chat: &mut ChatWindow, result: &fold::CommitResult) -> Result<(), ReloadRequired> {
    let encoded = postcard::to_allocvec(result).map_err(|_| ReloadRequired::CanonicalBody)?;
    if encoded.len() > COMMIT_RESULT_MAX_BYTES {
        return Err(ReloadRequired::ResultTooLarge);
    }
    for deleted in &result.deleted {
        chat.entries.retain(|entry| entry.key() != deleted);
    }
    for (from, to) in &result.redirected {
        chat.entries.retain(|entry| entry.key() != from);
        if let Some(existing) = chat.aliases.iter_mut().find(|(source, _)| source == from) {
            existing.1.clone_from(to);
        } else {
            chat.aliases.push((from.clone(), to.clone()));
        }
    }
    for body in &result.bodies {
        let placement = result
            .placed
            .iter()
            .find(|placement| placement.key == body.key)
            .ok_or(ReloadRequired::CanonicalBody)?;
        let stored = decode_body(chat.protocol, placement, body)?;
        chat.entries.retain(|entry| entry.key() != stored.key());
        chat.entries.push(stored);
    }
    sort_entries(&mut chat.entries);
    let retained = if chat.retain_oldest {
        WindowEnd::Oldest
    } else {
        WindowEnd::Newest
    };
    let evicted = trim_window(&mut chat.entries, retained, chat.max_entries);
    chat.newest_evicted |= retained == WindowEnd::Oldest && evicted;
    for pending in &chat.pending {
        pending.mutations.apply_to(
            &mut chat.entries,
            &mut chat.aliases,
            pending.head.segment(),
            false,
        )?;
    }
    let evicted = trim_window(&mut chat.entries, retained, chat.max_entries);
    chat.newest_evicted |= retained == WindowEnd::Oldest && evicted;
    chat.view_epoch = chat.view_epoch.saturating_add(1);
    Ok(())
}

fn decode_body(
    protocol: StructuredProtocol,
    placement: &Placement,
    body: &Stored<JsonBytes>,
) -> Result<StoredDto, ReloadRequired> {
    macro_rules! decode {
        ($ty:ty, $variant:ident) => {{
            let entry: $ty =
                postcard::from_bytes(&body.entry.0).map_err(|_| ReloadRequired::CanonicalBody)?;
            StoredDto::$variant(Box::new(Stored {
                key: placement.key.clone(),
                segment: placement.segment,
                order: placement.order,
                revision: placement.revision,
                entry,
            }))
        }};
    }
    Ok(match protocol {
        StructuredProtocol::ClaudePtyTranscript => decode!(ClaudeEntry, Claude),
        StructuredProtocol::ClaudeSdk => decode!(ClaudeSdkEntry, ClaudeSdk),
        StructuredProtocol::Codex => decode!(CodexEntry, Codex),
    })
}

fn paged_result(
    state: &mut StoreState,
    agent: AgentId,
    attempt: AttemptId,
    op: OpId,
    page: PageDto,
) -> StoreUpdate {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    if chat.attempt != attempt {
        return StoreUpdate::default();
    }
    let Some(active_op) = chat.page_request.as_ref().map(|request| request.op) else {
        return StoreUpdate::default();
    };
    if active_op != op {
        return StoreUpdate::default();
    }
    let request = chat
        .page_request
        .take()
        .expect("the active page request was just observed");
    if request.view_epoch != chat.view_epoch || request.content_revision != chat.content_revision {
        return retry_page(state, agent, request.n);
    }
    macro_rules! install {
        ($page:expr, $variant:ident) => {{
            if $page.content_revision != chat.content_revision {
                return retry_page(state, agent, request.n);
            }
            let older = $page
                .entries
                .into_iter()
                .map(|value| StoredDto::$variant(Box::new(value)))
                .collect::<Vec<_>>();
            chat.entries.extend(older);
            sort_entries(&mut chat.entries);
            chat.entries
                .dedup_by(|left, right| left.key() == right.key());
            chat.newest_evicted |=
                trim_window(&mut chat.entries, WindowEnd::Oldest, chat.max_entries);
            for pending in &chat.pending {
                if pending
                    .mutations
                    .apply_to(
                        &mut chat.entries,
                        &mut chat.aliases,
                        pending.head.segment(),
                        false,
                    )
                    .is_err()
                {
                    return reload(state, agent);
                }
            }
            sort_entries(&mut chat.entries);
            chat.entries
                .dedup_by(|left, right| left.key() == right.key());
            chat.newest_evicted |=
                trim_window(&mut chat.entries, WindowEnd::Oldest, chat.max_entries);
            chat.boundaries = merge_boundaries(&chat.boundaries, &$page.boundaries);
            chat.first_page = $page.next;
            chat.view_epoch = chat.view_epoch.saturating_add(1);
        }};
    }
    match page {
        PageDto::Claude(page) => install!(page, Claude),
        PageDto::ClaudeSdk(page) => install!(page, ClaudeSdk),
        PageDto::Codex(page) => install!(page, Codex),
    }
    StoreUpdate::default()
}

fn failed_result(
    state: &mut StoreState,
    agent: Option<AgentId>,
    attempt: AttemptId,
    op: OpId,
    kind: StoreOpKind,
    error: StoreError,
) -> StoreUpdate {
    let Some(agent) = agent else {
        if kind == StoreOpKind::FleetLoad && state.startup_fleet_op == Some(op) {
            state.startup_fleet_op = None;
            state.fleet_settled = true;
        }
        if matches!(kind, StoreOpKind::ViewGet | StoreOpKind::ViewSet)
            && error == StoreError::RecoveryRequired
        {
            if kind == StoreOpKind::ViewGet && state.startup_view_op == Some(op) {
                state.startup_view_op = None;
            }
            return StoreUpdate::default();
        }
        return StoreUpdate::effects(vec![Effect::StoreFailed { kind, error }]);
    };
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    if chat.attempt != attempt {
        return StoreUpdate::default();
    }
    let active_op = match kind {
        StoreOpKind::Load => chat.active_load == Some(op),
        StoreOpKind::Commit => chat.in_flight.as_ref().is_some_and(|batch| batch.op == op),
        StoreOpKind::Page => chat
            .page_request
            .as_ref()
            .is_some_and(|request| request.op == op),
        StoreOpKind::Invalidate => chat.invalidation_op == Some(op),
        StoreOpKind::FleetLoad
        | StoreOpKind::FleetApply
        | StoreOpKind::ViewGet
        | StoreOpKind::ViewSet => false,
    };
    if !active_op {
        return StoreUpdate::default();
    }
    if error == StoreError::GenerationMoved {
        if chat.state == ChatState::Flushing {
            abandon_flush(state, agent);
            return StoreUpdate::default();
        }
        return reload(state, agent);
    }
    match (kind, error) {
        (StoreOpKind::Commit, StoreError::Busy | StoreError::Io) => {
            let Some(mut in_flight) = chat.in_flight.take() else {
                return StoreUpdate::default();
            };
            if in_flight.op != op {
                chat.in_flight = Some(in_flight);
                return StoreUpdate::default();
            }
            if in_flight.busy_retries == 0 {
                in_flight.busy_retries = 1;
                let retry = commit_effect(state.profile, agent, chat, &in_flight);
                chat.in_flight = Some(in_flight);
                StoreUpdate::effects(vec![Effect::RetryStore {
                    after_ms: 1_000,
                    op: Box::new(retry),
                }])
            } else {
                StoreUpdate::effects(vec![Effect::StoreFailed { kind, error }])
            }
        }
        (StoreOpKind::Page, StoreError::Busy) => {
            chat.page_request = None;
            StoreUpdate::default()
        }
        (_, error) => StoreUpdate::effects(vec![Effect::StoreFailed { kind, error }]),
    }
}

pub(crate) fn update_chat_stream(
    state: &mut StoreState,
    agent: AgentId,
    attempt: StreamAttempt,
    event: ChatStreamMsg,
) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if chat.stream_attempt != attempt {
        return Vec::new();
    }
    if matches!(
        event,
        ChatStreamMsg::Opened { .. } | ChatStreamMsg::Closed { .. }
    ) {
        chat.stream_opening = false;
    }
    match event {
        ChatStreamMsg::Opened { facts, at } => opened(state, agent, facts, at),
        ChatStreamMsg::Batch { at, entries } => batch(state, agent, at, entries),
        ChatStreamMsg::ReplayComplete { at } => replay_complete(state, agent, at),
        ChatStreamMsg::Closed { at, reason } => closed(state, agent, at, reason),
    }
}

pub(crate) fn reconnect(state: &mut StoreState) -> Vec<Effect> {
    for chat in state.chats.values_mut() {
        chat.stream_opening = false;
    }
    let agents = state.chats.keys().copied().collect::<Vec<_>>();
    agents
        .into_iter()
        .filter_map(|agent| reconnect_chat(state, agent))
        .collect()
}

pub(crate) fn reconnect_chat(state: &mut StoreState, agent: AgentId) -> Option<Effect> {
    let chat = state.chats.get_mut(&agent)?;
    if chat.state != ChatState::Painted || chat.stream_opening {
        return None;
    }
    chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
    let query = chat.head.as_ref().map_or(
        StoreStreamQuery::TailCount {
            count: crate::REPLAY_TAIL,
            tail_bound: Some(crate::REPLAY_TAIL),
        },
        |head| StoreStreamQuery::After {
            after: head.through(),
            tail_bound: Some(crate::REPLAY_TAIL),
        },
    );
    Some(open_effect(
        agent,
        chat.protocol,
        chat.stream_attempt,
        query,
        chat.paused,
    ))
}

fn opened(
    state: &mut StoreState,
    agent: AgentId,
    facts: ReplayFactsDto,
    at: DateTime<Utc>,
) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if chat.state != ChatState::Painted {
        return Vec::new();
    }
    chat.replay_through = facts.through;
    let existing_through = chat.head.as_ref().map_or(0, HeadDto::through);
    let invalidation_previous_through = chat.invalidation_previous_through.take();
    let invalidation_baseline = chat.next_baseline.take();
    let baseline = invalidation_baseline.or_else(|| match (&facts.outcome, chat.head.is_some()) {
        (ReplayOutcomeDto::Continuous, true) => None,
        (ReplayOutcomeDto::Continuous, false) => {
            Some(if facts.through == 0 || facts.selected_from == 1 {
                Baseline::Start
            } else {
                Baseline::Truncated {
                    from: if facts.selected_from == 0 {
                        facts.through.saturating_add(1)
                    } else {
                        facts.selected_from
                    },
                }
            })
        }
        (ReplayOutcomeDto::Truncated { .. } | ReplayOutcomeDto::Reset { .. }, true) => {
            Some(Baseline::Gap {
                after: existing_through,
            })
        }
        (ReplayOutcomeDto::Truncated { .. } | ReplayOutcomeDto::Reset { .. }, false) => {
            Some(Baseline::Truncated {
                from: if facts.selected_from == 0 {
                    facts.through.saturating_add(1)
                } else {
                    facts.selected_from
                },
            })
        }
    });
    if let Some(baseline) = baseline {
        let successor = chat.segment_high_water.saturating_add(1).max(1);
        let (predecessor, previous_through) =
            if let Some(previous_through) = invalidation_previous_through {
                (
                    (chat.segment_high_water > 0).then_some(chat.segment_high_water),
                    previous_through,
                )
            } else {
                (chat.head.as_ref().map(HeadDto::segment), existing_through)
            };
        chat.transition = Some(SegmentTransition {
            predecessor,
            successor,
            baseline,
            previous_through,
            selected_from: (facts.selected_from != 0).then_some(facts.selected_from),
            replay_through: facts.through,
            opened_at: at,
        });
        chat.segment_high_water = successor;
        chat.head = Some(new_head(chat.protocol, successor, baseline, at));
    }
    chat.state = ChatState::CatchingUp;
    chat.catching_up_since = Some(at);
    Vec::new()
}

fn new_head(
    protocol: StructuredProtocol,
    segment: SegmentId,
    baseline: Baseline,
    at: DateTime<Utc>,
) -> HeadDto {
    macro_rules! head {
        ($fold:ty, $variant:ident) => {{
            let mut tip = <$fold>::default();
            tip.begin(segment, baseline);
            let summary = tip.summary();
            HeadDto::$variant(Head {
                segment,
                baseline,
                through: 0,
                tip_version: <$fold>::TIP_VERSION,
                entry_version: <$fold>::ENTRY_VERSION,
                tip,
                summary,
                observed_at: at,
            })
        }};
    }
    match protocol {
        StructuredProtocol::ClaudePtyTranscript => head!(ClaudeFold, Claude),
        StructuredProtocol::ClaudeSdk => head!(ClaudeSdkFold, ClaudeSdk),
        StructuredProtocol::Codex => head!(CodexFold, Codex),
    }
}

fn batch(
    state: &mut StoreState,
    agent: AgentId,
    at: DateTime<Utc>,
    entries: Vec<StreamEntry>,
) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if !matches!(
        chat.state,
        ChatState::CatchingUp | ChatState::Live | ChatState::Flushing
    ) {
        return Vec::new();
    }
    let Some(head) = chat.head.as_mut() else {
        return reload(state, agent).effects;
    };
    let mut combined: Option<MutationBatchDto> = None;
    for entry in entries {
        let payload = match serde_json::to_vec(&entry.payload) {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        let mutations = head.apply(
            Input::Row {
                seq: entry.seq,
                published_at: entry.published_at,
                activity_at: entry.activity_at,
                historical: entry.historical,
                payload: &payload,
            },
            at,
        );
        append_mutations(&mut combined, mutations);
    }
    let Some(mutations) = combined else {
        return Vec::new();
    };
    enqueue(state, agent, mutations)
}

fn append_mutations(target: &mut Option<MutationBatchDto>, source: MutationBatchDto) {
    if let Some(target) = target {
        target.append(source);
    } else {
        *target = Some(source);
    }
}

fn enqueue(state: &mut StoreState, agent: AgentId, mutations: MutationBatchDto) -> Vec<Effect> {
    let op = state.op();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    let Some(head) = chat.head.clone() else {
        return Vec::new();
    };
    chat.view_epoch = chat.view_epoch.saturating_add(1);
    if mutations
        .apply_to(&mut chat.entries, &mut chat.aliases, head.segment(), false)
        .is_err()
    {
        return reload(state, agent).effects;
    }
    let retained = if chat.retain_oldest {
        WindowEnd::Oldest
    } else {
        WindowEnd::Newest
    };
    let evicted = trim_window(&mut chat.entries, retained, chat.max_entries);
    chat.newest_evicted |= retained == WindowEnd::Oldest && evicted;
    let (mutations, transition) = if let Some(transition) = chat.transition.as_ref() {
        let replay_ready = head.through() >= transition.replay_through;
        let mut combined = None;
        for pending in chat.pending.drain(..) {
            append_mutations(&mut combined, pending.mutations);
        }
        append_mutations(&mut combined, mutations);
        (
            combined.expect("the current stream batch contributes mutations"),
            replay_ready.then(|| {
                chat.transition
                    .take()
                    .expect("the replay transition was just observed")
            }),
        )
    } else {
        (mutations, None)
    };
    let replay_waiting = chat.transition.is_some();
    if let Some(pending) = chat.pending.last_mut() {
        pending.head = head;
        if transition.is_some() {
            pending.transition = transition;
        }
        pending.mutations.append(mutations);
        pending.bytes = pending.mutations.encoded_bytes();
    } else {
        let bytes = mutations.encoded_bytes();
        chat.pending.push(PendingCommit {
            op,
            head,
            transition,
            mutations,
            bytes,
            busy_retries: 0,
        });
    }
    let mut effects = if replay_waiting {
        Vec::new()
    } else {
        dispatch_next(state, agent)
    };
    if let Some(chat) = state.chats.get_mut(&agent)
        && chat.pending_bytes() > PENDING_COMMIT_MAX_BYTES
        && !replay_waiting
        && !chat.paused
    {
        chat.paused = true;
        effects.push(Effect::PauseStream(agent));
    }
    effects
}

fn dispatch_next(state: &mut StoreState, agent: AgentId) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if chat.in_flight.is_some() || chat.pending.is_empty() {
        return Vec::new();
    }
    let pending = chat.pending.remove(0);
    let effect = commit_effect(state.profile, agent, chat, &pending);
    chat.in_flight = Some(pending);
    vec![effect]
}

fn commit_effect(
    profile: ProfileGeneration,
    agent: AgentId,
    chat: &ChatWindow,
    pending: &PendingCommit,
) -> Effect {
    Effect::Store(StoreOp::Commit {
        profile,
        attempt: chat.attempt,
        op: pending.op,
        agent,
        generations: chat
            .generations
            .expect("a persistable chat has store generations"),
        expected: chat.expected,
        head: Box::new(pending.head.clone()),
        transition: pending.transition.clone(),
        mutations: pending.mutations.clone(),
        interest: interest(chat),
    })
}

fn interest(chat: &ChatWindow) -> WindowInterest {
    WindowInterest {
        // A followed-tip window is a contiguous suffix, already described by
        // `feed_from`. Enumerating every key duplicated a growing window into
        // every commit. A scrolled window is not contiguous with the tip and
        // still names its exact held keys.
        held_keys: if chat.retain_oldest {
            chat.entries
                .iter()
                .map(|entry| entry.key().clone())
                .collect()
        } else {
            Vec::new()
        },
        feed_from: if chat.retain_oldest {
            None
        } else {
            chat.entries.first().map(|entry| {
                let (segment, order, key) = entry.position();
                (segment, order, key.clone())
            })
        },
        view_epoch: chat.view_epoch,
        result_max_bytes: COMMIT_RESULT_MAX_BYTES,
    }
}

fn replay_complete(state: &mut StoreState, agent: AgentId, at: DateTime<Utc>) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if chat.state != ChatState::CatchingUp {
        return Vec::new();
    }
    chat.state = ChatState::Live;
    chat.catching_up_since = None;
    let through = chat
        .replay_through
        .max(chat.head.as_ref().map_or(0, HeadDto::through));
    let mutations = chat
        .head
        .as_mut()
        .map(|head| head.apply(Input::ReplayComplete { through, at }, at));
    mutations.map_or_else(Vec::new, |mutations| enqueue(state, agent, mutations))
}

fn closed(
    state: &mut StoreState,
    agent: AgentId,
    at: DateTime<Utc>,
    reason: StreamCloseReason,
) -> Vec<Effect> {
    let mutations = {
        let Some(chat) = state.chats.get_mut(&agent) else {
            return Vec::new();
        };
        if !matches!(chat.state, ChatState::CatchingUp | ChatState::Live) {
            return Vec::new();
        }
        match reason {
            StreamCloseReason::AgentExited { exit_code } => chat
                .head
                .as_mut()
                .map(|head| head.apply(Input::ProcessExited { exit_code, at }, at)),
            _ => {
                chat.state = ChatState::Painted;
                chat.catching_up_since = None;
                // Transport loss makes the reducer-local observer stale, but
                // it does not revoke facts already committed at the durable
                // cursor. The next exact-cursor open must be able to restore
                // those obligations and the prior turn condition before it
                // folds any newer rows.
                None
            }
        }
    };
    mutations.map_or_else(Vec::new, |mutations| enqueue(state, agent, mutations))
}

pub(crate) fn page_older(state: &mut StoreState, agent: AgentId, n: usize) -> Vec<Effect> {
    let op = state.op();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    let Some(mut token) = chat.first_page.clone() else {
        return Vec::new();
    };
    if chat.page_request.is_some() {
        return Vec::new();
    }
    chat.retain_oldest = true;
    token.view_epoch = chat.view_epoch;
    token.content_revision = chat.content_revision;
    chat.page_request = Some(PageRequest {
        op,
        view_epoch: chat.view_epoch,
        content_revision: chat.content_revision,
        n,
    });
    vec![Effect::Store(StoreOp::Page {
        profile: state.profile,
        attempt: chat.attempt,
        op,
        agent,
        protocol: chat.protocol,
        token,
        n,
    })]
}

pub(crate) fn follow_tip(state: &mut StoreState, agent: AgentId) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    chat.retain_oldest = false;
    chat.page_request = None;
    if !chat.newest_evicted {
        return Vec::new();
    }
    reload(state, agent).effects
}

fn retry_page(state: &mut StoreState, agent: AgentId, n: usize) -> StoreUpdate {
    StoreUpdate::effects(page_older(state, agent, n))
}

pub(crate) fn close_chat(
    state: &mut StoreState,
    agent: AgentId,
    now: DateTime<Utc>,
) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    chat.state = ChatState::Flushing;
    chat.flush_deadline = Some(now + FLUSH_DEADLINE);
    let mut effects = vec![Effect::CloseStream { agent }];
    if chat.transition.is_none() {
        effects.extend(dispatch_next(state, agent));
    }
    finish_flush_if_clean(state, agent, &mut effects);
    effects
}

pub(crate) fn flush_deadline(
    state: &mut StoreState,
    agent: AgentId,
    now: DateTime<Utc>,
) -> Vec<Effect> {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return Vec::new();
    };
    if chat.state != ChatState::Flushing
        || chat.flush_deadline.is_none_or(|deadline| now < deadline)
    {
        return Vec::new();
    }
    chat.abandoned_flush = chat.in_flight.is_some() || !chat.pending.is_empty();
    chat.pending.clear();
    chat.in_flight = None;
    chat.state = ChatState::Absent;
    Vec::new()
}

fn finish_flush_if_clean(state: &mut StoreState, agent: AgentId, effects: &mut Vec<Effect>) {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return;
    };
    if chat.state == ChatState::Flushing && chat.in_flight.is_none() && chat.pending.is_empty() {
        chat.state = ChatState::Absent;
        chat.flush_deadline = None;
        if !effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseStream { agent: id } if *id == agent))
        {
            effects.push(Effect::CloseStream { agent });
        }
    }
}

fn abandon_flush(state: &mut StoreState, agent: AgentId) {
    let Some(chat) = state.chats.get_mut(&agent) else {
        return;
    };
    chat.abandoned_flush = chat.in_flight.is_some() || !chat.pending.is_empty();
    chat.pending.clear();
    chat.in_flight = None;
    chat.transition = None;
    chat.page_request = None;
    chat.flush_deadline = None;
    chat.paused = false;
    chat.state = ChatState::Absent;
}

fn reload(state: &mut StoreState, agent: AgentId) -> StoreUpdate {
    let new_attempt = state.attempt();
    let op = state.op();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    chat.state = ChatState::Reloading;
    chat.attempt = new_attempt;
    chat.pending.clear();
    chat.in_flight = None;
    chat.page_request = None;
    chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
    chat.active_load = Some(op);
    StoreUpdate::effects(vec![
        Effect::CloseStream { agent },
        Effect::Store(StoreOp::Load {
            profile: state.profile,
            attempt: new_attempt,
            op,
            agent,
            protocol: chat.protocol,
            window: window_budget(chat.max_entries, chat.view_epoch.saturating_add(1)),
        }),
    ])
}

fn open_effect(
    agent: AgentId,
    protocol: StructuredProtocol,
    attempt: StreamAttempt,
    query: StoreStreamQuery,
    paused: bool,
) -> Effect {
    Effect::OpenStoreStream {
        agent,
        protocol,
        attempt,
        query,
        paused,
    }
}

fn merge_boundaries(existing: &[BoundaryAt], changed: &[BoundaryAt]) -> Vec<BoundaryAt> {
    let mut merged = existing.to_vec();
    for boundary in changed {
        if let Some(current) = merged.iter_mut().find(|current| {
            current.segment == boundary.segment && current.before == boundary.before
        }) {
            *current = boundary.clone();
        } else {
            merged.push(boundary.clone());
        }
    }
    merged.sort_by(|left, right| (left.segment, &left.before).cmp(&(right.segment, &right.before)));
    merged
}

fn sort_entries(entries: &mut [StoredDto]) {
    entries.sort_by(|left, right| left.position().cmp(&right.position()));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowEnd {
    Oldest,
    Newest,
}

/// Retain the end nearest the viewport and report whether the opposite end moved.
fn trim_window(entries: &mut Vec<StoredDto>, retained: WindowEnd, max_entries: usize) -> bool {
    let mut trimmed = false;
    if entries.len() > max_entries {
        let excess = entries.len() - max_entries;
        match retained {
            WindowEnd::Oldest => entries.truncate(max_entries),
            WindowEnd::Newest => {
                entries.drain(..excess);
            }
        }
        trimmed = true;
    }
    while postcard::to_allocvec(entries.as_slice())
        .map_or(true, |bytes| bytes.len() > WINDOW_MAX_BYTES)
        && entries.len() > 1
    {
        match retained {
            WindowEnd::Oldest => {
                entries.pop();
            }
            WindowEnd::Newest => {
                entries.remove(0);
            }
        }
        trimmed = true;
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use fold::claude_pty::{ClaudeEntry, ClaudePartial};
    use fold::{Entry, Mutation, Order, Patch};
    use model::AgentId;
    use uuid::Uuid;

    use super::*;

    fn at(second: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + second, 0).unwrap()
    }

    fn agent() -> AgentId {
        Uuid::from_u128(42)
    }

    fn partial(text: String, revision: Revision) -> ClaudePartial {
        ClaudePartial {
            text: Patch::set(text, revision),
            ..ClaudePartial::default()
        }
    }

    fn stored(text: &str, seq: u64) -> Stored<ClaudeEntry> {
        let revision = Revision::row(seq);
        Stored {
            key: EntryKey::new("message:one").unwrap(),
            segment: 1,
            order: Order::new(1, 0).unwrap(),
            revision,
            entry: ClaudeEntry::from_partial(&partial(text.to_owned(), revision)).unwrap(),
        }
    }

    fn completed_report(text: &str) -> StoredDto {
        let revision = Revision::row(1);
        let entry = ClaudeEntry::from_partial(&ClaudePartial {
            kind: Patch::set(fold::claude_pty::ClaudeEntryKind::AgentMessage, revision),
            body: Patch::set(
                fold::claude_pty::ClaudeBody::AgentMessage {
                    id: Some("report".to_owned()),
                    context: None,
                    from: "worker/host".to_owned(),
                    kind: model::AgentMessageKind::Completed,
                },
                revision,
            ),
            text: Patch::set(text.to_owned(), revision),
            ..ClaudePartial::default()
        })
        .unwrap();
        StoredDto::Claude(Box::new(Stored {
            key: EntryKey::new("agent-message:report").unwrap(),
            segment: 1,
            order: Order::new(1, 0).unwrap(),
            revision,
            entry,
        }))
    }

    fn chat() -> ChatWindow {
        let mut chat = ChatWindow::loading(
            StructuredProtocol::ClaudePtyTranscript,
            AttemptId(1),
            WINDOW_MAX_ENTRIES,
        );
        chat.state = ChatState::Live;
        chat.generations = Some(Generations {
            fleet: 1,
            chat: 1,
            provider: 1,
        });
        chat.head = Some(new_head(
            StructuredProtocol::ClaudePtyTranscript,
            1,
            Baseline::Start,
            at(0),
        ));
        chat.segment_high_water = 1;
        chat
    }

    #[test]
    fn in_place_canonical_reconciliation_reapplies_the_pending_suffix() {
        let mut chat = chat();
        let canonical = stored("canonical", 1);
        chat.entries = vec![StoredDto::Claude(Box::new(canonical.clone()))];

        let revision = Revision::row(2);
        let pending = MutationBatchDto::Claude(vec![Mutation::Upsert {
            key: canonical.key.clone(),
            order: canonical.order,
            revision,
            entry: partial("speculative".to_owned(), revision),
        }]);
        pending
            .apply_to(&mut chat.entries, &mut chat.aliases, 1, false)
            .unwrap();
        chat.pending.push(PendingCommit {
            op: OpId(2),
            head: chat.head.clone().unwrap(),
            transition: None,
            bytes: pending.encoded_bytes(),
            mutations: pending,
            busy_retries: 0,
        });

        let body = postcard::to_allocvec(&canonical.entry).unwrap();
        reconcile(
            &mut chat,
            &fold::CommitResult {
                expected: ExpectedHead::Present {
                    fence: 1,
                    version: 2,
                },
                content_revision: 2,
                placed: vec![Placement {
                    key: canonical.key.clone(),
                    segment: canonical.segment,
                    order: canonical.order,
                    revision: canonical.revision,
                }],
                bodies: vec![Stored {
                    key: canonical.key,
                    segment: canonical.segment,
                    order: canonical.order,
                    revision: canonical.revision,
                    entry: JsonBytes(body),
                }],
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        )
        .unwrap();

        let [StoredDto::Claude(entry)] = chat.entries.as_slice() else {
            panic!("one Claude entry remains")
        };
        assert_eq!(entry.entry.text(), Some("speculative"));
        let retained = chat.retention();
        assert_eq!(retained.visible_entries, 1);
        assert_eq!(retained.canonical_entries, 0);
        assert_eq!(retained.canonical_entry_bytes, 0);
    }

    #[test]
    fn fresh_store_backed_upsert_leaves_the_visible_allocation_untouched() {
        let mut chat = chat();
        chat.entries = vec![StoredDto::Claude(Box::new(stored("held", 1)))];
        let entries_allocation = chat.entries.as_ptr();
        let held_entry = match &chat.entries[0] {
            StoredDto::Claude(entry) => std::ptr::from_ref(entry.as_ref()),
            _ => unreachable!(),
        };
        let revision = Revision::row(2);
        let fresh = MutationBatchDto::Claude(vec![Mutation::Upsert {
            key: EntryKey::new("message:fresh").unwrap(),
            order: Order::new(2, 0).unwrap(),
            revision,
            entry: partial("fresh".to_owned(), revision),
        }]);

        fresh
            .apply_to(&mut chat.entries, &mut chat.aliases, 1, false)
            .unwrap();

        assert_eq!(chat.entries.as_ptr(), entries_allocation);
        let StoredDto::Claude(entry) = &chat.entries[0] else {
            unreachable!()
        };
        assert_eq!(std::ptr::from_ref(entry.as_ref()), held_entry);
        assert_eq!(entry.entry.text(), Some("held"));
    }

    #[test]
    fn followed_tip_interest_uses_the_contiguous_lower_bound() {
        let mut chat = chat();
        chat.entries = vec![StoredDto::Claude(Box::new(stored("held", 1)))];

        let followed = interest(&chat);
        assert!(followed.held_keys.is_empty());
        assert!(followed.feed_from.is_some());

        chat.retain_oldest = true;
        let scrolled = interest(&chat);
        assert_eq!(scrolled.held_keys.len(), 1);
        assert!(scrolled.feed_from.is_none());
    }

    #[test]
    fn pending_byte_budget_pauses_then_a_commit_resumes_the_stream() {
        let mut state = StoreState::default();
        state.chats.insert(agent(), chat());
        let revision = Revision::row(1);
        let mutations = MutationBatchDto::Claude(vec![Mutation::Upsert {
            key: EntryKey::new("message:large").unwrap(),
            order: Order::new(1, 0).unwrap(),
            revision,
            entry: partial("x".repeat(PENDING_COMMIT_MAX_BYTES + 1), revision),
        }]);
        let effects = enqueue(&mut state, agent(), mutations);
        assert!(matches!(
            effects.first(),
            Some(Effect::Store(StoreOp::Commit { .. }))
        ));
        assert!(matches!(effects.last(), Some(Effect::PauseStream(id)) if *id == agent()));
        assert!(state.chats.get(&agent()).unwrap().paused);

        let op = state
            .chats
            .get(&agent())
            .unwrap()
            .in_flight
            .as_ref()
            .unwrap()
            .op;
        let effects = committed_result(
            &mut state,
            agent(),
            AttemptId(1),
            op,
            fold::CommitResult {
                expected: ExpectedHead::Present {
                    fence: 1,
                    version: 1,
                },
                content_revision: 1,
                placed: Vec::new(),
                bodies: Vec::new(),
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        );
        assert!(matches!(effects.effects.as_slice(), [Effect::ResumeStream(id)] if *id == agent()));
        assert!(!state.chats.get(&agent()).unwrap().paused);
    }

    #[test]
    fn startup_selects_the_platform_window_policy() {
        for max_entries in [WINDOW_MAX_ENTRIES, PHONE_WINDOW_MAX_ENTRIES] {
            let mut state = StoreState::default();
            startup(
                &mut state,
                ProfileGeneration(1),
                Generations {
                    fleet: 1,
                    chat: 1,
                    provider: 1,
                },
                max_entries,
            );
            open_chat(&mut state, agent(), StructuredProtocol::ClaudePtyTranscript);
            assert_eq!(state.chats.get(&agent()).unwrap().max_entries, max_entries);
        }
    }

    #[test]
    fn durable_window_reports_when_a_completed_body_can_fold() {
        assert!(completed_report("summary\ndetail").has_foldable_completion());
        assert!(!completed_report("summary").has_foldable_completion());
    }

    #[test]
    fn behind_requires_both_cuts_and_a_strictly_newer_host_cut() {
        let progress = model::Progress {
            through: 11,
            at: at(1),
            revision: 2,
        };
        assert!(!behind(None, Some(10)));
        assert!(!behind(Some(&progress), None));
        assert!(!behind(Some(&progress), Some(11)));
        assert!(behind(Some(&progress), Some(10)));
    }
}
