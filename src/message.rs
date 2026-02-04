use crate::config::Config;
use crate::route::Route;
use crate::structured_log::StructuredLog;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Type of agent to spawn
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AgentType {
    /// Claude Code agent (passes --session-id to claude command)
    Claude,
    /// Test agent for E2E tests (only available in dev/test builds)
    #[cfg(any(debug_assertions, test))]
    TestAgent(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Hook {
    Claude(ClaudeHook),
}

/// Claude Code hook event - uses Claude's tagged JSON format
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
    PermissionRequest(ClaudePermissionRequest),
}

/// SessionStart hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeSessionStart {
    pub session_id: Uuid,
    pub transcript_path: String,
}

/// PermissionRequest hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudePermissionRequest {
    pub session_id: Uuid,
    #[serde(flatten)]
    pub tool: ClaudePermissionTool,
}

/// Tool data from Claude Code permission requests.
/// Uses adjacently-tagged format: tool_name determines variant, tool_input is content.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "tool_name", content = "tool_input")]
pub enum ClaudePermissionTool {
    Edit {
        file_path: String,
        old_string: String,
        new_string: String,
        #[serde(default)]
        replace_all: bool,
    },
}

/// Response to a permission request (sent from dashboard to server)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PermissionResponse {
    /// Press "1" - accept this edit
    Yes,
    /// Press "2" - accept all edits
    YesAll,
    /// Press "3" - deny
    No,
}

/// Protocol-level errors that can be returned in response messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ProtocolError {
    /// Generic server error with message
    ServerError(String),
    /// The proposed link name is already in use
    LinkNameTaken,
    /// No route found to reach the destination. Contains the path traversed
    /// up to and including the hop that couldn't be resolved.
    NoRouteFound(Route),
    /// Invalid or missing authentication credentials
    InvalidCredentials,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::ServerError(msg) => write!(f, "{}", msg),
            ProtocolError::LinkNameTaken => write!(f, "Link name already in use"),
            ProtocolError::NoRouteFound(route) => write!(f, "No route found: {:?}", route),
            ProtocolError::InvalidCredentials => write!(f, "Invalid or missing credentials"),
        }
    }
}

/// Request to create a new agent
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreateAgentRequest {
    pub agent_id: Uuid,
    pub alias: Option<String>,
    pub agent_type: AgentType,
    pub working_dir: PathBuf,
    pub rows: u16,
    pub cols: u16,
}

/// All protocol messages between client and server
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    // Client -> Server
    /// List all running agents
    ListAgents,

    /// Create a new agent with the given type
    CreateAgent(CreateAgentRequest),

    /// Subscribe to an agent's output stream (routable)
    /// agent_id can be a UUID string or an alias
    Subscribe {
        src: Route,
        dst: Route,
        agent_id: String,
        rows: u16,
        cols: u16,
    },

    /// Send raw input bytes to the subscribed agent (routable)
    /// agent_id can be a UUID string or an alias
    /// No automatic Enter - bytes are written directly to PTY
    InputBytes {
        src: Route,
        dst: Route,
        agent_id: String,
        data: Vec<u8>,
    },

    /// Send input text and submit (WebSocket only)
    /// Writes data bytes, waits briefly, then sends Enter
    /// This ensures Claude Code interprets Enter as "submit" not "newline"
    SubmitInput {
        src: Route,
        dst: Route,
        agent_id: String,
        data: Vec<u8>,
    },

    /// Shutdown the server
    Shutdown,

    // Server -> Client
    /// Response to ListAgents
    ListAgentsResult { agents: Vec<AgentInfo> },

    /// Response to CreateAgent
    CreateAgentResult {
        success: bool,
        error: Option<ProtocolError>,
    },

    /// Response to Subscribe (routable)
    SubscribeResult {
        src: Route,
        dst: Route,
        agent_id: String,
        success: bool,
        error: Option<ProtocolError>,
    },

    /// Output bytes from the agent (routable)
    Output {
        src: Route,
        dst: Route,
        agent_id: String,
        data: Vec<u8>,
    },

    /// Agent session has ended
    AgentEnded,

    /// Generic error response
    Error { message: String },

    // Client -> Server: remote connection management
    /// Request local server to connect to a remote amux server
    ConnectToServer { address: String },

    /// Response to ConnectToServer
    ConnectToServerResult {
        success: bool,
        error: Option<ProtocolError>,
    },

    // Handshake (unified for client-server and server-server)
    /// Sent to initiate connection handshake with proposed link name.
    /// Token is optional - required when connecting to cloud servers.
    Connect {
        link_name: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        token: Option<String>,
    },

    /// Response to Connect
    ConnectResponse {
        success: bool,
        error: Option<ProtocolError>,
    },

    // Hook events
    /// Hook event from CLI hook handler (e.g., Claude Code SessionStart)
    HookEvent { hook: Hook },

    /// Acknowledgement of HookEvent
    HookEventResult {
        success: bool,
        error: Option<ProtocolError>,
    },

    // Structured output for WebSocket clients
    /// Structured log entry from agent (for WebSocket subscribers)
    StructuredOutput {
        src: Route,
        dst: Route,
        agent_id: String,
        entry: StructuredLog,
    },

    // Permission request response (from dashboard to server, routable)
    /// Response to a permission request - sends keystroke to agent
    PermissionRequestResponse {
        src: Route,
        dst: Route,
        agent_id: String,
        response: PermissionResponse,
    },

    // Debug
    /// Request server debug information (local only)
    Debug,

    /// Response to Debug
    DebugResult { info: ServerDebugInfo },
}

/// Information about a running agent
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub agent_id: Uuid,
    pub alias: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
}

/// Debug information about server state
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerDebugInfo {
    /// Whether this server is running as a cloud server (TLS + token auth)
    pub is_cloud_server: bool,
    /// Whether cloud mode is enabled in state (connect to cloud)
    pub use_cloud_mode: bool,
    pub agent_count: usize,
    pub route_count: usize,
    pub routes: Vec<String>,
    pub config: ConfigDebugInfo,
}

/// Debug information about server config
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfigDebugInfo {
    pub host_name: String,
    pub socket_path: PathBuf,
    pub tcp_port: u16,
    pub websocket_port: u16,
    pub cloud_url: String,
}

impl From<&Config> for ConfigDebugInfo {
    fn from(config: &Config) -> Self {
        Self {
            host_name: config.host_name.clone(),
            socket_path: config.socket_path.clone(),
            tcp_port: config.tcp_port,
            websocket_port: config.websocket_port,
            cloud_url: config.cloud_url.clone(),
        }
    }
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

    #[test]
    fn test_message_roundtrip_list_agents() {
        let msg = Message::ListAgents;
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(decoded, Message::ListAgents));
    }

    #[test]
    fn test_message_roundtrip_create_agent() {
        let test_uuid = Uuid::new_v4();
        let msg = Message::CreateAgent(CreateAgentRequest {
            agent_id: test_uuid,
            alias: Some("test".to_string()),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/home/user/project"),
            rows: 24,
            cols: 80,
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::CreateAgent(req) = decoded {
            assert_eq!(req.agent_id, test_uuid);
            assert_eq!(req.alias, Some("test".to_string()));
            assert_eq!(req.agent_type, AgentType::Claude);
            assert_eq!(req.working_dir, PathBuf::from("/home/user/project"));
            assert_eq!(req.rows, 24);
            assert_eq!(req.cols, 80);
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[test]
    fn test_message_roundtrip_subscribe_result() {
        let msg = Message::SubscribeResult {
            src: Route::from_link("host-a"),
            dst: Route::from_link("host-b"),
            agent_id: Uuid::new_v4().to_string(),
            success: true,
            error: None,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::SubscribeResult { success, error, .. } = decoded {
            assert!(success);
            assert!(error.is_none());
        } else {
            panic!("Expected SubscribeResult");
        }
    }

    #[test]
    fn test_agent_info_roundtrip() {
        let test_uuid = Uuid::new_v4();
        let info = AgentInfo {
            agent_id: test_uuid,
            alias: Some("claude-1".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };
        let encoded = rmp_serde::to_vec(&info).unwrap();
        let decoded: AgentInfo = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded.agent_id, test_uuid);
        assert_eq!(decoded.alias, Some("claude-1".to_string()));
        assert_eq!(decoded.command, "claude");
        assert_eq!(decoded.working_dir, PathBuf::from("/tmp"));
    }

    #[test]
    fn test_message_roundtrip_hook_event() {
        let test_uuid = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: test_uuid,
            transcript_path: "/tmp/transcript.jsonl".to_string(),
        }));
        let msg = Message::HookEvent { hook };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::HookEvent { hook: decoded_hook } = decoded else {
            panic!("Expected HookEvent");
        };
        match decoded_hook {
            Hook::Claude(ClaudeHook::SessionStart(session)) => {
                assert_eq!(session.session_id, test_uuid);
                assert_eq!(session.transcript_path, "/tmp/transcript.jsonl");
            }
            _ => panic!("Expected SessionStart hook"),
        }
    }
}
