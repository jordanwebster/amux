//! Chapter 9 — The chat feed: status entries, subagents, unknown rows.
//!
//! `docs/CHAT.md` B7/B8/G1. Subagent facts come from the main file alone
//! (no child-file tailing): launch and completion are FACTs, running is
//! an inference for later phases. API errors are FACT rows. And the
//! tolerate-unknown rule runs in both directions: rows the build does not
//! know become explicit unrecognized entries — the format is internal to
//! Claude Code and whole row generations appear and vanish across
//! versions — while absent fields never crash a fold.
//!
//! The subagent and API-error rows here are authored to the shapes in
//! `notes/chat-v1/transcript-semantics.md` §12/§13/§20 (no Phase 0
//! scenario captured them); the real-Claude suite (req. H) re-grounds
//! them when those scenarios land.

use amux_ui::Msg;
use amux_ui::claude::{FeedEntryKind, SuccessFacts, ToolInvocation, ToolOutcome, TurnDuration};
use serde_json::json;

use crate::harness::*;

const SESSION: &str = "22222222-2222-4222-8222-222222222222";

fn envelope(uuid: u128) -> serde_json::Value {
    json!({
        "uuid": uuid::Uuid::from_u128(uuid).to_string(),
        "sessionId": SESSION,
        "timestamp": "2026-08-11T22:00:01.000Z",
    })
}

fn row(uuid: u128, mut extra: serde_json::Value) -> serde_json::Value {
    let mut base = envelope(uuid);
    base.as_object_mut()
        .expect("object")
        .append(extra.as_object_mut().expect("object"));
    base
}

/// A synchronous Task renders as one line with the child's description;
/// completion is FACT (`toolUseResult.status:"completed"` with duration
/// and tool count). Child transcript files are not tailed (B7).
#[test]
fn a_synchronous_subagent_is_one_line_with_completion_facts() {
    let spawn = row(
        0x7000,
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_task", "role": "assistant", "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": "toolu_task", "name": "Task",
                    "input": {"description": "survey the fixtures", "subagent_type": "Explore"}
                }]
            }
        }),
    );
    let done = row(
        0x7001,
        json!({
            "type": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_task", "content": "…"
            }]},
            "toolUseResult": {
                "agentId": "abc123", "agentType": "Explore", "status": "completed",
                "totalDurationMs": 52000, "totalToolUseCount": 7, "totalTokens": 9000,
                "content": "…", "prompt": "…"
            }
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, vec![spawn, done])],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let tool = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the task line");
    assert!(matches!(
        &tool.invocation,
        ToolInvocation::Task { description: Some(text), background: false, .. }
            if text == "survey the fixtures"
    ));
    assert_eq!(
        tool.outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::TaskCompleted {
                agent_id: Some("abc123".to_string()),
                duration_ms: Some(52000),
                tool_count: Some(7),
            }
        }
    );
}

/// A background Task acknowledges its launch (FACT), completion arrives
/// later as a task-notification user row (FACT it finished, content is
/// prose), and turn ends expose the FACT count of still-running agents.
#[test]
fn a_background_subagent_launches_notifies_and_is_counted() {
    let spawn = row(
        0x7100,
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_bg", "role": "assistant", "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": "toolu_bg", "name": "Task",
                    "input": {"description": "long survey", "subagent_type": "Explore",
                               "run_in_background": true}
                }]
            }
        }),
    );
    let launched = row(
        0x7101,
        json!({
            "type": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_bg", "content": "…"
            }]},
            "toolUseResult": {
                "agentId": "bg9", "isAsync": true, "status": "async_launched",
                "description": "long survey", "outputFile": "…", "prompt": "…"
            }
        }),
    );
    let turn_end = row(
        0x7102,
        json!({
            "type": "system", "subtype": "turn_duration",
            "durationMs": 4000, "messageCount": 3, "pendingBackgroundAgentCount": 1
        }),
    );
    let notice = row(
        0x7103,
        json!({
            "type": "user",
            "message": {"role": "user", "content": "Background task bg9 finished."},
            "origin": {"kind": "task-notification"},
            "promptSource": "system"
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![spawn, launched, turn_end, notice],
        )],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let tool = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the task line");
    assert!(matches!(
        &tool.invocation,
        ToolInvocation::Task {
            background: true,
            ..
        }
    ));
    assert_eq!(
        tool.outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::TaskLaunched {
                agent_id: Some("bg9".to_string())
            }
        }
    );
    let turn = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .expect("the turn marker");
    assert_eq!(turn.duration, TurnDuration::Measured { ms: 4000 });
    assert_eq!(turn.pending_background_agents, Some(1));
    assert!(
        layer.entries().any(|entry| matches!(
            &entry.kind,
            FeedEntryKind::TaskNotification(notice)
                if notice.text.contains("finished")
        )),
        "the completion notice is a one-line fact"
    );
}

/// An API error row is a status entry, not a message (B8/§20): the
/// synthetic message never enters the upsert path.
#[test]
fn an_api_error_row_is_a_status_entry_not_a_message() {
    let error = row(
        0x7200,
        json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "error": "server_error",
            "message": {
                "id": uuid::Uuid::from_u128(0xdead).to_string(),
                "model": "<synthetic>", "role": "assistant",
                "stop_reason": "stop_sequence",
                "content": [{"type": "text", "text": "API Error: upstream overloaded"}]
            }
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, vec![error])],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let api_error = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::ApiError(error) => Some(error),
            _ => None,
        })
        .expect("the error entry");
    assert_eq!(api_error.error.as_deref(), Some("server_error"));
    assert!(
        api_error
            .text
            .as_deref()
            .unwrap_or("")
            .contains("API Error")
    );
    assert!(
        !layer
            .entries()
            .any(|entry| matches!(entry.kind, FeedEntryKind::Message(_))),
        "synthetic rows never become messages"
    );
}

/// Unknown row types are retained and rendered as explicit unrecognized
/// entries, never silently dropped (G1) — including the vanished older
/// generations (`progress`, `summary`) and rows with no type at all.
#[test]
fn unknown_rows_become_explicit_unrecognized_entries() {
    let rows = vec![
        json!({"type": "progress", "data": {"type": "agent_progress"}}),
        json!({"type": "summary", "summary": "an older-generation row"}),
        json!({"noType": true}),
        row(
            0x7300,
            json!({"type": "system", "subtype": "hologram", "content": "…"}),
        ),
        row(
            0x7301,
            json!({
                "type": "assistant",
                "message": {"id": "msg_x", "role": "assistant", "stop_reason": "end_turn",
                             "content": [{"type": "hologram", "data": "…"}]}
            }),
        ),
    ];
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, rows)],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let unrecognized: Vec<_> = layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Unrecognized(entry) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(unrecognized.len(), 5, "every unknown shape is stated");
    assert_eq!(unrecognized[0].row_type.as_deref(), Some("progress"));
    assert_eq!(unrecognized[2].row_type, None);
    assert_eq!(unrecognized[3].detail.as_deref(), Some("hologram"));
    assert_eq!(unrecognized[4].row_type.as_deref(), Some("assistant"));
}

/// Rules never crash on absent fields: rows stripped to nothing but a
/// type still fold (to unrecognized entries or nothing), and the model
/// stays coherent.
#[test]
fn skeletal_rows_never_crash_the_fold() {
    let rows = vec![
        json!({"type": "user"}),
        json!({"type": "assistant"}),
        json!({"type": "system"}),
        json!({"type": "attachment"}),
        json!({"type": "permission-mode"}),
        json!({"type": "hook.stop"}),
        json!({"type": "hook.permission_request"}),
        json!({"type": "amux.transcript_ready"}),
    ];
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, rows)],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(layer.transcript_ready());
    assert!(layer.turn_end_presignal());
    // The contentless user row and the unknown system subtype are stated.
    assert_eq!(
        layer
            .entries()
            .filter(|entry| matches!(entry.kind, FeedEntryKind::Unrecognized(_)))
            .count(),
        2
    );
}

/// `redacted_thinking` renders the same marker flagged redacted (B3).
#[test]
fn redacted_thinking_is_a_flagged_marker() {
    let redacted = row(
        0x7400,
        json!({
            "type": "assistant",
            "message": {"id": "msg_r", "role": "assistant", "stop_reason": "end_turn",
                         "content": [{"type": "redacted_thinking", "data": "…"}]}
        }),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, vec![redacted])],
    ]));
    let thinking = claude_layer(&model, "fix-auth-bug")
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("the marker");
    assert!(thinking.redacted);
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![(
        "feed_edges::authored_rows",
        seq([
            chat_base("fix-auth-bug"),
            vec![batch(
                "fix-auth-bug",
                10,
                vec![
                    json!({"type": "progress", "data": {}}),
                    row(
                        0x7500,
                        json!({
                            "type": "assistant",
                            "isApiErrorMessage": true,
                            "error": "server_error",
                            "message": {"id": "e", "role": "assistant",
                                         "stop_reason": "stop_sequence",
                                         "content": [{"type": "text", "text": "API Error"}]}
                        }),
                    ),
                ],
            )],
        ]),
    )]
}
