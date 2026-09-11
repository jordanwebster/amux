//! Node-side domain and wire adapters for agent routing.

mod events;
mod wire;

pub use model::{
    Agent, AgentEvent, AgentKind, AgentParent, AgentType, ArtifactRef, BaseIdentity, ClaudeDriver,
    CreateAgentRequest, DiffBase, DiffFile, DiffResponse, Protocol, RenameAgentRequest,
    SessionCloseReason, SpawnInheritance, SubscribeSessionEvent, TerminalSize, WorkingOn,
};

pub(crate) use ::wire::{
    agent_kind_from_wire, agent_kind_to_wire, artifact_kind_from_wire, artifact_ref_to_wire, claude_driver_from_wire,
    diff_base_from_wire, diff_response_to_wire,
};
pub(crate) use events::{agent_event_from_wire, agent_event_to_wire};
pub(crate) use wire::{
    CreateAgentConfig, CreateAgentRpcRequest, SendInputRequest, SessionInputEvent,
    SetAgentStatusRequest, SubscribeSessionRequest, agent_from_wire, agent_parent_from_wire,
    agent_to_wire, create_agent_request_from_wire, delete_agent_id_from_wire,
    envelope_from_wire, envelope_to_wire, rename_agent_request_from_wire,
    send_input_event_from_client_wire, send_input_event_to_agent_wire, send_input_request_from_wire, session_output_event_to_wire, set_agent_status_request_from_wire,
    subscribe_protocol_from_client_wire, subscribe_protocol_to_agent_wire, subscribe_session_request_from_wire,
};
