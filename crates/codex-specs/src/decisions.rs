//! Specifications for the decisions Codex puts to the client beyond a plain
//! command approval: forms and links from a tool server, access grants, the
//! automatic reviewer, and approval scopes that outlive one request.
//!
//! Each scenario answers every server request the turn raises from what the
//! request itself says, so replay sends the same answers the capture sent.

use std::path::Path;

use codex::{
    ApprovalPolicy, ApprovalsReviewer, Codex, GranularApprovalPolicy, SandboxMode, Thread,
    ThreadEvent, ThreadEventStream, TurnEvent,
};
use serde_json::{Value, json};

use super::{ScenarioReport, next_event, report, stringify, thread_config};

/// The tool server capture registers in the Codex home for the tool-server
/// specification, and the one tool it offers.
pub const SPEC_TOOL_SERVER: &str = "spec";
pub const SPEC_TOOL: &str = "ask_the_operator";

/// Reads one turn to completion, answering each server request with what
/// `respond` returns for it.
pub(super) async fn drive_turn(
    thread: &Thread,
    events: &mut ThreadEventStream,
    respond: &mut dyn FnMut(&ThreadEvent) -> Option<Value>,
) -> Result<Vec<ThreadEvent>, String> {
    let mut seen = Vec::new();
    loop {
        let event = next_event(events, "turn completion").await?;
        let request = match &event.event {
            TurnEvent::ServerRequest { id, .. } => Some(id.clone()),
            TurnEvent::ApprovalRequired(request) => Some(request.request_id()),
            TurnEvent::ToolCallRequired(request) => Some(request.request_id.clone()),
            _ => None,
        };
        if let Some(id) = request {
            let answer = respond(&event)
                .ok_or_else(|| format!("no answer for the {} request", event.method))?;
            thread.respond_raw(id, answer).await.map_err(stringify)?;
        }
        let done = matches!(event.event, TurnEvent::TurnCompleted { .. });
        seen.push(event);
        if done {
            return Ok(seen);
        }
    }
}

fn methods(seen: &[ThreadEvent]) -> Vec<&str> {
    seen.iter().map(|event| event.method.as_str()).collect()
}

async fn open(
    codex: &Codex,
    config: codex::ThreadConfig,
) -> Result<(Thread, ThreadEventStream), String> {
    let thread = codex.start_thread(config).await.map_err(stringify)?;
    let events = thread.events().await.map_err(stringify)?;
    Ok((thread, events))
}

fn overrides(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// A tool call to an MCP server first asks whether the tool may run, then the
/// server's own elicitation arrives as a form (fields to fill in) or as a link
/// (a page to visit). The form is accepted with its field filled in; the link
/// is cancelled.
pub(super) async fn tool_server_form(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = open(codex, thread_config(model, project)).await?;
    thread
        .start_turn(format!(
            "Call the {SPEC_TOOL} tool from the {SPEC_TOOL_SERVER} tool server twice, one call \
             at a time: first with word BLUE, then with word LINK. Then reply with what each \
             call returned."
        ))
        .await
        .map_err(stringify)?;
    let mut forms = 0;
    let mut links = 0;
    let seen = drive_turn(&thread, &mut events, &mut |event| {
        if event.method == "mcpServer/elicitation/request" {
            let params = &event.params;
            if params["mode"] == "url" {
                links += 1;
                return Some(json!({ "action": "cancel", "content": null }));
            }
            if params
                .pointer("/requestedSchema/properties/confirmed")
                .is_some()
            {
                forms += 1;
                return Some(json!({ "action": "accept", "content": { "confirmed": "BLUE" } }));
            }
            // Codex's own question whether the tool may run at all.
            return Some(json!({ "action": "accept", "content": {} }));
        }
        Some(json!({ "decision": "accept" }))
    })
    .await?;
    let called = seen.iter().any(|event| {
        event.method == "item/completed" && event.params["item"]["type"] == "mcpToolCall"
    });
    if forms == 0 {
        return Err(format!(
            "the tool server's form never arrived: {:?}",
            methods(&seen)
        ));
    }
    if !called {
        return Err("no MCP tool call completed".to_string());
    }
    Ok(report(&thread))
}

/// With the request-permissions tool on, Codex asks the client to widen the
/// sandbox for the turn: here, network access. The grant answers with exactly
/// what was asked, for the turn.
pub(super) async fn access_grant(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.approval_policy = Some(ApprovalPolicy::Granular(GranularApprovalPolicy {
        sandbox_approval: true,
        rules: true,
        skill_approval: false,
        request_permissions: true,
        mcp_elicitations: true,
    }));
    config.config = Some(overrides(&[(
        "features.request_permissions_tool",
        json!(true),
    )]));
    let (thread, mut events) = open(codex, config).await?;
    thread
        .start_turn(
            "Use your request_permissions tool to ask me for network access. Once granted, run \
             `curl -sI https://example.com` and reply with its status line.",
        )
        .await
        .map_err(stringify)?;
    let mut grants = 0;
    let seen = drive_turn(&thread, &mut events, &mut |event| {
        if event.method == "item/permissions/requestApproval" {
            grants += 1;
            return Some(json!({
                "permissions": event.params["permissions"].clone(),
                "scope": "turn",
            }));
        }
        Some(json!({ "decision": "accept" }))
    })
    .await?;
    if grants == 0 {
        return Err(format!(
            "Codex never asked for a permission grant: {:?}",
            methods(&seen)
        ));
    }
    Ok(report(&thread))
}

/// With the automatic reviewer chosen, a command that needs approval goes to
/// a reviewing subagent, whose verdict arrives as review notifications
/// instead of a request to the client.
pub(super) async fn automatic_review(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.sandbox = Some(SandboxMode::ReadOnly);
    config.approvals_reviewer = Some(ApprovalsReviewer::AutoReview);
    let (thread, mut events) = open(codex, config).await?;
    thread
        .start_turn(
            "Run this exact shell command and no substitute: touch reviewed.txt. Then say DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = drive_turn(&thread, &mut events, &mut |_| {
        Some(json!({ "decision": "accept" }))
    })
    .await?;
    if !seen
        .iter()
        .any(|event| event.method == "item/autoApprovalReview/completed")
    {
        return Err(format!(
            "no automatic review completed: {:?}",
            super::rows::items_of(&seen, &["commandExecution", "agentMessage"])
        ));
    }
    Ok(report(&thread))
}

/// Approvals that last beyond the request: a command approved together with
/// the rule Codex proposes for similar commands, and a file change approved
/// for the rest of the session.
pub(super) async fn approval_scopes(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.sandbox = Some(SandboxMode::ReadOnly);
    let (thread, mut events) = open(codex, config).await?;
    thread
        .start_turn(
            "Do these steps in order, one tool call each: run the shell command `touch \
             first.txt`; then use apply_patch to create notes.txt containing the line one; then \
             use apply_patch to change that line to two. Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let mut amended = 0;
    let mut for_session = 0;
    let seen = drive_turn(&thread, &mut events, &mut |event| match event.method.as_str() {
        "item/commandExecution/requestApproval" => {
            let amendment = &event.params["proposedExecpolicyAmendment"];
            if amendment.is_array() {
                amended += 1;
                Some(json!({ "decision": {
                    "acceptWithExecpolicyAmendment": { "execpolicy_amendment": amendment.clone() }
                }}))
            } else {
                Some(json!({ "decision": "accept" }))
            }
        }
        "item/fileChange/requestApproval" => {
            for_session += 1;
            Some(json!({ "decision": "acceptForSession" }))
        }
        _ => Some(json!({ "decision": "accept" })),
    })
    .await?;
    if amended == 0 || for_session == 0 {
        return Err(format!(
            "expected a command rule and a session-wide file approval, got {amended} and \
             {for_session}: {:?}",
            methods(&seen)
        ));
    }
    Ok(report(&thread))
}
