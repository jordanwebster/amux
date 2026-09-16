//! Store-backed reducer lifecycle: the A12 table as recorded messages.

use fold::claude_pty::ClaudeFold;
use fold::{
    Baseline, BaselineReason, CommitResult, ExpectedHead, Fleet, FleetAgent, FleetHost,
    Generations, Head, HeadState, Loaded, Membership, OpId as StoreOpId, ProviderFold, StoreError,
};
use ui_state::{
    AttemptId, ChatCommand, ChatState, ChatStreamMsg, Effect, LoadedDto, Model, Msg,
    ProfileGeneration, ReplayFactsDto, ReplayOutcomeDto, StoreMsg, StoreOp, StoreOpKind,
    StoreStreamQuery, StreamCloseReason, StreamEntry, update,
};

use crate::harness::*;

const PROFILE: ProfileGeneration = ProfileGeneration(5);
const GENERATIONS: Generations = Generations {
    fleet: 11,
    chat: 12,
    provider: 13,
};

fn empty_loaded(head: HeadState<ClaudeFold>) -> LoadedDto {
    LoadedDto::Claude(Loaded {
        generations: GENERATIONS,
        fence: 7,
        content_revision: 9,
        segment_high_water: 2,
        head,
        window: Vec::new(),
        boundaries: Vec::new(),
        first_page: None,
        aliases: Vec::new(),
        host: None,
        progress: None,
    })
}

fn loaded_with_page(head: HeadState<ClaudeFold>) -> LoadedDto {
    let LoadedDto::Claude(mut loaded) = empty_loaded(head) else {
        unreachable!()
    };
    loaded.first_page = Some(fold::PageToken {
        generations: GENERATIONS,
        content_revision: 9,
        view_epoch: 1,
        before: (
            2,
            fold::Order::new(10, 0).unwrap(),
            fold::EntryKey::new("message:anchor").unwrap(),
        ),
    });
    LoadedDto::Claude(loaded)
}

fn usable_head(through: u64) -> HeadState<ClaudeFold> {
    let mut tip = ClaudeFold::default();
    tip.begin(2, Baseline::Start);
    HeadState::Usable(
        3,
        Head {
            segment: 2,
            baseline: Baseline::Start,
            through,
            tip_version: ClaudeFold::TIP_VERSION,
            entry_version: ClaudeFold::ENTRY_VERSION,
            summary: tip.summary(),
            tip,
            observed_at: t0(),
        },
    )
}

fn inventory_model() -> Model {
    let mut model = Model::default();
    for msg in seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("stored", "nova")),
        ],
        synced(),
    ]) {
        update(&mut model, msg);
    }
    model
}

fn begin_open(model: &mut Model) -> (AttemptId, StoreOpId) {
    let effects = update(
        model,
        Msg::Chat(ChatCommand::Open {
            agent: agent_id("stored"),
        }),
    );
    let [Effect::Store(StoreOp::Load { attempt, op, .. })] = effects.as_slice() else {
        panic!("open must issue exactly one store load: {effects:?}");
    };
    (*attempt, *op)
}

fn load(model: &mut Model, attempt: AttemptId, op: StoreOpId, loaded: LoadedDto) -> Vec<Effect> {
    update(
        model,
        Msg::Store(StoreMsg::Loaded {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: agent_id("stored"),
            loaded: Box::new(loaded),
        }),
    )
}

fn stream_attempt(effects: &[Effect]) -> fold::StreamAttempt {
    let [Effect::OpenStoreStream { attempt, .. }] = effects else {
        panic!("load must open exactly one store stream: {effects:?}");
    };
    *attempt
}

fn continuous(through: u64) -> ReplayFactsDto {
    ReplayFactsDto {
        retained_from: 1,
        through,
        selected_from: through.saturating_add(1),
        reset_at: 0,
        outcome: ReplayOutcomeDto::Continuous,
    }
}

fn commit_result(expected: ExpectedHead) -> CommitResult {
    CommitResult {
        expected,
        content_revision: 10,
        placed: Vec::new(),
        bodies: Vec::new(),
        deleted: Vec::new(),
        redirected: Vec::new(),
        boundaries: Vec::new(),
    }
}

fn live_empty(model: &mut Model) -> (AttemptId, fold::StreamAttempt) {
    let (attempt, op) = begin_open(model);
    let effects = load(model, attempt, op, empty_loaded(HeadState::None));
    let stream = stream_attempt(&effects);
    update(
        model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: continuous(0),
                at: t0_plus(1),
            },
        },
    );
    let effects = update(
        model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: t0_plus(2) },
        },
    );
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!("the first segment must persist its transition: {effects:?}");
    };
    update(
        model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: *op,
            agent: agent_id("stored"),
            result: commit_result(ExpectedHead::Present {
                fence: 7,
                version: 1,
            }),
        }),
    );
    (attempt, stream)
}

fn recorded_batch(stream: fold::StreamAttempt, from: u64) -> Msg {
    Msg::ChatStream {
        agent: agent_id("stored"),
        attempt: stream,
        event: ChatStreamMsg::Batch {
            at: t0_plus(3),
            entries: vec![StreamEntry::observed(
                from,
                t0_plus(3),
                serde_json::json!({"type":"future", "uuid":format!("raw-{from}")}),
            )],
        },
    }
}

#[test]
fn a_batch_without_window_mutations_still_commits_its_advanced_head() {
    let mut model = inventory_model();
    let (_, stream) = live_empty(&mut model);
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: t0_plus(3),
                entries: vec![StreamEntry::observed(
                    1,
                    t0_plus(3),
                    serde_json::json!({"type":"file-history-snapshot"}),
                )],
            },
        },
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::Commit { .. })]
    ));
}

#[test]
fn loading_usable_none_and_failure_each_paint_before_opening_the_stream() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(usable_head(40)));
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Painted
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream {
            query: StoreStreamQuery::After { after: 40, .. },
            ..
        }]
    ));

    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(HeadState::None));
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream {
            query: StoreStreamQuery::TailCount { .. },
            ..
        }]
    ));

    let (attempt, op) = begin_open(&mut model);
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Failed {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: Some(agent_id("stored")),
            kind: StoreOpKind::Load,
            error: StoreError::UnsupportedFormat,
        }),
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::Painted);
    assert!(chat.live_only);
    assert_eq!(chat.persistence_error, Some(StoreError::UnsupportedFormat));
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream { .. }]
    ));
}

#[test]
fn a_needs_baseline_head_invalidates_once_then_opens_from_the_previous_cut() {
    let mut model = inventory_model();
    let (attempt, load_op) = begin_open(&mut model);
    let effects = load(
        &mut model,
        attempt,
        load_op,
        empty_loaded(HeadState::NeedsBaseline {
            previous_through: 31,
            reason: BaselineReason::TipVersion,
        }),
    );
    let [Effect::Store(StoreOp::Invalidate { op, .. })] = effects.as_slice() else {
        panic!("needs-baseline must invalidate: {effects:?}");
    };
    let invalidate_op = *op;
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Invalidating
    );

    let late = update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: StoreOpId(invalidate_op.0 + 1),
            agent: agent_id("stored"),
            result: commit_result(ExpectedHead::Absent { fence: 8 }),
        }),
    );
    assert!(late.is_empty());
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Invalidating
    );

    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: invalidate_op,
            agent: agent_id("stored"),
            result: commit_result(ExpectedHead::Absent { fence: 8 }),
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Painted
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream {
            query: StoreStreamQuery::After { after: 31, .. },
            ..
        }]
    ));

    let stream = stream_attempt(&effects);
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: continuous(31),
                at: t0_plus(1),
            },
        },
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::CatchingUp);
    let ui_state::HeadDto::Claude(head) = chat.head.as_ref().unwrap() else {
        panic!("Claude chat has a Claude head")
    };
    assert_eq!(head.baseline, Baseline::VersionGap { after: 31 });
}

#[test]
fn painted_catching_up_live_loss_and_exit_follow_the_stream_rows() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(usable_head(10)));
    let stream = stream_attempt(&effects);
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: continuous(10),
                at: t0_plus(1),
            },
        },
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::CatchingUp
    );
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: t0_plus(2) },
        },
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Live
    );

    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Closed {
                at: t0_plus(3),
                reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
            },
        },
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Live
    );

    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Closed {
                at: t0_plus(4),
                reason: StreamCloseReason::HostUnreachable,
            },
        },
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Painted
    );
}

#[test]
fn truncated_fresh_open_store_unavailability_and_reconnect_remain_visible_states() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(HeadState::None));
    let stream = stream_attempt(&effects);
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 5,
                    through: 10,
                    selected_from: 5,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Truncated { missing_after: 4 },
                },
                at: t0_plus(1),
            },
        },
    );
    let ui_state::HeadDto::Claude(head) = model
        .chat(agent_id("stored"))
        .unwrap()
        .head
        .as_ref()
        .unwrap()
    else {
        panic!("Claude chat has a Claude head")
    };
    assert_eq!(head.baseline, Baseline::Truncated { from: 5 });

    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Closed {
                at: t0_plus(2),
                reason: StreamCloseReason::Reset,
            },
        },
    );
    let effects = update(&mut model, connected("nova"));
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream {
            query: StoreStreamQuery::After { after: 0, .. },
            ..
        }]
    ));

    let mut unavailable = inventory_model();
    begin_open(&mut unavailable);
    let effects = update(
        &mut unavailable,
        Msg::Store(StoreMsg::Unavailable {
            profile: ProfileGeneration(0),
            error: StoreError::Corrupt,
        }),
    );
    let chat = unavailable.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::Painted);
    assert!(chat.live_only);
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream { .. }]
    ));
}

#[test]
fn generation_movement_reloads() {
    let mut model = inventory_model();
    let (attempt, stream) = live_empty(&mut model);
    let effects = update(&mut model, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!("a folded batch must begin one commit: {effects:?}");
    };
    let op = *op;
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Failed {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: Some(agent_id("stored")),
            kind: StoreOpKind::Commit,
            error: StoreError::GenerationMoved,
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Reloading
    );
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::CloseStream { .. },
            Effect::Store(StoreOp::Load { .. })
        ]
    ));
}

#[test]
fn batches_commit_optimistically_then_conflict_replaces_the_window_and_attempt() {
    let mut model = inventory_model();
    let (attempt, stream) = live_empty(&mut model);
    let effects = update(&mut model, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!(
            "a folded batch must begin one commit: {effects:?}; chat={:?}",
            model.chat(agent_id("stored"))
        );
    };
    let commit_op = *op;
    let before_attempt = model.chat(agent_id("stored")).unwrap().attempt;
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Conflict {
            profile: ProfileGeneration(0),
            attempt,
            op: commit_op,
            agent: agent_id("stored"),
            loaded: Box::new(empty_loaded(HeadState::None)),
        }),
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert!(chat.attempt.0 > before_attempt.0);
    assert_eq!(chat.state, ChatState::Painted);
    assert!(matches!(
        effects.as_slice(),
        [Effect::CloseStream { .. }, Effect::OpenStoreStream { .. }]
    ));
}

#[test]
fn busy_retries_the_same_commit_once_then_degrades_only_that_chat() {
    let mut model = inventory_model();
    let (attempt, stream) = live_empty(&mut model);
    let effects = update(&mut model, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!(
            "a folded batch must begin one commit: {effects:?}; chat={:?}",
            model.chat(agent_id("stored"))
        );
    };
    let commit_op = *op;
    let retry = update(
        &mut model,
        Msg::Store(StoreMsg::Failed {
            profile: ProfileGeneration(0),
            attempt,
            op: commit_op,
            agent: Some(agent_id("stored")),
            kind: StoreOpKind::Commit,
            error: StoreError::Busy,
        }),
    );
    assert!(matches!(
        retry.as_slice(),
        [Effect::RetryStore {
            after_ms: 1_000,
            ..
        }]
    ));
    update(
        &mut model,
        Msg::Store(StoreMsg::Failed {
            profile: ProfileGeneration(0),
            attempt,
            op: commit_op,
            agent: Some(agent_id("stored")),
            kind: StoreOpKind::Commit,
            error: StoreError::Busy,
        }),
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert!(chat.live_only);
    assert_eq!(chat.persistence_error, Some(StoreError::Busy));
}

#[test]
fn a_page_is_retried_when_local_content_moves_after_the_request() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, loaded_with_page(usable_head(0)));
    let stream = stream_attempt(&effects);
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: continuous(0),
                at: t0_plus(1),
            },
        },
    );
    let page_effects = update(
        &mut model,
        Msg::Chat(ChatCommand::PageOlder {
            agent: agent_id("stored"),
            n: 50,
        }),
    );
    let [Effect::Store(StoreOp::Page { op: page_op, .. })] = page_effects.as_slice() else {
        panic!("page request missing: {page_effects:?}");
    };
    let page_op = *page_op;
    let stale = update(
        &mut model,
        Msg::Store(StoreMsg::Paged {
            profile: ProfileGeneration(0),
            attempt,
            op: StoreOpId(page_op.0.saturating_add(100)),
            agent: agent_id("stored"),
            page: ui_state::PageDto::Claude(fold::Page {
                entries: Vec::new(),
                boundaries: Vec::new(),
                next: None,
                content_revision: 9,
            }),
        }),
    );
    assert!(
        stale.is_empty(),
        "a stale op must not consume the active page"
    );
    update(&mut model, recorded_batch(stream, 1));
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Paged {
            profile: ProfileGeneration(0),
            attempt,
            op: page_op,
            agent: agent_id("stored"),
            page: ui_state::PageDto::Claude(fold::Page {
                entries: Vec::new(),
                boundaries: Vec::new(),
                next: None,
                content_revision: 9,
            }),
        }),
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::Page { .. })]
    ));
}

#[test]
fn a_failed_page_is_dropped_without_retrying_an_unrelated_operation() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    load(&mut model, attempt, op, loaded_with_page(usable_head(0)));
    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::PageOlder {
            agent: agent_id("stored"),
            n: 25,
        }),
    );
    let [Effect::Store(StoreOp::Page { op, .. })] = effects.as_slice() else {
        panic!("page request missing: {effects:?}");
    };
    let page_op = *op;
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Failed {
            profile: ProfileGeneration(0),
            attempt,
            op: page_op,
            agent: Some(agent_id("stored")),
            kind: StoreOpKind::Page,
            error: StoreError::Io,
        }),
    );
    assert!(effects.is_empty());
    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::PageOlder {
            agent: agent_id("stored"),
            n: 25,
        }),
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::Page { .. })]
    ));
}

#[test]
fn dirty_close_waits_for_the_in_flight_commit_and_abandons_at_five_seconds() {
    let mut model = inventory_model();
    let (_, stream) = live_empty(&mut model);
    let effects = update(&mut model, recorded_batch(stream, 1));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::Commit { .. })]
    ));
    update(
        &mut model,
        Msg::Chat(ChatCommand::Close {
            agent: agent_id("stored"),
            now: t0_plus(10),
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Flushing
    );
    update(
        &mut model,
        Msg::Chat(ChatCommand::FlushDeadline {
            agent: agent_id("stored"),
            now: t0_plus(14),
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Flushing
    );
    update(
        &mut model,
        Msg::Chat(ChatCommand::FlushDeadline {
            agent: agent_id("stored"),
            now: t0_plus(15),
        }),
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::Absent);
    assert!(chat.abandoned_flush);
}

#[test]
fn close_with_no_dirty_state_finishes_immediately_and_deadline_is_idempotent() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    load(&mut model, attempt, op, empty_loaded(HeadState::None));
    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::Close {
            agent: agent_id("stored"),
            now: t0(),
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Absent
    );
    assert!(matches!(effects.as_slice(), [Effect::CloseStream { .. }]));

    update(
        &mut model,
        Msg::Chat(ChatCommand::FlushDeadline {
            agent: agent_id("stored"),
            now: t0_plus(5),
        }),
    );
    assert!(!model.chat(agent_id("stored")).unwrap().abandoned_flush);
}

#[test]
fn startup_loads_fleet_and_remembered_chat_before_network_and_persists_deltas() {
    let mut model = Model::default();
    let effects = update(
        &mut model,
        Msg::StoreStartup {
            profile: PROFILE,
            generations: GENERATIONS,
        },
    );
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::Store(StoreOp::FleetLoad { .. }),
            Effect::Store(StoreOp::ViewGet { .. })
        ]
    ));
    update(
        &mut model,
        Msg::Store(StoreMsg::ViewLoaded {
            profile: PROFILE,
            op: StoreOpId(2),
            value: Some(agent_id("stored").to_string()),
        }),
    );
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::FleetLoaded {
            profile: PROFILE,
            op: StoreOpId(1),
            fleet: remembered_fleet(),
        }),
    );
    assert!(model.agent(agent_id("stored")).unwrap().remembered);
    assert_eq!(model.remembered_chat(), Some(agent_id("stored")));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::Load { .. })]
    ));

    let effects = update(
        &mut model,
        Msg::FleetDelta(fold::FleetDelta::Reachability {
            host_id: host_id("nova"),
            online: false,
        }),
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::Store(StoreOp::FleetApply { .. })]
    ));
}

fn remembered_fleet() -> Fleet {
    Fleet {
        hosts: vec![FleetHost {
            host: a_host("nova"),
            revision: 1,
            updated_at: t0(),
        }],
        agents: vec![FleetAgent {
            agent: an_agent("stored", "nova"),
            membership: Membership::Cached,
            absent_since: None,
            last_opened_at: Some(t0()),
        }],
    }
}

/// Store results live in the same recording as network and interaction
/// messages; replay must reproduce the complete model after every prefix.
pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![(
        "store/startup-remembered-chat-live-close",
        vec![
            Msg::StoreStartup {
                profile: PROFILE,
                generations: GENERATIONS,
            },
            Msg::Store(StoreMsg::ViewLoaded {
                profile: PROFILE,
                op: StoreOpId(2),
                value: Some(agent_id("stored").to_string()),
            }),
            Msg::Store(StoreMsg::FleetLoaded {
                profile: PROFILE,
                op: StoreOpId(1),
                fleet: remembered_fleet(),
            }),
            Msg::Store(StoreMsg::Loaded {
                profile: PROFILE,
                attempt: AttemptId(1),
                op: StoreOpId(3),
                agent: agent_id("stored"),
                loaded: Box::new(empty_loaded(HeadState::None)),
            }),
            Msg::ChatStream {
                agent: agent_id("stored"),
                attempt: fold::StreamAttempt(1),
                event: ChatStreamMsg::Opened {
                    facts: continuous(0),
                    at: t0_plus(1),
                },
            },
            Msg::ChatStream {
                agent: agent_id("stored"),
                attempt: fold::StreamAttempt(1),
                event: ChatStreamMsg::ReplayComplete { at: t0_plus(2) },
            },
            Msg::Chat(ChatCommand::Close {
                agent: agent_id("stored"),
                now: t0_plus(3),
            }),
        ],
    )]
}
