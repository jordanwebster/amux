//! Specifications for the control protocol: requests the SDK sends alongside a
//! turn, and the acknowledgements Claude Code sends back.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::expect;
use crate::sdk::PermissionMode;

pub(super) static PERMISSION_MODE_AND_MODEL: SpecDef = SpecDef {
    name: "control/permission_mode_and_model",
    fixture: "controls",
    setup: modes_setup,
    run: |session| Box::pin(permission_mode_and_model(session)),
};

fn modes_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly CONTROL_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// Two controls issued while a turn is already in flight are acknowledged
/// independently, and neither disturbs the turn.
async fn permission_mode_and_model(session: &mut SpecSession) {
    // Both controls are answered on the same channel the turn is streaming
    // over, so a request id that correlated loosely would hand this
    // acknowledgement the other control's reply.
    let applied = session
        .set_permission_mode(PermissionMode::AcceptEdits)
        .await
        .expect("the mode control is acknowledged");
    expect!(
        applied == Some(PermissionMode::AcceptEdits),
        "a mode acknowledgement names the mode Claude Code actually applied"
    );

    session
        .set_model(HAIKU)
        .await
        .expect("the model control is acknowledged");

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "controls issued mid-turn leave the turn able to complete"
    );
    expect!(
        turn.text() == "CONTROL_OK",
        "the turn answers the prompt it was given, not the controls: {:?}",
        turn.text()
    );
    expect!(
        turn.saw("system.status"),
        "the mode change also arrives as an unsolicited status frame, which the \
         stream surfaces rather than swallowing"
    );
}

pub(super) static SESSION_INTROSPECTION: SpecDef = SpecDef {
    name: "control/session_introspection",
    fixture: "introspection",
    setup: introspection_setup,
    run: |session| Box::pin(session_introspection(session)),
};

fn introspection_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly INTROSPECT_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A session can be asked what it is, without disturbing the turn it is
/// running.
///
/// These controls exist because a caller building an interface needs to render
/// the session's own state - which model, which commands, how much context is
/// left - rather than assume the options it asked for were the ones granted.
async fn session_introspection(session: &mut SpecSession) {
    let initialization = session.initialization();
    expect!(
        initialization.models.iter().any(|model| model
            .resolved_model
            .as_deref()
            .is_some_and(|resolved| resolved == HAIKU)),
        "initialization reports the models this session can actually switch to, \
         resolved to concrete ids rather than aliases: {:?}",
        initialization
            .models
            .iter()
            .map(|model| model.value.as_str())
            .collect::<Vec<_>>()
    );
    expect!(
        !initialization.commands.is_empty(),
        "initialization reports the slash commands Claude Code will accept"
    );

    let usage = session
        .context_usage()
        .await
        .expect("the context-usage control is answered");
    expect!(
        usage.total_tokens > 0 && usage.total_tokens < usage.max_tokens,
        "context usage accounts for a real, partly-filled window: {} of {}",
        usage.total_tokens,
        usage.max_tokens
    );
    expect!(
        usage
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<u64>()
            >= usage.total_tokens,
        "the reported categories account for the total, rather than the total \
         being an unexplained number"
    );

    // A session with no MCP servers configured reports none, rather than
    // failing or reporting something it inherited.
    let servers = session
        .mcp_server_status()
        .await
        .expect("the MCP status control is answered");
    expect!(
        !servers.iter().any(|server| server.name.trim().is_empty()),
        "every server it does report is named: {:?}",
        servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
    );

    // Nothing is backgrounded and no task exists, which is the case a caller
    // hits constantly and the one most likely to be mis-parsed: Claude Code
    // answers with an object, not the bare boolean the declaration suggests.
    expect!(
        session
            .background_tasks(None)
            .await
            .expect("the background-tasks control is answered"),
        "backgrounding with no target reports that it happened"
    );
    session
        .stop_task("task_that_does_not_exist")
        .await
        .expect("stopping an unknown task is answered rather than refused");

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "introspection controls do not disturb the turn they interleave with"
    );
    expect!(
        turn.text() == "INTROSPECT_OK",
        "the turn answers its prompt: {:?}",
        turn.text()
    );
}

pub(super) static SESSION_MAINTENANCE: SpecDef = SpecDef {
    name: "control/session_maintenance",
    fixture: "session_maintenance",
    setup: maintenance_setup,
    run: |session| Box::pin(session_maintenance(session)),
};

fn maintenance_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly MAINTENANCE_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// The controls that reload a session's own configuration are answered
/// independently while a turn is in flight, and leave the session able to
/// finish it.
///
/// These are deliberately modest claims. Reloading skills from disk cannot be
/// shown to have changed anything without also changing what is on disk, so
/// what is stated here is what a recording can actually settle: each control is
/// answered with its own reply rather than another's, and the turn survives
/// them. Reinitialization is the exception - it re-reports the whole session,
/// so it can be checked against what the session said about itself at startup.
async fn session_maintenance(session: &mut SpecSession) {
    let opening_model = session.initialization().models.len();

    let skills = session
        .reload_skills()
        .await
        .expect("the skills reload control is answered");
    let plugins = session
        .reload_plugins()
        .await
        .expect("the plugins reload control is answered");
    session
        .apply_flag_settings(serde_json::json!({}))
        .await
        .expect("the flag settings control is answered");
    session
        .seed_read_state("maintenance.txt", 1)
        .await
        .expect("the read-state control is answered");

    // A control that acts on a server Claude Code manages refuses one it does
    // not, by name, rather than reporting a success that changed nothing.
    let refused = session
        .toggle_mcp_server("not-a-server", false)
        .await
        .expect_err("toggling a server that does not exist is refused");
    expect!(
        refused.to_string().contains("not-a-server"),
        "and the refusal names what could not be found: {refused}"
    );

    let reinitialized = session
        .reinitialize()
        .await
        .expect("the reinitialize control is answered");
    expect!(
        reinitialized.models.len() == opening_model,
        "reinitializing re-reports the same session rather than a different \
         one: {} models against {opening_model} at startup",
        reinitialized.models.len()
    );
    expect!(
        reinitialized.account.email == session.initialization().account.email,
        "and it is still billed to the same account"
    );
    expect!(
        reinitialized.commands.len() == plugins.commands.len()
            && reinitialized.agents.len() == plugins.agents.len(),
        "reloading plugins re-reports the session's own commands and agents, so \
         it answered the request it was given rather than another control's: \
         {} / {} commands, {} / {} agents",
        reinitialized.commands.len(),
        plugins.commands.len(),
        reinitialized.agents.len(),
        plugins.agents.len()
    );
    expect!(
        skills
            .skills
            .iter()
            .all(|skill| !skill.name.trim().is_empty()),
        "and reloading skills answers with named skills rather than a shape \
         borrowed from the plugin reply: {:?}",
        skills.skills.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    let turn = session.turn().await;
    expect!(
        turn.succeeded(),
        "maintenance controls leave the turn able to complete"
    );
    expect!(
        turn.text() == "MAINTENANCE_OK",
        "the turn answers its prompt: {:?}",
        turn.text()
    );
}

pub(super) static CONNECTED_MCP_SERVERS: SpecDef = SpecDef {
    name: "control/connected_mcp_servers",
    fixture: "connected_mcp_servers",
    setup: connected_mcp_setup,
    run: |session| Box::pin(connected_mcp_servers(session)),
};

/// The model this records against.
///
/// Haiku answers the permission-mode override with a warning that auto mode is
/// unavailable to it, which is a recording of the refusal rather than of the
/// control working.
const SONNET: &str = "claude-sonnet-5";

/// The name the external server is configured under.
const EXTERNAL: &str = "external";

/// The name a second copy is introduced under while the session is running.
const ADDED: &str = "added_later";

fn connected_mcp_setup() -> SessionSetup {
    let mut setup =
        SessionSetup::conversation(SONNET, "Reply with exactly SERVERS_OK and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.mcp_servers = std::collections::HashMap::from([(
        EXTERNAL.to_string(),
        crate::specs::tools::external_server(),
    )]);
    setup.options.persist_session = Some(false);
    setup
}

/// The controls that act on servers Claude Code dialled, against one it did.
///
/// Every one of these is a no-op or a refusal for the in-process server,
/// because that one is served rather than connected to. Against a real
/// connection they have somewhere to act, and what they report is the only way
/// a caller can tell that they did.
async fn connected_mcp_servers(session: &mut SpecSession) {
    let connected = session
        .mcp_server_status()
        .await
        .expect("the MCP status control is answered");
    expect!(
        connected.iter().any(|server| server.name == EXTERNAL),
        "a server Claude Code dialled is reported as connected, unlike the \
         in-process one: {:?}",
        connected
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
    );

    let override_result = session
        .set_mcp_permission_mode_override(EXTERNAL, Some(crate::sdk::McpPermissionMode::Auto))
        .await
        .expect("the permission-mode override control is answered");
    expect!(
        override_result.warning.is_none(),
        "and a model that has auto mode takes the override without complaint, \
         where Haiku answers with a warning instead: {:?}",
        override_result.warning
    );

    session
        .reconnect_mcp_server(EXTERNAL)
        .await
        .expect("a connected server can be reconnected");

    let replaced = session
        .set_mcp_servers(std::collections::HashMap::from([(
            ADDED.to_string(),
            crate::specs::tools::external_server(),
        )]))
        .await
        .expect("the server set can be replaced");
    expect!(
        replaced.added.contains(&ADDED.to_string()) && replaced.errors.is_empty(),
        "a server named in the replacement set is added, and says so, which is \
         how a caller knows the set it asked for is the set it got: added {:?}, \
         removed {:?}, errors {:?}",
        replaced.added,
        replaced.removed,
        replaced.errors
    );

    let after = session
        .mcp_server_status()
        .await
        .expect("status is still answered afterwards");
    expect!(
        after.iter().any(|server| server.name == ADDED),
        "and the session reports it alongside the rest afterwards: {:?}",
        after
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
    );

    let turn = session.turn().await;
    expect!(
        turn.succeeded() && turn.text() == "SERVERS_OK",
        "and none of it disturbs the turn that follows: {:?}",
        turn.text()
    );
}
