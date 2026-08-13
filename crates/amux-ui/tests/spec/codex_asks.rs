//! Chapter 15 — Codex asks: raw request context plus synthesized obligation.
//!
//! The answerable row contains only an opaque request id and wire-verbatim
//! choices. Display facts come from the immediately preceding raw request.
//! Unsupported `requestUserInput` is visible and blocks the turn, but creates
//! no answerable obligation in V1.

use amux_ui::codex::{ApprovalResolution, AskContext, CodexPhase, FeedEntryKind, WorkState};
use amux_ui::{Attention, Why};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "codex-asks";

fn ask_rows(request_id: Value) -> Vec<Value> {
    vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"exec-1","type":"commandExecution",
            "command":"cargo test","cwd":"/work","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1",
            "command":"cargo test","cwd":"/work","reason":"run tests?"}),
        json!({"type":"amux.codex_approval_required","request_id":request_id,
            "availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"x":1}},"cancel"]}),
    ]
}

fn model(rows: Vec<Value>) -> amux_ui::Model {
    fold(seq([codex_base(AGENT), vec![batch(AGENT, 10, rows)]]))
}

pub fn sequences() -> Vec<(&'static str, Vec<amux_ui::Msg>)> {
    vec![
        (
            "codex correlated command approval",
            seq([
                codex_base(AGENT),
                vec![batch(AGENT, 10, ask_rows(json!(7)))],
            ]),
        ),
        (
            "codex unsupported user question",
            seq([
                codex_base(AGENT),
                vec![batch(
                    AGENT,
                    20,
                    vec![
                        json!({"type":"amux.codex_ready"}),
                        json!({"type":"turn/started","turn":{"id":"t","status":"inProgress"}}),
                        json!({"type":"item/tool/requestUserInput","itemId":"q1","questions":[
                            {"id":"choice","question":"Pick one"}
                        ]}),
                    ],
                )],
            ]),
        ),
    ]
}

#[test]
fn raw_and_synthesized_rows_form_one_answerable_obligation() {
    let model = model(ask_rows(json!(7)));
    let layer = codex_layer(&model, AGENT);
    let ask = layer.ask_head().expect("ask");
    assert_eq!(ask.request_id, json!(7), "integer ids remain opaque JSON");
    assert_eq!(ask.available_decisions.as_array().unwrap().len(), 3);
    assert_eq!(ask.actions.len(), 3);
    assert!(ask.actions[0].decision.is_some());
    assert!(
        ask.actions[1].decision.is_none(),
        "object choice visible but disabled"
    );
    assert!(
        matches!(&ask.context, AskContext::Command { command, cwd, reason, .. }
        if command == "cargo test" && cwd.as_deref() == Some("/work") && reason.as_deref() == Some("run tests?"))
    );
    assert_eq!(
        model.agent(agent_id(AGENT)).unwrap().attention,
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
    assert!(matches!(
        amux_ui::codex::phase(&model, agent_id(AGENT)),
        CodexPhase::AwaitingApproval { .. }
    ));
}

#[test]
fn an_approval_without_immediately_preceding_context_is_not_rendered_as_an_ask() {
    let model = model(vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1","command":"x"}),
        json!({"type":"warning","message":"intervening"}),
        json!({"type":"amux.codex_approval_required","request_id":"lost","availableDecisions":["accept"]}),
    ]);
    let layer = codex_layer(&model, AGENT);
    assert_eq!(layer.ask_count(), 0);
    assert!(layer.entries().any(|entry| matches!(&entry.kind,
        FeedEntryKind::Error(error) if error.message.contains("without its preceding request context"))));
}

#[test]
fn only_satisfied_resolutions_continue_work_and_five_death_reasons_abandon_it() {
    for (reason, expected) in [
        ("answered", WorkState::Running),
        ("answered_elsewhere", WorkState::Running),
        (
            "response_failed",
            WorkState::Abandoned {
                reason: ApprovalResolution::ResponseFailed,
            },
        ),
        (
            "connection_lost",
            WorkState::Abandoned {
                reason: ApprovalResolution::ConnectionLost,
            },
        ),
        (
            "queue_overflow",
            WorkState::Abandoned {
                reason: ApprovalResolution::QueueOverflow,
            },
        ),
        (
            "event_stream_error",
            WorkState::Abandoned {
                reason: ApprovalResolution::EventStreamError,
            },
        ),
        (
            "session_stopped",
            WorkState::Abandoned {
                reason: ApprovalResolution::SessionStopped,
            },
        ),
    ] {
        let mut rows = ask_rows(json!("request"));
        rows.push(json!({"type":"amux.codex_approval_resolved",
            "request_id":"request","reason":reason}));
        let model = model(rows);
        let layer = codex_layer(&model, AGENT);
        assert_eq!(layer.ask_count(), 0, "{reason} closes the obligation");
        let state = layer
            .entries()
            .find_map(|entry| match &entry.kind {
                FeedEntryKind::Work(work) if work.item_id == "exec-1" => Some(work.state.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(state, expected, "resolution semantics for {reason}");
    }
}

#[test]
fn unsupported_user_input_blocks_without_creating_an_answerable_ask() {
    let model = model(vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"t","status":"inProgress"}}),
        json!({"type":"item/tool/requestUserInput","itemId":"question-1","questions":[
            {"id":"choice","question":"Pick one"}
        ]}),
    ]);
    let layer = codex_layer(&model, AGENT);
    assert_eq!(layer.ask_count(), 0);
    assert!(layer.entries().any(|entry| matches!(&entry.kind,
        FeedEntryKind::Work(work) if work.state == WorkState::BlockedUnsupported)));
    assert!(matches!(
        amux_ui::codex::phase(&model, agent_id(AGENT)),
        CodexPhase::BlockedUnsupported { .. }
    ));
    assert_eq!(
        model.agent(agent_id(AGENT)).unwrap().attention,
        Attention::NeedsYou { why: Why::Question }
    );
}
