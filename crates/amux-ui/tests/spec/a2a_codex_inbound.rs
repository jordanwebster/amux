//! Chapter 22 — Codex inbound: agent messages and amux's own tools.
//!
//! Codex's carrier injects a message into a thread, and the native thread
//! then shows nothing for it — so the daemon writes the one synthesized row
//! that says a message arrived, and this layer reads it. The outbound half
//! is an MCP tool call against the server amux runs for the thread, which
//! Codex does report; amux's own tools are separated from anyone else's so
//! the fleet's work on itself reads in the fleet's words.
//!
//! The message half folds the graduated Codex 0.148.0 structural capture.
//! The tool half is built from the `mcpToolCall` item shape, so a running,
//! completed and failed call are all visible in one chapter rather than
//! only whichever outcome a recorded turn happened to take.

use amux_ui::codex::{
    AgentMessageEntry, CodexLayer, FeedEntryKind, WorkEntry, WorkKind, WorkOutcome, WorkState,
};
use amux_ui::{AgentMessageKind, Msg};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "codex-child";

/// The row the daemon writes when a carrier accepts a message.
fn message_row(kind: &str, delivery: &str) -> serde_json::Value {
    json!({
        "type": "amux.codex_message",
        "id": "00000000-0000-0000-0000-0000000000a1",
        "kind": kind,
        "from": "lead/00000000-0000-0000-0000-0000000000c0",
        "from_id": "00000000-0000-0000-0000-0000000000b0",
        "context": "00000000-0000-0000-0000-0000000000a0",
        "text": "review the patch",
        "delivery": delivery,
    })
}

/// The committed structural capture, which carries one delivered message.
fn fixture_sequence() -> Vec<Msg> {
    seq([
        codex_base(AGENT),
        vec![batch(AGENT, 20, codex_fixture_rows())],
    ])
}

/// An `mcpToolCall` item against a named server, in the shape Codex reports
/// one: the same fields carry a call that has only started and a call that
/// has landed.
fn mcp_row(finality: &str, id: &str, server: &str, tool: &str, rest: serde_json::Value) -> Value {
    let mut item = json!({
        "id": id,
        "type": "mcpToolCall",
        "server": server,
        "tool": tool,
        "arguments": {"working_on": "reviewing the patch"},
    });
    let (Value::Object(item_fields), Value::Object(rest)) = (&mut item, rest) else {
        unreachable!("both are objects");
    };
    item_fields.extend(rest);
    json!({"type": format!("item/{finality}"), "item": item})
}

/// One amux tool call over amux's own MCP server, from started to completed.
fn tool_sequence() -> Vec<Msg> {
    seq([
        codex_base(AGENT),
        vec![batch(
            AGENT,
            30,
            vec![
                mcp_row(
                    "started",
                    "call-1",
                    amux::agent_tools::MCP_SERVER_NAME,
                    "status",
                    json!({"status": "inProgress"}),
                ),
                mcp_row(
                    "completed",
                    "call-1",
                    amux::agent_tools::MCP_SERVER_NAME,
                    "status",
                    json!({
                        "status": "completed",
                        "result": {"content": [{"type": "text", "text": "status set"}]},
                    }),
                ),
            ],
        )],
    ])
}

fn send_sequence() -> Vec<Msg> {
    seq([
        codex_base(AGENT),
        vec![batch(
            AGENT,
            35,
            vec![mcp_row(
                "completed",
                "send-1",
                amux::agent_tools::MCP_SERVER_NAME,
                "send",
                json!({
                    "arguments": {"to": "reviewer", "text": "\n  \nreview the patch"},
                    "status": "completed"
                }),
            )],
        )],
    ])
}

fn messages(layer: &CodexLayer) -> Vec<AgentMessageEntry> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::AgentMessage(message) => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn work(layer: &CodexLayer) -> Vec<WorkEntry> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Work(work) => Some(work.clone()),
            _ => None,
        })
        .collect()
}

/// The synthesized row folds to a message, carrying every fact the daemon
/// authored — including which of the three carriers accepted it.
#[test]
fn a2a_codex_inbound_folds_the_synthesized_message_row() {
    let model = fold(fixture_sequence());
    let inbound = messages(codex_layer(&model, AGENT));
    assert_eq!(inbound.len(), 1, "the capture carries one delivery");
    let message = &inbound[0];
    assert_eq!(message.kind, AgentMessageKind::Message);
    assert_eq!(message.text, "hello from another agent");
    assert_eq!(message.delivery.as_deref(), Some("inject_queued"));
    assert!(message.from.starts_with("sender/"));
    assert!(message.id.is_some() && message.context.is_some());
    assert!(model.check_invariants().is_empty());
}

/// The daemon states the kind; a kind this build does not know is shown as
/// what it is rather than dropped or guessed at.
#[test]
fn a2a_codex_inbound_keeps_every_stated_kind() {
    let rows = vec![
        message_row("message", "inject_queued"),
        message_row("completed", "inject_started"),
        message_row("exited", "turn_started"),
        message_row("teleported", "inject_queued"),
    ];
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 40, rows)]]));
    let kinds: Vec<AgentMessageKind> = messages(codex_layer(&model, AGENT))
        .into_iter()
        .map(|message| message.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            AgentMessageKind::Message,
            AgentMessageKind::Completed,
            AgentMessageKind::Exited,
            AgentMessageKind::Other {
                label: "teleported".to_string()
            },
        ]
    );
}

/// A message is not a turn and raises no attention of its own (U7): a
/// queued delivery is not the human being needed.
#[test]
fn a2a_codex_inbound_raises_no_attention() {
    let before = fold(codex_base(AGENT));
    let after = fold(seq([
        codex_base(AGENT),
        vec![batch(
            AGENT,
            40,
            vec![message_row("message", "inject_queued")],
        )],
    ]));
    assert_eq!(
        after.agent(agent_id(AGENT)).unwrap().attention,
        before.agent(agent_id(AGENT)).unwrap().attention,
    );
}

/// A call to amux's own tool on amux's own server reads as amux's work at
/// every point of its life: running while it is open, and succeeded once
/// Codex says it completed.
#[test]
fn a2a_codex_inbound_folds_an_amux_mcp_tool_call() {
    let model = fold(tool_sequence());
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 1, "one item, folded twice: {work:?}");
    let WorkKind::AmuxTool {
        tool,
        arguments,
        success,
    } = &work[0].kind
    else {
        panic!("expected amux work: {work:?}");
    };
    assert_eq!(tool, "status");
    assert_eq!(arguments["working_on"], json!("reviewing the patch"));
    assert_eq!(*success, Some(true));
    assert_eq!(
        work[0].state,
        WorkState::Done {
            outcome: WorkOutcome::Succeeded
        }
    );
    assert!(model.check_invariants().is_empty());
}

#[test]
fn a2a_codex_inbound_types_recognized_sends_and_keeps_unknown_shapes_raw() {
    let mut msgs = send_sequence();
    msgs.extend(seq([vec![batch(
        AGENT,
        36,
        vec![mcp_row(
            "completed",
            "send-raw",
            amux::agent_tools::MCP_SERVER_NAME,
            "send",
            json!({
                "arguments": {"to": "reviewer", "text": ["not", "a", "string"]},
                "status": "completed"
            }),
        )],
    )]]));
    let model = fold(msgs);
    let work = work(codex_layer(&model, AGENT));
    assert!(matches!(
        &work[0].kind,
        WorkKind::AmuxSend { to, text, success: Some(true) }
            if to == "reviewer" && text == "\n  \nreview the patch"
    ));
    assert!(matches!(
        &work[1].kind,
        WorkKind::AmuxTool { tool, arguments, success: Some(true) }
            if tool == "send" && arguments["text"].is_array()
    ));
}

/// A call still in flight is amux's work with no verdict yet, rather than a
/// silent success or a failure the model never reported.
#[test]
fn a2a_codex_inbound_reads_a_running_amux_mcp_call_as_undecided() {
    let model = fold(seq([
        codex_base(AGENT),
        vec![batch(
            AGENT,
            30,
            vec![mcp_row(
                "started",
                "call-1",
                amux::agent_tools::MCP_SERVER_NAME,
                "spawn",
                json!({"status": "inProgress"}),
            )],
        )],
    ]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 1);
    assert!(
        matches!(&work[0].kind, WorkKind::AmuxTool { success: None, .. }),
        "{work:?}"
    );
    assert_eq!(work[0].state, WorkState::Running);
}

/// A failure is stated either way Codex states it — a failed status, or an
/// error body on a call whose status still claims completion. A stated error
/// settles it, because a tool that reported an error did not do the work.
#[test]
fn a2a_codex_inbound_reads_a_failed_amux_mcp_call_as_failed() {
    let rows = vec![
        mcp_row(
            "completed",
            "call-failed",
            amux::agent_tools::MCP_SERVER_NAME,
            "stop",
            json!({"status": "failed", "error": {"message": "no such child"}}),
        ),
        mcp_row(
            "completed",
            "call-errored",
            amux::agent_tools::MCP_SERVER_NAME,
            "send",
            json!({"status": "completed", "error": {"message": "unknown agent"}}),
        ),
    ];
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 40, rows)]]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 2);
    for entry in &work {
        assert!(
            matches!(
                &entry.kind,
                WorkKind::AmuxTool {
                    success: Some(false),
                    ..
                }
            ),
            "{entry:?}"
        );
    }
    assert_eq!(
        work[0].state,
        WorkState::Done {
            outcome: WorkOutcome::Failed
        }
    );
}

/// The discriminator is amux's own server plus amux's own tool list, not a
/// name that looks familiar: another server offering a tool amux also names
/// stays that server's, and a tool amux never defined stays generic even on
/// amux's server.
#[test]
fn a2a_codex_inbound_leaves_foreign_mcp_calls_alone() {
    let rows = vec![
        mcp_row(
            "completed",
            "mcp-1",
            "other",
            "send",
            json!({"status": "completed"}),
        ),
        mcp_row(
            "completed",
            "mcp-2",
            amux::agent_tools::MCP_SERVER_NAME,
            "teleport",
            json!({"status": "completed"}),
        ),
    ];
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 50, rows)]]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 2);
    assert!(
        work.iter()
            .all(|entry| matches!(entry.kind, WorkKind::McpTool { .. })),
        "{work:?}"
    );
}

/// amux registers no dynamic tools at all, so every dynamic call is
/// somebody else's — including one that borrows an amux tool name, with or
/// without a namespace. Reading such a call as amux's work would credit the
/// fleet with work it never did.
#[test]
fn a2a_codex_inbound_leaves_every_dynamic_tool_generic() {
    let rows = vec![
        json!({"type":"item/completed", "item":{
            "id":"exec-1", "type":"dynamicToolCall", "namespace":"other",
            "tool":"send", "arguments":{"to":"probe"}, "status":"completed", "success":true
        }}),
        json!({"type":"item/completed", "item":{
            "id":"exec-2", "type":"dynamicToolCall", "namespace":null,
            "tool":"send", "arguments":{"to":"probe"}, "status":"completed", "success":true
        }}),
        json!({"type":"item/completed", "item":{
            "id":"exec-3", "type":"dynamicToolCall", "namespace":null,
            "tool":"teleport", "arguments":{}, "status":"completed", "success":true
        }}),
    ];
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 60, rows)]]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 3);
    assert!(
        work.iter()
            .all(|entry| matches!(entry.kind, WorkKind::DynamicTool { .. })),
        "{work:?}"
    );
}

/// Every amux tool the daemon exposes reads as one, so a tool added to that
/// list cannot quietly start rendering as a stranger's.
#[test]
fn a2a_codex_inbound_covers_every_registered_amux_tool() {
    let rows: Vec<Value> = amux::agent_tools::definitions()
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            mcp_row(
                "completed",
                &format!("mcp-{index}"),
                amux::agent_tools::MCP_SERVER_NAME,
                definition.name,
                json!({"status": "completed"}),
            )
        })
        .collect();
    let expected = rows.len();
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 70, rows)]]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), expected);
    assert!(
        work.iter()
            .all(|entry| matches!(entry.kind, WorkKind::AmuxTool { .. })),
        "{work:?}"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("a2a_codex_inbound::structural_fixture", fixture_sequence()),
        ("a2a_codex_inbound::amux_mcp_tool", tool_sequence()),
        ("a2a_codex_inbound::typed_amux_send", send_sequence()),
    ]
}
