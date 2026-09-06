//! Chapter 7 — The chat feed: prompts, messages, and markers.
//!
//! `docs/CHAT.md` B1/B2/B3 against the derived provider fixtures. Message identity
//! rides `message.id` (one content block per row, appended in file order);
//! finality is FACT on a non-null `stop_reason`; "streaming" is never a
//! state. Thinking durations are INFERRED from FACT timestamps and never
//! cross an interrupt or compaction. The turn authority is
//! `system/turn_duration`; the amux `hook.stop` row is the arrival-ordered
//! pre-signal, reconciled when the authority lands.

use amux_ui::Msg;
use amux_ui::claude::{FeedEntryKind, MessageFinality, PromptSource, ToolOutcome, TurnDuration};
use serde_json::json;

use crate::harness::*;

/// Compact names for a whole-feed shape walk.
fn kind_words(model: &amux_ui::Model, agent: &str) -> Vec<&'static str> {
    claude_layer(model, agent)
        .entries()
        .map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(_) => "prompt",
            FeedEntryKind::Message(_) => "message",
            FeedEntryKind::Thinking(_) => "thinking",
            FeedEntryKind::Turn(_) => "turn",
            FeedEntryKind::Compaction(_) => "compaction",
            FeedEntryKind::CompactSummary(_) => "compact-summary",
            FeedEntryKind::Tool(_) => "tool",
            FeedEntryKind::TaskNotification(_) => "task-notification",
            FeedEntryKind::AgentMessage(_) => "agent-message",
            FeedEntryKind::Interruption(_) => "interruption",
            FeedEntryKind::ApiError(_) => "api-error",
            FeedEntryKind::Unrecognized(_) => "unrecognized",
        })
        .collect()
}

/// The pong fixture cut at the hook has the prompt in the feed, the
/// `hook.stop` pre-signal recorded, and no turn marker — hook
/// rows are arrival-ordered and may precede the transcript tail, so the
/// marker waits for the `turn_duration` authority (B3).
#[test]
fn hook_stop_is_a_presignal_not_a_marker() {
    let model = fold(chat_feed_through(
        "fix-auth-bug",
        "pong",
        ChatAnchor::StopHook(0),
    ));
    assert_eq!(kind_words(&model, "fix-auth-bug"), vec!["prompt"]);
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(layer.turn_end_presignal());
    let unrecognized: Vec<_> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Unrecognized(row) => row.row_type.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        unrecognized,
        Vec::<&str>::new(),
        "bridge bookkeeping absorbs to session state, not feed entries"
    );
    let prompt = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .expect("the prompt entry");
    assert_eq!(
        prompt.text,
        "Reply exactly PTY_SPEC_PROMPT_OK and nothing else."
    );
    assert_eq!(prompt.source, PromptSource::Typed);
    assert!(prompt.prompt_id.is_some(), "Phase 3's reconciliation key");
}

/// The permission recordings preserve the allowed and denied runs as
/// separate provider specifications.
#[test]
fn the_permission_fixtures_fold_to_the_documented_feed() {
    let allowed = fold(chat_feed("fix-auth-bug", "permission"));
    assert_eq!(
        kind_words(&allowed, "fix-auth-bug"),
        vec!["prompt", "thinking", "tool"]
    );

    let denied = fold(chat_feed_through(
        "fix-auth-bug",
        "permission_deny_feedback",
        ChatAnchor::TurnDuration(0),
    ));
    assert_eq!(
        kind_words(&denied, "fix-auth-bug"),
        vec!["prompt", "thinking", "tool", "interruption", "turn"]
    );
}

/// A message spans several rows sharing `message.id`: blocks append in
/// file order, and finality is the FACT on the rows' `stop_reason` (B2).
#[test]
fn messages_upsert_by_message_id_and_finalize_on_stop_reason() {
    let model = fold(chat_feed("fix-auth-bug", "pong"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let messages: Vec<_> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Message(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1, "one text-bearing message in the fixture");
    let message = messages[0];
    assert_eq!(message.message_id, "msg_011CeYPVJFBzDZ8nZKGiLhSm");
    assert_eq!(message.segments.len(), 1);
    assert!(!message.segments[0].is_empty());
    assert_eq!(
        message.finality,
        MessageFinality::Final {
            stop_reason: "end_turn".to_string()
        }
    );
}

/// A null-stop message flushed before an interrupt is final-as-interrupted,
/// paired by `interruptedMessageId` rather than arrival guesswork
/// (docs/CHAT.md B2/B8).
#[test]
fn a_mid_generation_interrupt_closes_its_flushed_message() {
    let prompt = review_row(
        0x7100,
        "2026-08-11T22:00:00.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "begin"},
            "origin": {"kind": "human"},
            "promptSource": "typed"
        }),
    );
    let partial = review_row(
        0x7101,
        "2026-08-11T22:00:01.000Z",
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_interrupted",
                "role": "assistant",
                "stop_reason": null,
                "content": [{"type": "text", "text": "partial answer"}]
            }
        }),
    );
    let interrupt = review_row(
        0x7102,
        "2026-08-11T22:00:02.000Z",
        json!({
            "type": "user",
            "interruptedMessageId": "msg_interrupted",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user]"}
            ]}
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, vec![prompt, partial, interrupt])],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let message = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Message(message) => Some(message),
            _ => None,
        })
        .expect("the flushed partial message");
    assert_eq!(message.message_id, "msg_interrupted");
    assert_eq!(message.finality, MessageFinality::Interrupted);
    let interruption = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Interruption(interruption) => Some(interruption),
            _ => None,
        })
        .expect("the paired interruption");
    assert_eq!(
        interruption.interrupted_message_id.as_deref(),
        Some("msg_interrupted")
    );
}

/// Thinking markers are retroactive and INFERRED from FACT timestamps:
/// thinking row minus the previous uuid row, clamped at zero (B3). The
/// fixture's wall clock makes the expected values exact.
#[test]
fn thinking_durations_come_from_the_timestamp_chain() {
    let model = fold(chat_feed("fix-auth-bug", "permission"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let durations: Vec<Option<i64>> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Thinking(thinking) => Some(thinking.duration_ms),
            _ => None,
        })
        .collect();
    assert_eq!(durations, vec![Some(1703)]);
}

/// The previous-row timestamp chain includes rows that render as other entry
/// kinds: a tool result and the next human prompt each become the base for the
/// following thinking marker (docs/CHAT.md B3).
#[test]
fn thinking_durations_chain_through_tool_results_and_following_prompts() {
    let prompt = review_row(
        0x7200,
        "2026-08-11T22:00:00.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "first"},
            "origin": {"kind": "human"},
            "promptSource": "typed"
        }),
    );
    let first_thinking = review_row(
        0x7201,
        "2026-08-11T22:00:01.000Z",
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_thinking_1", "role": "assistant", "stop_reason": null,
                "content": [{"type": "thinking", "thinking": "one"}]
            }
        }),
    );
    let tool_result = review_row(
        0x7202,
        "2026-08-11T22:00:03.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_chain", "content": "done"}
            ]}
        }),
    );
    let second_thinking = review_row(
        0x7203,
        "2026-08-11T22:00:05.000Z",
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_thinking_2", "role": "assistant", "stop_reason": null,
                "content": [{"type": "thinking", "thinking": "two"}]
            }
        }),
    );
    let next_prompt = review_row(
        0x7204,
        "2026-08-11T22:00:08.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "second"},
            "origin": {"kind": "human"},
            "promptSource": "typed"
        }),
    );
    let third_thinking = review_row(
        0x7205,
        "2026-08-11T22:00:11.000Z",
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_thinking_3", "role": "assistant", "stop_reason": "end_turn",
                "content": [{"type": "thinking", "thinking": "three"}]
            }
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                prompt,
                first_thinking,
                tool_result,
                second_thinking,
                next_prompt,
                third_thinking,
            ],
        )],
    ]));
    let durations: Vec<_> = claude_layer(&model, "fix-auth-bug")
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Thinking(thinking) => Some(thinking.duration_ms),
            _ => None,
        })
        .collect();
    assert_eq!(durations, vec![Some(1000), Some(2000), Some(3000)]);
}

/// The turn authority carries the wall-time-verified duration and the
/// cumulative message count (FACTs); the pre-signal is cleared once it
/// lands.
#[test]
fn turn_duration_is_the_authority() {
    let model = fold(chat_feed("fix-auth-bug", "pong"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let turns: Vec<_> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .collect();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].duration, TurnDuration::Measured { ms: 1292 });
    assert_eq!(turns[0].message_count, Some(9));
    assert!(!layer.turn_end_presignal());
}

/// A mid-generation tool interrupt records the interrupted tool request and
/// the turn marker shows elapsed-from-prompt, tagged inferred (B3/§17).
#[test]
fn an_interrupt_closes_the_tool_request_and_infers_the_turn() {
    let model = fold(chat_feed("fix-auth-bug", "interrupt"));
    assert_eq!(
        kind_words(&model, "fix-auth-bug"),
        vec!["prompt", "thinking", "tool", "interruption", "turn",]
    );
    let layer = claude_layer(&model, "fix-auth-bug");
    let interruption = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Interruption(interruption) => Some(interruption),
            _ => None,
        })
        .expect("the interruption entry");
    assert_eq!(
        interruption.interrupted_message_id.as_deref(),
        Some("msg_011CeYTfdzqBFAHSu3UajiAB")
    );
    let turn = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .expect("the inferred turn marker");
    assert_eq!(
        turn.duration,
        TurnDuration::SincePrompt { ms: 4007 },
        "08.076 − 04.069, elapsed from the prompt row"
    );
}

/// The compact fixture end to end: four measured turns, the typed `/compact`
/// as a bare prompt (no origin — never a turn start), the boundary with its
/// FACT token counts, and the transcript-only summary. Meta and
/// local-command records fold to session bookkeeping, not entries.
#[test]
fn compaction_folds_to_boundary_plus_summary() {
    let model = fold(chat_feed("fix-auth-bug", "compact"));
    assert_eq!(
        kind_words(&model, "fix-auth-bug"),
        vec![
            "prompt",
            "thinking",
            "message",
            "turn",
            "prompt",
            "thinking",
            "message",
            "turn",
            "prompt",
            "thinking",
            "message",
            "turn",
            "prompt",
            "thinking",
            "message",
            "turn",
            "prompt", // the bare `/compact` record, source unstated
            "compaction",
            "compact-summary",
        ]
    );
    let layer = claude_layer(&model, "fix-auth-bug");
    let bare = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .nth(4)
        .expect("the /compact record");
    assert_eq!(bare.text, "/compact");
    assert_eq!(bare.source, PromptSource::Unstated);
    let compaction = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Compaction(compaction) => Some(compaction),
            _ => None,
        })
        .expect("the boundary");
    assert_eq!(compaction.trigger.as_deref(), Some("manual"));
    assert_eq!(compaction.pre_tokens, Some(34695));
    assert_eq!(compaction.post_tokens, Some(2587));
}

/// Durations are never computed across a compaction (B3): a thinking row
/// whose previous uuid row sits before the boundary reports no duration
/// rather than a wrong one.
#[test]
fn thinking_durations_never_cross_a_compaction() {
    let thinking_after_boundary = json!({
        "type": "assistant",
        "uuid": uuid::Uuid::from_u128(0x4000).to_string(),
        "sessionId": "70b0fb0d-1cf7-4a52-9ee7-d4f21bcc35b4",
        "timestamp": "2026-08-11T22:31:00.000Z",
        "message": {
            "id": "msg_after_compact",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": "", "signature": ""}]
        }
    });
    let mut rows = chat_rows("compact");
    rows.push(thinking_after_boundary);
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, rows)],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let last_thinking = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .last()
        .expect("the post-boundary thinking marker");
    assert_eq!(
        last_thinking.duration_ms, None,
        "the chain broke at the boundary — no duration, not a wrong one"
    );
}

/// A new `message.id` closes a still-open (null-stop) message as
/// abandoned; a later row carrying the stop FACT wins over the inference
/// (B2: never left "streaming").
#[test]
fn abandoned_closure_yields_to_a_late_finality_fact() {
    let session = "22222222-2222-4222-8222-222222222222";
    let open_text = |id: &str, uuid: u128, text: &str| {
        json!({
            "type": "assistant",
            "uuid": uuid::Uuid::from_u128(uuid).to_string(),
            "sessionId": session,
            "timestamp": "2026-08-11T22:00:01.000Z",
            "message": {
                "id": id, "role": "assistant", "stop_reason": null,
                "content": [{"type": "text", "text": text}]
            }
        })
    };
    let final_row = |id: &str, uuid: u128| {
        json!({
            "type": "assistant",
            "uuid": uuid::Uuid::from_u128(uuid).to_string(),
            "sessionId": session,
            "timestamp": "2026-08-11T22:00:02.000Z",
            "message": {
                "id": id, "role": "assistant", "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "tail"}]
            }
        })
    };

    // Abandonment: a new id arrives while msg_a is still open.
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                open_text("msg_a", 0x5000, "left open"),
                open_text("msg_b", 0x5001, "next"),
            ],
        )],
    ]));
    let finality = |model: &amux_ui::Model, id: &str| {
        claude_layer(model, "fix-auth-bug")
            .entries()
            .find_map(|entry| match &entry.kind {
                FeedEntryKind::Message(message) if message.message_id == id => {
                    Some(message.finality.clone())
                }
                _ => None,
            })
            .expect("message entry")
    };
    assert_eq!(finality(&model, "msg_a"), MessageFinality::Abandoned);
    assert_eq!(finality(&model, "msg_b"), MessageFinality::Open);

    // The late FACT upgrades the inference.
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                open_text("msg_a", 0x5000, "left open"),
                open_text("msg_b", 0x5001, "next"),
                final_row("msg_a", 0x5002),
            ],
        )],
    ]));
    assert_eq!(
        finality(&model, "msg_a"),
        MessageFinality::Final {
            stop_reason: "end_turn".to_string()
        }
    );
}

/// An unpaired tool in a final message is the running-INFERRED display
/// state — visible in the question fixtures, where the fixture also
/// resolves it (FACT once the result lands).
#[test]
fn an_unpaired_tool_in_a_final_message_is_pending_until_the_result() {
    let mut msgs = chat_base("fix-auth-bug");
    let before = chat_rows_before("question_single", ChatAnchor::ToolResult(0));
    assert!(
        before
            .iter()
            .rev()
            .all(|row| row["message"]["content"][0]["type"] != "tool_result")
    );
    msgs.push(batch("fix-auth-bug", 10, before));
    let model = fold(msgs.clone());
    let layer = claude_layer(&model, "fix-auth-bug");
    let tool = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the question tool line");
    assert_eq!(tool.outcome, ToolOutcome::Pending);
    assert!(tool.message_final, "renders as running, not streaming");

    msgs.push(batch(
        "fix-auth-bug",
        20,
        vec![chat_row("question_single", ChatAnchor::ToolResult(0))],
    ));
    let model = fold(msgs);
    let layer = claude_layer(&model, "fix-auth-bug");
    let tool = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the question tool line");
    assert!(matches!(tool.outcome, ToolOutcome::Success { .. }));
    assert_eq!(layer.open_tool_count(), 0);
}

/// Session-state rows fold as latest-wins facts, never feed entries: the
/// permission mode (D4's source of truth), the generated title, and the
/// `agent-name` row.
#[test]
fn session_state_rows_fold_to_latest_wins_facts() {
    let model = fold(chat_feed("fix-auth-bug", "plan_auto"));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert_eq!(layer.session().permission_mode.as_deref(), Some("plan"));
    assert_eq!(
        layer.session().agent_name.as_deref(),
        Some("update-readme-content")
    );
    assert!(layer.session().ai_title.is_some());

    let tools = fold(chat_feed("fix-auth-bug", "tools"));
    assert_eq!(
        claude_layer(&tools, "fix-auth-bug")
            .session()
            .permission_mode
            .as_deref(),
        Some("bypassPermissions")
    );
}

/// PreToolUse and PostToolUse duplicate the transcript's typed tool blocks.
/// They never add a second tool or an unrecognized feed entry
/// (docs/CHAT.md §The feed).
#[test]
fn tool_lifecycle_hooks_are_known_session_fact_rows() {
    let rows = chat_rows("tools");
    for row_type in ["hook.pre_tool_use", "hook.post_tool_use"] {
        assert!(
            rows.iter().any(|row| row["type"] == row_type),
            "the provider fixture exercises {row_type}"
        );
    }

    let model = fold(chat_feed("fix-auth-bug", "tools"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let unrecognized: Vec<_> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Unrecognized(row) => row.row_type.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(unrecognized, Vec::<&str>::new());
}

const REVIEW_SESSION: &str = "22222222-2222-4222-8222-222222222222";

fn review_row(uuid: u128, at: &str, mut extra: serde_json::Value) -> serde_json::Value {
    let mut base = json!({
        "uuid": uuid::Uuid::from_u128(uuid).to_string(),
        "sessionId": REVIEW_SESSION,
        "timestamp": at,
    });
    base.as_object_mut()
        .expect("object")
        .append(extra.as_object_mut().expect("object"));
    base
}

fn interrupt_row(uuid: u128, at: &str) -> serde_json::Value {
    review_row(
        uuid,
        at,
        json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user]"}
            ]}
        }),
    )
}

/// `origin.kind:"human"` is the definitive turn-start discriminator: a
/// `promptSource` label this build does not know is decoration, and
/// tolerate-unknown means it degrades gracefully — the turn clock still
/// starts, so a later interrupt can still infer its elapsed marker.
#[test]
fn a_human_origin_prompt_with_an_unknown_source_still_starts_the_turn() {
    let prompt = review_row(
        0x8000,
        "2026-08-11T22:00:00.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "carry on"},
            "origin": {"kind": "human"},
            "promptSource": "telepathy"
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![prompt, interrupt_row(0x8001, "2026-08-11T22:00:07.500Z")],
        )],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let prompt = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .expect("the prompt entry");
    assert_eq!(
        prompt.source,
        PromptSource::Other {
            label: "telepathy".to_string()
        }
    );
    let turn = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .expect("the interrupt still measures from the prompt");
    assert_eq!(turn.duration, TurnDuration::SincePrompt { ms: 7500 });
}

/// Zero is not a fact: a `turn_duration` row without a readable
/// `durationMs` degrades to an unrecognized entry — it never fabricates a
/// measured zero and never overwrites a better inferred marker.
#[test]
fn a_malformed_turn_duration_never_fabricates_a_measured_zero() {
    let prompt = review_row(
        0x8100,
        "2026-08-11T22:00:00.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "work"},
            "origin": {"kind": "human"},
            "promptSource": "typed"
        }),
    );
    let malformed = review_row(
        0x8102,
        "2026-08-11T22:00:04.000Z",
        json!({"type": "system", "subtype": "turn_duration", "messageCount": 3}),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                prompt,
                interrupt_row(0x8101, "2026-08-11T22:00:03.000Z"),
                malformed,
            ],
        )],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let turn = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .expect("the inferred marker");
    assert_eq!(
        turn.duration,
        TurnDuration::SincePrompt { ms: 3000 },
        "the inferred marker stands; no measured zero was invented"
    );
    assert!(
        layer.entries().any(|entry| matches!(
            &entry.kind,
            FeedEntryKind::Unrecognized(entry)
                if entry.detail.as_deref() == Some("turn_duration without durationMs")
        )),
        "the malformed authority row is stated, not interpreted"
    );
}

/// Durations are never computed across a compaction (B3) — the elapsed
/// prompt base ends at the boundary too, so an interrupt landing after it
/// infers no elapsed marker rather than one measured across the boundary.
#[test]
fn an_interrupt_after_a_compaction_infers_no_elapsed_marker() {
    let prompt = review_row(
        0x8200,
        "2026-08-11T22:00:00.000Z",
        json!({
            "type": "user",
            "message": {"role": "user", "content": "work"},
            "origin": {"kind": "human"},
            "promptSource": "typed"
        }),
    );
    let boundary = review_row(
        0x8201,
        "2026-08-11T22:00:10.000Z",
        json!({
            "type": "system", "subtype": "compact_boundary",
            "compactMetadata": {"trigger": "auto", "preTokens": 1000, "postTokens": 100}
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                prompt,
                boundary,
                interrupt_row(0x8202, "2026-08-11T22:00:20.000Z"),
            ],
        )],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(
        layer
            .entries()
            .any(|entry| matches!(entry.kind, FeedEntryKind::Interruption(_))),
        "the interruption itself is recorded"
    );
    assert!(
        !layer
            .entries()
            .any(|entry| matches!(entry.kind, FeedEntryKind::Turn(_))),
        "no elapsed marker measured across the boundary"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "feed_turns::permission",
            chat_feed("fix-auth-bug", "permission"),
        ),
        (
            "feed_turns::interrupt",
            chat_feed("fix-auth-bug", "interrupt"),
        ),
        ("feed_turns::compact", chat_feed("fix-auth-bug", "compact")),
        (
            "feed_turns::unknown_source_turn",
            seq([
                chat_base("fix-auth-bug"),
                vec![batch(
                    "fix-auth-bug",
                    10,
                    vec![
                        review_row(
                            0x8000,
                            "2026-08-11T22:00:00.000Z",
                            json!({
                                "type": "user",
                                "message": {"role": "user", "content": "carry on"},
                                "origin": {"kind": "human"},
                                "promptSource": "telepathy"
                            }),
                        ),
                        interrupt_row(0x8001, "2026-08-11T22:00:07.500Z"),
                    ],
                )],
            ]),
        ),
        (
            "feed_turns::compaction_interrupt",
            seq([
                chat_base("fix-auth-bug"),
                vec![batch(
                    "fix-auth-bug",
                    10,
                    vec![
                        review_row(
                            0x8200,
                            "2026-08-11T22:00:00.000Z",
                            json!({
                                "type": "user",
                                "message": {"role": "user", "content": "work"},
                                "origin": {"kind": "human"},
                                "promptSource": "typed"
                            }),
                        ),
                        review_row(
                            0x8201,
                            "2026-08-11T22:00:10.000Z",
                            json!({
                                "type": "system", "subtype": "compact_boundary",
                                "compactMetadata": {"trigger": "auto"}
                            }),
                        ),
                        interrupt_row(0x8202, "2026-08-11T22:00:20.000Z"),
                    ],
                )],
            ]),
        ),
    ]
}
/// Every row type the committed provider fixtures deliver is classified:
/// it folds to a typed feed entry, a session fact, or a documented
/// absorption — never to an unrecognized entry. A new provider row shape
/// landing in a fixture turns this red until the fold names it. Genuinely
/// unknown shapes keep their explicit unrecognized rendering (G1),
/// exercised with synthetic rows in `feed_edges`.
#[test]
fn every_fixture_row_type_is_classified() {
    let chat = [
        "pong",
        "tools",
        "permission",
        "question_single",
        "question_multi",
        "interrupt",
        "plan_approve",
        "plan_reject",
        "compact",
        "clear",
        "mode_cycle",
        "permission_session",
        "permission_deny_feedback",
        "question_tabs",
        "question_mixed",
        "question_other_single",
        "plan_auto",
        "prompt_multiline",
    ];
    let a2a = ["socket_delivery", "pty_delivery", "mcp_tools"];
    let fixtures = chat
        .iter()
        .map(|name| (*name, chat_rows(name)))
        .chain(a2a.iter().map(|name| (*name, a2a_rows(name))));
    for (name, rows) in fixtures {
        let model = fold(seq([
            chat_base("fix-auth-bug"),
            vec![batch("fix-auth-bug", 10, rows)],
        ]));
        let layer = claude_layer(&model, "fix-auth-bug");
        let unrecognized: Vec<_> = layer
            .entries()
            .filter_map(|entry| match &entry.kind {
                FeedEntryKind::Unrecognized(row) => Some(row.row_type.clone()),
                _ => None,
            })
            .collect();
        assert!(
            unrecognized.is_empty(),
            "{name}: unclassified fixture rows {unrecognized:?}"
        );
    }
}

/// Every main-session assistant message states the model it ran on and
/// what its context cost. Both are latest-wins session facts, and the
/// cost is fresh input plus both cache halves — the same three fields the
/// stream-JSON session reports, folded to the same number.
#[test]
fn assistant_messages_fold_model_and_context_cost_as_latest_wins_facts() {
    let assistant = |n: u32, model: &str, usage: serde_json::Value| {
        json!({
            "type": "assistant",
            "uuid": format!("cccccccc-0000-4000-8000-0000000000{n:02}"),
            "sessionId": chat_session_id("pong"),
            "timestamp": format!("2026-08-11T22:00:{n:02}.000Z"),
            "message": {
                "id": format!("msg-{n}"),
                "model": model,
                "role": "assistant",
                "stop_reason": "end_turn",
                "content": [{"type": "text", "text": format!("reply {n}")}],
                "usage": usage,
            }
        })
    };
    let mut msgs = chat_base("fix-auth-bug");
    msgs.push(batch(
        "fix-auth-bug",
        10,
        vec![assistant(
            1,
            "claude-opus-5",
            json!({"input_tokens": 3, "cache_read_input_tokens": 1000,
                   "cache_creation_input_tokens": 200, "output_tokens": 5}),
        )],
    ));
    let model = fold(msgs.clone());
    let session = claude_layer(&model, "fix-auth-bug").session();
    assert_eq!(session.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(session.context_used_tokens, Some(1203));

    msgs.push(batch(
        "fix-auth-bug",
        20,
        vec![assistant(
            2,
            "claude-sonnet-5",
            json!({"input_tokens": 7, "cache_read_input_tokens": 1200, "output_tokens": 9}),
        )],
    ));
    let model = fold(msgs);
    let session = claude_layer(&model, "fix-auth-bug").session();
    assert_eq!(session.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(
        session.context_used_tokens,
        Some(1207),
        "a missing cache half counts as zero, not as a stale carry-over"
    );
}
