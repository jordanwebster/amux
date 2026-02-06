use crate::config::Config;
use crate::message::{
    ClaudeHook, ClaudePermissionTool, Hook, LocalMessage, Message, ProtocolError,
};
use crate::route::generate_hook_link;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, BufRead};
use tokio::net::UnixStream;

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

/// Handle Claude Code hook event.
/// Reads JSON from stdin and sends HookEvent to server.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: &Config) {
    if let Err(e) = handle_claude_hook_inner(config) {
        log!("hooks: error: {}", e);
    }
}

fn handle_claude_hook_inner(config: &Config) -> io::Result<()> {
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
            log!("hooks: failed to parse: {} - raw input: {}", e, input);
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
    };

    log!("hooks: claude {}", describe_hook(&claude_hook));

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
        .write_message(&Message::Local(LocalMessage::Connect {
            link_name,
            token: None,
        }))
        .await
        .map_err(io::Error::other)?;

    // Wait for ConnectResponse
    let response = transport.read_message().await.map_err(io::Error::other)?;

    match response {
        Message::Local(LocalMessage::ConnectResponse { success: true, .. }) => {}
        Message::Local(LocalMessage::ConnectResponse {
            success: false,
            error: Some(ProtocolError::LinkNameTaken),
        }) => {
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
        .write_message(&Message::Local(LocalMessage::HookEvent { hook }))
        .await
        .map_err(io::Error::other)?;

    // Wait for acknowledgement
    let ack = transport.read_message().await.map_err(io::Error::other)?;

    match ack {
        Message::Local(LocalMessage::HookEventResult { success: true, .. }) => {
            log!("hooks: server acknowledged HookEvent");
            Ok(())
        }
        Message::Local(LocalMessage::HookEventResult {
            success: false,
            error,
        }) => {
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

fn describe_hook(hook: &ClaudeHook) -> String {
    match hook {
        ClaudeHook::SessionStart(s) => {
            format!("session {} at {}", s.session_id, s.transcript_path)
        }
        ClaudeHook::PermissionRequest(p) => match &p.tool {
            ClaudePermissionTool::Edit { file_path, .. } => {
                format!("session {} Edit {}", p.session_id, file_path)
            }
        },
    }
}
