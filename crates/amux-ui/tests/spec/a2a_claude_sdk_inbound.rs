//! SDK agent messages retain their sender, lifecycle kind and delivery identity.
//! The outbound tool uses Claude's shared vocabulary from the captured MCP rows.

use amux_ui::claude::facts::ToolInvocation;
use amux_ui::claude_sdk::FeedEntryKind;
use amux_ui::{AgentMessageKind, Model, Msg};
use serde_json::Value;

use crate::harness::*;

const AGENT: &str = "sdk-recipient";

fn rows(name: &str) -> Vec<Value> {
    let raw = match name {
        "message" => include_str!("../../../amux/tests/fixtures/a2a/sdk_recipient.rows.jsonl"),
        "completed" => include_str!("../../../amux/tests/fixtures/a2a/sdk_completed.rows.jsonl"),
        "exited" => include_str!("../../../amux/tests/fixtures/a2a/sdk_exited.rows.jsonl"),
        "send" => {
            // The PTY capture retained the tool body but omitted message IDs.
            // Add the SDK envelope identity; the captured call and result stay intact.
            let mut rows = a2a_rows("mcp_tools");
            rows[0]["message"]["id"] = "sdk-mcp-capture".into();
            rows[0]["uuid"] = "sdk-mcp-send".into();
            return rows;
        }
        _ => panic!("unknown carrier"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn sequence(name: &str) -> Vec<Msg> {
    seq([
        claude_sdk_base(AGENT),
        rows(name)
            .into_iter()
            .enumerate()
            .map(|(i, row)| batch(AGENT, i as i64, vec![row]))
            .collect(),
    ])
}

#[test]
fn message_completion_and_empty_exit_keep_identity_without_borrowing_the_humans_voice() {
    for (name, kind) in [
        ("message", AgentMessageKind::Message),
        ("completed", AgentMessageKind::Completed),
        ("exited", AgentMessageKind::Exited),
    ] {
        let model = fold(sequence(name));
        capture(name, &model);
        let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
        assert_eq!(entries.len(), 1);
        let FeedEntryKind::AgentMessage(message) = &entries[0].kind else {
            panic!("an agent delivery must remain an agent message");
        };
        let envelope = &rows(name)[0]["envelope"];
        assert_eq!(message.kind, kind);
        assert_eq!(message.id.as_deref(), envelope["id"].as_str());
        assert_eq!(message.context.as_deref(), envelope["context"].as_str());
        assert_eq!(message.from, envelope["from"]["name"]);
        assert_eq!(message.text, envelope["text"]);
        assert_eq!(message.delivery.as_deref(), Some("stream"));
    }
}

#[test]
fn outbound_send_names_the_recipient_and_pairs_the_captured_tool_result() {
    let model = fold(sequence("send"));
    capture("send", &model);
    let entries: Vec<_> = claude_sdk_layer(&model, AGENT).entries().collect();
    assert_eq!(entries.len(), 1);
    let FeedEntryKind::Tool(tool) = &entries[0].kind else {
        panic!("send tool")
    };
    assert_eq!(tool.name, "mcp__amux__send");
    assert_eq!(
        tool.invocation,
        ToolInvocation::AmuxSend {
            to: Some("probe".into()),
            text: Some("A2A_MCP_SENT_21240".into()),
        }
    );
    let result = tool.result.as_ref().unwrap();
    assert!(!result.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap()["id"],
        "a2a-mcp-capture"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    ["message", "completed", "exited", "send"]
        .into_iter()
        .map(|name| (name, sequence(name)))
        .collect()
}

#[test]
fn recorded_agent_message_sequences_equal_live_state_after_every_message() {
    for (name, msgs) in sequences() {
        crate::wire_free::assert_differential_sequence(name, msgs);
    }
}

fn capture(name: &str, model: &Model) {
    if let Some(path) = std::env::var_os("CLAUDE_SDK_A2A_EVIDENCE") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_string_pretty(
                &claude_sdk_layer(model, AGENT).entries().collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
