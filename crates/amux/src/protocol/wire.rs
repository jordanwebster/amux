//! Protobuf wire boundary.
//!
//! Generated `prost` types stay in this module. Application code should convert
//! at the protocol boundary instead of carrying generated structs through
//! server, client, or agent logic.

mod agent_rpc;
mod error;
mod generated;
mod runtime;

pub(crate) use agent_rpc::{
    AgentLifecycleRequest, AgentLifecycleResponse, AgentRecord, CreateAgentConfig,
    CreateAgentRpcRequest, SendInputRequest, SessionInputEvent, SessionOutputEvent,
    SubscribeSessionRequest, agent_entry_from_domain, agent_entry_to_domain,
    decode_agent_lifecycle_request_payload, decode_agent_lifecycle_response_frame,
    decode_send_input_request, decode_session_output_event_payload,
    decode_subscribe_session_request, encode_agent_lifecycle_request_payload,
    encode_agent_lifecycle_response_frame, encode_send_input_request_payload,
    encode_session_output_event_payload, encode_subscribe_session_request_payload,
};
pub(crate) use error::{
    DecodeError, EncodeError, decode_protocol_error, encode_connect_invalid_link_name_response,
    encode_connect_protocol_version_mismatch_response, encode_protocol_error,
};
pub(crate) use generated::*;
pub(crate) use runtime::{
    decode_agent_event, decode_message, decode_routing_event, encode_agent_event, encode_message,
    encode_routing_event, host_from_wire, host_to_wire,
};
