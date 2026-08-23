use std::path::Path;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::ClaudeSession;
use super::inbox::ClaudeDeliveryTarget;
use crate::agents::claude::io;
use crate::agents::{
    AGENT_TYPE_CLAUDE, AgentBackend, AgentDeliveryTarget, AgentParent, HookEnvironment, HookError,
    HookOutcome, LocalAgentNameSource, PtyHandle, SessionEvent, SpawnInheritance, StopPolicy,
    StructuredInput, StructuredLogSource, terminal_io_protocols,
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

    fn start(
        &mut self,
        _event_tx: &mpsc::Sender<SessionEvent>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        ClaudeSession::start(self)
    }

    async fn stop(&self, policy: StopPolicy) {
        ClaudeSession::stop(self, policy).await;
    }

    fn agent_type(&self) -> &'static str {
        AGENT_TYPE_CLAUDE
    }

    fn spawn_inheritance(&self) -> SpawnInheritance {
        SpawnInheritance {
            claude_permission_args: crate::agent_tools::claude_permission_args(&self.args),
            ..SpawnInheritance::default()
        }
    }

    fn parent(&self) -> Option<AgentParent> {
        self.parent
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

    fn delivery_target(&self) -> Box<dyn AgentDeliveryTarget> {
        Box::new(ClaudeDeliveryTarget::new(self))
    }

    fn structured_input(&self) -> Option<Box<dyn StructuredInput>> {
        Some(Box::new(self.structured_input_target()))
    }

    async fn handle_hook_payload(
        &mut self,
        payload: &[u8],
        env: &HookEnvironment,
    ) -> std::result::Result<HookOutcome, HookError> {
        ClaudeSession::handle_hook_payload(self, payload, env).await
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
            parent: self.parent,
            working_on: None,
        })
    }

    fn debug_json(&self, verbose: bool) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(DebugView::new(self, verbose))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::agents::{
        AgentParent, AgentType, CreateAgentRequest, Delivery, DeliveryLiveness, HookEnvironment,
    };
    use crate::envelope::{Envelope, EnvelopeKind, Sender};

    #[tokio::test]
    async fn a2a_pty_carrier_delivers_the_tagged_envelope_program() {
        let recipient_id = Uuid::new_v4();
        let pty = PtyHandle::test_echo();
        let mut output = pty.subscribe_with_query(None).await.unwrap();
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
            crate::agents::claude::ClaudeVersionCache::default(),
        );
        session.pty = Some(pty);
        session
            .delivery_ready
            .store(true, std::sync::atomic::Ordering::Release);
        let envelope = Envelope {
            id: Uuid::new_v4(),
            context: None,
            from: Sender::Human,
            to: AgentParent {
                agent_id: recipient_id,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello from the fleet".to_string(),
        };

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), session.deliver(&envelope))
                .await
                .unwrap()
                .unwrap(),
            Delivery::Pty
        );
        assert_eq!(
            output.read().await.unwrap(),
            format!("\x1b[200~{}\x1b[201~", crate::envelope::format(&envelope)).into_bytes()
        );
        assert_eq!(output.read().await.unwrap(), b"\r");
    }

    #[tokio::test]
    async fn transcript_observation_without_session_start_enables_pty_delivery() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        tokio::fs::write(&transcript_path, "").await.unwrap();

        let recipient_id = Uuid::new_v4();
        let pty = PtyHandle::test_echo();
        let mut output = pty.subscribe_with_query(None).await.unwrap();
        let mut session = ClaudeSession::new(
            &CreateAgentRequest {
                agent_id: recipient_id,
                host_id: None,
                name: Some("recipient".to_string()),
                agent_type: AgentType::Claude,
                working_dir: dir.path().to_path_buf(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            },
            dir.path().to_path_buf(),
            crate::agents::claude::ClaudeVersionCache::default(),
        );
        session.pty = Some(pty);
        session.transcript_ingest = Some(
            crate::agents::claude::transcript_ingest::TranscriptIngest::with_delivery_ready(
                StructuredLogSource::new(32),
                session.delivery_ready.clone(),
                crate::agents::claude::ClaudeVersionCache::default(),
            ),
        );
        let target = session.delivery_target();

        let payload = serde_json::to_vec(&json!({
            "hook_event_name": "Notification",
            "session_id": Uuid::new_v4(),
            "transcript_path": transcript_path,
            "cwd": dir.path(),
            "message": "transcript observed",
        }))
        .unwrap();
        session
            .handle_hook_payload(&payload, &HookEnvironment::new())
            .await
            .unwrap();

        target
            .wait_until_live(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(target.liveness().unwrap(), DeliveryLiveness::Live));

        let envelope = Envelope {
            id: Uuid::new_v4(),
            context: None,
            from: Sender::Human,
            to: AgentParent {
                agent_id: recipient_id,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello without SessionStart".to_string(),
        };
        assert_eq!(target.deliver(&envelope).await.unwrap(), Delivery::Pty);
        assert_eq!(
            output.read().await.unwrap(),
            format!("\x1b[200~{}\x1b[201~", crate::envelope::format(&envelope)).into_bytes()
        );
        assert_eq!(output.read().await.unwrap(), b"\r");
    }
}
