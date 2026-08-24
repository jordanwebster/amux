use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

use super::core::{ClaudeMessagingCredentials, ClaudeSession};
use crate::agents::claude::hooks::{ClaudeHookKind, ParsedClaudeHook};
use crate::agents::{HookEnvironment, HookError, HookOutcome};

const MESSAGING_SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const MESSAGING_TOKEN_ENV: &str = "CLAUDE_CODE_MESSAGING_TOKEN";

/// Hook delivery is at-least-once by construction: user and project settings
/// may repeat the managed registration, and Claude Code runs every
/// registration for every event. Each registration delivers the identical
/// stdin JSON to the same daemon.
///
/// The daemon is the seam where deliveries become stream facts, so it emits
/// each fact once: a payload byte-identical to the previously emitted one
/// within this window is the same event re-delivered, not a new event.
/// Distinct events are never identical this close together (payloads carry
/// `prompt_id`, `tool_input`, message text), and the window keeps a
/// genuinely identical future event (the same notification re-fired minutes
/// later) honest.
const HOOK_DEDUPE_WINDOW: Duration = Duration::from_secs(2);

/// FNV-1a over the payload's canonical JSON (serde_json objects iterate
/// key-sorted), so byte-order differences in delivery cannot defeat the
/// duplicate check.
fn hook_fingerprint(raw: &Value) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in raw.to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl ClaudeSession {
    /// Link a transcript file for structured output tailing.
    async fn link_transcript(&self, path: PathBuf) {
        if let Some(ingest) = &self.transcript_ingest {
            ingest.link_transcript(path).await;
        }
    }

    async fn sync_hook_metadata(&mut self, hook: &ParsedClaudeHook) {
        if let Some(session_id) = hook.session_id() {
            self.session_id = Some(session_id);
        }

        if let Some(transcript_path) = hook.transcript_path() {
            self.link_transcript(PathBuf::from(transcript_path)).await;
        }
    }

    fn sync_messaging_credentials(&mut self, env: &HookEnvironment) {
        let Some((socket_path, token)) = env
            .get(MESSAGING_SOCKET_ENV)
            .filter(|value| !value.is_empty())
            .zip(
                env.get(MESSAGING_TOKEN_ENV)
                    .filter(|value| !value.is_empty()),
            )
        else {
            return;
        };

        self.messaging_credentials = Some(ClaudeMessagingCredentials {
            socket_path: PathBuf::from(socket_path),
            token: token.clone(),
        });
    }

    pub(crate) async fn handle_hook_payload(
        &mut self,
        payload: &[u8],
        env: &HookEnvironment,
    ) -> std::result::Result<HookOutcome, HookError> {
        let hook =
            ParsedClaudeHook::parse_payload(payload).map_err(|e| HookError::InvalidPayload {
                message: e.to_string(),
            })?;
        self.sync_messaging_credentials(env);
        let is_unknown = hook.is_unknown();
        let is_session_end = hook.is_session_end();
        if hook.kind == ClaudeHookKind::SessionStart && !self.readonly {
            self.delivery_ready.store(true, Ordering::Release);
        }
        let completion = hook.last_assistant_message().map(str::to_string);
        if is_unknown {
            tracing::warn!(agent_id = %self.agent_id, "received unknown Claude hook variant");
        }
        let emitted = self.handle_hook(hook).await;
        Ok(match (is_unknown, self.readonly && is_session_end) {
            (true, _) => HookOutcome::Noop,
            (false, true) => HookOutcome::WithdrawSession,
            (false, false) if emitted && completion.is_some() => HookOutcome::Completed {
                text: completion.expect("completion presence checked above"),
            },
            (false, false) => HookOutcome::KeepSession,
        })
    }

    pub(crate) async fn bootstrap_external_hook(
        agent_id: Uuid,
        payload: &[u8],
        env: &HookEnvironment,
    ) -> std::result::Result<Option<Self>, HookError> {
        let hook =
            ParsedClaudeHook::parse_payload(payload).map_err(|e| HookError::InvalidPayload {
                message: e.to_string(),
            })?;

        if hook.is_unknown() {
            tracing::warn!(%agent_id, "received unknown external Claude hook variant");
            return Ok(None);
        }

        if hook.is_session_end() {
            tracing::debug!(%agent_id, "ignoring external Claude SessionEnd for unknown session");
            return Ok(None);
        }

        let cwd = hook
            .cwd()
            .ok_or(HookError::MissingBootstrapField { field: "cwd" })?;
        if hook.transcript_path().is_none() {
            return Err(HookError::MissingBootstrapField {
                field: "transcript_path",
            });
        }

        let mut session = Self::new_readonly(agent_id, PathBuf::from(cwd));
        session.sync_messaging_credentials(env);
        session.handle_hook(hook).await;
        Ok(Some(session))
    }

    /// Handle a hook event.
    ///
    /// Internal side effects (session_id, transcript linking) use the typed
    /// `ClaudeHook`. Structured output for `hook.permission_request`,
    /// `hook.stop`, and `hook.notification` passes through the original raw
    /// JSON with a `type` field injected — no field loss from typed
    /// round-tripping. `SessionEnd` is internal-only (agent cleanup) and is
    /// not emitted as structured output. Duplicate deliveries of one event
    /// (multiple hook registrations — see `HOOK_DEDUPE_WINDOW`) emit one
    /// structured row.
    pub(crate) async fn handle_hook(&mut self, hook: ParsedClaudeHook) -> bool {
        self.sync_hook_metadata(&hook).await;
        if self.transcript_ingest.is_none() {
            return false;
        }
        // Internal-only kinds return; emitted kinds name their structured
        // `type` tag.
        let type_tag = match hook.kind {
            ClaudeHookKind::SessionStart => {
                let Some(common) = hook.common else {
                    return false;
                };
                tracing::debug!(
                    agent_id = %self.agent_id,
                    session_id = %common.session_id,
                    transcript_path = common.transcript_path,
                    "session started"
                );
                return false;
            }
            ClaudeHookKind::SessionEnd => {
                tracing::debug!(agent_id = %self.agent_id, "session ended");
                return false;
            }
            ClaudeHookKind::Unknown => return false,
            ClaudeHookKind::PermissionRequest => "hook.permission_request",
            ClaudeHookKind::Stop => "hook.stop",
            ClaudeHookKind::Notification => "hook.notification",
        };
        if self.is_duplicate_hook_delivery(&hook.raw) {
            tracing::debug!(
                agent_id = %self.agent_id,
                hook = type_tag,
                "suppressed duplicate hook delivery"
            );
            return false;
        }
        tracing::debug!(agent_id = %self.agent_id, hook = type_tag, "structured hook");
        let mut value = hook.raw;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("type".to_string(), json!(type_tag));
        }
        if let Some(ingest) = &self.transcript_ingest {
            ingest.log_source().write(value).await;
        }
        true
    }

    /// True when this payload is a re-delivery of the event just emitted:
    /// identical fingerprint within `HOOK_DEDUPE_WINDOW`.
    fn is_duplicate_hook_delivery(&mut self, raw: &Value) -> bool {
        let fingerprint = hook_fingerprint(raw);
        let now = tokio::time::Instant::now();
        let duplicate = self
            .last_emitted_hook
            .is_some_and(|(last, at)| last == fingerprint && now - at <= HOOK_DEDUPE_WINDOW);
        self.last_emitted_hook = Some((fingerprint, now));
        duplicate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop_fixture_payload() -> Vec<u8> {
        let row = include_str!("../../../../tests/fixtures/a2a/stop_payload.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("fixture row is JSON"))
            .find(|row| row.get("type").and_then(Value::as_str) == Some("hook.stop"))
            .expect("fixture contains the captured Stop hook");
        serde_json::to_vec(&row).expect("fixture hook serializes")
    }

    fn permission_payload(command: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/nonexistent/amux-test/transcript.jsonl",
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": {"command": command},
        }))
        .expect("payload serializes")
    }

    /// Hook delivery is at-least-once — user or project settings can repeat
    /// the managed registration and run the hook for the same event with
    /// identical stdin. The daemon emits the fact once: byte-identical
    /// payloads within the dedupe window collapse; distinct payloads and
    /// re-deliveries past the window do not.
    #[tokio::test(start_paused = true)]
    async fn duplicate_hook_deliveries_emit_one_structured_row() {
        let mut session =
            ClaudeSession::new_readonly(Uuid::from_u128(1), PathBuf::from("/nonexistent"));
        let log = session.log_source().expect("readonly session tails");

        session
            .handle_hook_payload(&permission_payload("echo one"), &HookEnvironment::new())
            .await
            .expect("hook accepted");
        session
            .handle_hook_payload(&permission_payload("echo one"), &HookEnvironment::new())
            .await
            .expect("hook accepted");
        assert_eq!(log.current_seq().await, 1, "a double delivery is one fact");

        session
            .handle_hook_payload(&permission_payload("echo two"), &HookEnvironment::new())
            .await
            .expect("hook accepted");
        assert_eq!(
            log.current_seq().await,
            2,
            "a distinct payload is a new fact"
        );

        tokio::time::advance(HOOK_DEDUPE_WINDOW + Duration::from_millis(1)).await;
        session
            .handle_hook_payload(&permission_payload("echo two"), &HookEnvironment::new())
            .await
            .expect("hook accepted");
        assert_eq!(
            log.current_seq().await,
            3,
            "an identical event outside the window is a new fact"
        );
    }

    #[tokio::test]
    async fn a2a_hook_env_forward_stores_and_refreshes_messaging_credentials() {
        let payload = stop_fixture_payload();
        let mut session =
            ClaudeSession::new_readonly(Uuid::from_u128(1), PathBuf::from("/nonexistent"));
        let mut first = HookEnvironment::from([
            (
                MESSAGING_SOCKET_ENV.to_string(),
                "/runtime/first.sock".to_string(),
            ),
            (MESSAGING_TOKEN_ENV.to_string(), "first-token".to_string()),
        ]);

        session
            .handle_hook_payload(&payload, &first)
            .await
            .expect("captured hook accepted");
        let credentials = session
            .messaging_credentials
            .as_ref()
            .expect("credentials stored on first hook");
        assert_eq!(
            credentials.socket_path,
            PathBuf::from("/runtime/first.sock")
        );
        assert_eq!(credentials.token, "first-token");

        first.insert(
            MESSAGING_SOCKET_ENV.to_string(),
            "/runtime/rotated.sock".to_string(),
        );
        first.insert(MESSAGING_TOKEN_ENV.to_string(), "rotated-token".to_string());
        session
            .handle_hook_payload(&payload, &first)
            .await
            .expect("duplicate captured hook accepted");
        let credentials = session
            .messaging_credentials
            .as_ref()
            .expect("credentials remain available");
        assert_eq!(
            credentials.socket_path,
            PathBuf::from("/runtime/rotated.sock")
        );
        assert_eq!(credentials.token, "rotated-token");

        let debug = serde_json::to_string(&crate::debug::DebugView::new(&session, false))
            .expect("session debug view serializes");
        assert_eq!(
            serde_json::from_str::<Value>(&debug)
                .expect("debug view is JSON")
                .get("has_messaging_credentials")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(!debug.contains("rotated-token"));
    }
}
