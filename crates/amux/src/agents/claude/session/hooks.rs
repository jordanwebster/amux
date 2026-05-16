use std::path::PathBuf;

use serde_json::json;
use uuid::Uuid;

use super::core::ClaudeSession;
use crate::agents::claude::hooks::{ClaudeHookKind, ParsedClaudeHook};
use crate::agents::{HookError, HookOutcome};

impl ClaudeSession {
    /// Link a transcript file for structured output tailing.
    async fn link_transcript(&self, path: PathBuf) {
        if let Some(log_source) = &self.log_source {
            log_source.link_transcript(path).await;
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

    pub(crate) async fn handle_hook_payload(
        &mut self,
        payload: &[u8],
    ) -> std::result::Result<HookOutcome, HookError> {
        let hook =
            ParsedClaudeHook::parse_payload(payload).map_err(|e| HookError::InvalidPayload {
                message: e.to_string(),
            })?;
        let is_unknown = hook.is_unknown();
        let is_session_end = hook.is_session_end();
        if is_unknown {
            tracing::warn!(agent_id = %self.agent_id, "received unknown Claude hook variant");
        }
        self.handle_hook(hook).await;
        Ok(match (is_unknown, self.readonly && is_session_end) {
            (true, _) => HookOutcome::Noop,
            (false, true) => HookOutcome::WithdrawSession,
            (false, false) => HookOutcome::KeepSession,
        })
    }

    pub(crate) async fn bootstrap_external_hook(
        agent_id: Uuid,
        payload: &[u8],
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
    /// not emitted as structured output.
    pub(crate) async fn handle_hook(&mut self, hook: ParsedClaudeHook) {
        self.sync_hook_metadata(&hook).await;
        let Some(log_source) = &self.log_source else {
            return;
        };
        match hook.kind {
            ClaudeHookKind::SessionStart => {
                let Some(common) = hook.common else {
                    return;
                };
                tracing::debug!(
                    agent_id = %self.agent_id,
                    session_id = %common.session_id,
                    transcript_path = common.transcript_path,
                    "session started"
                );
            }
            ClaudeHookKind::PermissionRequest => {
                tracing::debug!(agent_id = %self.agent_id, "permission request");
                let mut value = hook.raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.permission_request"));
                }
                log_source.write(value).await;
            }
            ClaudeHookKind::Stop => {
                tracing::debug!(agent_id = %self.agent_id, "agent stopped");
                let mut value = hook.raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.stop"));
                }
                log_source.write(value).await;
            }
            ClaudeHookKind::Notification => {
                tracing::debug!(agent_id = %self.agent_id, "notification");
                let mut value = hook.raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.notification"));
                }
                log_source.write(value).await;
            }
            ClaudeHookKind::SessionEnd => {
                tracing::debug!(agent_id = %self.agent_id, "session ended");
            }
            ClaudeHookKind::Unknown => {}
        }
    }
}
