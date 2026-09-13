//! Pure, transport-independent state transitions for amux UIs.
//!
//! A reducer over reified inputs (`docs/UI.md` is the normative design):
//! every stimulus is a serializable [`Msg`], transitions are the pure
//! [`update`] function, and side effects leave as [`Effect`] data. There is exactly one
//! reducer implementation — this crate; renderers borrow the [`Model`] and
//! format, never derive.
//!
//! Purity boundary: `msg`, `model`, and `update` are the reducer core — no
//! IO, clocks, or randomness are imported there.

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
pub mod review;
mod update;

// Kernel entity vocabulary re-exported so renderers depend on ui-state alone.
// The profile a runtime is bound to. Renderers name accounts, so the id
// travels with the rest of the entity vocabulary rather than making the
// TUI depend on the kernel crate.
pub use ::model::{
    Agent, AgentId, AgentKind, AgentParent, AgentType, ArtifactId, ArtifactKind, ArtifactRef,
    BaseIdentity, Capabilities, ClaudeDriver, DiffBase, DiffFile, DiffResponse, HostEntry, HostId,
    HostTrustStatus, ProfileId, Protocol, WorkingOn,
};
pub use attachments::{
    ARTIFACT_SIZE_CAP, AttachmentIndex, AttachmentKind, AttachmentLine, DIFF_MIME, DraftAttachment,
    Mention, MentionKind, PASTE_TOKEN_CHARS, PASTE_TOKEN_LINES, PASTED_NAME, Pasted, REVIEW_NAME,
    Segment, format_mention, paste, review_mention, split_mentions, text_mention,
};
pub use claude::{ClaudeCommand, SendGate};
pub use claude_sdk::{ClaudeSdkCommand, ClaudeSdkInput, SdkAnswer};
pub use codex::{CodexCommand, CodexDecision, CodexInput};
pub use effect::{DumpReason, Effect, InputPayload};
pub use msg::{
    Command, DisconnectReason, Ephemeral, FlowClass, Msg, OpError, OpId, OpOutcome, ServerMsg,
    StreamCloseReason, StreamEntry, StreamMsg,
};
pub use provider::ProviderFacts;
pub use queue::{Draft, DraftSegment, QueueCommand, QueueDelivery, QueuedMessage};
pub use update::{NOT_CONNECTED_ERROR, REPLAY_TAIL, update};

pub use self::model::{
    AgentCard, AgentLayer, AgentMessageKind, AgentMessagePresentation, AgentMessageSender,
    AgentPhase, Attention, ClaudeSdkLayer, Connection, FamilyMember, FamilyNeed, FinishedOp,
    FleetItem, HostState, MessageDigest, Model, PendingOp, StreamPhase, StreamState,
    StructuredProtocol, Violation, Why, agent_type_label, display_name_fallback,
    format_relative_age, message_digest,
};
