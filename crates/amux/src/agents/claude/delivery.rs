use std::sync::atomic::Ordering;

use async_trait::async_trait;

use super::pty_backend::ClaudePtyBackend;
use crate::agents::{AgentDeliveryTarget, Delivery, DeliveryError, DeliveryLiveness};
use crate::envelope::{Envelope, format_cross_session};

pub(super) struct ClaudeDeliveryTarget {
    readonly: bool,
    control: Option<claude::pty::Control>,
    messaging: Option<super::pty_backend::MessagingCredentials>,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ClaudeDeliveryTarget {
    pub(super) fn new(backend: &ClaudePtyBackend) -> Self {
        let (readonly, control, messaging, ready) = backend.delivery_snapshot();
        Self {
            readonly,
            control,
            messaging,
            ready,
        }
    }
}

#[async_trait]
impl AgentDeliveryTarget for ClaudeDeliveryTarget {
    fn liveness(&self) -> std::result::Result<DeliveryLiveness, DeliveryError> {
        if self.readonly {
            return Err(DeliveryError::FailedPrecondition(
                "session is readonly and cannot receive messages".to_string(),
            ));
        }
        if !self.ready.load(Ordering::Acquire) {
            return Ok(DeliveryLiveness::Pending(
                "Claude session has not completed startup".to_string(),
            ));
        }
        if self.control.is_some() {
            Ok(DeliveryLiveness::Live)
        } else {
            Ok(DeliveryLiveness::Pending(
                "Claude delivery target is not ready".to_string(),
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
        let control = self.control.as_ref().expect("live delivery has control");
        let fallback = crate::envelope::format(envelope);
        let (text, carrier) = match (
            format_cross_session(envelope, "prompting"),
            self.messaging.as_ref(),
        ) {
            (Ok(text), Some(credentials)) => (
                text,
                claude::pty::Carrier::Socket {
                    path: credentials.socket_path.clone(),
                    token: credentials.token.clone(),
                    confirmation: envelope.id.to_string(),
                },
            ),
            _ => (fallback, claude::pty::Carrier::Pty),
        };
        let outcome = control
            .deliver(&text, carrier)
            .await
            .map_err(|error| DeliveryError::Failed(error.to_string()))?;
        Ok(match outcome {
            claude::pty::DeliveryOutcome::Socket => Delivery::Socket,
            claude::pty::DeliveryOutcome::Pty
            | claude::pty::DeliveryOutcome::PtyFallback { .. } => Delivery::Pty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentBackend, AgentParent, AgentType, ClaudeDriver, CreateAgentRequest};
    use crate::envelope::{EnvelopeKind, Sender};
    use std::path::PathBuf;
    use uuid::Uuid;

    #[tokio::test]
    async fn scripted_session_delivers_through_provider_control() {
        let id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id: id,
            host_id: None,
            name: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Pty,
            },
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let backend = ClaudePtyBackend::scripted(
            &request,
            PathBuf::from("/tmp"),
            crate::agents::claude::ClaudeVersionCache::default(),
            crate::agents::mcp_launch_route_for_tests(Uuid::new_v4()),
        );
        let envelope = crate::envelope::Envelope {
            id: Uuid::new_v4(),
            context: None,
            from: Sender::Human,
            to: AgentParent {
                agent_id: id,
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Message,
            text: "hello".to_string(),
        };
        assert_eq!(backend.deliver(&envelope).await.unwrap(), Delivery::Pty);
    }
}
