use std::path::Path;
use std::sync::Arc;

use agent_runtime::test_support::{FixtureRowReader, fixture_runtime, register_sdk_fixture};
use anyhow::{Context as _, Result};
use host_api::LocalAgentHost;
use model::AgentParent;
use model::envelope::Envelope;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Exercises SDK carriers and lifecycle notifications against a real local registry.
pub struct ClaudeSdkA2aHarness {
    host: Arc<dyn LocalAgentHost>,
    outbound: mpsc::Receiver<Envelope>,
}

impl ClaudeSdkA2aHarness {
    pub async fn new(directory: &Path, host_id: Uuid) -> Result<Self> {
        let host = fixture_runtime(directory, host_id)?;
        let outbound = host.subscribe_outbound_envelopes().await;
        Ok(Self { host, outbound })
    }

    pub async fn register(
        &self,
        name: &str,
        parent: Option<AgentParent>,
        session: claude::sdk::Session,
    ) -> Result<SdkRecipientRows> {
        let mut rows = SdkRecipientRows(
            register_sdk_fixture(self.host.as_ref(), name, parent, session).await?,
        );
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
        self.host.agent(agent_id).await.is_ok()
    }

    pub async fn stop(self) {
        self.host.stop_all().await;
    }
}

pub struct SdkRecipientRows(FixtureRowReader);

impl SdkRecipientRows {
    pub async fn next(&mut self) -> Result<Value> {
        self.0.read_row().await.context("SDK recipient log closed")
    }
}
