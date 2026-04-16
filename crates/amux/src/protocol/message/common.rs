use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::route::Route;

/// Information about a connected host (machine running amux server).
/// Propagated via AnnounceHost/WithdrawHost between peers.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(pub Uuid);

impl SubscriptionId {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn nil() -> Self {
        Self(Uuid::nil())
    }

    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Query parameter for structured subscriptions, controlling which entries
/// are replayed on subscribe.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubscribeQuery {
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
    /// The requested subscription is unknown or has already ended.
    #[error("Unknown subscription")]
    UnknownSubscription,
    /// The requested structured subscribe query is not supported by this peer.
    #[error("Unsupported subscribe query")]
    UnsupportedSubscribeQuery,
    /// Generic server error with message
    #[error("{message}")]
    ServerError { message: String },
    /// The proposed link name is already in use
    #[error("Link name already in use")]
    LinkNameTaken,
    /// Invalid or missing authentication credentials
    #[error("Invalid or missing credentials")]
    InvalidCredentials,
    /// The proposed link name is invalid (e.g., contains "." which is the route separator)
    #[error("Invalid link name (must not contain '.')")]
    InvalidLinkName,
    /// Protocol version mismatch between client and server
    #[error("amux upgrade required (protocol v{server_version}, client v{client_version})")]
    ProtocolMismatch {
        server_version: u32,
        client_version: u32,
    },
    /// Client binary version is below the server's minimum requirement
    #[error("amux upgrade required (minimum v{minimum_version}, you have v{client_version})")]
    UpgradeRequired {
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
    ProtocolMismatch,
    UserRequested,
    Updating,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionCloseReason {
    SourceClosed,
    Unsubscribed,
    LeaseExpired,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownReason::ProtocolMismatch => write!(f, "amux upgrade required"),
            ShutdownReason::UserRequested => write!(f, "server shutting down"),
            ShutdownReason::Updating => write!(f, "server updating"),
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
