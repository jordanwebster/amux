//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! PTY input transport are encapsulated here.

use super::{LocalAgentNameSource, PtyHandle, SessionEvent, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{ClaudeHook, Hook};
use crate::debug::DebugView;
use crate::error::Result;
use crate::message::{CreateAgentRequest, ProtocolError, SubscribeQuery};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) enum PtyInput {
    Bytes(Vec<u8>),
    Delay(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveNameCandidate {
    name: String,
    source: LocalAgentNameSource,
}

#[derive(Debug, Default)]
struct NameSnifferState {
    latest_slug: Option<String>,
    latest_agent_name: Option<String>,
    last_emitted: Option<EffectiveNameCandidate>,
}

impl NameSnifferState {
    /// Record a structured output Value. Returns true if internal state changed.
    fn ingest(&mut self, value: &Value) -> bool {
        let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");

        if entry_type == "agent-name"
            && let Some(name) = value.get("agentName").and_then(Value::as_str)
        {
            self.latest_agent_name = Some(name.to_string());
            return true;
        }

        if let Some(slug) = value.get("slug").and_then(Value::as_str) {
            self.latest_slug = Some(slug.to_string());
            return true;
        }

        false
    }

    /// The current best name candidate based on all ingested data.
    fn effective_candidate(&self) -> Option<EffectiveNameCandidate> {
        let (name, source) = self
            .latest_agent_name
            .as_ref()
            .map(|n| (n, LocalAgentNameSource::ProviderName))
            .or_else(|| {
                self.latest_slug
                    .as_ref()
                    .map(|n| (n, LocalAgentNameSource::ProviderSlug))
            })?;
        Some(EffectiveNameCandidate {
            name: name.clone(),
            source,
        })
    }

    /// Ingest an output event and return a candidate only if it differs from the last emission.
    fn observe(&mut self, value: &Value) -> Option<EffectiveNameCandidate> {
        if !self.ingest(value) {
            return None;
        }
        let candidate = self.effective_candidate()?;
        if self.last_emitted.as_ref() == Some(&candidate) {
            return None;
        }
        self.last_emitted = Some(candidate.clone());
        Some(candidate)
    }
}

fn spawn_name_sniffer(
    log_source: StructuredLogSource,
    event_tx: mpsc::Sender<SessionEvent>,
    agent_id: Uuid,
    user_id: Uuid,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut reader) = log_source.subscribe().await else {
            return;
        };

        let mut state = NameSnifferState::default();

        while let Some(entry) = reader.read().await {
            let Some(candidate) = state.observe(&entry.payload) else {
                continue;
            };

            if event_tx
                .send(SessionEvent::NameCandidateChanged {
                    agent_id,
                    user_id,
                    name: candidate.name,
                    source: candidate.source,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

pub struct ClaudeSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<crate::message::TerminalSize>,
    /// Claude session ID. Set from SessionStart hook during normal operation,
    /// or pre-set before `start()` for resume (triggers `--resume <id>`).
    pub(super) session_id: Option<Uuid>,
    /// True for externally-started sessions (no PTY, transcript-only)
    pub(super) readonly: bool,
    /// Extra arguments passed to the claude command
    pub(super) args: Vec<String>,
    name_source: LocalAgentNameSource,
    name_sniffer_abort: Option<AbortHandle>,
    pub(super) created_at: DateTime<Utc>,
}

impl ClaudeSession {
    /// Create a new ClaudeSession from a CreateAgentRequest.
    /// Does not spawn the process — call [`start`] afterwards.
    pub fn new(req: &CreateAgentRequest) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
            session_id: None,
            readonly: false,
            args: req.args.clone(),
            name_source: if req.name.is_some() {
                LocalAgentNameSource::Amux
            } else {
                LocalAgentNameSource::Unset
            },
            name_sniffer_abort: None,
            created_at: Utc::now(),
        }
    }

    /// Create a readonly session for an externally-started Claude process.
    /// Has a StructuredLogSource (for transcript tailing) but no PTY.
    pub fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        Self {
            agent_id,
            name: None,
            command: "claude".to_string(),
            working_dir,
            pty: None,
            log_source: Some(StructuredLogSource::new()),
            terminal_size: None,
            session_id: None,
            readonly: true,
            args: vec![],
            name_source: LocalAgentNameSource::Unset,
            name_sniffer_abort: None,
            created_at: Utc::now(),
        }
    }

    pub fn name_source(&self) -> LocalAgentNameSource {
        self.name_source
    }

    pub fn set_name_and_source(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        self.name = name;
        self.name_source = source;
        if matches!(source, LocalAgentNameSource::Amux)
            && let Some(abort) = self.name_sniffer_abort.take()
        {
            abort.abort();
        }
    }

    pub fn maybe_start_name_sniffer(
        &mut self,
        user_id: Uuid,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) {
        if self.name_sniffer_abort.is_some()
            || matches!(self.name_source, LocalAgentNameSource::Amux)
        {
            return;
        }
        let Some(log_source) = &self.log_source else {
            return;
        };

        let handle =
            spawn_name_sniffer(log_source.clone(), event_tx.clone(), self.agent_id, user_id);
        self.name_sniffer_abort = Some(handle.abort_handle());
    }

    /// Link a transcript file for structured output tailing.
    pub async fn link_transcript(&self, path: PathBuf) {
        if let Some(log_source) = &self.log_source {
            log_source.link_transcript(path).await;
        }
    }

    async fn sync_hook_metadata(&mut self, hook: &ClaudeHook) -> Result<()> {
        if let Some(session_id) = hook.session_id() {
            self.session_id = Some(session_id);
        }

        if let Some(transcript_path) = hook.transcript_path() {
            self.link_transcript(PathBuf::from(transcript_path)).await;
        }

        Ok(())
    }

    /// Spawn the Claude Code process. Returns an exit handle that completes
    /// when the process exits. If `session_id` is set, passes `--resume <id>`.
    /// Extra args from creation are appended.
    pub fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let env = [("AMUX_AGENT_ID", self.agent_id.to_string())];
        let mut args: Vec<String> = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => vec![],
        };
        args.extend(self.args.iter().cloned());
        let (pty, log_source, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &args,
            &self.working_dir,
            &env,
            self.terminal_size,
        )?;
        self.pty = Some(pty);
        self.log_source = Some(log_source);
        Ok(exit_handle)
    }

    /// Validate seq and send structured input to Claude Code.
    pub async fn send_structured_input(
        &self,
        client_seq: u64,
        payload: Value,
    ) -> std::result::Result<(), ProtocolError> {
        if self.readonly {
            return Err(ProtocolError::ServerError(
                "session is readonly".to_string(),
            ));
        }
        let current_seq = self.current_seq().await;
        if client_seq != current_seq {
            return Err(ProtocolError::SequenceNumberMismatch {
                client_seq,
                current_seq,
            });
        }

        let actions: Vec<PtyInput> = serde_json::from_value(payload)
            .map_err(|e| ProtocolError::ServerError(format!("invalid pty input: {e}")))?;

        tracing::info!(
            agent_id = %self.agent_id,
            client_seq,
            action_count = actions.len(),
            "structured input accepted"
        );

        let Some(pty) = &self.pty else {
            return Ok(());
        };
        for action in &actions {
            match action {
                PtyInput::Bytes(bytes) => pty
                    .send_input(bytes.clone())
                    .await
                    .map_err(|e| ProtocolError::ServerError(e.to_string()))?,
                PtyInput::Delay(ms) => {
                    let clamped = (*ms).min(5000);
                    tokio::time::sleep(Duration::from_millis(u64::from(clamped))).await
                }
            }
        }
        Ok(())
    }

    /// Handle a hook event.
    ///
    /// Internal side effects (session_id, transcript linking) use the typed
    /// `ClaudeHook`. Structured output for `hook.permission_request`,
    /// `hook.stop`, and `hook.notification` passes through the original raw
    /// JSON with a `type` field injected — no field loss from typed
    /// round-tripping. `SessionEnd` is internal-only (agent cleanup) and is
    /// not emitted as structured output.
    pub async fn handle_hook(&mut self, hook: Hook) -> Result<()> {
        let Hook::Claude(ref claude_hook, _) = hook;
        self.sync_hook_metadata(claude_hook).await?;
        let Some(log_source) = &self.log_source else {
            return Ok(());
        };
        match hook {
            Hook::Claude(ClaudeHook::SessionStart(session_start), _) => {
                tracing::debug!(
                    agent_id = %self.agent_id,
                    session_id = %session_start.session_id,
                    transcript_path = session_start.transcript_path,
                    "session started"
                );
            }
            Hook::Claude(ClaudeHook::PermissionRequest(_), raw) => {
                tracing::debug!(agent_id = %self.agent_id, "permission request");
                let mut value = raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.permission_request"));
                }
                log_source.write(value).await;
            }
            Hook::Claude(ClaudeHook::Stop(_), raw) => {
                tracing::debug!(agent_id = %self.agent_id, "agent stopped");
                let mut value = raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.stop"));
                }
                log_source.write(value).await;
            }
            Hook::Claude(ClaudeHook::Notification(ref n), raw) => {
                tracing::debug!(
                    agent_id = %self.agent_id,
                    notification_type = %n.notification_type,
                    "notification"
                );
                let mut value = raw;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("type".to_string(), json!("hook.notification"));
                }
                log_source.write(value).await;
            }
            Hook::Claude(ClaudeHook::SessionEnd(_), _) => {
                // Internal cleanup only — not emitted as structured output
                tracing::debug!(agent_id = %self.agent_id, "session ended");
            }
            Hook::Claude(ClaudeHook::Unknown, _) => {}
        }
        Ok(())
    }

    /// Return the current structured output sequence number.
    pub async fn current_seq(&self) -> u64 {
        match &self.log_source {
            Some(log_source) => log_source.current_seq().await,
            None => 0,
        }
    }

    pub fn log_source(&self) -> Option<StructuredLogSource> {
        self.log_source.clone()
    }

    /// Subscribe to structured log output.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.log_source.as_ref()?.subscribe().await
    }

    /// Subscribe to structured log output with an optional query filter
    /// and return the matching seq.
    pub async fn subscribe_with_query(
        &self,
        query: Option<SubscribeQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        self.log_source.as_ref()?.subscribe_with_query(query).await
    }

    /// Shut down the session according to the given policy.
    pub async fn stop(&self, policy: super::StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "shutting down claude session");
        if let Some(abort) = &self.name_sniffer_abort {
            abort.abort();
        }
        match policy {
            super::StopPolicy::Interrupt => {
                if let Some(pty) = &self.pty {
                    let _ = pty.send_input(vec![0x03]).await;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = pty.send_input(vec![0x03]).await;
                }
            }
        }
        if let Some(pty) = &self.pty {
            pty.close().await;
        }
        if let Some(log_source) = &self.log_source {
            log_source.close().await;
        }
    }
}

impl Serialize for DebugView<'_, ClaudeSession> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let session = self.inner;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "claude")?;
        if let Some(session_id) = session.session_id {
            map.serialize_entry("session_id", &session_id)?;
        }
        map.serialize_entry("readonly", &session.readonly)?;
        map.serialize_entry("has_pty", &session.pty.is_some())?;
        if let Some(log_source) = &session.log_source {
            map.serialize_entry("transcript", &DebugView::new(log_source, self.verbose))?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{LocalAgentNameSource, SessionEvent};
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    #[test]
    fn test_pty_input_deserializes() {
        let json = r#"[{"Bytes":[104,101,108,108,111]},{"Delay":20},{"Bytes":[13]}]"#;
        let actions: Vec<PtyInput> = serde_json::from_str(json).unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], PtyInput::Bytes(b"hello".to_vec()));
        assert_eq!(actions[1], PtyInput::Delay(20));
        assert_eq!(actions[2], PtyInput::Bytes(vec![13]));
    }

    #[test]
    fn test_pty_input_rejects_unknown_variant() {
        let json = r#"[{"Unknown":"bad"}]"#;
        assert!(serde_json::from_str::<Vec<PtyInput>>(json).is_err());
    }

    #[test]
    fn test_pty_input_empty_array() {
        let actions: Vec<PtyInput> = serde_json::from_str("[]").unwrap();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn non_session_start_hook_links_transcript_once() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        tokio::fs::write(
            &transcript_path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"world\"}]},\"uuid\":\"a1\",\"timestamp\":\"2026-03-29T10:00:01Z\"}\n"
            ),
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let transcript_path_str = transcript_path.display().to_string();
        let cwd = dir.path().display().to_string();
        let mut session = ClaudeSession::new_readonly(Uuid::new_v4(), dir.path().to_path_buf());

        session
            .handle_hook(Hook::from_claude(ClaudeHook::PermissionRequest(Box::new(
                crate::claude::types::ClaudePermissionRequest {
                    session_id,
                    transcript_path: transcript_path_str.clone(),
                    cwd: cwd.clone(),
                    tool: crate::claude::types::ClaudePermissionTool::Bash {
                        tool_input: crate::claude::types::BashToolInput {
                            command: "echo hi".to_string(),
                            description: Some("test".to_string()),
                            timeout: None,
                            run_in_background: None,
                            dangerously_disable_sandbox: None,
                        },
                    },
                },
            ))))
            .await
            .unwrap();

        // 2 transcript lines + 1 hook event = 3
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if session.current_seq().await >= 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let seq_after_first_hook = session.current_seq().await;
        session
            .handle_hook(Hook::from_claude(ClaudeHook::Stop(
                crate::claude::types::ClaudeStop {
                    session_id,
                    stop_hook_active: false,
                    last_assistant_message: String::new(),
                    transcript_path: transcript_path_str,
                    cwd,
                },
            )))
            .await
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if session.current_seq().await > seq_after_first_hook {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(session.session_id, Some(session_id));
        assert_eq!(session.current_seq().await, seq_after_first_hook + 1);
    }

    #[tokio::test]
    async fn session_end_hook_does_not_emit_structured_output() {
        let dir = tempdir().unwrap();
        let session_id = Uuid::new_v4();
        let cwd = dir.path().display().to_string();
        let mut session = ClaudeSession::new_readonly(Uuid::new_v4(), dir.path().to_path_buf());

        // Link transcript so the log source exists
        let transcript_path = dir.path().join("transcript.jsonl");
        tokio::fs::write(&transcript_path, "").await.unwrap();
        session
            .handle_hook(Hook::from_claude(ClaudeHook::SessionStart(
                crate::claude::types::ClaudeSessionStart {
                    session_id,
                    transcript_path: transcript_path.display().to_string(),
                    cwd: cwd.clone(),
                },
            )))
            .await
            .unwrap();

        let seq_before = session.current_seq().await;

        session
            .handle_hook(Hook::from_claude(ClaudeHook::SessionEnd(
                crate::claude::types::ClaudeSessionEnd {
                    session_id,
                    transcript_path: transcript_path.display().to_string(),
                    cwd,
                },
            )))
            .await
            .unwrap();

        // SessionEnd is internal-only — seq must not advance
        assert_eq!(
            session.current_seq().await,
            seq_before,
            "SessionEnd should not emit structured output"
        );
    }

    #[tokio::test]
    async fn name_sniffer_ignores_custom_title() {
        let log_source = StructuredLogSource::new();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let sniffer =
            spawn_name_sniffer(log_source.clone(), event_tx, Uuid::new_v4(), Uuid::new_v4());

        log_source
            .write(json!({"type": "custom-title", "customTitle": "Ignored"}))
            .await;

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), event_rx.recv())
                .await
                .is_err(),
            "custom-title should not emit a name candidate event"
        );

        sniffer.abort();
        let _ = sniffer.await;
    }

    #[tokio::test]
    async fn name_sniffer_emits_same_name_when_source_upgrades() {
        let log_source = StructuredLogSource::new();
        let agent_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let sniffer = spawn_name_sniffer(log_source.clone(), event_tx, agent_id, user_id);

        log_source
            .write(json!({
                "type": "user",
                "message": {"content": "hello"},
                "uuid": "u1",
                "timestamp": "2026-04-03T10:00:00Z",
                "slug": "shared-name"
            }))
            .await;

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            first,
            SessionEvent::NameCandidateChanged {
                agent_id: id,
                user_id: uid,
                name,
                source: LocalAgentNameSource::ProviderSlug,
            } if id == agent_id && uid == user_id && name == "shared-name"
        ));

        log_source
            .write(json!({"type": "agent-name", "agentName": "shared-name"}))
            .await;

        let second = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            second,
            SessionEvent::NameCandidateChanged {
                name,
                source: LocalAgentNameSource::ProviderName,
                ..
            } if name == "shared-name"
        ));

        sniffer.abort();
        let _ = sniffer.await;
    }

    #[tokio::test]
    async fn maybe_start_name_sniffer_runs_for_existing_provider_slug_name() {
        let agent_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let dir = tempdir().unwrap();
        let mut session = ClaudeSession::new_readonly(agent_id, dir.path().to_path_buf());
        session.set_name_and_source(
            Some("slug-derived-name".to_string()),
            LocalAgentNameSource::ProviderSlug,
        );

        let (event_tx, mut event_rx) = mpsc::channel(4);
        session.maybe_start_name_sniffer(user_id, &event_tx);

        let log_source = session
            .log_source()
            .expect("readonly session has log source");
        log_source
            .write(json!({"type": "agent-name", "agentName": "upgraded-provider-name"}))
            .await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            SessionEvent::NameCandidateChanged {
                agent_id: id,
                user_id: uid,
                name,
                source: LocalAgentNameSource::ProviderName,
            } if id == agent_id && uid == user_id && name == "upgraded-provider-name"
        ));
    }

    #[tokio::test]
    async fn maybe_start_name_sniffer_runs_for_existing_provider_name() {
        let agent_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let dir = tempdir().unwrap();
        let mut session = ClaudeSession::new_readonly(agent_id, dir.path().to_path_buf());
        session.set_name_and_source(
            Some("provider-name".to_string()),
            LocalAgentNameSource::ProviderName,
        );

        let (event_tx, mut event_rx) = mpsc::channel(4);
        session.maybe_start_name_sniffer(user_id, &event_tx);

        let log_source = session
            .log_source()
            .expect("readonly session has log source");
        log_source
            .write(json!({"type": "agent-name", "agentName": "renamed-provider-name"}))
            .await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            SessionEvent::NameCandidateChanged {
                agent_id: id,
                user_id: uid,
                name,
                source: LocalAgentNameSource::ProviderName,
            } if id == agent_id && uid == user_id && name == "renamed-provider-name"
        ));
    }
}
