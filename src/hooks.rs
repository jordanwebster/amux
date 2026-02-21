use crate::config::Config;
use crate::message::{
    ClaudeHook, ClaudePermissionTool, Command, DirectMessage, Hook, Message, PROTOCOL_VERSION,
    ProtocolError,
};
use crate::route::generate_hook_link;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, BufRead};
use tokio::net::UnixStream;

/// Handle Claude Code hook event.
/// Reads JSON from stdin and sends HookEvent to server.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: &Config) {
    if let Err(e) = handle_claude_hook_inner(config) {
        tracing::warn!(error = %e, "hook handling failed");
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

    tracing::debug!(hook = %describe_hook(&claude_hook), "received hook");

    let hook = Hook::Claude(claude_hook);

    if config.socket_path.exists() {
        let socket_path = config.socket_path.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                if let Err(e) = send_hook_event_to_server_inner(&socket_path, hook).await {
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
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "Handshake failed",
            ));
        }
    }

    // Send HandleHook command
    transport
        .write_message(&Message::Command(Command::HandleHook { hook }))
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
            ClaudePermissionTool::Edit { tool_input } => {
                format!("session {} Edit {}", p.session_id, tool_input.file_path)
            }
            ClaudePermissionTool::AskUserQuestion { tool_input } => {
                let count = tool_input.questions.len();
                format!(
                    "session {} AskUserQuestion ({count} question(s))",
                    p.session_id
                )
            }
            ClaudePermissionTool::Bash { tool_input } => {
                format!("session {} Bash `{}`", p.session_id, tool_input.command)
            }
            ClaudePermissionTool::Write { tool_input } => {
                format!("session {} Write {}", p.session_id, tool_input.file_path)
            }
            ClaudePermissionTool::WebFetch { tool_input } => {
                format!("session {} WebFetch {}", p.session_id, tool_input.url)
            }
            ClaudePermissionTool::WebSearch { tool_input } => {
                format!("session {} WebSearch {}", p.session_id, tool_input.query)
            }
            ClaudePermissionTool::NotebookEdit { tool_input } => {
                format!(
                    "session {} NotebookEdit {}",
                    p.session_id, tool_input.notebook_path
                )
            }
            ClaudePermissionTool::Skill { tool_input } => {
                format!("session {} Skill {}", p.session_id, tool_input.skill)
            }
            ClaudePermissionTool::ExitPlanMode { .. } => {
                format!("session {} ExitPlanMode", p.session_id)
            }
            ClaudePermissionTool::Unknown => {
                format!("session {} Unknown tool", p.session_id)
            }
        },
    }
}
