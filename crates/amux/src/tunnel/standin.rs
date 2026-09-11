//! Private compile bridge for the frame-based tunnel implementation.
//!
//! Protocol v2 removed these shapes from the public wire. Keeping them private
//! lets the obsolete tunnel implementation compile without redefining the link
//! protocol while native channels replace that implementation.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TunnelOpen {
    pub(crate) tunnel_id: Vec<u8>,
    pub(crate) src: Vec<u8>,
    pub(crate) dst: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TunnelData {
    pub(crate) tunnel_id: Vec<u8>,
    pub(crate) dst: Vec<u8>,
    pub(crate) payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TunnelClose {
    pub(crate) tunnel_id: Vec<u8>,
    pub(crate) dst: Vec<u8>,
    pub(crate) error: Option<crate::protocol::wire::pb::Error>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Message {
    pub(crate) body: Option<message::Body>,
}

pub(crate) mod message {
    use super::{TunnelClose, TunnelData, TunnelOpen};

    #[derive(Debug, Clone, PartialEq)]
    #[allow(clippy::enum_variant_names)]
    pub(crate) enum Body {
        TunnelOpen(TunnelOpen),
        TunnelData(TunnelData),
        TunnelClose(TunnelClose),
    }
}

pub(crate) fn encode_protocol_error(
    error: &crate::protocol::ProtocolError,
) -> crate::protocol::wire::pb::Error {
    crate::protocol::wire::encode_protocol_error(error)
}

pub(crate) fn decode_protocol_error(
    error: crate::protocol::wire::pb::Error,
) -> crate::protocol::ProtocolError {
    crate::protocol::wire::decode_protocol_error(error)
}
