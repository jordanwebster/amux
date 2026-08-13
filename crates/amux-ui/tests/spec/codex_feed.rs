//! Chapter 14 — Codex rows: native items, turns, deltas, and boundaries.
//!
//! Codex is a continuous row stream, not a task-per-turn actor. Turn state is
//! derived only from `turn/started` and matching `turn/completed` rows;
//! item rows upsert by item id even outside a live turn. The first ready row
//! is a quiet attach marker, while later ready rows are visible re-syncs.

use amux_ui::codex::{
    BoundaryEntry, CodexPhase, FeedEntryKind, ItemFinality, MessagePhase, TurnStatus, WorkKind,
    WorkOutcome, WorkState,
};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "codex-fix";

fn feed(rows: Vec<serde_json::Value>) -> amux_ui::Model {
    fold(seq([codex_base(AGENT), vec![batch(AGENT, 10, rows)]]))
}

fn rich_turn() -> Vec<serde_json::Value> {
    vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started", "turn":{"id":"turn-1","status":"inProgress"}}),
        json!({"type":"item/started", "turnId":"turn-1", "item":{
            "id":"user-1", "type":"userMessage",
            "content":[{"type":"text","text":"Reply exactly PONG."}]
        }}),
        json!({"type":"item/completed", "turnId":"turn-1", "item":{
            "id":"user-1", "type":"userMessage",
            "content":[{"type":"text","text":"Reply exactly PONG."}]
        }}),
        json!({"type":"item/started", "item":{
            "id":"msg-1", "type":"agentMessage", "text":"", "phase":"final_answer"
        }}),
        json!({"type":"item/agentMessage/delta", "itemId":"msg-1", "delta":"P"}),
        json!({"type":"item/agentMessage/delta", "itemId":"msg-1", "delta":"ONG?"}),
        json!({"type":"item/completed", "item":{
            "id":"msg-1", "type":"agentMessage", "text":"PONG", "phase":"final_answer"
        }}),
        json!({"type":"thread/tokenUsage/updated", "tokenUsage":{
            "total":{"inputTokens":12,"cachedInputTokens":3,"outputTokens":4,
                     "reasoningOutputTokens":1,"totalTokens":16},
            "modelContextWindow":128000
        }}),
        json!({"type":"turn/completed", "turn":{"id":"turn-1","status":"completed"}}),
    ]
}

pub fn sequences() -> Vec<(&'static str, Vec<amux_ui::Msg>)> {
    vec![
        (
            "codex protocol prompt and authoritative message",
            seq([codex_base(AGENT), vec![batch(AGENT, 10, rich_turn())]]),
        ),
        (
            "codex committed P5b structural fixture",
            seq([
                codex_base(AGENT),
                vec![batch(AGENT, 20, codex_fixture_rows())],
            ]),
        ),
        (
            "codex gap and repeatable ready",
            seq([
                codex_base(AGENT),
                vec![batch(
                    AGENT,
                    30,
                    vec![
                        json!({"type":"amux.codex_ready"}),
                        json!({"type":"amux.codex_gap","reason":"connection_lost"}),
                        json!({"type":"amux.codex_ready"}),
                    ],
                )],
            ]),
        ),
    ]
}

#[test]
fn user_messages_are_protocol_sourced_and_messages_grow_then_finalize_in_place() {
    let model = feed(rich_turn());
    let layer = codex_layer(&model, AGENT);
    let entries: Vec<_> = layer.entries().collect();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry.kind, FeedEntryKind::Prompt(_)))
            .count(),
        1,
        "started/completed upsert one protocol-sourced prompt"
    );
    let message = entries
        .iter()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Message(message) => Some(message),
            _ => None,
        })
        .expect("message entry");
    assert_eq!(message.text, "PONG", "completed text beats deltas");
    assert_eq!(message.phase, MessagePhase::FinalAnswer);
    assert_eq!(message.finality, ItemFinality::Complete);

    let turn = entries
        .iter()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Turn(turn) => Some(turn),
            _ => None,
        })
        .expect("turn entry");
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.token_usage.as_ref().unwrap().total_tokens, Some(16));
    assert_eq!(layer.active_turn_id(), None);
    assert!(matches!(
        amux_ui::codex::phase(&model, agent_id(AGENT)),
        CodexPhase::Idle
    ));
}

#[test]
fn command_output_backfills_only_when_no_deltas_arrived() {
    let rows = vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"t","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"a","type":"commandExecution",
            "command":"printf A","cwd":"/work","status":"inProgress"}}),
        json!({"type":"item/completed","item":{"id":"a","type":"commandExecution",
            "command":"printf A","cwd":"/work","status":"completed",
            "exitCode":0,"aggregatedOutput":"A"}}),
        json!({"type":"item/started","item":{"id":"b","type":"commandExecution",
            "command":"printf B","cwd":"/work","status":"inProgress"}}),
        json!({"type":"item/commandExecution/outputDelta","itemId":"b","stream":"stdout","delta":"B"}),
        json!({"type":"item/completed","item":{"id":"b","type":"commandExecution",
            "command":"printf B","cwd":"/work","status":"completed",
            "exitCode":7,"aggregatedOutput":"DUPLICATE"}}),
    ];
    let model = feed(rows);
    let work: Vec<_> = codex_layer(&model, AGENT)
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Work(work) => Some(work),
            _ => None,
        })
        .collect();
    assert_eq!(work.len(), 2);
    assert_eq!(work[0].stdout_head, "A", "aggregated output backfilled");
    assert_eq!(work[1].stdout_head, "B", "deltas suppress backfill");
    assert!(matches!(
        work[1].kind,
        WorkKind::Command {
            exit_code: Some(7),
            ..
        }
    ));
    assert_eq!(
        work[1].state,
        WorkState::Done {
            outcome: WorkOutcome::Succeeded
        }
    );
}

#[test]
fn reasoning_plans_files_tools_usage_compaction_errors_and_unknowns_are_visible() {
    let rows = vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"t","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"r","type":"reasoning","content":[],"summary":[]}}),
        json!({"type":"item/reasoning/summaryPartAdded","itemId":"r","summaryIndex":0}),
        json!({"type":"item/reasoning/summaryTextDelta","itemId":"r","summaryIndex":0,"delta":"summary"}),
        json!({"type":"item/reasoning/textDelta","itemId":"r","delta":"private thought"}),
        json!({"type":"item/completed","item":{"id":"r","type":"reasoning",
            "content":["final thought"],"summary":["final summary"]}}),
        json!({"type":"item/plan/delta","itemId":"p","delta":"step one"}),
        json!({"type":"turn/plan/updated","explanation":"because","plan":[
            {"step":"test","status":"inProgress"}
        ]}),
        json!({"type":"item/fileChange/patchUpdated","itemId":"f","changes":[
            {"path":"/work/a.rs","kind":{"type":"modify"},"diff":"+x"}
        ]}),
        json!({"type":"turn/diff/updated","turnId":"t","diff":"diff --git a/a b/a"}),
        json!({"type":"item/started","item":{"id":"m","type":"mcpToolCall",
            "server":"srv","tool":"lookup","arguments":{"x":1},"status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"d","type":"dynamicToolCall",
            "tool":"deploy","arguments":{},"status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"w","type":"webSearch","query":"rust"}}),
        json!({"type":"thread/compacted","turnId":"t"}),
        json!({"type":"warning","message":"nearly full"}),
        json!({"type":"error","error":{"message":"retrying"},"willRetry":true}),
        json!({"type":"future/method","payload":1}),
    ];
    let model = feed(rows);
    let entries: Vec<_> = codex_layer(&model, AGENT).entries().collect();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.kind, FeedEntryKind::Reasoning(_)))
    );
    assert!(entries.iter().any(
        |e| matches!(e.kind, FeedEntryKind::Work(ref w) if matches!(w.kind, WorkKind::Plan { .. }))
    ));
    assert!(entries.iter().any(|e| matches!(e.kind, FeedEntryKind::Work(ref w) if matches!(w.kind, WorkKind::McpTool { .. }))));
    assert!(entries.iter().any(|e| matches!(e.kind, FeedEntryKind::Work(ref w) if matches!(w.kind, WorkKind::DynamicTool { .. }))));
    assert!(entries.iter().any(|e| matches!(e.kind, FeedEntryKind::Work(ref w) if matches!(w.kind, WorkKind::WebSearch { .. }))));
    assert!(entries.iter().any(|e| matches!(
        e.kind,
        FeedEntryKind::Boundary(BoundaryEntry::Compacted { .. })
    )));
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e.kind, FeedEntryKind::Error(_)))
            .count(),
        2
    );
    assert!(
        entries.iter().any(
            |e| matches!(&e.kind, FeedEntryKind::Unrecognized(u) if u.method == "future/method")
        )
    );
}

#[test]
fn gap_and_ready_reset_accumulators_but_preserve_the_observed_interrupt_id() {
    let rows = vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"turn-live","status":"inProgress"}}),
        json!({"type":"item/agentMessage/delta","itemId":"m","delta":"old"}),
        json!({"type":"amux.codex_gap","reason":"connection_lost"}),
        json!({"type":"amux.codex_ready"}),
        json!({"type":"item/agentMessage/delta","itemId":"m","delta":"new"}),
    ];
    let model = feed(rows);
    let layer = codex_layer(&model, AGENT);
    let message = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Message(message) if message.item_id == "m" => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(message.text, "new", "pre-gap delta buffer was reset");
    assert_eq!(layer.active_turn_id(), Some("turn-live"));
    assert!(
        layer
            .entries()
            .any(|e| matches!(e.kind, FeedEntryKind::Boundary(BoundaryEntry::Gap { .. })))
    );
    assert!(
        layer
            .entries()
            .any(|e| matches!(e.kind, FeedEntryKind::Boundary(BoundaryEntry::Ready)))
    );
}

#[test]
fn a_late_old_turn_completion_does_not_clear_the_current_turn() {
    let model = feed(vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"old","status":"inProgress"}}),
        json!({"type":"turn/started","turn":{"id":"new","status":"inProgress"}}),
        json!({"type":"turn/completed","turn":{"id":"old","status":"completed"}}),
        json!({"type":"item/completed","item":{"id":"late","type":"agentMessage",
            "text":"late history","phase":"commentary"}}),
    ]);
    assert_eq!(codex_layer(&model, AGENT).active_turn_id(), Some("new"));
    assert!(matches!(
        amux_ui::codex::phase(&model, agent_id(AGENT)),
        CodexPhase::Thinking
    ));
}

#[test]
fn the_committed_capture_is_a_regression_anchor_even_with_redacted_payloads() {
    let model = feed(codex_fixture_rows());
    let layer = codex_layer(&model, AGENT);
    assert!(layer.entry_count() > 0);
    assert!(model.check_invariants().is_empty());
}
