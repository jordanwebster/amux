//! Chapter 17 — Codex projection agreement.
//!
//! Codex phase, fleet attention, and the write gate are projections of one
//! ordered layer classification. This matrix locks their agreement across
//! the lifecycle states most likely to expose precedence drift.

use amux_ui::codex::{CodexCommand, CodexDecision, CodexPhase, SendGate};
use amux_ui::{
    Attention, Command, DisconnectReason, Model, Msg, StreamCloseReason, StreamMsg, Why,
};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "codex-agreement";

fn raw_base(truncated: bool, replay_complete: bool) -> Vec<Msg> {
    let mut messages = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&a_codex_agent(AGENT, "nova")),
        ],
        synced(),
        vec![stream(AGENT, StreamMsg::Opened { truncated })],
    ]);
    if replay_complete {
        messages.push(stream(AGENT, StreamMsg::ReplayComplete));
    }
    messages
}

fn with_rows(rows: Vec<serde_json::Value>) -> Vec<Msg> {
    seq([codex_base(AGENT), vec![batch(AGENT, 10, rows)]])
}

fn approval_rows() -> Vec<serde_json::Value> {
    vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1",
            "command":"cargo test"}),
        json!({"type":"amux.codex_approval_required","request_id":"request-1",
            "availableDecisions":["accept","decline"]}),
    ]
}

fn named_states() -> Vec<(&'static str, Vec<Msg>)> {
    let ready = || vec![json!({"type":"amux.codex_ready"})];
    let active = || {
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
        ]
    };
    let completed = || {
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
            json!({"type":"turn/completed","turn":{"id":"turn-1","status":"completed"}}),
        ]
    };
    let interrupted = || {
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
            json!({"type":"turn/completed","turn":{"id":"turn-1","status":"interrupted"}}),
        ]
    };

    let mut read_only_with_ask = approval_rows();
    read_only_with_ask.push(json!({"type":"amux.codex_reconnect_error",
        "error":{"message":"writer unavailable"}}));

    let mut answer_in_flight = with_rows(approval_rows());
    answer_in_flight.push(command(
        op(17),
        Command::Codex(CodexCommand::Answer {
            agent: agent_id(AGENT),
            request_id: json!("request-1"),
            decision: CodexDecision::Accept,
        }),
    ));

    let mut active_input_in_flight = with_rows(active());
    active_input_in_flight.push(command(
        op(18),
        Command::Codex(CodexCommand::Steer {
            agent: agent_id(AGENT),
            text: "keep going".to_string(),
        }),
    ));

    vec![
        ("fresh/pre-ready", codex_base(AGENT)),
        ("replaying", raw_base(false, false)),
        ("truncated-blind", raw_base(true, true)),
        (
            "read-only",
            with_rows(vec![json!({"type":"amux.codex_reconnect_error",
                "error":{"message":"writer unavailable"}})]),
        ),
        ("read-only with pending ask", with_rows(read_only_with_ask)),
        ("pending ask live", with_rows(approval_rows())),
        (
            "unsupported ask outstanding",
            with_rows(vec![
                json!({"type":"amux.codex_ready"}),
                json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
                json!({"type":"item/tool/requestUserInput","itemId":"question-1",
                    "questions":[{"id":"choice","question":"Pick one"}]}),
            ]),
        ),
        ("active turn", with_rows(active())),
        ("completed turn", with_rows(completed())),
        ("interrupted turn", with_rows(interrupted())),
        (
            "gap",
            with_rows(vec![
                json!({"type":"amux.codex_ready"}),
                json!({"type":"amux.codex_gap","reason":"connection_lost"}),
            ]),
        ),
        (
            "stale",
            seq([
                with_rows(active()),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::HostUnreachable,
                    },
                )],
            ]),
        ),
        (
            "thread-closed",
            with_rows(vec![
                json!({"type":"amux.codex_ready"}),
                json!({"type":"thread/closed"}),
            ]),
        ),
        (
            "exited",
            seq([
                with_rows(ready()),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                    },
                )],
            ]),
        ),
        ("answer in flight", answer_in_flight),
        ("active turn with input in flight", active_input_in_flight),
        // Lifecycle churn no other chapter reaches. Registering these puts
        // them under the differential spec's per-Msg invariant sweep
        // (`wire_free::differential_fold_matches_live_state_after_every_msg`)
        // as well as the agreement assertions below, for free and forever.
        (
            "reconnect epoch churn over a folded layer",
            seq([
                with_rows(approval_rows()),
                vec![
                    disconnected(DisconnectReason::TransportError {
                        message: "connection reset".to_string(),
                    }),
                    connected("nova"),
                    host_up(&a_host("nova")),
                    agent_up(&a_codex_agent(AGENT, "nova")),
                ],
                synced(),
            ]),
        ),
        (
            "retryable close, reopen, replay again",
            seq([
                with_rows(active()),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::TransportError {
                            message: "connection reset".to_string(),
                        },
                    },
                )],
                vec![agent_up(&a_codex_agent(AGENT, "nova"))],
                vec![stream(AGENT, StreamMsg::Opened { truncated: false })],
                vec![batch(AGENT, 20, approval_rows())],
                vec![stream(AGENT, StreamMsg::ReplayComplete)],
            ]),
        ),
        (
            "approval arrives mid-replay",
            seq([
                raw_base(false, false),
                vec![batch(AGENT, 10, approval_rows())],
                vec![stream(AGENT, StreamMsg::ReplayComplete)],
            ]),
        ),
    ]
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    named_states()
}

fn projections(model: &Model) -> (CodexPhase, Attention, SendGate) {
    let agent = agent_id(AGENT);
    let card = model.agent(agent).expect("Codex card");
    (
        amux_ui::codex::phase(model, agent),
        card.attention,
        amux_ui::codex::send_gate(model, agent),
    )
}

#[test]
fn phase_attention_and_send_gate_stay_in_agreement_across_folded_states() {
    for (name, messages) in named_states() {
        let model = fold(messages);
        let (phase, attention, send_gate) = projections(&model);

        assert!(
            model.check_invariants().is_empty(),
            "{name}: checked projection invariant failed: {:?}",
            model.check_invariants()
        );
        assert!(
            attention != Attention::Idle || send_gate == SendGate::Ready,
            "{name}: Idle must never accompany a refusing gate ({send_gate:?})"
        );
        assert!(
            !matches!(attention, Attention::NeedsYou { .. })
                || matches!(
                    (attention, send_gate),
                    (
                        Attention::NeedsYou {
                            why: Why::Permission | Why::Question
                        },
                        SendGate::NeedsYou
                    ) | (Attention::NeedsYou { why: Why::Finished }, SendGate::Ready)
                ),
            "{name}: NeedsYou must identify an available answer/interrupt or a finished turn"
        );
        assert!(
            phase != CodexPhase::Unknown || attention == Attention::Unknown,
            "{name}: Unknown phase must degrade attention to Unknown"
        );
    }
}

#[test]
fn read_only_outranks_an_existing_ask_and_thread_closed_is_recoverable() {
    let read_only = fold(
        named_states()
            .into_iter()
            .find(|(name, _)| *name == "read-only with pending ask")
            .expect("state")
            .1,
    );
    assert_eq!(
        projections(&read_only),
        (CodexPhase::ReadOnly, Attention::Unknown, SendGate::ReadOnly)
    );

    let closed_rows = vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"thread/closed"}),
    ];
    let closed = fold(with_rows(closed_rows.clone()));
    assert_eq!(
        projections(&closed),
        (CodexPhase::Idle, Attention::Unknown, SendGate::Closed)
    );
    assert!(
        SendGate::Closed
            .refusal()
            .expect("closed refusal")
            .contains("closed")
    );

    let mut revived_rows = closed_rows;
    revived_rows.push(json!({"type":"amux.codex_ready"}));
    let revived = fold(with_rows(revived_rows));
    assert_eq!(
        projections(&revived),
        (CodexPhase::Idle, Attention::Idle, SendGate::Ready)
    );
}

#[test]
fn active_turn_with_input_in_flight_keeps_only_interrupt_safe() {
    let model = fold(
        named_states()
            .into_iter()
            .find(|(name, _)| *name == "active turn with input in flight")
            .expect("state")
            .1,
    );
    let agent = agent_id(AGENT);
    assert_eq!(
        projections(&model),
        (
            CodexPhase::Thinking,
            Attention::Working,
            SendGate::InputInFlight
        )
    );
    assert!(!amux_ui::codex::allows_prompt(&model, agent));
    assert!(!amux_ui::codex::allows_steer(&model, agent));
    assert!(!amux_ui::codex::allows_answer(&model, agent));
    assert!(amux_ui::codex::allows_interrupt(&model, agent));
}
