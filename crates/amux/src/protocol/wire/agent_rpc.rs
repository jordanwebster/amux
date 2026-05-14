#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use prost::Message as ProstMessage;
use uuid::Uuid;

use crate::protocol::agent::Agent;
use crate::protocol::message::{
    AgentType, CreateAgentRequest as DomainCreateAgentRequest, ProtocolError, RenameAgentRequest,
    RequestFrame as DomainRequestFrame, ResponseFrame as DomainResponseFrame, TerminalSize,
};
use crate::protocol::method;
use crate::protocol::route::Route;
use crate::protocol::wire::generated::amux::v1 as pb;
use crate::protocol::wire::{self, DeleteAgentRequest};

#[derive(Debug, Clone)]
pub(crate) enum AgentLifecycleRequest {
    Create(CreateAgentRpcRequest),
    Rename(RenameAgentRequest),
    Delete { agent_id: Uuid },
}

#[derive(Debug, Clone)]
pub(crate) enum AgentLifecycleResponse {
    Create(Result<AgentRecord, ProtocolError>),
    Rename(Result<AgentRecord, ProtocolError>),
    Delete(Result<(), ProtocolError>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionOutputEvent {
    Opened,
    Output { payload: Vec<u8> },
    ReplayComplete { cursor: Option<Vec<u8>> },
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRecord {
    pub(crate) id: Uuid,
    pub(crate) host_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    pub(crate) working_dir: PathBuf,
    pub(crate) agent_type: String,
    pub(crate) io_protocols: Vec<String>,
    pub(crate) readonly: bool,
    pub(crate) args: Vec<String>,
    pub(crate) created_at_unix_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct CreateAgentRpcRequest {
    pub(crate) agent_id: Option<Uuid>,
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
    ClaudeSdk {
        working_dir: PathBuf,
        args: Vec<String>,
    },
    TestAgent {
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
    },
}

impl AgentLifecycleResponse {
    pub(crate) fn method_name(&self) -> &'static str {
        match self {
            Self::Create(_) => method::AGENT_CREATE_NAME,
            Self::Rename(_) => method::AGENT_RENAME_NAME,
            Self::Delete(_) => method::AGENT_DELETE_NAME,
        }
    }
}

impl From<&Agent> for AgentRecord {
    fn from(agent: &Agent) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name.clone(),
            command: agent.command.clone(),
            working_dir: agent.working_dir.clone(),
            agent_type: agent.agent_type.clone(),
            io_protocols: agent.io_protocols.clone(),
            readonly: agent.readonly,
            args: agent.args.clone(),
            created_at_unix_ms: agent.created_at.timestamp_millis(),
        }
    }
}

impl AgentRecord {
    pub(crate) fn into_agent(self, route: Route) -> Result<Agent, wire::DecodeError> {
        let created_at = Utc
            .timestamp_millis_opt(self.created_at_unix_ms)
            .single()
            .ok_or_else(|| wire::DecodeError::Invalid("invalid agent created_at".into()))?;
        Ok(Agent {
            id: self.id,
            host_id: self.host_id,
            name: self.name,
            command: self.command,
            working_dir: self.working_dir,
            route,
            agent_type: self.agent_type,
            io_protocols: self.io_protocols.clone(),
            readonly: self.readonly,
            args: self.args,
            created_at,
        })
    }
}

pub(crate) fn agent_entry_from_domain(agent: Agent) -> Result<wire::AgentEntry, wire::EncodeError> {
    Ok(wire::AgentEntry {
        agent: Some(agent_to_wire(&AgentRecord::from(&agent))?),
        route: Some(route_to_wire(&agent.route)),
    })
}

pub(crate) fn agent_entry_to_domain(entry: wire::AgentEntry) -> Result<Agent, wire::DecodeError> {
    let agent = entry
        .agent
        .ok_or_else(|| wire::DecodeError::Invalid("AgentEntry missing agent".into()))?;
    let route = entry
        .route
        .ok_or_else(|| wire::DecodeError::Invalid("AgentEntry missing route".into()))
        .and_then(route_from_wire)?;
    agent_from_wire(agent)?.into_agent(route)
}

impl CreateAgentRpcRequest {
    pub(crate) fn from_domain(
        request: &DomainCreateAgentRequest,
    ) -> Result<Self, wire::EncodeError> {
        let agent = match &request.agent_type {
            AgentType::Claude => CreateAgentConfig::ClaudePty {
                working_dir: request.working_dir.clone(),
                args: request.args.clone(),
                terminal_size: request.terminal_size,
            },
            #[cfg(any(debug_assertions, test))]
            AgentType::TestAgent { command } => {
                if !request.args.is_empty() {
                    return Err(wire::EncodeError::Invalid(
                        "TestAgentCreateConfig cannot represent args".to_string(),
                    ));
                }
                CreateAgentConfig::TestAgent {
                    command: command.clone(),
                    working_dir: request.working_dir.clone(),
                    terminal_size: request.terminal_size,
                }
            }
            AgentType::Unknown => {
                return Err(wire::EncodeError::Invalid(
                    "cannot encode unknown agent type".to_string(),
                ));
            }
        };

        Ok(Self {
            agent_id: Some(request.agent_id),
            name: request.name.clone(),
            agent,
        })
    }
}

pub(crate) fn encode_agent_lifecycle_request_payload(
    request: &AgentLifecycleRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(match request {
        AgentLifecycleRequest::Create(request) => create_agent_request_to_wire(request)?,
        AgentLifecycleRequest::Rename(request) => wire::RenameAgentRequest {
            agent_id: uuid_to_bytes(request.agent_id),
            name: request.name.clone(),
        }
        .encode_to_vec(),
        AgentLifecycleRequest::Delete { agent_id } => DeleteAgentRequest {
            agent_id: uuid_to_bytes(*agent_id),
        }
        .encode_to_vec(),
    })
}

pub(crate) fn decode_agent_lifecycle_request_payload(
    method: &str,
    payload: &[u8],
) -> Result<AgentLifecycleRequest, wire::DecodeError> {
    match method {
        method::AGENT_CREATE_NAME => {
            let request = wire::CreateAgentRequest::decode(payload)?;
            Ok(AgentLifecycleRequest::Create(
                create_agent_request_from_wire(request)?,
            ))
        }
        method::AGENT_RENAME_NAME => {
            let request = wire::RenameAgentRequest::decode(payload)?;
            Ok(AgentLifecycleRequest::Rename(RenameAgentRequest {
                agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
                name: request.name,
            }))
        }
        method::AGENT_DELETE_NAME => {
            let request = DeleteAgentRequest::decode(payload)?;
            Ok(AgentLifecycleRequest::Delete {
                agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
            })
        }
        method => Err(wire::DecodeError::Invalid(format!(
            "unsupported agent lifecycle method {method}"
        ))),
    }
}

pub(crate) fn encode_subscribe_session_request_payload(
    request: &SubscribeSessionRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(subscribe_session_request_to_wire(request)?.encode_to_vec())
}

pub(crate) fn decode_subscribe_session_request(
    request: &DomainRequestFrame,
) -> Result<SubscribeSessionRequest, wire::DecodeError> {
    decode_subscribe_session_request_parts(&request.method, &request.payload)
}

fn decode_subscribe_session_request_parts(
    method_name: &str,
    payload: &[u8],
) -> Result<SubscribeSessionRequest, wire::DecodeError> {
    if method_name != method::AGENT_SUBSCRIBE_SESSION_NAME {
        return Err(wire::DecodeError::Invalid(format!(
            "expected SubscribeSession request method {}, got {}",
            method::AGENT_SUBSCRIBE_SESSION_NAME,
            method_name
        )));
    }
    let request = pb::SubscribeSessionRequest::decode(payload)?;
    subscribe_session_request_from_wire(request)
}

pub(crate) fn encode_send_input_request_payload(
    request: &SendInputRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(send_input_request_to_wire(request)?.encode_to_vec())
}

pub(crate) fn decode_send_input_request(
    request: &DomainRequestFrame,
) -> Result<SendInputRequest, wire::DecodeError> {
    if request.method != method::AGENT_SEND_INPUT_NAME {
        return Err(wire::DecodeError::Invalid(format!(
            "expected SendInput request method {}, got {}",
            method::AGENT_SEND_INPUT_NAME,
            request.method
        )));
    }
    let request = pb::SendInputRequest::decode(request.payload.as_slice())?;
    send_input_request_from_wire(request)
}

pub(crate) fn encode_session_output_event_payload(event: &SessionOutputEvent) -> Vec<u8> {
    session_output_event_to_wire(event).encode_to_vec()
}

pub(crate) fn decode_session_output_event_payload(
    payload: &[u8],
) -> Result<SessionOutputEvent, wire::DecodeError> {
    let event = pb::SubscribeSessionResponse::decode(payload)?;
    session_output_event_from_wire(event)
}

pub(crate) fn encode_agent_lifecycle_response_frame(
    response: &AgentLifecycleResponse,
) -> Result<DomainResponseFrame, wire::EncodeError> {
    Ok(match response {
        AgentLifecycleResponse::Create(Ok(agent)) => DomainResponseFrame::Payload(
            wire::CreateAgentResponse {
                agent: Some(agent_to_wire(agent)?),
            }
            .encode_to_vec(),
        ),
        AgentLifecycleResponse::Create(Err(error)) => DomainResponseFrame::Error(error.clone()),
        AgentLifecycleResponse::Rename(Ok(agent)) => DomainResponseFrame::Payload(
            wire::RenameAgentResponse {
                agent: Some(agent_to_wire(agent)?),
            }
            .encode_to_vec(),
        ),
        AgentLifecycleResponse::Rename(Err(error)) => DomainResponseFrame::Error(error.clone()),
        AgentLifecycleResponse::Delete(Ok(())) => {
            DomainResponseFrame::Payload(wire::Empty {}.encode_to_vec())
        }
        AgentLifecycleResponse::Delete(Err(error)) => DomainResponseFrame::Error(error.clone()),
    })
}

pub(crate) fn decode_agent_lifecycle_response_frame(
    method: &str,
    response: &DomainResponseFrame,
) -> Result<AgentLifecycleResponse, wire::DecodeError> {
    match response {
        DomainResponseFrame::Payload(payload) => {
            decode_agent_lifecycle_response_payload(method, payload)
        }
        DomainResponseFrame::Error(error) => agent_lifecycle_error_response(method, error.clone()),
    }
}

fn decode_agent_lifecycle_response_payload(
    method: &str,
    payload: &[u8],
) -> Result<AgentLifecycleResponse, wire::DecodeError> {
    match method {
        method::AGENT_CREATE_NAME => {
            let response = wire::CreateAgentResponse::decode(payload)?;
            Ok(AgentLifecycleResponse::Create(Ok(agent_from_wire(
                response.agent.ok_or_else(|| {
                    wire::DecodeError::Invalid("CreateAgentResponse missing agent".into())
                })?,
            )?)))
        }
        method::AGENT_RENAME_NAME => {
            let response = wire::RenameAgentResponse::decode(payload)?;
            Ok(AgentLifecycleResponse::Rename(Ok(agent_from_wire(
                response.agent.ok_or_else(|| {
                    wire::DecodeError::Invalid("RenameAgentResponse missing agent".into())
                })?,
            )?)))
        }
        method::AGENT_DELETE_NAME => {
            if !payload.is_empty() {
                return Err(wire::DecodeError::Invalid(format!(
                    "DeleteAgentResponse payload must be empty, got {} bytes",
                    payload.len()
                )));
            }
            Ok(AgentLifecycleResponse::Delete(Ok(())))
        }
        method => Err(wire::DecodeError::Invalid(format!(
            "unsupported agent lifecycle response method {method}"
        ))),
    }
}

fn agent_lifecycle_error_response(
    method: &str,
    error: ProtocolError,
) -> Result<AgentLifecycleResponse, wire::DecodeError> {
    match method {
        method::AGENT_CREATE_NAME => Ok(AgentLifecycleResponse::Create(Err(error))),
        method::AGENT_RENAME_NAME => Ok(AgentLifecycleResponse::Rename(Err(error))),
        method::AGENT_DELETE_NAME => Ok(AgentLifecycleResponse::Delete(Err(error))),
        method => Err(wire::DecodeError::Invalid(format!(
            "unsupported agent lifecycle response method {method}"
        ))),
    }
}

fn is_agent_lifecycle_method(method: &str) -> bool {
    agent_lifecycle_method(method).is_some()
}

fn agent_lifecycle_method(method_name: &str) -> Option<&'static str> {
    match method_name {
        method::AGENT_CREATE_NAME => Some(method::AGENT_CREATE_NAME),
        method::AGENT_RENAME_NAME => Some(method::AGENT_RENAME_NAME),
        method::AGENT_DELETE_NAME => Some(method::AGENT_DELETE_NAME),
        _ => None,
    }
}

fn create_agent_request_to_wire(
    request: &CreateAgentRpcRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    let agent = match &request.agent {
        CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } => wire::create_agent_request::Agent::Claude(wire::ClaudeCreateConfig {
            working_dir: path_to_proto_string(working_dir)?,
            args: args.clone(),
            runtime: Some(wire::claude_create_config::Runtime::Pty(
                wire::ClaudePtyRuntime {
                    initial_terminal_size: terminal_size.map(terminal_size_to_wire),
                },
            )),
        }),
        CreateAgentConfig::ClaudeSdk { working_dir, args } => {
            wire::create_agent_request::Agent::Claude(wire::ClaudeCreateConfig {
                working_dir: path_to_proto_string(working_dir)?,
                args: args.clone(),
                runtime: Some(wire::claude_create_config::Runtime::Sdk(
                    wire::ClaudeSdkRuntime {},
                )),
            })
        }
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => wire::create_agent_request::Agent::TestAgent(wire::TestAgentCreateConfig {
            command: command.clone(),
            working_dir: path_to_proto_string(working_dir)?,
            initial_terminal_size: terminal_size.map(terminal_size_to_wire),
        }),
    };

    Ok(wire::CreateAgentRequest {
        agent_id: request.agent_id.map(uuid_to_bytes).unwrap_or_default(),
        name: request.name.clone(),
        agent: Some(agent),
    }
    .encode_to_vec())
}

fn subscribe_session_request_to_wire(
    request: &SubscribeSessionRequest,
) -> Result<pb::SubscribeSessionRequest, wire::EncodeError> {
    Ok(pb::SubscribeSessionRequest {
        agent_id: uuid_to_bytes(request.agent_id),
        io_protocol: request.io_protocol.clone(),
        args: request.args.clone(),
    })
}

fn subscribe_session_request_from_wire(
    request: pb::SubscribeSessionRequest,
) -> Result<SubscribeSessionRequest, wire::DecodeError> {
    Ok(SubscribeSessionRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        io_protocol: request.io_protocol,
        args: request.args,
    })
}

fn send_input_request_to_wire(
    request: &SendInputRequest,
) -> Result<pb::SendInputRequest, wire::EncodeError> {
    let event = match &request.event {
        SessionInputEvent::Input { input_id, payload } => {
            pb::send_input_request::Event::Input(pb::SessionInput {
                input_id: input_id.clone(),
                payload: payload.clone(),
            })
        }
        SessionInputEvent::Control { payload } => {
            pb::send_input_request::Event::Control(pb::SessionControl {
                payload: payload.clone(),
            })
        }
    };
    Ok(pb::SendInputRequest {
        agent_id: uuid_to_bytes(request.agent_id),
        io_protocol: request.io_protocol.clone(),
        event: Some(event),
    })
}

fn send_input_request_from_wire(
    request: pb::SendInputRequest,
) -> Result<SendInputRequest, wire::DecodeError> {
    let event = request
        .event
        .ok_or_else(|| wire::DecodeError::Invalid("SendInputRequest missing event".into()))?;
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

fn session_output_event_to_wire(event: &SessionOutputEvent) -> pb::SubscribeSessionResponse {
    let event = match event {
        SessionOutputEvent::Opened => {
            pb::subscribe_session_response::Event::Opened(pb::SessionOpened {})
        }
        SessionOutputEvent::Output { payload } => {
            pb::subscribe_session_response::Event::Output(pb::SessionOutput {
                payload: payload.clone(),
            })
        }
        SessionOutputEvent::ReplayComplete { cursor } => {
            pb::subscribe_session_response::Event::ReplayComplete(pb::ReplayComplete {
                cursor: cursor.clone(),
            })
        }
    };
    pb::SubscribeSessionResponse { event: Some(event) }
}

fn session_output_event_from_wire(
    event: pb::SubscribeSessionResponse,
) -> Result<SessionOutputEvent, wire::DecodeError> {
    let event = event.event.ok_or_else(|| {
        wire::DecodeError::Invalid("SubscribeSessionResponse missing event".into())
    })?;
    match event {
        pb::subscribe_session_response::Event::Opened(_) => Ok(SessionOutputEvent::Opened),
        pb::subscribe_session_response::Event::Output(output) => Ok(SessionOutputEvent::Output {
            payload: output.payload,
        }),
        pb::subscribe_session_response::Event::ReplayComplete(replay_complete) => {
            Ok(SessionOutputEvent::ReplayComplete {
                cursor: replay_complete.cursor,
            })
        }
    }
}

fn create_agent_request_from_wire(
    request: wire::CreateAgentRequest,
) -> Result<CreateAgentRpcRequest, wire::DecodeError> {
    let agent_id = optional_uuid_from_bytes("agent_id", request.agent_id)?;
    let agent = request
        .agent
        .ok_or_else(|| wire::DecodeError::Invalid("CreateAgentRequest missing agent".into()))?;

    let agent = match agent {
        wire::create_agent_request::Agent::Claude(claude) => {
            let runtime = claude.runtime.ok_or_else(|| {
                wire::DecodeError::Invalid("ClaudeCreateConfig missing runtime".into())
            })?;
            match runtime {
                wire::claude_create_config::Runtime::Pty(pty) => CreateAgentConfig::ClaudePty {
                    working_dir: PathBuf::from(claude.working_dir),
                    args: claude.args,
                    terminal_size: pty
                        .initial_terminal_size
                        .map(terminal_size_from_wire)
                        .transpose()?,
                },
                wire::claude_create_config::Runtime::Sdk(wire::ClaudeSdkRuntime {}) => {
                    CreateAgentConfig::ClaudeSdk {
                        working_dir: PathBuf::from(claude.working_dir),
                        args: claude.args,
                    }
                }
            }
        }
        wire::create_agent_request::Agent::TestAgent(test_agent) => CreateAgentConfig::TestAgent {
            command: test_agent.command,
            working_dir: PathBuf::from(test_agent.working_dir),
            terminal_size: test_agent
                .initial_terminal_size
                .map(terminal_size_from_wire)
                .transpose()?,
        },
    };

    Ok(CreateAgentRpcRequest {
        agent_id,
        name: request.name,
        agent,
    })
}

fn agent_to_wire(agent: &AgentRecord) -> Result<wire::Agent, wire::EncodeError> {
    Ok(wire::Agent {
        agent_id: uuid_to_bytes(agent.id),
        host_id: uuid_to_bytes(agent.host_id),
        name: agent.name.clone(),
        command: agent.command.clone(),
        working_dir: path_to_proto_string(&agent.working_dir)?,
        agent_type: agent.agent_type.clone(),
        io_protocols: agent.io_protocols.clone(),
        readonly: agent.readonly,
        args: agent.args.clone(),
        created_at_unix_ms: agent.created_at_unix_ms,
    })
}

fn agent_from_wire(agent: wire::Agent) -> Result<AgentRecord, wire::DecodeError> {
    Utc.timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| wire::DecodeError::Invalid("invalid agent created_at".into()))?;

    Ok(AgentRecord {
        id: required_uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: required_uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        agent_type: agent.agent_type,
        io_protocols: agent.io_protocols,
        readonly: agent.readonly,
        args: agent.args,
        created_at_unix_ms: agent.created_at_unix_ms,
    })
}

fn route_to_wire(route: &Route) -> wire::Route {
    wire::Route {
        links: route.iter().map(|link| link.as_str().to_string()).collect(),
    }
}

fn route_from_wire(route: wire::Route) -> Result<Route, wire::DecodeError> {
    Route::from_links(route.links)
        .map_err(|error| wire::DecodeError::Invalid(format!("invalid route: {error}")))
}

fn path_to_proto_string(path: &Path) -> Result<String, wire::EncodeError> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        wire::EncodeError::Invalid("CreateAgentRequest.working_dir must be valid UTF-8".to_string())
    })
}

fn terminal_size_to_wire(size: TerminalSize) -> wire::TerminalSize {
    wire::TerminalSize {
        rows: u32::from(size.rows),
        cols: u32::from(size.cols),
    }
}

fn terminal_size_from_wire(size: wire::TerminalSize) -> Result<TerminalSize, wire::DecodeError> {
    Ok(TerminalSize {
        rows: size.rows.try_into().map_err(|_| {
            wire::DecodeError::Invalid(format!("terminal rows out of range: {}", size.rows))
        })?,
        cols: size.cols.try_into().map_err(|_| {
            wire::DecodeError::Invalid(format!("terminal cols out of range: {}", size.cols))
        })?,
    })
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

fn optional_uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Option<Uuid>, wire::DecodeError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    required_uuid_from_bytes(name, bytes).map(Some)
}

fn required_uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_RAW_V1: &str = "claude_raw_v1";
    const CLAUDE_PTY_TRANSCRIPT_V1: &str = "claude_pty_transcript_v1";

    fn sample_agent_record() -> AgentRecord {
        AgentRecord {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("dev".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp/work"),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                CLAUDE_RAW_V1.to_string(),
                CLAUDE_PTY_TRANSCRIPT_V1.to_string(),
            ],
            readonly: false,
            args: vec!["--resume".to_string(), "abc".to_string()],
            created_at_unix_ms: 1_700_000_000_123,
        }
    }

    #[test]
    fn create_agent_request_encodes_as_payload() {
        let agent_id = Uuid::new_v4();
        let request = AgentLifecycleRequest::Create(CreateAgentRpcRequest {
            agent_id: Some(agent_id),
            name: Some("dev".to_string()),
            agent: CreateAgentConfig::ClaudePty {
                working_dir: PathBuf::from("/tmp/work"),
                args: vec!["--resume".to_string(), "abc".to_string()],
                terminal_size: Some(TerminalSize {
                    rows: 40,
                    cols: 120,
                }),
            },
        });

        let encoded = encode_agent_lifecycle_request_payload(&request).unwrap();
        let request = wire::CreateAgentRequest::decode(encoded.as_slice()).unwrap();
        assert_eq!(request.agent_id, agent_id.as_bytes());
        let Some(wire::create_agent_request::Agent::Claude(claude)) = request.agent else {
            panic!("expected claude config");
        };
        assert_eq!(claude.working_dir, "/tmp/work");
        assert_eq!(claude.args, ["--resume", "abc"]);
        let Some(wire::claude_create_config::Runtime::Pty(pty)) = claude.runtime else {
            panic!("expected pty runtime");
        };
        assert_eq!(
            pty.initial_terminal_size,
            Some(wire::TerminalSize {
                rows: 40,
                cols: 120
            })
        );
    }

    #[test]
    fn create_agent_request_roundtrips_empty_agent_id() {
        let request = AgentLifecycleRequest::Create(CreateAgentRpcRequest {
            agent_id: None,
            name: None,
            agent: CreateAgentConfig::ClaudePty {
                working_dir: PathBuf::from("/tmp/work"),
                args: Vec::new(),
                terminal_size: None,
            },
        });

        let encoded = encode_agent_lifecycle_request_payload(&request).unwrap();
        let decoded =
            decode_agent_lifecycle_request_payload(method::AGENT_CREATE_NAME, &encoded).unwrap();
        let AgentLifecycleRequest::Create(decoded) = decoded else {
            panic!("expected create request");
        };
        assert_eq!(decoded.agent_id, None);
    }

    #[test]
    fn claude_sdk_request_roundtrips_to_dispatchable_variant() {
        let request = AgentLifecycleRequest::Create(CreateAgentRpcRequest {
            agent_id: Some(Uuid::new_v4()),
            name: None,
            agent: CreateAgentConfig::ClaudeSdk {
                working_dir: PathBuf::from("/tmp/work"),
                args: vec!["--json".to_string()],
            },
        });

        let encoded = encode_agent_lifecycle_request_payload(&request).unwrap();
        let decoded =
            decode_agent_lifecycle_request_payload(method::AGENT_CREATE_NAME, &encoded).unwrap();
        let AgentLifecycleRequest::Create(decoded) = decoded else {
            panic!("expected create request");
        };
        let CreateAgentConfig::ClaudeSdk { working_dir, args } = decoded.agent else {
            panic!("expected sdk runtime");
        };
        assert_eq!(working_dir, PathBuf::from("/tmp/work"));
        assert_eq!(args, ["--json"]);
    }

    #[test]
    fn test_agent_request_roundtrips_to_dispatchable_variant() {
        let request = AgentLifecycleRequest::Create(CreateAgentRpcRequest {
            agent_id: Some(Uuid::new_v4()),
            name: None,
            agent: CreateAgentConfig::TestAgent {
                command: "/tmp/test-agent".to_string(),
                working_dir: PathBuf::from("/tmp/work"),
                terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            },
        });

        let encoded = encode_agent_lifecycle_request_payload(&request).unwrap();
        let decoded =
            decode_agent_lifecycle_request_payload(method::AGENT_CREATE_NAME, &encoded).unwrap();
        let AgentLifecycleRequest::Create(decoded) = decoded else {
            panic!("expected create request");
        };
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

    #[test]
    fn current_domain_test_agent_request_rejects_unrepresentable_args() {
        let request = DomainCreateAgentRequest {
            agent_id: Uuid::new_v4(),
            name: None,
            agent_type: AgentType::TestAgent {
                command: "/tmp/test-agent".to_string(),
            },
            working_dir: PathBuf::from("/tmp/work"),
            terminal_size: None,
            args: vec!["--flag".to_string()],
        };

        let err = CreateAgentRpcRequest::from_domain(&request).unwrap_err();
        assert!(
            err.to_string().contains("cannot represent args"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_request_rejects_non_utf8_working_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let request = AgentLifecycleRequest::Create(CreateAgentRpcRequest {
            agent_id: Some(Uuid::new_v4()),
            name: None,
            agent: CreateAgentConfig::ClaudePty {
                working_dir: PathBuf::from(OsString::from_vec(vec![0xff])),
                args: Vec::new(),
                terminal_size: None,
            },
        });

        let err = encode_agent_lifecycle_request_payload(&request).unwrap_err();
        assert!(
            err.to_string().contains("working_dir must be valid UTF-8"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rename_and_delete_requests_roundtrip() {
        let rename_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        let rename = encode_agent_lifecycle_request_payload(&AgentLifecycleRequest::Rename(
            RenameAgentRequest {
                agent_id: rename_id,
                name: "renamed".to_string(),
            },
        ))
        .unwrap();
        let delete = encode_agent_lifecycle_request_payload(&AgentLifecycleRequest::Delete {
            agent_id: delete_id,
        })
        .unwrap();

        let AgentLifecycleRequest::Rename(decoded) =
            decode_agent_lifecycle_request_payload(method::AGENT_RENAME_NAME, &rename).unwrap()
        else {
            panic!("expected rename request");
        };
        assert_eq!(decoded.agent_id, rename_id);
        assert_eq!(decoded.name, "renamed");

        let AgentLifecycleRequest::Delete { agent_id } =
            decode_agent_lifecycle_request_payload(method::AGENT_DELETE_NAME, &delete).unwrap()
        else {
            panic!("expected delete request");
        };
        assert_eq!(agent_id, delete_id);
    }

    #[test]
    fn delete_request_rejects_invalid_uuid_length() {
        let payload = DeleteAgentRequest {
            agent_id: vec![1, 2, 3],
        }
        .encode_to_vec();

        let err = decode_agent_lifecycle_request_payload(method::AGENT_DELETE_NAME, &payload)
            .unwrap_err();
        assert!(
            err.to_string().contains("agent_id must be 16 bytes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn subscribe_session_request_is_recognized_strictly() {
        let request = SubscribeSessionRequest {
            agent_id: Uuid::new_v4(),
            io_protocol: "claude_raw_v1".to_string(),
            args: Some(vec![1, 2, 3]),
        };
        let encoded = encode_subscribe_session_request_payload(&request).unwrap();
        assert_eq!(
            subscribe_session_request_from_wire(
                pb::SubscribeSessionRequest::decode(encoded.as_slice()).unwrap()
            )
            .unwrap(),
            request
        );
        assert_eq!(
            decode_subscribe_session_request(&DomainRequestFrame {
                method: method::AGENT_SUBSCRIBE_SESSION_NAME.to_string(),
                payload: encoded,
            })
            .unwrap(),
            request
        );

        let wrong_method = DomainRequestFrame {
            method: method::AGENT_LIST_NAME.to_string(),
            payload: wire::Empty {}.encode_to_vec(),
        };
        let err = decode_subscribe_session_request(&wrong_method).unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "expected SubscribeSession request method {}, got {}",
                method::AGENT_SUBSCRIBE_SESSION_NAME,
                method::AGENT_LIST_NAME
            )),
            "unexpected error: {err}"
        );

        let invalid_payload = DomainRequestFrame {
            method: method::AGENT_SUBSCRIBE_SESSION_NAME.to_string(),
            payload: vec![1, 2, 3],
        };
        let err = decode_subscribe_session_request(&invalid_payload).unwrap_err();
        assert!(
            err.to_string().contains("failed to decode Protobuf"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn send_input_requests_roundtrip() {
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
            let request = SendInputRequest {
                agent_id: Uuid::new_v4(),
                io_protocol: "claude_raw_v1".to_string(),
                event,
            };
            let encoded = encode_send_input_request_payload(&request).unwrap();
            let decoded = decode_send_input_request(&DomainRequestFrame {
                method: method::AGENT_SEND_INPUT_NAME.to_string(),
                payload: encoded,
            })
            .unwrap();
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn session_output_events_roundtrip() {
        let events = [
            SessionOutputEvent::Opened,
            SessionOutputEvent::Output {
                payload: b"hello".to_vec(),
            },
            SessionOutputEvent::ReplayComplete {
                cursor: Some(b"cursor-2".to_vec()),
            },
        ];

        for event in events {
            let encoded = encode_session_output_event_payload(&event);
            let decoded = decode_session_output_event_payload(&encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn create_response_encodes_as_response_payload() {
        let agent = sample_agent_record();
        let response = encode_agent_lifecycle_response_frame(&AgentLifecycleResponse::Create(Ok(
            agent.clone(),
        )))
        .unwrap();
        let DomainResponseFrame::Payload(payload) = response else {
            panic!("expected response payload");
        };
        let response = wire::CreateAgentResponse::decode(payload.as_slice()).unwrap();
        let wire_agent = response.agent.expect("agent should be present");

        assert_eq!(wire_agent.agent_id, agent.id.as_bytes());
        assert_eq!(wire_agent.host_id, agent.host_id.as_bytes());
        assert_eq!(wire_agent.name, agent.name);
        assert_eq!(wire_agent.working_dir, "/tmp/work");
        assert_eq!(
            wire_agent.io_protocols,
            [
                CLAUDE_RAW_V1.to_string(),
                CLAUDE_PTY_TRANSCRIPT_V1.to_string()
            ]
        );
    }

    #[test]
    fn create_response_roundtrips_success_and_error() {
        let agent = sample_agent_record();
        let success = encode_agent_lifecycle_response_frame(&AgentLifecycleResponse::Create(Ok(
            agent.clone(),
        )))
        .unwrap();
        let error = encode_agent_lifecycle_response_frame(&AgentLifecycleResponse::Create(Err(
            ProtocolError::ServerError {
                message: "boom".to_string(),
            },
        )))
        .unwrap();

        let AgentLifecycleResponse::Create(Ok(decoded)) =
            decode_agent_lifecycle_response_frame(method::AGENT_CREATE_NAME, &success).unwrap()
        else {
            panic!("expected create success");
        };
        assert_eq!(decoded, agent);

        let AgentLifecycleResponse::Create(Err(ProtocolError::ServerError { message })) =
            decode_agent_lifecycle_response_frame(method::AGENT_CREATE_NAME, &error).unwrap()
        else {
            panic!("expected create error");
        };
        assert_eq!(message, "boom");
    }

    #[test]
    fn rename_and_delete_responses_roundtrip() {
        let agent = sample_agent_record();
        let rename = encode_agent_lifecycle_response_frame(&AgentLifecycleResponse::Rename(Ok(
            agent.clone(),
        )))
        .unwrap();
        let delete =
            encode_agent_lifecycle_response_frame(&AgentLifecycleResponse::Delete(Ok(()))).unwrap();

        let AgentLifecycleResponse::Rename(Ok(decoded)) =
            decode_agent_lifecycle_response_frame(method::AGENT_RENAME_NAME, &rename).unwrap()
        else {
            panic!("expected rename success");
        };
        assert_eq!(decoded, agent);

        let AgentLifecycleResponse::Delete(Ok(())) =
            decode_agent_lifecycle_response_frame(method::AGENT_DELETE_NAME, &delete).unwrap()
        else {
            panic!("expected delete success");
        };
    }

    #[test]
    fn response_decode_preserves_multiple_agent_io_protocols() {
        let agent = sample_agent_record();
        let mut wire_agent = agent_to_wire(&agent).unwrap();
        wire_agent.io_protocols = vec![
            CLAUDE_RAW_V1.to_string(),
            CLAUDE_PTY_TRANSCRIPT_V1.to_string(),
        ];

        let AgentLifecycleResponse::Create(Ok(decoded)) = decode_agent_lifecycle_response_frame(
            method::AGENT_CREATE_NAME,
            &DomainResponseFrame::Payload(
                wire::CreateAgentResponse {
                    agent: Some(wire_agent.clone()),
                }
                .encode_to_vec(),
            ),
        )
        .unwrap() else {
            panic!("expected create success");
        };
        assert_eq!(decoded.io_protocols, wire_agent.io_protocols);
        let agent = decoded.into_agent(Route::empty()).unwrap();
        assert_eq!(agent.io_protocols, wire_agent.io_protocols);
    }

    #[test]
    fn response_decode_rejects_invalid_agent_created_at() {
        let mut agent = agent_to_wire(&sample_agent_record()).unwrap();
        agent.created_at_unix_ms = i64::MAX;

        let err = decode_agent_lifecycle_response_frame(
            method::AGENT_CREATE_NAME,
            &DomainResponseFrame::Payload(
                wire::CreateAgentResponse { agent: Some(agent) }.encode_to_vec(),
            ),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid agent created_at"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn delete_response_rejects_non_empty_success_payload() {
        let err = decode_agent_lifecycle_response_frame(
            method::AGENT_DELETE_NAME,
            &DomainResponseFrame::Payload(
                wire::DebugResponse {
                    dump: "not empty".to_string(),
                }
                .encode_to_vec(),
            ),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("payload must be empty"),
            "unexpected error: {err}"
        );
    }
}
