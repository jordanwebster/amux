//! Claude Code hook handler (client-side).
//!
//! Invoked through the managed session's absolute amux route by Claude Code's
//! hook system. Reads hook event JSON from stdin and forwards it to either the
//! managed session's hook socket or the daemon. Exits immediately with code 0
//! and no stdout so Claude Code is never blocked.
//!
//! For external sessions (no AMUX_AGENT_ID), uses the hook's session_id as
//! the agent_id so the server can create a readonly session.

use std::collections::HashMap;
use std::io::{self, Read};

use amux::Config;
use anyhow::Result;
use serde_json::Value;

const MESSAGING_ENV_KEYS: &[&str] = &[
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
];

/// Handle Claude Code hook event.
/// Reads JSON from stdin and sends HookEvent to server.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_hook(config: Option<&Config>) {
    if let Err(e) = handle_claude_hook_inner(config) {
        tracing::warn!(error = %e, "hook handling failed");
    }
}

fn handle_claude_hook_inner(config: Option<&Config>) -> io::Result<()> {
    let mut payload = Vec::new();
    io::stdin().lock().read_to_end(&mut payload)?;

    if let Some(socket) = std::env::var_os("CLAUDE_HOOK_SOCKET") {
        let env = messaging_environment();
        return match messaging_credentials(&env) {
            Some(credentials) => claude::hooks::forward_with_messaging(
                &payload,
                std::path::Path::new(&socket),
                &credentials,
            ),
            None => claude::hooks::forward(&payload, std::path::Path::new(&socket)),
        };
    }

    let raw: Value = match serde_json::from_slice(&payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "hook parse failed");
            return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
        }
    };

    let payload = serde_json::to_vec(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    tracing::debug!("received Claude hook");

    // Fire-and-forget: connect, send, don't wait for ack.
    // Hooks must exit quickly so Claude Code is never blocked.
    let config = config
        .ok_or_else(|| io::Error::other("external hook requires a profile config"))?
        .clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if let Err(e) = send_hook_event(&config, payload, messaging_environment()).await {
                tracing::debug!(error = %e, "server not running or hook delivery failed");
            }
        });
    });

    Ok(())
}

fn messaging_environment() -> HashMap<String, String> {
    messaging_environment_with(|key| std::env::var(key).ok())
}

fn messaging_environment_with(
    mut read: impl FnMut(&str) -> Option<String>,
) -> HashMap<String, String> {
    MESSAGING_ENV_KEYS
        .iter()
        .filter_map(|key| read(key).map(|value| ((*key).to_string(), value)))
        .collect()
}

fn messaging_credentials(
    env: &HashMap<String, String>,
) -> Option<claude::hooks::MessagingCredentials> {
    env.get("CLAUDE_CODE_MESSAGING_SOCKET")
        .filter(|value| !value.is_empty())
        .zip(
            env.get("CLAUDE_CODE_MESSAGING_TOKEN")
                .filter(|value| !value.is_empty()),
        )
        .map(|(socket_path, token)| claude::hooks::MessagingCredentials {
            socket_path: socket_path.into(),
            token: token.clone(),
        })
}

async fn send_hook_event(
    config: &Config,
    payload: Vec<u8>,
    env: HashMap<String, String>,
) -> Result<()> {
    let client = crate::client_common::open_daemon(config).await?;
    client.handle_hook(payload.into(), env).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_claude_messaging_environment() {
        let values = HashMap::from([
            (
                "CLAUDE_CODE_MESSAGING_SOCKET",
                "/runtime/claude.sock".to_string(),
            ),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret".to_string()),
            ("CLAUDE_CONFIG_DIR", "/private/config".to_string()),
        ]);

        let forwarded = messaging_environment_with(|key| values.get(key).cloned());

        assert_eq!(forwarded.len(), 2);
        assert_eq!(
            forwarded.get("CLAUDE_CODE_MESSAGING_SOCKET"),
            Some(&"/runtime/claude.sock".to_string())
        );
        assert_eq!(
            forwarded.get("CLAUDE_CODE_MESSAGING_TOKEN"),
            Some(&"secret".to_string())
        );
        assert!(!forwarded.contains_key("CLAUDE_CONFIG_DIR"));
        let credentials = messaging_credentials(&forwarded).unwrap();
        assert_eq!(
            credentials.socket_path,
            std::path::PathBuf::from("/runtime/claude.sock")
        );
        assert_eq!(credentials.token, "secret");
    }

    #[test]
    fn requires_both_messaging_values() {
        let only_socket = HashMap::from([(
            "CLAUDE_CODE_MESSAGING_SOCKET".to_string(),
            "/runtime/claude.sock".to_string(),
        )]);
        assert!(messaging_credentials(&only_socket).is_none());

        let empty_token = HashMap::from([
            (
                "CLAUDE_CODE_MESSAGING_SOCKET".to_string(),
                "/runtime/claude.sock".to_string(),
            ),
            ("CLAUDE_CODE_MESSAGING_TOKEN".to_string(), String::new()),
        ]);
        assert!(messaging_credentials(&empty_token).is_none());
    }
}
