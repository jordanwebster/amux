use uuid::Uuid;

use crate::protocol::message::{FrameBody, ProtocolError, ResponseFrame};
use crate::protocol::wire;

#[derive(Debug, Clone, PartialEq)]
pub enum OpenSessionOutputEvent {
    Opened,
    Output {
        payload: Vec<u8>,
        cursor: Option<Vec<u8>>,
    },
    InputResult {
        input_id: Vec<u8>,
        result: Result<(), ProtocolError>,
    },
    ReplayComplete {
        cursor: Option<Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenSessionServerFrame {
    Event(OpenSessionOutputEvent),
    Response(Result<(), ProtocolError>),
}

#[derive(Debug, thiserror::Error)]
pub enum OpenSessionCodecError {
    #[error("failed to encode OpenSession frame: {0}")]
    Encode(String),
    #[error("failed to decode OpenSession frame: {0}")]
    Decode(String),
}

pub fn encode_open_session_request(
    agent_id: Uuid,
    io_protocol: impl Into<String>,
    args: Option<Vec<u8>>,
) -> Result<Vec<u8>, OpenSessionCodecError> {
    wire::encode_open_session_request(&wire::SessionOpenRequest {
        agent_id,
        io_protocol: io_protocol.into(),
        args,
    })
    .map_err(|error| OpenSessionCodecError::Encode(error.to_string()))
}

pub fn encode_open_session_input(
    input_id: Vec<u8>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, OpenSessionCodecError> {
    wire::encode_open_session_input_event(&wire::OpenSessionInputEvent::Input { input_id, payload })
        .map_err(|error| OpenSessionCodecError::Encode(error.to_string()))
}

pub fn encode_open_session_cancel() -> Result<Vec<u8>, OpenSessionCodecError> {
    wire::encode_open_session_cancel()
        .map_err(|error| OpenSessionCodecError::Encode(error.to_string()))
}

pub fn decode_open_session_server_frame(
    payload: &[u8],
) -> Result<OpenSessionServerFrame, OpenSessionCodecError> {
    let body = wire::decode_frame_body(payload)
        .map_err(|error| OpenSessionCodecError::Decode(error.to_string()))?;
    decode_open_session_server_frame_body(body)
}

pub fn decode_open_session_server_frame_body(
    body: FrameBody,
) -> Result<OpenSessionServerFrame, OpenSessionCodecError> {
    match body {
        FrameBody::StreamItem(payload) => wire::decode_open_session_output_event_payload(&payload)
            .map(|event| OpenSessionServerFrame::Event(event.into()))
            .map_err(|error| OpenSessionCodecError::Decode(error.to_string())),
        FrameBody::Response(ResponseFrame::Payload(payload)) => {
            if payload.is_empty() {
                Ok(OpenSessionServerFrame::Response(Ok(())))
            } else {
                Err(OpenSessionCodecError::Decode(format!(
                    "OpenSession success response payload must be empty, got {} bytes",
                    payload.len()
                )))
            }
        }
        FrameBody::Response(ResponseFrame::Error(error)) => {
            Ok(OpenSessionServerFrame::Response(Err(error)))
        }
        FrameBody::Request(_) | FrameBody::Cancel => Err(OpenSessionCodecError::Decode(
            "OpenSession server frame must be a stream item or response".to_string(),
        )),
    }
}

impl From<wire::OpenSessionOutputEvent> for OpenSessionOutputEvent {
    fn from(event: wire::OpenSessionOutputEvent) -> Self {
        match event {
            wire::OpenSessionOutputEvent::Opened => Self::Opened,
            wire::OpenSessionOutputEvent::Output { payload, cursor } => {
                Self::Output { payload, cursor }
            }
            wire::OpenSessionOutputEvent::InputResult { input_id, result } => {
                Self::InputResult { input_id, result }
            }
            wire::OpenSessionOutputEvent::ReplayComplete { cursor } => {
                Self::ReplayComplete { cursor }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::wire;

    #[test]
    fn server_frame_decoder_accepts_output_events_and_terminal_responses() {
        let output =
            wire::encode_open_session_output_event(&wire::OpenSessionOutputEvent::Output {
                payload: b"hello".to_vec(),
                cursor: None,
            })
            .unwrap();
        assert_eq!(
            decode_open_session_server_frame(&output).unwrap(),
            OpenSessionServerFrame::Event(OpenSessionOutputEvent::Output {
                payload: b"hello".to_vec(),
                cursor: None,
            })
        );

        let response = wire::encode_open_session_response(Ok(())).unwrap();
        assert_eq!(
            decode_open_session_server_frame(&response).unwrap(),
            OpenSessionServerFrame::Response(Ok(()))
        );
    }
}
