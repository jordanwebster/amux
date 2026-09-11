//! Agent runtime: session lifecycle, PTY management, and hook dispatch.

mod attachments;
mod buffer;
pub(crate) mod claude;
pub(crate) mod codex;
mod debug;
mod hook;
mod log_source;
mod naming;
mod pty;
mod record;
mod session;
pub(crate) mod terminal_io;
#[cfg(any(debug_assertions, test))]
mod test_agent;
pub use attachments::attachments_row;
pub(crate) use attachments::{
    ArtifactOwners, MaterialiseBackend, artifact_read_rule, compute_diff, materialise_and_log,
    materialise_paths, spawn_artifact_sweeper, store_error,
};
pub(crate) use buffer::{
    BroadcastRead, ByteReplayQuery, MultiplexByteBuffer, MultiplexByteReader,
    MultiplexStructuredBuffer, MultiplexStructuredReader, SequencedReplayQuery, StructuredOutput,
};
#[cfg(unix)]
pub(crate) use codex::CodexRawPtyLease;
pub(crate) use debug::{BackendState, BufferDebug, ObligationDebug, OutputDebug, SessionDebug};
pub(crate) use hook::{ExternalHookBootstrap, HookError, HookOutcome};
pub(crate) use log_source::StructuredLogSource;
pub(crate) use model::{HookEnvironment, SpawnInheritance};
pub use model::{
    Agent, AgentEvent, AgentKind, AgentParent, AgentType, ArtifactRef, ClaudeDriver,
    CreateAgentRequest, Protocol, RenameAgentRequest,
    SessionCloseReason, TerminalSize, WorkingOn,
};
pub(crate) use naming::LocalAgentNameSource;
pub(crate) use pty::{PtyHandle, spawn_pty_agent};
pub(crate) use record::{AgentRecord, SessionEvent, StopPolicy};
#[cfg(test)]
pub(crate) use session::mcp_launch_route_for_tests;
pub(crate) use session::{
    AgentBackend, AgentDeliveryTarget, AgentDeps, AgentSession, Delivery, DeliveryError,
    DeliveryLiveness, McpLaunchRoute, Plane, RawPtyTarget, StructuredInput, StructuredInputEvent,
    agent_from_suspended, bootstrap_external_hook, new_agent,
};
#[cfg(any(debug_assertions, test))]
pub(crate) use test_agent::TestAgentSession;
#[cfg(test)]
pub(crate) use test_agent::io::{
    TEST_DELAYED_DELIVERY_COMMAND, TEST_FAILED_DELIVERY_COMMAND, TEST_UNAVAILABLE_DELIVERY_COMMAND,
};
#[cfg(any(test, testnet))]
pub(crate) use test_agent::io::{TEST_ECHO_COMMAND, TEST_ECHO_V1};
