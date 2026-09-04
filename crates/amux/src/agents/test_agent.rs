//! Test agent session for E2E testing.
//!
//! Only available in debug/test builds. Spawns an arbitrary command
//! (typically `test-agent`) without Claude-specific environment or hooks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, testnet))]
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use uuid::Uuid;

use super::{PtyHandle, spawn_pty_agent};
use crate::agents::{
    AgentBackend, AgentDeliveryTarget, AgentKind, AgentParent, BackendState, CreateAgentRequest,
    Delivery, DeliveryError, DeliveryLiveness, LocalAgentNameSource, Plane, Protocol, RawPtyTarget,
    SessionDebug, SessionEvent, StopPolicy, StructuredLogSource, TerminalSize,
};
#[cfg(test)]
use crate::agents::{MultiplexStructuredReader, SequencedReplayQuery};
use crate::debug::DebugView;
use crate::protocol::ProtocolError;

const STRUCTURED_LOG_RETENTION: usize = 1000;

#[cfg(any(test, testnet))]
pub(crate) mod io {
    pub(crate) const TEST_ECHO_COMMAND: &str = "__amux_test_echo__";
    pub(crate) const TEST_DELAYED_DELIVERY_COMMAND: &str = "__amux_test_delayed_delivery__";
    pub(crate) const TEST_FAILED_DELIVERY_COMMAND: &str = "__amux_test_failed_delivery__";
    pub(crate) const TEST_UNAVAILABLE_DELIVERY_COMMAND: &str = "__amux_test_unavailable_delivery__";
    pub(crate) const TEST_ECHO_V1: &str = "test_echo_v1";
}

pub(crate) struct TestAgentSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) parent: Option<AgentParent>,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,
    delivery_ready: Arc<AtomicBool>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<TerminalSize>,
    pub(crate) created_at: DateTime<Utc>,
}

struct TestAgentDeliveryTarget {
    pty: Option<PtyHandle>,
    ready: Arc<AtomicBool>,
}

#[async_trait]
impl AgentDeliveryTarget for TestAgentDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        if self.ready.load(Ordering::Acquire) {
            Ok(DeliveryLiveness::Live)
        } else {
            Ok(DeliveryLiveness::Pending(
                "test agent delivery target is not ready".to_string(),
            ))
        }
    }

    async fn deliver(
        &self,
        envelope: &crate::envelope::Envelope,
    ) -> std::result::Result<Delivery, DeliveryError> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(DeliveryError::FailedPrecondition(
                "test agent delivery target is not ready".to_string(),
            ));
        }
        let pty = self
            .pty
            .as_ref()
            .ok_or_else(|| DeliveryError::Failed("test agent PTY is unavailable".to_string()))?;
        pty.send_input(crate::envelope::format(envelope).into_bytes())
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))?;
        Ok(Delivery::Pty)
    }
}

impl TestAgentSession {
    /// Create a new TestAgentSession.
    /// Does not spawn the process — call [`start`] afterwards.
    pub(crate) fn new(req: &CreateAgentRequest, cmd: String) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: cmd,
            working_dir: req.working_dir.clone(),
            parent: req.parent,
            pty: None,
            log_source: None,
            delivery_ready: Arc::new(AtomicBool::new(false)),
            terminal_size: req.terminal_size,
            created_at: Utc::now(),
        }
    }

    #[cfg(test)]
    pub(crate) fn echo_for_tests(agent_id: Uuid, name: Option<String>) -> Self {
        Self {
            agent_id,
            name,
            command: "test-echo".to_string(),
            working_dir: std::env::temp_dir(),
            parent: None,
            pty: Some(PtyHandle::test_echo()),
            log_source: None,
            delivery_ready: Arc::new(AtomicBool::new(true)),
            terminal_size: None,
            created_at: Utc::now(),
        }
    }

    pub(super) fn from_suspended(
        req: &CreateAgentRequest,
        cmd: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: cmd,
            working_dir: req.working_dir.clone(),
            parent: req.parent,
            pty: None,
            log_source: None,
            delivery_ready: Arc::new(AtomicBool::new(false)),
            terminal_size: req.terminal_size,
            created_at,
        }
    }

    /// Spawn the test agent process. Returns an exit handle that completes
    /// when the process exits.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        #[cfg(any(test, testnet))]
        if self.command == io::TEST_ECHO_COMMAND {
            self.pty = Some(PtyHandle::test_echo());
            self.log_source = Some(StructuredLogSource::new(STRUCTURED_LOG_RETENTION));
            self.delivery_ready.store(true, Ordering::Release);
            return Ok(tokio::spawn(std::future::pending::<()>()));
        }
        #[cfg(any(test, testnet))]
        if self.command == io::TEST_DELAYED_DELIVERY_COMMAND {
            self.pty = Some(PtyHandle::test_echo());
            let ready = self.delivery_ready.clone();
            return Ok(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                ready.store(true, Ordering::Release);
                std::future::pending::<()>().await;
            }));
        }
        #[cfg(any(test, testnet))]
        if self.command == io::TEST_UNAVAILABLE_DELIVERY_COMMAND {
            return Ok(tokio::spawn(std::future::pending::<()>()));
        }
        #[cfg(any(test, testnet))]
        if self.command == io::TEST_FAILED_DELIVERY_COMMAND {
            self.delivery_ready.store(true, Ordering::Release);
            return Ok(tokio::spawn(std::future::pending::<()>()));
        }

        let (pty, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &[],
            &self.working_dir,
            &[],
            &[],
            self.terminal_size,
        )?;
        let log_source = StructuredLogSource::new(STRUCTURED_LOG_RETENTION);
        let exit_log_source = log_source.clone();
        self.pty = Some(pty);
        self.delivery_ready.store(true, Ordering::Release);
        self.log_source = Some(log_source);
        Ok(tokio::spawn(async move {
            let _ = exit_handle.await;
            exit_log_source.close().await;
        }))
    }

    /// Subscribe to structured log output with an optional query filter
    /// and return the matching seq.
    #[cfg(test)]
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SequencedReplayQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        self.log_source.as_ref()?.subscribe_with_query(query).await
    }

    /// Shut down the session: close PTY and log source.
    pub(crate) async fn stop(&self) {
        tracing::info!(agent_id = %self.agent_id, "shutting down test agent session");
        if let Some(pty) = &self.pty {
            pty.close().await;
        }
        if let Some(log_source) = &self.log_source {
            log_source.close().await;
        }
    }
}

#[async_trait]
impl AgentBackend for TestAgentSession {
    fn agent_id(&self) -> Uuid {
        self.agent_id
    }

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn set_local_name(&mut self, name: Option<String>, _source: LocalAgentNameSource) {
        self.name = name;
    }

    fn command(&self) -> &str {
        &self.command
    }

    fn working_dir(&self) -> &std::path::Path {
        &self.working_dir
    }

    fn readonly(&self) -> bool {
        false
    }

    fn args(&self) -> &[String] {
        &[]
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn start(
        &mut self,
        _event_tx: &tokio::sync::mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        TestAgentSession::start(self)
    }

    async fn stop(&self, _policy: StopPolicy) {
        TestAgentSession::stop(self).await;
    }

    fn kind(&self) -> AgentKind {
        AgentKind::TestAgent
    }

    fn plane(&self, protocol: Protocol) -> std::result::Result<Plane, ProtocolError> {
        match protocol {
            Protocol::TerminalV1 | Protocol::TestEchoV1 => self
                .pty
                .clone()
                .map(RawPtyTarget::Existing)
                .map(Plane::Terminal)
                .ok_or_else(|| ProtocolError::FailedPrecondition {
                    message: "test-agent PTY is not active".to_string(),
                }),
            Protocol::ClaudePtyTranscriptV1 | Protocol::ClaudeSdkV1 | Protocol::CodexSdkV1 => {
                Err(ProtocolError::NotExposed {
                    kind: self.kind(),
                    protocol,
                })
            }
        }
    }

    fn attachment_log(&self) -> Option<StructuredLogSource> {
        self.log_source.clone()
    }

    fn parent(&self) -> Option<AgentParent> {
        self.parent
    }

    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(TestAgentDeliveryTarget {
            pty: self.pty.clone(),
            ready: self.delivery_ready.clone(),
        })
    }

    fn suspended_state(&self) -> Result<crate::suspend::SuspendedAgent> {
        Ok(crate::suspend::SuspendedAgent::TestAgent {
            agent_id: self.agent_id,
            name: self.name.clone(),
            command: self.command.clone(),
            working_dir: self.working_dir.clone(),
            terminal_size: self.terminal_size,
            created_at: self.created_at,
            parent: self.parent,
            working_on: None,
        })
    }

    async fn debug_json(&self, verbose: bool) -> serde_json::Result<serde_json::Value> {
        let pty_output = match &self.pty {
            Some(pty) => Some(pty.debug_output().await),
            None => None,
        };
        let log_output = match &self.log_source {
            Some(log) => Some(log.debug_snapshot().await),
            None => None,
        };
        let subscriber_count = pty_output
            .iter()
            .chain(log_output.iter())
            .map(|output| output.subscriber_count)
            .sum();
        let backend = match (&self.pty, &pty_output) {
            (Some(pty), Some(output)) => pty.backend_state(output),
            _ if self.delivery_ready.load(Ordering::Acquire) => BackendState::Running { pid: None },
            _ => BackendState::Starting,
        };
        let session = SessionDebug::new(
            pty_output.as_ref().or(log_output.as_ref()),
            subscriber_count,
            backend,
            Vec::new(),
        );
        let mut value = serde_json::to_value(DebugView::new(self, verbose))?;
        value
            .as_object_mut()
            .expect("test-agent debug view is an object")
            .insert("session".to_string(), serde_json::to_value(session)?);
        Ok(value)
    }
}

impl Serialize for DebugView<'_, TestAgentSession> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let session = self.inner;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "test_agent")?;
        map.serialize_entry("has_pty", &session.pty.is_some())?;
        map.serialize_entry("has_structured_log", &session.log_source.is_some())?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn structured_subscribe_does_not_wait_for_transcript_linking() {
        let session = TestAgentSession {
            agent_id: Uuid::new_v4(),
            name: None,
            command: "test-agent".to_string(),
            working_dir: std::env::temp_dir(),
            parent: None,
            pty: None,
            log_source: Some(StructuredLogSource::new(STRUCTURED_LOG_RETENTION)),
            delivery_ready: Arc::new(AtomicBool::new(false)),
            terminal_size: None,
            created_at: Utc::now(),
        };

        let (_reader, seq) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            session.subscribe_with_query(None),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn debug_json_reports_live_subscriber_and_moving_buffer_tail() {
        let session = TestAgentSession::echo_for_tests(Uuid::new_v4(), Some("echo".to_string()));
        let pty = session.pty.as_ref().unwrap();
        let mut reader = pty.subscribe_with_query(None).await.unwrap();

        let before = session.debug_json(true).await.unwrap();
        assert_eq!(before["session"]["subscriber_count"], 1);
        assert_eq!(before["session"]["buffer"]["tail_seq"], 0);

        pty.send_input(b"hello".to_vec()).await.unwrap();
        assert_eq!(reader.read().await.unwrap(), b"hello");

        let after = session.debug_json(true).await.unwrap();
        assert_eq!(after["session"]["subscriber_count"], 1);
        assert_eq!(after["session"]["buffer"]["head_seq"], 0);
        assert_eq!(after["session"]["buffer"]["tail_seq"], 5);
        assert_eq!(after["session"]["buffer"]["bytes"], 5);
        assert_eq!(after["session"]["backend"]["state"], "running");
        assert!(after["session"]["backend"]["pid"].is_null());
        assert_eq!(after["session"]["obligations"], serde_json::json!([]));
    }
}
