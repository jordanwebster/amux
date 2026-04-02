//! Claude Code hook handler (client-side).
//!
//! Invoked as `amux hooks claude <event>` by Claude Code's hook system. Reads hook event JSON
//! from stdin, connects to the local server over Unix socket, sends a
//! HandleHook command fire-and-forget (no ack wait). Exits immediately with
//! code 0 and no stdout so Claude Code is never blocked.
//!
//! For external sessions (no AMUX_AGENT_ID), uses the hook's session_id as
//! the agent_id so the server can create a readonly session.

use amux::protocol::{ClaudeHook, Command, Hook, Message, PreToolUse};
use amux::{Config, ConnectPolicy, connect, current_parent_pid};
use std::io::{self, BufRead};
use uuid::Uuid;

/// Handle Claude Code hook event.
/// Reads JSON from stdin and sends HookEvent to server.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: &Config) {
    if let Err(e) = handle_claude_hook_inner(config) {
        tracing::warn!(error = %e, "hook handling failed");
    }
}

fn handle_claude_hook_inner(config: &Config) -> io::Result<()> {
    // Read stdin first — we need the hook data for both amux-managed and external sessions
    let stdin = io::stdin();
    let mut input = String::new();
    for line in stdin.lock().lines() {
        input.push_str(&line?);
    }

    let claude_hook: ClaudeHook = match serde_json::from_str(&input) {
        Ok(hook) => hook,
        Err(e) => {
            tracing::error!(error = %e, "hook parse failed");
            return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
        }
    };

    if matches!(claude_hook, ClaudeHook::Unknown) {
        tracing::warn!(input = %input, "unrecognized hook event");
        return Ok(());
    }

    // Determine agent_id: AMUX_AGENT_ID for managed sessions, session_id for external
    let agent_id = match std::env::var("AMUX_AGENT_ID") {
        Ok(id) => id.parse::<Uuid>().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid AMUX_AGENT_ID: {e}"),
            )
        })?,
        Err(_) => {
            // External session — use session_id as agent_id
            let Some(session_id) = claude_hook.session_id() else {
                return Ok(());
            };
            session_id
        }
    };

    if let ClaudeHook::PermissionRequest(ref p) = claude_hook
        && matches!(p.tool, PreToolUse::Unknown)
    {
        tracing::warn!(input = %input, "unrecognized permission request tool");
    }
    tracing::debug!(hook = %claude_hook, "received hook");

    let hook = Hook::Claude(claude_hook);

    // Fire-and-forget: connect, send, don't wait for ack.
    // Hooks must exit quickly so Claude Code is never blocked.
    let config = config.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if let Err(e) = send_hook_event(&config, agent_id, hook).await {
                tracing::debug!(error = %e, "server not running or hook delivery failed");
            }
        });
    });

    Ok(())
}

async fn send_hook_event(config: &Config, agent_id: Uuid, hook: Hook) -> amux::Result<()> {
    let conn = connect(config, ConnectPolicy::ExistingOnly).await?;
    conn.send(&Message::Command(Command::HandleHook {
        agent_id,
        hook: Box::new(hook),
        source_ppid: current_parent_pid(),
    }))
    .await?;
    Ok(())
}
