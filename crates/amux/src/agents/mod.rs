//! Agent runtime: session lifecycle, PTY management, and hook dispatch.

mod buffer;
pub(crate) mod claude;
mod events;
mod hook;
mod log_source;
mod naming;
mod pty;
mod session;
mod session_events;
#[cfg(any(debug_assertions, test))]
mod test_agent;
mod types;
mod wire;

pub(crate) use buffer::{
    BroadcastRead, ByteReplayQuery, MultiplexByteBuffer, MultiplexByteReader,
    MultiplexStructuredBuffer, MultiplexStructuredReader, SequencedReplayQuery, StructuredOutput,
};
pub use events::AgentEvent;
pub(crate) use events::{agent_event_from_wire, agent_event_to_wire};
pub(crate) use hook::{ExternalHookBootstrap, HookError, HookOutcome};
pub(crate) use log_source::StructuredLogSource;
pub(crate) use naming::LocalAgentNameSource;
pub(crate) use pty::{PtyHandle, spawn_pty_agent};
pub(crate) use session::{
    AgentRecord, AgentSession, SessionEvent, StopPolicy, StructuredInputTarget,
};
pub use session_events::{SessionCloseReason, SubscribeSessionEvent};
#[cfg(any(debug_assertions, test))]
pub(crate) use test_agent::TestAgentSession;
#[cfg(any(test, feature = "testnet"))]
pub(crate) use test_agent::io::{TEST_ECHO_COMMAND, TEST_ECHO_V1};
pub(crate) use types::AGENT_TYPE_CLAUDE;
#[cfg(any(debug_assertions, test))]
pub(crate) use types::AGENT_TYPE_TEST_AGENT;
pub use types::{Agent, AgentType, CreateAgentRequest, RenameAgentRequest, TerminalSize};
pub(crate) use wire::{
    CreateAgentConfig, CreateAgentRpcRequest, SendInputRequest, SessionInputEvent,
    SubscribeSessionRequest, agent_from_wire, agent_to_wire, create_agent_request_from_wire,
    delete_agent_id_from_wire, rename_agent_request_from_wire, send_input_request_from_wire,
    session_output_event_to_wire, subscribe_session_request_from_wire,
};
#[cfg(test)]
pub(crate) use wire::{decode_session_output_event_payload, encode_session_output_event_payload};
