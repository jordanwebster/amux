//! Chapter 5 — Attention: the unified Claude interpretation and the
//! subscription policy behind it.
//!
//! There is exactly ONE fold (`docs/CHAT.md` E2): kernel attention is a
//! projection of the same Claude-layer state the chat phase derives from —
//! stream entries in, attention out — so fleet badges and the chat agree
//! whether or not a chat is open (E3). Notification-wording heuristics are
//! forbidden ground: interpretation routes on
//! `hook.permission_request.tool_name`. Degradation is always to
//! `Unknown`, never to a wrong badge.

use amux_ui::claude::{AskWhy, ChatPhase};
use amux_ui::{Attention, Effect, FleetItem, Model, Msg, StreamCloseReason, StreamMsg, Why};

use crate::harness::*;

fn attention_of(model: &Model, agent: &str) -> Attention {
    let card = model.agent(agent_id(agent)).expect("card exists");
    model.effective_attention(card)
}

/// A pending permission marks the agent as needing you, and the fleet
/// ranks it first.
#[test]
fn permission_request_marks_needs_you() {
    let model = fold(chat_feed_prefix("fix-auth-bug", "permission", 8));
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
    assert!(
        matches!(
            model.fleet().first(),
            Some(FleetItem::Agent(card)) if card.agent.id == agent_id("fix-auth-bug")
        ),
        "NeedsYou ranks first in the fleet"
    );
}

/// A pending AskUserQuestion is NeedsYou(Question) — routed on the hook's
/// `tool_name`, which fires for questions too.
#[test]
fn a_pending_question_marks_needs_you_question() {
    let model = fold(chat_feed_prefix("fix-auth-bug", "question_single", 8));
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou { why: Why::Question }
    );
}

/// The plan-approval notification says "needs your approval" — no
/// "permission" substring (fixture-verified). The old notification-text
/// split would have shown `?` where `!` belongs; the unified fold routes
/// on `tool_name` and cannot be fooled by wording.
#[test]
fn plan_approval_is_permission_not_question_whatever_the_wording_says() {
    // plan_reject prefix through BOTH hook.notification rows.
    let model = fold(chat_feed_prefix("fix-auth-bug", "plan_reject", 39));
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
}

/// Between prompt and turn-end signal the agent is Working; activity rows
/// cannot strobe the badge — only turn signals and asks leave Working.
#[test]
fn activity_between_prompt_and_turn_end_is_working() {
    let mut model = fold(chat_feed_prefix("fix-auth-bug", "permission", 6));
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Working);
    // Fold the first turn's transcript rows one at a time, skipping the
    // hook pair (rows 6-7): message, tool_use, and result rows keep it
    // Working at every step.
    for (step, row) in chat_rows("permission")[8..11].iter().enumerate() {
        amux_ui::update(
            &mut model,
            batch("fix-auth-bug", 11 + step as i64, vec![row.clone()]),
        );
        assert_eq!(
            attention_of(&model, "fix-auth-bug"),
            Attention::Working,
            "attention left Working at step {step}"
        );
    }
}

/// The turn authority (and the arrival-ordered stop pre-signal before it)
/// marks the turn complete: the agent finished and wants your review.
#[test]
fn a_completed_turn_marks_finished() {
    let model = fold(chat_feed("fix-auth-bug", "permission"));
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou { why: Why::Finished }
    );
    // The pre-signal alone reports the same, before the tail catches up
    // (the question fixture's capture window closed at the hook).
    let presignal = fold(chat_feed("fix-auth-bug", "question_single"));
    assert_eq!(
        attention_of(&presignal, "fix-auth-bug"),
        Attention::NeedsYou { why: Why::Finished }
    );
}

/// An interrupt is the user closing the turn deliberately: nothing to come
/// look at — Idle, not Finished.
#[test]
fn an_interrupted_turn_settles_to_idle() {
    let model = fold(chat_feed("fix-auth-bug", "interrupt"));
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Idle);
}

/// A fresh, empty, complete window is honest Idle; the same absence of
/// evidence over a TRUNCATED window is Unknown — the request may have
/// fallen outside the window.
#[test]
fn blind_windows_are_idle_only_when_complete() {
    let fresh = fold(chat_base("fix-auth-bug"));
    assert_eq!(attention_of(&fresh, "fix-auth-bug"), Attention::Idle);

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
                vec![serde_json::json!({"type": "amux.transcript_ready"})],
            ),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ]));
    assert_eq!(attention_of(&truncated, "fix-auth-bug"), Attention::Unknown);
}

/// A late-joining client derives the pending permission purely from
/// replay: the request rides the buffer like every other row.
#[test]
fn late_join_replay_derives_pending_permission() {
    let model = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
            batch("fix-auth-bug", 5, chat_rows("permission")[..8].to_vec()),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ]));
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
}

/// An API error degrades attention to Unknown — the kernel vocabulary
/// cannot say "errored", retries run invisibly, and a Working or Idle
/// badge would be a lie. The chat phase carries the errored FACT.
#[test]
fn an_api_error_degrades_attention_to_unknown() {
    let error_row = serde_json::json!({
        "type": "assistant",
        "uuid": "cccccccc-0000-4000-8000-000000000009",
        "sessionId": "9f635f35-5e8c-49a8-b035-8408c6981b11",
        "timestamp": "2026-08-11T22:00:05.000Z",
        "isApiErrorMessage": true,
        "error": "server_error",
        "message": {
            "id": "e0000000-0000-4000-8000-000000000009",
            "model": "<synthetic>", "role": "assistant", "stop_reason": "stop_sequence",
            "content": [{"type": "text", "text": "API error"}]
        },
    });
    let model = fold(seq([
        chat_feed_prefix("fix-auth-bug", "permission", 6),
        vec![batch("fix-auth-bug", 20, vec![error_row])],
    ]));
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Unknown);
    assert_eq!(
        model.claude_phase(agent_id("fix-auth-bug")),
        ChatPhase::Errored,
        "the chat states the error the badge cannot"
    );
}

/// The E1 staleness cap applies to the fleet badge at read time: a silent
/// "working" past the cap degrades to Unknown, in agreement with the chat
/// phase (E3) — and the status WORD degrades with the badge, because both
/// derive from the same effective attention. An unknown badge beside a
/// "working" label would be two derivations of one fact.
#[test]
fn stale_working_degrades_the_fleet_badge_and_label_together() {
    let live = fold(seq([
        chat_feed_prefix("fix-auth-bug", "permission", 6),
        vec![tick(10 + 599)],
    ]));
    assert_eq!(attention_of(&live, "fix-auth-bug"), Attention::Working);
    assert_eq!(
        live.status_label_for(live.agent(agent_id("fix-auth-bug")).expect("card")),
        "working"
    );
    let stale = fold(seq([
        chat_feed_prefix("fix-auth-bug", "permission", 6),
        vec![tick(10 + 601)],
    ]));
    assert_eq!(attention_of(&stale, "fix-auth-bug"), Attention::Unknown);
    assert_eq!(
        stale.status_label_for(stale.agent(agent_id("fix-auth-bug")).expect("card")),
        "–",
        "the label is the badge's fact, not the cached one"
    );
}

/// THE unification property (E2/E3): on every fixture, folded row by row,
/// the fleet's needs-you badge and the chat phase's needs-you state are
/// the same interpretation — never two folds drifting apart.
#[test]
fn fleet_attention_and_chat_phase_share_one_interpretation() {
    for fixture in [
        "pong",
        "tools",
        "permission",
        "question_single",
        "question_multi",
        "interrupt",
        "plan_approve",
        "plan_reject",
        "compact",
    ] {
        let mut model = fold(chat_base("fix-auth-bug"));
        for (step, row) in chat_rows(fixture).into_iter().enumerate() {
            amux_ui::update(
                &mut model,
                batch("fix-auth-bug", 10 + step as i64, vec![row]),
            );
            let attention_needs = match attention_of(&model, "fix-auth-bug") {
                Attention::NeedsYou {
                    why: Why::Permission,
                } => Some(AskWhy::Permission),
                Attention::NeedsYou { why: Why::Question } => Some(AskWhy::Question),
                _ => None,
            };
            let phase_needs = match model.claude_phase(agent_id("fix-auth-bug")) {
                ChatPhase::NeedsYou { why, .. } => Some(why),
                _ => None,
            };
            assert_eq!(
                attention_needs, phase_needs,
                "{fixture} step {step}: fleet and chat disagree on needs-you"
            );
        }
    }
}

/// Kernel subscription policy: every local agent advertising the structured
/// stream is subscribed exactly once; remote agents join when the user
/// attaches.
#[test]
fn subscription_policy_covers_local_agents_and_attached_remotes() {
    let local = an_agent("local-agent", "nova");
    let remote = an_agent("remote-agent", "hetzner");
    let (_, effects) = fold_with_effects(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&a_host("hetzner")),
            agent_up(&local),
            agent_up(&remote),
            // A second upsert must not resubscribe.
            agent_up(&local),
        ],
        synced(),
    ]));
    let opens: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::OpenStream { agent, tail } => Some((*agent, *tail)),
            _ => None,
        })
        .collect();
    assert_eq!(
        opens,
        vec![(agent_id("local-agent"), amux_ui::REPLAY_TAIL)],
        "exactly one open, local only"
    );

    let (_, effects) = fold_with_effects(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&a_host("hetzner")),
            agent_up(&remote),
        ],
        synced(),
        vec![Msg::UserAttached {
            agent: agent_id("remote-agent"),
        }],
    ]));
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::OpenStream { agent, .. } if *agent == agent_id("remote-agent")
        )),
        "attaching to a remote agent subscribes its stream"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "attention::permission_pending",
            chat_feed_prefix("fix-auth-bug", "permission", 8),
        ),
        (
            "attention::plan_wording_lock",
            chat_feed_prefix("fix-auth-bug", "plan_reject", 39),
        ),
        (
            "attention::finished",
            chat_feed("fix-auth-bug", "question_single"),
        ),
        (
            "attention::late_join",
            seq([
                vec![
                    connected("nova"),
                    host_up(&a_host("nova")),
                    agent_up(&an_agent("fix-auth-bug", "nova")),
                ],
                synced(),
                vec![
                    stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
                    batch("fix-auth-bug", 5, chat_rows("permission")[..8].to_vec()),
                    stream("fix-auth-bug", StreamMsg::ReplayComplete),
                ],
            ]),
        ),
        (
            "attention::exited",
            seq([
                chat_feed("fix-auth-bug", "interrupt"),
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
