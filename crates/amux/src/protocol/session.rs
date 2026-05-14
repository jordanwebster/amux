use uuid::Uuid;

use crate::protocol::message::{FrameBody, ProtocolError, ResponseFrame};
use crate::protocol::wire;

#[derive(Debug, Clone, PartialEq)]
pub enum SubscribeSessionEvent {
    Opened,
    Output { payload: Vec<u8> },
    ReplayComplete { cursor: Option<Vec<u8>> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscribeSessionFrame {
    Event(SubscribeSessionEvent),
    Response(Result<(), ProtocolError>),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionCodecError {
    #[error("failed to encode session request payload: {0}")]
    Encode(String),
    #[error("failed to decode session RPC frame: {0}")]
    Decode(String),
}

pub fn encode_subscribe_session_request(
    agent_id: Uuid,
    io_protocol: impl Into<String>,
    args: Option<Vec<u8>>,
) -> Result<Vec<u8>, SessionCodecError> {
    wire::encode_subscribe_session_request_payload(&wire::SubscribeSessionRequest {
        agent_id,
        io_protocol: io_protocol.into(),
        args,
    })
    .map_err(|error| SessionCodecError::Encode(error.to_string()))
}

pub fn encode_send_input_request(
    agent_id: Uuid,
    io_protocol: impl Into<String>,
    input_id: Vec<u8>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, SessionCodecError> {
    wire::encode_send_input_request_payload(&wire::SendInputRequest {
        agent_id,
        io_protocol: io_protocol.into(),
        event: wire::SessionInputEvent::Input { input_id, payload },
    })
    .map_err(|error| SessionCodecError::Encode(error.to_string()))
}

pub fn encode_send_control_request(
    agent_id: Uuid,
    io_protocol: impl Into<String>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, SessionCodecError> {
    wire::encode_send_input_request_payload(&wire::SendInputRequest {
        agent_id,
        io_protocol: io_protocol.into(),
        event: wire::SessionInputEvent::Control { payload },
    })
    .map_err(|error| SessionCodecError::Encode(error.to_string()))
}

pub fn decode_subscribe_session_frame_body(
    body: FrameBody,
) -> Result<SubscribeSessionFrame, SessionCodecError> {
    match body {
        FrameBody::StreamItem(payload) => wire::decode_session_output_event_payload(&payload)
            .map(|event| SubscribeSessionFrame::Event(event.into()))
            .map_err(|error| SessionCodecError::Decode(error.to_string())),
        FrameBody::Response(ResponseFrame::Payload(payload)) => {
            if payload.is_empty() {
                Ok(SubscribeSessionFrame::Response(Ok(())))
            } else {
                Err(SessionCodecError::Decode(format!(
                    "SubscribeSession success response payload must be empty, got {} bytes",
                    payload.len()
                )))
            }
        }
        FrameBody::Response(ResponseFrame::Error(error)) => {
            Ok(SubscribeSessionFrame::Response(Err(error)))
        }
        FrameBody::Request(_) | FrameBody::Cancel | FrameBody::RoutingError { .. } => {
            Err(SessionCodecError::Decode(
                "SubscribeSession server frame must be a stream item or response".to_string(),
            ))
        }
    }
}

impl From<wire::SessionOutputEvent> for SubscribeSessionEvent {
    fn from(event: wire::SessionOutputEvent) -> Self {
        match event {
            wire::SessionOutputEvent::Opened => Self::Opened,
            wire::SessionOutputEvent::Output { payload } => Self::Output { payload },
            wire::SessionOutputEvent::ReplayComplete { cursor } => Self::ReplayComplete { cursor },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::wire;

    #[test]
    fn server_frame_decoder_accepts_output_events_and_terminal_responses() {
        assert_eq!(
            decode_subscribe_session_frame_body(FrameBody::StreamItem(
                wire::encode_session_output_event_payload(&wire::SessionOutputEvent::Output {
                    payload: b"hello".to_vec(),
                })
            ))
            .unwrap(),
            SubscribeSessionFrame::Event(SubscribeSessionEvent::Output {
                payload: b"hello".to_vec(),
            })
        );

        assert_eq!(
            decode_subscribe_session_frame_body(FrameBody::Response(ResponseFrame::Payload(
                Vec::new()
            )))
            .unwrap(),
            SubscribeSessionFrame::Response(Ok(()))
        );
    }
}
