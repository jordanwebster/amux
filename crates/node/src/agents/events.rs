use model::AgentEvent;
use uuid::Uuid;
use wire as protocol_wire;

pub(crate) fn agent_event_to_wire(
    event: &AgentEvent,
) -> Result<protocol_wire::SubscribeAgentEventsResponse, protocol_wire::EncodeError> {
    let event = match event {
        AgentEvent::HostInventory {
            host_id,
            agents,
            through_revision,
        } => protocol_wire::subscribe_agent_events_response::Event::HostInventory(
            protocol_wire::HostInventory {
                host_id: uuid_to_bytes(*host_id),
                agents: agents
                    .iter()
                    .map(crate::agents::agent_to_wire)
                    .collect::<Result<_, _>>()?,
                through_revision: *through_revision,
            },
        ),
        AgentEvent::SnapshotComplete {
            host_id,
            through_revision,
        } => protocol_wire::subscribe_agent_events_response::Event::SnapshotComplete(
            protocol_wire::SnapshotComplete {
                host_id: uuid_to_bytes(*host_id),
                through_revision: *through_revision,
            },
        ),
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
        AgentEvent::AgentDown {
            host_id,
            agent_id,
            inventory_revision,
        } => protocol_wire::subscribe_agent_events_response::Event::AgentDown(
            protocol_wire::AgentDown {
                host_id: uuid_to_bytes(*host_id),
                agent_id: uuid_to_bytes(*agent_id),
                reason: None,
                inventory_revision: *inventory_revision,
            },
        ),
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
                host_id: uuid_from_bytes("host_id", event.host_id)?,
                agent_id: uuid_from_bytes("agent_id", event.agent_id)?,
                inventory_revision: event.inventory_revision,
            })
        }
        protocol_wire::subscribe_agent_events_response::Event::AgentUpdated(event) => {
            let agent = event.agent.ok_or_else(|| {
                protocol_wire::DecodeError::Invalid("missing AgentUpdated agent".into())
            })?;
            agent_updated_from_wire(agent)
        }
        protocol_wire::subscribe_agent_events_response::Event::HostInventory(event) => {
            Ok(AgentEvent::HostInventory {
                host_id: uuid_from_bytes("host_id", event.host_id)?,
                agents: event
                    .agents
                    .into_iter()
                    .map(crate::agents::agent_from_wire)
                    .collect::<Result<_, _>>()?,
                through_revision: event.through_revision,
            })
        }
        protocol_wire::subscribe_agent_events_response::Event::SnapshotComplete(event) => {
            Ok(AgentEvent::SnapshotComplete {
                host_id: uuid_from_bytes("host_id", event.host_id)?,
                through_revision: event.through_revision,
            })
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
    use model::Agent;

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
                    driver: model::ClaudeDriver::Pty,
                },
                readonly: false,
                args: Vec::new(),
                created_at: chrono::Utc::now(),
                parent: None,
                working_on: None,
                inventory_revision: 1,
            },
        })
        .unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }
}
