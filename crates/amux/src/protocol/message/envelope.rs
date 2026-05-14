use super::common::{CallId, ProtocolError, ShutdownReason};
use crate::protocol::route::Route;

/// Protobuf-shaped transport message used after handshake.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Frame(Frame),
    Ping,
    Pong,
    Reauth(ReauthRequest),
    ReauthResponse(ReauthResponse),
    GoAway(GoAway),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub src: Route,
    pub dst: Route,
    pub call_id: CallId,
    pub body: FrameBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FrameBody {
    Request(RequestFrame),
    Response(ResponseFrame),
    StreamItem(Vec<u8>),
    Cancel,
    RoutingError {
        failed_route: Route,
        error: ProtocolError,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestFrame {
    pub method: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFrame {
    Payload(Vec<u8>),
    Error(ProtocolError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReauthRequest {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReauthResponse {
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoAway {
    pub reason: ShutdownReason,
}

impl Message {
    pub fn routing_error_for_route(
        src: Route,
        dst: Route,
        call_id: CallId,
        failed_route: Route,
        error: ProtocolError,
    ) -> Self {
        Message::Frame(Frame {
            src,
            dst,
            call_id,
            body: FrameBody::RoutingError {
                failed_route,
                error,
            },
        })
    }

    pub fn type_label(&self) -> &'static str {
        match self {
            Message::Frame(frame) => frame.type_label(),
            Message::Ping => "Ping",
            Message::Pong => "Pong",
            Message::Reauth(_) => "Reauth",
            Message::ReauthResponse(response) => {
                if response.error.is_some() {
                    "ReauthResponse::Error"
                } else {
                    "ReauthResponse::Accepted"
                }
            }
            Message::GoAway(_) => "GoAway",
        }
    }

    /// Encode the top-level runtime envelope to protobuf `TransportMessage` bytes.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, crate::protocol::wire::EncodeError> {
        crate::protocol::wire::encode_message(self)
    }

    /// Decode a top-level runtime envelope from protobuf `TransportMessage` bytes.
    pub(crate) fn decode(data: &[u8]) -> Result<Self, crate::protocol::wire::DecodeError> {
        crate::protocol::wire::decode_message(data)
    }
}

impl Frame {
    fn type_label(&self) -> &'static str {
        self.body.type_label()
    }
}

impl FrameBody {
    fn type_label(&self) -> &'static str {
        match self {
            FrameBody::Request(_) => "Frame::Request",
            FrameBody::Response(response) => match response {
                ResponseFrame::Payload(_) => "Frame::Response",
                ResponseFrame::Error(_) => "Frame::ResponseError",
            },
            FrameBody::StreamItem(_) => "Frame::StreamItem",
            FrameBody::Cancel => "Frame::Cancel",
            FrameBody::RoutingError { .. } => "Frame::RoutingError",
        }
    }
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;
    use crate::protocol::link::Link;

    #[test]
    fn top_level_message_rejects_non_protobuf_bytes() {
        assert!(Message::decode(b"\x81\xa4kind\xa6direct").is_err());
    }

    #[test]
    fn heartbeat_roundtrips_as_ping() {
        let msg = Message::Ping;

        let decoded = Message::decode(&msg.encode().unwrap()).unwrap();

        assert_eq!(decoded, Message::Ping);
        assert_eq!(msg.type_label(), "Ping");
    }

    #[test]
    fn goaway_reasons_roundtrip() {
        let reasons = [
            ShutdownReason::UpdateRequired,
            ShutdownReason::ProtocolError,
            ShutdownReason::UserRequested,
            ShutdownReason::Updating,
            ShutdownReason::Suspending,
            ShutdownReason::Restarting,
            ShutdownReason::AuthExpired,
        ];

        for reason in reasons {
            let msg = Message::GoAway(GoAway {
                reason: reason.clone(),
            });

            let decoded = Message::decode(&msg.encode().unwrap()).unwrap();

            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn frame_stream_item_roundtrips_with_empty_dst() {
        let call_id = CallId::from(uuid::Uuid::from_u128(8));
        let msg = Message::Frame(Frame {
            src: Route::from_link(Link::new("peer-link").unwrap()),
            dst: Route::empty(),
            call_id: call_id.clone(),
            body: FrameBody::StreamItem(
                crate::protocol::wire::SubscribeRoutingEventsResponse {
                    event: Some(
                        crate::protocol::wire::subscribe_routing_events_response::Event::SnapshotComplete(
                            crate::protocol::wire::SnapshotComplete {},
                        ),
                    ),
                }
                .encode_to_vec(),
            ),
        });

        let decoded = Message::decode(&msg.encode().unwrap()).unwrap();

        assert_eq!(decoded, msg);
        assert_eq!(decoded.type_label(), "Frame::StreamItem");
    }

    #[test]
    fn frame_stream_item_roundtrips_with_non_empty_dst() {
        let msg = Message::Frame(Frame {
            src: Route::from_link(Link::new("src-link").unwrap()),
            dst: Route::from_link(Link::new("dst-link").unwrap()),
            call_id: CallId::from(uuid::Uuid::from_u128(9)),
            body: FrameBody::StreamItem(b"opaque".to_vec()),
        });

        let decoded = Message::decode(&msg.encode().unwrap()).unwrap();

        assert_eq!(decoded, msg);
        assert_eq!(decoded.type_label(), "Frame::StreamItem");
    }

    #[test]
    fn top_level_message_rejects_missing_transport_message_kind() {
        let encoded = crate::protocol::wire::TransportMessage { message: None }.encode_to_vec();
        assert!(Message::decode(&encoded).is_err());
    }
}
