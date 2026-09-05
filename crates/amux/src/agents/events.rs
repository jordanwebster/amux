use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::Agent;
use crate::protocol::wire as protocol_wire;

/// Routed agent-inventory stream events.
///
/// These are carried in protobuf stream items for
/// `AgentService.SubscribeAgentEvents` and `ClientService.SubscribeAgents`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SnapshotComplete,
    /// ClientService-only authority, attributed to the authenticated source host.
    HostInventory {
        host_id: Uuid,
        agent_ids: Vec<Uuid>,
    },
    AgentUp {
        agent: Agent,
    },
    AgentUpdated {
        agent: Agent,
    },
    AgentDown {
        agent_id: Uuid,
    },
}

impl AgentEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::HostInventory { .. } => "Agent::HostInventory",
            Self::SnapshotComplete => "Agent::SnapshotComplete",
            Self::AgentUp { .. } => "Agent::AgentUp",
            Self::AgentUpdated { .. } => "Agent::AgentUpdated",
            Self::AgentDown { .. } => "Agent::AgentDown",
        }
    }
}

pub(crate) fn agent_event_to_wire(
    event: &AgentEvent,
) -> Result<protocol_wire::SubscribeAgentEventsResponse, protocol_wire::EncodeError> {
    let event = match event {
        AgentEvent::HostInventory { .. } => {
            return Err(protocol_wire::EncodeError::Invalid(
                "host inventory authority belongs to ClientService".into(),
            ));
        }
        AgentEvent::SnapshotComplete => {
            protocol_wire::subscribe_agent_events_response::Event::SnapshotComplete(
                protocol_wire::SnapshotComplete {},
            )
        }
        AgentEvent::AgentUp { agent } => {
            protocol_wire::subscribe_agent_events_response::Event::AgentUp(protocol_wire::AgentUp {
                agent: Some(crate::agents::agent_to_wire(agent)?),
            })
        }
        AgentEvent::AgentUpdated { agent } => {
            protocol_wire::subscribe_agent_events_response::Event::AgentUpdated(
                protocol_wire::AgentUpdated {
                    agent: Some(crate::agents::agent_to_wire(agent)?),
                },
            )
        }
        AgentEvent::AgentDown { agent_id } => {
            protocol_wire::subscribe_agent_events_response::Event::AgentDown(
                protocol_wire::AgentDown {
                    agent_id: uuid_to_bytes(*agent_id),
                    reason: None,
                },
            )
        }
    };
    Ok(protocol_wire::SubscribeAgentEventsResponse { event: Some(event) })
}

pub(crate) fn agent_event_from_wire(
    event: protocol_wire::SubscribeAgentEventsResponse,
) -> Result<AgentEvent, protocol_wire::DecodeError> {
    let event = event
        .event
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("missing AgentEvent event".into()))?;
    match event {
        protocol_wire::subscribe_agent_events_response::Event::AgentUp(event) => {
            let agent = event.agent.ok_or_else(|| {
                protocol_wire::DecodeError::Invalid("missing AgentUp agent".into())
            })?;
            agent_up_from_wire(agent)
        }
        protocol_wire::subscribe_agent_events_response::Event::AgentDown(event) => {
            Ok(AgentEvent::AgentDown {
                agent_id: uuid_from_bytes("agent_id", event.agent_id)?,
            })
        }
        protocol_wire::subscribe_agent_events_response::Event::AgentUpdated(event) => {
            let agent = event.agent.ok_or_else(|| {
                protocol_wire::DecodeError::Invalid("missing AgentUpdated agent".into())
            })?;
            agent_updated_from_wire(agent)
        }
        protocol_wire::subscribe_agent_events_response::Event::SnapshotComplete(_) => {
            Ok(AgentEvent::SnapshotComplete)
        }
    }
}

fn agent_up_from_wire(
    agent: protocol_wire::Agent,
) -> Result<AgentEvent, protocol_wire::DecodeError> {
    let agent = crate::agents::agent_from_wire(agent)?;
    Ok(AgentEvent::AgentUp { agent })
}

fn agent_updated_from_wire(
    agent: protocol_wire::Agent,
) -> Result<AgentEvent, protocol_wire::DecodeError> {
    let agent = crate::agents::agent_from_wire(agent)?;
    Ok(AgentEvent::AgentUpdated { agent })
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, protocol_wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        protocol_wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn agent_event_to_wire_rejects_non_utf8_working_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let error = agent_event_to_wire(&AgentEvent::AgentUp {
            agent: Agent {
                id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
                name: Some("bad-path".to_string()),
                command: "claude".to_string(),
                working_dir: std::path::PathBuf::from(OsString::from_vec(vec![0xff])),
                kind: crate::agents::AgentKind::Claude {
                    driver: crate::agents::ClaudeDriver::Pty,
                },
                readonly: false,
                args: Vec::new(),
                created_at: chrono::Utc::now(),
                parent: None,
                working_on: None,
            },
        })
        .unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }
}
