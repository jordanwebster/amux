//! Chapter 12 — Phase: the E1 derivation table.
//!
//! `docs/CHAT.md` §Phase and attention. The Model derives one session
//! phase, each value tagged fact vs inferred; degradation is always to
//! Unknown, never to a wrong badge. `Model::claude_phase` computes it once
//! for every renderer: kernel subscription catch-up gates to Replaying,
//! a dead transport degrades to Unknown, and the layer derives the rest —
//! with `now` (entering via Ticks) capping the inferred states.

use amux_ui::claude::{AskWhy, ChatPhase, PhaseTag};
use amux_ui::{Msg, StreamCloseReason, StreamMsg};
use serde_json::json;

use crate::harness::*;

fn phase_of(model: &amux_ui::Model) -> ChatPhase {
    amux_ui::claude::phase(model, agent_id("fix-auth-bug"))
}

fn ready() -> serde_json::Value {
    json!({"type": "amux.transcript_ready"})
}

// --- replaying --------------------------------------------------------------

/// Before `amux.transcript_ready` everything is replay (FACT at the amux
/// layer): during kernel subscription catch-up AND while transcript rows
/// fold without the ready marker.
#[test]
fn before_transcript_ready_the_phase_is_replaying() {
    // Kernel catch-up: the stream opened, replay not yet complete.
    let opening = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![stream(
            "fix-auth-bug",
            StreamMsg::Opened { truncated: false },
        )],
    ]));
    assert_eq!(phase_of(&opening), ChatPhase::Replaying);

    // Transcript catch-up: rows folded mid-replay (before ReplayComplete),
    // ready marker not yet seen — the permission fixture's rows before its
    // marker.
    let mid_transcript = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: false }),
            batch(
                "fix-auth-bug",
                5,
                chat_rows_before("permission", ChatAnchor::TranscriptReady),
            ),
        ],
    ]));
    assert_eq!(phase_of(&mid_transcript), ChatPhase::Replaying);
    assert_eq!(phase_of(&mid_transcript).tag(), Some(PhaseTag::Fact));
}

/// THE truncated-live-window unlock (the most common real-world attach):
/// a long-running session writes >1000 rows after `amux.transcript_ready`,
/// so a late attacher's truncated tail no longer CONTAINS the marker.
/// `StreamMsg::ReplayComplete` is the out-of-band unlock — after it, the
/// window is LIVE with truncated history: phase derives from the tail's
/// facts, live prompts and permission hooks surface, and the honest
/// missing-history boundary (B9) stays. Liveness is never suppressed by
/// truncation.
#[test]
fn a_truncated_live_window_unlocks_after_replay_complete() {
    // The replayed tail contains a prompt after the evicted ready marker.
    let attach = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
            batch(
                "fix-auth-bug",
                5,
                vec![chat_row("permission", ChatAnchor::Prompt(0))],
            ),
        ],
    ]);
    let mid_replay = fold(attach.clone());
    assert_eq!(
        phase_of(&mid_replay),
        ChatPhase::Replaying,
        "still catching up before ReplayComplete"
    );

    let live = seq([
        attach,
        vec![stream("fix-auth-bug", StreamMsg::ReplayComplete)],
    ]);
    let unlocked = fold(live.clone());
    assert_eq!(
        phase_of(&unlocked),
        ChatPhase::Working,
        "the tail's open turn surfaces — never Replaying forever"
    );
    assert!(
        claude_layer(&unlocked, "fix-auth-bug").history_truncated(),
        "missing-history honesty stays"
    );

    // A live permission hook after the unlock surfaces immediately, on the
    // badge and the phase alike.
    let asked = fold(seq([
        live,
        vec![batch(
            "fix-auth-bug",
            20,
            vec![chat_row("permission", ChatAnchor::PermissionRequest(0))],
        )],
    ]));
    assert_eq!(
        phase_of(&asked),
        ChatPhase::NeedsYou {
            why: AskWhy::Permission,
            tag: PhaseTag::Fact
        }
    );
    let card = asked.agent(agent_id("fix-auth-bug")).expect("card");
    assert_eq!(
        asked.effective_attention(card),
        amux_ui::Attention::NeedsYou {
            why: amux_ui::Why::Permission
        },
        "attention updates on the live window too"
    );
}

/// A fresh session has no transcript file until its first turn (B10): a
/// complete, untruncated, row-less window is an empty chat — idle,
/// inferred — not a loading band.
#[test]
fn a_fresh_empty_session_is_idle_not_replaying() {
    let model = fold(chat_base("fix-auth-bug"));
    assert_eq!(
        phase_of(&model),
        ChatPhase::Idle {
            tag: PhaseTag::Inferred
        }
    );
}

// --- working ----------------------------------------------------------------

/// A human prompt with no turn-end signal is working — INFERRED, because a
/// crash leaves it stuck (the cap below turns that into Unknown).
#[test]
fn a_prompt_without_a_turn_end_signal_is_working() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "permission",
        ChatAnchor::Prompt(0),
    ));
    assert_eq!(phase_of(&model), ChatPhase::Working);
    assert_eq!(phase_of(&model).tag(), Some(PhaseTag::Inferred));
}

/// The staleness cap (E1): working with no observed delivery past the cap
/// degrades to Unknown — never guesses that a silent claude is alive.
#[test]
fn stale_working_degrades_to_unknown() {
    let base = chat_feed_through("fix-auth-bug", "permission", ChatAnchor::Prompt(0));
    let live = fold(seq([base.clone(), vec![tick(10 + 599)]]));
    assert_eq!(phase_of(&live), ChatPhase::Working, "inside the cap");
    let stale = fold(seq([base, vec![tick(10 + 601)]]));
    assert_eq!(phase_of(&stale), ChatPhase::Unknown, "past the cap");
}

// --- idle -------------------------------------------------------------------

/// `turn_duration` is the idle authority: FACT at the signal, decaying to
/// INFERRED — an external session may already be typing.
#[test]
fn turn_authority_idle_is_fact_then_decays_to_inferred() {
    let closed = fold(chat_feed("fix-auth-bug", "pong"));
    assert_eq!(
        phase_of(&closed),
        ChatPhase::Idle {
            tag: PhaseTag::Fact
        }
    );
    let fresh = fold(seq([chat_feed("fix-auth-bug", "pong"), vec![tick(30)]]));
    assert_eq!(
        phase_of(&fresh),
        ChatPhase::Idle {
            tag: PhaseTag::Fact
        }
    );
    let decayed = fold(seq([
        chat_feed("fix-auth-bug", "pong"),
        vec![tick(10 + 120)],
    ]));
    assert_eq!(
        phase_of(&decayed),
        ChatPhase::Idle {
            tag: PhaseTag::Inferred
        }
    );
}

/// The arrival-ordered `hook.stop` pre-signal reports idle before the
/// transcript tail catches up — INFERRED until the authority lands. The
/// prefix is cut exactly there.
#[test]
fn the_stop_presignal_reports_idle_inferred() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "pong",
        ChatAnchor::StopHook(0),
    ));
    assert_eq!(
        phase_of(&model),
        ChatPhase::Idle {
            tag: PhaseTag::Inferred
        }
    );
}

/// An interrupt closes the turn as a FACT (§17 rows): the user did it.
#[test]
fn an_interrupt_closes_the_turn_to_idle_fact() {
    let model = fold(chat_feed("fix-auth-bug", "interrupt"));
    assert_eq!(
        phase_of(&model),
        ChatPhase::Idle {
            tag: PhaseTag::Fact
        }
    );
}

// --- needs-you --------------------------------------------------------------

/// The pending permission FACT: `hook.permission_request` newer than any
/// resolving result.
#[test]
fn a_pending_permission_is_needs_you_permission() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "permission",
        ChatAnchor::PermissionRequest(0),
    ));
    assert_eq!(
        phase_of(&model),
        ChatPhase::NeedsYou {
            why: AskWhy::Permission,
            tag: PhaseTag::Fact
        }
    );
}

/// The pending question: the final message's AskUserQuestion unpaired
/// (FACT-grade), here with the hook FACT ahead of it.
#[test]
fn a_pending_question_is_needs_you_question() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "question_single",
        ChatAnchor::PermissionRequest(0),
    ));
    assert_eq!(
        phase_of(&model),
        ChatPhase::NeedsYou {
            why: AskWhy::Question,
            tag: PhaseTag::Fact
        }
    );
}

/// Plan review is needs-you: PERMISSION — the plan-review variant, never a
/// question, whatever the notification wording says (E2).
#[test]
fn a_pending_plan_review_is_needs_you_permission() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "plan_reject",
        ChatAnchor::PermissionRequest(0),
    ));
    assert_eq!(
        phase_of(&model),
        ChatPhase::NeedsYou {
            why: AskWhy::Permission,
            tag: PhaseTag::Fact
        }
    );
}

// --- errored ----------------------------------------------------------------

fn api_error_row() -> serde_json::Value {
    json!({
        "type": "assistant",
        "uuid": "cccccccc-0000-4000-8000-000000000001",
        "sessionId": chat_session_id("permission"),
        "timestamp": "2026-08-11T22:00:05.000Z",
        "isApiErrorMessage": true,
        "error": "server_error",
        "message": {
            "id": "e0000000-0000-4000-8000-000000000001",
            "model": "<synthetic>", "role": "assistant", "stop_reason": "stop_sequence",
            "content": [{"type": "text", "text": "API error"}]
        },
    })
}

fn normal_assistant_row() -> serde_json::Value {
    json!({
        "type": "assistant",
        "uuid": "cccccccc-0000-4000-8000-000000000002",
        "sessionId": chat_session_id("permission"),
        "timestamp": "2026-08-11T22:00:06.000Z",
        "message": {
            "id": "msg_recovered", "role": "assistant", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "recovered"}]
        },
    })
}

/// `isApiErrorMessage:true` is the errored FACT; recovery is only visible
/// as the next normal assistant message (§20) — which clears it.
#[test]
fn an_api_error_row_is_errored_until_the_next_normal_message() {
    let errored_msgs = seq([
        chat_feed_through("fix-auth-bug", "permission", ChatAnchor::Prompt(0)),
        vec![batch("fix-auth-bug", 20, vec![api_error_row()])],
    ]);
    let errored = fold(errored_msgs.clone());
    assert_eq!(phase_of(&errored), ChatPhase::Errored);
    assert_eq!(phase_of(&errored).tag(), Some(PhaseTag::Fact));

    let recovered = fold(seq([
        errored_msgs,
        vec![batch("fix-auth-bug", 30, vec![normal_assistant_row()])],
    ]));
    assert_eq!(phase_of(&recovered), ChatPhase::Working);
}

// --- unknown ----------------------------------------------------------------

/// A truncated window with no evidence reports Unknown — absence of
/// evidence in a bounded history is not evidence of idleness. The same
/// blind window over a complete history is honest Idle.
#[test]
fn a_truncated_blind_window_degrades_to_unknown_not_wrong() {
    let truncated = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
            batch("fix-auth-bug", 5, vec![ready()]),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ]));
    assert_eq!(phase_of(&truncated), ChatPhase::Unknown);
    assert_eq!(phase_of(&truncated).tag(), None, "no epistemic claim");

    let complete = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 5, vec![ready()])],
    ]));
    assert_eq!(
        phase_of(&complete),
        ChatPhase::Idle {
            tag: PhaseTag::Inferred
        }
    );
}

/// A dead transport degrades to Unknown — but keeps the obligations: the
/// reopened window replays and refolds them.
#[test]
fn transport_loss_degrades_to_unknown_and_keeps_obligations() {
    let model = fold(seq([
        chat_feed_through(
            "fix-auth-bug",
            "permission",
            ChatAnchor::PermissionRequest(0),
        ),
        vec![stream(
            "fix-auth-bug",
            StreamMsg::Closed {
                reason: StreamCloseReason::TransportError {
                    message: "reset".to_string(),
                },
            },
        )],
    ]));
    assert_eq!(phase_of(&model), ChatPhase::Unknown);
    assert_eq!(
        claude_layer(&model, "fix-auth-bug").ask_count(),
        1,
        "obligations are not dropped on transport loss"
    );
}

/// An exited agent has nothing left to need: asks close with the process,
/// and the phase settles — orderly termination is a FACT that overrides
/// truncation and staleness, so even a truncated window settles to idle
/// instead of degrading to Unknown (the card's own phase carries the
/// exit).
#[test]
fn agent_exit_closes_asks_and_settles_the_phase() {
    let exit = || {
        stream(
            "fix-auth-bug",
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
            },
        )
    };
    let model = fold(seq([
        chat_feed_through(
            "fix-auth-bug",
            "permission",
            ChatAnchor::PermissionRequest(0),
        ),
        vec![exit()],
    ]));
    assert_eq!(claude_layer(&model, "fix-auth-bug").ask_count(), 0);
    assert_eq!(
        phase_of(&model),
        ChatPhase::Idle {
            tag: PhaseTag::Fact
        }
    );

    // The truncated-window variant: without the exit FACT this window
    // would fall to Unknown; termination settles it.
    let truncated = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
            batch(
                "fix-auth-bug",
                5,
                vec![chat_row("permission", ChatAnchor::Prompt(0))],
            ),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
            exit(),
        ],
    ]));
    assert_eq!(
        phase_of(&truncated),
        ChatPhase::Idle {
            tag: PhaseTag::Fact
        },
        "definitive termination overrides truncation"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "phase::truncated_live_unlock",
            seq([
                vec![
                    connected("nova"),
                    host_up(&a_host("nova")),
                    agent_up(&an_agent("fix-auth-bug", "nova")),
                ],
                synced(),
                vec![
                    stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
                    batch(
                        "fix-auth-bug",
                        5,
                        vec![chat_row("permission", ChatAnchor::Prompt(0))],
                    ),
                    stream("fix-auth-bug", StreamMsg::ReplayComplete),
                    batch(
                        "fix-auth-bug",
                        20,
                        vec![chat_row("permission", ChatAnchor::PermissionRequest(0))],
                    ),
                ],
            ]),
        ),
        (
            "phase::working_then_stale",
            seq([
                chat_feed_through("fix-auth-bug", "permission", ChatAnchor::Prompt(0)),
                vec![tick(10 + 601)],
            ]),
        ),
        (
            "phase::idle_decay",
            seq([chat_feed("fix-auth-bug", "pong"), vec![tick(10 + 120)]]),
        ),
        (
            "phase::errored_then_recovered",
            seq([
                chat_feed_through("fix-auth-bug", "permission", ChatAnchor::Prompt(0)),
                vec![
                    batch("fix-auth-bug", 20, vec![api_error_row()]),
                    batch("fix-auth-bug", 30, vec![normal_assistant_row()]),
                ],
            ]),
        ),
        (
            "phase::transport_loss",
            seq([
                chat_feed_through(
                    "fix-auth-bug",
                    "permission",
                    ChatAnchor::PermissionRequest(0),
                ),
                vec![stream(
                    "fix-auth-bug",
                    StreamMsg::Closed {
                        reason: StreamCloseReason::TransportError {
                            message: "reset".to_string(),
                        },
                    },
                )],
            ]),
        ),
        (
            "phase::exit_closes_asks",
            seq([
                chat_feed_through(
                    "fix-auth-bug",
                    "permission",
                    ChatAnchor::PermissionRequest(0),
                ),
                vec![stream(
                    "fix-auth-bug",
                    StreamMsg::Closed {
                        reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
                    },
                )],
            ]),
        ),
    ]
}
