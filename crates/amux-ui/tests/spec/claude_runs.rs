//! Claude's feed projection groups consecutive read-only exploration while
//! leaving the native entries available unchanged. Run identity and membership
//! are domain facts; path preview limits remain renderer policy.

use amux_ui::Model;
use amux_ui::claude::FeedItem;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::harness::*;

const AGENT: &str = "fix-auth";
const SESSION: &str = "22222222-2222-4222-8222-222222222222";

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

fn tool_row(index: u64, name: &str, input: Value) -> Value {
    json!({
        "type": "assistant",
        "uuid": Uuid::from_u128(0x6000 + u128::from(index)).to_string(),
        "sessionId": SESSION,
        "timestamp": "2026-08-12T09:00:01.000Z",
        "message": {
            "id": "msg-explore",
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": format!("toolu-{index}"),
                "name": name,
                "input": input
            }]
        }
    })
}

fn result_row(index: u64) -> Value {
    json!({
        "type": "user",
        "uuid": Uuid::from_u128(0x7000 + u128::from(index)).to_string(),
        "sessionId": SESSION,
        "timestamp": "2026-08-12T09:00:02.000Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": format!("toolu-{index}"),
                "content": "done"
            }]
        }
    })
}

fn read(index: u64, path: &str) -> Value {
    tool_row(index, "Read", json!({"file_path": path}))
}

fn messages(rows: Vec<Value>) -> Vec<amux_ui::Msg> {
    seq([chat_base(AGENT), vec![batch(AGENT, 10, rows)]])
}

fn items(rows: Vec<Value>) -> Vec<Item> {
    let model = fold(messages(rows));
    claude_layer(&model, AGENT)
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
fn three_reads_fold_under_the_first_entry_id_with_every_path() {
    assert_eq!(
        items(vec![read(1, "a.rs"), read(2, "b.rs"), read(3, "c.rs")]),
        vec![Item::Run {
            id: 0,
            member_ids: vec![0, 1, 2],
            reads: 3,
            searches: 0,
            read_paths: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
        }]
    );
}

#[test]
fn consequential_tools_split_exploration_runs() {
    for (name, input) in [
        ("Edit", json!({"file_path": "a.rs"})),
        ("Write", json!({"file_path": "a.rs"})),
        ("Bash", json!({"command": "cargo test"})),
    ] {
        assert_eq!(
            items(vec![
                read(1, "a.rs"),
                tool_row(2, name, input),
                read(3, "b.rs")
            ]),
            vec![Item::Entry(0), Item::Entry(1), Item::Entry(2)],
            "{name} remains outside exploration runs"
        );
    }
}

#[test]
fn a_lone_read_stays_an_entry() {
    assert_eq!(items(vec![read(1, "only.rs")]), vec![Item::Entry(0)]);
}

#[test]
fn an_in_place_tool_outcome_does_not_interrupt_the_run() {
    assert_eq!(
        items(vec![
            read(1, "a.rs"),
            result_row(1),
            tool_row(2, "Grep", json!({"pattern": "retry"})),
            read(3, "b.rs"),
        ]),
        vec![Item::Run {
            id: 0,
            member_ids: vec![0, 1, 2],
            reads: 2,
            searches: 1,
            read_paths: vec!["a.rs".into(), "b.rs".into()],
        }]
    );
}

#[test]
fn a_missing_agent_has_no_layer_or_items() {
    assert!(Model::default().claude(agent_id("missing")).is_none());
}

pub fn sequences() -> Vec<(&'static str, Vec<amux_ui::Msg>)> {
    vec![(
        "claude_runs::read_search_run",
        messages(vec![
            read(1, "a.rs"),
            tool_row(2, "Grep", json!({"pattern": "retry"})),
            read(3, "b.rs"),
        ]),
    )]
}
