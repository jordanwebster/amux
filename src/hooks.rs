use crate::config::Config;
use crate::message;
use crate::message::Message;
use crate::transport::{Transport, UnixTransport};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use tokio::net::UnixStream;
use uuid::Uuid;

const HOOKS_LOG_FILE: &str = "claude_hooks.jsonl";

/// Claude Code hook event data (for parsing JSON from stdin)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
    PermissionRequest(ClaudePermissionRequest),
}

/// SessionStart hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeSessionStart {
    pub cwd: String,
    pub session_id: String,
    pub source: String,
    pub transcript_path: String,
}

/// Tool data from Claude Code permission requests.
/// Uses adjacently-tagged format: tool_name determines variant, tool_input is content.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "tool_name", content = "tool_input")]
pub enum ClaudePermissionTool {
    Edit {
        file_path: String,
        old_string: String,
        new_string: String,
        #[serde(default)]
        replace_all: bool,
    },
}

/// PermissionRequest hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudePermissionRequest {
    pub session_id: String,
    #[serde(flatten)]
    pub tool: ClaudePermissionTool,
}

/// Convert from JSON input types to wire protocol types.
///
/// We maintain separate enums because:
/// - `hooks::ClaudeHook` uses `#[serde(tag = "hook_event_name")]` for parsing
///   Claude Code's JSON input format
/// - `message::ClaudeHook` is untagged for bincode compatibility (bincode doesn't
///   support internally tagged enums - fails with `DeserializeAnyNotSupported`)
impl TryFrom<ClaudeHook> for message::Hook {
    type Error = HookConversionError;

    fn try_from(hook: ClaudeHook) -> Result<Self, Self::Error> {
        match hook {
            ClaudeHook::SessionStart(session) => {
                let session_id = Uuid::parse_str(&session.session_id)?;
                Ok(message::Hook::Claude(message::ClaudeHook::SessionStart {
                    session_id,
                    transcript_path: session.transcript_path,
                }))
            }
            ClaudeHook::PermissionRequest(req) => {
                let session_id = Uuid::parse_str(&req.session_id)?;
                let tool = req.tool.into();
                Ok(message::Hook::Claude(
                    message::ClaudeHook::PermissionRequest { session_id, tool },
                ))
            }
        }
    }
}

/// Error type for hook conversion
#[derive(Debug)]
pub enum HookConversionError {
    InvalidUuid(uuid::Error),
}

impl std::fmt::Display for HookConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUuid(e) => write!(f, "invalid UUID: {}", e),
        }
    }
}

impl std::error::Error for HookConversionError {}

impl From<uuid::Error> for HookConversionError {
    fn from(e: uuid::Error) -> Self {
        Self::InvalidUuid(e)
    }
}

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

    // Parse into typed struct
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

    // Convert to wire protocol type (session_id must be valid UUID)
    let hook: message::Hook = claude_hook.try_into().map_err(|e: HookConversionError| {
        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
    })?;

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
    hook: message::Hook,
) -> io::Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;
    let mut transport = UnixTransport::new(stream);

    // Send Connect handshake
    let client_id = format!("hook-{}", Uuid::new_v4());
    transport
        .write_message(&Message::Connect {
            host_id: client_id.clone(),
        })
        .await
        .map_err(io::Error::other)?;

    // Wait for ConnectResponse
    let response = transport.read_message().await.map_err(io::Error::other)?;

    match response {
        Message::ConnectResponse { success: true, .. } => {}
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
        } => Err(io::Error::other(
            error.unwrap_or_else(|| "Unknown error".to_string()),
        )),
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
