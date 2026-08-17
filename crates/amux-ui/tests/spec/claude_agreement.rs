//! Chapter 18 — Claude classification and projection agreement.
//!
//! Claude phase, effective fleet attention, prompt gate, and mode-cycle gate
//! are projections of one Claude-native condition. The exceptions here are
//! intentional Claude semantics, not Codex rules copied across layers.

use amux_ui::claude::{AskWhy, ChatPhase, PhaseTag};
use amux_ui::{
    Attention, ClaudeCommand, Command, Model, Msg, SendGate, StreamCloseReason, StreamMsg, Why,
};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "claude-agreement";

fn send_prompt(op_n: u8, text: &str) -> Msg {
    command(
        op(op_n),
        Command::Claude(ClaudeCommand::SendPrompt {
            agent: agent_id(AGENT),
            text: text.to_string(),
        }),
    )
}

fn api_error() -> serde_json::Value {
    json!({
        "type": "assistant",
        "uuid": "cccccccc-0000-4000-8000-000000000099",
        "sessionId": "9f635f35-5e8c-49a8-b035-8408c6981b11",
        "timestamp": "2026-08-11T22:00:05.000Z",
        "isApiErrorMessage": true,
        "error": "server_error",
        "message": {
            "id": "e0000000-0000-4000-8000-000000000099",
            "model": "<synthetic>",
            "role": "assistant",
            "stop_reason": "stop_sequence",
            "content": [{"type": "text", "text": "API error"}]
        }
    })
}

fn new_permission_hook() -> serde_json::Value {
    json!({
        "type": "hook.permission_request",
        "hook_event_name": "PermissionRequest",
        "prompt_id": "new-prompt-after-local-send",
        "session_id": "9f635f35-5e8c-49a8-b035-8408c6981b11",
        "tool_name": "Bash",
        "tool_input": {"command": "echo new-request"},
        "permission_mode": "default",
        "permission_suggestions": []
    })
}

fn reconciled_prompt(text: &str) -> serde_json::Value {
    json!({
        "type": "user",
        "uuid": "cccccccc-0000-4000-8000-000000000100",
        "sessionId": "9f635f35-5e8c-49a8-b035-8408c6981b11",
        "timestamp": "2026-08-11T22:10:12.000Z",
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": text}
    })
}

fn named_states() -> Vec<(&'static str, Vec<Msg>)> {
    let mut echo_at_rest = chat_feed(AGENT, "permission");
    echo_at_rest.push(send_prompt(40, "next task"));

    let mut echo_with_ask = echo_at_rest.clone();
    echo_with_ask.push(batch(AGENT, 80, vec![new_permission_hook()]));

    let mut echo_with_error = echo_at_rest.clone();
    echo_with_error.push(batch(AGENT, 81, vec![api_error()]));

    let mut offline_host = a_host("nova");
    offline_host.online = false;

    vec![
        ("fresh empty rest", chat_base(AGENT)),
        (
            "kernel replay",
            seq([
                vec![
                    connected("nova"),
                    host_up(&a_host("nova")),
                    agent_up(&an_agent(AGENT, "nova")),
                ],
                synced(),
                vec![stream(AGENT, StreamMsg::Opened { truncated: false })],
            ]),
        ),
        ("permission ask", chat_feed_prefix(AGENT, "permission", 8)),
        (
            "unverified question ask",
            chat_feed_prefix(AGENT, "question_other_single", 8),
        ),
        ("working turn", chat_feed_prefix(AGENT, "permission", 6)),
        ("finished turn", chat_feed(AGENT, "permission")),
        ("stop presignal", chat_feed(AGENT, "question_single")),
        ("interrupted turn", chat_feed(AGENT, "interrupt")),
        (
            "transport unknown",
            seq([
                chat_feed_prefix(AGENT, "permission", 6),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::HostUnreachable,
                    },
                )],
            ]),
        ),
        (
            "exited",
            seq([
                chat_feed(AGENT, "interrupt"),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                    },
                )],
            ]),
        ),
        ("echo at rest", echo_at_rest),
        ("echo coexisting with ask", echo_with_ask),
        ("echo coexisting with error", echo_with_error),
        (
            "offline finished turn",
            seq([chat_feed(AGENT, "permission"), vec![host_up(&offline_host)]]),
        ),
        (
            "stale working turn",
            seq([
                chat_feed_prefix(AGENT, "permission", 6),
                vec![tick(10 + 601)],
            ]),
        ),
    ]
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    named_states()
        .into_iter()
        // C2 owns registration of the prompt-echo/ask lifecycle. C1 still
        // exercises every intermediate projection below without claiming
        // that reserved lifecycle-registration work.
        .filter(|(name, _)| *name != "echo coexisting with ask")
        .collect()
}

fn projections(model: &Model) -> Option<(ChatPhase, Attention, SendGate)> {
    let agent = agent_id(AGENT);
    let card = model.agent(agent)?;
    Some((
        amux_ui::claude::phase(model, agent),
        model.effective_attention(card),
        amux_ui::claude::send_gate(model, agent),
    ))
}

fn assert_agreement(name: &str, step: usize, model: &Model) {
    let Some((phase, attention, send_gate)) = projections(model) else {
        return;
    };
    assert!(
        model.check_invariants().is_empty(),
        "{name} step {step}: structural invariant failed: {:?}",
        model.check_invariants()
    );
    assert!(
        phase != ChatPhase::Unknown || attention == Attention::Unknown,
        "{name} step {step}: Unknown phase must have Unknown effective attention"
    );
    if phase == ChatPhase::Replaying {
        assert_eq!(attention, Attention::Unknown, "{name} step {step}");
        assert_eq!(send_gate, SendGate::Replaying, "{name} step {step}");
    }
    match attention {
        Attention::Idle => assert!(
            matches!(send_gate, SendGate::Ready | SendGate::Exited),
            "{name} step {step}: Claude orderly exit is the deliberate Idle/refusal exception"
        ),
        Attention::NeedsYou {
            why: Why::Permission | Why::Question,
        } => assert_eq!(send_gate, SendGate::NeedsYou, "{name} step {step}"),
        Attention::NeedsYou { why: Why::Finished } => {
            assert_eq!(send_gate, SendGate::Ready, "{name} step {step}")
        }
        Attention::Working | Attention::Unknown => {}
    }
    if send_gate == SendGate::SendInFlight
        && !matches!(phase, ChatPhase::Unknown | ChatPhase::Errored)
    {
        let card = model.agent(agent_id(AGENT)).expect("projected Claude card");
        assert_eq!(
            card.attention,
            Attention::Working,
            "{name} step {step}: a send in flight projects cached Working attention"
        );
        assert!(
            matches!(attention, Attention::Working | Attention::Unknown),
            "{name} step {step}: effective send attention is Working until dispatch evidence ages out, while offline degradation is also Unknown"
        );
    }
}

#[test]
fn phase_attention_and_gates_agree_after_every_message() {
    for (name, messages) in named_states() {
        let mut model = Model::default();
        for (step, message) in messages.into_iter().enumerate() {
            amux_ui::update(&mut model, message);
            assert_agreement(name, step, &model);
        }
    }
}

#[test]
fn claude_specific_exceptions_and_send_precedence_are_explicit() {
    let state = |wanted| {
        fold(
            named_states()
                .into_iter()
                .find(|(name, _)| *name == wanted)
                .expect("named Claude state")
                .1,
        )
    };

    assert_eq!(
        projections(&state("fresh empty rest")),
        Some((
            ChatPhase::Idle {
                tag: PhaseTag::Inferred
            },
            Attention::Idle,
            SendGate::Ready
        ))
    );
    assert_eq!(
        projections(&state("exited")),
        Some((
            ChatPhase::Idle {
                tag: PhaseTag::Fact
            },
            Attention::Idle,
            SendGate::Exited
        ))
    );
    assert_eq!(
        projections(&state("echo coexisting with ask")),
        Some((
            ChatPhase::NeedsYou {
                why: AskWhy::Permission,
                tag: PhaseTag::Fact
            },
            Attention::Working,
            SendGate::SendInFlight
        ))
    );
    assert_eq!(
        projections(&state("echo coexisting with error")),
        Some((
            ChatPhase::Errored,
            Attention::Unknown,
            SendGate::SendInFlight
        ))
    );

    let offline = state("offline finished turn");
    assert!(matches!(
        amux_ui::claude::phase(&offline, agent_id(AGENT)),
        ChatPhase::Idle { .. }
    ));
    assert_eq!(
        projections(&offline).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Unknown, SendGate::Ready)),
        "offline degradation is one-way and does not rewrite the condition"
    );
    assert_eq!(
        projections(&state("stale working turn")),
        Some((ChatPhase::Unknown, Attention::Unknown, SendGate::Unknown))
    );
}

#[test]
fn prompt_echo_ages_from_dispatch_while_its_send_gate_stays_closed() {
    let mut model = fold(seq([chat_feed(AGENT, "permission"), vec![tick(10 + 601)]]));

    amux_ui::update(&mut model, send_prompt(41, "next task"));
    assert_eq!(
        projections(&model).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Working, SendGate::SendInFlight)),
        "the unresolved local echo is fresher than the old idle transcript"
    );

    amux_ui::update(&mut model, tick(10 + 601 + 601));
    assert_eq!(
        projections(&model).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Unknown, SendGate::SendInFlight)),
        "the unresolved echo's attention ages out without reopening unsafe input"
    );

    amux_ui::update(
        &mut model,
        batch(AGENT, 10 + 601 + 602, vec![reconciled_prompt("next task")]),
    );
    assert!(claude_layer(&model, AGENT).pending_echoes().is_empty());
    assert_eq!(
        projections(&model),
        Some((ChatPhase::Working, Attention::Working, SendGate::Working))
    );

    amux_ui::update(&mut model, tick(10 + 601 + 602 + 601));
    assert_eq!(
        projections(&model),
        Some((ChatPhase::Unknown, Attention::Unknown, SendGate::Unknown)),
        "after reconciliation, ordinary transcript staleness applies again"
    );

    let mut failed = fold(seq([
        chat_feed(AGENT, "permission"),
        vec![tick(10 + 601), send_prompt(42, "retry task")],
    ]));
    assert_eq!(
        projections(&failed).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Working, SendGate::SendInFlight))
    );
    amux_ui::update(&mut failed, op_failed(op(42), "send rejected"));
    assert!(claude_layer(&failed, AGENT).pending_echoes().is_empty());
    assert_eq!(
        projections(&failed).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::NeedsYou { why: Why::Finished }, SendGate::Ready)),
        "failed-send removal restores the transcript's prior projection"
    );
}

#[test]
fn echo_free_working_still_ages_from_transcript_delivery() {
    let mut model = fold(chat_feed_prefix(AGENT, "permission", 6));

    amux_ui::update(&mut model, tick(10 + 600));
    assert_eq!(
        projections(&model),
        Some((ChatPhase::Working, Attention::Working, SendGate::Working))
    );

    amux_ui::update(&mut model, tick(10 + 601));
    assert_eq!(
        projections(&model),
        Some((ChatPhase::Unknown, Attention::Unknown, SendGate::Unknown))
    );
}

#[test]
fn offline_host_still_outranks_a_fresh_prompt_echo() {
    let mut offline_host = a_host("nova");
    offline_host.online = false;
    let model = fold(seq([
        chat_feed(AGENT, "permission"),
        vec![tick(10 + 601), send_prompt(43, "next task")],
        vec![host_up(&offline_host)],
    ]));

    assert_eq!(
        projections(&model).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Unknown, SendGate::SendInFlight)),
        "offline-host degradation remains authoritative"
    );
}
