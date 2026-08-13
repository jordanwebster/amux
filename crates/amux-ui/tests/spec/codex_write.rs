//! Chapter 16 — Codex writes: typed commands to native protobuf payloads.
//!
//! Prompt starts a turn, steer keeps the observed turn id, interrupt always
//! carries that explicit id, and approvals serialize the opaque request id.
//! The reducer-minted operation UUID is forwarded as the input correlation id.

use amux_ui::codex::{
    CodexCommand, CodexDecision, CodexInput, CodexPhase, FeedEntryKind, InFlightKind, PromptPart,
    PromptSource, SendGate,
};
use amux_ui::{Attention, Command, Effect, InputPayload, Msg, OpOutcome, StreamMsg};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "codex-write";

fn command_msg(n: u8, native: CodexCommand) -> Msg {
    command(op(n), Command::Codex(native))
}

fn live_rows() -> Vec<serde_json::Value> {
    vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"turn-live","status":"inProgress"}}),
    ]
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "codex prompt input",
            seq([
                codex_base(AGENT),
                vec![batch(AGENT, 10, vec![json!({"type":"amux.codex_ready"})])],
                vec![command_msg(
                    1,
                    CodexCommand::Prompt {
                        agent: agent_id(AGENT),
                        text: "hello".into(),
                    },
                )],
            ]),
        ),
        (
            "codex explicit interrupt input",
            seq([
                codex_base(AGENT),
                vec![batch(AGENT, 20, live_rows())],
                vec![command_msg(
                    2,
                    CodexCommand::Interrupt {
                        agent: agent_id(AGENT),
                    },
                )],
            ]),
        ),
    ]
}

fn send_effect(effects: &[Effect]) -> (&[u8], &CodexInput) {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::SendInput {
                input_id,
                payload: InputPayload::Codex { payload },
                ..
            } => Some((input_id.as_slice(), payload)),
            _ => None,
        })
        .expect("Codex send effect")
}

fn failure_message(model: &amux_ui::Model, op_n: u8) -> String {
    match &model.finished_op(op(op_n)).expect("op finished").outcome {
        OpOutcome::Error { error } => error.message.clone(),
        other => panic!("expected an error outcome, got {other:?}"),
    }
}

fn send_input_count(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, Effect::SendInput { .. }))
        .count()
}

#[test]
fn idle_prompt_encodes_native_input_and_tracks_only_the_correlated_in_flight_op() {
    let msgs = seq([
        codex_base(AGENT),
        vec![batch(AGENT, 10, vec![json!({"type":"amux.codex_ready"})])],
        vec![command_msg(
            1,
            CodexCommand::Prompt {
                agent: agent_id(AGENT),
                text: "hello".into(),
            },
        )],
    ]);
    let (model, effects) = fold_with_effects(msgs);
    let (input_id, input) = send_effect(&effects);
    assert_eq!(input_id, op(1).0.as_bytes());
    let CodexInput::UserTurn { input } = input else {
        panic!("user turn")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(input).unwrap(),
        json!([{"type":"text","text":"hello"}])
    );
    let in_flight: Vec<_> = codex_layer(&model, AGENT).in_flight_inputs().collect();
    assert_eq!(in_flight.len(), 1);
    assert!(matches!(in_flight[0].kind, InFlightKind::Prompt));
    assert!(
        !codex_layer(&model, AGENT)
            .entries()
            .any(|entry| matches!(entry.kind, amux_ui::codex::FeedEntryKind::Prompt(_))),
        "prompt entries come from protocol userMessage items, never a local echo"
    );
}

#[test]
fn prompt_waits_for_ready_but_a_truncated_replay_can_use_its_idle_row() {
    let before_ready = codex_base(AGENT);
    let startup_model = fold(before_ready.clone());
    assert_eq!(
        amux_ui::codex::send_gate(&startup_model, agent_id(AGENT)),
        SendGate::Replaying
    );
    let (refused, effects) = fold_with_effects(seq([
        before_ready,
        vec![command_msg(
            4,
            CodexCommand::Prompt {
                agent: agent_id(AGENT),
                text: "too early".into(),
            },
        )],
    ]));
    assert_eq!(send_input_count(&effects), 0);
    assert_eq!(failure_message(&refused, 4), "send gated while replaying");

    let truncated = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&a_codex_agent(AGENT, "nova")),
        ],
        synced(),
        vec![stream(AGENT, StreamMsg::Opened { truncated: true })],
        vec![batch(
            AGENT,
            10,
            vec![json!({"type":"thread/status/changed","status":{"type":"idle"}})],
        )],
        vec![stream(AGENT, StreamMsg::ReplayComplete)],
        vec![command_msg(
            5,
            CodexCommand::Prompt {
                agent: agent_id(AGENT),
                text: "safe after truncated catch-up".into(),
            },
        )],
    ]);
    let (_, effects) = fold_with_effects(truncated);
    assert_eq!(
        send_input_count(&effects),
        1,
        "a truncated replay may have evicted the ready marker"
    );
}

#[test]
fn complete_replay_stays_non_idle_everywhere_until_the_ready_row_arrives() {
    let model = fold(codex_base(AGENT));
    let agent = agent_id(AGENT);
    let card = model.agent(agent).expect("Codex fleet card");

    assert_eq!(amux_ui::codex::phase(&model, agent), CodexPhase::Replaying);
    assert_eq!(model.effective_attention(card), Attention::Unknown);
    assert_eq!(
        amux_ui::codex::send_gate(&model, agent),
        SendGate::Replaying
    );
}

#[test]
fn initial_reconnect_failure_outranks_the_missing_ready_row() {
    let msgs = seq([
        codex_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![json!({
                "type":"amux.codex_reconnect_error",
                "error":{"message":"initial attach failed"}
            })],
        )],
        vec![command_msg(
            8,
            CodexCommand::Prompt {
                agent: agent_id(AGENT),
                text: "retry?".into(),
            },
        )],
    ]);
    let (model, effects) = fold_with_effects(msgs);
    let agent = agent_id(AGENT);

    assert_eq!(amux_ui::codex::phase(&model, agent), CodexPhase::ReadOnly);
    assert_eq!(amux_ui::codex::send_gate(&model, agent), SendGate::ReadOnly);
    assert_eq!(send_input_count(&effects), 0);
    assert_eq!(
        failure_message(&model, 8),
        "Codex thread is read-only until reconnect succeeds"
    );
}

#[test]
fn input_result_not_rpc_success_closes_the_layer_in_flight_marker() {
    let mut msgs = seq([
        codex_base(AGENT),
        vec![batch(AGENT, 10, vec![json!({"type":"amux.codex_ready"})])],
        vec![
            command_msg(
                1,
                CodexCommand::Prompt {
                    agent: agent_id(AGENT),
                    text: "hello".into(),
                },
            ),
            op_result(op(1), OpOutcome::InputSent),
        ],
    ]);
    let model = fold(msgs.clone());
    assert_eq!(codex_layer(&model, AGENT).in_flight_inputs().count(), 1);
    msgs.push(batch(
        AGENT,
        20,
        vec![json!({
            "type":"amux.input_result", "input_id":op(1).0.as_bytes(), "ok":{}
        })],
    ));
    assert_eq!(
        codex_layer(&fold(msgs), AGENT).in_flight_inputs().count(),
        0
    );
}

#[test]
fn successful_steer_result_promotes_the_correlated_text_to_an_honest_echo() {
    let mut msgs = seq([codex_base(AGENT), vec![batch(AGENT, 10, live_rows())]]);
    msgs.push(command_msg(
        1,
        CodexCommand::Steer {
            agent: agent_id(AGENT),
            text: "also check tests".into(),
        },
    ));
    assert!(
        !codex_layer(&fold(msgs.clone()), AGENT)
            .entries()
            .any(|entry| matches!(entry.kind, FeedEntryKind::Prompt(_))),
        "an unacknowledged steer is not yet durable feed history"
    );

    msgs.push(batch(
        AGENT,
        20,
        vec![json!({
            "type":"amux.input_result", "input_id":op(1).0.as_bytes(), "ok":{}
        })],
    ));
    let model = fold(msgs);
    let prompts: Vec<_> = codex_layer(&model, AGENT)
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].source, PromptSource::SteerEcho);
    assert_eq!(
        prompts[0].parts,
        vec![PromptPart::Text {
            text: "also check tests".into()
        }]
    );
}

#[test]
fn steer_and_interrupt_always_carry_the_layers_observed_turn_id() {
    let base = seq([codex_base(AGENT), vec![batch(AGENT, 10, live_rows())]]);
    let mut steer = base.clone();
    steer.push(command_msg(
        1,
        CodexCommand::Steer {
            agent: agent_id(AGENT),
            text: "also check tests".into(),
        },
    ));
    let (_, effects) = fold_with_effects(steer);
    assert!(matches!(send_effect(&effects).1,
        CodexInput::Steer { turn_id, .. } if turn_id == "turn-live"));

    let mut interrupt = base;
    interrupt.push(batch(
        AGENT,
        20,
        vec![json!({"type":"amux.codex_gap","reason":"connection_lost"})],
    ));
    interrupt.push(command_msg(
        2,
        CodexCommand::Interrupt {
            agent: agent_id(AGENT),
        },
    ));
    let (_, effects) = fold_with_effects(interrupt);
    assert!(matches!(send_effect(&effects).1,
        CodexInput::Interrupt { turn_id } if turn_id == "turn-live"));
}

#[test]
fn read_only_reconnect_state_refuses_steer_and_interrupt_before_dispatch() {
    for (op_n, command) in [
        (
            6,
            CodexCommand::Steer {
                agent: agent_id(AGENT),
                text: "more".into(),
            },
        ),
        (
            7,
            CodexCommand::Interrupt {
                agent: agent_id(AGENT),
            },
        ),
    ] {
        let rows = [
            live_rows(),
            vec![json!({"type":"amux.codex_reconnect_error",
                "error":{"message":"writer unavailable"}})],
        ]
        .concat();
        let msgs = seq([
            codex_base(AGENT),
            vec![batch(AGENT, 10, rows)],
            vec![command_msg(op_n, command)],
        ]);
        let (model, effects) = fold_with_effects(msgs);
        assert!(matches!(
            amux_ui::codex::phase(&model, agent_id(AGENT)),
            CodexPhase::ReadOnly
        ));
        assert_eq!(send_input_count(&effects), 0);
        assert_eq!(
            failure_message(&model, op_n),
            "Codex thread is read-only until reconnect succeeds"
        );
    }
}

#[test]
fn answers_are_typed_and_serialize_opaque_request_ids() {
    let rows = vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"t","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"e","command":"test"}),
        json!({"type":"amux.codex_approval_required","request_id":"req-7",
            "availableDecisions":["accept","cancel"]}),
    ];
    let mut msgs = seq([codex_base(AGENT), vec![batch(AGENT, 10, rows)]]);
    msgs.push(command_msg(
        3,
        CodexCommand::Answer {
            agent: agent_id(AGENT),
            request_id: json!("req-7"),
            decision: CodexDecision::Accept,
        },
    ));
    let (_, effects) = fold_with_effects(msgs);
    let CodexInput::ApprovalDecision {
        request_id,
        decision,
    } = send_effect(&effects).1
    else {
        panic!("approval decision")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(request_id).unwrap(),
        json!("req-7")
    );
    assert_eq!(decision, "accept");
}
