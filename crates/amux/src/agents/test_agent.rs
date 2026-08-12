//! Test agent session for E2E testing.
//!
//! Only available in debug/test builds. Spawns an arbitrary command
//! (typically `test-agent`) without Claude-specific environment or hooks.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use uuid::Uuid;

use super::{PtyHandle, spawn_pty_agent};
use crate::agents::{
    AGENT_TYPE_TEST_AGENT, AgentBackend, CreateAgentRequest, LocalAgentNameSource, StopPolicy,
    StructuredLogSource, TerminalSize, terminal_io_protocols,
};
#[cfg(test)]
use crate::agents::{MultiplexStructuredReader, SequencedReplayQuery};
use crate::debug::DebugView;

const STRUCTURED_LOG_RETENTION: usize = 1000;

#[cfg(any(test, feature = "testnet"))]
pub(crate) mod io {
    pub(crate) const TEST_ECHO_COMMAND: &str = "__amux_test_echo__";
    pub(crate) const TEST_ECHO_V1: &str = "test_echo_v1";
}

pub(crate) struct TestAgentSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<TerminalSize>,
    pub(crate) created_at: DateTime<Utc>,
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
            pty: None,
            log_source: None,
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
            pty: Some(PtyHandle::test_echo()),
            log_source: None,
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
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
            created_at,
        }
    }

    /// Spawn the test agent process. Returns an exit handle that completes
    /// when the process exits.
    pub(crate) fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        #[cfg(any(test, feature = "testnet"))]
        if self.command == io::TEST_ECHO_COMMAND {
            self.pty = Some(PtyHandle::test_echo());
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
        self.log_source = Some(log_source);
        Ok(tokio::spawn(async move {
            let _ = exit_handle.await;
            exit_log_source.close().await;
        }))
    }

    pub(crate) fn log_source(&self) -> Option<StructuredLogSource> {
        self.log_source.clone()
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

    fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        TestAgentSession::start(self)
    }

    async fn stop(&self, _policy: StopPolicy) {
        TestAgentSession::stop(self).await;
    }

    fn agent_type(&self) -> &'static str {
        AGENT_TYPE_TEST_AGENT
    }

    fn io_protocols(&self) -> Vec<String> {
        #[cfg(any(test, feature = "testnet"))]
        {
            let mut protocols = terminal_io_protocols(self.pty.as_ref());
            protocols.push(io::TEST_ECHO_V1.to_string());
            protocols
        }
        #[cfg(not(any(test, feature = "testnet")))]
        {
            terminal_io_protocols(self.pty.as_ref())
        }
    }

    fn log_source(&self) -> Option<StructuredLogSource> {
        TestAgentSession::log_source(self)
    }

    fn pty_handle(&self) -> Option<&PtyHandle> {
        self.pty.as_ref()
    }

    fn suspended_state(&self) -> Result<crate::suspend::SuspendedAgent> {
        Ok(crate::suspend::SuspendedAgent::TestAgent {
            agent_id: self.agent_id,
            name: self.name.clone(),
            command: self.command.clone(),
            working_dir: self.working_dir.clone(),
            terminal_size: self.terminal_size,
            created_at: self.created_at,
        })
    }

    fn debug_json(&self, verbose: bool) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(DebugView::new(self, verbose))
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
            pty: None,
            log_source: Some(StructuredLogSource::new(STRUCTURED_LOG_RETENTION)),
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
}
