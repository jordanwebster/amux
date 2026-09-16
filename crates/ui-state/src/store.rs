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
use model::{AgentId, ReplayFacts, ReplayOutcome, Seq, StructuredProtocol};
use serde::{Deserialize, Serialize};

use crate::{Effect, StreamCloseReason, StreamEntry};

pub const PENDING_COMMIT_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const WINDOW_MAX_ENTRIES: usize = 800;
pub const WINDOW_MAX_BYTES: usize = 16 * 1024 * 1024;
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
                let concrete = entries
                    .iter()
                    .map(|stored| match stored {
                        StoredDto::$entry_variant(value) => Ok((**value).clone()),
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
                let held = entries
                    .iter()
                    .map(|entry| entry.key().clone())
                    .collect::<std::collections::BTreeSet<_>>();
                let filtered = if admit_new {
                    $mutations.clone()
                } else {
                    $mutations
                        .iter()
                        .filter(|mutation| match mutation {
                            Mutation::Upsert { key, .. } => held.contains(key),
                            Mutation::Delete { .. } | Mutation::Alias { .. } => true,
                        })
                        .cloned()
                        .collect()
                };
                oracle
                    .apply(&filtered)
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
    Unavailable {
        profile: ProfileGeneration,
        error: StoreError,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatCommand {
    Open { agent: AgentId },
    PageOlder { agent: AgentId, n: usize },
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
    pub live_only: bool,
    pub persistence_error: Option<StoreError>,
    /// The stream's opening time while a replay is still being consumed.
    /// Renderers use it to suppress a distracting catch-up flash.
    #[serde(default)]
    pub catching_up_since: Option<DateTime<Utc>>,
    pub paused: bool,
    pub abandoned_flush: bool,
    canonical_entries: Vec<StoredDto>,
    canonical_aliases: Vec<(EntryKey, EntryKey)>,
    pending: Vec<PendingCommit>,
    in_flight: Option<PendingCommit>,
    page_request: Option<PageRequest>,
    transition: Option<SegmentTransition>,
    next_baseline: Option<Baseline>,
    invalidation_previous_through: Option<Seq>,
    invalidation_op: Option<OpId>,
    active_load: Option<OpId>,
    load_retries: u8,
    replay_through: Seq,
    flush_deadline: Option<DateTime<Utc>>,
}

impl ChatWindow {
    fn loading(protocol: StructuredProtocol, attempt: AttemptId) -> Self {
        Self {
            state: ChatState::Loading,
            protocol,
            attempt,
            stream_attempt: StreamAttempt(0),
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
            live_only: false,
            persistence_error: None,
            catching_up_since: None,
            paused: false,
            abandoned_flush: false,
            canonical_entries: Vec::new(),
            canonical_aliases: Vec::new(),
            pending: Vec::new(),
            in_flight: None,
            page_request: None,
            transition: None,
            next_baseline: None,
            invalidation_previous_through: None,
            invalidation_op: None,
            active_load: None,
            load_retries: 0,
            replay_through: 0,
            flush_deadline: None,
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending.iter().map(|batch| batch.bytes).sum::<usize>()
            + self.in_flight.as_ref().map_or(0, |batch| batch.bytes)
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoreState {
    pub profile: ProfileGeneration,
    pub generations: Option<Generations>,
    pub chats: BTreeMap<AgentId, ChatWindow>,
    pub remembered_chat: Option<AgentId>,
    pub unavailable: Option<StoreError>,
    next_attempt: u64,
    next_op: u64,
    startup_fleet_op: Option<OpId>,
    startup_view_op: Option<OpId>,
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
) -> Vec<Effect> {
    state.profile = profile;
    state.generations = Some(generations);
    state.unavailable = None;
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
    let mut chat = ChatWindow::loading(protocol, attempt);
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
            window: WindowBudget::desktop(0),
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
            StoreUpdate {
                remembered_chat: state.remembered_chat,
                ..StoreUpdate::default()
            }
        }
        StoreMsg::ViewLoaded { .. } | StoreMsg::ViewSet { .. } => StoreUpdate::default(),
        StoreMsg::Unavailable { error, .. } => {
            state.unavailable = Some(error);
            let mut effects = Vec::new();
            for (agent, chat) in &mut state.chats {
                let needs_open = matches!(
                    chat.state,
                    ChatState::Loading | ChatState::Reloading | ChatState::Invalidating
                );
                let was_paused = make_live_only(chat, error);
                if was_paused {
                    effects.push(Effect::ResumeStream(*agent));
                }
                if needs_open {
                    chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
                    chat.state = ChatState::Painted;
                    effects.push(open_effect(
                        *agent,
                        chat.protocol,
                        chat.stream_attempt,
                        StoreStreamQuery::TailCount {
                            count: crate::REPLAY_TAIL,
                            tail_bound: Some(crate::REPLAY_TAIL),
                        },
                    ));
                }
            }
            StoreUpdate::effects(effects)
        }
        StoreMsg::FleetLoaded { .. } => StoreUpdate::default(),
    }
}

#[derive(Default)]
pub(crate) struct StoreUpdate {
    pub effects: Vec<Effect>,
    pub fleet: Option<Fleet>,
    pub remembered_chat: Option<AgentId>,
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
        | StoreMsg::ViewSet { profile, .. }
        | StoreMsg::Unavailable { profile, .. } => *profile,
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
    chat.load_retries = 0;
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
            chat.canonical_aliases = chat.aliases.clone();
            chat.entries = $loaded
                .window
                .into_iter()
                .map(|value| StoredDto::$entry_variant(Box::new(value)))
                .collect();
            chat.canonical_entries = chat.entries.clone();
            chat.pending.clear();
            chat.in_flight = None;
            chat.transition = None;
            chat.next_baseline = None;
            chat.invalidation_op = None;
            chat.invalidation_previous_through = None;
            chat.page_request = None;
            chat.paused = false;
            chat.live_only = false;
            chat.persistence_error = None;
            chat.catching_up_since = None;
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
    let new_attempt = state.attempt();
    let Some(chat) = state.chats.get_mut(&agent) else {
        return StoreUpdate::default();
    };
    chat.state = ChatState::Reloading;
    chat.attempt = new_attempt;
    chat.pending.clear();
    chat.in_flight = None;
    chat.entries.clear();
    chat.canonical_entries.clear();
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

/// Install a canonical acknowledged prefix, then replay the speculative suffix.
pub fn reconcile(chat: &mut ChatWindow, result: &fold::CommitResult) -> Result<(), ReloadRequired> {
    let encoded = postcard::to_allocvec(result).map_err(|_| ReloadRequired::CanonicalBody)?;
    if encoded.len() > COMMIT_RESULT_MAX_BYTES {
        return Err(ReloadRequired::ResultTooLarge);
    }
    for deleted in &result.deleted {
        chat.canonical_entries
            .retain(|entry| entry.key() != deleted);
    }
    for (from, to) in &result.redirected {
        chat.canonical_entries.retain(|entry| entry.key() != from);
        if let Some(existing) = chat
            .canonical_aliases
            .iter_mut()
            .find(|(source, _)| source == from)
        {
            existing.1.clone_from(to);
        } else {
            chat.canonical_aliases.push((from.clone(), to.clone()));
        }
    }
    for body in &result.bodies {
        let placement = result
            .placed
            .iter()
            .find(|placement| placement.key == body.key)
            .ok_or(ReloadRequired::CanonicalBody)?;
        let stored = decode_body(chat.protocol, placement, body)?;
        chat.canonical_entries
            .retain(|entry| entry.key() != stored.key());
        chat.canonical_entries.push(stored);
    }
    sort_entries(&mut chat.canonical_entries);
    chat.entries.clone_from(&chat.canonical_entries);
    chat.aliases.clone_from(&chat.canonical_aliases);
    for pending in &chat.pending {
        pending.mutations.apply_to(
            &mut chat.entries,
            &mut chat.aliases,
            pending.head.segment(),
            false,
        )?;
    }
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
            let mut older = $page
                .entries
                .into_iter()
                .map(|value| StoredDto::$variant(Box::new(value)))
                .collect::<Vec<_>>();
            older.append(&mut chat.entries);
            chat.entries = older;
            sort_entries(&mut chat.entries);
            chat.entries
                .dedup_by(|left, right| left.key() == right.key());
            trim_window(&mut chat.entries);
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
        if error == StoreError::Corrupt {
            state.unavailable = Some(error);
        }
        return StoreUpdate::default();
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
        return reload(state, agent);
    }
    match (kind, error) {
        (StoreOpKind::Load, StoreError::Busy | StoreError::Io) if chat.load_retries == 0 => {
            chat.load_retries = 1;
            StoreUpdate::effects(vec![Effect::RetryStore {
                after_ms: 1_000,
                op: Box::new(Effect::Store(StoreOp::Load {
                    profile: state.profile,
                    attempt,
                    op,
                    agent,
                    protocol: chat.protocol,
                    window: WindowBudget::desktop(chat.view_epoch),
                })),
            }])
        }
        (StoreOpKind::Load, error) => {
            let was_paused = make_live_only(chat, error);
            chat.state = ChatState::Painted;
            chat.stream_attempt = StreamAttempt(chat.stream_attempt.0.saturating_add(1));
            let mut effects = Vec::new();
            if was_paused {
                effects.push(Effect::ResumeStream(agent));
            }
            effects.push(open_effect(
                agent,
                chat.protocol,
                chat.stream_attempt,
                StoreStreamQuery::TailCount {
                    count: crate::REPLAY_TAIL,
                    tail_bound: Some(crate::REPLAY_TAIL),
                },
            ));
            StoreUpdate::effects(effects)
        }
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
                let was_paused = make_live_only(chat, error);
                StoreUpdate::effects(
                    was_paused
                        .then_some(Effect::ResumeStream(agent))
                        .into_iter()
                        .collect(),
                )
            }
        }
        (_, StoreError::UnsupportedFormat) => {
            let was_paused = make_live_only(chat, error);
            StoreUpdate::effects(
                was_paused
                    .then_some(Effect::ResumeStream(agent))
                    .into_iter()
                    .collect(),
            )
        }
        (StoreOpKind::Page, _) => {
            chat.page_request = None;
            StoreUpdate::default()
        }
        (_, error) => {
            let was_paused = make_live_only(chat, error);
            StoreUpdate::effects(
                was_paused
                    .then_some(Effect::ResumeStream(agent))
                    .into_iter()
                    .collect(),
            )
        }
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
    match event {
        ChatStreamMsg::Opened { facts, at } => opened(state, agent, facts, at),
        ChatStreamMsg::Batch { at, entries } => batch(state, agent, at, entries),
        ChatStreamMsg::ReplayComplete { at } => replay_complete(state, agent, at),
        ChatStreamMsg::Closed { at, reason } => closed(state, agent, at, reason),
    }
}

pub(crate) fn reconnect(state: &mut StoreState) -> Vec<Effect> {
    let mut effects = Vec::new();
    for (agent, chat) in &mut state.chats {
        if chat.state != ChatState::Painted {
            continue;
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
        effects.push(open_effect(
            *agent,
            chat.protocol,
            chat.stream_attempt,
            query,
        ));
    }
    effects
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
        .apply_to(
            &mut chat.entries,
            &mut chat.aliases,
            head.segment(),
            chat.live_only,
        )
        .is_err()
    {
        return reload(state, agent).effects;
    }
    if chat.live_only {
        trim_window(&mut chat.entries);
        return Vec::new();
    }
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
    if chat.live_only || chat.in_flight.is_some() || chat.pending.is_empty() {
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
        held_keys: chat
            .entries
            .iter()
            .map(|entry| entry.key().clone())
            .collect(),
        feed_from: chat.entries.first().map(|entry| {
            let (segment, order, key) = entry.position();
            (segment, order, key.clone())
        }),
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
                let mutations = chat
                    .head
                    .as_mut()
                    .map(|head| head.apply(Input::ObserverLost { at }, at));
                chat.state = ChatState::Painted;
                chat.catching_up_since = None;
                mutations
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
    if chat.page_request.is_some() || chat.live_only {
        return Vec::new();
    }
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
    effects.extend(dispatch_next(state, agent));
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
            window: WindowBudget::desktop(chat.view_epoch.saturating_add(1)),
        }),
    ])
}

fn make_live_only(chat: &mut ChatWindow, error: StoreError) -> bool {
    let was_paused = chat.paused;
    chat.live_only = true;
    chat.persistence_error = Some(error);
    chat.pending.clear();
    chat.in_flight = None;
    chat.page_request = None;
    chat.active_load = None;
    chat.invalidation_op = None;
    chat.paused = false;
    was_paused
}

fn open_effect(
    agent: AgentId,
    protocol: StructuredProtocol,
    attempt: StreamAttempt,
    query: StoreStreamQuery,
) -> Effect {
    Effect::OpenStoreStream {
        agent,
        protocol,
        attempt,
        query,
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

fn trim_window(entries: &mut Vec<StoredDto>) {
    if entries.len() > WINDOW_MAX_ENTRIES {
        entries.drain(..entries.len() - WINDOW_MAX_ENTRIES);
    }
    while postcard::to_allocvec(entries.as_slice())
        .map_or(true, |bytes| bytes.len() > WINDOW_MAX_BYTES)
        && entries.len() > 1
    {
        entries.remove(0);
    }
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

    fn chat() -> ChatWindow {
        let mut chat = ChatWindow::loading(StructuredProtocol::ClaudePtyTranscript, AttemptId(1));
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
    fn canonical_reconciliation_reapplies_the_pending_suffix() {
        let mut chat = chat();
        let canonical = stored("canonical", 1);
        chat.canonical_entries = vec![StoredDto::Claude(Box::new(canonical.clone()))];
        chat.entries = chat.canonical_entries.clone();

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
