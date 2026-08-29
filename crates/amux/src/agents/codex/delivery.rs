use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::backend::{CodexLive, CodexRuntime, update_attached};
use crate::agents::{
    AgentDeliveryTarget, Delivery, DeliveryError, DeliveryLiveness, StructuredLogSource,
};
use crate::envelope::{Envelope, Sender};

pub(super) struct CodexDeliveryTarget {
    runtime: Arc<Mutex<CodexRuntime>>,
    log_source: StructuredLogSource,
}

impl CodexDeliveryTarget {
    pub(super) fn new(runtime: Arc<Mutex<CodexRuntime>>, log_source: StructuredLogSource) -> Self {
        Self {
            runtime,
            log_source,
        }
    }

    fn live_and_active(&self) -> Result<(CodexLive, bool)> {
        let state = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let attached = state
            .attached
            .as_ref()
            .ok_or_else(|| anyhow!("Codex thread is not attached"))?;
        let live = attached
            .live
            .clone()
            .ok_or_else(|| anyhow!("Codex thread is read-only until reconnect succeeds"))?;
        Ok((live, attached.active_turn_id.is_some()))
    }

    async fn deliver_envelope(&self, envelope: &Envelope) -> Result<Delivery> {
        let text = crate::envelope::format(envelope);
        let (live, active) = self.live_and_active()?;
        let item = json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        });

        let delivery = match live.control.inject(vec![item]).await {
            Ok(()) if active => Delivery::InjectQueued,
            Ok(()) => {
                let turn_id = live.control.empty_turn().await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(turn_id);
                });
                Delivery::InjectStarted
            }
            Err(inject_error) => {
                tracing::warn!(
                    %inject_error,
                    envelope_id = %envelope.id,
                    "Codex message injection failed; starting a visible turn"
                );
                let turn_id = live.control.user_turn(codex::TurnInput::Text(text)).await?;
                update_attached(&self.runtime, |attached| {
                    attached.active_turn_id = Some(turn_id);
                });
                Delivery::TurnStarted
            }
        };

        self.log_source
            .write(codex_message_row(envelope, delivery))
            .await;
        Ok(delivery)
    }
}

#[async_trait]
impl AgentDeliveryTarget for CodexDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        let state = self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match state.attached.as_ref() {
            Some(attached) if attached.live.is_some() => Ok(DeliveryLiveness::Live),
            Some(_) => Ok(DeliveryLiveness::Pending(
                "Codex thread is read-only until reconnect succeeds".to_string(),
            )),
            None => Ok(DeliveryLiveness::Pending(
                "Codex thread is not attached".to_string(),
            )),
        }
    }

    async fn deliver(&self, envelope: &Envelope) -> std::result::Result<Delivery, DeliveryError> {
        self.deliver_envelope(envelope)
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))
    }
}

pub(super) fn codex_message_row(envelope: &Envelope, delivery: Delivery) -> Value {
    let (from, from_id) = match &envelope.from {
        Sender::Agent(agent) => (
            format!("{}/{}", agent.name, agent.host_id),
            Some(agent.agent_id),
        ),
        Sender::Human => ("human".to_string(), None),
    };
    json!({
        "type": "amux.codex_message",
        "id": envelope.id,
        "kind": envelope.kind,
        "from": from,
        "from_id": from_id,
        "context": envelope.context,
        "text": envelope.text,
        "delivery": delivery.carrier(),
    })
}
