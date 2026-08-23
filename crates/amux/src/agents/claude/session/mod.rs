//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! PTY input transport are encapsulated here.

#[cfg(test)]
use serde_json::json;

#[cfg(test)]
use crate::agents::StructuredLogSource;
#[cfg(test)]
use crate::agents::claude::hooks::{ClaudeHookKind, HookCommon, ParsedClaudeHook};

mod backend;
mod core;
mod hooks;
mod inbox;
mod input;
mod name_sniffer;

pub(crate) use core::ClaudeSession;

#[cfg(test)]
use input::PtyInput;
#[cfg(test)]
use name_sniffer::spawn_name_sniffer;

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::agents::{LocalAgentNameSource, SessionEvent};

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
        let mut session = ClaudeSession::new_readonly(
            Uuid::new_v4(),
            dir.path().to_path_buf(),
            crate::agents::claude::ClaudeVersionCache::default(),
        );

        session
            .handle_hook(ParsedClaudeHook::from_typed(
                ClaudeHookKind::PermissionRequest,
                HookCommon {
                    session_id,
                    transcript_path: transcript_path_str.clone(),
                    cwd: cwd.clone(),
                },
            ))
            .await;

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
            .handle_hook(ParsedClaudeHook::from_typed(
                ClaudeHookKind::Stop,
                HookCommon {
                    session_id,
                    transcript_path: transcript_path_str,
                    cwd,
                },
            ))
            .await;

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
        let mut session = ClaudeSession::new_readonly(
            Uuid::new_v4(),
            dir.path().to_path_buf(),
            crate::agents::claude::ClaudeVersionCache::default(),
        );

        // Link transcript so the log source exists
        let transcript_path = dir.path().join("transcript.jsonl");
        tokio::fs::write(&transcript_path, "").await.unwrap();
        session
            .handle_hook(ParsedClaudeHook::from_typed(
                ClaudeHookKind::SessionStart,
                HookCommon {
                    session_id,
                    transcript_path: transcript_path.display().to_string(),
                    cwd: cwd.clone(),
                },
            ))
            .await;

        let seq_before = session.current_seq().await;

        session
            .handle_hook(ParsedClaudeHook::from_typed(
                ClaudeHookKind::SessionEnd,
                HookCommon {
                    session_id,
                    transcript_path: transcript_path.display().to_string(),
                    cwd,
                },
            ))
            .await;

        // SessionEnd is internal-only — seq must not advance
        assert_eq!(
            session.current_seq().await,
            seq_before,
            "SessionEnd should not emit structured output"
        );
    }

    #[tokio::test]
    async fn name_sniffer_ignores_custom_title() {
        let log_source = StructuredLogSource::new(1000);
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let sniffer = spawn_name_sniffer(log_source.clone(), event_tx, Uuid::new_v4());

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
        let log_source = StructuredLogSource::new(1000);
        let agent_id = Uuid::new_v4();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let sniffer = spawn_name_sniffer(log_source.clone(), event_tx, agent_id);

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
                name,
                source: LocalAgentNameSource::ProviderSlug,
            } if id == agent_id && name == "shared-name"
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
        let dir = tempdir().unwrap();
        let mut session = ClaudeSession::new_readonly(
            agent_id,
            dir.path().to_path_buf(),
            crate::agents::claude::ClaudeVersionCache::default(),
        );
        session.set_name_and_source(
            Some("slug-derived-name".to_string()),
            LocalAgentNameSource::ProviderSlug,
        );

        let (event_tx, mut event_rx) = mpsc::channel(4);
        session.maybe_start_name_sniffer(&event_tx);

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
                name,
                source: LocalAgentNameSource::ProviderName,
            } if id == agent_id && name == "upgraded-provider-name"
        ));
    }

    #[tokio::test]
    async fn maybe_start_name_sniffer_runs_for_existing_provider_name() {
        let agent_id = Uuid::new_v4();
        let dir = tempdir().unwrap();
        let mut session = ClaudeSession::new_readonly(
            agent_id,
            dir.path().to_path_buf(),
            crate::agents::claude::ClaudeVersionCache::default(),
        );
        session.set_name_and_source(
            Some("provider-name".to_string()),
            LocalAgentNameSource::ProviderName,
        );

        let (event_tx, mut event_rx) = mpsc::channel(4);
        session.maybe_start_name_sniffer(&event_tx);

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
                name,
                source: LocalAgentNameSource::ProviderName,
            } if id == agent_id && name == "renamed-provider-name"
        ));
    }
}
