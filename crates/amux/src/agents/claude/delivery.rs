use std::sync::atomic::Ordering;

use async_trait::async_trait;

use super::pty_backend::ClaudePtyBackend;
use crate::agents::{AgentDeliveryTarget, Delivery, DeliveryError, DeliveryLiveness};
use crate::envelope::{Envelope, format_cross_session};

pub(super) struct ClaudeDeliveryTarget {
    readonly: bool,
    control: Option<claude::pty::Control>,
    messaging: Option<claude::hooks::MessagingCredentials>,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ClaudeDeliveryTarget {
    pub(super) fn new(backend: &ClaudePtyBackend) -> Self {
        let (readonly, control, messaging, ready) = backend.delivery_snapshot();
        Self {
            readonly,
            control,
            messaging,
            ready,
        }
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
        if !self.ready.load(Ordering::Acquire) {
            return Ok(DeliveryLiveness::Pending(
                "Claude session has not completed startup".to_string(),
            ));
        }
        if self.control.is_some() {
            Ok(DeliveryLiveness::Live)
        } else {
            Ok(DeliveryLiveness::Pending(
                "Claude delivery target is not ready".to_string(),
            ))
        }
    }

    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        match self.liveness()? {
            DeliveryLiveness::Live => {}
            DeliveryLiveness::Pending(reason) => {
                return Err(DeliveryError::FailedPrecondition(reason));
            }
        }
        let control = self.control.as_ref().expect("live delivery has control");
        let fallback = crate::envelope::format(envelope);
        let (text, carrier) = match (
            format_cross_session(envelope, "prompting"),
            self.messaging.as_ref(),
        ) {
            (Ok(text), Some(credentials)) => (
                text,
                claude::pty::Carrier::Socket {
                    path: credentials.socket_path.clone(),
                    token: credentials.token.clone(),
                    confirmation: envelope.id.to_string(),
                },
            ),
            _ => (fallback, claude::pty::Carrier::Pty),
        };
        let outcome = control
            .deliver(&text, carrier)
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))?;
        Ok(match outcome {
            claude::pty::DeliveryOutcome::Socket => Delivery::Socket,
            claude::pty::DeliveryOutcome::Pty
            | claude::pty::DeliveryOutcome::PtyFallback { .. } => Delivery::Pty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{
        AgentBackend, AgentKind, AgentParent, AgentRecord, AgentType, ClaudeDriver,
        CreateAgentRequest,
    };
    use crate::envelope::{AgentSender, EnvelopeKind, Sender};
    use chrono::Utc;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    fn envelope(id: Uuid, recipient: Uuid) -> Envelope {
        Envelope {
            id,
            context: None,
            from: Sender::Agent(AgentSender {
                agent_id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
                name: "sender".to_string(),
                kind: "test".to_string(),
            }),
            to: AgentParent {
                agent_id: recipient,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello".to_string(),
        }
    }

    fn managed_session() -> (
        ClaudePtyBackend,
        mpsc::Sender<claude::hooks::HookPayload>,
        mpsc::Sender<(PathBuf, claude::transcript::TranscriptRow)>,
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<()>,
    ) {
        let (_output_tx, output) = mpsc::channel(1);
        let (writer, peer) = tokio::io::duplex(64 * 1024);
        let (hooks, hook_tx) = claude::pty::HookSource::channel(8);
        let (transcript, row_tx, _paths) = claude::pty::TranscriptSource::channel(8);
        let session = claude::pty::from_sources(claude::pty::Sources {
            pty: claude::pty::PtySource {
                output,
                writer: Box::new(writer),
                handle: None,
                exit: Box::pin(std::future::pending()),
            },
            hooks,
            transcript,
            version: claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
        });
        let id = Uuid::new_v4();
        let record = AgentRecord {
            id,
            host_id: Uuid::new_v4(),
            name: None,
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudePtyBackend::with_session(record, session);
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ingest = backend.start(&event_tx).unwrap();
        (backend, hook_tx, row_tx, peer, ingest)
    }

    fn session_start(
        messaging: Option<claude::hooks::MessagingCredentials>,
    ) -> claude::hooks::HookPayload {
        let raw = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": Uuid::new_v4(),
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp",
        });
        claude::hooks::HookPayload::SessionStart(claude::hooks::HookCommon {
            session_id: raw["session_id"].as_str().unwrap().parse().unwrap(),
            transcript_path: PathBuf::from("/tmp/transcript.jsonl"),
            cwd: PathBuf::from("/tmp"),
            permission_mode: None,
            messaging,
            raw,
        })
    }

    async fn wait_for_hook(backend: &ClaudePtyBackend, expect_messaging: bool) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let (_, _, messaging, ready) = backend.delivery_snapshot();
                if ready.load(Ordering::Acquire) && messaging.is_some() == expect_messaging {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("managed hook was not ingested");
    }

    #[tokio::test]
    async fn scripted_session_delivers_through_provider_control() {
        let id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id: id,
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Pty,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudePtyBackend::scripted(
            &request,
            PathBuf::from("/tmp"),
            crate::agents::claude::ClaudeVersionCache::default(),
            crate::agents::mcp_launch_route_for_tests(Uuid::new_v4()),
        );
        let envelope = crate::envelope::Envelope {
            id: Uuid::new_v4(),
            context: None,
            from: Sender::Human,
            to: AgentParent {
                agent_id: id,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello".to_string(),
        };
        assert_eq!(backend.deliver(&envelope).await.unwrap(), Delivery::Pty);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_session_uses_messaging_credentials_from_hook_socket() {
        let dir = tempfile::Builder::new()
            .prefix("amd")
            .tempdir_in("/tmp")
            .unwrap();
        let socket_path = dir.path().join("messaging.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (backend, hooks, rows, _peer, _ingest) = managed_session();
        hooks
            .send(session_start(Some(claude::hooks::MessagingCredentials {
                socket_path: socket_path.clone(),
                token: "secret".to_string(),
            })))
            .await
            .unwrap();
        wait_for_hook(&backend, true).await;

        let envelope = envelope(Uuid::new_v4(), backend.agent_id());
        let confirmation = envelope.id.to_string();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufReader::new(stream).lines();
            let auth: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let message: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(auth, serde_json::json!({"type":"auth","token":"secret"}));
            assert!(
                message["message"]["content"]
                    .as_str()
                    .unwrap()
                    .contains(&confirmation)
            );
            rows.send((
                PathBuf::from("/tmp/transcript.jsonl"),
                claude::transcript::TranscriptRow::parse(serde_json::json!({
                    "type": "queue-operation",
                    "operation": "enqueue",
                    "content": confirmation,
                })),
            ))
            .await
            .unwrap();
        });

        assert_eq!(backend.deliver(&envelope).await.unwrap(), Delivery::Socket);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn managed_session_without_messaging_credentials_falls_back_to_paste() {
        let (backend, hooks, _rows, mut peer, _ingest) = managed_session();
        hooks.send(session_start(None)).await.unwrap();
        wait_for_hook(&backend, false).await;

        let envelope = envelope(Uuid::new_v4(), backend.agent_id());
        let expected_text = crate::envelope::format(&envelope);
        let expected = claude::pty::paste_program(&expected_text);
        assert_eq!(backend.deliver(&envelope).await.unwrap(), Delivery::Pty);

        let expected_bytes: Vec<u8> = expected
            .into_iter()
            .filter_map(|step| match step {
                claude::pty::PtyInput::Bytes(bytes) => Some(bytes),
                claude::pty::PtyInput::Delay(_) => None,
            })
            .flatten()
            .collect();
        let mut actual = vec![0; expected_bytes.len()];
        peer.read_exact(&mut actual).await.unwrap();
        assert_eq!(actual, expected_bytes);
    }
}
