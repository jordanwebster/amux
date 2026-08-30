//! Specifications for work the model delegates rather than does itself.

use std::collections::HashMap;

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::expect;
use crate::sdk::{AgentDefinition, PermissionMode};

pub(super) static SUBAGENT_TASK: SpecDef = SpecDef {
    name: "agents/subagent_task",
    fixture: "subagent_task",
    setup: subagent_setup,
    run: |session| Box::pin(subagent_task(session)),
};

fn subagent_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Delegate to the `counter` subagent, then reply with exactly what it said.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup.options.agents = HashMap::from([(
        "counter".to_string(),
        AgentDefinition {
            description: "Counts to three and reports the result.".to_string(),
            prompt: "Reply with exactly ONE TWO THREE and nothing else.".to_string(),
            // No tools: the subagent's only job is to answer, which keeps the
            // recording about delegation rather than about what it delegated.
            tools: Some(Vec::new()),
            disallowed_tools: None,
            model: Some(HAIKU.to_string()),
            mcp_servers: None,
            skills: None,
            initial_prompt: None,
            max_turns: None,
            // Claude Code will otherwise decide per turn whether to run the
            // subagent in the background, and a backgrounded one answers with
            // a launch receipt instead of its result. Pinning it makes this a
            // specification about delegation rather than about that coin flip.
            background: Some(false),
            memory: None,
            effort: None,
            permission_mode: None,
            observer: None,
            observer_message: None,
        },
    )]);
    setup
}

/// A subagent is a task the session owns, announced under the tool call that
/// spawned it and reported on until it finishes - which may be after the turn
/// that asked for it has already ended.
///
/// Two claims here are load-bearing. The task announcement carries the id of
/// the tool call that caused it, which is the only way a caller can attribute
/// delegated work to the request behind it. And the completion news arrives on
/// the same stream under the same task id, so a caller that stopped reading at
/// the result would simply never learn the answer.
///
/// Nothing here claims *when* the answer arrives. Claude Code decides per turn
/// whether to run delegated work in the background, and setting
/// `background: Some(false)` does not reliably override it. Waiting for the
/// news rather than for the turn is what makes this specification true either
/// way.
async fn subagent_task(session: &mut SpecSession) {
    session.advance_to("system.task_notification").await;
    let session_so_far = session.drain().await;

    expect!(
        session_so_far.succeeded(),
        "the turn that delegated still completes as one turn"
    );
    expect!(
        session_so_far.tools_used().contains(&"Agent"),
        "delegation happens through a tool call like any other: {:?}",
        session_so_far.tools_used()
    );

    let started = session_so_far.tasks_started();
    expect!(
        started.len() == 1,
        "one delegation announces one task: {} announced",
        started.len()
    );
    let task = started[0];
    expect!(
        task.subagent_type.as_deref() == Some("counter"),
        "the announcement names which of the caller's agent definitions ran: {:?}",
        task.subagent_type
    );
    expect!(
        session_so_far
            .tool_uses()
            .iter()
            .any(|(id, name)| *name == "Agent" && Some(*id) == task.tool_use_id.as_deref()),
        "the task carries the id of the tool call that spawned it, which is the \
         only way a caller can attribute delegated work to the request that \
         caused it: task {:?} against calls {:?}",
        task.tool_use_id,
        session_so_far.tool_uses()
    );

    let news = session_so_far.task_notifications();
    expect!(
        news.len() == 1,
        "the one task reports back once: {} notifications",
        news.len()
    );
    let notification = news[0];
    expect!(
        notification.task_id == task.task_id,
        "the completion news is correlated to the task that was announced: {} \
         against {}",
        notification.task_id,
        task.task_id
    );
    expect!(
        notification.status == "completed" && notification.summary.contains("ONE TWO THREE"),
        "and it carries what the subagent was asked to produce: {} / {:?}",
        notification.status,
        notification.summary
    );
}
