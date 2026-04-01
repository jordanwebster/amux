//! Test agent session for E2E testing.
//!
//! Only available in debug/test builds. Spawns an arbitrary command
//! (typically `test-agent`) without Claude-specific environment or hooks.

use super::{PtyHandle, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::error::Result;
use crate::message::CreateAgentRequest;
use std::path::PathBuf;
use uuid::Uuid;

pub struct TestAgentSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<crate::message::TerminalSize>,
}

impl TestAgentSession {
    /// Create a new TestAgentSession.
    /// Does not spawn the process — call [`start`] afterwards.
    pub fn new(req: &CreateAgentRequest, cmd: String) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: cmd,
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
        }
    }

    /// Spawn the test agent process. Returns an exit handle that completes
    /// when the process exits.
    pub fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
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

    /// Subscribe to structured log output and return the matching seq.
    pub async fn subscribe_with_current_seq(&self) -> Option<(MultiplexStructuredReader, u64)> {
        self.log_source.as_ref()?.subscribe_with_current_seq().await
    }

    /// Shut down the session: close PTY and log source.
    pub async fn stop(&self) {
        tracing::info!(agent_id = %self.agent_id, "shutting down test agent session");
        if let Some(pty) = &self.pty {
            pty.close().await;
        }
        if let Some(log_source) = &self.log_source {
            log_source.close().await;
        }
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
        };

        let (_reader, seq) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            session.subscribe_with_current_seq(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seq, 0);
    }
}
