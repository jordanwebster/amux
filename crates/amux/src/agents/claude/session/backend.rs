use std::path::Path;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::ClaudeSession;
use crate::agents::claude::io;
use crate::agents::{
    AGENT_TYPE_CLAUDE, AgentBackend, HookError, HookOutcome, LocalAgentNameSource, PtyHandle,
    SessionEvent, StopPolicy, StructuredInput, StructuredLogSource, terminal_io_protocols,
};
use crate::debug::DebugView;
use crate::suspend::SuspendedAgent;

#[async_trait]
impl AgentBackend for ClaudeSession {
    fn agent_id(&self) -> Uuid {
        self.agent_id
    }

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn set_local_name(&mut self, name: Option<String>, source: LocalAgentNameSource) {
        self.set_name_and_source(name, source);
    }

    fn command(&self) -> &str {
        &self.command
    }

    fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn readonly(&self) -> bool {
        self.readonly
    }

    fn args(&self) -> &[String] {
        &self.args
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        ClaudeSession::start(self)
    }

    async fn stop(&self, policy: StopPolicy) {
        ClaudeSession::stop(self, policy).await;
    }

    fn agent_type(&self) -> &'static str {
        AGENT_TYPE_CLAUDE
    }

    fn io_protocols(&self) -> Vec<String> {
        let mut protocols = terminal_io_protocols(self.pty.as_ref());
        protocols.push(io::PTY_TRANSCRIPT_V1.to_string());
        protocols
    }

    fn log_source(&self) -> Option<StructuredLogSource> {
        ClaudeSession::log_source(self)
    }

    fn pty_handle(&self) -> Result<Option<PtyHandle>> {
        Ok(self.pty.clone())
    }

    fn structured_input(&self) -> Option<Box<dyn StructuredInput>> {
        Some(Box::new(self.structured_input_target()))
    }

    async fn handle_hook_payload(
        &mut self,
        payload: &[u8],
    ) -> std::result::Result<HookOutcome, HookError> {
        ClaudeSession::handle_hook_payload(self, payload).await
    }

    fn maybe_start_name_sniffer(&mut self, event_tx: &mpsc::Sender<SessionEvent>) {
        ClaudeSession::maybe_start_name_sniffer(self, event_tx);
    }

    fn local_name_source(&self) -> Option<LocalAgentNameSource> {
        Some(self.name_source())
    }

    fn suspended_state(&self) -> Result<SuspendedAgent> {
        let name_source = self.name_source();
        let session_id = self.session_id.ok_or_else(|| {
            anyhow!(
                "cannot suspend claude agent {}: no session_id (SessionStart hook not received)",
                self.agent_id
            )
        })?;
        Ok(SuspendedAgent::Claude {
            agent_id: self.agent_id,
            name: self.name.clone(),
            name_source: name_source.into(),
            working_dir: self.working_dir.clone(),
            terminal_size: self.terminal_size,
            created_at: self.created_at,
            args: self.args.clone(),
            session_id,
        })
    }

    fn debug_json(&self, verbose: bool) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(DebugView::new(self, verbose))
    }
}
