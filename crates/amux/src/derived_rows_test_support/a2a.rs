use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use chrono::{TimeZone as _, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agents::{
    AgentKind, AgentParent, AgentRecord, ClaudeDriver, McpLaunchRoute, MultiplexStructuredReader,
};
use crate::envelope::Envelope;
use crate::services::{LocalAgentHost, PtyAgentHost};

/// Exercises SDK carriers and lifecycle notifications against a real local registry.
pub struct ClaudeSdkA2aHarness {
    host: Arc<PtyAgentHost>,
    outbound: mpsc::Receiver<Envelope>,
}

impl ClaudeSdkA2aHarness {
    pub async fn new(directory: &Path, host_id: Uuid) -> Result<Self> {
        std::fs::create_dir_all(directory.join("data"))?;
        let route = McpLaunchRoute::new(
            std::env::current_exe()?,
            None,
            directory.join("control.sock"),
            host_id,
        )?;
        let host = PtyAgentHost::new_with_mcp_launch_route(
            route,
            directory.join("keymap"),
            directory.join("data"),
            Vec::new(),
        )?;
        let outbound = host.subscribe_outbound_envelopes().await;
        Ok(Self { host, outbound })
    }

    pub async fn register(
        &self,
        name: &str,
        parent: Option<AgentParent>,
        session: claude::sdk::Session,
    ) -> Result<SdkRecipientRows> {
        let record = AgentRecord {
            id: session.control.session_id().parse()?,
            host_id: self.host.host_id(),
            name: Some(name.to_string()),
            command: "claude".to_string(),
            working_dir: "<MACHINE_PATH>".into(),
            kind: AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc.timestamp_opt(0, 0).single().expect("Unix epoch exists"),
            parent,
            working_on: None,
        };
        let mut rows = SdkRecipientRows(self.host.register_sdk_fixture(record, session).await?);
        assert_eq!(rows.next().await?["type"], "amux.claude_sdk.ready");
        assert_eq!(rows.next().await?["type"], "amux.claude_sdk.session_facts");
        Ok(rows)
    }

    pub async fn deliver(&self, envelope: Value) -> Result<()> {
        self.host
            .send_message(serde_json::from_value(envelope)?)
            .await?;
        Ok(())
    }

    pub async fn next_envelope(&mut self) -> Result<Value> {
        let envelope = self
            .outbound
            .recv()
            .await
            .context("lifecycle stream closed")?;
        Ok(serde_json::to_value(envelope)?)
    }

    pub async fn contains(&self, agent_id: Uuid) -> bool {
        self.host
            .state()
            .read()
            .await
            .local_agents
            .contains_key(&agent_id)
    }

    pub async fn stop(self) {
        self.host.stop_all().await;
    }
}

pub struct SdkRecipientRows(MultiplexStructuredReader);

impl SdkRecipientRows {
    pub async fn next(&mut self) -> Result<Value> {
        Ok(self
            .0
            .read()
            .await
            .context("SDK recipient log closed")?
            .payload)
    }
}
