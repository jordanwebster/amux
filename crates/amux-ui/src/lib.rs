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

pub mod attachments;
pub mod claude;
pub mod claude_sdk;
pub mod codex;
pub mod diff;
mod effect;
mod model;
mod msg;
pub mod provider;
pub mod queue;
pub use provider::ProviderFacts;
mod recorder;
pub mod report;
pub mod review;
mod runtime;
mod update;

// Kernel entity vocabulary re-exported so renderers depend on amux-ui alone.
// The profile a runtime is bound to. Renderers name accounts, so the id
// travels with the rest of the entity vocabulary rather than making the
// TUI depend on the kernel crate.
pub use amux::installation::ProfileId;
pub use amux::{
    Agent, AgentId, AgentKind, AgentParent, AgentType, ArtifactId, ArtifactKind, ArtifactRef,
    BaseIdentity, Capabilities, ClaudeDriver, DiffBase, DiffFile, DiffResponse, HostEntry, HostId,
    HostTrustStatus, Protocol, WorkingOn, claude_io,
};
pub use attachments::{
    ARTIFACT_SIZE_CAP, AttachmentIndex, AttachmentKind, AttachmentLine, DraftAttachment, Mention,
    MentionKind, Segment, format_mention, split_mentions,
};
pub use claude::{ClaudeCommand, SendGate};
pub use claude_sdk::{ClaudeSdkCommand, ClaudeSdkInput, SdkAnswer};
pub use codex::{CodexCommand, CodexDecision, CodexInput};
pub use effect::{DumpReason, Effect, InputPayload};
pub use model::{
    AgentCard, AgentLayer, AgentMessageKind, AgentMessagePresentation, AgentMessageSender,
    AgentPhase, Attention, ClaudeSdkLayer, Connection, FamilyMember, FamilyNeed, FinishedOp,
    FleetItem, HostState, MessageDigest, Model, PendingOp, StreamPhase, StreamState,
    StructuredProtocol, Violation, Why, agent_type_label, display_name_fallback,
    format_relative_age, message_digest,
};
pub use msg::{
    Command, DisconnectReason, Ephemeral, FlowClass, Msg, OpError, OpId, OpOutcome, ServerMsg,
    StreamCloseReason, StreamEntry, StreamMsg,
};
pub use queue::{Draft, DraftSegment, QueueCommand, QueueDelivery, QueuedMessage};
pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, MSGS_SCHEMA_VERSION, Recorder, RecorderSnapshot, ReplayError,
    replay_msgs,
};
pub use runtime::{
    AttachmentClient, AttachmentClientFuture, AttachmentOpener, BUILD, ConnectFailure,
    ConnectFuture, Connector, Generation, LateResult, MsgTap, ProfileDirectory, ProfileEntry,
    ReportExtras, ReportExtrasProvider, Runtime, RuntimeOptions, execute_put_then_send,
    write_panic_report,
};
#[cfg(debug_assertions)]
pub use runtime::{RuntimeGone, ShellEdge};
pub use update::{NOT_CONNECTED_ERROR, REPLAY_TAIL, update};
