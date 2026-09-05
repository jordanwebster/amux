//! Claude task-list snapshots replace session facts without growing the feed.
use amux_ui::claude::{FeedEntryKind, ToolOutcome};
use amux_ui::provider::{TaskList, TodoState, facts};
use amux_ui::{Model, Msg, StreamCloseReason, StreamMsg, update};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "todos";
fn rows() -> Vec<Value> {
    include_str!("../fixtures/todos/rows.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn messages() -> Vec<Msg> {
    seq([
        chat_base(AGENT),
        rows()
            .into_iter()
            .enumerate()
            .map(|(i, row)| batch(AGENT, i as i64 + 10, vec![row]))
            .collect(),
    ])
}
pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![("Claude task list replacements and clearing", messages())]
}
fn todos(model: &Model) -> Option<TaskList> {
    facts(model, agent_id(AGENT)).todos
}

#[test]
fn todos_recording_matches_live_after_every_message_and_checkpoint() {
    let mut live = Model::default();
    let mut recording = Vec::new();
    for msg in messages() {
        recording.push(serde_json::to_string(&msg).unwrap());
        update(&mut live, msg);
        let replay = fold(
            recording
                .iter()
                .map(|line| serde_json::from_str(line).unwrap()),
        );
        assert_eq!(live, replay);
        let checkpoint: Model =
            serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        assert_eq!(live, checkpoint);
        assert!(live.check_invariants().is_empty());
    }
}

#[test]
fn todos_replace_complete_and_clear_without_transcript_rows() {
    let mut model = fold(chat_base(AGENT));
    assert_eq!(todos(&model), None);
    let mut previous = None;
    for (i, pair) in rows().chunks(2).enumerate() {
        update(
            &mut model,
            batch(AGENT, i as i64 * 2 + 10, vec![pair[0].clone()]),
        );
        assert_eq!(
            todos(&model),
            previous,
            "unconfirmed writes leave the prior list intact"
        );
        update(
            &mut model,
            batch(AGENT, i as i64 * 2 + 11, vec![pair[1].clone()]),
        );
        let list = todos(&model).unwrap();
        let expected = [
            (1, 3, Some("Building the app")),
            (1, 2, Some("Running checks")),
            (1, 1, None),
            (0, 0, None),
        ][i];
        assert_eq!((list.done, list.total, list.current.as_deref()), expected);
        assert_eq!(list.items.len(), list.total);
        if i == 0 {
            assert_eq!(
                list.items,
                vec![
                    ("Read the code".into(), TodoState::Completed),
                    ("Build the app".into(), TodoState::InProgress),
                    ("Run checks".into(), TodoState::Pending),
                ]
            );
        }
        assert_eq!(claude_layer(&model, AGENT).entry_count(), 0);
        println!(
            "Confirmed task list: {}",
            serde_json::to_string(&list).unwrap()
        );
        previous = Some(list);
    }
    update(&mut model, batch(AGENT, 30, rows()));
    assert_eq!(
        todos(&model),
        previous,
        "shrink replay cannot restore an older list"
    );
    // Duplicate content blocks in fresh row envelopes are idempotent too.
    let repeated = rows()
        .into_iter()
        .map(|mut row| {
            row.as_object_mut().unwrap().remove("uuid");
            row
        })
        .collect();
    update(&mut model, batch(AGENT, 31, repeated));
    assert_eq!(todos(&model), previous);
    assert_eq!(claude_layer(&model, AGENT).entry_count(), 0);
}

#[test]
fn todos_errors_and_unknown_shapes_preserve_the_list_and_remain_visible() {
    for status in [None, Some("future_status"), Some("in_progress")] {
        let mut model = fold(seq([
            chat_base(AGENT),
            vec![batch(AGENT, 10, rows()[..2].to_vec())],
        ]));
        let before = todos(&model);
        let mut tool = rows()[2].clone();
        if let Some(status) = status {
            tool["message"]["content"][0]["input"]["todos"][0]["status"] = json!(status);
        } else {
            tool["message"]["content"][0]["input"] = json!({});
        }
        let mut result = rows()[3].clone();
        result["message"]["content"][0]["is_error"] = json!(true);
        update(&mut model, batch(AGENT, 12, vec![tool]));
        assert_eq!(
            claude_layer(&model, AGENT).entry_count(),
            usize::from(status != Some("in_progress")),
            "unknown task shapes retain the original invocation"
        );
        update(&mut model, batch(AGENT, 13, vec![result]));
        assert_eq!(todos(&model), before);
        assert!(claude_layer(&model, AGENT).entries().any(|entry| matches!(&entry.kind,
            FeedEntryKind::Tool(tool) if tool.name.as_deref() == Some("TodoWrite") && matches!(tool.outcome, ToolOutcome::Failed { .. }))));
    }
}

#[test]
fn todos_survive_feed_eviction_and_disconnect_but_reset_with_a_new_window() {
    let mut model = fold(seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, rows()[..2].to_vec())],
    ]));
    let before = todos(&model);
    for i in 0..1001 {
        update(
            &mut model,
            batch(AGENT, 20 + i, vec![json!({"type":"future_row", "value":i})]),
        );
    }
    assert_eq!(todos(&model), before);
    update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::HostUnreachable,
            },
        ),
    );
    assert_eq!(todos(&model), before);
    update(
        &mut model,
        stream(AGENT, StreamMsg::Opened { truncated: true }),
    );
    assert_eq!(todos(&model), None);
    update(&mut model, batch(AGENT, 1100, rows()[..2].to_vec()));
    assert_eq!(todos(&model), before);
    update(
        &mut model,
        batch(
            AGENT,
            1101,
            vec![json!({"type":"mode", "sessionId":"new-session"})],
        ),
    );
    assert_eq!(todos(&model), None);
}

#[test]
fn todos_pending_checkpoint_resolves_once_and_never_guesses_the_current_task() {
    for status in ["pending", "in_progress", "completed"] {
        let mut tool = rows()[0].clone();
        tool["message"]["content"][0]["input"]["todos"] = json!([
            {"content":"Read the code", "status":status}
        ]);
        let mut live = fold(seq([chat_base(AGENT), vec![batch(AGENT, 10, vec![tool])]]));
        let mut restored: Model =
            serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        let result = batch(AGENT, 11, vec![rows()[1].clone()]);
        update(&mut live, result.clone());
        update(&mut restored, result);
        assert_eq!(live, restored);
        assert_eq!(
            todos(&live).unwrap().current.as_deref(),
            (status == "in_progress").then_some("Read the code")
        );
    }
}
