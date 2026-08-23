//! Chapter 22 — Codex inbound: agent messages and amux's own tools.
//!
//! Codex's carrier injects a message into a thread, and the native thread
//! then shows nothing for it — so the daemon writes the one synthesized row
//! that says a message arrived, and this layer reads it. The outbound half
//! is a dynamic tool call, which Codex does report; amux's own tools are
//! separated from anyone else's so the fleet's work on itself reads in the
//! fleet's words.
//!
//! Folded over the graduated Codex 0.148.0 captures.

use amux_ui::codex::{AgentMessageEntry, CodexLayer, FeedEntryKind, WorkEntry, WorkKind};
use amux_ui::{AgentMessageKind, Msg};
use serde_json::json;

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

/// The graduated live capture of Codex calling an amux tool through the
/// dynamic-tool carrier.
fn tool_capture_sequence() -> Vec<Msg> {
    seq([
        codex_base(AGENT),
        vec![batch(AGENT, 30, codex_capture_rows("a2a_tools"))],
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

/// The live capture: Codex called amux's `send` through the dynamic-tool
/// carrier, and the layer reads it as amux's tool rather than an anonymous
/// dynamic call.
#[test]
fn a2a_codex_inbound_folds_an_amux_tool_call() {
    let model = fold(tool_capture_sequence());
    let work = work(codex_layer(&model, AGENT));
    let amux: Vec<&WorkEntry> = work
        .iter()
        .filter(|entry| matches!(entry.kind, WorkKind::AmuxTool { .. }))
        .collect();
    assert_eq!(amux.len(), 1, "the capture calls send once: {work:?}");
    let WorkKind::AmuxTool {
        tool,
        arguments,
        success,
    } = &amux[0].kind
    else {
        unreachable!("filtered above");
    };
    assert_eq!(tool, "send");
    assert_eq!(arguments["to"], json!("probe"));
    assert_eq!(*success, Some(true));
    assert!(model.check_invariants().is_empty());
}

/// A namespaced tool, or one amux never registered, stays somebody else's:
/// the discriminator is the registrar's own list, not a name that looks
/// familiar.
#[test]
fn a2a_codex_inbound_leaves_foreign_dynamic_tools_alone() {
    let rows = vec![
        json!({"type":"item/completed", "item":{
            "id":"exec-1", "type":"dynamicToolCall", "namespace":"other",
            "tool":"send", "arguments":{"to":"probe"}, "status":"completed", "success":true
        }}),
        json!({"type":"item/completed", "item":{
            "id":"exec-2", "type":"dynamicToolCall", "namespace":null,
            "tool":"teleport", "arguments":{}, "status":"completed", "success":true
        }}),
    ];
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 50, rows)]]));
    let work = work(codex_layer(&model, AGENT));
    assert_eq!(work.len(), 2);
    assert!(
        work.iter()
            .all(|entry| matches!(entry.kind, WorkKind::DynamicTool { .. })),
        "{work:?}"
    );
}

/// Every amux tool the daemon registers reads as one, so a tool added to
/// that list cannot quietly start rendering as a stranger's.
#[test]
fn a2a_codex_inbound_covers_every_registered_amux_tool() {
    let rows: Vec<serde_json::Value> = amux::agent_tools::definitions()
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            json!({"type":"item/completed", "item":{
                "id": format!("exec-{index}"), "type":"dynamicToolCall",
                "namespace": null, "tool": definition.name, "arguments": {},
                "status":"completed", "success": true
            }})
        })
        .collect();
    let expected = rows.len();
    let model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 60, rows)]]));
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
        ("a2a_codex_inbound::tool_capture", tool_capture_sequence()),
    ]
}
