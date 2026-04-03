use crate::agent_registry::Agent;
use crate::config::Config;
use crate::route::Route;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::claude::types::{AgentStructuredInput, AgentStructuredOutput, Hook};

/// Information about a connected host (machine running amux server).
/// Propagated via AnnounceHost/WithdrawHost between peers.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Host {
    /// Ephemeral ID generated at server startup (not persisted)
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
pub enum AgentType {
    /// Claude Code agent (sets AMUX_AGENT_ID env var)
    Claude,
    /// Test agent for E2E tests (only available in dev/test builds)
    #[cfg(any(debug_assertions, test))]
    TestAgent(String),
}

/// Protocol-level errors that can be returned in response messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    /// Generic server error with message
    #[error("{0}")]
    ServerError(String),
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
    VersionMismatch {
        server_version: u32,
        client_version: u32,
    },
    /// Structured input seq doesn't match current output seq
    #[error("sequence number mismatch (client {client_seq}, server {current_seq})")]
    SequenceNumberMismatch { client_seq: u64, current_seq: u64 },
    /// Structured input was sent to an agent that expects a different input family
    #[error("structured input type mismatch (expected {expected}, received {received})")]
    StructuredInputTypeMismatch { expected: String, received: String },
}

/// Reason for server shutdown notification
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ShutdownReason {
    ProtocolMismatch,
    UserRequested,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownReason::ProtocolMismatch => write!(f, "amux upgrade required"),
            ShutdownReason::UserRequested => write!(f, "server shutting down"),
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

/// Messages that carry src/dst routing information and can be forwarded across hops.
/// agent_id is Uuid — callers must resolve names to UUIDs before constructing.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RoutableMessage {
    SubscribeRaw {
        agent_id: Uuid,
        /// Terminal dimensions for PTY resize. None means don't resize.
        #[serde(default)]
        terminal_size: Option<TerminalSize>,
    },
    SubscribeStructured {
        agent_id: Uuid,
    },
    SubscribeRawResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    SubscribeStructuredResult {
        agent_id: Uuid,
        /// Current sequence number at subscribe time. Clients use this as
        /// their initial seq when no StructuredOutput messages have arrived.
        seq: u64,
        error: Option<ProtocolError>,
    },
    CreateAgent(CreateAgentRequest),
    CreateAgentResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    RawInput {
        agent_id: Uuid,
        data: Vec<u8>,
    },
    RawOutput {
        agent_id: Uuid,
        data: Vec<u8>,
    },
    StructuredOutput {
        agent_id: Uuid,
        seq: u64,
        data: Box<AgentStructuredOutput>,
    },
    StructuredInput {
        agent_id: Uuid,
        seq: u64,
        data: AgentStructuredInput,
    },
    StructuredInputResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    SubscriptionClosed {
        agent_id: Uuid,
    },
    UnknownMessage,
}

impl RoutableMessage {
    /// Human-readable label for this variant, used in error messages.
    pub fn type_label(&self) -> &'static str {
        match self {
            RoutableMessage::SubscribeRaw { .. } => "SubscribeRaw",
            RoutableMessage::SubscribeStructured { .. } => "SubscribeStructured",
            RoutableMessage::SubscribeRawResult { .. } => "SubscribeRawResult",
            RoutableMessage::SubscribeStructuredResult { .. } => "SubscribeStructuredResult",
            RoutableMessage::CreateAgent(_) => "CreateAgent",
            RoutableMessage::CreateAgentResult { .. } => "CreateAgentResult",
            RoutableMessage::RawInput { .. } => "RawInput",
            RoutableMessage::RawOutput { .. } => "RawOutput",
            RoutableMessage::StructuredOutput { .. } => "StructuredOutput",
            RoutableMessage::StructuredInput { .. } => "StructuredInput",
            RoutableMessage::StructuredInputResult { .. } => "StructuredInputResult",
            RoutableMessage::SubscriptionClosed { .. } => "SubscriptionClosed",
            RoutableMessage::UnknownMessage => "UnknownMessage",
        }
    }

    /// Encode routable message to bytes using MessagePack (named/map format)
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode routable message from bytes using MessagePack
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

/// Messages that are handled directly by the receiving server (no routing).
/// Used for peer-to-peer protocol messages after handshake.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DirectMessage {
    Reauth {
        token: String,
    },
    ReauthResult {
        error: Option<ProtocolError>,
    },
    /// Advertise or refresh agent metadata for a known UUID.
    AnnounceAgent {
        agent_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        route: Route,
        agent_type: AgentType,
        readonly: bool,
        created_at: DateTime<Utc>,
    },
    WithdrawAgent {
        agent_id: Uuid,
    },
    AnnounceHost {
        id: Uuid,
        name: String,
        route: Route,
        version: String,
    },
    WithdrawHost {
        id: Uuid,
    },
}

/// CLI-only commands that must not be accepted from remote peers.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    ListAgents,
    ListAgentsResult {
        agents: Vec<Agent>,
    },
    ResolveAgent {
        identifier: String,
    },
    ResolveAgentResult {
        agent: Option<Agent>,
    },
    Shutdown,
    ShutdownNotification(ShutdownReason),
    Debug,
    DebugResult {
        info: ServerDebugInfo,
    },
    ConnectToServer {
        address: String,
    },
    ConnectToServerResult {
        error: Option<ProtocolError>,
    },
    HandleHook {
        agent_id: Uuid,
        hook: Box<Hook>,
        #[serde(default)]
        source_ppid: Option<u32>,
    },
    HandleHookResult {
        error: Option<ProtocolError>,
    },
    Suspend,
    SuspendResult {
        suspended_count: usize,
        error: Option<ProtocolError>,
    },
    Resume,
    ResumeResult {
        resumed_count: usize,
        failed_count: usize,
        error: Option<ProtocolError>,
    },
}

impl Command {
    /// Short label for this variant, for use in logs and error messages
    pub fn type_label(&self) -> &'static str {
        match self {
            Command::ListAgents => "ListAgents",
            Command::ListAgentsResult { .. } => "ListAgentsResult",
            Command::ResolveAgent { .. } => "ResolveAgent",
            Command::ResolveAgentResult { .. } => "ResolveAgentResult",
            Command::Shutdown => "Shutdown",
            Command::ShutdownNotification(_) => "ShutdownNotification",
            Command::Debug => "Debug",
            Command::DebugResult { .. } => "DebugResult",
            Command::ConnectToServer { .. } => "ConnectToServer",
            Command::ConnectToServerResult { .. } => "ConnectToServerResult",
            Command::HandleHook { .. } => "HandleHook",
            Command::HandleHookResult { .. } => "HandleHookResult",
            Command::Suspend => "Suspend",
            Command::SuspendResult { .. } => "SuspendResult",
            Command::Resume => "Resume",
            Command::ResumeResult { .. } => "ResumeResult",
        }
    }
}

/// All protocol messages between client and server
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Routable {
        src: Route,
        dst: Route,
        request_id: u64,
        payload: Vec<u8>,
    },
    Direct(DirectMessage),
    Command(Command),
}

impl Message {
    /// Convenience constructor for Routable messages.
    /// Encodes the RoutableMessage into the opaque payload.
    pub fn routable(src: Route, dst: Route, request_id: u64, message: &RoutableMessage) -> Self {
        Message::Routable {
            src,
            dst,
            request_id,
            payload: message
                .encode()
                .expect("RoutableMessage encode cannot fail"),
        }
    }

    /// Short qualified label for this variant, for use in logs and error messages.
    /// Returns e.g. "Routable", "Direct::Reauth", "Command::Shutdown".
    pub fn type_label(&self) -> &'static str {
        match self {
            Message::Routable { .. } => "Routable",
            Message::Direct(d) => match d {
                DirectMessage::Reauth { .. } => "Direct::Reauth",
                DirectMessage::ReauthResult { .. } => "Direct::ReauthResult",
                DirectMessage::AnnounceAgent { .. } => "Direct::AnnounceAgent",
                DirectMessage::WithdrawAgent { .. } => "Direct::WithdrawAgent",
                DirectMessage::AnnounceHost { .. } => "Direct::AnnounceHost",
                DirectMessage::WithdrawHost { .. } => "Direct::WithdrawHost",
            },
            Message::Command(c) => match c {
                Command::ListAgents => "Command::ListAgents",
                Command::ListAgentsResult { .. } => "Command::ListAgentsResult",
                Command::ResolveAgent { .. } => "Command::ResolveAgent",
                Command::ResolveAgentResult { .. } => "Command::ResolveAgentResult",
                Command::Shutdown => "Command::Shutdown",
                Command::ShutdownNotification(_) => "Command::ShutdownNotification",
                Command::Debug => "Command::Debug",
                Command::DebugResult { .. } => "Command::DebugResult",
                Command::ConnectToServer { .. } => "Command::ConnectToServer",
                Command::ConnectToServerResult { .. } => "Command::ConnectToServerResult",
                Command::HandleHook { .. } => "Command::HandleHook",
                Command::HandleHookResult { .. } => "Command::HandleHookResult",
                Command::Suspend => "Command::Suspend",
                Command::SuspendResult { .. } => "Command::SuspendResult",
                Command::Resume => "Command::Resume",
                Command::ResumeResult { .. } => "Command::ResumeResult",
            },
        }
    }
}

/// Debug information about server state (aggregated across all users)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerDebugInfo {
    /// Whether this server is running as a cloud server (TLS + token auth)
    pub is_cloud_server: bool,
    /// Whether cloud mode is enabled in state (connect to cloud)
    pub use_cloud_mode: bool,
    pub user_count: usize,
    pub agent_count: usize,
    pub remote_agent_count: usize,
    pub host_count: usize,
    pub route_count: usize,
    pub peer_link_count: usize,
    pub config: Config,
}

impl Message {
    /// Encode message to bytes using MessagePack (named/map format for compatibility)
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode message from bytes using MessagePack
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Core architectural pattern: opaque payload two-step encoding ---

    #[test]
    fn test_routable_message_standalone_encode_decode() {
        let agent_id = Uuid::new_v4();
        let rm = RoutableMessage::RawOutput {
            agent_id,
            data: b"hello".to_vec(),
        };
        let encoded = rm.encode().unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::RawOutput {
            agent_id: decoded_id,
            data,
        } = decoded
        else {
            panic!("Expected RawOutput");
        };
        assert_eq!(decoded_id, agent_id);
        assert_eq!(data, b"hello");
    }

    #[test]
    fn test_opaque_payload_two_step_roundtrip() {
        // Core architectural invariant: RoutableMessage is encoded into an opaque
        // payload inside Message::Routable, allowing intermediate hops to forward
        // without deserializing the inner message.
        let agent_id = Uuid::new_v4();
        let rm = RoutableMessage::CreateAgentResult {
            agent_id,
            error: None,
        };
        let msg = Message::routable(Route::from_link("src"), Route::from_link("dst"), 99, &rm);
        let wire = msg.encode().unwrap();
        let decoded_msg = Message::decode(&wire).unwrap();
        let Message::Routable {
            payload,
            request_id,
            ..
        } = decoded_msg
        else {
            panic!("Expected Routable");
        };
        assert_eq!(request_id, 99);
        let decoded_rm = RoutableMessage::decode(&payload).unwrap();
        assert!(matches!(
            decoded_rm,
            RoutableMessage::CreateAgentResult { error: None, .. }
        ));
    }

    // --- Behavioral method tests ---

    #[test]
    fn test_agent_is_remote_after_deserialization() {
        let info = Agent {
            id: Uuid::new_v4(),
            name: Some("remote-agent".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::from_link("host-a"),
            agent_type: AgentType::Claude,
            readonly: false,
            created_at: Utc::now(),
        };
        let encoded = rmp_serde::to_vec_named(&info).unwrap();
        let decoded: Agent = rmp_serde::from_slice(&encoded).unwrap();
        assert!(decoded.is_remote());
    }

    // --- Backward compatibility: #[serde(default)] contract tests ---
    // These simulate old clients that lack fields added in later versions.
    // Removing the #[serde(default)] annotations would silently break compat.

    #[test]
    fn test_subscribe_raw_backward_compat_without_terminal_size() {
        // Old clients send SubscribeRaw without terminal_size field.
        // The #[serde(default)] ensures it defaults to None.
        #[derive(Serialize)]
        enum OldRoutableMessage {
            SubscribeRaw { agent_id: Uuid },
        }
        let agent_id = Uuid::new_v4();
        let old_msg = OldRoutableMessage::SubscribeRaw { agent_id };
        let encoded = rmp_serde::to_vec_named(&old_msg).unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::SubscribeRaw {
            agent_id: decoded_id,
            terminal_size,
        } = decoded
        else {
            panic!("Expected SubscribeRaw, got {:?}", decoded);
        };
        assert_eq!(decoded_id, agent_id);
        assert_eq!(
            terminal_size, None,
            "missing terminal_size should default to None"
        );
    }

    #[test]
    fn test_create_agent_backward_compat_without_terminal_size() {
        // Old clients send CreateAgent without terminal_size field.
        // The #[serde(default)] ensures it defaults to None.
        #[derive(Serialize)]
        struct OldCreateAgentRequest {
            agent_id: Uuid,
            name: Option<String>,
            agent_type: AgentType,
            working_dir: PathBuf,
        }
        #[derive(Serialize)]
        enum OldRoutableMessage {
            CreateAgent(OldCreateAgentRequest),
        }
        let agent_id = Uuid::new_v4();
        let old_msg = OldRoutableMessage::CreateAgent(OldCreateAgentRequest {
            agent_id,
            name: Some("test".to_string()),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp"),
        });
        let encoded = rmp_serde::to_vec_named(&old_msg).unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::CreateAgent(req) = decoded else {
            panic!("Expected CreateAgent, got {:?}", decoded);
        };
        assert_eq!(req.agent_id, agent_id);
        assert_eq!(
            req.terminal_size, None,
            "missing terminal_size should default to None"
        );
    }

    // --- Forward compatibility contract tests ---
    // These verify that unknown message variants produce decode errors (not panics),
    // which the reader_loop skips to keep the connection alive.

    #[test]
    fn test_unknown_direct_variant_produces_decode_error() {
        // Simulate a future DirectMessage variant that this version doesn't know about.
        // The reader_loop handles this by skipping the message (not killing the connection).
        #[derive(Serialize)]
        enum FutureDirectMessage {
            Ping { seq: u64 },
        }
        #[derive(Serialize)]
        enum FutureMessage {
            Direct(FutureDirectMessage),
        }
        let future_msg = FutureMessage::Direct(FutureDirectMessage::Ping { seq: 42 });
        let encoded = rmp_serde::to_vec_named(&future_msg).unwrap();
        assert!(
            Message::decode(&encoded).is_err(),
            "unknown DirectMessage variant should produce a decode error"
        );
    }

    #[test]
    fn test_unknown_top_level_variant_produces_decode_error() {
        // Simulate a future top-level Message variant that this version doesn't know about.
        #[derive(Serialize)]
        enum FutureMessage {
            Stream { id: u64 },
        }
        let future_msg = FutureMessage::Stream { id: 1 };
        let encoded = rmp_serde::to_vec_named(&future_msg).unwrap();
        assert!(
            Message::decode(&encoded).is_err(),
            "unknown Message variant should produce a decode error"
        );
    }
}
