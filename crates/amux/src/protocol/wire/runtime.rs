use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use prost::Message as ProstMessage;
use uuid::Uuid;

use crate::protocol::message::{
    FrameBody, GoAway, LocalFrame, Message, PeerFrame, ProtocolError, ReauthRequest,
    ReauthResponse, ResponseFrame, RoutedCallId, RoutedFrame, RoutedFrameMessage, RoutingEvent,
    ShutdownReason,
};
use crate::protocol::route::Route;
use crate::protocol::wire::{self, frame_body, response, transport_message};

pub(crate) fn encode_message(message: &Message) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(message_to_wire(message)?.encode_to_vec())
}

pub(crate) fn decode_message(data: &[u8]) -> Result<Message, wire::DecodeError> {
    let message = wire::TransportMessage::decode(data)?;
    wire_to_message(message)
}

pub(crate) fn encode_frame_body(body: &FrameBody) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(frame_body_to_wire(body).encode_to_vec())
}

pub(crate) fn decode_frame_body(data: &[u8]) -> Result<FrameBody, wire::DecodeError> {
    let body = wire::FrameBody::decode(data)?;
    frame_body_from_wire(Some(body))
}

pub(crate) fn encode_routing_event(event: &RoutingEvent) -> Result<Vec<u8>, wire::EncodeError> {
    Ok(routing_event_to_wire(event)?.encode_to_vec())
}

pub(crate) fn decode_routing_event(payload: &[u8]) -> Result<RoutingEvent, wire::DecodeError> {
    let event = wire::SubscribeRoutingEventsResponse::decode(payload)?;
    routing_event_from_wire(event)
}

fn message_to_wire(message: &Message) -> Result<wire::TransportMessage, wire::EncodeError> {
    let message = match message {
        Message::Routed(frame) => transport_message::Message::Routed(routed_to_wire(frame)),
        Message::Peer(frame) => transport_message::Message::Peer(peer_to_wire(frame)),
        Message::Local(frame) => transport_message::Message::Local(local_to_wire(frame)),
        Message::Ping => transport_message::Message::Ping(wire::Ping {}),
        Message::Pong => transport_message::Message::Pong(wire::Pong {}),
        Message::Reauth(reauth) => transport_message::Message::Reauth(wire::ReauthRequest {
            auth_token: reauth.token.clone(),
        }),
        Message::ReauthResponse(reauth) => {
            transport_message::Message::ReauthResponse(wire::ReauthResponse {
                outcome: Some(match &reauth.error {
                    Some(error) => {
                        wire::reauth_response::Outcome::Error(wire::encode_protocol_error(error))
                    }
                    None => wire::reauth_response::Outcome::Accepted(wire::Empty {}),
                }),
            })
        }
        Message::GoAway(goaway) => transport_message::Message::Goaway(wire::GoAway {
            reason: shutdown_reason_to_wire(&goaway.reason),
            error: None,
            drain_timeout_ms: 0,
        }),
        Message::PeerSnapshot { .. } => {
            return Err(wire::EncodeError::Invalid(
                "cannot encode internal peer snapshot batch".to_string(),
            ));
        }
    };
    Ok(wire::TransportMessage {
        message: Some(message),
    })
}

fn wire_to_message(message: wire::TransportMessage) -> Result<Message, wire::DecodeError> {
    match message
        .message
        .ok_or_else(|| wire::DecodeError::Invalid("missing TransportMessage message".into()))?
    {
        transport_message::Message::Routed(frame) => routed_from_wire(frame).map(Message::Routed),
        transport_message::Message::Peer(frame) => peer_from_wire(frame).map(Message::Peer),
        transport_message::Message::Local(frame) => local_from_wire(frame).map(Message::Local),
        transport_message::Message::Ping(_) => Ok(Message::Ping),
        transport_message::Message::Pong(_) => Ok(Message::Pong),
        transport_message::Message::Reauth(reauth) => Ok(Message::Reauth(ReauthRequest {
            token: reauth.auth_token,
        })),
        transport_message::Message::ReauthResponse(response) => {
            Ok(Message::ReauthResponse(ReauthResponse {
                error: reauth_error(response)?,
            }))
        }
        transport_message::Message::Goaway(goaway) => Ok(Message::GoAway(GoAway {
            reason: shutdown_reason_from_wire(goaway.reason),
        })),
    }
}

fn routed_to_wire(frame: &RoutedFrame) -> wire::RoutedFrame {
    wire::RoutedFrame {
        src: Some(route_to_wire(&frame.src)),
        dst: Some(route_to_wire(&frame.dst)),
        call_id: frame.call_id.as_bytes().to_vec(),
        message: Some(match &frame.message {
            RoutedFrameMessage::Payload(payload) => {
                wire::routed_frame::Message::Payload(payload.clone())
            }
            RoutedFrameMessage::RoutingError {
                failed_route,
                error,
            } => wire::routed_frame::Message::RoutingError(wire::RoutingError {
                error: Some(wire::encode_protocol_error(error)),
                failed_route: Some(route_to_wire(failed_route)),
            }),
        }),
    }
}

fn routed_from_wire(frame: wire::RoutedFrame) -> Result<RoutedFrame, wire::DecodeError> {
    let src = required_route_from_wire("RoutedFrame.src", frame.src)?;
    let dst = required_route_from_wire("RoutedFrame.dst", frame.dst)?;
    let call_id = RoutedCallId::from_bytes(frame.call_id).map_err(wire::DecodeError::Invalid)?;
    let message = match frame
        .message
        .ok_or_else(|| wire::DecodeError::Invalid("missing RoutedFrame message".into()))?
    {
        wire::routed_frame::Message::Payload(payload) => RoutedFrameMessage::Payload(payload),
        wire::routed_frame::Message::RoutingError(routing_error) => {
            let error = routing_error
                .error
                .ok_or_else(|| wire::DecodeError::Invalid("missing RoutingError error".into()))?;
            let failed_route = routing_error.failed_route.ok_or_else(|| {
                wire::DecodeError::Invalid("missing RoutingError failed_route".into())
            })?;
            RoutedFrameMessage::RoutingError {
                failed_route: route_from_wire(failed_route)?,
                error: wire::decode_protocol_error(error),
            }
        }
    };
    Ok(RoutedFrame {
        src,
        dst,
        call_id,
        message,
    })
}

fn peer_to_wire(frame: &PeerFrame) -> wire::PeerFrame {
    wire::PeerFrame {
        call_id: frame.call_id.as_bytes().to_vec(),
        body: Some(frame_body_to_wire(&frame.body)),
    }
}

fn peer_from_wire(frame: wire::PeerFrame) -> Result<PeerFrame, wire::DecodeError> {
    Ok(PeerFrame {
        call_id: RoutedCallId::from_bytes(frame.call_id).map_err(wire::DecodeError::Invalid)?,
        body: frame_body_from_wire(frame.body)?,
    })
}

fn local_to_wire(frame: &LocalFrame) -> wire::LocalFrame {
    wire::LocalFrame {
        call_id: frame.call_id.as_bytes().to_vec(),
        body: Some(frame_body_to_wire(&frame.body)),
    }
}

fn local_from_wire(frame: wire::LocalFrame) -> Result<LocalFrame, wire::DecodeError> {
    Ok(LocalFrame {
        call_id: RoutedCallId::from_bytes(frame.call_id).map_err(wire::DecodeError::Invalid)?,
        body: frame_body_from_wire(frame.body)?,
    })
}

fn frame_body_to_wire(body: &FrameBody) -> wire::FrameBody {
    let kind = match body {
        FrameBody::Request(request) => frame_body::Kind::Request(wire::Request {
            method: request.method.clone(),
            payload: request.payload.clone(),
        }),
        FrameBody::Response(response) => frame_body::Kind::Response(wire::Response {
            outcome: Some(match response {
                ResponseFrame::Payload(payload) => response::Outcome::Payload(payload.clone()),
                ResponseFrame::Error(error) => {
                    response::Outcome::Error(wire::encode_protocol_error(error))
                }
            }),
        }),
        FrameBody::StreamItem(payload) => frame_body::Kind::StreamItem(wire::StreamItem {
            payload: payload.clone(),
        }),
        FrameBody::Cancel => frame_body::Kind::Cancel(wire::Cancel {}),
    };
    wire::FrameBody { kind: Some(kind) }
}

fn frame_body_from_wire(body: Option<wire::FrameBody>) -> Result<FrameBody, wire::DecodeError> {
    let body = body.ok_or_else(|| wire::DecodeError::Invalid("missing FrameBody".into()))?;
    match body
        .kind
        .ok_or_else(|| wire::DecodeError::Invalid("missing FrameBody kind".into()))?
    {
        frame_body::Kind::Request(request) => {
            Ok(FrameBody::Request(crate::protocol::message::RequestFrame {
                method: request.method,
                payload: request.payload,
            }))
        }
        frame_body::Kind::Response(response) => {
            let outcome = response
                .outcome
                .ok_or_else(|| wire::DecodeError::Invalid("missing Response outcome".into()))?;
            Ok(FrameBody::Response(match outcome {
                response::Outcome::Payload(payload) => ResponseFrame::Payload(payload),
                response::Outcome::Error(error) => {
                    ResponseFrame::Error(wire::decode_protocol_error(error))
                }
            }))
        }
        frame_body::Kind::StreamItem(item) => Ok(FrameBody::StreamItem(item.payload)),
        frame_body::Kind::Cancel(_) => Ok(FrameBody::Cancel),
    }
}

fn reauth_error(
    response: wire::ReauthResponse,
) -> Result<Option<ProtocolError>, wire::DecodeError> {
    match response
        .outcome
        .ok_or_else(|| wire::DecodeError::Invalid("missing ReauthResponse outcome".into()))?
    {
        wire::reauth_response::Outcome::Accepted(_) => Ok(None),
        wire::reauth_response::Outcome::Error(error) => {
            Ok(Some(wire::decode_protocol_error(error)))
        }
    }
}

fn route_to_wire(route: &Route) -> wire::Route {
    wire::Route {
        links: route.iter().map(|link| link.as_str().to_string()).collect(),
    }
}

fn required_route_from_wire(
    field: &str,
    route: Option<wire::Route>,
) -> Result<Route, wire::DecodeError> {
    let route = route.ok_or_else(|| wire::DecodeError::Invalid(format!("missing {field}")))?;
    route_from_wire(route)
}

fn route_from_wire(route: wire::Route) -> Result<Route, wire::DecodeError> {
    Route::from_links(route.links)
        .map_err(|e| wire::DecodeError::Invalid(format!("invalid route: {e}")))
}

fn routing_event_to_wire(
    event: &RoutingEvent,
) -> Result<wire::SubscribeRoutingEventsResponse, wire::EncodeError> {
    let event = match event {
        RoutingEvent::SnapshotComplete => {
            wire::subscribe_routing_events_response::Event::SnapshotComplete(
                wire::SnapshotComplete {},
            )
        }
        RoutingEvent::AgentUp {
            agent_id,
            host_id,
            name,
            command,
            working_dir,
            agent_type,
            io_protocols,
            readonly,
            args,
            created_at,
        } => wire::subscribe_routing_events_response::Event::AgentUp(wire::AgentUp {
            agent: Some(wire::Agent {
                agent_id: uuid_to_bytes(*agent_id),
                host_id: uuid_to_bytes(*host_id),
                name: name.clone(),
                command: command.clone(),
                working_dir: working_dir.to_string_lossy().into_owned(),
                agent_type: agent_type.clone(),
                io_protocols: io_protocols.clone(),
                readonly: *readonly,
                args: args.clone(),
                created_at_unix_ms: created_at.timestamp_millis(),
            }),
        }),
        RoutingEvent::AgentDown { agent_id } => {
            wire::subscribe_routing_events_response::Event::AgentDown(wire::AgentDown {
                agent_id: uuid_to_bytes(*agent_id),
                reason: None,
            })
        }
        RoutingEvent::HostUp {
            id,
            name,
            route,
            version,
        } => wire::subscribe_routing_events_response::Event::HostUp(wire::HostUp {
            host: Some(wire::Host {
                host_id: uuid_to_bytes(*id),
                name: name.clone(),
                route: Some(route_to_wire(route)),
                version: version.clone(),
            }),
        }),
        RoutingEvent::HostDown { id, route } => {
            wire::subscribe_routing_events_response::Event::HostDown(wire::HostDown {
                host_id: uuid_to_bytes(*id),
                route: Some(route_to_wire(route)),
                reason: None,
            })
        }
        RoutingEvent::Unknown => {
            return Err(wire::EncodeError::Invalid(
                "cannot encode unknown routing event".to_string(),
            ));
        }
    };
    Ok(wire::SubscribeRoutingEventsResponse { event: Some(event) })
}

fn routing_event_from_wire(
    event: wire::SubscribeRoutingEventsResponse,
) -> Result<RoutingEvent, wire::DecodeError> {
    let event = event
        .event
        .ok_or_else(|| wire::DecodeError::Invalid("missing RoutingEvent event".into()))?;
    match event {
        wire::subscribe_routing_events_response::Event::HostUp(event) => {
            let host = event
                .host
                .ok_or_else(|| wire::DecodeError::Invalid("missing HostUp host".into()))?;
            host_event_from_wire(host)
        }
        wire::subscribe_routing_events_response::Event::HostUpdated(event) => {
            let host = event
                .host
                .ok_or_else(|| wire::DecodeError::Invalid("missing HostUpdated host".into()))?;
            host_event_from_wire(host)
        }
        wire::subscribe_routing_events_response::Event::HostDown(event) => {
            Ok(RoutingEvent::HostDown {
                id: uuid_from_bytes("host_id", event.host_id)?,
                route: required_route_from_wire("HostDown.route", event.route)?,
            })
        }
        wire::subscribe_routing_events_response::Event::AgentUp(event) => {
            let agent = event
                .agent
                .ok_or_else(|| wire::DecodeError::Invalid("missing AgentUp agent".into()))?;
            agent_event_from_wire(agent)
        }
        wire::subscribe_routing_events_response::Event::AgentUpdated(event) => {
            let agent = event
                .agent
                .ok_or_else(|| wire::DecodeError::Invalid("missing AgentUpdated agent".into()))?;
            agent_event_from_wire(agent)
        }
        wire::subscribe_routing_events_response::Event::AgentDown(event) => {
            Ok(RoutingEvent::AgentDown {
                agent_id: uuid_from_bytes("agent_id", event.agent_id)?,
            })
        }
        wire::subscribe_routing_events_response::Event::SnapshotComplete(_) => {
            Ok(RoutingEvent::SnapshotComplete)
        }
    }
}

fn host_event_from_wire(host: wire::Host) -> Result<RoutingEvent, wire::DecodeError> {
    Ok(RoutingEvent::HostUp {
        id: uuid_from_bytes("host_id", host.host_id)?,
        name: host.name,
        route: required_route_from_wire("Host.route", host.route)?,
        version: host.version,
    })
}

fn agent_event_from_wire(agent: wire::Agent) -> Result<RoutingEvent, wire::DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| wire::DecodeError::Invalid("invalid agent created_at".into()))?;
    Ok(RoutingEvent::AgentUp {
        agent_id: uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        agent_type: agent.agent_type,
        io_protocols: agent.io_protocols.clone(),
        readonly: agent.readonly,
        args: agent.args,
        created_at,
    })
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

fn shutdown_reason_to_wire(reason: &ShutdownReason) -> i32 {
    match reason {
        ShutdownReason::UpdateRequired => wire::GoAwayReason::UpdateRequired as i32,
        ShutdownReason::ProtocolError => wire::GoAwayReason::ProtocolError as i32,
        ShutdownReason::UserRequested => wire::GoAwayReason::UserShutdown as i32,
        ShutdownReason::Updating => wire::GoAwayReason::Updating as i32,
        ShutdownReason::Suspending => wire::GoAwayReason::Suspending as i32,
        ShutdownReason::Restarting => wire::GoAwayReason::Restarting as i32,
        ShutdownReason::AuthExpired => wire::GoAwayReason::AuthExpired as i32,
    }
}

fn shutdown_reason_from_wire(reason: i32) -> ShutdownReason {
    match wire::GoAwayReason::try_from(reason).unwrap_or(wire::GoAwayReason::Unspecified) {
        wire::GoAwayReason::UpdateRequired => ShutdownReason::UpdateRequired,
        wire::GoAwayReason::ProtocolError => ShutdownReason::ProtocolError,
        wire::GoAwayReason::Updating => ShutdownReason::Updating,
        wire::GoAwayReason::Suspending => ShutdownReason::Suspending,
        wire::GoAwayReason::Restarting => ShutdownReason::Restarting,
        wire::GoAwayReason::AuthExpired => ShutdownReason::AuthExpired,
        wire::GoAwayReason::UserShutdown => ShutdownReason::UserRequested,
        _ => ShutdownReason::UserRequested,
    }
}
