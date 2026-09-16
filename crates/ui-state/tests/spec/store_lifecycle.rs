//! Store-backed reducer lifecycle: the A12 table as recorded messages.

use fold::claude_pty::ClaudeFold;
use fold::claude_sdk::ClaudeSdkFold;
use fold::codex::CodexFold;
use fold::{
    Baseline, BaselineReason, CommitResult, ExpectedHead, Fleet, FleetAgent, FleetHost,
    Generations, Head, HeadState, Input, JsonBytes, Loaded, Membership, MutationOracle,
    OpId as StoreOpId, Placement, ProviderFold, StoreError, Stored,
};
use serde_json::json;
use ui_state::{
    AttemptId, ChatCommand, ChatState, ChatStreamMsg, Effect, LoadedDto, Model, Msg,
    MutationBatchDto, ProfileGeneration, ReplayFactsDto, ReplayOutcomeDto, ServerMsg, StoreMsg,
    StoreOp, StoreOpKind, StoreStreamQuery, StreamCloseReason, StreamEntry, update,
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

fn empty_codex_loaded() -> LoadedDto {
    LoadedDto::Codex(Loaded {
        generations: GENERATIONS,
        fence: 7,
        content_revision: 9,
        segment_high_water: 2,
        head: HeadState::None,
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

fn inventory_model_with(agent: model::Agent) -> Model {
    let mut model = Model::default();
    for msg in seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&agent),
        ],
        synced(),
    ]) {
        update(&mut model, msg);
    }
    model
}

fn apply_tip_row<F: ProviderFold>(tip: &mut F, seq: u64, row: serde_json::Value) {
    let payload = serde_json::to_vec(&row).unwrap();
    tip.apply(Input::Row {
        seq,
        published_at: t0_plus(seq as i64),
        activity_at: Some(t0_plus(seq as i64)),
        historical: false,
        payload: &payload,
    });
}

fn sdk_loaded_with_pending_ask() -> LoadedDto {
    let mut tip = ClaudeSdkFold::default();
    tip.begin(2, Baseline::Start);
    apply_tip_row(
        &mut tip,
        10,
        json!({"type":"amux.claude_sdk.permission_required","request_id":"stored-sdk-ask","tool_name":"Write","input":{"file_path":"/tmp/a","content":"hello"},"suggestions":[]}),
    );
    LoadedDto::ClaudeSdk(Loaded {
        generations: GENERATIONS,
        fence: 7,
        content_revision: 9,
        segment_high_water: 2,
        head: HeadState::Usable(
            3,
            Head {
                segment: 2,
                baseline: Baseline::Start,
                through: 10,
                tip_version: ClaudeSdkFold::TIP_VERSION,
                entry_version: ClaudeSdkFold::ENTRY_VERSION,
                summary: tip.summary(),
                tip,
                observed_at: t0(),
            },
        ),
        window: Vec::new(),
        boundaries: Vec::new(),
        first_page: None,
        aliases: Vec::new(),
        host: None,
        progress: None,
    })
}

fn codex_loaded_with_pending_ask() -> LoadedDto {
    let mut tip = CodexFold::default();
    tip.begin(2, Baseline::Start);
    apply_tip_row(
        &mut tip,
        9,
        json!({"type":"item/commandExecution/requestApproval","itemId":"stored-command","command":"cargo test"}),
    );
    apply_tip_row(
        &mut tip,
        10,
        json!({"type":"amux.codex_approval_required","item_id":"stored-command","request_id":"stored-codex-ask","availableDecisions":["accept","cancel"]}),
    );
    LoadedDto::Codex(Loaded {
        generations: GENERATIONS,
        fence: 7,
        content_revision: 9,
        segment_high_water: 2,
        head: HeadState::Usable(
            3,
            Head {
                segment: 2,
                baseline: Baseline::Start,
                through: 10,
                tip_version: CodexFold::TIP_VERSION,
                entry_version: CodexFold::ENTRY_VERSION,
                summary: tip.summary(),
                tip,
                observed_at: t0(),
            },
        ),
        window: Vec::new(),
        boundaries: Vec::new(),
        first_page: None,
        aliases: Vec::new(),
        host: None,
        progress: None,
    })
}

fn pty_loaded_with_pending_ask() -> LoadedDto {
    let mut tip = ClaudeFold::default();
    tip.begin(2, Baseline::Start);
    apply_tip_row(
        &mut tip,
        10,
        json!({"type":"hook.permission_request","session_id":"stored-session","tool_name":"Write","tool_input":{"file_path":"/tmp/a","content":"hello"},"permission_suggestions":[]}),
    );
    let head = Head {
        segment: 2,
        baseline: Baseline::Start,
        through: 10,
        tip_version: ClaudeFold::TIP_VERSION,
        entry_version: ClaudeFold::ENTRY_VERSION,
        summary: tip.summary(),
        tip,
        observed_at: t0(),
    };
    empty_loaded(HeadState::Usable(3, head))
}

fn pty_loaded_with_working_turn() -> LoadedDto {
    let mut tip = ClaudeFold::default();
    tip.begin(2, Baseline::Start);
    apply_tip_row(
        &mut tip,
        10,
        json!({
            "type":"user",
            "uuid":"stored-prompt",
            "timestamp":"2025-10-09T08:53:30Z",
            "origin":{"kind":"human"},
            "message":{"content":"keep working"}
        }),
    );
    let head = Head {
        segment: 2,
        baseline: Baseline::Start,
        through: 10,
        tip_version: ClaudeFold::TIP_VERSION,
        entry_version: ClaudeFold::ENTRY_VERSION,
        summary: tip.summary(),
        tip,
        observed_at: t0(),
    };
    empty_loaded(HeadState::Usable(3, head))
}

fn begin_open(model: &mut Model) -> (AttemptId, StoreOpId) {
    let effects = update(
        model,
        Msg::Chat(ChatCommand::Open {
            agent: agent_id("stored"),
        }),
    );
    let [
        Effect::Store(StoreOp::Load { attempt, op, .. }),
        Effect::Store(StoreOp::ViewSet {
            kind, key, value, ..
        }),
    ] = effects.as_slice()
    else {
        panic!("open must load the chat and remember it: {effects:?}");
    };
    assert_eq!((kind.as_str(), key.as_str()), ("ui", "remembered_chat"));
    assert_eq!(value, &agent_id("stored").to_string());
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

#[test]
fn opening_an_already_active_remembered_chat_is_idempotent() {
    let mut model = inventory_model();
    let (attempt, _) = begin_open(&mut model);

    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::Open {
            agent: agent_id("stored"),
        }),
    );

    assert!(effects.is_empty());
    assert_eq!(model.chat(agent_id("stored")).unwrap().attempt, attempt);
}

fn stream_attempt(effects: &[Effect]) -> fold::StreamAttempt {
    let [Effect::OpenStoreStream { attempt, .. }] = effects else {
        panic!("load must open exactly one store stream: {effects:?}");
    };
    *attempt
}

fn complete_empty_replay(
    model: &mut Model,
    stream: fold::StreamAttempt,
    through: u64,
    seconds: i64,
) {
    update(
        model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: continuous(through),
                at: t0_plus(seconds),
            },
        },
    );
    update(
        model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete {
                at: t0_plus(seconds + 1),
            },
        },
    );
}

fn reconnect_empty_replay(model: &mut Model, stream: fold::StreamAttempt, through: u64) {
    update(
        model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Closed {
                at: t0_plus(20),
                reason: StreamCloseReason::HostUnreachable,
            },
        },
    );
    let effects = update(model, connected("nova"));
    let reopened = stream_attempt(&effects);
    complete_empty_replay(model, reopened, through, 21);
}

#[test]
fn store_cursor_restores_pty_ask_on_open_and_reconnect_without_new_rows() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, pty_loaded_with_pending_ask());
    let stream = stream_attempt(&effects);

    complete_empty_replay(&mut model, stream, 10, 1);
    assert_eq!(model.claude(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::NeedsYou
    );
    assert!(ui_state::claude::allows_answer(&model, agent_id("stored")));

    reconnect_empty_replay(&mut model, stream, 10);
    assert_eq!(model.claude(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::NeedsYou
    );
}

#[test]
fn store_cursor_restores_sdk_ask_on_open_and_reconnect_without_new_rows() {
    let mut sdk = an_agent("stored", "nova");
    sdk.kind = model::AgentKind::Claude {
        driver: model::ClaudeDriver::Sdk,
    };
    let mut model = inventory_model_with(sdk);
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, sdk_loaded_with_pending_ask());
    let stream = stream_attempt(&effects);

    complete_empty_replay(&mut model, stream, 10, 1);
    assert_eq!(model.claude_sdk(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::claude_sdk::send_gate(&model, agent_id("stored")),
        ui_state::claude_sdk::SendGate::NeedsYou
    );

    reconnect_empty_replay(&mut model, stream, 10);
    assert_eq!(model.claude_sdk(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::claude_sdk::send_gate(&model, agent_id("stored")),
        ui_state::claude_sdk::SendGate::NeedsYou
    );
}

#[test]
fn store_cursor_restores_codex_ask_on_open_and_reconnect_without_new_rows() {
    let mut model = inventory_model_with(a_codex_agent("stored", "nova"));
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, codex_loaded_with_pending_ask());
    let stream = stream_attempt(&effects);

    complete_empty_replay(&mut model, stream, 10, 1);
    assert_eq!(model.codex(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::codex::send_gate(&model, agent_id("stored")),
        ui_state::codex::SendGate::NeedsYou
    );

    reconnect_empty_replay(&mut model, stream, 10);
    assert_eq!(model.codex(agent_id("stored")).unwrap().ask_count(), 1);
    assert_eq!(
        ui_state::codex::send_gate(&model, agent_id("stored")),
        ui_state::codex::SendGate::NeedsYou
    );
}

#[test]
fn store_cursor_without_a_head_never_opens_the_pty_send_gate() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(HeadState::None));
    let stream = stream_attempt(&effects);

    complete_empty_replay(&mut model, stream, 0, 1);

    assert_eq!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::Unknown
    );
}

#[test]
fn authoritative_ready_rows_clear_unknown_pre_cursor_state() {
    let cases = [
        (
            inventory_model(),
            empty_loaded(HeadState::None),
            serde_json::json!({"type":"amux.transcript_ready"}),
        ),
        (
            inventory_model_with(a_codex_agent("stored", "nova")),
            empty_codex_loaded(),
            serde_json::json!({"type":"amux.codex_ready"}),
        ),
    ];
    for (mut model, loaded, ready) in cases {
        let (attempt, op) = begin_open(&mut model);
        let effects = load(&mut model, attempt, op, loaded);
        let stream = stream_attempt(&effects);
        update(
            &mut model,
            Msg::ChatStream {
                agent: agent_id("stored"),
                attempt: stream,
                event: ChatStreamMsg::Opened {
                    facts: continuous(1),
                    at: t0_plus(1),
                },
            },
        );
        update(
            &mut model,
            Msg::ChatStream {
                agent: agent_id("stored"),
                attempt: stream,
                event: ChatStreamMsg::Batch {
                    at: t0_plus(2),
                    entries: vec![StreamEntry::observed(1, t0_plus(2), ready)],
                },
            },
        );
        update(
            &mut model,
            Msg::ChatStream {
                agent: agent_id("stored"),
                attempt: stream,
                event: ChatStreamMsg::ReplayComplete { at: t0_plus(3) },
            },
        );
        match model
            .agent(agent_id("stored"))
            .unwrap()
            .structured_protocol()
        {
            Some(model::StructuredProtocol::ClaudePtyTranscript) => assert_eq!(
                ui_state::claude::send_gate(&model, agent_id("stored")),
                ui_state::SendGate::Ready
            ),
            Some(model::StructuredProtocol::Codex) => assert_eq!(
                ui_state::codex::send_gate(&model, agent_id("stored")),
                ui_state::codex::SendGate::Ready
            ),
            protocol => panic!("unexpected protocol {protocol:?}"),
        }
    }
}

#[test]
fn store_cursor_keeps_a_working_turn_closed_to_new_prompts_after_reconnect() {
    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, pty_loaded_with_working_turn());
    let stream = stream_attempt(&effects);

    complete_empty_replay(&mut model, stream, 10, 1);
    assert_eq!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::Working
    );

    reconnect_empty_replay(&mut model, stream, 10);
    assert_eq!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::Working
    );
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

fn large_recorded_batch(stream: fold::StreamAttempt, from: u64) -> Msg {
    Msg::ChatStream {
        agent: agent_id("stored"),
        attempt: stream,
        event: ChatStreamMsg::Batch {
            at: t0_plus(3),
            entries: (0..30)
                .map(|offset| {
                    let seq = from + offset;
                    let content = (0..5)
                        .map(|slot| {
                            serde_json::json!({
                                "type": "text",
                                "text": format!("{slot}:{}", "x".repeat(80 * 1024)),
                            })
                        })
                        .collect::<Vec<_>>();
                    StreamEntry::observed(
                        seq,
                        t0_plus(3),
                        serde_json::json!({
                            "type": "assistant",
                            "uuid": uuid::Uuid::from_u128(u128::from(seq)).to_string(),
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2025-10-09T08:53:23.000Z",
                            "message": {
                                "id": format!("large-message-{seq:04}"),
                                "role": "assistant",
                                "stop_reason": "end_turn",
                                "content": content,
                            },
                        }),
                    )
                })
                .collect(),
        },
    }
}

#[test]
fn oversized_catch_up_reaches_replay_completion_before_backpressure_pauses_it() {
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
                facts: continuous(100),
                at: t0_plus(1),
            },
        },
    );

    let effects = update(&mut model, large_recorded_batch(stream, 1));
    let chat = model.chat(agent_id("stored")).unwrap();
    assert!(
        effects.is_empty(),
        "the transition waits for replay: {effects:?}"
    );
    assert!(chat.pending_bytes() > ui_state::store::PENDING_COMMIT_MAX_BYTES);
    assert!(!chat.paused, "the completion marker must remain readable");

    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: t0_plus(4) },
        },
    );
    let [
        Effect::Store(StoreOp::Commit {
            op,
            transition: Some(_),
            ..
        }),
        Effect::PauseStream(agent),
    ] = effects.as_slice()
    else {
        panic!("replay completion must dispatch the transition before pausing: {effects:?}");
    };
    let commit_op = *op;
    assert_eq!(*agent, agent_id("stored"));

    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: commit_op,
            agent: agent_id("stored"),
            result: commit_result(ExpectedHead::Present {
                fence: 7,
                version: 1,
            }),
        }),
    );
    assert!(
        matches!(effects.as_slice(), [Effect::ResumeStream(agent)] if *agent == agent_id("stored"))
    );
    assert!(!model.chat(agent_id("stored")).unwrap().paused);
}

#[test]
fn an_oversized_canonical_commit_result_reloads_instead_of_going_live_only() {
    let mut model = inventory_model();
    let (attempt, stream) = live_empty(&mut model);
    let effects = update(&mut model, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!("the stream batch must begin a commit: {effects:?}");
    };
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: *op,
            agent: agent_id("stored"),
            result: CommitResult {
                expected: ExpectedHead::Present {
                    fence: 7,
                    version: 2,
                },
                content_revision: 11,
                placed: Vec::new(),
                bodies: vec![Stored {
                    key: fold::EntryKey::new("message:oversized").unwrap(),
                    segment: 1,
                    order: fold::Order::new(1, 0).unwrap(),
                    revision: fold::Revision::row(1),
                    entry: JsonBytes(vec![0; ui_state::store::COMMIT_RESULT_MAX_BYTES + 1]),
                }],
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        }),
    );
    let chat = model.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::Reloading);
    assert!(!chat.live_only);
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::CloseStream { .. },
            Effect::Store(StoreOp::Load { .. })
        ]
    ));
}

#[test]
fn a_persisted_stream_never_grows_the_visible_window_past_its_budget() {
    let mut model = inventory_model();
    let (_, stream) = live_empty(&mut model);
    let entries = (1..=1_000)
        .map(|seq| {
            StreamEntry::observed(
                seq,
                t0_plus(3),
                serde_json::json!({
                    "type": "assistant",
                    "uuid": uuid::Uuid::from_u128(u128::from(seq)).to_string(),
                    "sessionId": "22222222-2222-4222-8222-222222222222",
                    "timestamp": "2025-10-09T08:53:23.000Z",
                    "message": {
                        "id": format!("message-{seq:04}"),
                        "role": "assistant",
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": format!("stored row {seq:04}")}],
                    },
                }),
            )
        })
        .collect();
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id("stored"),
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: t0_plus(3),
                entries,
            },
        },
    );
    let [
        Effect::Store(StoreOp::Commit {
            attempt,
            op,
            mutations: MutationBatchDto::Claude(mutations),
            ..
        }),
    ] = effects.as_slice()
    else {
        panic!("the stream batch must be persisted: {effects:?}");
    };
    let mut oracle = MutationOracle::default();
    oracle.apply(mutations).unwrap();
    let canonical = oracle.entries();
    let placed = canonical
        .iter()
        .map(|entry| Placement {
            key: entry.key.clone(),
            segment: entry.segment,
            order: entry.order,
            revision: entry.revision,
        })
        .collect();
    let bodies = canonical
        .into_iter()
        .map(|entry| Stored {
            key: entry.key,
            segment: entry.segment,
            order: entry.order,
            revision: entry.revision,
            entry: JsonBytes(postcard::to_allocvec(&entry.entry).unwrap()),
        })
        .collect();
    update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt: *attempt,
            op: *op,
            agent: agent_id("stored"),
            result: CommitResult {
                expected: ExpectedHead::Present {
                    fence: 8,
                    version: 2,
                },
                content_revision: 11,
                placed,
                bodies,
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        }),
    );
    let window = &model.chat(agent_id("stored")).unwrap().entries;
    assert_eq!(window.len(), ui_state::store::WINDOW_MAX_ENTRIES);
    assert!(
        postcard::to_allocvec(window).unwrap().len() <= ui_state::store::WINDOW_MAX_BYTES,
        "the retained window also obeys its encoded-byte budget"
    );
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
fn stream_batches_coalesce_behind_the_in_flight_commit() {
    let mut model = inventory_model();
    let (attempt, stream) = live_empty(&mut model);
    let first = update(&mut model, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = first.as_slice() else {
        panic!("the first batch must begin a commit: {first:?}");
    };
    let first_op = *op;

    assert!(update(&mut model, recorded_batch(stream, 2)).is_empty());
    assert!(update(&mut model, recorded_batch(stream, 3)).is_empty());

    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op: first_op,
            agent: agent_id("stored"),
            result: commit_result(ExpectedHead::Present {
                fence: 7,
                version: 2,
            }),
        }),
    );
    let [Effect::Store(StoreOp::Commit { head, .. })] = effects.as_slice() else {
        panic!("the queued suffix must become one commit: {effects:?}");
    };
    assert_eq!(head.through(), 3);
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

    let mut model = inventory_model();
    let (attempt, op) = begin_open(&mut model);
    let effects = load(&mut model, attempt, op, empty_loaded(HeadState::None));
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream {
            query: StoreStreamQuery::TailCount { .. },
            ..
        }]
    ));

    let mut model = inventory_model();
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
                facts: continuous(32),
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

    let effects = update(&mut model, recorded_batch(stream, 32));
    let [
        Effect::Store(StoreOp::Commit {
            transition: Some(transition),
            ..
        }),
    ] = effects.as_slice()
    else {
        panic!("the invalidated successor must persist its transition: {effects:?}");
    };
    assert_eq!(transition.predecessor, Some(2));
    assert_eq!(transition.successor, 3);
    assert_eq!(transition.previous_through, 31);
}

#[test]
fn every_invalidation_storage_failure_falls_back_to_a_live_tail() {
    for error in [
        StoreError::Busy,
        StoreError::Io,
        StoreError::DiskFull,
        StoreError::OverBudget,
        StoreError::UnsupportedFormat,
    ] {
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
        let effects = update(
            &mut model,
            Msg::Store(StoreMsg::Failed {
                profile: ProfileGeneration(0),
                attempt,
                op: *op,
                agent: Some(agent_id("stored")),
                kind: StoreOpKind::Invalidate,
                error,
            }),
        );
        let chat = model.chat(agent_id("stored")).unwrap();
        assert_eq!(chat.state, ChatState::Painted, "{error:?}");
        assert!(chat.live_only, "{error:?}");
        assert_eq!(chat.persistence_error, Some(error));
        assert!(matches!(
            effects.as_slice(),
            [Effect::OpenStoreStream {
                query: StoreStreamQuery::TailCount { .. },
                paused: false,
                ..
            }]
        ));
    }
}

#[test]
fn a_conflict_answering_invalidation_installs_the_fresh_load() {
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

    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Conflict {
            profile: ProfileGeneration(0),
            attempt,
            op: *op,
            agent: agent_id("stored"),
            loaded: Box::new(empty_loaded(usable_head(44))),
        }),
    );
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Painted
    );
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::CloseStream { .. },
            Effect::OpenStoreStream {
                query: StoreStreamQuery::After { after: 44, .. },
                ..
            }
        ]
    ));
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
fn closing_during_catch_up_never_commits_an_incomplete_transition_or_reopens() {
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
                facts: continuous(100),
                at: t0_plus(1),
            },
        },
    );
    assert!(update(&mut model, recorded_batch(stream, 1)).is_empty());

    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::Close {
            agent: agent_id("stored"),
            now: t0_plus(10),
        }),
    );
    assert!(matches!(effects.as_slice(), [Effect::CloseStream { .. }]));
    assert_eq!(
        model.chat(agent_id("stored")).unwrap().state,
        ChatState::Flushing
    );

    let effects = update(
        &mut model,
        Msg::Chat(ChatCommand::FlushDeadline {
            agent: agent_id("stored"),
            now: t0_plus(15),
        }),
    );
    assert!(effects.is_empty());
    let chat = model.chat(agent_id("stored")).unwrap();
    assert_eq!(chat.state, ChatState::Absent);
    assert!(chat.abandoned_flush);
}

#[test]
fn store_conflicts_and_failures_while_flushing_never_reopen_the_chat() {
    let mut conflicted = inventory_model();
    let (attempt, stream) = live_empty(&mut conflicted);
    let effects = update(&mut conflicted, recorded_batch(stream, 1));
    let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
        panic!("the stream batch must begin a commit: {effects:?}");
    };
    let commit_op = *op;
    update(
        &mut conflicted,
        Msg::Chat(ChatCommand::Close {
            agent: agent_id("stored"),
            now: t0_plus(10),
        }),
    );
    let effects = update(
        &mut conflicted,
        Msg::Store(StoreMsg::Conflict {
            profile: ProfileGeneration(0),
            attempt,
            op: commit_op,
            agent: agent_id("stored"),
            loaded: Box::new(empty_loaded(usable_head(1))),
        }),
    );
    assert!(effects.is_empty());
    assert_eq!(
        conflicted.chat(agent_id("stored")).unwrap().state,
        ChatState::Absent
    );

    for error in [StoreError::GenerationMoved, StoreError::Io] {
        let mut failed = inventory_model();
        let (attempt, stream) = live_empty(&mut failed);
        let effects = update(&mut failed, recorded_batch(stream, 1));
        let [Effect::Store(StoreOp::Commit { op, .. })] = effects.as_slice() else {
            panic!("the stream batch must begin a commit: {effects:?}");
        };
        let commit_op = *op;
        update(
            &mut failed,
            Msg::Chat(ChatCommand::Close {
                agent: agent_id("stored"),
                now: t0_plus(10),
            }),
        );
        let effects = update(
            &mut failed,
            Msg::Store(StoreMsg::Failed {
                profile: ProfileGeneration(0),
                attempt,
                op: commit_op,
                agent: Some(agent_id("stored")),
                kind: StoreOpKind::Commit,
                error,
            }),
        );
        assert!(effects.is_empty(), "{error:?} must not reopen a stream");
        assert_eq!(
            failed.chat(agent_id("stored")).unwrap().state,
            ChatState::Absent,
            "{error:?}"
        );
    }
}

#[test]
fn reconnect_starts_a_replacement_stream_with_existing_backpressure() {
    let mut model = inventory_model();
    let (_, stream) = live_empty(&mut model);
    let effects = update(&mut model, large_recorded_batch(stream, 1));
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::Store(StoreOp::Commit { .. }),
            Effect::PauseStream(_)
        ]
    ));
    assert!(model.chat(agent_id("stored")).unwrap().paused);

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
    let effects = update(&mut model, connected("nova"));
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenStoreStream { paused: true, .. }]
    ));
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
fn startup_loads_fleet_and_places_the_remembered_cursor_without_opening_a_chat() {
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
    assert!(
        effects.is_empty(),
        "startup must not open the remembered chat"
    );
    assert!(model.chat(agent_id("stored")).is_none());

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

/// A device whose own node answers at once while a paired remote host does
/// not: the remote host's remembered rows stay remembered, send-gated and on
/// screen until that host's own inventory decides them.
fn remembered_model_after_local_sync(agents: &[&str]) -> Model {
    let mut model = Model::default();
    update(
        &mut model,
        Msg::StoreStartup {
            profile: PROFILE,
            generations: GENERATIONS,
        },
    );
    let mut fleet = remembered_fleet();
    fleet.agents = agents
        .iter()
        .map(|name| FleetAgent {
            agent: an_agent(name, "nova"),
            membership: Membership::Cached,
            absent_since: None,
            last_opened_at: None,
        })
        .collect();
    update(
        &mut model,
        Msg::Store(StoreMsg::FleetLoaded {
            profile: PROFILE,
            op: StoreOpId(1),
            fleet,
        }),
    );
    for msg in [
        ServerMsg::Connected {
            local_host_id: Some(host_id("phone")),
        },
        ServerMsg::HostUpserted {
            host: a_host("phone"),
        },
        ServerMsg::HostUpserted {
            host: an_offline_host("nova"),
        },
        ServerMsg::HostsSynchronized,
        ServerMsg::AgentsSynchronized,
    ] {
        update(&mut model, Msg::Server(msg));
    }
    model
}

#[test]
fn remembered_rows_of_an_unanswered_remote_host_survive_local_synchronization() {
    let model = remembered_model_after_local_sync(&["stored"]);
    assert!(model.is_synchronized());
    let card = model
        .agent(agent_id("stored"))
        .expect("a paired host that has not answered cannot disprove its remembered rows");
    assert!(card.remembered);
    assert_ne!(
        ui_state::claude::send_gate(&model, agent_id("stored")),
        ui_state::SendGate::Ready,
        "a remembered row never accepts a send"
    );
}

#[test]
fn a_remote_inventory_confirms_listed_remembered_rows_and_removes_the_rest() {
    let mut model = remembered_model_after_local_sync(&["kept", "removed"]);
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentUpserted {
            agent: an_agent("kept", "nova"),
        }),
    );
    update(
        &mut model,
        Msg::Server(ServerMsg::HostInventory {
            host_id: host_id("nova"),
            agent_ids: vec![agent_id("kept")],
        }),
    );
    assert!(!model.agent(agent_id("kept")).unwrap().remembered);
    assert!(model.agent(agent_id("removed")).is_none());
}

#[test]
fn unpairing_or_an_unpaired_snapshot_forgets_remembered_rows() {
    let mut model = remembered_model_after_local_sync(&["stored"]);
    update(
        &mut model,
        Msg::Server(ServerMsg::HostRemoved {
            id: host_id("nova"),
        }),
    );
    assert!(model.agent(agent_id("stored")).is_none());

    let mut model = Model::default();
    update(
        &mut model,
        Msg::StoreStartup {
            profile: PROFILE,
            generations: GENERATIONS,
        },
    );
    update(
        &mut model,
        Msg::Store(StoreMsg::FleetLoaded {
            profile: PROFILE,
            op: StoreOpId(1),
            fleet: remembered_fleet(),
        }),
    );
    for msg in [
        ServerMsg::Connected {
            local_host_id: Some(host_id("phone")),
        },
        ServerMsg::HostUpserted {
            host: a_host("phone"),
        },
        ServerMsg::HostsSynchronized,
        ServerMsg::AgentsSynchronized,
    ] {
        update(&mut model, Msg::Server(msg));
    }
    assert!(
        model.agent(agent_id("stored")).is_none(),
        "a host missing from the paired set cannot keep remembered rows"
    );
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
        "store/startup-remembered-cursor",
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
        ],
    )]
}
