//! Claude Code hook handler (client-side).
//!
//! Invoked as `amux hooks claude <event>` by Claude Code's hook system. Reads hook event JSON
//! from stdin, connects to the local server over Unix socket, sends a
//! HandleHook command fire-and-forget (no ack wait). Exits immediately with
//! code 0 and no stdout so Claude Code is never blocked.
//!
//! For external sessions (no AMUX_AGENT_ID), uses the hook's session_id as
//! the agent_id so the server can create a readonly session.

use amux::protocol::{Command, HookProvider, Message};
use amux::{Config, ConnectPolicy, connect};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::io::{self, BufRead};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(tag = "hook_event_name")]
enum ExternalClaudeHook {
    SessionStart(ExternalHookCommon),
    PermissionRequest(ExternalHookCommon),
    Stop(ExternalHookCommon),
    SessionEnd(ExternalHookCommon),
    Notification(ExternalHookCommon),
    #[serde(other)]
    Unknown,
}

impl ExternalClaudeHook {
    fn session_id(self) -> Option<Uuid> {
        match self {
            Self::SessionStart(common)
            | Self::PermissionRequest(common)
            | Self::Stop(common)
            | Self::SessionEnd(common)
            | Self::Notification(common) => Some(common.session_id),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ExternalHookCommon {
    session_id: Uuid,
}

fn parse_external_agent_id(payload: &[u8]) -> std::result::Result<Option<Uuid>, serde_json::Error> {
    let hook: ExternalClaudeHook = serde_json::from_slice(payload)?;
    Ok(hook.session_id())
}

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

    let raw: Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "hook parse failed");
            return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
        }
    };

    let payload = serde_json::to_vec(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    // Determine agent_id: AMUX_AGENT_ID for managed sessions, session_id for external
    let (agent_id, external) = match std::env::var("AMUX_AGENT_ID") {
        Ok(id) => id
            .parse::<Uuid>()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid AMUX_AGENT_ID: {e}"),
                )
            })
            .map(|id| (id, false))?,
        Err(_) => {
            // External session — use the parsed session_id as agent_id.
            let session_id = match parse_external_agent_id(&payload) {
                Ok(Some(session_id)) => session_id,
                Ok(None) => return Ok(()),
                Err(error) => {
                    tracing::error!(error = %error, "external hook parse failed");
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ));
                }
            };
            (session_id, true)
        }
    };

    tracing::debug!(%agent_id, external, "received hook");

    // Fire-and-forget: connect, send, don't wait for ack.
    // Hooks must exit quickly so Claude Code is never blocked.
    let config = config.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if let Err(e) = send_hook_event(&config, agent_id, payload, external).await {
                tracing::debug!(error = %e, "server not running or hook delivery failed");
            }
        });
    });

    Ok(())
}

async fn send_hook_event(
    config: &Config,
    agent_id: Uuid,
    payload: Vec<u8>,
    external: bool,
) -> Result<()> {
    let conn = connect(config, ConnectPolicy::ExistingOnly).await?;
    conn.send(&Message::Command {
        command: Command::HandleHook {
            agent_id,
            provider: HookProvider::Claude,
            payload,
            external,
        },
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_external_agent_id;
    use uuid::Uuid;

    #[test]
    fn parses_known_external_hook_session_id() {
        let session_id = Uuid::new_v4();
        let payload = format!(
            r#"{{"hook_event_name":"SessionStart","session_id":"{session_id}","cwd":"/tmp","transcript_path":"/tmp/transcript.jsonl"}}"#
        );

        let parsed = parse_external_agent_id(payload.as_bytes()).unwrap();

        assert_eq!(parsed, Some(session_id));
    }

    #[test]
    fn ignores_unknown_external_hook() {
        let payload = br#"{"hook_event_name":"FutureHook","session_id":"00000000-0000-0000-0000-000000000001"}"#;

        let parsed = parse_external_agent_id(payload).unwrap();

        assert_eq!(parsed, None);
    }

    #[test]
    fn rejects_malformed_external_hook_payload() {
        let payload = br#"{"hook_event_name":"SessionStart","session_id":"not-a-uuid"}"#;

        assert!(parse_external_agent_id(payload).is_err());
    }
}
