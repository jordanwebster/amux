//! Chapter 5 — Attention: the Claude summarizer fold and the subscription
//! policy behind it.
//!
//! Attention is derived at observation time by a pure per-agent fold —
//! stream entries in, attention out — with zero core changes. Fixture rows
//! live in `fixtures/*.json`, each carrying `capturedWith` provenance.
//! Degradation is always to `Unknown`, never to a wrong badge.

use amux_ui::{Attention, Effect, FleetItem, Msg, StreamMsg, Why};

use crate::harness::*;

fn fixture_rows(file: &str, key: &str) -> Vec<serde_json::Value> {
    let contents: serde_json::Value = match file {
        "claude_permission_flow" => {
            serde_json::from_str(include_str!("fixtures/claude_permission_flow.json"))
        }
        "claude_stop_and_notification" => {
            serde_json::from_str(include_str!("fixtures/claude_stop_and_notification.json"))
        }
        "claude_truncated_tail" => {
            serde_json::from_str(include_str!("fixtures/claude_truncated_tail.json"))
        }
        other => panic!("unknown fixture {other}"),
    }
    .expect("fixture parses");
    assert!(
        contents.get("capturedWith").is_some(),
        "fixtures carry provenance"
    );
    contents
        .get(key)
        .and_then(|rows| rows.as_array())
        .unwrap_or_else(|| panic!("fixture {file} has rows under {key}"))
        .clone()
}

/// A local claude agent whose stream is live (complete window).
fn subscribed_base() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: false }),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ])
}

fn permission_pending_sequence() -> Vec<Msg> {
    seq([
        subscribed_base(),
        vec![
            batch(
                "fix-auth-bug",
                10,
                fixture_rows("claude_permission_flow", "workingTurn"),
            ),
            batch(
                "fix-auth-bug",
                40,
                fixture_rows("claude_permission_flow", "permissionRequest"),
            ),
        ],
    ])
}

fn permission_answered_sequence() -> Vec<Msg> {
    seq([
        permission_pending_sequence(),
        vec![batch(
            "fix-auth-bug",
            70,
            fixture_rows("claude_permission_flow", "answeredAndResumed"),
        )],
    ])
}

fn stop_sequence() -> Vec<Msg> {
    seq([
        subscribed_base(),
        vec![batch(
            "fix-auth-bug",
            10,
            fixture_rows("claude_stop_and_notification", "streamingThenStop"),
        )],
    ])
}

fn late_join_sequence() -> Vec<Msg> {
    // Replay begins mid-stream (truncated) but the window still holds the
    // request: the fold derives the pending permission from replay alone.
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
                [
                    fixture_rows("claude_permission_flow", "workingTurn"),
                    fixture_rows("claude_permission_flow", "permissionRequest"),
                ]
                .concat(),
            ),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ])
}

fn truncated_blind_window_sequence() -> Vec<Msg> {
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
                fixture_rows("claude_truncated_tail", "weakRowsOnly"),
            ),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ])
}

fn attention_of(model: &amux_ui::Model, agent: &str) -> Attention {
    model.agent(agent_id(agent)).expect("card exists").attention
}

/// A permission request in the stream marks the agent as needing you, and
/// the fleet ranks it first.
#[test]
fn permission_request_marks_needs_you() {
    let model = fold(permission_pending_sequence());
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

/// A permission answered through raw passthrough is only visible as
/// subsequent activity — which is exactly what clears the badge.
#[test]
fn activity_after_permission_clears_to_working() {
    let model = fold(permission_answered_sequence());
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Working);
}

/// Stop marks the turn complete: the agent finished and wants your review.
#[test]
fn stop_marks_turn_complete() {
    let model = fold(stop_sequence());
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou { why: Why::Finished }
    );
}

/// Hooks alone do not distinguish question from permission — Notification
/// text does, interpreted here at observation time.
#[test]
fn notification_text_distinguishes_permission_from_question() {
    let permission = fold(seq([
        subscribed_base(),
        vec![batch(
            "fix-auth-bug",
            10,
            fixture_rows("claude_stop_and_notification", "permissionNotification"),
        )],
    ]));
    assert_eq!(
        attention_of(&permission, "fix-auth-bug"),
        Attention::NeedsYou {
            why: Why::Permission
        }
    );

    let question = fold(seq([
        subscribed_base(),
        vec![batch(
            "fix-auth-bug",
            10,
            fixture_rows("claude_stop_and_notification", "waitingNotification"),
        )],
    ]));
    assert_eq!(
        attention_of(&question, "fix-auth-bug"),
        Attention::NeedsYou { why: Why::Question }
    );
}

/// A late-joining client derives the pending permission purely from replay.
#[test]
fn late_join_replay_derives_pending_permission() {
    let model = fold(late_join_sequence());
    assert_eq!(
        attention_of(&model, "fix-auth-bug"),
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
}

/// A truncated window with no attention-bearing rows reports Unknown — the
/// request may have fallen outside the window, and absence of evidence in a
/// bounded history is not evidence of idleness.
#[test]
fn tail_window_missing_the_request_degrades_to_unknown_not_wrong() {
    let model = fold(truncated_blind_window_sequence());
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Unknown);

    // The same blind window over a complete history is honest Idle.
    let complete = fold(seq([
        subscribed_base(),
        vec![batch(
            "fix-auth-bug",
            5,
            fixture_rows("claude_truncated_tail", "weakRowsOnly"),
        )],
    ]));
    assert_eq!(attention_of(&complete, "fix-auth-bug"), Attention::Idle);
}

/// Streaming output cannot strobe the badge: fold assistant activity row by
/// row and the attention stays Working at every step (hysteresis by
/// construction — only hook rows leave Working).
#[test]
fn streaming_output_does_not_strobe_working() {
    let mut model = fold(permission_answered_sequence());
    assert_eq!(attention_of(&model, "fix-auth-bug"), Attention::Working);
    let chunk = &fixture_rows("claude_permission_flow", "workingTurn")[1];
    for step in 0..20 {
        amux_ui::update(
            &mut model,
            batch("fix-auth-bug", 100 + step, vec![chunk.clone()]),
        );
        assert_eq!(
            attention_of(&model, "fix-auth-bug"),
            Attention::Working,
            "attention left Working at streaming step {step}"
        );
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
            permission_pending_sequence(),
        ),
        (
            "attention::permission_answered",
            permission_answered_sequence(),
        ),
        ("attention::stop", stop_sequence()),
        ("attention::late_join", late_join_sequence()),
        (
            "attention::truncated_blind_window",
            truncated_blind_window_sequence(),
        ),
    ]
}
