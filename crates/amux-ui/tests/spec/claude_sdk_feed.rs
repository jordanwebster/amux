//! Claude SDK feed: authoritative blocks, task lifecycles and honest tails.
//!
//! Recorded daemon rows anchor provider shapes. Synthetic rows exercise
//! truncation, interleaving and future vocabulary absent from the corpus.

use amux_ui::claude_sdk::{
    BoundaryEntry, CONTENT_BYTES_RETAINED, FEED_RETAINED, FeedEntryKind, Finality, TaskState,
};
use amux_ui::{AgentMessageKind, Model, Msg, StreamMsg, update};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-feed";

fn rows(name: &str) -> Vec<Value> {
    let raw = match name {
        "streamed" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
        }
        "text" => include_str!("../../../amux/tests/fixtures/rows/claude-sdk/text_turn.rows.jsonl"),
        "tools" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/max_turns.rows.jsonl")
        }
        "tasks" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/subagent_task.rows.jsonl")
        }
        "compacted" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/compacted.rows.jsonl")
        }
        "cleared" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/cleared.rows.jsonl")
        }
        "interrupted" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/interrupted.rows.jsonl")
        }
        "recipient" => include_str!("../../../amux/tests/fixtures/a2a/sdk_recipient.rows.jsonl"),
        "completed" => include_str!("../../../amux/tests/fixtures/a2a/sdk_completed.rows.jsonl"),
        "exited" => include_str!("../../../amux/tests/fixtures/a2a/sdk_exited.rows.jsonl"),
        _ => panic!("unknown fixture"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn sequence(rows: Vec<Value>) -> Vec<Msg> {
    seq([
        claude_sdk_base(AGENT),
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| batch(AGENT, i as i64, vec![row]))
            .collect(),
    ])
}

fn feed(rows: Vec<Value>) -> Model {
    fold(sequence(rows))
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    [
        "streamed",
        "text",
        "tools",
        "tasks",
        "compacted",
        "cleared",
        "interrupted",
        "recipient",
        "completed",
        "exited",
    ]
    .into_iter()
    .map(|name| (name, sequence(rows(name))))
    .collect()
}

#[test]
fn recorded_streaming_blocks_grow_then_finalize_in_place_without_losing_siblings() {
    let mut model = fold(claude_sdk_base(AGENT));
    let mut identities = Vec::new();
    let mut saw_live_text = false;
    for (i, row) in rows("streamed").into_iter().enumerate() {
        update(&mut model, batch(AGENT, i as i64, vec![row]));
        for entry in claude_sdk_layer(&model, AGENT).entries() {
            if let FeedEntryKind::Message(message) = &entry.kind {
                if !message.text.is_empty() && message.finality == Finality::Streaming {
                    saw_live_text = true;
                    capture(&model, "streaming-reply");
                }
                if !message.text.is_empty() {
                    identities.push(entry.id);
                }
            }
        }
    }
    capture(&model, "final-reply");
    assert!(
        saw_live_text,
        "recorded text delta is visible before final row"
    );
    assert!(
        identities.iter().all(|id| *id == identities[0]),
        "final text replaces the same feed block"
    );
    let layer = claude_sdk_layer(&model, AGENT);
    let messages: Vec<_> = layer
        .entries()
        .filter_map(|e| match &e.kind {
            FeedEntryKind::Message(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "SEVEN");
    assert_eq!(messages[0].finality, Finality::Complete);
    let thinking: Vec<_> = layer
        .entries()
        .filter_map(|e| match &e.kind {
            FeedEntryKind::Thinking(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(thinking.len(), 1);
    assert!(!thinking[0].text.is_empty());
    assert_eq!(thinking[0].finality, Finality::Complete);
    assert!(
        !layer
            .entries()
            .any(|e| matches!(e.kind, FeedEntryKind::Unrecognized(_)))
    );
}

#[test]
fn final_fragments_share_a_message_id_but_keep_both_tool_calls_and_their_results() {
    let model = feed(rows("tools"));
    capture(&model, "tool-results-and-usage");
    let tools: Vec<_> = claude_sdk_layer(&model, AGENT)
        .entries()
        .filter_map(|e| match &e.kind {
            FeedEntryKind::Tool(t) => Some((e, t)),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(
        tools[0].0.block.as_ref().unwrap().message_id,
        tools[1].0.block.as_ref().unwrap().message_id
    );
    assert_ne!(tools[0].1.tool_use_id, tools[1].1.tool_use_id);
    for (_, tool) in tools {
        assert_eq!(tool.name, "Write");
        assert!(
            tool.result
                .as_ref()
                .unwrap()
                .text
                .contains("File created successfully")
        );
        assert!(!tool.result.as_ref().unwrap().is_error);
    }
    let turn = claude_sdk_layer(&model, AGENT)
        .entries()
        .find_map(|e| match &e.kind {
            FeedEntryKind::Turn(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert!(turn.is_error);
    assert_eq!(turn.outcome, "error_max_turns");
    assert_eq!(turn.usage.output_tokens, Some(283));
    assert_eq!(turn.usage.cache_read_input_tokens, Some(19155));
    assert_eq!(turn.num_turns, Some(2));
    assert!(turn.total_cost_usd.unwrap() > 0.0);
    assert_eq!(
        turn.model_usage.as_ref().unwrap()["claude-haiku-4-5-20251001"]["contextWindow"],
        200000
    );
}

#[test]
fn recorded_task_lifecycle_updates_one_entry_with_completion_and_usage() {
    let model = feed(rows("tasks"));
    capture(&model, "completed-task");
    let tasks: Vec<_> = claude_sdk_layer(&model, AGENT).tasks().collect();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].description, "Count to three");
    assert_eq!(tasks[0].subagent_type.as_deref(), Some("counter"));
    assert_eq!(tasks[0].state, TaskState::Completed);
    assert_eq!(tasks[0].summary.as_deref(), Some("ONE TWO THREE"));
    assert_eq!(tasks[0].usage.as_ref().unwrap().total_tokens, Some(1440));
    assert_eq!(tasks[0].usage.as_ref().unwrap().duration_ms, Some(2375));

    // The launch row and the task are one subagent, so they are one entry:
    // the `Agent` tool use is taken over where it sat, and the subagent's
    // own rows arrive marked with that launch.
    let layer = claude_sdk_layer(&model, AGENT);
    assert_eq!(
        tasks[0].tool_use_id.as_deref(),
        Some("toolu_011tXt8wWZsDcqNkmr2GAzYp")
    );
    assert!(
        !layer
            .entries()
            .any(|e| matches!(&e.kind, FeedEntryKind::Tool(t) if t.name == "Agent")),
        "the launch tool row must not survive beside its task"
    );
    let subagent_rows: Vec<_> = layer
        .entries()
        .filter(|e| e.parent_tool_use_id() == Some("toolu_011tXt8wWZsDcqNkmr2GAzYp"))
        .collect();
    assert!(
        subagent_rows
            .iter()
            .any(|e| matches!(&e.kind, FeedEntryKind::Message(m) if m.text.contains("ONE"))),
        "the subagent's own reply arrives marked with its launch: {subagent_rows:?}"
    );
    // The session repeated the result in its own words; that reply is the
    // session's, and stays unmarked.
    assert!(
        layer
            .entries()
            .filter(|e| e.parent_tool_use_id().is_none())
            .any(|e| matches!(&e.kind, FeedEntryKind::Message(m) if m.text == "ONE TWO THREE")),
        "the session's own reply stays the session's"
    );
}

/// The prompt a parent hands its subagent arrives as a `user` row marked
/// with the launch; it is the task's description, not something the
/// person said, so it makes no prompt entry. The subagent's tool results
/// still pair with its own tool rows.
#[test]
fn a_subagents_prompt_row_is_not_the_persons_prompt() {
    let model = feed(vec![
        json!({"type":"user","uuid":"u1","parent_tool_use_id":"toolu_launch",
               "message":{"role":"user","content":"Count to three and report the result."}}),
        json!({"type":"assistant","uuid":"u2","parent_tool_use_id":"toolu_launch",
        "message":{"id":"msg_child","role":"assistant","content":[
            {"type":"tool_use","id":"toolu_child_read","name":"Read","input":{"file_path":"a.rs"}}
        ]}}),
        json!({"type":"user","uuid":"u3","parent_tool_use_id":"toolu_launch",
        "message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_child_read","content":"fn main() {}"}
        ]}}),
    ]);
    let layer = claude_sdk_layer(&model, AGENT);
    assert!(
        !layer
            .entries()
            .any(|e| matches!(e.kind, FeedEntryKind::Prompt(_))),
        "no prompt entry for the subagent's own prompt"
    );
    let read = layer
        .entries()
        .find_map(|e| match &e.kind {
            FeedEntryKind::Tool(t) if t.tool_use_id == "toolu_child_read" => Some((e, t)),
            _ => None,
        })
        .expect("the subagent's read");
    assert_eq!(read.0.parent_tool_use_id(), Some("toolu_launch"));
    assert_eq!(
        read.1.result.as_ref().map(|r| r.text.as_str()),
        Some("fn main() {}")
    );
}

/// A task row can land while the launching tool block is still streaming,
/// and the task list can name the task before any row carries its launch
/// id. The entry stays the task through all of it: the final row for the
/// block adds nothing the lifecycle rows do not state, and the launch
/// tool's own result is not the task's outcome.
#[test]
fn a_task_keeps_its_launch_row_through_late_tool_rows_and_results() {
    let stream =
        |event: Value| json!({"type": "stream_event", "parent_tool_use_id": null, "event": event});
    let model = feed(vec![
        stream(json!({"type": "message_start", "message": {"id": "msg_launch"}})),
        stream(
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "toolu_launch", "name": "Task", "input": {}
            }}),
        ),
        stream(json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": "{\"description\": \"count to three\""
        }})),
        // The task list names the task before its launch id is known.
        json!({"type":"system","subtype":"background_tasks_changed","tasks":[
            {"task_id":"t1","description":"count to three","task_type":"local_agent"}
        ]}),
        json!({"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_launch",
               "description":"count to three","subagent_type":"counter"}),
        stream(json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": ", \"subagent_type\": \"counter\"}"
        }})),
        stream(json!({"type": "content_block_stop", "index": 0})),
        json!({
            "type": "assistant",
            "uuid": "u1",
            "parent_tool_use_id": null,
            "message": {"id": "msg_launch", "role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_launch", "name": "Task",
                "input": {"description": "count to three", "subagent_type": "counter"}
            }]},
        }),
        json!({"type":"user","uuid":"u3","parent_tool_use_id":null,"message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_launch","content":"Agent launched."}
        ]}}),
        json!({"type":"system","subtype":"task_notification","task_id":"t1","tool_use_id":"toolu_launch",
               "status":"completed","summary":"THREE"}),
    ]);
    let layer = claude_sdk_layer(&model, AGENT);
    let entries: Vec<_> = layer.entries().collect();
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e.kind, FeedEntryKind::Task(_) | FeedEntryKind::Tool(_)))
            .count(),
        1,
        "one entry for one subagent: {entries:?}"
    );
    let task = layer.tasks().next().expect("the task");
    assert_eq!(task.description, "count to three");
    assert_eq!(task.subagent_type.as_deref(), Some("counter"));
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.summary.as_deref(), Some("THREE"));
}

#[test]
fn compaction_reset_status_and_synthetic_user_rows_remain_distinct() {
    let model = feed(rows("compacted"));
    capture(&model, "compaction");
    let layer = claude_sdk_layer(&model, AGENT);
    assert!(layer.entries().any(|e| matches!(&e.kind, FeedEntryKind::Compaction(c) if c.pre_tokens == Some(29543) && c.post_tokens == Some(2963))));
    assert!(
        layer
            .entries()
            .any(|e| matches!(&e.kind, FeedEntryKind::Status(s) if s.status == "compacting"))
    );
    assert!(layer.entries().any(|e| matches!(&e.kind, FeedEntryKind::Prompt(p) if p.synthetic && p.text.contains("Summary:"))));
    assert!(
        !layer.history_truncated(),
        "provider compaction does not erase retained chat history"
    );
    let cleared = feed(rows("cleared"));
    assert!(
        claude_sdk_layer(&cleared, AGENT)
            .entries()
            .any(|e| matches!(
                e.kind,
                FeedEntryKind::Boundary(BoundaryEntry::ConversationReset { .. })
            ))
    );
    let interrupted = feed(rows("interrupted"));
    assert!(
        claude_sdk_layer(&interrupted, AGENT)
            .entries()
            .any(|e| matches!(&e.kind, FeedEntryKind::Turn(t) if t.is_error))
    );
}

#[test]
fn daemon_envelopes_preserve_agent_messages_completion_and_exit() {
    for (fixture, kind) in [
        ("recipient", AgentMessageKind::Message),
        ("completed", AgentMessageKind::Completed),
        ("exited", AgentMessageKind::Exited),
    ] {
        let model = feed(rows(fixture));
        let message = claude_sdk_layer(&model, AGENT)
            .entries()
            .find_map(|e| match &e.kind {
                FeedEntryKind::AgentMessage(m) => Some(m),
                _ => None,
            })
            .unwrap();
        assert_eq!(message.kind, kind);
        assert_eq!(message.delivery.as_deref(), Some("stream"));
        assert!(message.id.is_some());
        assert_eq!(
            message.text,
            rows(fixture)[0]["envelope"]["text"].as_str().unwrap()
        );
        assert!(matches!(message.from.as_str(), "parent" | "child"));
    }
}

fn event(parent: Option<&str>, event: Value) -> Value {
    json!({"type":"stream_event", "parent_tool_use_id":parent, "event":event})
}
fn start(id: &str, parent: Option<&str>) -> Value {
    event(parent, json!({"type":"message_start", "message":{"id":id}}))
}
fn text_start(index: u64) -> Value {
    event(
        None,
        json!({"type":"content_block_start", "index":index, "content_block":{"type":"text","text":""}}),
    )
}
fn delta(text: &str) -> Value {
    event(
        None,
        json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta","text":text}}),
    )
}

#[test]
fn recorded_accepted_prompts_are_visible_before_their_replies() {
    for name in ["text", "streamed", "tools", "tasks", "interrupted"] {
        let recorded = rows(name);
        let row = recorded
            .iter()
            .find(|row| row["type"] == "user" && row["input_id"].is_string())
            .expect("the daemon publishes accepted input");
        let model = feed(recorded.clone());
        let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
        let prompt_index = entries
            .iter()
            .position(|entry| {
                matches!(&entry.kind,
            FeedEntryKind::Prompt(prompt) if prompt.uuid.as_deref() == row["uuid"].as_str())
            })
            .unwrap();
        let FeedEntryKind::Prompt(prompt) = &entries[prompt_index].kind else {
            unreachable!()
        };
        assert_eq!(prompt.text, row["message"]["content"].as_str().unwrap());
        assert!(!prompt.synthetic);
        assert!(!prompt.replay);
        let reply_index = entries
            .iter()
            .position(|entry| {
                matches!(
                    &entry.kind,
                    FeedEntryKind::Message(_) | FeedEntryKind::Thinking(_) | FeedEntryKind::Tool(_)
                )
            })
            .unwrap();
        assert!(prompt_index < reply_index, "{name}: prompt precedes work");
        capture(&model, &format!("{name}-accepted-prompt"));
    }
}

#[test]
fn prompts_images_and_unknown_rows_are_visible_without_fabricated_content() {
    let model = feed(vec![
        json!({"type":"user", "uuid":"p", "message":{"content":[{"type":"text","text":"hello"},{"type":"image","source":{}}]}}),
        json!({"type":"future"}),
        json!({"type":"assistant","message":{"content":[]}}),
    ]);
    let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
    assert!(
        matches!(&entries[0].kind, FeedEntryKind::Prompt(p) if p.text == "hello" && p.image_count == 1 && !p.synthetic)
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e.kind, FeedEntryKind::Unrecognized(_)))
            .count(),
        2
    );
}

#[test]
fn authoritative_text_beats_deltas_and_duplicate_final_rows() {
    let final_row = json!({"type":"assistant", "uuid":"a", "message":{"id":"m", "content":[{"type":"text", "text":"final"}]}});
    let model = feed(vec![
        start("m", None),
        text_start(0),
        delta("draft"),
        final_row.clone(),
        delta("late"),
        final_row,
    ]);
    let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
    assert_eq!(entries.len(), 1);
    assert!(
        matches!(&entries[0].kind, FeedEntryKind::Message(m) if m.text == "final" && m.finality == Finality::Complete)
    );
}

#[test]
fn tool_input_streams_then_authoritative_input_and_error_result_replace_it() {
    let model = feed(vec![
        start("m", None),
        event(
            None,
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"Bash","input":{}}}),
        ),
        event(
            None,
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"false\"}"}}),
        ),
        json!({"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"false"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true,"content":"exit 1"}]}}),
    ]);
    let tool = claude_sdk_layer(&model, AGENT)
        .entries()
        .find_map(|e| match &e.kind {
            FeedEntryKind::Tool(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool.input.as_ref().unwrap()["command"], "false");
    assert!(tool.result.as_ref().unwrap().is_error);
    assert_eq!(tool.result.as_ref().unwrap().text, "exit 1");
}

#[test]
fn parent_and_child_streams_do_not_share_the_active_message_cursor() {
    let model = feed(vec![
        start("parent", None),
        text_start(0),
        start("child", Some("task")),
        delta("parent text"),
        event(
            Some("task"),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"child text"}}),
        ),
    ]);
    let messages: Vec<_> = claude_sdk_layer(&model, AGENT)
        .entries()
        .filter_map(|e| match &e.kind {
            FeedEntryKind::Message(m) => Some(m.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(messages, ["parent text", "child text"]);
}

#[test]
fn results_gaps_and_disconnects_stop_incomplete_streams_and_history_loss_is_sticky() {
    for end in [
        json!({"type":"result","subtype":"success"}),
        json!({"type":"amux.claude_sdk.gap", "resumed_session_id":"s"}),
    ] {
        let mut model = feed(vec![
            start("m", None),
            text_start(0),
            delta("unfinished"),
            end,
        ]);
        assert!(claude_sdk_layer(&model, AGENT).entries().any(
            |e| matches!(&e.kind, FeedEntryKind::Message(m) if m.finality == Finality::Interrupted)
        ));
        let loss = claude_sdk_layer(&model, AGENT).history_truncated();
        update(
            &mut model,
            batch(
                AGENT,
                40,
                vec![json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":true})],
            ),
        );
        assert_eq!(claude_sdk_layer(&model, AGENT).history_truncated(), loss);
    }
    let mut model = feed(vec![start("m", None), delta("unfinished")]);
    update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: amux_ui::StreamCloseReason::TransportError {
                    message: "lost".into(),
                },
            },
        ),
    );
    assert!(claude_sdk_layer(&model, AGENT).entries().any(
        |e| matches!(&e.kind, FeedEntryKind::Message(m) if m.finality == Finality::Interrupted)
    ));
}

#[test]
fn retention_bounds_entries_and_stream_bytes_and_reports_only_real_history_loss() {
    let model = feed(
        (0..FEED_RETAINED + 3)
            .map(|i| json!({"type":"user", "uuid":i.to_string(), "message":{"content":"hello"}}))
            .collect(),
    );
    let layer = claude_sdk_layer(&model, AGENT);
    assert_eq!(layer.entry_count(), FEED_RETAINED);
    assert_eq!(layer.evicted_entries(), 3);
    assert!(layer.history_truncated());
    let model = feed(vec![
        start("m", None),
        delta(&"é".repeat(CONTENT_BYTES_RETAINED)),
        delta("more"),
    ]);
    let layer = claude_sdk_layer(&model, AGENT);
    let entry = layer.entries().next().unwrap();
    assert!(entry.content_truncated);
    assert!(
        !layer.history_truncated(),
        "clipped block does not claim earlier entries are missing"
    );
    assert!(
        matches!(&entry.kind, FeedEntryKind::Message(m) if m.text.len() == CONTENT_BYTES_RETAINED)
    );
    let model = feed(vec![
        json!({"type":"amux.claude_sdk.message", "envelope":{"text":"notice", "kind":"x".repeat(CONTENT_BYTES_RETAINED + 1)}}),
    ]);
    let entry = claude_sdk_layer(&model, AGENT).entries().next().unwrap();
    assert!(entry.content_truncated);
    assert!(matches!(&entry.kind, FeedEntryKind::AgentMessage(m)
        if matches!(&m.kind, AgentMessageKind::Other { label } if label.len() == CONTENT_BYTES_RETAINED)));
    let model = fold(seq([
        claude_sdk_base(AGENT),
        vec![stream(AGENT, StreamMsg::Opened { truncated: true })],
    ]));
    assert!(claude_sdk_layer(&model, AGENT).history_truncated());
}

#[test]
fn replay_tail_reconciles_a_final_block_at_its_observed_stream_index() {
    let model = feed(vec![
        start("m", None),
        text_start(3),
        event(
            None,
            json!({"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"draft"}}),
        ),
        json!({"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"text","text":"final"}]}}),
    ]);
    let entries: Vec<_> = claude_sdk_layer(&model, AGENT)
        .entries()
        .filter(|e| e.block.as_ref().is_some_and(|b| b.index == 3))
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(
        matches!(&entries[0].kind, FeedEntryKind::Message(m) if m.text == "final" && m.finality == Finality::Complete)
    );
}

#[test]
fn task_progress_and_unknown_status_preserve_each_tasks_identity_and_last_facts() {
    let model = feed(vec![
        json!({"type":"system","subtype":"task_started","task_id":"a","description":"inspect"}),
        json!({"type":"system","subtype":"task_started","task_id":"b","description":"build"}),
        json!({"type":"system","subtype":"task_progress","task_id":"a","last_tool_name":"Read","usage":{"total_tokens":12}}),
        json!({"type":"system","subtype":"task_updated","task_id":"b","patch":{"status":"future-state"}}),
    ]);
    let tasks: Vec<_> = claude_sdk_layer(&model, AGENT).tasks().collect();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].last_tool.as_deref(), Some("Read"));
    assert_eq!(tasks[0].state, TaskState::Running);
    assert_eq!(tasks[1].state, TaskState::Unknown("future-state".into()));
    assert_eq!(tasks[1].description, "build");
}

#[test]
fn orphan_results_and_missing_stream_starts_are_readable_without_inventing_work() {
    let model = feed(vec![
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"missing","content":"retained result"}]}}),
        delta("no start"),
        json!({"type":"system","subtype":"future"}),
    ]);
    let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
    assert!(
        matches!(&entries[0].kind, FeedEntryKind::Tool(t) if t.name.is_empty() && t.input.is_none() && t.result.as_ref().unwrap().text == "retained result")
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e.kind, FeedEntryKind::Unrecognized(_)))
            .count(),
        2
    );
}

#[test]
fn bounded_feed_checkpoints_resume_the_same_fold_with_stream_cursors_intact() {
    for (_, sequence) in sequences() {
        let mut live = Model::default();
        for msg in sequence {
            let mut checkpoint: Model =
                serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
            update(&mut checkpoint, msg.clone());
            update(&mut live, msg);
            assert_eq!(checkpoint, live);
        }
    }
}

/// Optional capture of the public feed boundary, without inventing a screen
/// before a renderer exists. The rows still pass through the production reducer.
fn capture(model: &Model, name: &str) {
    if let Some(directory) = std::env::var_os("CLAUDE_SDK_FEED_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let layer = claude_sdk_layer(model, AGENT);
        let value = json!({"history_truncated":layer.history_truncated(), "entries":layer.entries().collect::<Vec<_>>()});
        std::fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

/// A replay tail can begin at the task rows, after the launch block was
/// evicted or never retained. The launch's final row then arrives for a
/// task that has no block: it attaches to the task rather than adding a
/// tool beside it, and the launch tool's result pairs with the task.
#[test]
fn a_lifecycle_first_tail_attaches_the_late_launch_block_to_its_task() {
    let model = feed(vec![
        json!({"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_launch",
               "description":"count to three","subagent_type":"counter"}),
        json!({
            "type": "assistant",
            "uuid": "u1",
            "parent_tool_use_id": null,
            "message": {"id": "msg_launch", "role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_launch", "name": "Task",
                "input": {"description": "count to three", "subagent_type": "counter"}
            }]},
        }),
        json!({"type":"user","uuid":"u2","parent_tool_use_id":null,"message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_launch","content":"Agent launched."}
        ]}}),
    ]);
    let layer = claude_sdk_layer(&model, AGENT);
    let entries: Vec<_> = layer.entries().collect();
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e.kind, FeedEntryKind::Task(_) | FeedEntryKind::Tool(_)))
            .count(),
        1,
        "one entry for one subagent: {entries:?}"
    );
    let task = entries
        .iter()
        .find(|e| matches!(e.kind, FeedEntryKind::Task(_)))
        .expect("the task");
    assert_eq!(
        task.block.as_ref().map(|b| b.message_id.as_str()),
        Some("msg_launch"),
        "the late launch block now belongs to the task"
    );
}

/// A subagent's tool result can arrive without its invocation — a tail
/// that starts at the result, or an evicted launch. The retained result
/// still says whose it was.
#[test]
fn a_result_only_tail_keeps_its_subagent_attribution() {
    let model = feed(vec![json!({
        "type":"user","uuid":"u1","parent_tool_use_id":"toolu_launch",
        "message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_orphan","content":"done"}
        ]}
    })]);
    let entry = claude_sdk_layer(&model, AGENT)
        .entries()
        .find(|e| matches!(&e.kind, FeedEntryKind::Tool(t) if t.tool_use_id == "toolu_orphan"))
        .expect("the result-only tool row");
    assert!(entry.block.is_none());
    assert_eq!(entry.parent_tool_use_id(), Some("toolu_launch"));
}

/// A clipped lifecycle row leaves the task marked as clipped; the launch
/// block's own small final row does not clear that.
#[test]
fn a_late_launch_row_does_not_clear_a_tasks_clipped_content_flag() {
    let huge = "x".repeat(CONTENT_BYTES_RETAINED + 1);
    let model = feed(vec![
        json!({"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_launch",
               "description": huge, "subagent_type":"counter"}),
        json!({
            "type": "assistant",
            "uuid": "u1",
            "parent_tool_use_id": null,
            "message": {"id": "msg_launch", "role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_launch", "name": "Task", "input": {}
            }]},
        }),
    ]);
    let task = claude_sdk_layer(&model, AGENT)
        .entries()
        .find(|e| matches!(e.kind, FeedEntryKind::Task(_)))
        .expect("the task");
    assert!(task.content_truncated);
}
