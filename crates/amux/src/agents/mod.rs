//! Agent runtime: session lifecycle, PTY management, and hook dispatch.

mod attachments;
mod buffer;
pub(crate) mod claude;
pub(crate) mod codex;
mod debug;
mod events;
#[cfg(feature = "local-agents")]
mod hook;
mod kind;
mod log_source;
mod naming;
#[cfg(feature = "local-agents")]
mod pty;
mod record;
#[cfg(feature = "local-agents")]
mod session;
mod session_events;
pub(crate) mod terminal_io;
#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
mod test_agent;
mod types;
mod wire;

pub(crate) use attachments::{
    ArtifactOwners, MaterialiseBackend, artifact_kind_from_wire, artifact_kind_to_wire,
    artifact_read_rule, artifact_ref_from_wire, artifact_ref_to_wire, compute_diff,
    diff_base_from_wire, diff_base_to_wire, diff_response_from_wire, diff_response_to_wire,
    materialise_and_log, materialise_paths, spawn_artifact_sweeper, store_error,
};
pub use attachments::{
    ArtifactRef, BaseIdentity, DiffBase, DiffFile, DiffResponse, attachments_row,
};
pub(crate) use buffer::{
    BroadcastRead, ByteReplayQuery, MultiplexByteBuffer, MultiplexByteReader,
    MultiplexStructuredBuffer, MultiplexStructuredReader, SequencedReplayQuery, StructuredOutput,
};
#[cfg(all(feature = "local-agents", unix))]
pub(crate) use codex::CodexRawPtyLease;
pub(crate) use debug::{BackendState, BufferDebug, ObligationDebug, OutputDebug, SessionDebug};
pub use events::AgentEvent;
pub(crate) use events::{agent_event_from_wire, agent_event_to_wire};
#[cfg(feature = "local-agents")]
pub(crate) use hook::{ExternalHookBootstrap, HookError, HookOutcome};
pub use kind::{AgentKind, ClaudeDriver, Protocol};
pub(crate) use log_source::StructuredLogSource;
pub(crate) use naming::LocalAgentNameSource;
#[cfg(feature = "local-agents")]
pub(crate) use pty::{PtyHandle, spawn_pty_agent};
pub(crate) use record::{AgentRecord, SessionEvent, StopPolicy};
#[cfg(all(feature = "local-agents", test))]
pub(crate) use session::mcp_launch_route_for_tests;
#[cfg(feature = "local-agents")]
pub(crate) use session::{
    AgentBackend, AgentDeliveryTarget, AgentDeps, AgentSession, Delivery, DeliveryError,
    DeliveryLiveness, McpLaunchRoute, Plane, RawPtyTarget, StructuredInput, StructuredInputEvent,
    agent_from_suspended, bootstrap_external_hook, new_agent,
};
pub use session_events::{SessionCloseReason, SubscribeSessionEvent};
#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
pub(crate) use test_agent::TestAgentSession;
#[cfg(all(feature = "local-agents", test))]
pub(crate) use test_agent::io::{
    TEST_DELAYED_DELIVERY_COMMAND, TEST_FAILED_DELIVERY_COMMAND, TEST_UNAVAILABLE_DELIVERY_COMMAND,
};
#[cfg(all(feature = "local-agents", any(test, testnet)))]
pub(crate) use test_agent::io::{TEST_ECHO_COMMAND, TEST_ECHO_V1};
#[cfg(all(feature = "local-agents", unix))]
pub(crate) use types::AGENT_TYPE_CODEX;
#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
pub(crate) use types::AGENT_TYPE_TEST_AGENT;
pub(crate) use types::{AGENT_TYPE_CLAUDE, HookEnvironment, SpawnInheritance};
pub use types::{
    Agent, AgentParent, AgentType, CreateAgentRequest, RenameAgentRequest, TerminalSize, WorkingOn,
};
pub(crate) use wire::{
    CreateAgentConfig, CreateAgentRpcRequest, SendInputRequest, SessionInputEvent,
    SetAgentStatusRequest, SubscribeSessionRequest, agent_from_wire, agent_kind_from_wire,
    agent_kind_to_wire, agent_parent_from_wire, agent_to_wire, claude_driver_from_wire,
    claude_driver_to_wire, create_agent_request_from_wire, delete_agent_id_from_wire,
    envelope_from_wire, envelope_to_wire, rename_agent_request_from_wire,
    send_input_event_from_client_wire, send_input_event_to_agent_wire,
    send_input_event_to_client_wire, send_input_request_from_wire, session_output_event_to_wire,
    session_output_payload_from_wire, set_agent_status_request_from_wire,
    subscribe_protocol_from_client_wire, subscribe_protocol_to_agent_wire,
    subscribe_protocol_to_client_wire, subscribe_session_request_from_wire,
};
#[cfg(test)]
pub(crate) use wire::{decode_session_output_event_payload, encode_session_output_event_payload};
