//! Agent-to-agent delivery through Claude's stream-JSON input.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::sdk_backend::{ClaudeSdkBackend, Runtime};
use super::sdk_io::{ClaudeSdkSynthesized, ClaudeSdkV1Row};
use crate::agents::{
    AgentDeliveryTarget, Delivery, DeliveryError, DeliveryLiveness, StructuredLogSource,
};
use crate::envelope::Envelope;

pub(super) struct ClaudeSdkDeliveryTarget {
    runtime: Arc<Mutex<Runtime>>,
    log: StructuredLogSource,
}

impl ClaudeSdkDeliveryTarget {
    pub(super) fn new(backend: &ClaudeSdkBackend) -> Self {
        let (runtime, log) = backend.delivery_snapshot();
        Self { runtime, log }
    }
}

#[async_trait]
impl AgentDeliveryTarget for ClaudeSdkDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        let runtime = self.runtime.lock().expect("Claude SDK runtime poisoned");
        if runtime.exited {
            return Err(DeliveryError::FailedPrecondition(
                "Claude SDK session has exited".to_string(),
            ));
        }
        if runtime.ready && runtime.control.is_some() {
            Ok(DeliveryLiveness::Live)
        } else {
            Ok(DeliveryLiveness::Pending(
                "Claude SDK session has not completed startup".to_string(),
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
        let control = self
            .runtime
            .lock()
            .expect("Claude SDK runtime poisoned")
            .control
            .clone()
            .ok_or_else(|| {
                DeliveryError::FailedPrecondition(
                    "Claude SDK delivery target is not ready".to_string(),
                )
            })?;
        control
            .prompt(claude::sdk::UserMessage::text(crate::envelope::format(
                envelope,
            )))
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))?;
        self.log
            .write(
                ClaudeSdkV1Row::Synthesized(ClaudeSdkSynthesized::Message {
                    envelope: serde_json::to_value(envelope)
                        .expect("amux envelopes serialize as JSON"),
                    delivery: Delivery::Stream.carrier().to_string(),
                })
                .into_json(),
            )
            .await;
        Ok(Delivery::Stream)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use chrono::Utc;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, duplex};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::agents::{
        AgentBackend, AgentKind, AgentParent, AgentRecord, ClaudeDriver, Plane, Protocol,
        SessionEvent,
    };
    use crate::envelope::{AgentSender, EnvelopeKind, Sender};

    async fn read_json_line(reader: &mut BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write_json_line(writer: &mut tokio::io::DuplexStream, value: serde_json::Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }

    fn envelope(recipient: Uuid) -> Envelope {
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
                agent_id: recipient,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello SDK Claude".to_string(),
        }
    }

    async fn session() -> (claude::sdk::Session, tokio::task::JoinHandle<()>, Uuid) {
        let session_id = Uuid::new_v4();
        let (sdk_stdin, server_stdin) = duplex(16 * 1024);
        let (server_stdout, sdk_stdout) = duplex(16 * 1024);
        let server = tokio::spawn(async move {
            let mut stdin = BufReader::new(server_stdin);
            let mut stdout = server_stdout;
            let init = read_json_line(&mut stdin).await;
            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": init["request_id"],
                        "response": {
                            "commands": [],
                            "agents": [],
                            "output_style": "default",
                            "available_output_styles": [],
                            "models": [],
                            "account": {}
                        }
                    }
                }),
            )
            .await;
            let prompt = read_json_line(&mut stdin).await;
            let text = prompt["message"]["content"].as_str().unwrap();
            let parsed = crate::envelope::parse(text).unwrap();
            assert_eq!(parsed.text, "hello SDK Claude");
            assert_eq!(parsed.from_kind.as_deref(), Some("codex"));
            std::future::pending::<()>().await;
        });
        let provider = claude::sdk::from_io(
            BufReader::new(sdk_stdout),
            sdk_stdin,
            claude::sdk::QueryOptions {
                session_id: Some(session_id.to_string()),
                ..claude::sdk::QueryOptions::default()
            },
        )
        .await
        .unwrap();
        (provider, server, session_id)
    }

    #[derive(Default)]
    struct FailAfterFirstLine {
        first_line_written: bool,
    }

    impl AsyncWrite for FailAfterFirstLine {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.first_line_written {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "forced prompt failure",
                )));
            }
            if bytes.contains(&b'\n') {
                self.first_line_written = true;
            }
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn failed_prompt_session() -> (claude::sdk::Session, tokio::task::JoinHandle<()>, Uuid) {
        let session_id = Uuid::new_v4();
        let (server_stdout, sdk_stdout) = duplex(16 * 1024);
        let server = tokio::spawn(async move {
            let mut stdout = server_stdout;
            write_json_line(
                &mut stdout,
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": "req_0",
                        "response": {
                            "commands": [],
                            "agents": [],
                            "output_style": "default",
                            "available_output_styles": [],
                            "models": [],
                            "account": {}
                        }
                    }
                }),
            )
            .await;
            std::future::pending::<()>().await;
        });
        let provider = claude::sdk::from_io(
            BufReader::new(sdk_stdout),
            FailAfterFirstLine::default(),
            claude::sdk::QueryOptions {
                session_id: Some(session_id.to_string()),
                ..claude::sdk::QueryOptions::default()
            },
        )
        .await
        .unwrap();
        (provider, server, session_id)
    }

    #[tokio::test]
    async fn delivery_tracks_ready_and_exit_and_writes_recipient_row() {
        let (provider, server, agent_id) = session().await;
        let record = AgentRecord {
            id: agent_id,
            host_id: Uuid::new_v4(),
            name: Some("recipient".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudeSdkBackend::with_session(record, provider);
        let target = backend.delivery_target();
        assert!(matches!(
            target.liveness(),
            Ok(DeliveryLiveness::Pending(_))
        ));

        let Plane::Structured { log, .. } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
            panic!("Claude SDK plane must be structured");
        };
        let mut rows = log.subscribe().await.unwrap();
        let (event_tx, _event_rx) = mpsc::channel::<SessionEvent>(8);
        let ingest = backend.start(&event_tx).unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(2), rows.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready.payload["type"], "amux.claude_sdk.ready");
        let facts = rows.read().await.unwrap();
        assert_eq!(facts.payload["type"], "amux.claude_sdk.session_facts");
        assert!(matches!(target.liveness(), Ok(DeliveryLiveness::Live)));

        let envelope = envelope(agent_id);
        assert_eq!(target.deliver(&envelope).await.unwrap(), Delivery::Stream);
        let row = tokio::time::timeout(Duration::from_secs(2), rows.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.payload["type"], "amux.claude_sdk.message");
        assert_eq!(row.payload["delivery"], "stream");
        assert_eq!(
            row.payload["envelope"],
            serde_json::to_value(&envelope).unwrap()
        );

        server.abort();
        let _ = server.await;
        ingest.await.unwrap();
        assert!(matches!(
            target.liveness(),
            Err(DeliveryError::FailedPrecondition(message))
                if message == "Claude SDK session has exited"
        ));
    }

    #[tokio::test]
    async fn failed_prompt_does_not_write_recipient_row() {
        let (provider, server, agent_id) = failed_prompt_session().await;
        let record = AgentRecord {
            id: agent_id,
            host_id: Uuid::new_v4(),
            name: Some("recipient".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut backend = ClaudeSdkBackend::with_session(record, provider);
        let target = backend.delivery_target();
        let Plane::Structured { log, .. } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
            panic!("Claude SDK plane must be structured");
        };
        let mut rows = log.subscribe().await.unwrap();
        let (event_tx, _event_rx) = mpsc::channel::<SessionEvent>(8);
        let ingest = backend.start(&event_tx).unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(2), rows.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready.payload["type"], "amux.claude_sdk.ready");
        let facts = rows.read().await.unwrap();
        assert_eq!(facts.payload["type"], "amux.claude_sdk.session_facts");

        let error = target.deliver(&envelope(agent_id)).await.unwrap_err();
        assert!(
            matches!(&error, DeliveryError::Failed(message) if message.contains("forced prompt failure")),
            "delivery must report the provider write failure: {error}"
        );
        ingest.await.unwrap();
        assert!(
            rows.read().await.is_none(),
            "a rejected provider prompt must not produce a recipient message row"
        );

        server.abort();
        let _ = server.await;
    }
}
