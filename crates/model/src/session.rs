//! Typed session arguments, inputs, outputs, and stream events shared by every
//! session RPC boundary: the client API, the routed client service, the node's
//! host boundary, and the provider runtime. The protobuf oneofs are translated
//! to and from these values exactly once, at the wire edge.

use crate::{
    ClaudePtyTranscriptV1Args, ClaudePtyTranscriptV1Input, ClaudeSdkInput, ClaudeSdkV1Args,
    CodexSdkInput, CodexSdkV1Args, Protocol, TerminalSize, TerminalV1Args,
};

/// Arguments selecting one session protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionArgs {
    TerminalV1(TerminalV1Args),
    ClaudePtyTranscriptV1(ClaudePtyTranscriptV1Args),
    ClaudeSdkV1(ClaudeSdkV1Args),
    CodexSdkV1(CodexSdkV1Args),
    TestEchoV1,
}

impl SessionArgs {
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::TerminalV1(_) => Protocol::TerminalV1,
            Self::ClaudePtyTranscriptV1(_) => Protocol::ClaudePtyTranscriptV1,
            Self::ClaudeSdkV1(_) => Protocol::ClaudeSdkV1,
            Self::CodexSdkV1(_) => Protocol::CodexSdkV1,
            Self::TestEchoV1 => Protocol::TestEchoV1,
        }
    }
}

/// Replay query shared by every sequenced session protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayQuery {
    /// Resume strictly after the last sequence observed by the client.
    After { after: u64, tail_bound: Option<u64> },
    /// Replay the newest retained rows, subject to the optional stricter cap.
    TailCount { count: u64, tail_bound: Option<u64> },
}

/// Input accepted by one session protocol or its shared control plane.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionInput {
    TerminalV1 { payload: Vec<u8> },
    ClaudePtyTranscriptV1(ClaudePtyTranscriptV1Input),
    ClaudeSdkV1(ClaudeSdkInput),
    CodexSdkV1(CodexSdkInput),
    Control(SessionControl),
    TestEchoV1 { payload: Vec<u8> },
}

impl SessionInput {
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::TerminalV1 { .. } | Self::Control(_) => Protocol::TerminalV1,
            Self::ClaudePtyTranscriptV1(_) => Protocol::ClaudePtyTranscriptV1,
            Self::ClaudeSdkV1(_) => Protocol::ClaudeSdkV1,
            Self::CodexSdkV1(_) => Protocol::CodexSdkV1,
            Self::TestEchoV1 { .. } => Protocol::TestEchoV1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControl {
    Resize(TerminalSize),
}

/// Output produced by one session protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutput {
    TerminalV1 { payload: Vec<u8> },
    ClaudePtyTranscriptV1(StructuredRow),
    ClaudeSdkV1(StructuredRow),
    CodexSdkV1(StructuredRow),
    TestEchoV1 { payload: Vec<u8> },
}

/// One sequenced row of a structured protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredRow {
    pub seq: u64,
    pub published_at_unix_ms: i64,
    pub activity_at_unix_ms: Option<i64>,
    pub historical: bool,
    pub payload: Vec<u8>,
}

/// Facts about the replay snapshot selected for a structured subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayFacts {
    pub retained_from: u64,
    pub through: u64,
    pub selected_from: u64,
    pub reset_at: u64,
    pub outcome: ReplayOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayOutcome {
    Continuous,
    Truncated { missing_after: u64 },
    Reset { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscribeSessionEvent {
    /// Unset replay facts mean a byte-oriented terminal session.
    Opened {
        replay: Option<ReplayFacts>,
    },
    Output(SessionOutput),
    ReplayComplete,
    Closed {
        reason: SessionCloseReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseReason {
    AgentDeleted,
    AgentExited { exit_code: Option<i32> },
    HostUnreachable,
    Reset,
    InternalError { detail: String },
}

impl std::fmt::Display for SessionCloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentDeleted => f.write_str("agent deleted"),
            Self::AgentExited {
                exit_code: Some(code),
            } => {
                write!(f, "agent exited with status {code}")
            }
            Self::AgentExited { exit_code: None } => f.write_str("agent exited"),
            Self::HostUnreachable => f.write_str("host unreachable"),
            Self::Reset => f.write_str("session reset"),
            Self::InternalError { detail } => write!(f, "internal error: {detail}"),
        }
    }
}
