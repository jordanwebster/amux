//! Test agent session for E2E testing.
//!
//! Only available in debug/test builds. Spawns an arbitrary command
//! (typically `test-agent`) without Claude-specific environment or hooks.

use super::{PtyHandle, spawn_pty_agent};
use crate::agent::StructuredLogSource;
use crate::buffer::MultiplexStructuredReader;
use crate::debug::DebugView;
use crate::protocol::message::{CreateAgentRequest, SubscribeQuery};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::path::PathBuf;
use uuid::Uuid;

pub(crate) struct TestAgentSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<crate::protocol::message::TerminalSize>,
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
        let (pty, log_source, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &[],
            &self.working_dir,
            &[],
            self.terminal_size,
        )?;
        self.pty = Some(pty);
        self.log_source = Some(log_source);
        Ok(exit_handle)
    }

    #[cfg(test)]
    pub(crate) fn log_source(&self) -> Option<StructuredLogSource> {
        self.log_source.clone()
    }

    /// Subscribe to structured log output with an optional query filter
    /// and return the matching seq.
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SubscribeQuery>,
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

impl Serialize for DebugView<'_, TestAgentSession> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let session = self.inner;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", "test_agent")?;
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

    #[tokio::test]
    async fn structured_subscribe_does_not_wait_for_transcript_linking() {
        let session = TestAgentSession {
            agent_id: Uuid::new_v4(),
            name: None,
            command: "test-agent".to_string(),
            working_dir: std::env::temp_dir(),
            pty: None,
            log_source: Some(StructuredLogSource::new()),
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
