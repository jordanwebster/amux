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
use crate::protocol::wire::{
    self, DeleteAgentRequest, FrameBody, Request, Response, frame_body, response,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenSessionInputEvent {
    Input { input_id: Vec<u8>, payload: Vec<u8> },
    Control { payload: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OpenSessionOutputEvent {
    Opened,
    Output {
        payload: Vec<u8>,
    },
    /// Protocol-defined acknowledgement for a prior input event.
    InputResult {
        input_id: Vec<u8>,
        result: Result<(), ProtocolError>,
    },
    ReplayComplete {
        cursor: Option<Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenSessionClientFrame {
    Open(SessionOpenRequest),
    Input(OpenSessionInputEvent),
    Control { payload: Vec<u8> },
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoutedFrameBodyKind {
    Request { method: String },
    Response,
    StreamItem,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionOpenRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) io_protocol: String,
    pub(crate) args: Option<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) struct AgentLifecycleDecodeError {
    method: &'static str,
    source: wire::DecodeError,
}

#[derive(Debug)]
pub(crate) struct OpenSessionDecodeError {
    source: wire::DecodeError,
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

impl AgentLifecycleRequest {
    pub(crate) fn method_name(&self) -> &'static str {
        match self {
            Self::Create(_) => method::AGENT_CREATE_NAME,
            Self::Rename(_) => method::AGENT_RENAME_NAME,
            Self::Delete { .. } => method::AGENT_DELETE_NAME,
        }
    }
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

impl AgentLifecycleDecodeError {
    pub(crate) fn method(&self) -> &'static str {
        self.method
    }

    pub(crate) fn source(&self) -> &wire::DecodeError {
        &self.source
    }
}

impl OpenSessionDecodeError {
    pub(crate) fn source(&self) -> &wire::DecodeError {
        &self.source
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

pub(crate) fn encode_agent_lifecycle_request(
    request: &AgentLifecycleRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    let payload = match request {
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
    };

    Ok(FrameBody {
        kind: Some(frame_body::Kind::Request(Request {
            method: request.method_name().to_string(),
            payload,
        })),
    }
    .encode_to_vec())
}

pub(crate) fn decode_agent_lifecycle_request(
    payload: &[u8],
) -> Result<AgentLifecycleRequest, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    let frame_body::Kind::Request(request) = body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed FrameBody kind".into()))?
    else {
        return Err(wire::DecodeError::Invalid(
            "routed agent lifecycle payload must be a request".to_string(),
        ));
    };

    decode_agent_lifecycle_request_payload(&request.method, &request.payload)
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
            "unsupported routed agent lifecycle method {method}"
        ))),
    }
}

pub(crate) fn decode_agent_lifecycle_request_if_present(
    payload: &[u8],
) -> Result<Option<AgentLifecycleRequest>, AgentLifecycleDecodeError> {
    let Ok(body) = FrameBody::decode(payload) else {
        return Ok(None);
    };
    let Some(frame_body::Kind::Request(request)) = body.kind else {
        return Ok(None);
    };
    let Some(method) = agent_lifecycle_method(&request.method) else {
        return Ok(None);
    };

    decode_agent_lifecycle_request(
        &FrameBody {
            kind: Some(frame_body::Kind::Request(request)),
        }
        .encode_to_vec(),
    )
    .map(Some)
    .map_err(|source| AgentLifecycleDecodeError { method, source })
}

pub(crate) fn decode_routed_frame_body_kind(
    payload: &[u8],
) -> Result<RoutedFrameBodyKind, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    match body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed FrameBody kind".into()))?
    {
        frame_body::Kind::Request(request) => Ok(RoutedFrameBodyKind::Request {
            method: request.method,
        }),
        frame_body::Kind::Response(_) => Ok(RoutedFrameBodyKind::Response),
        frame_body::Kind::StreamItem(_) => Ok(RoutedFrameBodyKind::StreamItem),
        frame_body::Kind::Cancel(_) => Ok(RoutedFrameBodyKind::Cancel),
    }
}

pub(crate) fn encode_routed_error_response(
    error: &ProtocolError,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(error_response_body(error).encode_to_vec())
}

pub(crate) fn encode_open_session_request() -> Result<Vec<u8>, wire::EncodeError> {
    Ok(FrameBody {
        kind: Some(frame_body::Kind::Request(Request {
            method: method::AGENT_OPEN_SESSION_NAME.to_string(),
            payload: wire::Empty {}.encode_to_vec(),
        })),
    }
    .encode_to_vec())
}

pub(crate) fn encode_open_session_open_event(
    request: &SessionOpenRequest,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(FrameBody {
        kind: Some(frame_body::Kind::StreamItem(wire::StreamItem {
            payload: open_session_client_frame_to_payload(&OpenSessionClientFrame::Open(
                request.clone(),
            ))?,
        })),
    }
    .encode_to_vec())
}

pub(crate) fn decode_open_session_request(
    request: &DomainRequestFrame,
) -> Result<(), wire::DecodeError> {
    decode_open_session_request_parts(&request.method, &request.payload)
}

fn decode_open_session_request_parts(
    method_name: &str,
    payload: &[u8],
) -> Result<(), wire::DecodeError> {
    if method_name != method::AGENT_OPEN_SESSION_NAME {
        return Err(wire::DecodeError::Invalid(format!(
            "expected OpenSession request method {}, got {}",
            method::AGENT_OPEN_SESSION_NAME,
            method_name
        )));
    }
    if !payload.is_empty() {
        return Err(wire::DecodeError::Invalid(
            "OpenSession request payload must be Empty".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn decode_open_session_request_if_present(
    payload: &[u8],
) -> Result<Option<()>, OpenSessionDecodeError> {
    let Ok(body) = FrameBody::decode(payload) else {
        return Ok(None);
    };
    let Some(frame_body::Kind::Request(request)) = body.kind else {
        return Ok(None);
    };
    if request.method != method::AGENT_OPEN_SESSION_NAME {
        return Ok(None);
    }
    decode_open_session_request_parts(&request.method, &request.payload)
        .map(|()| Some(()))
        .map_err(|source| OpenSessionDecodeError { source })
}

pub(crate) fn encode_open_session_input_event(
    event: &OpenSessionInputEvent,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(FrameBody {
        kind: Some(frame_body::Kind::StreamItem(wire::StreamItem {
            payload: open_session_client_frame_to_payload(&OpenSessionClientFrame::Input(
                event.clone(),
            ))?,
        })),
    }
    .encode_to_vec())
}

pub(crate) fn decode_open_session_input_event(
    payload: &[u8],
) -> Result<OpenSessionInputEvent, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    let frame_body::Kind::StreamItem(item) = body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed FrameBody kind".into()))?
    else {
        return Err(wire::DecodeError::Invalid(
            "OpenSession input must be a stream item".to_string(),
        ));
    };
    decode_open_session_input_payload(&item.payload)
}

pub(crate) fn decode_open_session_input_payload(
    payload: &[u8],
) -> Result<OpenSessionInputEvent, wire::DecodeError> {
    match decode_open_session_client_frame_payload(payload)? {
        OpenSessionClientFrame::Input(event) => Ok(event),
        OpenSessionClientFrame::Control { .. } => Err(wire::DecodeError::Invalid(
            "OpenSession stream item payload must be an input client event".to_string(),
        )),
        OpenSessionClientFrame::Open(_) => Err(wire::DecodeError::Invalid(
            "OpenSession stream item payload must be an input client event".to_string(),
        )),
        OpenSessionClientFrame::Cancel => Err(wire::DecodeError::Invalid(
            "OpenSession stream item payload cannot be cancel".to_string(),
        )),
    }
}

pub(crate) fn encode_open_session_cancel() -> Result<Vec<u8>, wire::EncodeError> {
    Ok(FrameBody {
        kind: Some(frame_body::Kind::Cancel(wire::Cancel {})),
    }
    .encode_to_vec())
}

pub(crate) fn decode_open_session_client_frame(
    payload: &[u8],
) -> Result<OpenSessionClientFrame, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    match body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed FrameBody kind".into()))?
    {
        frame_body::Kind::Request(request) => {
            decode_open_session_request(&DomainRequestFrame {
                method: request.method,
                payload: request.payload,
            })?;
            Err(wire::DecodeError::Invalid(
                "OpenSession request frame does not carry a client stream event".to_string(),
            ))
        }
        frame_body::Kind::StreamItem(item) => {
            decode_open_session_client_frame_payload(&item.payload)
        }
        frame_body::Kind::Cancel(_) => Ok(OpenSessionClientFrame::Cancel),
        frame_body::Kind::Response(_) => Err(wire::DecodeError::Invalid(
            "OpenSession client frame must be a request, stream item, or cancel".to_string(),
        )),
    }
}

pub(crate) fn decode_open_session_client_frame_if_present(
    payload: &[u8],
) -> Result<Option<OpenSessionClientFrame>, OpenSessionDecodeError> {
    let Ok(body) = FrameBody::decode(payload) else {
        return Ok(None);
    };
    match body.kind {
        Some(frame_body::Kind::Request(request))
            if request.method == method::AGENT_OPEN_SESSION_NAME =>
        {
            decode_open_session_request(&DomainRequestFrame {
                method: request.method,
                payload: request.payload,
            })
            .map(|()| None)
            .map_err(|source| OpenSessionDecodeError { source })
        }
        Some(frame_body::Kind::StreamItem(item)) => {
            decode_open_session_client_frame_payload(&item.payload)
                .map(Some)
                .map_err(|source| OpenSessionDecodeError { source })
        }
        Some(frame_body::Kind::Cancel(_)) => Ok(Some(OpenSessionClientFrame::Cancel)),
        Some(frame_body::Kind::Request(_)) | Some(frame_body::Kind::Response(_)) | None => Ok(None),
    }
}

pub(crate) fn encode_open_session_output_event(
    event: &OpenSessionOutputEvent,
) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(FrameBody {
        kind: Some(frame_body::Kind::StreamItem(wire::StreamItem {
            payload: encode_open_session_output_event_payload(event),
        })),
    }
    .encode_to_vec())
}

pub(crate) fn encode_open_session_output_event_payload(event: &OpenSessionOutputEvent) -> Vec<u8> {
    session_output_event_to_wire(event).encode_to_vec()
}

pub(crate) fn decode_open_session_output_event(
    payload: &[u8],
) -> Result<OpenSessionOutputEvent, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    let frame_body::Kind::StreamItem(item) = body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed FrameBody kind".into()))?
    else {
        return Err(wire::DecodeError::Invalid(
            "OpenSession output must be a stream item".to_string(),
        ));
    };
    let event = wire::OpenSessionResponse::decode(item.payload.as_slice())?;
    session_output_event_from_wire(event)
}

pub(crate) fn decode_open_session_output_event_payload(
    payload: &[u8],
) -> Result<OpenSessionOutputEvent, wire::DecodeError> {
    let event = wire::OpenSessionResponse::decode(payload)?;
    session_output_event_from_wire(event)
}

pub(crate) fn encode_open_session_response(
    result: Result<(), ProtocolError>,
) -> Result<Vec<u8>, wire::EncodeError> {
    let body = match result {
        Ok(()) => response_body(Vec::new()),
        Err(error) => error_response_body(&error),
    };
    Ok(body.encode_to_vec())
}

pub(crate) fn decode_open_session_response(
    payload: &[u8],
) -> Result<Result<(), ProtocolError>, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    let frame_body::Kind::Response(response) = body.kind.ok_or_else(|| {
        wire::DecodeError::Invalid("missing routed response FrameBody kind".into())
    })?
    else {
        return Err(wire::DecodeError::Invalid(
            "OpenSession terminal frame must be a response".to_string(),
        ));
    };
    let outcome = response
        .outcome
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed Response outcome".into()))?;
    match outcome {
        response::Outcome::Payload(payload) => {
            if !payload.is_empty() {
                return Err(wire::DecodeError::Invalid(format!(
                    "OpenSession success response payload must be empty, got {} bytes",
                    payload.len()
                )));
            }
            Ok(Ok(()))
        }
        response::Outcome::Error(error) => Ok(Err(wire::decode_protocol_error(error))),
    }
}

pub(crate) fn encode_agent_lifecycle_response(
    response: &AgentLifecycleResponse,
) -> Result<Vec<u8>, wire::EncodeError> {
    let body = crate::protocol::message::FrameBody::Response(
        encode_agent_lifecycle_response_frame(response)?,
    );
    wire::encode_frame_body(&body)
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

pub(crate) fn decode_agent_lifecycle_response(
    method: &str,
    payload: &[u8],
) -> Result<AgentLifecycleResponse, wire::DecodeError> {
    let body = FrameBody::decode(payload)?;
    let frame_body::Kind::Response(response) = body.kind.ok_or_else(|| {
        wire::DecodeError::Invalid("missing routed response FrameBody kind".into())
    })?
    else {
        return Err(wire::DecodeError::Invalid(
            "routed agent lifecycle payload must be a response".to_string(),
        ));
    };

    let outcome = response
        .outcome
        .ok_or_else(|| wire::DecodeError::Invalid("missing routed Response outcome".into()))?;

    match (method, outcome) {
        (method::AGENT_CREATE_NAME, response::Outcome::Payload(payload)) => {
            let response = wire::CreateAgentResponse::decode(payload.as_slice())?;
            Ok(AgentLifecycleResponse::Create(Ok(agent_from_wire(
                response.agent.ok_or_else(|| {
                    wire::DecodeError::Invalid("CreateAgentResponse missing agent".into())
                })?,
            )?)))
        }
        (method::AGENT_CREATE_NAME, response::Outcome::Error(error)) => Ok(
            AgentLifecycleResponse::Create(Err(wire::decode_protocol_error(error))),
        ),
        (method::AGENT_RENAME_NAME, response::Outcome::Payload(payload)) => {
            let response = wire::RenameAgentResponse::decode(payload.as_slice())?;
            Ok(AgentLifecycleResponse::Rename(Ok(agent_from_wire(
                response.agent.ok_or_else(|| {
                    wire::DecodeError::Invalid("RenameAgentResponse missing agent".into())
                })?,
            )?)))
        }
        (method::AGENT_RENAME_NAME, response::Outcome::Error(error)) => Ok(
            AgentLifecycleResponse::Rename(Err(wire::decode_protocol_error(error))),
        ),
        (method::AGENT_DELETE_NAME, response::Outcome::Payload(payload)) => {
            if !payload.is_empty() {
                return Err(wire::DecodeError::Invalid(format!(
                    "DeleteAgentResponse Empty payload must be empty, got {} bytes",
                    payload.len()
                )));
            }
            Ok(AgentLifecycleResponse::Delete(Ok(())))
        }
        (method::AGENT_DELETE_NAME, response::Outcome::Error(error)) => Ok(
            AgentLifecycleResponse::Delete(Err(wire::decode_protocol_error(error))),
        ),
        (method, _) => Err(wire::DecodeError::Invalid(format!(
            "unsupported routed agent lifecycle response method {method}"
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

fn session_input_event_to_wire(
    event: &OpenSessionInputEvent,
) -> Result<wire::OpenSessionRequest, wire::EncodeError> {
    let event = match event {
        OpenSessionInputEvent::Input { input_id, payload } => {
            wire::open_session_request::Event::Input(wire::SessionInput {
                input_id: input_id.clone(),
                payload: payload.clone(),
            })
        }
        OpenSessionInputEvent::Control { payload } => {
            wire::open_session_request::Event::Control(wire::SessionControl {
                payload: payload.clone(),
            })
        }
    };
    Ok(wire::OpenSessionRequest { event: Some(event) })
}

fn session_input_event_from_wire(
    event: wire::OpenSessionRequest,
) -> Result<OpenSessionInputEvent, wire::DecodeError> {
    let event = event
        .event
        .ok_or_else(|| wire::DecodeError::Invalid("OpenSessionRequest missing event".into()))?;
    match event {
        wire::open_session_request::Event::Input(input) => Ok(OpenSessionInputEvent::Input {
            input_id: input.input_id,
            payload: input.payload,
        }),
        wire::open_session_request::Event::Control(control) => Ok(OpenSessionInputEvent::Control {
            payload: control.payload,
        }),
        wire::open_session_request::Event::Open(_) => Err(wire::DecodeError::Invalid(
            "OpenSessionRequest open event is not a session input event".to_string(),
        )),
    }
}

fn session_output_event_to_wire(event: &OpenSessionOutputEvent) -> wire::OpenSessionResponse {
    let event = match event {
        OpenSessionOutputEvent::Opened => {
            wire::open_session_response::Event::Opened(wire::SessionOpened {})
        }
        OpenSessionOutputEvent::Output { payload } => {
            wire::open_session_response::Event::Output(wire::SessionOutput {
                payload: payload.clone(),
            })
        }
        OpenSessionOutputEvent::InputResult { input_id, result } => {
            let outcome = match result {
                Ok(()) => wire::session_input_result::Outcome::Accepted(wire::Empty {}),
                Err(error) => {
                    wire::session_input_result::Outcome::Error(wire::encode_protocol_error(error))
                }
            };
            wire::open_session_response::Event::InputResult(wire::SessionInputResult {
                input_id: input_id.clone(),
                outcome: Some(outcome),
            })
        }
        OpenSessionOutputEvent::ReplayComplete { cursor } => {
            wire::open_session_response::Event::ReplayComplete(wire::ReplayComplete {
                cursor: cursor.clone(),
            })
        }
    };
    wire::OpenSessionResponse { event: Some(event) }
}

fn session_output_event_from_wire(
    event: wire::OpenSessionResponse,
) -> Result<OpenSessionOutputEvent, wire::DecodeError> {
    let event = event
        .event
        .ok_or_else(|| wire::DecodeError::Invalid("OpenSessionResponse missing event".into()))?;
    match event {
        wire::open_session_response::Event::Opened(_) => Ok(OpenSessionOutputEvent::Opened),
        wire::open_session_response::Event::Output(output) => Ok(OpenSessionOutputEvent::Output {
            payload: output.payload,
        }),
        wire::open_session_response::Event::InputResult(input_result) => {
            let outcome = input_result.outcome.ok_or_else(|| {
                wire::DecodeError::Invalid("SessionInputResult missing outcome".into())
            })?;
            let result = match outcome {
                wire::session_input_result::Outcome::Accepted(_) => Ok(()),
                wire::session_input_result::Outcome::Error(error) => {
                    Err(wire::decode_protocol_error(error))
                }
            };
            Ok(OpenSessionOutputEvent::InputResult {
                input_id: input_result.input_id,
                result,
            })
        }
        wire::open_session_response::Event::ReplayComplete(replay_complete) => {
            Ok(OpenSessionOutputEvent::ReplayComplete {
                cursor: replay_complete.cursor,
            })
        }
    }
}

fn open_session_request_to_wire(request: &SessionOpenRequest) -> wire::SessionOpen {
    wire::SessionOpen {
        agent_id: uuid_to_bytes(request.agent_id),
        io_protocol: request.io_protocol.clone(),
        args: request.args.clone(),
    }
}

fn open_session_request_from_wire(
    request: wire::SessionOpen,
) -> Result<SessionOpenRequest, wire::DecodeError> {
    Ok(SessionOpenRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        io_protocol: request.io_protocol,
        args: request.args,
    })
}

fn open_session_client_frame_to_payload(
    frame: &OpenSessionClientFrame,
) -> Result<Vec<u8>, wire::EncodeError> {
    let event = match frame {
        OpenSessionClientFrame::Open(request) => {
            wire::open_session_request::Event::Open(open_session_request_to_wire(request))
        }
        OpenSessionClientFrame::Input(event) => {
            session_input_event_to_wire(event)?.event.ok_or_else(|| {
                wire::EncodeError::Invalid("OpenSessionRequest missing event".to_string())
            })?
        }
        OpenSessionClientFrame::Control { payload } => {
            wire::open_session_request::Event::Control(wire::SessionControl {
                payload: payload.clone(),
            })
        }
        OpenSessionClientFrame::Cancel => {
            return Err(wire::EncodeError::Invalid(
                "OpenSession cancel is encoded as a routed Cancel frame".to_string(),
            ));
        }
    };
    Ok(wire::OpenSessionRequest { event: Some(event) }.encode_to_vec())
}

pub(crate) fn decode_open_session_client_frame_payload(
    payload: &[u8],
) -> Result<OpenSessionClientFrame, wire::DecodeError> {
    let event = wire::OpenSessionRequest::decode(payload)?
        .event
        .ok_or_else(|| wire::DecodeError::Invalid("OpenSessionRequest missing event".into()))?;
    match event {
        wire::open_session_request::Event::Open(request) => Ok(OpenSessionClientFrame::Open(
            open_session_request_from_wire(request)?,
        )),
        wire::open_session_request::Event::Input(input) => Ok(OpenSessionClientFrame::Input(
            OpenSessionInputEvent::Input {
                input_id: input.input_id,
                payload: input.payload,
            },
        )),
        wire::open_session_request::Event::Control(control) => {
            Ok(OpenSessionClientFrame::Control {
                payload: control.payload,
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

fn response_body(payload: Vec<u8>) -> FrameBody {
    FrameBody {
        kind: Some(frame_body::Kind::Response(Response {
            outcome: Some(response::Outcome::Payload(payload)),
        })),
    }
}

fn error_response_body(error: &ProtocolError) -> FrameBody {
    FrameBody {
        kind: Some(frame_body::Kind::Response(Response {
            outcome: Some(response::Outcome::Error(wire::encode_protocol_error(error))),
        })),
    }
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
    fn create_agent_request_encodes_as_routed_frame_body() {
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

        let encoded = encode_agent_lifecycle_request(&request).unwrap();
        let body = FrameBody::decode(encoded.as_slice()).unwrap();
        let Some(frame_body::Kind::Request(request)) = body.kind else {
            panic!("expected request body");
        };
        assert_eq!(request.method, method::AGENT_CREATE_NAME);

        let request = wire::CreateAgentRequest::decode(request.payload.as_slice()).unwrap();
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

        let encoded = encode_agent_lifecycle_request(&request).unwrap();
        let decoded = decode_agent_lifecycle_request(&encoded).unwrap();
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

        let encoded = encode_agent_lifecycle_request(&request).unwrap();
        let decoded = decode_agent_lifecycle_request(&encoded).unwrap();
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

        let encoded = encode_agent_lifecycle_request(&request).unwrap();
        let decoded = decode_agent_lifecycle_request(&encoded).unwrap();
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

        let err = encode_agent_lifecycle_request(&request).unwrap_err();
        assert!(
            err.to_string().contains("working_dir must be valid UTF-8"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rename_and_delete_requests_roundtrip() {
        let rename_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        let rename =
            encode_agent_lifecycle_request(&AgentLifecycleRequest::Rename(RenameAgentRequest {
                agent_id: rename_id,
                name: "renamed".to_string(),
            }))
            .unwrap();
        let delete = encode_agent_lifecycle_request(&AgentLifecycleRequest::Delete {
            agent_id: delete_id,
        })
        .unwrap();

        let AgentLifecycleRequest::Rename(decoded) =
            decode_agent_lifecycle_request(&rename).unwrap()
        else {
            panic!("expected rename request");
        };
        assert_eq!(decoded.agent_id, rename_id);
        assert_eq!(decoded.name, "renamed");

        let AgentLifecycleRequest::Delete { agent_id } =
            decode_agent_lifecycle_request(&delete).unwrap()
        else {
            panic!("expected delete request");
        };
        assert_eq!(agent_id, delete_id);
    }

    #[test]
    fn delete_request_rejects_invalid_uuid_length() {
        let body = FrameBody {
            kind: Some(frame_body::Kind::Request(Request {
                method: method::AGENT_DELETE_NAME.to_string(),
                payload: DeleteAgentRequest {
                    agent_id: vec![1, 2, 3],
                }
                .encode_to_vec(),
            })),
        };

        let err = decode_agent_lifecycle_request(&body.encode_to_vec()).unwrap_err();
        assert!(
            err.to_string().contains("agent_id must be 16 bytes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_session_request_is_recognized_strictly() {
        let encoded = encode_open_session_request().unwrap();
        let FrameBody {
            kind: Some(frame_body::Kind::Request(wire_request)),
        } = FrameBody::decode(encoded.as_slice()).unwrap()
        else {
            panic!("expected OpenSession request frame");
        };
        assert_eq!(wire_request.method, method::AGENT_OPEN_SESSION_NAME);
        assert!(wire_request.payload.is_empty());
        assert_eq!(
            decode_open_session_request_if_present(&encoded).unwrap(),
            Some(())
        );

        let open = SessionOpenRequest {
            agent_id: Uuid::new_v4(),
            io_protocol: "claude_raw_v1".to_string(),
            args: Some(vec![1, 2, 3]),
        };
        let encoded_open = encode_open_session_open_event(&open).unwrap();
        let FrameBody {
            kind: Some(frame_body::Kind::StreamItem(item)),
        } = FrameBody::decode(encoded_open.as_slice()).unwrap()
        else {
            panic!("expected OpenSession open stream item");
        };
        let client_event = wire::OpenSessionRequest::decode(item.payload.as_slice()).unwrap();
        let Some(wire::open_session_request::Event::Open(decoded_open)) = client_event.event else {
            panic!("expected open event");
        };
        assert_eq!(open_session_request_from_wire(decoded_open).unwrap(), open);

        let body = FrameBody {
            kind: Some(frame_body::Kind::Request(Request {
                method: method::AGENT_LIST_NAME.to_string(),
                payload: wire::Empty {}.encode_to_vec(),
            })),
        };
        assert_eq!(
            decode_open_session_request_if_present(&body.encode_to_vec()).unwrap(),
            None
        );

        let body = FrameBody {
            kind: Some(frame_body::Kind::Request(Request {
                method: method::AGENT_OPEN_SESSION_NAME.to_string(),
                payload: vec![1, 2, 3],
            })),
        };
        let err = decode_open_session_request_if_present(&body.encode_to_vec()).unwrap_err();
        assert!(
            err.source().to_string().contains("must be Empty"),
            "unexpected error: {}",
            err.source()
        );
    }

    #[test]
    fn open_session_input_events_roundtrip() {
        let events = [
            OpenSessionInputEvent::Input {
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                payload: b"hello".to_vec(),
            },
            OpenSessionInputEvent::Control {
                payload: b"resize".to_vec(),
            },
        ];

        for event in events {
            let encoded = encode_open_session_input_event(&event).unwrap();
            let FrameBody {
                kind: Some(frame_body::Kind::StreamItem(item)),
            } = FrameBody::decode(encoded.as_slice()).unwrap()
            else {
                panic!("expected OpenSession input stream item");
            };
            let client_event = wire::OpenSessionRequest::decode(item.payload.as_slice()).unwrap();
            assert!(matches!(
                client_event.event,
                Some(
                    wire::open_session_request::Event::Input(_)
                        | wire::open_session_request::Event::Control(_)
                )
            ));
            if matches!(event, OpenSessionInputEvent::Input { .. }) {
                let decoded = decode_open_session_input_event(&encoded).unwrap();
                assert_eq!(decoded, event);
            }
        }
    }

    #[test]
    fn open_session_output_events_roundtrip() {
        let events = [
            OpenSessionOutputEvent::Opened,
            OpenSessionOutputEvent::Output {
                payload: b"hello".to_vec(),
            },
            OpenSessionOutputEvent::InputResult {
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                result: Ok(()),
            },
            OpenSessionOutputEvent::InputResult {
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                result: Err(ProtocolError::NoAgentFound),
            },
            OpenSessionOutputEvent::ReplayComplete {
                cursor: Some(b"cursor-2".to_vec()),
            },
        ];

        for event in events {
            let encoded = encode_open_session_output_event(&event).unwrap();
            let decoded = decode_open_session_output_event(&encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn open_session_response_roundtrips_success_and_error() {
        let success = encode_open_session_response(Ok(())).unwrap();
        let error = encode_open_session_response(Err(ProtocolError::Unimplemented {
            message: "not yet".to_string(),
        }))
        .unwrap();

        assert_eq!(decode_open_session_response(&success).unwrap(), Ok(()));
        assert_eq!(
            decode_open_session_response(&error).unwrap(),
            Err(ProtocolError::Unimplemented {
                message: "not yet".to_string()
            })
        );
    }

    #[test]
    fn create_response_encodes_as_routed_frame_body() {
        let agent = sample_agent_record();
        let encoded =
            encode_agent_lifecycle_response(&AgentLifecycleResponse::Create(Ok(agent.clone())))
                .unwrap();

        let body = FrameBody::decode(encoded.as_slice()).unwrap();
        let Some(frame_body::Kind::Response(response)) = body.kind else {
            panic!("expected response body");
        };
        let Some(response::Outcome::Payload(payload)) = response.outcome else {
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
        let success =
            encode_agent_lifecycle_response(&AgentLifecycleResponse::Create(Ok(agent.clone())))
                .unwrap();
        let error = encode_agent_lifecycle_response(&AgentLifecycleResponse::Create(Err(
            ProtocolError::ServerError {
                message: "boom".to_string(),
            },
        )))
        .unwrap();

        let AgentLifecycleResponse::Create(Ok(decoded)) =
            decode_agent_lifecycle_response(method::AGENT_CREATE_NAME, &success).unwrap()
        else {
            panic!("expected create success");
        };
        assert_eq!(decoded, agent);

        let AgentLifecycleResponse::Create(Err(ProtocolError::ServerError { message })) =
            decode_agent_lifecycle_response(method::AGENT_CREATE_NAME, &error).unwrap()
        else {
            panic!("expected create error");
        };
        assert_eq!(message, "boom");
    }

    #[test]
    fn rename_and_delete_responses_roundtrip() {
        let agent = sample_agent_record();
        let rename =
            encode_agent_lifecycle_response(&AgentLifecycleResponse::Rename(Ok(agent.clone())))
                .unwrap();
        let delete =
            encode_agent_lifecycle_response(&AgentLifecycleResponse::Delete(Ok(()))).unwrap();

        let AgentLifecycleResponse::Rename(Ok(decoded)) =
            decode_agent_lifecycle_response(method::AGENT_RENAME_NAME, &rename).unwrap()
        else {
            panic!("expected rename success");
        };
        assert_eq!(decoded, agent);

        let AgentLifecycleResponse::Delete(Ok(())) =
            decode_agent_lifecycle_response(method::AGENT_DELETE_NAME, &delete).unwrap()
        else {
            panic!("expected delete success");
        };
    }

    #[test]
    fn response_decode_rejects_wrong_body_kind() {
        let request = encode_agent_lifecycle_request(&AgentLifecycleRequest::Delete {
            agent_id: Uuid::new_v4(),
        })
        .unwrap();

        let err = decode_agent_lifecycle_response(method::AGENT_DELETE_NAME, &request).unwrap_err();
        assert!(
            err.to_string().contains("must be a response"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn response_decode_preserves_multiple_agent_io_protocols() {
        let agent = sample_agent_record();
        let mut wire_agent = agent_to_wire(&agent).unwrap();
        wire_agent.io_protocols = vec![
            CLAUDE_RAW_V1.to_string(),
            CLAUDE_PTY_TRANSCRIPT_V1.to_string(),
        ];
        let body = response_body(
            wire::CreateAgentResponse {
                agent: Some(wire_agent.clone()),
            }
            .encode_to_vec(),
        );

        let AgentLifecycleResponse::Create(Ok(decoded)) =
            decode_agent_lifecycle_response(method::AGENT_CREATE_NAME, &body.encode_to_vec())
                .unwrap()
        else {
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
        let body = response_body(wire::CreateAgentResponse { agent: Some(agent) }.encode_to_vec());

        let err = decode_agent_lifecycle_response(method::AGENT_CREATE_NAME, &body.encode_to_vec())
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid agent created_at"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn delete_response_rejects_non_empty_success_payload() {
        let body = response_body(
            wire::DebugResponse {
                dump: "not empty".to_string(),
            }
            .encode_to_vec(),
        );

        let err = decode_agent_lifecycle_response(method::AGENT_DELETE_NAME, &body.encode_to_vec())
            .unwrap_err();
        assert!(
            err.to_string().contains("Empty payload must be empty"),
            "unexpected error: {err}"
        );
    }
}
