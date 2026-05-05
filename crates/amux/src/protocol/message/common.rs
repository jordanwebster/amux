use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::route::Route;

/// Routed call identity carried by `RoutedFrame.call_id`.
///
/// The protobuf contract uses 128-bit non-zero call IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoutedCallId(Vec<u8>);

impl RoutedCallId {
    pub const LEN: usize = 16;

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() != Self::LEN {
            return Err(format!(
                "routed call_id must be {} bytes, got {}",
                Self::LEN,
                bytes.len()
            ));
        }
        if bytes.iter().all(|byte| *byte == 0) {
            return Err("routed call_id must be non-zero".to_string());
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Uuid> for RoutedCallId {
    fn from(uuid: Uuid) -> Self {
        assert_ne!(uuid, Uuid::nil(), "routed call_id must be non-zero");
        Self(uuid.as_bytes().to_vec())
    }
}

/// Information about a connected host (machine running amux server).
/// Propagated via peer routing events between servers.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Host {
    /// Stable ID loaded from state and announced to peers
    pub id: Uuid,
    /// Human-readable hostname from config
    pub name: String,
    /// Route to reach this host (built up as it propagates)
    pub route: Route,
    /// amux version of the host
    pub version: String,
}

/// Type of agent to spawn
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentType {
    /// Claude Code agent (sets AMUX_AGENT_ID env var)
    Claude,
    /// Test agent for E2E tests (only available in dev/test builds)
    #[cfg(any(debug_assertions, test))]
    TestAgent { command: String },
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookProvider {
    Claude,
    #[serde(other)]
    Unknown,
}

/// Replay query for sequenced output buffers.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SequencedReplayQuery {
    /// Replay entries with `seq >= seq`. O(log n) seek + O(k) replay.
    Since { seq: u64 },
    /// Replay only the last `count` entries. O(count) replay.
    Tail { count: u64 },
    #[serde(other)]
    Unknown,
}

/// Protocol-level errors that can be returned in response messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ProtocolError {
    /// The requested agent session is no longer available on this connection.
    #[error("No agent found")]
    NoAgentFound,
    /// The requested protocol method or variant is not implemented by this peer.
    #[error("{message}")]
    Unimplemented { message: String },
    /// The active call was cancelled by its caller.
    #[error("{message}")]
    Cancelled { message: String },
    /// The request payload or arguments are invalid.
    #[error("{message}")]
    InvalidArgument { message: String },
    /// The requested resource already exists.
    #[error("{message}")]
    AlreadyExists { message: String },
    /// The method exists, but the caller is not allowed to invoke it in this scope.
    #[error("{message}")]
    PermissionDenied { message: String },
    /// The routed call could not be delivered to its destination.
    #[error("{message}")]
    Unreachable { message: String },
    /// Generic server error with message
    #[error("{message}")]
    ServerError { message: String },
    /// Invalid or missing authentication credentials
    #[error("Invalid or missing credentials")]
    InvalidCredentials,
    /// The receiver was unable to allocate a required protocol resource.
    #[error("{message}")]
    ResourceExhausted { message: String },
    /// The proposed link name is invalid.
    #[error("invalid link name `{name}`: {reason}")]
    InvalidLinkName { name: String, reason: String },
    /// Protocol version mismatch between client and server
    #[error(
        "amux update required (supported protocol versions {supported_versions:?}, peer supports {peer_supported_versions:?})"
    )]
    ProtocolMismatch {
        supported_versions: Vec<u32>,
        peer_supported_versions: Vec<u32>,
    },
    /// Client binary version is below the server's minimum requirement.
    #[error("amux update required (minimum v{minimum_version}, you have v{client_version})")]
    UpdateRequired {
        minimum_version: String,
        client_version: String,
    },
    /// Structured input seq doesn't match current output seq
    #[error("sequence number mismatch (client {client_seq}, server {current_seq})")]
    SequenceNumberMismatch { client_seq: u64, current_seq: u64 },
    #[serde(other)]
    #[error("Unknown protocol error")]
    Unknown,
}

/// Output format requested for the server `debug` dump.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DebugFormat {
    #[default]
    Yaml,
    Json,
}

/// Reason for server shutdown notification
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    UpdateRequired,
    ProtocolError,
    UserRequested,
    Updating,
    Suspending,
    Restarting,
    AuthExpired,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownReason::UpdateRequired => write!(f, "amux update required"),
            ShutdownReason::ProtocolError => write!(f, "protocol error"),
            ShutdownReason::UserRequested => write!(f, "server shutting down"),
            ShutdownReason::Updating => write!(f, "server updating"),
            ShutdownReason::Suspending => write!(f, "server suspending"),
            ShutdownReason::Restarting => write!(f, "server restarting"),
            ShutdownReason::AuthExpired => write!(f, "authentication expired"),
        }
    }
}

/// Terminal dimensions for PTY creation and resizing
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// Request to create a new agent
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreateAgentRequest {
    pub agent_id: Uuid,
    pub name: Option<String>,
    pub agent_type: AgentType,
    pub working_dir: PathBuf,
    /// Terminal dimensions. None means use defaults (future: headless mode).
    #[serde(default)]
    pub terminal_size: Option<TerminalSize>,
    /// Extra arguments passed to the agent command (e.g., --fork-session --resume <id>)
    #[serde(default)]
    pub args: Vec<String>,
}

/// Request to rename an existing agent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RenameAgentRequest {
    pub agent_id: Uuid,
    pub name: String,
}
