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
        "sessionId": chat_session_id("pong"),
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
        "session_id": chat_session_id("pong"),
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
        "sessionId": chat_session_id("pong"),
        "timestamp": "2026-08-11T22:10:12.000Z",
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": text}
    })
}

fn observation_only(mut messages: Vec<Msg>) -> Vec<Msg> {
    for message in &mut messages {
        if let Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent }) = message {
            agent.readonly = true;
        }
    }
    messages
}

fn named_states() -> Vec<(&'static str, Vec<Msg>)> {
    let mut echo_at_rest = chat_feed(AGENT, "pong");
    echo_at_rest.push(send_prompt(40, "next task"));

    let mut echo_with_error = echo_at_rest.clone();
    echo_with_error.push(batch(AGENT, 81, vec![api_error()]));

    vec![
        ("fresh empty rest", chat_base(AGENT)),
        (
            "observation-only fresh empty rest",
            observation_only(chat_base(AGENT)),
        ),
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
        (
            "permission ask",
            chat_feed_through(AGENT, "permission", ChatAnchor::PermissionRequest(0)),
        ),
        (
            "unverified question ask",
            chat_feed_through(
                AGENT,
                "question_other_single",
                ChatAnchor::PermissionRequest(0),
            ),
        ),
        (
            "unverified permission menu",
            seq([
                chat_base(AGENT),
                vec![batch(AGENT, 82, vec![new_permission_hook()])],
            ]),
        ),
        (
            "readonly permission ask",
            observation_only(chat_feed_through(
                AGENT,
                "permission",
                ChatAnchor::PermissionRequest(0),
            )),
        ),
        (
            "working turn",
            chat_feed_through(AGENT, "permission", ChatAnchor::Prompt(0)),
        ),
        (
            "observation-only working turn",
            observation_only(chat_feed_through(
                AGENT,
                "permission",
                ChatAnchor::Prompt(0),
            )),
        ),
        ("finished turn", chat_feed(AGENT, "pong")),
        (
            "stop presignal",
            chat_feed_through(AGENT, "pong", ChatAnchor::StopHook(0)),
        ),
        ("interrupted turn", chat_feed(AGENT, "interrupt")),
        (
            "transport unknown",
            seq([
                chat_feed_through(AGENT, "permission", ChatAnchor::Prompt(0)),
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
        (
            "observation-only exited",
            observation_only(seq([
                chat_feed(AGENT, "interrupt"),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                    },
                )],
            ])),
        ),
        ("echo at rest", echo_at_rest),
        ("echo coexisting with error", echo_with_error),
        (
            "stale working turn",
            seq([
                chat_feed_through(AGENT, "permission", ChatAnchor::Prompt(0)),
                vec![tick(10 + 601)],
            ]),
        ),
    ]
}

/// The seven Claude lifecycles checkpoint 3 found outside every registered
/// chapter. Keeping them together makes the coverage claim auditable; both
/// the agreement matrix below and `wire_free` inspect every intermediate Msg.
fn remaining_lifecycles() -> Vec<(&'static str, Vec<Msg>)> {
    let mut echo_with_ask = chat_feed(AGENT, "pong");
    echo_with_ask.push(send_prompt(40, "next task"));
    echo_with_ask.push(batch(AGENT, 80, vec![new_permission_hook()]));

    let mut offline_host = a_host("nova");
    offline_host.online = false;

    vec![
        (
            "retryable close -> reopen -> replay",
            seq([
                chat_feed_through(AGENT, "permission", ChatAnchor::Prompt(0)),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::TransportError {
                            message: "connection reset".to_string(),
                        },
                    },
                )],
                vec![agent_up(&an_agent(AGENT, "nova"))],
                vec![stream(AGENT, StreamMsg::Opened { truncated: false })],
                vec![batch(
                    AGENT,
                    20,
                    chat_rows_through("permission", ChatAnchor::PermissionRequest(0)),
                )],
                vec![stream(AGENT, StreamMsg::ReplayComplete)],
            ]),
        ),
        (
            "exit then agent re-upsert",
            seq([
                chat_feed(AGENT, "interrupt"),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                    },
                )],
                vec![agent_up(&an_agent(AGENT, "nova"))],
            ]),
        ),
        (
            "Closed AgentDeleted while card remains listed",
            seq([
                chat_feed_through(AGENT, "pong", ChatAnchor::StopHook(0)),
                vec![stream(
                    AGENT,
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentDeleted,
                    },
                )],
            ]),
        ),
        ("prompt echo in flight when an ask arrives", echo_with_ask),
        (
            "/clear relink during replay",
            seq([
                chat_feed(AGENT, "pong"),
                vec![batch(AGENT, 20, chat_rows("clear"))],
            ]),
        ),
        (
            "folded Claude layer on an offline host",
            seq([chat_feed(AGENT, "pong"), vec![host_up(&offline_host)]]),
        ),
        (
            "non-truncated ask mid-replay",
            seq([
                chat_feed(AGENT, "pong"),
                // A new session id is the `/clear` relink fact. The first
                // row opens a non-truncated layer replay while the kernel
                // stream remains Live; the hook then carries an apparent
                // ask whose resolving suffix is not authoritative yet.
                vec![batch(
                    AGENT,
                    20,
                    chat_rows_before("permission", ChatAnchor::TranscriptReady),
                )],
                vec![batch(
                    AGENT,
                    21,
                    vec![chat_row("permission", ChatAnchor::PermissionRequest(0))],
                )],
                vec![batch(
                    AGENT,
                    22,
                    vec![chat_row("permission", ChatAnchor::TranscriptReady)],
                )],
            ]),
        ),
    ]
}

fn agreement_cases() -> Vec<(&'static str, Vec<Msg>)> {
    named_states()
        .into_iter()
        .chain(remaining_lifecycles())
        .collect()
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    agreement_cases()
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
    let observer_readonly = model
        .agent(agent_id(AGENT))
        .is_some_and(|card| card.agent.readonly);
    if phase == ChatPhase::Replaying {
        assert_eq!(attention, Attention::Unknown, "{name} step {step}");
        assert_eq!(
            send_gate,
            if observer_readonly {
                SendGate::ReadOnly
            } else {
                SendGate::Replaying
            },
            "{name} step {step}"
        );
    }
    match attention {
        Attention::Idle => assert!(
            matches!(
                send_gate,
                SendGate::Ready | SendGate::Exited | SendGate::ReadOnly
            ),
            "{name} step {step}: Claude orderly exit is the deliberate Idle/refusal exception"
        ),
        Attention::NeedsYou {
            why: Why::Permission | Why::Question,
        } => assert!(
            matches!(send_gate, SendGate::NeedsYou | SendGate::ReadOnly),
            "{name} step {step}"
        ),
        Attention::NeedsYou { why: Why::Finished } => {
            assert!(
                matches!(send_gate, SendGate::Ready | SendGate::ReadOnly),
                "{name} step {step}"
            )
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
fn observation_only_preserves_claude_projections_but_refuses_interaction() {
    let pairs = [
        ("fresh empty rest", "observation-only fresh empty rest"),
        ("working turn", "observation-only working turn"),
        ("permission ask", "readonly permission ask"),
    ];
    for (writable_name, observer_name) in pairs {
        let state = |wanted| {
            fold(
                agreement_cases()
                    .into_iter()
                    .find(|(name, _)| *name == wanted)
                    .expect("named Claude state")
                    .1,
            )
        };
        let writable = state(writable_name);
        let observer = state(observer_name);
        let agent = agent_id(AGENT);
        let writable_projection = projections(&writable).expect("writable projection");
        let observer_projection = projections(&observer).expect("observer projection");
        assert_eq!(
            (observer_projection.0, observer_projection.1),
            (writable_projection.0, writable_projection.1),
            "{observer_name} keeps the visible session projection"
        );
        assert_eq!(observer_projection.2, SendGate::ReadOnly);
        assert!(!amux_ui::claude::allows_answer(&observer, agent));
        assert!(!amux_ui::claude::allows_interrupt(&observer, agent));
        assert_eq!(
            amux_ui::claude::mode_cycle_gate(&observer, agent),
            Some("agent is read-only — you are observing this session")
        );
        assert!(observer.check_invariants().is_empty());
    }

    let exited = fold(
        agreement_cases()
            .into_iter()
            .find(|(name, _)| *name == "observation-only exited")
            .expect("observer exited")
            .1,
    );
    assert_eq!(
        projections(&exited),
        Some((
            ChatPhase::Idle {
                tag: PhaseTag::Fact
            },
            Attention::Idle,
            SendGate::Exited
        )),
        "exited lifecycle outranks observer read-only"
    );
    assert!(exited.check_invariants().is_empty());
}

#[test]
fn phase_attention_and_gates_agree_after_every_message() {
    for (name, messages) in agreement_cases() {
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
            agreement_cases()
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
        projections(&state("prompt echo in flight when an ask arrives")),
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

    let offline = state("folded Claude layer on an offline host");
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
fn a_non_truncated_replay_prefix_outranks_its_held_ask_until_ready() {
    let mut lifecycle = remaining_lifecycles()
        .into_iter()
        .find(|(name, _)| *name == "non-truncated ask mid-replay")
        .expect("registered lifecycle")
        .1;
    let ready = lifecycle.pop().expect("ready row");

    let replaying = fold(lifecycle.clone());
    assert_eq!(claude_layer(&replaying, AGENT).ask_count(), 1);
    assert_eq!(
        projections(&replaying),
        Some((
            ChatPhase::Replaying,
            Attention::Unknown,
            SendGate::Replaying
        )),
        "an apparent ask in a replay prefix is not actionable"
    );

    lifecycle.push(ready);
    let authoritative = fold(lifecycle);
    assert_eq!(
        projections(&authoritative),
        Some((
            ChatPhase::NeedsYou {
                why: AskWhy::Permission,
                tag: PhaseTag::Fact,
            },
            Attention::NeedsYou {
                why: Why::Permission,
            },
            SendGate::NeedsYou,
        )),
        "the same held ask surfaces only after the new window's ready fact"
    );
}

#[test]
fn prompt_echo_ages_from_dispatch_while_its_send_gate_stays_closed() {
    let mut model = fold(seq([chat_feed(AGENT, "pong"), vec![tick(10 + 601)]]));

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
        chat_feed(AGENT, "pong"),
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
    let mut model = fold(chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::Prompt(0),
    ));

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
        chat_feed(AGENT, "pong"),
        vec![tick(10 + 601), send_prompt(43, "next task")],
        vec![host_up(&offline_host)],
    ]));

    assert_eq!(
        projections(&model).map(|(_, attention, gate)| (attention, gate)),
        Some((Attention::Unknown, SendGate::SendInFlight)),
        "offline-host degradation remains authoritative"
    );
}
