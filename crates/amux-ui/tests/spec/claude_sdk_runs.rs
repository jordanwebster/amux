//! The session feed's exploration runs: consecutive reads and searches
//! group under their first entry, and anything consequential splits them.
//! Run identity and membership are domain facts; path preview limits
//! remain renderer policy.

use amux_ui::claude_sdk::FeedItem;
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-runs";

#[derive(Debug, PartialEq, Eq)]
enum Item {
    Entry(u64),
    Run {
        id: u64,
        member_ids: Vec<u64>,
        reads: usize,
        searches: usize,
        read_paths: Vec<String>,
    },
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

fn assistant(index: u64, content: Vec<Value>) -> Value {
    json!({
        "type": "assistant",
        "parent_tool_use_id": null,
        "message": {
            "id": format!("msg_{index}"),
            "role": "assistant",
            "content": content,
        },
    })
}

fn read(id: &str, path: &str) -> Value {
    tool_use(id, "Read", json!({"file_path": path}))
}

/// An assistant row a subagent produced: the same shape, marked with the
/// tool use that launched it.
fn subagent_assistant(index: u64, parent: &str, content: Vec<Value>) -> Value {
    let mut row = assistant(index, content);
    row["parent_tool_use_id"] = json!(parent);
    row
}

fn search(id: &str, pattern: &str) -> Value {
    tool_use(id, "Grep", json!({"pattern": pattern}))
}

fn items(rows: Vec<Value>) -> Vec<Item> {
    let model = fold(seq([
        claude_sdk_base(AGENT),
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| batch(AGENT, i as i64, vec![row]))
            .collect(),
    ]));
    claude_sdk_layer(&model, AGENT)
        .feed_items()
        .map(|item| match item {
            FeedItem::Entry(entry) => Item::Entry(entry.id),
            FeedItem::ExplorationRun {
                id,
                member_ids,
                reads,
                searches,
                read_paths,
            } => Item::Run {
                id,
                member_ids,
                reads,
                searches,
                read_paths: read_paths.into_iter().map(str::to_string).collect(),
            },
        })
        .collect()
}

#[test]
fn consecutive_reads_and_searches_fold_under_the_first_entry_with_every_path() {
    assert_eq!(
        items(vec![assistant(
            1,
            vec![
                search("toolu_1", "max_attempts"),
                read("toolu_2", "a.rs"),
                read("toolu_3", "b.rs"),
            ],
        )]),
        vec![Item::Run {
            id: 0,
            member_ids: vec![0, 1, 2],
            reads: 2,
            searches: 1,
            read_paths: vec!["a.rs".into(), "b.rs".into()],
        }]
    );
}

/// An edit is consequential, so it keeps its own row and ends the run
/// that reached it; what follows starts a new one.
#[test]
fn a_consequential_tool_splits_a_run_in_two() {
    assert_eq!(
        items(vec![assistant(
            1,
            vec![
                read("toolu_1", "a.rs"),
                read("toolu_2", "b.rs"),
                tool_use("toolu_3", "Edit", json!({"file_path": "a.rs"})),
                read("toolu_4", "c.rs"),
                search("toolu_5", "retry"),
            ],
        )]),
        vec![
            Item::Run {
                id: 0,
                member_ids: vec![0, 1],
                reads: 2,
                searches: 0,
                read_paths: vec!["a.rs".into(), "b.rs".into()],
            },
            Item::Entry(2),
            Item::Run {
                id: 3,
                member_ids: vec![3, 4],
                reads: 1,
                searches: 1,
                read_paths: vec!["c.rs".into()],
            },
        ]
    );
}

#[test]
fn one_read_on_its_own_stays_an_entry() {
    assert_eq!(
        items(vec![assistant(1, vec![read("toolu_1", "a.rs")])]),
        vec![Item::Entry(0)]
    );
}

/// A tool block opens before its input has finished arriving. Its name is
/// there from the start, so the block that follows it can already tell it
/// is joining an exploration run rather than starting one.
#[test]
fn a_run_forms_while_the_tool_inputs_are_still_streaming() {
    let open = |index: u64, id: &str, name: &str| {
        json!({
            "type": "stream_event",
            "parent_tool_use_id": null,
            "event": {
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}},
            },
        })
    };
    let items = items(vec![
        json!({
            "type": "stream_event",
            "parent_tool_use_id": null,
            "event": {
                "type": "message_start",
                "message": {"id": "msg_stream", "role": "assistant", "content": []},
            },
        }),
        open(0, "toolu_1", "Read"),
        open(1, "toolu_2", "Grep"),
    ]);
    assert_eq!(
        items,
        vec![Item::Run {
            id: 0,
            member_ids: vec![0, 1],
            reads: 1,
            searches: 1,
            read_paths: Vec::new(),
        }]
    );
}

/// A subagent's reads arrive on the same stream as the session's, marked
/// with the tool use that launched them. They are that subagent's own
/// timeline: they never join the session's run on either side of them,
/// and they form no run of their own, since each paints as one
/// attributed line.
#[test]
fn a_subagents_reads_never_fold_into_the_sessions_exploration() {
    assert_eq!(
        items(vec![
            assistant(1, vec![read("toolu_1", "a.rs"), read("toolu_2", "b.rs")]),
            subagent_assistant(
                2,
                "toolu_task",
                vec![read("toolu_3", "c.rs"), read("toolu_4", "d.rs")],
            ),
            assistant(3, vec![read("toolu_5", "e.rs"), search("toolu_6", "retry")]),
        ]),
        vec![
            Item::Run {
                id: 0,
                member_ids: vec![0, 1],
                reads: 2,
                searches: 0,
                read_paths: vec!["a.rs".into(), "b.rs".into()],
            },
            Item::Entry(2),
            Item::Entry(3),
            Item::Run {
                id: 4,
                member_ids: vec![4, 5],
                reads: 1,
                searches: 1,
                read_paths: vec!["e.rs".into()],
            },
        ]
    );
}
