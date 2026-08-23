//! Chapter 21 — Claude outbound: sending a message to another agent.
//!
//! The inbound half of an amux conversation is read out of the recipient's
//! own rows (chapter 20). The outbound half is already a row Claude
//! records: the `mcp__amux__send` tool call. It folds to its own
//! invocation so the feed can say who the message left for, rather than
//! printing an MCP tool name and a JSON blob.
//!
//! Folded over the graduated a2a MCP capture.

use amux_ui::Msg;
use amux_ui::claude::{ClaudeLayer, FeedEntryKind, ToolEntry, ToolInvocation, ToolOutcome};

use crate::harness::*;

const AGENT: &str = "sender";

fn send_sequence() -> Vec<Msg> {
    seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, a2a_rows("mcp_tools"))],
    ])
}

/// A `mcp__amux__send` call with the given arguments, in the row shape
/// Claude records for any MCP tool.
fn send_call(id: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": id,
                "name": "mcp__amux__send",
                "input": input,
            }],
        },
    })
}

fn tools(layer: &ClaudeLayer) -> Vec<ToolEntry> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool.clone()),
            _ => None,
        })
        .collect()
}

/// The captured send folds to its own invocation, naming the agent the
/// message went to and carrying what was said.
#[test]
fn a2a_claude_send_row_folds_the_mcp_call() {
    let model = fold(send_sequence());
    let tools = tools(claude_layer(&model, AGENT));
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_deref(), Some("mcp__amux__send"));
    assert_eq!(
        tools[0].invocation,
        ToolInvocation::AmuxSend {
            to: Some("probe".to_string()),
            text: Some("A2A_MCP_SENT_21240".to_string()),
        }
    );
}

/// It is still an ordinary tool row: the paired result closes it exactly
/// as any other tool's does, so a send that failed reads as a failure.
#[test]
fn a2a_claude_send_row_pairs_with_its_result() {
    let model = fold(send_sequence());
    let captured = tools(claude_layer(&model, AGENT));
    assert!(
        matches!(captured[0].outcome, ToolOutcome::Success { .. }),
        "the captured send completed: {:?}",
        captured[0].outcome
    );

    let failed = fold(seq([
        chat_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![
                send_call(
                    "toolu_gone",
                    serde_json::json!({"to": "ghost", "text": "hi"}),
                ),
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_gone",
                        "content": "no agent named ghost",
                        "is_error": true,
                    }]},
                }),
            ],
        )],
    ]));
    let tools = tools(claude_layer(&failed, AGENT));
    assert!(
        matches!(tools[0].outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        tools[0].outcome
    );
}

/// Arguments a build does not get are absent, not invented: a call with no
/// recipient still renders as a send.
#[test]
fn a2a_claude_send_row_degrades_without_arguments() {
    let model = fold(seq([
        chat_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![send_call("toolu_bare", serde_json::json!({}))],
        )],
    ]));
    let tools = tools(claude_layer(&model, AGENT));
    assert_eq!(
        tools[0].invocation,
        ToolInvocation::AmuxSend {
            to: None,
            text: None
        }
    );
}

/// Only `send` has a row shape of its own. The other amux tools are
/// ordinary tool calls and stay that way — inventing a shape per tool is
/// how a feed grows a dialect.
#[test]
fn a2a_claude_send_row_leaves_the_other_amux_tools_ordinary() {
    let mut call = send_call("toolu_spawn", serde_json::json!({"kind": "codex"}));
    call["message"]["content"][0]["name"] = serde_json::json!("mcp__amux__spawn");
    let model = fold(seq([chat_base(AGENT), vec![batch(AGENT, 10, vec![call])]]));
    let tools = tools(claude_layer(&model, AGENT));
    assert_eq!(tools[0].name.as_deref(), Some("mcp__amux__spawn"));
    assert_eq!(tools[0].invocation, ToolInvocation::Other);
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![("a2a_claude_send_row::mcp_capture", send_sequence())]
}
