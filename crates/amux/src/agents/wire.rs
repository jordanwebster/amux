use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
#[cfg(test)]
use prost::Message as ProstMessage;
use protocol_wire::DeleteAgentRequest;
use uuid::Uuid;

use super::{Agent, SessionCloseReason, SubscribeSessionEvent};
use crate::agents::{RenameAgentRequest, TerminalSize};
use crate::protocol::wire::{self as protocol_wire, pb};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionInputEvent {
    Input { input_id: Vec<u8>, payload: Vec<u8> },
    Control { payload: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscribeSessionRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) io_protocol: String,
    pub(crate) args: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendInputRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) io_protocol: String,
    pub(crate) event: SessionInputEvent,
}

#[derive(Debug, Clone)]
pub(crate) struct CreateAgentRpcRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) agent: CreateAgentConfig,
}

#[derive(Debug, Clone)]
pub(crate) enum CreateAgentConfig {
    ClaudePty {
        working_dir: PathBuf,
        args: Vec<String>,
        terminal_size: Option<TerminalSize>,
    },
    TestAgent {
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
    },
}

#[cfg(test)]
pub(crate) fn encode_session_output_event_payload(event: &SubscribeSessionEvent) -> Vec<u8> {
    session_output_event_to_wire(event).encode_to_vec()
}

#[cfg(test)]
pub(crate) fn decode_session_output_event_payload(
    payload: &[u8],
) -> Result<SubscribeSessionEvent, protocol_wire::DecodeError> {
    let event = pb::SubscribeSessionResponse::decode(payload)?;
    session_output_event_from_wire(event)
}

pub(crate) fn subscribe_session_request_from_wire(
    request: pb::SubscribeSessionRequest,
) -> Result<SubscribeSessionRequest, protocol_wire::DecodeError> {
    Ok(SubscribeSessionRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        io_protocol: request.io_protocol,
        args: request.args,
    })
}

pub(crate) fn send_input_request_from_wire(
    request: pb::SendInputRequest,
) -> Result<SendInputRequest, protocol_wire::DecodeError> {
    let event = request.event.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SendInputRequest missing event".into())
    })?;
    let event = match event {
        pb::send_input_request::Event::Input(input) => SessionInputEvent::Input {
            input_id: input.input_id,
            payload: input.payload,
        },
        pb::send_input_request::Event::Control(control) => SessionInputEvent::Control {
            payload: control.payload,
        },
    };
    Ok(SendInputRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        io_protocol: request.io_protocol,
        event,
    })
}

pub(crate) fn session_output_event_to_wire(
    event: &SubscribeSessionEvent,
) -> pb::SubscribeSessionResponse {
    let event = match event {
        SubscribeSessionEvent::Opened => {
            pb::subscribe_session_response::Event::Opened(pb::SessionOpened {})
        }
        SubscribeSessionEvent::Output { payload } => {
            pb::subscribe_session_response::Event::Output(pb::SessionOutput {
                payload: payload.clone(),
            })
        }
        SubscribeSessionEvent::ReplayComplete { cursor } => {
            pb::subscribe_session_response::Event::ReplayComplete(pb::ReplayComplete {
                cursor: cursor.clone(),
            })
        }
        SubscribeSessionEvent::Closed { reason } => {
            pb::subscribe_session_response::Event::Closed(session_closed_to_wire(reason))
        }
    };
    pb::SubscribeSessionResponse { event: Some(event) }
}

#[cfg(test)]
fn session_output_event_from_wire(
    event: pb::SubscribeSessionResponse,
) -> Result<SubscribeSessionEvent, protocol_wire::DecodeError> {
    let event = event.event.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SubscribeSessionResponse missing event".into())
    })?;
    match event {
        pb::subscribe_session_response::Event::Opened(_) => Ok(SubscribeSessionEvent::Opened),
        pb::subscribe_session_response::Event::Output(output) => {
            Ok(SubscribeSessionEvent::Output {
                payload: output.payload,
            })
        }
        pb::subscribe_session_response::Event::ReplayComplete(replay_complete) => {
            Ok(SubscribeSessionEvent::ReplayComplete {
                cursor: replay_complete.cursor,
            })
        }
        pb::subscribe_session_response::Event::Closed(closed) => {
            Ok(SubscribeSessionEvent::Closed {
                reason: session_closed_from_wire(closed)?,
            })
        }
    }
}

fn session_closed_to_wire(reason: &SessionCloseReason) -> pb::SessionClosed {
    let reason = match reason {
        SessionCloseReason::AgentDeleted => {
            pb::session_closed::Reason::AgentDeleted(pb::AgentDeleted {})
        }
        SessionCloseReason::AgentExited { exit_code } => {
            pb::session_closed::Reason::AgentExited(pb::AgentExited {
                exit_code: *exit_code,
            })
        }
        SessionCloseReason::HostUnreachable => {
            pb::session_closed::Reason::HostUnreachable(pb::HostUnreachable {})
        }
        SessionCloseReason::InternalError { detail } => {
            pb::session_closed::Reason::InternalError(pb::InternalError {
                detail: detail.clone(),
            })
        }
    };
    pb::SessionClosed {
        reason: Some(reason),
    }
}

#[cfg(test)]
fn session_closed_from_wire(
    closed: pb::SessionClosed,
) -> Result<SessionCloseReason, protocol_wire::DecodeError> {
    let reason = closed.reason.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SessionClosed missing reason".into())
    })?;
    Ok(match reason {
        pb::session_closed::Reason::AgentDeleted(_) => SessionCloseReason::AgentDeleted,
        pb::session_closed::Reason::AgentExited(exited) => SessionCloseReason::AgentExited {
            exit_code: exited.exit_code,
        },
        pb::session_closed::Reason::HostUnreachable(_) => SessionCloseReason::HostUnreachable,
        pb::session_closed::Reason::InternalError(error) => SessionCloseReason::InternalError {
            detail: error.detail,
        },
    })
}

pub(crate) fn create_agent_request_from_wire(
    request: protocol_wire::CreateAgentRequest,
) -> Result<CreateAgentRpcRequest, protocol_wire::DecodeError> {
    let agent_id = required_uuid_from_bytes("agent_id", request.agent_id)?;
    let agent = request.agent.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("CreateAgentRequest missing agent".into())
    })?;

    let agent = match agent {
        protocol_wire::create_agent_request::Agent::Claude(claude) => {
            CreateAgentConfig::ClaudePty {
                working_dir: PathBuf::from(claude.working_dir),
                args: claude.args,
                terminal_size: claude
                    .initial_terminal_size
                    .map(terminal_size_from_wire)
                    .transpose()?,
            }
        }
        protocol_wire::create_agent_request::Agent::TestAgent(test_agent) => {
            CreateAgentConfig::TestAgent {
                command: test_agent.command,
                working_dir: PathBuf::from(test_agent.working_dir),
                terminal_size: test_agent
                    .initial_terminal_size
                    .map(terminal_size_from_wire)
                    .transpose()?,
            }
        }
    };

    Ok(CreateAgentRpcRequest {
        agent_id,
        name: request.name,
        agent,
    })
}

pub(crate) fn rename_agent_request_from_wire(
    request: protocol_wire::RenameAgentRequest,
) -> Result<RenameAgentRequest, protocol_wire::DecodeError> {
    if request.name.is_empty() {
        return Err(protocol_wire::DecodeError::Invalid(
            "RenameAgentRequest.name must not be empty".into(),
        ));
    }
    Ok(RenameAgentRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        name: request.name,
    })
}

pub(crate) fn delete_agent_id_from_wire(
    request: DeleteAgentRequest,
) -> Result<Uuid, protocol_wire::DecodeError> {
    required_uuid_from_bytes("agent_id", request.agent_id)
}

pub(crate) fn agent_to_wire(
    agent: &Agent,
) -> Result<protocol_wire::Agent, protocol_wire::EncodeError> {
    Ok(protocol_wire::Agent {
        agent_id: uuid_to_bytes(agent.id),
        host_id: uuid_to_bytes(agent.host_id),
        name: agent.name.clone(),
        command: agent.command.clone(),
        working_dir: path_to_proto_string("Agent.working_dir", &agent.working_dir)?,
        agent_type: agent.agent_type.clone(),
        io_protocols: agent.io_protocols.clone(),
        readonly: agent.readonly,
        args: agent.args.clone(),
        created_at_unix_ms: agent.created_at.timestamp_millis(),
    })
}

pub(crate) fn agent_from_wire(
    agent: protocol_wire::Agent,
) -> Result<Agent, protocol_wire::DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("invalid agent created_at".into()))?;

    Ok(Agent {
        id: required_uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: required_uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        agent_type: agent.agent_type,
        io_protocols: agent.io_protocols,
        readonly: agent.readonly,
        args: agent.args,
        created_at,
    })
}

pub(crate) fn path_to_proto_string(
    field: &'static str,
    path: &Path,
) -> Result<String, protocol_wire::EncodeError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| protocol_wire::EncodeError::Invalid(format!("{field} must be valid UTF-8")))
}

fn terminal_size_from_wire(
    size: protocol_wire::TerminalSize,
) -> Result<TerminalSize, protocol_wire::DecodeError> {
    Ok(TerminalSize {
        rows: size.rows.try_into().map_err(|_| {
            protocol_wire::DecodeError::Invalid(format!(
                "terminal rows out of range: {}",
                size.rows
            ))
        })?,
        cols: size.cols.try_into().map_err(|_| {
            protocol_wire::DecodeError::Invalid(format!(
                "terminal cols out of range: {}",
                size.cols
            ))
        })?,
    })
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

fn required_uuid_from_bytes(
    name: &str,
    bytes: Vec<u8>,
) -> Result<Uuid, protocol_wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        protocol_wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_agent_request_decodes_claude_create_config() {
        let agent_id = Uuid::new_v4();
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(agent_id),
            name: Some("dev".to_string()),
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: vec!["--resume".to_string(), "abc".to_string()],
                    initial_terminal_size: Some(protocol_wire::TerminalSize {
                        rows: 40,
                        cols: 120,
                    }),
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.name.as_deref(), Some("dev"));
        let CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected Claude create config");
        };
        assert_eq!(working_dir, PathBuf::from("/tmp/work"));
        assert_eq!(args, ["--resume", "abc"]);
        assert_eq!(
            terminal_size,
            Some(TerminalSize {
                rows: 40,
                cols: 120
            }),
        );
    }

    #[test]
    fn create_agent_request_rejects_empty_agent_id() {
        let request = protocol_wire::CreateAgentRequest {
            agent_id: Vec::new(),
            name: None,
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: Vec::new(),
                    initial_terminal_size: None,
                },
            )),
        };

        let error = create_agent_request_from_wire(request).unwrap_err();
        assert!(error.to_string().contains("agent_id must be 16 bytes"));
    }

    #[test]
    fn test_agent_request_decodes_to_dispatchable_variant() {
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(Uuid::new_v4()),
            name: None,
            agent: Some(protocol_wire::create_agent_request::Agent::TestAgent(
                protocol_wire::TestAgentCreateConfig {
                    command: "/tmp/test-agent".to_string(),
                    working_dir: "/tmp/work".to_string(),
                    initial_terminal_size: Some(protocol_wire::TerminalSize { rows: 24, cols: 80 }),
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        let CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected test agent config");
        };

        assert_eq!(command, "/tmp/test-agent");
        assert_eq!(working_dir, PathBuf::from("/tmp/work"));
        assert_eq!(terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
    }

    #[cfg(unix)]
    #[test]
    fn agent_to_wire_rejects_non_utf8_working_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        use chrono::Utc;

        let agent = Agent {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: None,
            command: "claude".to_string(),
            working_dir: PathBuf::from(OsString::from_vec(vec![0xff])),
            agent_type: "claude".to_string(),
            io_protocols: vec!["claude_raw_v1".to_string()],
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        };

        let err = agent_to_wire(&agent).unwrap_err();
        assert!(
            err.to_string().contains("working_dir must be valid UTF-8"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rename_and_delete_requests_roundtrip() {
        let rename_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        let decoded = rename_agent_request_from_wire(protocol_wire::RenameAgentRequest {
            agent_id: uuid_to_bytes(rename_id),
            name: "renamed".to_string(),
        })
        .unwrap();
        assert_eq!(decoded.agent_id, rename_id);
        assert_eq!(decoded.name, "renamed");

        let agent_id = delete_agent_id_from_wire(DeleteAgentRequest {
            agent_id: uuid_to_bytes(delete_id),
        })
        .unwrap();
        assert_eq!(agent_id, delete_id);
    }

    #[test]
    fn rename_request_rejects_empty_name() {
        let request = protocol_wire::RenameAgentRequest {
            agent_id: uuid_to_bytes(Uuid::new_v4()),
            name: String::new(),
        };

        let error = rename_agent_request_from_wire(request).unwrap_err();

        assert!(error.to_string().contains("name must not be empty"));
    }

    #[test]
    fn delete_request_rejects_invalid_uuid_length() {
        let request = DeleteAgentRequest {
            agent_id: vec![1, 2, 3],
        };

        let err = delete_agent_id_from_wire(request).unwrap_err();
        assert!(
            err.to_string().contains("agent_id must be 16 bytes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn subscribe_session_request_decodes_from_wire() {
        let agent_id = Uuid::new_v4();
        let request = SubscribeSessionRequest {
            agent_id,
            io_protocol: "claude_raw_v1".to_string(),
            args: Some(vec![1, 2, 3]),
        };
        let decoded = subscribe_session_request_from_wire(pb::SubscribeSessionRequest {
            agent_id: uuid_to_bytes(agent_id),
            io_protocol: request.io_protocol.clone(),
            args: request.args.clone(),
        })
        .unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn send_input_requests_decode_from_wire() {
        let events = [
            SessionInputEvent::Input {
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                payload: b"hello".to_vec(),
            },
            SessionInputEvent::Control {
                payload: b"resize".to_vec(),
            },
        ];

        for event in events {
            let agent_id = Uuid::new_v4();
            let io_protocol = "claude_raw_v1".to_string();
            let wire_event = match event.clone() {
                SessionInputEvent::Input { input_id, payload } => {
                    pb::send_input_request::Event::Input(pb::SessionInput { input_id, payload })
                }
                SessionInputEvent::Control { payload } => {
                    pb::send_input_request::Event::Control(pb::SessionControl { payload })
                }
            };
            let request = SendInputRequest {
                agent_id,
                io_protocol: io_protocol.clone(),
                event,
            };
            let decoded = send_input_request_from_wire(pb::SendInputRequest {
                agent_id: uuid_to_bytes(agent_id),
                io_protocol,
                event: Some(wire_event),
            })
            .unwrap();
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn session_output_events_roundtrip() {
        let events = [
            SubscribeSessionEvent::Opened,
            SubscribeSessionEvent::Output {
                payload: b"hello".to_vec(),
            },
            SubscribeSessionEvent::ReplayComplete {
                cursor: Some(b"cursor-2".to_vec()),
            },
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::AgentDeleted,
            },
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::AgentExited { exit_code: Some(9) },
            },
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::HostUnreachable,
            },
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::InternalError {
                    detail: "boom".to_string(),
                },
            },
        ];

        for event in events {
            let encoded = encode_session_output_event_payload(&event);
            let decoded = decode_session_output_event_payload(&encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }
}
