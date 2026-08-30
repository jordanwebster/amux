//! Specifications for the round trips that happen while a turn is running:
//! asking the caller for permission, calling a tool the caller supplied, and
//! running the caller's hooks.
//!
//! What these have in common is that Claude Code stops and waits for this
//! process to answer. Each is a place where a mis-correlated reply would give
//! one request another's answer, and the turn would still look like it worked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::expect;
use crate::sdk::{
    CreateSdkMcpServerOptions, HookEvent, HookSubscription, McpServerConfig, PermissionMode,
    SdkMcpToolOptions, SdkMcpToolResult, create_sdk_mcp_server, tool,
};

pub(super) static PERMISSION_CALLBACK: SpecDef = SpecDef {
    name: "tools/permission_callback",
    fixture: "permission_callback",
    setup: permission_setup,
    run: |session| Box::pin(permission_callback(session)),
};

fn permission_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Use the Write tool once to create tool.txt containing exactly \
         TOOL_OK, then reply exactly DONE.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup
}

/// A tool the model wants to use is offered to this process first, and the
/// turn continues only once this process answers.
async fn permission_callback(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "an approved tool call lets the turn finish"
    );
    expect!(
        turn.tools_used() == ["Write"],
        "the turn used exactly the tool it was asked for: {:?}",
        turn.tools_used()
    );
    expect!(
        !turn.tool_results().is_empty(),
        "the tool's result came back into the conversation, which is what makes \
         the model able to answer about it"
    );
    expect!(
        turn.text() == "DONE",
        "the model answers after the tool, not instead of it: {:?}",
        turn.text()
    );
}

pub(super) static IN_PROCESS_MCP: SpecDef = SpecDef {
    name: "tools/in_process_mcp",
    fixture: "in_process_mcp",
    setup: mcp_setup,
    run: |session| Box::pin(in_process_mcp(session)),
};

fn mcp_setup() -> SessionSetup {
    let echo = tool(
        "echo",
        "Echo the given text back verbatim.",
        serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }),
        |input: serde_json::Value, _call| async move {
            Ok(SdkMcpToolResult::text(format!(
                "echoed:{}",
                input
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
            )))
        },
        SdkMcpToolOptions::default(),
    )
    .expect("the echo tool is well formed");
    let server = create_sdk_mcp_server(CreateSdkMcpServerOptions {
        name: "spec".to_string(),
        version: None,
        instructions: None,
        tools: vec![echo],
        always_load: true,
    })
    .expect("the in-process server is well formed");

    let mut setup = SessionSetup::new(
        HAIKU,
        "Call the echo tool with text set to HELLO, then reply with exactly \
         what it returned.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup.options.mcp_servers = HashMap::from([("spec".to_string(), server)]);
    setup
}

/// A tool implemented in this process is reachable by the model as an ordinary
/// tool, and its result travels back the ordinary way.
///
/// Nothing here runs as a subprocess: the MCP traffic is carried over the same
/// control channel as everything else, which is why this is worth stating
/// separately from an external server.
async fn in_process_mcp(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "a turn that calls an in-process tool completes"
    );
    expect!(
        turn.tools_used() == ["mcp__spec__echo"],
        "the tool is offered under its namespaced MCP name, not its bare one: {:?}",
        turn.tools_used()
    );
    expect!(
        turn.tool_results()
            .iter()
            .any(|result| result.contains("echoed:HELLO")),
        "the result this process computed is what came back: {:?}",
        turn.tool_results()
    );
    expect!(
        turn.text().contains("echoed:HELLO"),
        "and the model answered from it: {:?}",
        turn.text()
    );
}

/// The hook log is shared between the session's callbacks and the claims made
/// about them, so it has to outlive the setup that installs it.
static HOOK_LOG: Mutex<Option<Arc<Mutex<Vec<String>>>>> = Mutex::new(None);

pub(super) static HOOK_LIFECYCLE: SpecDef = SpecDef {
    name: "tools/hook_lifecycle",
    fixture: "hook_lifecycle",
    setup: hooks_setup,
    run: |session| Box::pin(hook_lifecycle(session)),
};

fn hooks_setup() -> SessionSetup {
    let log = Arc::new(Mutex::new(Vec::new()));
    *HOOK_LOG.lock().expect("the hook log slot is not poisoned") = Some(log.clone());

    let mut setup = SessionSetup::new(
        HAIKU,
        "Use the Write tool once to create hook.txt containing exactly HOOK_OK, \
         then reply exactly DONE.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    for event in [
        HookEvent::UserPromptSubmit,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::Stop,
    ] {
        setup.options.hook_subscriptions.push(HookSubscription {
            event,
            matcher: None,
        });
    }
    setup.answer_hooks(Some(log));
    setup
}

/// Hooks registered by this process are called at the points in the turn they
/// name, in the order the turn reaches them.
///
/// The ordering claim is the load-bearing one. A hook that fired at the wrong
/// point - `PostToolUse` before the tool ran, say - would still look like a
/// working hook to anything that only counted calls.
async fn hook_lifecycle(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "hooks that allow the turn do not stop it finishing"
    );

    let fired = HOOK_LOG
        .lock()
        .expect("the hook log slot is not poisoned")
        .as_ref()
        .expect("the setup installed a hook log")
        .lock()
        .expect("the hook log is not poisoned")
        .clone();
    expect!(
        fired == ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"],
        "each hook fires once, at the point in the turn it names: {fired:?}"
    );
    expect!(
        turn.tools_used() == ["Write"],
        "the hooks surrounded a real tool call: {:?}",
        turn.tools_used()
    );
}

/// The word the model is told to confirm through the external server. It
/// travels caller -> model -> server -> elicitation -> caller -> tool result,
/// so seeing it come back is evidence the whole path ran.
const CONFIRMED: &str = "PELICAN";

/// The external server this specification talks to.
///
/// Named rather than pathed. A server configured through `set_mcp_servers`
/// travels inside a control request, and a recording redacts machine paths -
/// so a specification that named an absolute path would write one thing while
/// its recording held another, and could never replay. The capture puts the
/// built binary on `PATH` instead, which keeps the traffic the same wherever
/// it was recorded.
pub(super) fn external_server() -> McpServerConfig {
    McpServerConfig::Stdio(crate::sdk::McpStdioServerConfig {
        command: "spec-mcp-server".to_owned(),
        args: Vec::new(),
        env: HashMap::new(),
        timeout: None,
        always_load: None,
    })
}

pub(super) static ELICITED: SpecDef = SpecDef {
    name: "tools/elicited",
    fixture: "elicited",
    setup: elicited_setup,
    run: |session| Box::pin(elicited(session)),
};

fn elicited_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        format!(
            "Call the {TOOL} tool with word set to {CONFIRMED}, then reply with \
             exactly what it returned."
        ),
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup.accept_elicitation(serde_json::json!({"confirmed": CONFIRMED}));
    setup.options.mcp_servers = HashMap::from([("external".to_string(), external_server())]);
    setup
}

/// The tool the external server offers.
const TOOL: &str = "ask_the_operator";

/// A server can ask this process a question, and the answer reaches the tool.
///
/// This is the one direction of the protocol the in-process server cannot
/// show. Elicitation begins with the server, travels through Claude Code to
/// the caller, and its answer has to arrive back inside the tool's own result
/// so a specification that only checked the callback ran would not have
/// shown that the answer went anywhere.
async fn elicited(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "the turn finishes once the elicitation is answered"
    );
    expect!(
        turn.text().contains(CONFIRMED),
        "and the model reports what the tool returned, which is the answer this \
         process gave the server: {:?}",
        turn.text()
    );
}
