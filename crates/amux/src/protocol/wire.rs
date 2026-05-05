//! Protobuf wire boundary.
//!
//! Generated `prost` types stay in this module. Application code should convert
//! at the protocol boundary instead of carrying generated structs through
//! server, client, or agent logic.

mod error;
mod generated;
mod routed;
mod runtime;

pub(crate) use error::{
    DecodeError, EncodeError, decode_protocol_error, encode_connect_invalid_link_name_response,
    encode_connect_protocol_version_mismatch_response, encode_protocol_error,
};
pub(crate) use generated::*;
#[cfg(test)]
pub(crate) use routed::decode_agent_lifecycle_request_if_present;
#[cfg(test)]
pub(crate) use routed::encode_agent_lifecycle_response;
#[cfg(test)]
pub(crate) use routed::encode_open_session_output_event;
#[cfg(test)]
pub(crate) use routed::encode_open_session_response;
pub(crate) use routed::{
    AgentLifecycleRequest, AgentLifecycleResponse, AgentRecord, CreateAgentConfig,
    CreateAgentRpcRequest, OpenSessionClientFrame, OpenSessionInputEvent, OpenSessionOutputEvent,
    SessionOpenRequest, agent_entry_from_domain, agent_entry_to_domain,
    decode_agent_lifecycle_request_payload, decode_agent_lifecycle_response,
    decode_open_session_input_payload, decode_open_session_output_event_payload,
    decode_open_session_request, encode_agent_lifecycle_request,
    encode_agent_lifecycle_response_frame, encode_open_session_cancel,
    encode_open_session_input_event, encode_open_session_output_event_payload,
    encode_open_session_request,
};
pub(crate) use runtime::{
    decode_frame_body, decode_message, decode_routing_event, encode_frame_body, encode_message,
    encode_routing_event,
};
