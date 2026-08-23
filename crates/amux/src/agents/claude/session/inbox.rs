use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use uuid::Uuid;

use super::core::{ClaudeMessagingCredentials, ClaudeSession};
use super::input::{paste_program, send_pty_program};
use crate::agents::{
    AgentDeliveryTarget, Delivery, DeliveryError, DeliveryLiveness, PtyHandle,
    SequencedReplayQuery, StructuredLogSource,
};
use crate::envelope::{Envelope, format_cross_session};

const SOCKET_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct ClaudeDeliveryTarget {
    readonly: bool,
    pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,
    messaging_credentials: Option<ClaudeMessagingCredentials>,
    pty_only: Arc<AtomicBool>,
}

impl ClaudeDeliveryTarget {
    pub(super) fn new(session: &ClaudeSession) -> Self {
        Self {
            readonly: session.readonly,
            pty: session.pty.clone(),
            log_source: session.log_source(),
            messaging_credentials: session.messaging_credentials.clone(),
            pty_only: session.pty_only_delivery.clone(),
        }
    }

    async fn deliver_pty(
        &self,
        envelope: &Envelope,
    ) -> std::result::Result<Delivery, DeliveryError> {
        let pty = self
            .pty
            .as_ref()
            .ok_or_else(|| DeliveryError::Failed("Claude PTY is unavailable".to_string()))?;
        let program = paste_program(&crate::envelope::format(envelope));
        send_pty_program(pty, &program)
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))?;
        Ok(Delivery::Pty)
    }

    #[cfg(unix)]
    async fn try_socket(&self, envelope: &Envelope, content: &str) -> anyhow::Result<bool> {
        let credentials = self
            .messaging_credentials
            .as_ref()
            .expect("socket delivery requires snapshotted credentials");
        let log_source = self
            .log_source
            .as_ref()
            .expect("socket delivery requires a structured log source");

        let next_seq = log_source.current_seq().await.saturating_add(1);
        let Some((mut rows, _)) = log_source
            .subscribe_with_query(Some(SequencedReplayQuery::Since { seq: next_seq }))
            .await
        else {
            anyhow::bail!("Claude transcript source is closed");
        };

        let mut stream = UnixStream::connect(&credentials.socket_path).await?;
        let auth = json!({"type": "auth", "token": credentials.token});
        let message = json!({
            "type": "user",
            "message": {"role": "user", "content": content},
        });
        stream.write_all(auth.to_string().as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.write_all(message.to_string().as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.shutdown().await?;

        Ok(tokio::time::timeout(SOCKET_CONFIRMATION_TIMEOUT, async {
            while let Some(row) = rows.read().await {
                if row_confirms_delivery(&row.payload, envelope.id) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false))
    }
}

#[async_trait]
impl AgentDeliveryTarget for ClaudeDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        if self.readonly {
            return Err(DeliveryError::FailedPrecondition(
                "session is readonly and cannot receive messages".to_string(),
            ));
        }
        if self.pty.is_some() || (self.messaging_credentials.is_some() && self.log_source.is_some())
        {
            Ok(DeliveryLiveness::Live)
        } else {
            Ok(DeliveryLiveness::Pending(
                "Claude delivery target is not ready".to_string(),
            ))
        }
    }

    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        if self.readonly {
            return Err(DeliveryError::FailedPrecondition(
                "session is readonly and cannot receive messages".to_string(),
            ));
        }
        if self.pty_only.load(Ordering::Acquire) {
            return self.deliver_pty(envelope).await;
        }

        let Ok(content) = format_cross_session(envelope, "prompting") else {
            return self.deliver_pty(envelope).await;
        };
        if self.messaging_credentials.is_none() || self.log_source.is_none() {
            return self.deliver_pty(envelope).await;
        }

        #[cfg(unix)]
        match self.try_socket(envelope, &content).await {
            Ok(true) => return Ok(Delivery::Socket),
            Ok(false) => tracing::warn!(
                envelope_id = %envelope.id,
                "Claude inbox delivery was not confirmed; using PTY for this session"
            ),
            Err(error) => tracing::warn!(
                envelope_id = %envelope.id,
                %error,
                "Claude inbox delivery failed; using PTY for this session"
            ),
        }

        #[cfg(not(unix))]
        tracing::warn!(
            envelope_id = %envelope.id,
            "Claude inbox delivery is unavailable on this platform; using PTY for this session"
        );

        self.pty_only.store(true, Ordering::Release);
        self.deliver_pty(envelope).await
    }
}

fn row_confirms_delivery(row: &Value, envelope_id: Uuid) -> bool {
    let id = envelope_id.to_string();
    let peer_user = row.get("type").and_then(Value::as_str) == Some("user")
        && row.pointer("/origin/kind").and_then(Value::as_str) == Some("peer")
        && row
            .pointer("/message/content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains(&id));
    let queued_command = row.get("type").and_then(Value::as_str) == Some("attachment")
        && row.pointer("/attachment/type").and_then(Value::as_str) == Some("queued_command")
        && row
            .pointer("/attachment/prompt")
            .and_then(Value::as_str)
            .is_some_and(|prompt| prompt.contains(&id));
    peer_user || queued_command
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;
    use crate::agents::{AgentBackend, AgentParent, AgentType, CreateAgentRequest};
    use crate::envelope::{AgentSender, EnvelopeKind, Sender};

    fn test_envelope(recipient_id: Uuid, text: &str) -> Envelope {
        Envelope {
            id: Uuid::new_v4(),
            context: Some(Uuid::new_v4()),
            from: Sender::Agent(AgentSender {
                agent_id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
                name: "sender".to_string(),
                kind: "codex".to_string(),
            }),
            to: AgentParent {
                agent_id: recipient_id,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: text.to_string(),
        }
    }

    fn session(socket_path: PathBuf) -> (ClaudeSession, StructuredLogSource) {
        let recipient_id = Uuid::new_v4();
        let mut session = ClaudeSession::new(
            &CreateAgentRequest {
                agent_id: recipient_id,
                host_id: None,
                name: Some("recipient".to_string()),
                agent_type: AgentType::Claude,
                working_dir: PathBuf::from("/work"),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            },
            PathBuf::from("/runtime"),
            None,
        );
        session.pty = Some(PtyHandle::test_echo());
        let source = StructuredLogSource::new(32);
        session.transcript_ingest =
            Some(crate::agents::claude::transcript_ingest::TranscriptIngest::new(source.clone()));
        session.messaging_credentials = Some(ClaudeMessagingCredentials {
            socket_path,
            token: "socket-token".to_string(),
        });
        (session, source)
    }

    async fn read_socket_post(listener: UnixListener) -> (Value, Value) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let auth = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let message = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        (auth, message)
    }

    #[tokio::test(start_paused = true)]
    async fn a2a_socket_carrier_confirms_peer_user_row() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("claude.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (session, source) = session(socket_path);
        let envelope = test_envelope(session.agent_id, "hello over the inbox");
        let expected_content = format_cross_session(&envelope, "prompting").unwrap();
        let envelope_id = envelope.id;
        let server = tokio::spawn(async move {
            let posted = read_socket_post(listener).await;
            source
                .write(json!({
                    "type": "user",
                    "isMeta": true,
                    "origin": {"kind": "peer"},
                    "message": {"content": format!("peer wrapper {envelope_id}")},
                }))
                .await;
            posted
        });

        assert_eq!(session.deliver(&envelope).await.unwrap(), Delivery::Socket);
        assert!(!session.pty_only_delivery.load(Ordering::Acquire));
        let (auth, message) = server.await.unwrap();
        assert_eq!(auth, json!({"type": "auth", "token": "socket-token"}));
        assert_eq!(
            message,
            json!({
                "type": "user",
                "message": {"role": "user", "content": expected_content},
            })
        );
    }

    #[test]
    fn a2a_socket_carrier_accepts_only_attributable_confirmation_rows() {
        let id = Uuid::new_v4();
        assert!(row_confirms_delivery(
            &json!({
                "type": "user",
                "origin": {"kind": "peer"},
                "message": {"content": format!("native {id}")},
            }),
            id,
        ));
        assert!(row_confirms_delivery(
            &json!({
                "type": "attachment",
                "attachment": {"type": "queued_command", "prompt": format!("queued {id}")},
            }),
            id,
        ));
        assert!(!row_confirms_delivery(
            &json!({"type": "queue-operation", "content": id.to_string()}),
            id,
        ));
        assert!(!row_confirms_delivery(
            &json!({
                "type": "user",
                "origin": {"kind": "human"},
                "message": {"content": id.to_string()},
            }),
            id,
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a2a_socket_carrier_timeout_marks_pty_only_and_resends() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("claude.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (session, _source) = session(socket_path);
        let mut pty_output = session
            .pty
            .as_ref()
            .unwrap()
            .subscribe_with_query(None)
            .await
            .unwrap();
        let envelope = test_envelope(session.agent_id, "fallback body");
        let server = tokio::spawn(read_socket_post(listener));
        let started = tokio::time::Instant::now();

        assert_eq!(session.deliver(&envelope).await.unwrap(), Delivery::Pty);
        assert!(tokio::time::Instant::now() - started >= SOCKET_CONFIRMATION_TIMEOUT);
        assert!(session.pty_only_delivery.load(Ordering::Acquire));
        let (auth, message) = server.await.unwrap();
        assert_eq!(auth["token"], "socket-token");
        assert!(
            message["message"]["content"]
                .as_str()
                .unwrap()
                .contains(&envelope.id.to_string())
        );
        assert_eq!(
            pty_output.read().await.unwrap(),
            format!("\x1b[200~{}\x1b[201~", crate::envelope::format(&envelope)).into_bytes()
        );
        assert_eq!(pty_output.read().await.unwrap(), b"\r");

        let second = test_envelope(session.agent_id, "stays on PTY");
        assert_eq!(session.deliver(&second).await.unwrap(), Delivery::Pty);
        assert_eq!(
            pty_output.read().await.unwrap(),
            format!("\x1b[200~{}\x1b[201~", crate::envelope::format(&second)).into_bytes()
        );
        assert_eq!(pty_output.read().await.unwrap(), b"\r");
    }
}
