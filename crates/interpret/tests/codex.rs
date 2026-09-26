//! The Codex interpreter against the recorded app-server sessions in
//! codex-specs and authored facts for what no recording shows.

use std::path::{Path, PathBuf};

use interpret::codex::{
    CODEX_INJECT_CONSUMPTION, Codex, CodexWith, Drained, InjectConsumption, Parked, State,
};
use interpret::{Channel, Effect, Event, Fact, Interpreter, run_golden};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex")
}

fn golden(name: &str) {
    run_golden::<Codex>(&fixtures().join(format!("{name}.json"))).assert_ok();
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
    asks_cover_every_request_codex_makes => "asks",
    inputs_become_app_server_requests => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    rows_for_reroutes_errors_and_the_strip => "rows",
    recorded_access_grant => "recorded_access_grant",
    recorded_approval_allow => "recorded_approval_allow",
    recorded_approval_deny => "recorded_approval_deny",
    recorded_approval_scopes => "recorded_approval_scopes",
    recorded_automatic_review => "recorded_automatic_review",
    recorded_background_terminal => "recorded_background_terminal",
    recorded_compaction => "recorded_compaction",
    recorded_dynamic_tools => "recorded_dynamic_tools",
    recorded_exploring => "recorded_exploring",
    recorded_failing_command => "recorded_failing_command",
    recorded_file_changes => "recorded_file_changes",
    recorded_file_moved => "recorded_file_moved",
    recorded_image => "recorded_image",
    recorded_initialize_and_start => "recorded_initialize_and_start",
    recorded_inject_busy => "recorded_inject_busy",
    recorded_inject_drain => "recorded_inject_drain",
    recorded_inject_idle => "recorded_inject_idle",
    recorded_interrupt => "recorded_interrupt",
    recorded_plan_mode => "recorded_plan_mode",
    recorded_reasoning_summary => "recorded_reasoning_summary",
    recorded_signed_out => "recorded_signed_out",
    recorded_subagent => "recorded_subagent",
    recorded_thread_list_and_resume => "recorded_thread_list_and_resume",
    recorded_tool_server_form => "recorded_tool_server_form",
    recorded_turn_error => "recorded_turn_error",
    recorded_turn_retries => "recorded_turn_retries",
    recorded_turn_round_trip => "recorded_turn_round_trip",
    recorded_two_assistant_messages => "recorded_two_assistant_messages",
    recorded_web_search => "recorded_web_search",
}

/// Each consumption arm has its own golden over the same facts, so the
/// probe's constant can flip without losing either.
const ARM_FIXTURES: &[&str] = &["inject_parked", "inject_drained"];

#[test]
fn the_parked_arm_kicks_an_empty_turn_and_consumes_at_its_acknowledgement() {
    run_golden::<CodexWith<Parked>>(&fixtures().join("inject_parked.json")).assert_ok();
}

#[test]
fn the_drained_arm_consumes_at_the_inject_acknowledgement() {
    run_golden::<CodexWith<Drained>>(&fixtures().join("inject_drained.json")).assert_ok();
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
        .chain(ARM_FIXTURES)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(on_disk, named);
}

fn rpc(message: Value) -> Event {
    Event::Fact(Fact {
        channel: Channel::Rpc,
        payload: message.to_string().into_bytes(),
    })
}

fn writes(effects: &[Effect]) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => serde_json::from_slice(bytes).ok(),
            _ => None,
        })
        .collect()
}

fn agent_message(id: &[u8]) -> Event {
    Event::Input(wire::Input {
        input_id: id.to_vec(),
        of: Some(wire::input::Of::AgentMessage(wire::Envelope {
            id: id.to_vec(),
            kind: wire::EnvelopeKind::Message as i32,
            text: "from a peer".into(),
            ..Default::default()
        })),
    })
}

fn prompt(id: &[u8], text: &str) -> Event {
    Event::Input(wire::Input {
        input_id: id.to_vec(),
        of: Some(wire::input::Of::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                text: text.into(),
                ..Default::default()
            })),
        })),
    })
}

/// A thread with a turn running and an agent message injected into it,
/// acknowledged; then the turn ends. Returns what the turn end wrote and
/// the state after it.
fn inject_mid_turn<I: Interpreter<State = State>>() -> (State, Vec<Value>, bool) {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = I::initial(&spec, "test");
    I::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.157.0", "turns": []}, "model": "m"}}),
        ),
    );
    I::step(&mut state, prompt(b"p1", "count"));
    I::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
    );
    let inject = writes(&I::step(&mut state, agent_message(b"e1")).effects);
    assert_eq!(inject.len(), 1, "one inject, no kick while a turn runs");
    assert_eq!(inject[0]["method"], "thread/inject_items");
    I::step(
        &mut state,
        rpc(json!({"id": inject[0]["id"], "result": {}})),
    );
    let after_ack = state.shared().pending_messages().contains(&b"e1".to_vec());
    let end = writes(
        &I::step(
            &mut state,
            rpc(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}})),
        )
        .effects,
    );
    (state, end, after_ack)
}

fn is_empty_turn(write: &Value) -> bool {
    write["method"] == "turn/start" && write["params"]["input"] == json!([])
}

/// The probe (notes/rearchitect/probes/codex-inject-drain.md, codex-cli
/// 0.157.0) found that a running turn drains injected items: the message
/// is consumed at the inject's acknowledgement and no empty turn is kicked
/// at turn end.
#[test]
fn probe_decided_codex_consumption() {
    assert_eq!(CODEX_INJECT_CONSUMPTION, InjectConsumption::DrainedMidTurn);
    let (state, end, pending_after_ack) = inject_mid_turn::<Codex>();
    assert_eq!(state.consumption(), InjectConsumption::DrainedMidTurn);
    assert!(!pending_after_ack, "consumed at the inject acknowledgement");
    assert!(
        !end.iter().any(is_empty_turn),
        "no empty turn is kicked at turn end: {end:?}"
    );
    assert!(state.shared().quiescent());
}

#[test]
fn the_parked_arm_keeps_the_message_pending_until_the_kicked_turn_is_acknowledged() {
    let (mut state, end, pending_after_ack) = inject_mid_turn::<CodexWith<Parked>>();
    assert!(pending_after_ack, "an acknowledged inject is still parked");
    let kick = end
        .iter()
        .find(|write| is_empty_turn(write))
        .expect("an empty turn is kicked at turn end");
    assert!(!state.shared().quiescent());
    CodexWith::<Parked>::step(
        &mut state,
        rpc(json!({"id": kick["id"], "result": {"turn": {"id": "turn-2"}}})),
    );
    assert!(state.shared().pending_messages().is_empty());
}

#[test]
fn an_idle_inject_is_kicked_under_either_arm() {
    fn idle<I: Interpreter<State = State>>() {
        let (mut state, _) = I::initial(&wire::AgentSpec::default(), "test");
        I::step(
            &mut state,
            rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
        );
        let sent = writes(&I::step(&mut state, agent_message(b"e1")).effects);
        assert_eq!(sent[0]["method"], "thread/inject_items");
        assert!(is_empty_turn(&sent[1]), "{sent:?}");
        I::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
        assert!(
            !state.shared().pending_messages().is_empty(),
            "an idle inject waits for its turn"
        );
        I::step(
            &mut state,
            rpc(json!({"id": sent[1]["id"], "result": {"turn": {"id": "turn-1"}}})),
        );
        assert!(state.shared().pending_messages().is_empty());
    }
    idle::<CodexWith<Parked>>();
    idle::<CodexWith<Drained>>();
}

#[test]
fn a_prompt_with_attachments_leaves_them_to_the_agent_process() {
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default(), "test");
    Codex::step(
        &mut state,
        rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
    );
    let effects = Codex::step(
        &mut state,
        Event::Input(wire::Input {
            input_id: b"p1".to_vec(),
            of: Some(wire::input::Of::Codex(wire::CodexInput {
                of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                    text: "What is this? \u{FFFC}".into(),
                    attachments: vec![wire::Attachment::default()],
                })),
            })),
        }),
    )
    .effects;
    let Some(Effect::CodexTurnInput {
        request,
        attachments,
    }) = effects
        .iter()
        .find(|effect| matches!(effect, Effect::CodexTurnInput { .. }))
    else {
        panic!("a turn input with attachments: {effects:?}");
    };
    let request: Value = serde_json::from_slice(request).unwrap();
    assert_eq!(request["method"], "turn/start");
    assert_eq!(attachments.len(), 1);
}
