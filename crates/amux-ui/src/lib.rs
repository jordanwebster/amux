//! amux-ui: the client state library for amux UIs.
//!
//! A reducer over reified inputs (`docs/UI.md` is the normative design):
//! every stimulus is a serializable [`Msg`], transitions are the pure
//! [`update`] function, and side effects leave as [`Effect`] data executed
//! by the [`Runtime`] shell against `amux::Client`. There is exactly one
//! reducer implementation — this crate; renderers borrow the [`Model`] and
//! format, never derive.
//!
//! Purity boundary: `msg`, `model`, and `update` are the reducer core — no
//! IO, clocks, or randomness are imported there. The shell (`runtime`,
//! `recorder`) owns every resource.

pub mod claude;
pub mod codex;
mod effect;
mod model;
mod msg;
mod recorder;
mod runtime;
mod update;

// Kernel entity vocabulary re-exported so renderers depend on amux-ui alone.
pub use amux::{
    Agent, AgentId, AgentParent, AgentType, Capabilities, HostEntry, HostId, HostTrustStatus,
    WorkingOn,
};
pub use claude::{ClaudeCommand, SendGate};
pub use codex::{CodexCommand, CodexDecision, CodexInput};
pub use effect::{DumpReason, Effect, InputPayload};
pub use model::{
    AgentCard, AgentMessageKind, AgentPhase, Attention, Connection, FamilyMember, FamilyNeed,
    FinishedOp, FleetItem, HostState, Model, PendingOp, StreamPhase, StreamState,
    StructuredProtocol, Violation, Why, agent_type_label, display_name_fallback,
    format_relative_age,
};
pub use msg::{
    Command, DisconnectReason, Ephemeral, FlowClass, Msg, OpError, OpId, OpOutcome, ServerMsg,
    StreamCloseReason, StreamEntry, StreamMsg,
};
pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, DUMP_FORMAT_VERSION, DUMP_RETAINED_FILES, DumpHeader, Recorder,
    ReplayError, replay,
};
pub use runtime::{
    BUILD, ConnectFailure, ConnectFuture, Connector, Runtime, RuntimeOptions, write_panic_dump,
};
pub use update::{NOT_CONNECTED_ERROR, REPLAY_TAIL, update};
