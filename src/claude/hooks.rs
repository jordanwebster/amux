//! Claude Code hook handler (client-side).
//!
//! Invoked as `amux hook` by Claude Code's hook system. Reads hook event JSON
//! from stdin, connects to the local server over Unix socket, sends a
//! [`HandleHook`](crate::message::Command::HandleHook) command, and waits for
//! acknowledgement. Fails silently to avoid blocking Claude Code.

use crate::claude::types::{ClaudeHook, ClaudePermissionTool, Hook};
use crate::config::Config;
use crate::message::{Command, DirectMessage, Message, PROTOCOL_VERSION, ProtocolError};
use crate::route::generate_hook_link;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, BufRead};
use tokio::net::UnixStream;
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
    // Read agent_id from environment (set by amux when spawning Claude)
    let agent_id: Uuid = std::env::var("AMUX_AGENT_ID")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "AMUX_AGENT_ID not set"))?
        .parse()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid AMUX_AGENT_ID: {e}"),
            )
        })?;

    // Read all input from stdin
    let stdin = io::stdin();
    let mut input = String::new();

    for line in stdin.lock().lines() {
        input.push_str(&line?);
    }

    // Parse into typed struct
    let claude_hook: ClaudeHook = match serde_json::from_str(&input) {
        Ok(hook) => hook,
        Err(e) => {
            tracing::error!(error = %e, "hook parse failed");
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
    };

    // Unknown tools: log and return early (don't send to server)
    if let ClaudeHook::PermissionRequest(ref p) = claude_hook
        && matches!(p.tool, ClaudePermissionTool::Unknown)
    {
        tracing::warn!(input = %input, "unrecognized permission request tool");
        return Ok(());
    }

    tracing::debug!(hook = %claude_hook, "received hook");

    let hook = Hook::Claude(claude_hook);

    if config.socket_path.exists() {
        let socket_path = config.socket_path.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                if let Err(e) = send_hook_event_to_server_inner(&socket_path, agent_id, hook).await
                {
                    tracing::warn!(error = %e, "failed to send hook to server");
                }
            });
        });
    } else {
        tracing::debug!("server not running, skipping hook");
    }

    Ok(())
}

async fn send_hook_event_to_server_inner(
    socket_path: &std::path::Path,
    agent_id: Uuid,
    hook: Hook,
) -> io::Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;
    let mut transport = UnixTransport::new(stream);

    // Send Connect handshake with hook link name
    let link_name = generate_hook_link();
    transport
        .write_message(&Message::Direct(DirectMessage::Connect {
            link_name,
            token: None,
            version: PROTOCOL_VERSION,
        }))
        .await
        .map_err(io::Error::other)?;

    // Wait for ConnectResult
    let response = transport.read_message().await.map_err(io::Error::other)?;

    match response {
        Message::Direct(DirectMessage::ConnectResult { error: None }) => {}
        Message::Direct(DirectMessage::ConnectResult {
            error: Some(ProtocolError::LinkNameTaken),
        }) => {
            // Unlikely but possible - just fail
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Hook link name taken",
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!(
                    "handshake failed: expected ConnectResult(Ok), got {}",
                    other.type_label()
                ),
            ));
        }
    }

    // Send HandleHook command
    transport
        .write_message(&Message::Command(Command::HandleHook { agent_id, hook }))
        .await
        .map_err(io::Error::other)?;

    // Wait for acknowledgement
    let ack = transport.read_message().await.map_err(io::Error::other)?;

    match ack {
        Message::Command(Command::HandleHookResult { error: None }) => {
            tracing::debug!("hook acknowledged");
            Ok(())
        }
        Message::Command(Command::HandleHookResult { error: Some(e) }) => {
            Err(io::Error::other(e.to_string()))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected HandleHookResult, got {}", other.type_label()),
        )),
    }
}
