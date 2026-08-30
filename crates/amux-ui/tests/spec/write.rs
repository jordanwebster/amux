//! Chapter 13 — The write path: intents, the seq guard, the optimistic
//! lifecycle.
//!
//! `docs/CHAT.md` §The keystroke seam (C6), §Composer and control (D2–D4),
//! §Asks lifecycle (C5). Typed intents dispatch as Commands; the reducer
//! checks them against the ask in front of the user and hands the shell a
//! seq-guarded `SendInput` effect carrying the intent — never keystrokes,
//! which the session chooses; outcomes return as finished-op state —
//! a lost outcome must never leave a spinner. Confirmation is never the
//! send: prompts confirm by echo reconciliation against the transcript's
//! user row (content equality — the injected paste lands byte-identical,
//! §18d), ask answers by the transcript's resolution fact. Failures
//! resurface with the failure stated; remote resolution wins over local
//! pending state.
//!
//! Fixture-grounded: the permission / question_single / plan_approve /
//! mode_cycle recordings (Claude 2.1.251) provide the asks, cursors, and
//! mode facts these flows drive.

use amux_ui::claude::AskState;
use amux_ui::claude::answer::{
    AskAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};
use amux_ui::claude_io::Intent;
use amux_ui::{
    ClaudeCommand, Command, Effect, InputPayload, Msg, NOT_CONNECTED_ERROR, OpOutcome, SendGate,
    StreamMsg,
};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "fix-auth-bug";

fn agent() -> amux_ui::AgentId {
    agent_id(AGENT)
}

fn send_prompt(op_n: u8, text: &str) -> Msg {
    command(
        op(op_n),
        Command::Claude(ClaudeCommand::SendPrompt {
            agent: agent(),
            text: text.to_string(),
        }),
    )
}

fn answer_ask(op_n: u8, ask: u64, answer: AskAnswer) -> Msg {
    command(
        op(op_n),
        Command::Claude(ClaudeCommand::AnswerAsk {
            agent: agent(),
            ask,
            answer,
        }),
    )
}

fn allow_once(op_n: u8, ask: u64) -> Msg {
    answer_ask(
        op_n,
        ask,
        AskAnswer::Permission(PermissionAnswer::AllowOnce),
    )
}

fn the_layer(model: &amux_ui::Model) -> &amux_ui::claude::ClaudeLayer {
    claude_layer(model, AGENT)
}

/// The finished outcome's error message, or a panic when the op is not
/// finished / not an error.
fn failure_message(model: &amux_ui::Model, op_n: u8) -> String {
    let finished = model.finished_op(op(op_n)).expect("op finished");
    match &finished.outcome {
        OpOutcome::Error { error } => error.message.clone(),
        other => panic!("expected an error outcome, got {other:?}"),
    }
}

/// Every SendInput effect of a fold, destructured (the base sequences
/// also emit stream-management effects, which are not under test here).
fn send_inputs(effects: &[Effect]) -> Vec<(u64, Intent, bool)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendInput {
                agent: effect_agent,
                payload:
                    InputPayload::Claude {
                        expected_seq,
                        intent,
                        retry_stale,
                    },
                ..
            } => {
                assert_eq!(*effect_agent, agent());
                Some((*expected_seq, intent.clone(), *retry_stale))
            }
            _ => None,
        })
        .collect()
}

/// The newest SendInput effect of a fold.
fn send_input_effect(effects: &[Effect]) -> (u64, Intent, bool) {
    send_inputs(effects)
        .pop()
        .expect("a SendInput effect was dispatched")
}

/// The answer intent for the ask the session calls `ask_id`.
fn answer_intent(ask_id: &str, answer: AskAnswer) -> Intent {
    Intent::Answer {
        ask_id: ask_id.to_string(),
        answer,
    }
}

/// The permission fixture's hook carries a `prompt_id` and no tool_use id,
/// so that is the name the session knows every ask of that session by.
const PERMISSION_ASK: &str = "2974ae53-275b-4574-b836-d68d711dd08c";

fn mode_cycle_rest() -> Vec<Msg> {
    seq([
        chat_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![
                chat_row("permission", ChatAnchor::PermissionMode(0)),
                chat_row("permission", ChatAnchor::TranscriptReady),
            ],
        )],
    ])
}

// --- prompts (B1/D2) --------------------------------------------------------

/// An idle chat accepts a prompt: the dispatch yields ONE seq-guarded
/// SendInput effect carrying the prompt intent, the expected_seq is the
/// layer's stream cursor, and the optimistic echo pends with the
/// normalized text (the reconciliation key).
#[test]
fn a_sent_prompt_becomes_a_prompt_intent_and_a_pending_echo() {
    let mut msgs = chat_feed(AGENT, "pong");
    msgs.push(send_prompt(1, "fix the sync bug\r\nthen run the tests"));
    let (model, effects) = fold_with_effects(msgs);

    let input_id = effects.iter().find_map(|effect| match effect {
        Effect::SendInput { input_id, .. } => Some(input_id),
        _ => None,
    });
    assert_eq!(
        input_id.map(Vec::as_slice),
        Some(op(1).0.as_bytes().as_slice()),
        "the reducer exposes a deterministic protocol correlation id"
    );

    let (expected_seq, intent, retry_stale) = send_input_effect(&effects);
    assert_eq!(
        expected_seq,
        10 + chat_rows("pong").len() as u64,
        "the layer's cursor is the seq guard"
    );
    assert!(
        !retry_stale,
        "a prompt is positional — it never auto-retries"
    );
    assert_eq!(
        intent,
        Intent::Prompt {
            text: "fix the sync bug\nthen run the tests".to_string()
        },
        "the prompt leaves as its text; CRLF normalized"
    );

    let echoes = the_layer(&model).pending_echoes();
    assert_eq!(echoes.len(), 1);
    assert_eq!(echoes[0].op, op(1));
    assert_eq!(echoes[0].text, "fix the sync bug\nthen run the tests");
    assert_eq!(
        amux_ui::claude::send_gate(&model, agent()),
        SendGate::SendInFlight,
        "one echo in flight gates the next send"
    );
    assert_eq!(model.pending_ops().count(), 1);
}

/// The transcript's human user row with the same content IS the echo's
/// confirmation (B1): the echo collapses into the real entry. The
/// InputSent outcome only clears the op — injection is not confirmation.
#[test]
fn the_transcript_row_reconciles_the_echo() {
    let text = "fix the sync bug";
    let mut msgs = chat_feed(AGENT, "pong");
    msgs.push(send_prompt(1, text));
    msgs.push(op_result(op(1), OpOutcome::InputSent));
    let (model, _) = fold_with_effects(msgs.clone());
    assert_eq!(
        the_layer(&model).pending_echoes().len(),
        1,
        "InputSent alone never reconciles — the row does"
    );

    // The prompt row lands (same session as the fixture, human origin,
    // identical content — §18d: the paste lands byte-identical).
    msgs.push(batch(
        AGENT,
        40,
        vec![json!({
            "type": "user",
            "uuid": "eeeeeeee-0000-4000-8000-000000000001",
            "sessionId": chat_session_id("pong"),
            "timestamp": "2026-08-12T09:00:00.000Z",
            "message": {"role": "user", "content": text},
            "origin": {"kind": "human"},
            "promptSource": "typed",
        })],
    ));
    let model = fold(msgs);
    assert_eq!(the_layer(&model).pending_echoes().len(), 0, "reconciled");
    assert_eq!(
        amux_ui::claude::send_gate(&model, agent()),
        SendGate::Working,
        "the landed prompt opened the turn"
    );
}

/// A failed send removes the echo and states the failure as finished-op
/// state; the draft itself is renderer ViewState and resurfaces from
/// there (D1) — the model carries the fact.
#[test]
fn a_failed_prompt_send_drops_the_echo_and_states_the_failure() {
    let mut msgs = chat_feed(AGENT, "pong");
    msgs.push(send_prompt(1, "fix the sync bug"));
    msgs.push(op_failed(op(1), "input raced the session — seq moved"));
    let model = fold(msgs);
    assert_eq!(the_layer(&model).pending_echoes().len(), 0);
    assert_eq!(
        failure_message(&model, 1),
        "input raced the session — seq moved"
    );
    assert_eq!(
        amux_ui::claude::send_gate(&model, agent()),
        SendGate::Ready,
        "the failed send releases the in-flight gate"
    );
}

/// D2: send is gated on phase — the reducer refuses with the gate stated
/// and emits NO effect; the composer footer derives the same gate from
/// `Model::claude_send_gate` (one derivation).
#[test]
fn send_is_gated_by_phase() {
    // Working: the prompt row landed, no turn-end signal (permission
    // fixture cut mid-first-turn, before the hook).
    let mut working = chat_feed_through(AGENT, "permission", ChatAnchor::Prompt(0));
    working.push(send_prompt(1, "another thought"));
    let (model, effects) = fold_with_effects(working);
    assert!(
        send_inputs(&effects).is_empty(),
        "a gated send leaves no effect"
    );
    assert_eq!(
        amux_ui::claude::send_gate(&model, agent()),
        SendGate::Working
    );
    assert_eq!(failure_message(&model, 1), "send gated while working");

    // Needs-you: the ask panel owns the keystroke channel (C1).
    let mut needs_you = chat_feed_through(AGENT, "permission", ChatAnchor::PermissionRequest(0));
    needs_you.push(send_prompt(2, "hello"));
    let (model, effects) = fold_with_effects(needs_you);
    assert!(send_inputs(&effects).is_empty());
    assert_eq!(
        failure_message(&model, 2),
        "send gated — answer the pending ask"
    );

    // Replaying: rows folding without the ready marker or kernel replay
    // completion.
    let mut replaying = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent(AGENT, "nova")),
        ],
        synced(),
        vec![
            stream(AGENT, StreamMsg::Opened { truncated: false }),
            batch(
                AGENT,
                10,
                vec![json!({
                    "type": "user",
                    "uuid": "eeeeeeee-0000-4000-8000-000000000002",
                    "sessionId": "9f635f35-5e8c-49a8-b035-8408c6981b11",
                    "timestamp": "2026-08-12T09:00:00.000Z",
                    "message": {"role": "user", "content": "mid-replay"},
                    "origin": {"kind": "human"},
                    "promptSource": "typed",
                })],
            ),
        ],
    ]);
    replaying.push(send_prompt(3, "hello"));
    let (model, effects) = fold_with_effects(replaying);
    assert!(send_inputs(&effects).is_empty());
    assert_eq!(failure_message(&model, 3), "send gated while replaying");

    // An empty prompt is a typed refusal stated locally, not a round trip.
    let mut empty = chat_feed(AGENT, "pong");
    empty.push(send_prompt(4, "   "));
    let (model, effects) = fold_with_effects(empty);
    assert!(send_inputs(&effects).is_empty());
    assert_eq!(failure_message(&model, 4), "prompt must not be empty");

    // Disconnected: input commands fail fast like every command — there
    // is no offline queue.
    let (model, effects) = fold_with_effects(vec![send_prompt(5, "hello")]);
    assert!(effects.is_empty());
    assert_eq!(failure_message(&model, 5), NOT_CONNECTED_ERROR);
}

/// The in-band-delimiter guard (P2): prompt text carrying a literal
/// paste terminator (or any control byte) is refused whole — a broken-out
/// paste would run the remainder as live keystrokes in the remote
/// session. No effect, no echo, the gate stays open.
#[test]
fn a_prompt_carrying_the_paste_terminator_is_refused() {
    let mut msgs = chat_feed(AGENT, "pong");
    msgs.push(send_prompt(1, "benign start\u{1b}[201~1\r"));
    let (model, effects) = fold_with_effects(msgs);
    assert!(send_inputs(&effects).is_empty(), "no bytes leave");
    assert_eq!(the_layer(&model).pending_echoes().len(), 0, "no echo");
    assert!(
        failure_message(&model, 1).contains("control characters"),
        "the refusal states the class"
    );
    assert_eq!(amux_ui::claude::send_gate(&model, agent()), SendGate::Ready);
}

// --- ask answers (C5/C6) ----------------------------------------------------

/// Rows for a two-ask queue: the ready marker, a human prompt, and two
/// distinct suggestion-carrying permission hooks (the shape claude's
/// menu digits are verified against).
fn two_ask_rows() -> Vec<serde_json::Value> {
    let hook = |command: &str| {
        json!({
            "type": "hook.permission_request",
            "hook_event_name": "PermissionRequest",
            "session_id": "22222222-2222-4222-8222-222222222222",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "permission_mode": "default",
            "permission_suggestions": [
                {"type": "addDirectories", "destination": "session", "directories": ["/work"]}
            ],
        })
    };
    vec![
        json!({"type": "amux.transcript_ready"}),
        json!({
            "type": "user",
            "uuid": "eeeeeeee-0000-4000-8000-000000000010",
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-12T09:00:00.000Z",
            "message": {"role": "user", "content": "do both things"},
            "origin": {"kind": "human"},
            "promptSource": "typed",
        }),
        hook("echo first"),
        hook("echo second"),
    ]
}

/// Claude's remote menu only displays the HEAD of the queue (P1): an
/// answer addressed to a later queued ask would apply that ask's digits
/// to the head's menu — the wrong request. A non-head target refuses
/// without bytes and touches nothing; the head answers normally.
#[test]
fn an_answer_addressed_past_the_head_refuses_without_bytes() {
    let mut msgs = seq([chat_base(AGENT), vec![batch(AGENT, 10, two_ask_rows())]]);
    msgs.push(allow_once(1, 1));
    let (model, effects) = fold_with_effects(msgs.clone());
    assert!(
        send_inputs(&effects).is_empty(),
        "no bytes for a non-head target"
    );
    assert_eq!(
        failure_message(&model, 1),
        "ask is queued behind the current menu — answer the head ask first"
    );
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 2, "both asks untouched");
    assert_eq!(layer.ask_head().expect("head").id, 0);
    assert!(
        layer.asks().all(|ask| ask.state == AskState::Pending),
        "no ask flipped optimistic"
    );

    // The head answers normally.
    msgs.push(allow_once(2, 0));
    let (model, effects) = fold_with_effects(msgs);
    let (_, intent, _) = send_input_effect(&effects);
    assert_eq!(
        intent,
        answer_intent(
            // Neither synthetic hook carries an id, so the session names
            // the ask after the session and the tool.
            "permission:22222222-2222-4222-8222-222222222222:Bash",
            AskAnswer::Permission(PermissionAnswer::AllowOnce),
        )
    );
    assert!(matches!(
        the_layer(&model).ask_head().expect("head").state,
        AskState::AnsweredOptimistic { .. }
    ));
}

/// Allow-once on the fixture's permission ask: the intent names the ask
/// the session announced, the ask flips to answered-optimistic carrying
/// the answer (the pending marker's data), and the phase keeps stating
/// needs-you until the transcript confirms. The ask is addressable from
/// the hook row alone — no wait for the transcript to correlate it.
#[test]
fn an_allow_once_answer_names_the_ask_and_flips_it_optimistic() {
    let mut msgs = chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::ToolUse {
            name: "Bash",
            occurrence: 0,
        },
    );
    msgs.push(allow_once(1, 0));
    let (model, effects) = fold_with_effects(msgs);

    let (expected_seq, intent, retry_stale) = send_input_effect(&effects);
    assert_eq!(
        expected_seq,
        10 + chat_rows_through(
            "permission",
            ChatAnchor::ToolUse {
                name: "Bash",
                occurrence: 0,
            },
        )
        .len() as u64,
        "cursor at the correlated tool use"
    );
    assert!(!retry_stale, "menu answers are positional");
    assert_eq!(
        intent,
        answer_intent(
            PERMISSION_ASK,
            AskAnswer::Permission(PermissionAnswer::AllowOnce)
        )
    );

    let ask = the_layer(&model).ask_head().expect("still queued");
    let AskState::AnsweredOptimistic {
        op: in_flight,
        answer,
    } = &ask.state
    else {
        panic!("answered-optimistic, got {:?}", ask.state);
    };
    assert_eq!(*in_flight, op(1));
    assert_eq!(
        *answer,
        AskAnswer::Permission(PermissionAnswer::AllowOnce),
        "the pending marker carries the answer"
    );
}

/// The transcript's resolution fact confirms the optimistic answer: the
/// ask leaves the queue (the collapsed B5 fact renders from the tool
/// entry) — the InputSent outcome resolved only the op.
#[test]
fn the_resolution_fact_confirms_the_optimistic_answer() {
    let mut msgs = chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::ToolUse {
            name: "Bash",
            occurrence: 0,
        },
    );
    msgs.push(allow_once(1, 0));
    msgs.push(op_result(op(1), OpOutcome::InputSent));
    let (mid, _) = fold_with_effects(msgs.clone());
    assert_eq!(the_layer(&mid).ask_count(), 1, "awaiting the fact");

    // The fixture's own resolution row (the non-error tool_result).
    msgs.push(batch(
        AGENT,
        21,
        vec![chat_row("permission", ChatAnchor::ToolResult(0))],
    ));
    let model = fold(msgs);
    assert_eq!(the_layer(&model).ask_count(), 0, "confirmed and collapsed");
}

/// A seq-mismatch or transport failure resurfaces the ask with the
/// failure stated (C5) — never a stuck spinner — and the resurfaced ask
/// accepts a fresh answer.
#[test]
fn a_failed_answer_send_resurfaces_the_ask() {
    let mut msgs = chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::ToolUse {
            name: "Bash",
            occurrence: 0,
        },
    );
    msgs.push(allow_once(1, 0));
    msgs.push(op_failed(op(1), "sequence number mismatch: input raced"));
    let (model, _) = fold_with_effects(msgs.clone());
    let ask = the_layer(&model).ask_head().expect("resurfaced");
    assert_eq!(
        ask.state,
        AskState::SendFailed {
            message: "sequence number mismatch: input raced".to_string()
        }
    );

    // Re-answering the resurfaced ask dispatches a fresh intent.
    msgs.push(allow_once(2, 0));
    let (model, effects) = fold_with_effects(msgs);
    let (_, intent, _) = send_input_effect(&effects);
    assert_eq!(
        intent,
        answer_intent(
            PERMISSION_ASK,
            AskAnswer::Permission(PermissionAnswer::AllowOnce)
        )
    );
    assert!(matches!(
        the_layer(&model).ask_head().expect("queued").state,
        AskState::AnsweredOptimistic { .. }
    ));
}

/// Remote resolution wins over local pending state (C5): once the
/// transcript resolves the ask, a late failure for the superseded local
/// answer resurrects nothing — the failure is still finished-op state.
#[test]
fn remote_resolution_wins_over_a_late_send_failure() {
    let mut msgs = chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::ToolUse {
            name: "Bash",
            occurrence: 0,
        },
    );
    msgs.push(allow_once(1, 0));
    // Another client's allow lands as the resolution row…
    msgs.push(batch(
        AGENT,
        21,
        vec![chat_row("permission", ChatAnchor::ToolResult(0))],
    ));
    // …then the local send's failure arrives late.
    msgs.push(op_failed(op(1), "transport error: connection reset"));
    let model = fold(msgs);
    assert_eq!(the_layer(&model).ask_count(), 0, "no ghost ask");
    assert_eq!(
        failure_message(&model, 1),
        "transport error: connection reset",
        "the failure fact still lands as finished-op state"
    );
}

/// Answer routing is typed end to end: an answer that does not fit the
/// ask refuses without bytes; a second answer while one is in flight
/// refuses; an answer for a resolved ask states that resolution won.
#[test]
fn misfitting_late_and_duplicate_answers_refuse_without_bytes() {
    // Mismatched kind: a plan answer to a Bash permission.
    let mut msgs = chat_feed_through(
        AGENT,
        "permission",
        ChatAnchor::ToolUse {
            name: "Bash",
            occurrence: 0,
        },
    );
    msgs.push(answer_ask(1, 0, AskAnswer::Plan(PlanAnswer::ApproveAuto)));
    let (model, effects) = fold_with_effects(msgs.clone());
    assert!(
        send_inputs(&effects).is_empty(),
        "no bytes for a misfit answer"
    );
    assert!(failure_message(&model, 1).contains("does not fit the ask"));
    assert_eq!(
        the_layer(&model).ask_head().expect("queued").state,
        AskState::Pending,
        "the ask is untouched"
    );

    // Duplicate: answering again while the first is in flight.
    msgs.push(allow_once(2, 0));
    msgs.push(allow_once(3, 0));
    let (model, _) = fold_with_effects(msgs);
    assert_eq!(
        failure_message(&model, 3),
        "answer already in flight — awaiting confirmation"
    );

    // Resolved: the whole fixture resolves both asks; answering is late.
    let mut resolved = chat_feed_through(
        AGENT,
        "permission_deny_feedback",
        ChatAnchor::TurnDuration(0),
    );
    resolved.push(allow_once(4, 0));
    let (model, effects) = fold_with_effects(resolved);
    assert!(send_inputs(&effects).is_empty());
    assert_eq!(failure_message(&model, 4), "ask already resolved");
}

#[test]
fn a_prompt_echo_refuses_a_later_ask_answer_with_truthful_wording() {
    let mut source = chat_feed(AGENT, "pong");
    source.push(send_prompt(40, "next task"));
    source.push(batch(
        AGENT,
        80,
        vec![json!({
            "type": "hook.permission_request",
            "hook_event_name": "PermissionRequest",
            "prompt_id": "new-prompt-after-local-send",
            "session_id": chat_session_id("pong"),
            "tool_name": "Bash",
            "tool_input": {"command": "echo new-request"},
            "permission_mode": "default",
            "permission_suggestions": []
        })],
    ));
    let ask = the_layer(&fold(source.clone()))
        .ask_head()
        .expect("new ask coexists with the prompt echo")
        .id;
    source.push(allow_once(41, ask));

    let (model, effects) = fold_with_effects(source);
    assert_eq!(
        send_inputs(&effects).len(),
        1,
        "only the original prompt dispatches"
    );
    assert_eq!(
        failure_message(&model, 41),
        "send gated — a prompt is in flight"
    );
    assert!(matches!(
        the_layer(&model)
            .ask_head()
            .expect("ask remains pending")
            .state,
        AskState::Pending
    ));
}

/// Question and plan-review answers ride the same dispatch: each leaves
/// as the selection the user made against the ask the session named, and
/// nothing about the menu's layout travels with it.
#[test]
fn question_and_plan_answers_dispatch_their_typed_selections() {
    let mut question = chat_feed_through(
        AGENT,
        "question_single",
        ChatAnchor::ToolUse {
            name: "AskUserQuestion",
            occurrence: 0,
        },
    );
    question.push(answer_ask(
        1,
        0,
        AskAnswer::Question(QuestionResponse {
            answers: vec![QuestionAnswer {
                selected: vec![0],
                other: None,
            }],
        }),
    ));
    let (_, effects) = fold_with_effects(question);
    let (_, intent, _) = send_input_effect(&effects);
    assert_eq!(
        intent,
        answer_intent(
            "22ec6db8-b8ed-4add-94d1-0a8c3d944e0a",
            AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0],
                    other: None,
                }],
            }),
        ),
        "the chosen option travels, not the row it sits on"
    );

    let mut plan = chat_feed_through(AGENT, "plan_approve", ChatAnchor::PermissionRequest(0));
    plan.push(answer_ask(2, 0, AskAnswer::Plan(PlanAnswer::ApproveManual)));
    let (model, effects) = fold_with_effects(plan);
    let (_, intent, _) = send_input_effect(&effects);
    assert_eq!(
        intent,
        answer_intent(
            "389a1365-92b6-41f7-91e3-ec33c09165cd",
            AskAnswer::Plan(PlanAnswer::ApproveManual),
        )
    );
    assert!(matches!(
        the_layer(&model).ask_head().expect("queued").state,
        AskState::AnsweredOptimistic { .. }
    ));
}

// --- interrupt (D3) ---------------------------------------------------------

/// Interrupt is allowed in every state — working, ask pending, all of it —
/// and is the one stale-retryable intent (its meaning is not positional).
/// The transcript's interrupt rows then record the entry and close the
/// asks; the outcome only resolves the op.
#[test]
fn interrupt_dispatches_from_any_state_and_the_rows_close_the_asks() {
    let mut msgs = chat_feed_through(AGENT, "permission", ChatAnchor::PermissionRequest(0));
    msgs.push(command(
        op(1),
        Command::Claude(ClaudeCommand::Interrupt { agent: agent() }),
    ));
    let (model, effects) = fold_with_effects(msgs.clone());
    let (expected_seq, intent, retry_stale) = send_input_effect(&effects);
    assert_eq!(
        expected_seq,
        10 + chat_rows_through("permission", ChatAnchor::PermissionRequest(0)).len() as u64
    );
    assert!(retry_stale, "interrupt retries a stale seq mechanically");
    assert_eq!(intent, Intent::Interrupt);
    assert_eq!(the_layer(&model).ask_count(), 1, "dispatch closes nothing");

    // The §17 artifacts close the ask and record the entry (B8).
    msgs.push(op_result(op(1), OpOutcome::InputSent));
    msgs.push(batch(
        AGENT,
        30,
        vec![json!({
            "type": "user",
            "uuid": "eeeeeeee-0000-4000-8000-000000000003",
            "sessionId": chat_session_id("permission"),
            "timestamp": "2026-08-12T09:00:01.000Z",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user for tool use]"}
            ]},
        })],
    ));
    let model = fold(msgs);
    assert_eq!(the_layer(&model).ask_count(), 0, "the rows closed the ask");
}

#[test]
fn observation_only_refuses_every_claude_action_before_local_mutation() {
    let mut source = chat_feed_through(AGENT, "permission", ChatAnchor::PermissionRequest(0));
    for message in &mut source {
        if let Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent }) = message {
            agent.readonly = true;
        }
    }
    let before = fold(source.clone());
    let before_layer = the_layer(&before).clone();
    assert!(matches!(
        before_layer.ask_head().expect("permission ask").state,
        AskState::Pending
    ));

    let cases = [
        (
            20,
            ClaudeCommand::SendPrompt {
                agent: agent(),
                text: "new work".to_string(),
            },
        ),
        (
            21,
            ClaudeCommand::AnswerAsk {
                agent: agent(),
                ask: 0,
                answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
            },
        ),
        (22, ClaudeCommand::Interrupt { agent: agent() }),
        (23, ClaudeCommand::CyclePermissionMode { agent: agent() }),
    ];

    for (op_n, native) in cases {
        let (model, effects) = fold_with_effects(seq([
            source.clone(),
            vec![command(op(op_n), Command::Claude(native))],
        ]));
        assert!(send_inputs(&effects).is_empty());
        assert_eq!(model.pending_ops().count(), 0);
        assert_eq!(
            failure_message(&model, op_n),
            "agent is read-only — you are observing this session"
        );
        assert_eq!(the_layer(&model), &before_layer);
        assert!(the_layer(&model).pending_echoes().is_empty());
        assert!(matches!(
            the_layer(&model)
                .ask_head()
                .expect("permission ask retained")
                .state,
            AskState::Pending
        ));
        assert_eq!(
            amux_ui::claude::send_gate(&model, agent()),
            SendGate::ReadOnly
        );
    }
}

// --- permission-mode cycle (D4) ---------------------------------------------

/// The cycle is one intent; the mode FACT returns via hook payloads — the
/// cycle emits no transcript row (the mode_cycle fixture's verdict), so
/// nothing flips optimistically and the hook's `permission_mode` is what
/// updates the session fact.
#[test]
fn the_mode_cycle_dispatches_an_intent_and_the_hook_payload_carries_the_fact() {
    // After the first turn the fixture's session mode is `default`.
    let mut msgs = mode_cycle_rest();
    msgs.push(command(
        op(1),
        Command::Claude(ClaudeCommand::CyclePermissionMode { agent: agent() }),
    ));
    let (model, effects) = fold_with_effects(msgs.clone());
    let (_, intent, retry_stale) = send_input_effect(&effects);
    assert!(!retry_stale);
    assert_eq!(intent, Intent::CyclePermissionMode);
    assert_eq!(
        the_layer(&model).session().permission_mode.as_deref(),
        Some("default"),
        "no optimistic flip — the mode is fact-sourced"
    );

    // The recorded hook.stop states the cycled mode as `acceptEdits`.
    msgs.push(op_result(op(1), OpOutcome::InputSent));
    msgs.push(batch(
        AGENT,
        30,
        vec![chat_row("mode_cycle", ChatAnchor::StopHook(0))],
    ));
    let model = fold(msgs);
    assert_eq!(
        the_layer(&model).session().permission_mode.as_deref(),
        Some("acceptEdits"),
        "the hook payload is the D4 fact source"
    );

    // While an ask pends, CSI Z would navigate the form instead: refused.
    let mut gated = chat_feed_through(AGENT, "permission", ChatAnchor::PermissionRequest(0));
    gated.push(command(
        op(2),
        Command::Claude(ClaudeCommand::CyclePermissionMode { agent: agent() }),
    ));
    let (model, effects) = fold_with_effects(gated);
    assert!(send_inputs(&effects).is_empty());
    assert_eq!(
        failure_message(&model, 2),
        "mode cycle gated while an ask is pending"
    );
}

// --- sequences --------------------------------------------------------------

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let prompt_flow = {
        let mut msgs = chat_feed(AGENT, "pong");
        msgs.push(send_prompt(1, "fix the sync bug"));
        msgs.push(op_result(op(1), OpOutcome::InputSent));
        msgs.push(batch(
            AGENT,
            40,
            vec![json!({
                "type": "user",
                "uuid": "eeeeeeee-0000-4000-8000-000000000001",
                "sessionId": chat_session_id("pong"),
                "timestamp": "2026-08-12T09:00:00.000Z",
                "message": {"role": "user", "content": "fix the sync bug"},
                "origin": {"kind": "human"},
                "promptSource": "typed",
            })],
        ));
        msgs
    };
    let answer_failure = {
        let mut msgs = chat_feed_through(
            AGENT,
            "permission",
            ChatAnchor::ToolUse {
                name: "Bash",
                occurrence: 0,
            },
        );
        msgs.push(allow_once(1, 0));
        msgs.push(op_failed(op(1), "sequence number mismatch: input raced"));
        msgs.push(allow_once(2, 0));
        msgs
    };
    let remote_wins = {
        let mut msgs = chat_feed_through(
            AGENT,
            "permission",
            ChatAnchor::ToolUse {
                name: "Bash",
                occurrence: 0,
            },
        );
        msgs.push(allow_once(1, 0));
        msgs.push(batch(
            AGENT,
            21,
            vec![chat_row("permission", ChatAnchor::ToolResult(0))],
        ));
        msgs.push(op_failed(op(1), "transport error: connection reset"));
        msgs
    };
    let mode_cycle = {
        let mut msgs = mode_cycle_rest();
        msgs.push(command(
            op(1),
            Command::Claude(ClaudeCommand::CyclePermissionMode { agent: agent() }),
        ));
        msgs.push(op_result(op(1), OpOutcome::InputSent));
        msgs.push(batch(
            AGENT,
            30,
            vec![chat_row("mode_cycle", ChatAnchor::StopHook(0))],
        ));
        msgs
    };
    let question_answer = {
        let mut msgs = chat_feed_through(
            AGENT,
            "question_single",
            ChatAnchor::ToolUse {
                name: "AskUserQuestion",
                occurrence: 0,
            },
        );
        msgs.push(answer_ask(
            1,
            0,
            AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0],
                    other: None,
                }],
            }),
        ));
        msgs.push(op_result(op(1), OpOutcome::InputSent));
        msgs
    };
    let head_guard = {
        let mut msgs = seq([chat_base(AGENT), vec![batch(AGENT, 10, two_ask_rows())]]);
        msgs.push(allow_once(1, 1));
        msgs.push(allow_once(2, 0));
        msgs
    };
    vec![
        ("write::prompt_flow", prompt_flow),
        ("write::answer_failure", answer_failure),
        ("write::remote_wins", remote_wins),
        ("write::mode_cycle", mode_cycle),
        ("write::question_answer", question_answer),
        ("write::head_guard", head_guard),
    ]
}
