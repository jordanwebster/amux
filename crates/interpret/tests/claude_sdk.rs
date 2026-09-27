//! The Claude SDK interpreter against the recorded headless sessions in
//! claude-specs and authored facts for what no recording shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interpret::claude_sdk::{CORRELATION, ClaudeSdk, Correlation, State, client_uuid};
use interpret::{Channel, Effect, Event, Fact, Interpreter, run_golden};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude_sdk")
}

fn golden(name: &str) {
    run_golden::<ClaudeSdk>(&fixtures().join(format!("{name}.json"))).assert_ok();
}

macro_rules! goldens {
    ($($test:ident => $fixture:literal,)*) => {
        $(
            #[test]
            fn $test() {
                golden($fixture);
            }
        )*

        const FIXTURES: &[&str] = &[$($fixture),*];
    };
}

goldens! {
    amuxs_own_tools_draw_a_message_or_nothing => "amux_tools",
    lifecycle_events_write_boundaries_and_interrupt => "lifecycle",
    permission_asks_keep_scopes_reason_and_description => "asks",
    questions_and_plan_verdicts_are_asks => "verdicts",
    a_stopped_question_is_dismissed => "question_stopped",
    usage_health_and_sign_in_are_strip_fields => "strip",
    refusal_fallback_is_a_model_switch_row => "rows",
    inputs_become_stream_json_writes => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    recorded_cleared => "recorded_cleared",
    recorded_compacted => "recorded_compacted",
    recorded_configured_turn => "recorded_configured_turn",
    recorded_connected_mcp_servers => "recorded_connected_mcp_servers",
    recorded_controls => "recorded_controls",
    recorded_effortful_turn => "recorded_effortful_turn",
    recorded_elicitation_accepted => "recorded_elicitation_accepted",
    recorded_every_hook_event => "recorded_every_hook_event",
    recorded_forked => "recorded_forked",
    recorded_hook_lifecycle => "recorded_hook_lifecycle",
    recorded_in_process_mcp => "recorded_in_process_mcp",
    recorded_interrupted => "recorded_interrupted",
    recorded_introspection => "recorded_introspection",
    recorded_max_budget => "recorded_max_budget",
    recorded_max_turns => "recorded_max_turns",
    recorded_multi_turn => "recorded_multi_turn",
    recorded_permission_callback => "recorded_permission_callback",
    recorded_plan_reviewed => "recorded_plan_reviewed",
    recorded_question_asked => "recorded_question_asked",
    recorded_resumed => "recorded_resumed",
    recorded_resumed_at => "recorded_resumed_at",
    recorded_session_maintenance => "recorded_session_maintenance",
    recorded_streamed_turn => "recorded_streamed_turn",
    recorded_subagent_task => "recorded_subagent_task",
    recorded_text_turn => "recorded_text_turn",
    recorded_background_shell => "recorded_background_shell",
    recorded_client_id => "recorded_client_id",
    recorded_elicitation_declined => "recorded_elicitation_declined",
    recorded_elicitation_link_refused => "recorded_elicitation_link_refused",
    recorded_failed_tool_server => "recorded_failed_tool_server",
    recorded_failing_command => "recorded_failing_command",
    recorded_image => "recorded_image",
    recorded_question_dismissed => "recorded_question_dismissed",
    recorded_question_every_shape => "recorded_question_every_shape",
    recorded_side_channel => "recorded_side_channel",
    recorded_sign_in_problem => "recorded_sign_in_problem",
    recorded_task_list => "recorded_task_list",
    recorded_elicitation_cancelled => "recorded_elicitation_cancelled",
    recorded_steer_folded => "recorded_steer_folded",
    recorded_steer_preempted => "recorded_steer_preempted",
}

#[test]
fn every_fixture_has_a_test() {
    let mut on_disk = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect::<Vec<_>>();
    on_disk.sort();
    let mut named = FIXTURES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(on_disk, named);
}

fn stream(line: Value) -> Event {
    Event::Fact(Fact {
        channel: Channel::Stream,
        payload: line.to_string().into_bytes(),
    })
}

fn started() -> State {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec, "test");
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    state
}

/// Every item as a reader holding the whole stream has it: full items by
/// key with appends applied, without the clock.
fn final_items(state: &mut State, events: Vec<Event>) -> BTreeMap<String, (String, Vec<u8>)> {
    let mut items = BTreeMap::new();
    for event in events {
        let step = ClaudeSdk::step(state, event).step;
        for item in step.items {
            items.insert(item.key.clone(), (item.text, item.body));
        }
        for append in step.appends {
            items
                .get_mut(&append.key)
                .expect("an append follows its item")
                .0
                .push_str(&append.text);
        }
    }
    items
}

fn block_start(index: u32, kind: &str) -> Event {
    let block = match kind {
        "text" => json!({"type": "text", "text": ""}),
        _ => json!({"type": "thinking", "thinking": ""}),
    };
    stream(
        json!({"type": "stream_event", "event": {"type": "content_block_start", "index": index, "content_block": block}}),
    )
}

fn delta(index: u32, kind: &str, text: &str) -> Event {
    let delta = match kind {
        "text" => json!({"type": "text_delta", "text": text}),
        _ => json!({"type": "thinking_delta", "thinking": text}),
    };
    stream(
        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": index, "delta": delta}}),
    )
}

fn block_stop(index: u32) -> Event {
    stream(json!({"type": "stream_event", "event": {"type": "content_block_stop", "index": index}}))
}

fn whole(block: Value) -> Event {
    stream(
        json!({"type": "assistant", "uuid": "a", "message": {"id": "msg_1", "role": "assistant", "content": [block]}}),
    )
}

/// Streamed deltas split anywhere, deltas without the whole messages, and
/// the whole messages alone all end in the same items under the same keys.
#[test]
fn split_and_coalesced_stream_events_yield_the_same_final_items() {
    let start = stream(
        json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1"}}}),
    );
    let thinking = json!({"type": "thinking", "thinking": "Seven is prime.", "signature": "s"});
    let text = json!({"type": "text", "text": "SEVEN, a prime."});

    let coalesced = vec![whole(thinking.clone()), whole(text.clone())];
    let split = vec![
        start.clone(),
        block_start(0, "thinking"),
        delta(0, "thinking", "Seven "),
        delta(0, "thinking", "is prime."),
        whole(thinking.clone()),
        block_stop(0),
        block_start(1, "text"),
        delta(1, "text", "SEVEN"),
        delta(1, "text", ", a "),
        delta(1, "text", "prime."),
        whole(text.clone()),
        block_stop(1),
    ];
    let resplit = vec![
        start,
        block_start(0, "thinking"),
        delta(0, "thinking", "Seven is prime."),
        block_stop(0),
        whole(thinking),
        block_start(1, "text"),
        delta(1, "text", "SEVEN, a prime."),
        block_stop(1),
        whole(text),
    ];
    let expected = final_items(&mut started(), coalesced);
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        ["msg_1:0", "msg_1:1"],
        "keys are the block's index within its message"
    );
    assert_eq!(final_items(&mut started(), split), expected);
    assert_eq!(final_items(&mut started(), resplit), expected);
}

fn agent_message(envelope: &[u8], text: &str) -> Event {
    Event::Input(wire::Input {
        input_id: [b"in-", envelope].concat(),
        of: Some(wire::input::Of::AgentMessage(wire::Envelope {
            id: envelope.to_vec(),
            text: text.to_owned(),
            ..Default::default()
        })),
    })
}

/// The SDK probe found the uuid on a stdin message echoed on its replay and
/// lifecycle frames, and found messages written during a turn folding into
/// it: reflections are matched by that id, not by arrival order.
#[test]
fn probe_decided_sdk_correlation() {
    assert_eq!(CORRELATION, Correlation::ClientId);
    let mut state = started();
    let mut uuids = Vec::new();
    for (envelope, text) in [(b"env-1", "first"), (b"env-2", "second")] {
        let stepped = ClaudeSdk::step(&mut state, agent_message(envelope, text));
        assert!(stepped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Inject { envelope: sent, .. } if sent.id == envelope
        )));
        uuids.push(client_uuid(envelope));
    }
    assert_eq!(state.shared().pending_messages().len(), 2);

    // The second message is taken first: a FIFO match would clear the first.
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "command_lifecycle", "command_uuid": uuids[1], "state": "started"})),
    );
    let pending = state.shared().pending_messages();
    assert!(pending.contains(&b"env-1".to_vec()), "{pending:?}");
    assert!(!pending.contains(&b"env-2".to_vec()), "{pending:?}");

    // Its replay reflects by the same id.
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "user", "isReplay": true, "uuid": uuids[0], "message": {"role": "user", "content": "first"}}),
        ),
    );
    assert!(state.shared().pending_messages().is_empty());

    // Claude answered them in one turn.
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "result", "subtype": "success", "uuid": "r", "duration_ms": 10})),
    );

    // A person's prompt carries its input id as the message uuid, and its
    // item is keyed by it, the key Claude's own record uses.
    let prompt = Event::Input(wire::Input {
        input_id: b"prompt-1".to_vec(),
        of: Some(wire::input::Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "go".into(),
                ..Default::default()
            })),
        })),
    });
    let stepped = ClaudeSdk::step(&mut state, prompt);
    let uuid = client_uuid(b"prompt-1");
    assert!(
        stepped.effects.iter().any(
            |effect| matches!(effect, Effect::UserMessage { uuid: sent, .. } if *sent == uuid)
        )
    );
    let item = stepped
        .step
        .items
        .iter()
        .find(|item| item.key == uuid)
        .expect("the prompt's item");
    assert_eq!(item.input_id, b"prompt-1");
}

#[test]
fn client_uuids_are_well_formed() {
    let uuid = client_uuid(&[0xff; 16]);
    assert_eq!(uuid, "ffffffff-ffff-4fff-bfff-ffffffffffff");
    assert_eq!(client_uuid(b"p1").len(), 36);
}
