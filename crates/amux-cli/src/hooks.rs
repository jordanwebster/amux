//! Claude Code hook handler (client-side).
//!
//! Invoked as `amux hooks claude <event>` by Claude Code's hook system. Reads hook event JSON
//! from stdin, connects to the local server over Unix socket, sends a
//! HandleHook command, and waits for acknowledgement. Fails silently to avoid
//! blocking Claude Code.

use amux::protocol::{ClaudeHook, ClaudePermissionTool, Command, Hook, Message};
use amux::{AmuxError, Config, ConnectPolicy, Result, connect};
use std::io::{self, BufRead};
use uuid::Uuid;

/// Handle Claude Code hook event.
/// Reads JSON from stdin and sends HookEvent to server.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: &Config) {
    // Not in an amux session — silently ignore
    if std::env::var("AMUX_AGENT_ID").is_err() {
        return;
    }
    if let Err(e) = handle_claude_hook_inner(config) {
        tracing::warn!(error = %e, "hook handling failed");
    }
}

fn handle_claude_hook_inner(config: &Config) -> io::Result<()> {
    let agent_id: Uuid = std::env::var("AMUX_AGENT_ID")
        .expect("AMUX_AGENT_ID checked by caller")
        .parse()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid AMUX_AGENT_ID: {e}"),
            )
        })?;

    let stdin = io::stdin();
    let mut input = String::new();

    for line in stdin.lock().lines() {
        input.push_str(&line?);
    }

    let claude_hook: ClaudeHook = match serde_json::from_str(&input) {
        Ok(hook) => hook,
        Err(e) => {
            tracing::error!(error = %e, "hook parse failed");
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
    };

    if matches!(claude_hook, ClaudeHook::Unknown) {
        tracing::warn!(input = %input, "unrecognized hook event");
        return Ok(());
    }
    if let ClaudeHook::PermissionRequest(ref p) = claude_hook
        && matches!(p.tool, ClaudePermissionTool::Unknown)
    {
        tracing::warn!(input = %input, "unrecognized permission request tool");
        return Ok(());
    }

    tracing::debug!(hook = %claude_hook, "received hook");

    let hook = Hook::Claude(claude_hook);

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

async fn send_hook_event(config: &Config, agent_id: Uuid, hook: Hook) -> Result<()> {
    let conn = connect(config, ConnectPolicy::ExistingOnly).await?;

    conn.send(&Message::Command(Command::HandleHook { agent_id, hook }))
        .await?;

    let ack = conn.recv().await?;
    match ack {
        Message::Command(Command::HandleHookResult { error: None }) => Ok(()),
        Message::Command(Command::HandleHookResult { error: Some(e) }) => {
            Err(AmuxError::ServerError(e.to_string()))
        }
        other => Err(AmuxError::InvalidMessage(format!(
            "expected HandleHookResult, got {}",
            other.type_label()
        ))),
    }
}
