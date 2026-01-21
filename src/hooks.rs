use crate::config::Config;
use crate::message::{ClaudeHook, ClaudePermissionTool, Hook, Message, ProtocolError};
use crate::route::generate_hook_link;
use crate::transport::{Transport, UnixTransport};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use tokio::net::UnixStream;

const HOOKS_LOG_FILE: &str = "claude_hooks.jsonl";

impl From<ClaudePermissionTool> for crate::structured_log::PermissionTool {
    fn from(tool: ClaudePermissionTool) -> Self {
        match tool {
            ClaudePermissionTool::Edit {
                file_path,
                old_string,
                new_string,
                ..
            } => Self::Edit {
                file_path,
                old_string,
                new_string,
            },
        }
    }
}

#[derive(Serialize)]
struct HookLogEntry {
    timestamp: u64,
    provider: String,
    event: String,
    data: ClaudeHook,
}

/// Handle Claude Code hook event.
/// Reads JSON from stdin, sends HookEvent to server, and logs to file.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: &Config, event_name: &str) {
    if let Err(e) = handle_claude_hook_inner(config, event_name) {
        log!("hooks: claude {} error: {}", event_name, e);
    }
}

fn handle_claude_hook_inner(config: &Config, event_name: &str) -> io::Result<()> {
    // Read all input from stdin
    let stdin = io::stdin();
    let mut input = String::new();

    for line in stdin.lock().lines() {
        input.push_str(&line?);
    }

    // Parse into typed struct (unified type, no conversion needed)
    let claude_hook: ClaudeHook =
        serde_json::from_str(&input).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Log to file (for debugging)
    let entry = HookLogEntry {
        timestamp: get_unix_timestamp(),
        provider: "claude".to_string(),
        event: event_name.to_string(),
        data: claude_hook.clone(),
    };
    let _ = append_to_log(&entry);

    // Wrap in Hook::Claude for wire protocol
    let hook = Hook::Claude(claude_hook);

    if config.socket_path.exists() {
        let socket_path = config.socket_path.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                if let Err(e) = send_hook_event_to_server_inner(&socket_path, hook).await {
                    log!("hooks: failed to send to server: {}", e);
                }
            });
        });
    } else {
        log!("hooks: server not running, skipping HookEvent");
    }

    Ok(())
}

async fn send_hook_event_to_server_inner(
    socket_path: &std::path::Path,
    hook: Hook,
) -> io::Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;
    let mut transport = UnixTransport::new(stream);

    // Send Connect handshake with hook link name
    let link_name = generate_hook_link();
    transport
        .write_message(&Message::Connect { link_name })
        .await
        .map_err(io::Error::other)?;

    // Wait for ConnectResponse
    let response = transport.read_message().await.map_err(io::Error::other)?;

    match response {
        Message::ConnectResponse { success: true, .. } => {}
        Message::ConnectResponse {
            success: false,
            error: Some(ProtocolError::LinkNameTaken),
        } => {
            // Unlikely but possible - just fail
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Hook link name taken",
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "Handshake failed",
            ));
        }
    }

    // Send HookEvent
    transport
        .write_message(&Message::HookEvent { hook })
        .await
        .map_err(io::Error::other)?;

    // Wait for acknowledgement
    let ack = transport.read_message().await.map_err(io::Error::other)?;

    match ack {
        Message::HookEventResult { success: true, .. } => {
            log!("hooks: server acknowledged HookEvent");
            Ok(())
        }
        Message::HookEventResult {
            success: false,
            error,
        } => {
            let msg = error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Unknown error".to_string());
            Err(io::Error::other(msg))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unexpected response",
        )),
    }
}

fn get_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn append_to_log(entry: &HookLogEntry) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(HOOKS_LOG_FILE)?;

    let json =
        serde_json::to_string(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    writeln!(file, "{}", json)?;
    file.flush()?;

    Ok(())
}
