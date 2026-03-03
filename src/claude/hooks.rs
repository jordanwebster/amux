//! Claude Code hook handler (client-side).
//!
//! Invoked as `amux hook` by Claude Code's hook system. Reads hook event JSON
//! from stdin, connects to the local server over Unix socket, sends a
//! [`HandleHook`](crate::message::Command::HandleHook) command, and waits for
//! acknowledgement. Fails silently to avoid blocking Claude Code.

use crate::claude::types::{ClaudeHook, ClaudePermissionTool, Hook};
use crate::config::Config;
use crate::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::message::{Command, Message, ProtocolError};
use crate::route::generate_hook_link;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, BufRead};
use tokio::net::UnixStream;
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
    // Caller guarantees AMUX_AGENT_ID is set
    let agent_id: Uuid = std::env::var("AMUX_AGENT_ID")
        .expect("AMUX_AGENT_ID checked by caller")
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

    // Unknown hook or tool variants: log and return early (don't send to server)
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
    let connect = Connect {
        link_name,
        token: None,
        version: PROTOCOL_VERSION,
    };
    let payload = connect.encode().map_err(io::Error::other)?;
    transport
        .write_frame(&payload)
        .await
        .map_err(io::Error::other)?;

    // Wait for ConnectResult
    let payload = transport.read_frame().await.map_err(io::Error::other)?;
    let response = ConnectResult::decode(&payload).map_err(io::Error::other)?;

    match response.error {
        None => {}
        Some(ProtocolError::LinkNameTaken) => {
            // Unlikely but possible - just fail
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Hook link name taken",
            ));
        }
        Some(other) => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("handshake failed: {other}"),
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
