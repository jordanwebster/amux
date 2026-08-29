use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use prost::Message as ProstMessage;
use protocol_wire::DeleteAgentRequest;
use uuid::Uuid;

use super::{
    Agent, AgentKind, AgentParent, ClaudeDriver, Protocol, SessionCloseReason,
    SubscribeSessionEvent, WorkingOn,
};
use crate::agents::{RenameAgentRequest, TerminalSize};
use crate::envelope::{AgentSender, Envelope, EnvelopeKind, Sender};
use crate::protocol::wire::{self as protocol_wire, pb};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionInputEvent {
    Input { input_id: Vec<u8>, payload: Vec<u8> },
    Control { payload: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscribeSessionRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) protocol: Protocol,
    pub(crate) args: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendInputRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) protocol: Protocol,
    pub(crate) event: SessionInputEvent,
}

#[derive(Debug, Clone)]
pub(crate) struct CreateAgentRpcRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) parent: Option<AgentParent>,
    pub(crate) initial_prompt: Option<String>,
    pub(crate) agent: CreateAgentConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct SetAgentStatusRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) working_on: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum CreateAgentConfig {
    Claude {
        driver: ClaudeDriver,
        working_dir: PathBuf,
        args: Vec<String>,
        terminal_size: Option<TerminalSize>,
    },
    Codex {
        cwd: PathBuf,
        model: Option<String>,
        approval_policy: Option<String>,
        sandbox_policy: Option<String>,
        resume_thread_id: Option<String>,
    },
    TestAgent {
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
    },
}

#[cfg(test)]
pub(crate) fn encode_session_output_event_payload(
    event: &SubscribeSessionEvent,
    protocol: Protocol,
) -> Vec<u8> {
    session_output_event_to_wire(event, protocol)
        .expect("test event must contain a valid protocol payload")
        .encode_to_vec()
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
    let (protocol, args) = subscribe_protocol_from_agent_wire(request.protocol)?;
    Ok(SubscribeSessionRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        protocol,
        args,
    })
}

pub(crate) fn send_input_request_from_wire(
    request: pb::SendInputRequest,
) -> Result<SendInputRequest, protocol_wire::DecodeError> {
    let event = request.event.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SendInputRequest missing event".into())
    })?;
    let (protocol, event) = send_input_event_from_agent_wire(request.input_id, event)?;
    Ok(SendInputRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        protocol,
        event,
    })
}

pub(crate) fn subscribe_protocol_from_client_wire(
    protocol: Option<pb::client_subscribe_session_request::Protocol>,
) -> Result<(Protocol, Option<Vec<u8>>), protocol_wire::DecodeError> {
    let protocol = protocol.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("ClientSubscribeSessionRequest missing protocol".into())
    })?;
    Ok(match protocol {
        pb::client_subscribe_session_request::Protocol::TerminalV1(args) => {
            (Protocol::TerminalV1, Some(args.encode_to_vec()))
        }
        pb::client_subscribe_session_request::Protocol::ClaudePtyTranscriptV1(args) => {
            (Protocol::ClaudePtyTranscriptV1, Some(args.encode_to_vec()))
        }
        pb::client_subscribe_session_request::Protocol::ClaudeSdkV1(args) => {
            (Protocol::ClaudeSdkV1, Some(args.encode_to_vec()))
        }
        pb::client_subscribe_session_request::Protocol::CodexSdkV1(args) => {
            (Protocol::CodexSdkV1, Some(args.encode_to_vec()))
        }
        pb::client_subscribe_session_request::Protocol::TestEchoV1(_) => {
            (Protocol::TestEchoV1, None)
        }
    })
}

fn subscribe_protocol_from_agent_wire(
    protocol: Option<pb::subscribe_session_request::Protocol>,
) -> Result<(Protocol, Option<Vec<u8>>), protocol_wire::DecodeError> {
    let protocol = protocol.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SubscribeSessionRequest missing protocol".into())
    })?;
    Ok(match protocol {
        pb::subscribe_session_request::Protocol::TerminalV1(args) => {
            (Protocol::TerminalV1, Some(args.encode_to_vec()))
        }
        pb::subscribe_session_request::Protocol::ClaudePtyTranscriptV1(args) => {
            (Protocol::ClaudePtyTranscriptV1, Some(args.encode_to_vec()))
        }
        pb::subscribe_session_request::Protocol::ClaudeSdkV1(args) => {
            (Protocol::ClaudeSdkV1, Some(args.encode_to_vec()))
        }
        pb::subscribe_session_request::Protocol::CodexSdkV1(args) => {
            (Protocol::CodexSdkV1, Some(args.encode_to_vec()))
        }
        pb::subscribe_session_request::Protocol::TestEchoV1(_) => (Protocol::TestEchoV1, None),
    })
}

pub(crate) fn subscribe_protocol_to_agent_wire(
    protocol: Protocol,
    args: Option<&[u8]>,
) -> Result<pb::subscribe_session_request::Protocol, protocol_wire::DecodeError> {
    Ok(match protocol {
        Protocol::TerminalV1 => pb::subscribe_session_request::Protocol::TerminalV1(
            decode_optional_message(args, "TerminalV1Args")?,
        ),
        Protocol::ClaudePtyTranscriptV1 => {
            pb::subscribe_session_request::Protocol::ClaudePtyTranscriptV1(decode_optional_message(
                args,
                "ClaudePtyTranscriptV1Args",
            )?)
        }
        Protocol::ClaudeSdkV1 => pb::subscribe_session_request::Protocol::ClaudeSdkV1(
            decode_optional_message(args, "ClaudeSdkV1Args")?,
        ),
        Protocol::CodexSdkV1 => pb::subscribe_session_request::Protocol::CodexSdkV1(
            decode_optional_message(args, "CodexSdkV1Args")?,
        ),
        Protocol::TestEchoV1 => {
            reject_args(args, "TestEchoV1Args")?;
            pb::subscribe_session_request::Protocol::TestEchoV1(pb::TestEchoV1Args {})
        }
    })
}

pub(crate) fn subscribe_protocol_to_client_wire(
    protocol: Protocol,
    args: Option<&[u8]>,
) -> Result<pb::client_subscribe_session_request::Protocol, protocol_wire::DecodeError> {
    Ok(match protocol {
        Protocol::TerminalV1 => pb::client_subscribe_session_request::Protocol::TerminalV1(
            decode_optional_message(args, "TerminalV1Args")?,
        ),
        Protocol::ClaudePtyTranscriptV1 => {
            pb::client_subscribe_session_request::Protocol::ClaudePtyTranscriptV1(
                decode_optional_message(args, "ClaudePtyTranscriptV1Args")?,
            )
        }
        Protocol::ClaudeSdkV1 => pb::client_subscribe_session_request::Protocol::ClaudeSdkV1(
            decode_optional_message(args, "ClaudeSdkV1Args")?,
        ),
        Protocol::CodexSdkV1 => pb::client_subscribe_session_request::Protocol::CodexSdkV1(
            decode_optional_message(args, "CodexSdkV1Args")?,
        ),
        Protocol::TestEchoV1 => {
            reject_args(args, "TestEchoV1Args")?;
            pb::client_subscribe_session_request::Protocol::TestEchoV1(pb::TestEchoV1Args {})
        }
    })
}

pub(crate) fn send_input_event_from_client_wire(
    input_id: Vec<u8>,
    event: Option<pb::client_send_input_request::Event>,
) -> Result<(Protocol, SessionInputEvent), protocol_wire::DecodeError> {
    let event = event.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("ClientSendInputRequest missing event".into())
    })?;
    Ok(match event {
        pb::client_send_input_request::Event::TerminalV1(input) => (
            Protocol::TerminalV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.payload,
            },
        ),
        pb::client_send_input_request::Event::ClaudePtyTranscriptV1(input) => (
            Protocol::ClaudePtyTranscriptV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::client_send_input_request::Event::ClaudeSdkV1(input) => (
            Protocol::ClaudeSdkV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::client_send_input_request::Event::CodexSdkV1(input) => (
            Protocol::CodexSdkV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::client_send_input_request::Event::TestEchoV1(input) => (
            Protocol::TestEchoV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.payload,
            },
        ),
        pb::client_send_input_request::Event::Control(control) => (
            Protocol::TerminalV1,
            SessionInputEvent::Control {
                payload: control.encode_to_vec(),
            },
        ),
    })
}

fn send_input_event_from_agent_wire(
    input_id: Vec<u8>,
    event: pb::send_input_request::Event,
) -> Result<(Protocol, SessionInputEvent), protocol_wire::DecodeError> {
    Ok(match event {
        pb::send_input_request::Event::TerminalV1(input) => (
            Protocol::TerminalV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.payload,
            },
        ),
        pb::send_input_request::Event::ClaudePtyTranscriptV1(input) => (
            Protocol::ClaudePtyTranscriptV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::send_input_request::Event::ClaudeSdkV1(input) => (
            Protocol::ClaudeSdkV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::send_input_request::Event::CodexSdkV1(input) => (
            Protocol::CodexSdkV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.encode_to_vec(),
            },
        ),
        pb::send_input_request::Event::TestEchoV1(input) => (
            Protocol::TestEchoV1,
            SessionInputEvent::Input {
                input_id,
                payload: input.payload,
            },
        ),
        pb::send_input_request::Event::Control(control) => (
            Protocol::TerminalV1,
            SessionInputEvent::Control {
                payload: control.encode_to_vec(),
            },
        ),
    })
}

pub(crate) fn send_input_event_to_agent_wire(
    protocol: Protocol,
    event: &SessionInputEvent,
) -> Result<(Vec<u8>, pb::send_input_request::Event), protocol_wire::DecodeError> {
    let (input_id, event) = send_input_event_to_wire(protocol, event)?;
    let event = match event {
        OutboundInput::Terminal(input) => pb::send_input_request::Event::TerminalV1(input),
        OutboundInput::ClaudePty(input) => {
            pb::send_input_request::Event::ClaudePtyTranscriptV1(input)
        }
        OutboundInput::ClaudeSdk(input) => pb::send_input_request::Event::ClaudeSdkV1(input),
        OutboundInput::Codex(input) => pb::send_input_request::Event::CodexSdkV1(input),
        OutboundInput::TestEcho(input) => pb::send_input_request::Event::TestEchoV1(input),
        OutboundInput::Control(control) => pb::send_input_request::Event::Control(control),
    };
    Ok((input_id, event))
}

pub(crate) fn send_input_event_to_client_wire(
    protocol: Protocol,
    event: &SessionInputEvent,
) -> Result<(Vec<u8>, pb::client_send_input_request::Event), protocol_wire::DecodeError> {
    let (input_id, event) = send_input_event_to_wire(protocol, event)?;
    let event = match event {
        OutboundInput::Terminal(input) => pb::client_send_input_request::Event::TerminalV1(input),
        OutboundInput::ClaudePty(input) => {
            pb::client_send_input_request::Event::ClaudePtyTranscriptV1(input)
        }
        OutboundInput::ClaudeSdk(input) => pb::client_send_input_request::Event::ClaudeSdkV1(input),
        OutboundInput::Codex(input) => pb::client_send_input_request::Event::CodexSdkV1(input),
        OutboundInput::TestEcho(input) => pb::client_send_input_request::Event::TestEchoV1(input),
        OutboundInput::Control(control) => pb::client_send_input_request::Event::Control(control),
    };
    Ok((input_id, event))
}

enum OutboundInput {
    Terminal(pb::TerminalV1Input),
    ClaudePty(pb::ClaudePtyTranscriptV1Input),
    ClaudeSdk(pb::ClaudeSdkV1Input),
    Codex(pb::CodexSdkV1Input),
    TestEcho(pb::TestEchoV1Input),
    Control(pb::SessionControl),
}

fn send_input_event_to_wire(
    protocol: Protocol,
    event: &SessionInputEvent,
) -> Result<(Vec<u8>, OutboundInput), protocol_wire::DecodeError> {
    match event {
        SessionInputEvent::Control { payload } => Ok((
            Vec::new(),
            OutboundInput::Control(decode_message(payload, "SessionControl")?),
        )),
        SessionInputEvent::Input { input_id, payload } => {
            let event = match protocol {
                Protocol::TerminalV1 => OutboundInput::Terminal(pb::TerminalV1Input {
                    payload: payload.clone(),
                }),
                Protocol::ClaudePtyTranscriptV1 => {
                    OutboundInput::ClaudePty(decode_message(payload, "ClaudePtyTranscriptV1Input")?)
                }
                Protocol::ClaudeSdkV1 => {
                    OutboundInput::ClaudeSdk(decode_message(payload, "ClaudeSdkV1Input")?)
                }
                Protocol::CodexSdkV1 => {
                    OutboundInput::Codex(decode_message(payload, "CodexSdkV1Input")?)
                }
                Protocol::TestEchoV1 => OutboundInput::TestEcho(pb::TestEchoV1Input {
                    payload: payload.clone(),
                }),
            };
            Ok((input_id.clone(), event))
        }
    }
}

fn decode_optional_message<M: ProstMessage + Default>(
    bytes: Option<&[u8]>,
    name: &str,
) -> Result<M, protocol_wire::DecodeError> {
    match bytes {
        Some(bytes) => decode_message(bytes, name),
        None => Ok(M::default()),
    }
}

fn decode_message<M: ProstMessage + Default>(
    bytes: &[u8],
    name: &str,
) -> Result<M, protocol_wire::DecodeError> {
    M::decode(bytes).map_err(|error| {
        protocol_wire::DecodeError::Invalid(format!("invalid {name} protobuf: {error}"))
    })
}

fn reject_args(args: Option<&[u8]>, name: &str) -> Result<(), protocol_wire::DecodeError> {
    if args.is_some_and(|args| !args.is_empty()) {
        return Err(protocol_wire::DecodeError::Invalid(format!(
            "{name} does not accept arguments"
        )));
    }
    Ok(())
}

pub(crate) fn session_output_event_to_wire(
    event: &SubscribeSessionEvent,
    protocol: Protocol,
) -> Result<pb::SubscribeSessionResponse, protocol_wire::DecodeError> {
    let event = match event {
        SubscribeSessionEvent::Opened => {
            pb::subscribe_session_response::Event::Opened(pb::SessionOpened {})
        }
        SubscribeSessionEvent::Output { payload } => {
            pb::subscribe_session_response::Event::Output(pb::SessionOutput {
                output: Some(session_output_to_wire(protocol, payload)?),
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
    Ok(pb::SubscribeSessionResponse { event: Some(event) })
}

fn session_output_to_wire(
    protocol: Protocol,
    payload: &[u8],
) -> Result<pb::session_output::Output, protocol_wire::DecodeError> {
    Ok(match protocol {
        Protocol::TerminalV1 => pb::session_output::Output::TerminalV1(pb::TerminalV1Output {
            payload: payload.to_vec(),
        }),
        Protocol::ClaudePtyTranscriptV1 => pb::session_output::Output::ClaudePtyTranscriptV1(
            decode_message(payload, "ClaudePtyTranscriptV1Output")?,
        ),
        Protocol::ClaudeSdkV1 => {
            pb::session_output::Output::ClaudeSdkV1(decode_message(payload, "ClaudeSdkV1Output")?)
        }
        Protocol::CodexSdkV1 => {
            pb::session_output::Output::CodexSdkV1(decode_message(payload, "CodexSdkV1Output")?)
        }
        Protocol::TestEchoV1 => pb::session_output::Output::TestEchoV1(pb::TestEchoV1Output {
            payload: payload.to_vec(),
        }),
    })
}

pub(crate) fn session_output_payload_from_wire(
    output: pb::SessionOutput,
) -> Result<Vec<u8>, protocol_wire::DecodeError> {
    let output = output.output.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("SessionOutput missing output".into())
    })?;
    Ok(match output {
        pb::session_output::Output::TerminalV1(output) => output.payload,
        pb::session_output::Output::ClaudePtyTranscriptV1(output) => output.encode_to_vec(),
        pb::session_output::Output::ClaudeSdkV1(output) => output.encode_to_vec(),
        pb::session_output::Output::CodexSdkV1(output) => output.encode_to_vec(),
        pb::session_output::Output::TestEchoV1(output) => output.payload,
    })
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
                payload: session_output_payload_from_wire(output)?,
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
        protocol_wire::create_agent_request::Agent::Claude(claude) => CreateAgentConfig::Claude {
            driver: claude_driver_from_wire(claude.driver)?,
            working_dir: PathBuf::from(claude.working_dir),
            args: claude.args,
            terminal_size: claude
                .initial_terminal_size
                .map(terminal_size_from_wire)
                .transpose()?,
        },
        protocol_wire::create_agent_request::Agent::Codex(codex) => CreateAgentConfig::Codex {
            cwd: PathBuf::from(codex.cwd),
            model: codex.model,
            approval_policy: codex.approval_policy,
            sandbox_policy: codex.sandbox_policy,
            resume_thread_id: codex.resume_thread_id,
        },
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
        parent: request.parent.map(agent_parent_from_wire).transpose()?,
        initial_prompt: request.initial_prompt,
        agent,
    })
}

pub(crate) fn envelope_from_wire(
    envelope: protocol_wire::Envelope,
) -> Result<Envelope, protocol_wire::DecodeError> {
    let from = envelope
        .from
        .and_then(|sender| sender.value)
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("Envelope missing from".into()))?;
    let from = match from {
        protocol_wire::sender::Value::Agent(agent) => Sender::Agent(AgentSender {
            agent_id: required_uuid_from_bytes("from.agent_id", agent.agent_id)?,
            host_id: required_uuid_from_bytes("from.host_id", agent.host_id)?,
            name: agent.name,
            kind: agent.kind,
        }),
        protocol_wire::sender::Value::Human(_) => Sender::Human,
    };
    let kind = match protocol_wire::EnvelopeKind::try_from(envelope.kind) {
        Ok(protocol_wire::EnvelopeKind::Message) => EnvelopeKind::Message,
        Ok(protocol_wire::EnvelopeKind::Completed) => EnvelopeKind::Completed,
        Ok(protocol_wire::EnvelopeKind::Exited) => EnvelopeKind::Exited,
        Ok(protocol_wire::EnvelopeKind::Unspecified) | Err(_) => {
            return Err(protocol_wire::DecodeError::Invalid(
                "Envelope kind must be specified".into(),
            ));
        }
    };
    Ok(Envelope {
        id: required_uuid_from_bytes("id", envelope.id)?,
        context: envelope
            .context
            .map(|context| required_uuid_from_bytes("context", context))
            .transpose()?,
        from,
        to: envelope
            .to
            .map(agent_parent_from_wire)
            .transpose()?
            .ok_or_else(|| protocol_wire::DecodeError::Invalid("Envelope missing to".into()))?,
        kind,
        text: envelope.text,
    })
}

pub(crate) fn envelope_to_wire(envelope: &Envelope) -> protocol_wire::Envelope {
    let value = match &envelope.from {
        Sender::Agent(agent) => protocol_wire::sender::Value::Agent(protocol_wire::AgentSender {
            agent_id: uuid_to_bytes(agent.agent_id),
            host_id: uuid_to_bytes(agent.host_id),
            name: agent.name.clone(),
            kind: agent.kind.clone(),
        }),
        Sender::Human => protocol_wire::sender::Value::Human(protocol_wire::Human {}),
    };
    let kind = match envelope.kind {
        EnvelopeKind::Message => protocol_wire::EnvelopeKind::Message,
        EnvelopeKind::Completed => protocol_wire::EnvelopeKind::Completed,
        EnvelopeKind::Exited => protocol_wire::EnvelopeKind::Exited,
    };
    protocol_wire::Envelope {
        id: uuid_to_bytes(envelope.id),
        context: envelope.context.map(uuid_to_bytes),
        from: Some(protocol_wire::Sender { value: Some(value) }),
        to: Some(agent_parent_to_wire(envelope.to)),
        kind: kind as i32,
        text: envelope.text.clone(),
    }
}

pub(crate) fn set_agent_status_request_from_wire(
    request: protocol_wire::SetAgentStatusRequest,
) -> Result<SetAgentStatusRequest, protocol_wire::DecodeError> {
    Ok(SetAgentStatusRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        working_on: request.working_on,
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
        kind: Some(agent_kind_to_wire(agent.kind)),
        readonly: agent.readonly,
        args: agent.args.clone(),
        created_at_unix_ms: agent.created_at.timestamp_millis(),
        parent: agent.parent.map(agent_parent_to_wire),
        working_on: agent.working_on.as_ref().map(working_on_to_wire),
    })
}

pub(crate) fn agent_from_wire(
    agent: protocol_wire::Agent,
) -> Result<Agent, protocol_wire::DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("invalid agent created_at".into()))?;

    let parent = agent.parent.map(agent_parent_from_wire).transpose()?;
    let working_on = agent.working_on.map(working_on_from_wire).transpose()?;

    let kind = agent_kind_from_wire(
        agent
            .kind
            .ok_or_else(|| protocol_wire::DecodeError::Invalid("Agent missing kind".into()))?,
    )?;

    Ok(Agent {
        id: required_uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: required_uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        kind,
        readonly: agent.readonly,
        args: agent.args,
        created_at,
        parent,
        working_on,
    })
}

pub(crate) fn agent_kind_to_wire(kind: AgentKind) -> protocol_wire::AgentKind {
    let kind = match kind {
        AgentKind::Claude { driver } => {
            protocol_wire::agent_kind::Kind::Claude(protocol_wire::ClaudeKind {
                driver: claude_driver_to_wire(driver) as i32,
            })
        }
        AgentKind::Codex => protocol_wire::agent_kind::Kind::Codex(protocol_wire::CodexKind {}),
        AgentKind::TestAgent => {
            protocol_wire::agent_kind::Kind::TestAgent(protocol_wire::TestAgentKind {})
        }
    };
    protocol_wire::AgentKind { kind: Some(kind) }
}

pub(crate) fn agent_kind_from_wire(
    kind: protocol_wire::AgentKind,
) -> Result<AgentKind, protocol_wire::DecodeError> {
    let kind = kind
        .kind
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("AgentKind missing kind".into()))?;
    Ok(match kind {
        protocol_wire::agent_kind::Kind::Claude(claude) => AgentKind::Claude {
            driver: claude_driver_from_wire(claude.driver)?,
        },
        protocol_wire::agent_kind::Kind::Codex(_) => AgentKind::Codex,
        protocol_wire::agent_kind::Kind::TestAgent(_) => AgentKind::TestAgent,
    })
}

pub(crate) const fn claude_driver_to_wire(driver: ClaudeDriver) -> protocol_wire::ClaudeDriver {
    match driver {
        ClaudeDriver::Pty => protocol_wire::ClaudeDriver::Pty,
        ClaudeDriver::Sdk => protocol_wire::ClaudeDriver::Sdk,
    }
}

pub(crate) fn claude_driver_from_wire(
    driver: i32,
) -> Result<ClaudeDriver, protocol_wire::DecodeError> {
    match protocol_wire::ClaudeDriver::try_from(driver) {
        Ok(protocol_wire::ClaudeDriver::Pty) => Ok(ClaudeDriver::Pty),
        Ok(protocol_wire::ClaudeDriver::Sdk) => Ok(ClaudeDriver::Sdk),
        Ok(protocol_wire::ClaudeDriver::Unspecified) | Err(_) => Err(
            protocol_wire::DecodeError::Invalid("ClaudeDriver must be specified".into()),
        ),
    }
}

pub(crate) fn agent_parent_to_wire(parent: AgentParent) -> protocol_wire::AgentParent {
    protocol_wire::AgentParent {
        agent_id: uuid_to_bytes(parent.agent_id),
        host_id: uuid_to_bytes(parent.host_id),
    }
}

pub(crate) fn agent_parent_from_wire(
    parent: protocol_wire::AgentParent,
) -> Result<AgentParent, protocol_wire::DecodeError> {
    Ok(AgentParent {
        agent_id: required_uuid_from_bytes("parent.agent_id", parent.agent_id)?,
        host_id: required_uuid_from_bytes("parent.host_id", parent.host_id)?,
    })
}

fn working_on_to_wire(working_on: &WorkingOn) -> protocol_wire::WorkingOn {
    protocol_wire::WorkingOn {
        text: working_on.text.clone(),
        updated_at_unix_ms: working_on.updated_at.timestamp_millis(),
    }
}

fn working_on_from_wire(
    working_on: protocol_wire::WorkingOn,
) -> Result<WorkingOn, protocol_wire::DecodeError> {
    let updated_at = Utc
        .timestamp_millis_opt(working_on.updated_at_unix_ms)
        .single()
        .ok_or_else(|| {
            protocol_wire::DecodeError::Invalid("invalid working_on.updated_at".into())
        })?;
    Ok(WorkingOn {
        text: working_on.text,
        updated_at,
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
    fn agent_kinds_roundtrip_every_variant() {
        for kind in [
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            AgentKind::Codex,
            AgentKind::TestAgent,
        ] {
            assert_eq!(
                agent_kind_from_wire(agent_kind_to_wire(kind)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn a2a_record_roundtrip() {
        let parent = AgentParent {
            agent_id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
        };
        let updated_at = Utc.timestamp_millis_opt(1_777_777_777_777).unwrap();
        let record = crate::agents::AgentRecord {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("child".to_string()),
            command: "codex".to_string(),
            working_dir: PathBuf::from("/tmp/work"),
            kind: AgentKind::Codex,
            readonly: false,
            args: vec!["--model".to_string(), "gpt-5.6".to_string()],
            created_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
            parent: Some(parent),
            working_on: Some(WorkingOn {
                text: "implement the record".to_string(),
                updated_at,
            }),
        };

        let dto = Agent::from(record);
        let wire = agent_to_wire(&dto).unwrap();
        let decoded = agent_from_wire(wire).unwrap();

        assert_eq!(decoded, dto);
        assert_eq!(decoded.parent, Some(parent));
        assert_eq!(decoded.working_on.unwrap().updated_at, updated_at);
    }

    #[test]
    fn create_agent_request_decodes_claude_create_config() {
        let agent_id = Uuid::new_v4();
        let parent = AgentParent {
            agent_id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
        };
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(agent_id),
            name: Some("dev".to_string()),
            parent: Some(agent_parent_to_wire(parent)),
            initial_prompt: Some("start here".to_string()),
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: vec!["--resume".to_string(), "abc".to_string()],
                    initial_terminal_size: Some(protocol_wire::TerminalSize {
                        rows: 40,
                        cols: 120,
                    }),
                    driver: protocol_wire::ClaudeDriver::Sdk as i32,
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.name.as_deref(), Some("dev"));
        assert_eq!(decoded.parent, Some(parent));
        assert_eq!(decoded.initial_prompt.as_deref(), Some("start here"));
        let CreateAgentConfig::Claude {
            driver,
            working_dir,
            args,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected Claude create config");
        };
        assert_eq!(driver, ClaudeDriver::Sdk);
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
    fn create_agent_request_decodes_codex_create_config() {
        let agent_id = Uuid::new_v4();
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(agent_id),
            name: Some("codex-dev".to_string()),
            parent: None,
            initial_prompt: None,
            agent: Some(protocol_wire::create_agent_request::Agent::Codex(
                protocol_wire::CodexCreateConfig {
                    cwd: "/tmp/work".to_string(),
                    model: Some("gpt-5.6-sol".to_string()),
                    approval_policy: Some("on-request".to_string()),
                    sandbox_policy: Some("workspace-write".to_string()),
                    resume_thread_id: Some("thread-7".to_string()),
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.name.as_deref(), Some("codex-dev"));
        assert!(matches!(
            decoded.agent,
            CreateAgentConfig::Codex {
                ref cwd,
                ref model,
                ref approval_policy,
                ref sandbox_policy,
                ref resume_thread_id,
            } if cwd == Path::new("/tmp/work")
                && model.as_deref() == Some("gpt-5.6-sol")
                && approval_policy.as_deref() == Some("on-request")
                && sandbox_policy.as_deref() == Some("workspace-write")
                && resume_thread_id.as_deref() == Some("thread-7")
        ));
    }

    #[test]
    fn create_agent_request_rejects_empty_agent_id() {
        let request = protocol_wire::CreateAgentRequest {
            agent_id: Vec::new(),
            name: None,
            parent: None,
            initial_prompt: None,
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: Vec::new(),
                    initial_terminal_size: None,
                    driver: protocol_wire::ClaudeDriver::Pty as i32,
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
            parent: None,
            initial_prompt: None,
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
            kind: AgentKind::Claude {
                driver: crate::agents::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
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
    fn message_envelope_roundtrips_through_wire() {
        let envelope = Envelope {
            id: Uuid::new_v4(),
            context: Some(Uuid::new_v4()),
            from: Sender::Agent(AgentSender {
                agent_id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
                name: "sender".to_string(),
                kind: "codex".to_string(),
            }),
            to: AgentParent {
                agent_id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
            },
            kind: EnvelopeKind::Completed,
            text: "done".to_string(),
        };

        let decoded = envelope_from_wire(envelope_to_wire(&envelope)).unwrap();

        assert_eq!(decoded, envelope);
    }

    #[test]
    fn status_request_decodes_optional_work() {
        let agent_id = Uuid::new_v4();
        let decoded = set_agent_status_request_from_wire(protocol_wire::SetAgentStatusRequest {
            agent_id: uuid_to_bytes(agent_id),
            working_on: Some("checking protocol".to_string()),
        })
        .unwrap();

        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.working_on.as_deref(), Some("checking protocol"));
    }

    #[test]
    fn subscribe_session_requests_roundtrip_every_protocol() {
        let protocols = [
            pb::subscribe_session_request::Protocol::TerminalV1(pb::TerminalV1Args {
                terminal_size: Some(pb::TerminalSize { rows: 24, cols: 80 }),
                replay_query: None,
            }),
            pb::subscribe_session_request::Protocol::ClaudePtyTranscriptV1(
                pb::ClaudePtyTranscriptV1Args {
                    terminal_size: None,
                    replay_query: Some(pb::ClaudePtyTranscriptV1ReplayQuery {
                        query: Some(pb::claude_pty_transcript_v1_replay_query::Query::Since(4)),
                    }),
                },
            ),
            pb::subscribe_session_request::Protocol::ClaudeSdkV1(pb::ClaudeSdkV1Args {
                replay_query: Some(pb::ClaudeSdkV1ReplayQuery {
                    query: Some(pb::claude_sdk_v1_replay_query::Query::TailCount(3)),
                }),
            }),
            pb::subscribe_session_request::Protocol::ClaudeSdkV1(pb::ClaudeSdkV1Args {
                replay_query: Some(pb::ClaudeSdkV1ReplayQuery {
                    query: Some(pb::claude_sdk_v1_replay_query::Query::Since(9)),
                }),
            }),
            pb::subscribe_session_request::Protocol::CodexSdkV1(pb::CodexSdkV1Args {
                replay_query: Some(pb::CodexSdkV1ReplayQuery {
                    query: Some(pb::codex_sdk_v1_replay_query::Query::Since(7)),
                }),
            }),
            pb::subscribe_session_request::Protocol::TestEchoV1(pb::TestEchoV1Args {}),
        ];

        for protocol in protocols {
            let agent_id = Uuid::new_v4();
            let decoded = subscribe_session_request_from_wire(pb::SubscribeSessionRequest {
                agent_id: uuid_to_bytes(agent_id),
                protocol: Some(protocol),
            })
            .unwrap();
            assert_eq!(decoded.agent_id, agent_id);
            assert_eq!(
                subscribe_protocol_to_agent_wire(decoded.protocol, decoded.args.as_deref(),)
                    .unwrap(),
                protocol
            );
        }
    }

    #[test]
    fn client_subscription_mirror_roundtrips_every_protocol() {
        let protocols = [
            pb::client_subscribe_session_request::Protocol::TerminalV1(pb::TerminalV1Args {
                terminal_size: None,
                replay_query: None,
            }),
            pb::client_subscribe_session_request::Protocol::ClaudePtyTranscriptV1(
                pb::ClaudePtyTranscriptV1Args {
                    terminal_size: None,
                    replay_query: None,
                },
            ),
            pb::client_subscribe_session_request::Protocol::ClaudeSdkV1(pb::ClaudeSdkV1Args {
                replay_query: None,
            }),
            pb::client_subscribe_session_request::Protocol::CodexSdkV1(pb::CodexSdkV1Args {
                replay_query: None,
            }),
            pb::client_subscribe_session_request::Protocol::TestEchoV1(pb::TestEchoV1Args {}),
        ];

        for protocol in protocols {
            let (decoded, args) = subscribe_protocol_from_client_wire(Some(protocol)).unwrap();
            assert_eq!(
                subscribe_protocol_to_client_wire(decoded, args.as_deref()).unwrap(),
                protocol
            );
        }
    }

    #[test]
    fn send_input_requests_roundtrip_every_protocol_and_control() {
        let events = vec![
            pb::send_input_request::Event::TerminalV1(pb::TerminalV1Input {
                payload: b"terminal".to_vec(),
            }),
            pb::send_input_request::Event::ClaudePtyTranscriptV1(pb::ClaudePtyTranscriptV1Input {
                expected_seq: 2,
                actions: vec![pb::ClaudePtyTranscriptV1Action {
                    action: Some(pb::claude_pty_transcript_v1_action::Action::Write(
                        b"claude".to_vec(),
                    )),
                }],
            }),
            pb::send_input_request::Event::ClaudeSdkV1(pb::ClaudeSdkV1Input {
                input: Some(pb::claude_sdk_v1_input::Input::Prompt(
                    pb::ClaudeSdkPrompt {
                        text: "hello".into(),
                    },
                )),
            }),
            pb::send_input_request::Event::ClaudeSdkV1(pb::ClaudeSdkV1Input {
                input: Some(pb::claude_sdk_v1_input::Input::Interrupt(
                    pb::ClaudeSdkInterrupt {},
                )),
            }),
            pb::send_input_request::Event::ClaudeSdkV1(pb::ClaudeSdkV1Input {
                input: Some(pb::claude_sdk_v1_input::Input::PermissionDecision(
                    pb::ClaudeSdkPermissionDecision {
                        request_id: "permission-allow".into(),
                        decision: Some(pb::claude_sdk_permission_decision::Decision::Allow(
                            pb::ClaudeSdkPermissionAllow {
                                updated_input_json: Some(br#"{"path":"/tmp"}"#.to_vec()),
                                updated_permissions_json: vec![br#"{"type":"addRules"}"#.to_vec()],
                                tool_use_id: Some("tool-allow".into()),
                            },
                        )),
                    },
                )),
            }),
            pb::send_input_request::Event::ClaudeSdkV1(pb::ClaudeSdkV1Input {
                input: Some(pb::claude_sdk_v1_input::Input::PermissionDecision(
                    pb::ClaudeSdkPermissionDecision {
                        request_id: "permission-1".into(),
                        decision: Some(pb::claude_sdk_permission_decision::Decision::Deny(
                            pb::ClaudeSdkPermissionDeny {
                                message: "no".into(),
                                interrupt: Some(true),
                                tool_use_id: Some("tool-1".into()),
                            },
                        )),
                    },
                )),
            }),
            pb::send_input_request::Event::CodexSdkV1(pb::CodexSdkV1Input {
                input: Some(pb::codex_sdk_v1_input::Input::Interrupt(
                    pb::CodexSdkV1Interrupt {
                        turn_id: "turn-1".into(),
                    },
                )),
            }),
            pb::send_input_request::Event::TestEchoV1(pb::TestEchoV1Input {
                payload: b"echo".to_vec(),
            }),
            pb::send_input_request::Event::Control(pb::SessionControl {
                control: Some(pb::session_control::Control::Resize(pb::TerminalSize {
                    rows: 30,
                    cols: 100,
                })),
            }),
        ];

        for wire_event in events {
            let agent_id = Uuid::new_v4();
            let input_id = Uuid::new_v4().as_bytes().to_vec();
            let decoded = send_input_request_from_wire(pb::SendInputRequest {
                agent_id: uuid_to_bytes(agent_id),
                input_id,
                event: Some(wire_event.clone()),
            })
            .unwrap();
            let (_, encoded) =
                send_input_event_to_agent_wire(decoded.protocol, &decoded.event).unwrap();
            assert_eq!(encoded, wire_event);
        }
    }

    #[test]
    fn client_input_mirror_roundtrips_every_protocol_and_control() {
        let events = vec![
            pb::client_send_input_request::Event::TerminalV1(pb::TerminalV1Input {
                payload: b"terminal".to_vec(),
            }),
            pb::client_send_input_request::Event::ClaudePtyTranscriptV1(
                pb::ClaudePtyTranscriptV1Input {
                    expected_seq: 1,
                    actions: Vec::new(),
                },
            ),
            pb::client_send_input_request::Event::ClaudeSdkV1(pb::ClaudeSdkV1Input {
                input: Some(pb::claude_sdk_v1_input::Input::Interrupt(
                    pb::ClaudeSdkInterrupt {},
                )),
            }),
            pb::client_send_input_request::Event::CodexSdkV1(pb::CodexSdkV1Input {
                input: Some(pb::codex_sdk_v1_input::Input::UserTurn(
                    pb::CodexSdkV1UserTurn {
                        input: b"[]".to_vec(),
                    },
                )),
            }),
            pb::client_send_input_request::Event::TestEchoV1(pb::TestEchoV1Input {
                payload: b"echo".to_vec(),
            }),
            pb::client_send_input_request::Event::Control(pb::SessionControl {
                control: Some(pb::session_control::Control::Resize(pb::TerminalSize {
                    rows: 40,
                    cols: 120,
                })),
            }),
        ];

        for event in events {
            let (protocol, decoded) =
                send_input_event_from_client_wire(b"input-id".to_vec(), Some(event.clone()))
                    .unwrap();
            let (_, encoded) = send_input_event_to_client_wire(protocol, &decoded).unwrap();
            assert_eq!(encoded, event);
        }
    }

    #[test]
    fn session_output_events_roundtrip() {
        let events = [
            SubscribeSessionEvent::Opened,
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
            let encoded = encode_session_output_event_payload(&event, Protocol::TerminalV1);
            let decoded = decode_session_output_event_payload(&encoded).unwrap();
            assert_eq!(decoded, event);
        }

        let outputs = [
            (Protocol::TerminalV1, b"terminal".to_vec()),
            (
                Protocol::ClaudePtyTranscriptV1,
                pb::ClaudePtyTranscriptV1Output {
                    seq_id: 1,
                    payload: b"claude-pty".to_vec(),
                }
                .encode_to_vec(),
            ),
            (
                Protocol::ClaudeSdkV1,
                pb::ClaudeSdkV1Output {
                    seq_id: 2,
                    payload: b"claude-sdk".to_vec(),
                }
                .encode_to_vec(),
            ),
            (
                Protocol::CodexSdkV1,
                pb::CodexSdkV1Output {
                    seq: 3,
                    payload: b"codex".to_vec(),
                }
                .encode_to_vec(),
            ),
            (Protocol::TestEchoV1, b"echo".to_vec()),
        ];
        for (protocol, payload) in outputs {
            let event = SubscribeSessionEvent::Output { payload };
            let encoded = encode_session_output_event_payload(&event, protocol);
            assert_eq!(
                decode_session_output_event_payload(&encoded).unwrap(),
                event
            );
        }
    }
}
