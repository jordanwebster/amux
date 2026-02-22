use crate::agent_registry::Agent;
use crate::config::Config;
use crate::route::Route;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

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

/// Protocol version for Connect handshake. Increment on breaking changes.
pub const PROTOCOL_VERSION: u32 = 3;

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

/// Tool input fields for the Edit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EditToolInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Tool input fields for the AskUserQuestion tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionToolInput {
    pub questions: Vec<AskUserQuestionItem>,
}

/// A single question within an AskUserQuestion request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionItem {
    pub question: String,
    pub header: String,
    pub options: Vec<AskUserQuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

/// An option within an AskUserQuestion question
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionOption {
    pub label: String,
    pub description: String,
}

/// Tool input fields for the Bash tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BashToolInput {
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Tool input fields for the Write tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WriteToolInput {
    pub file_path: String,
    pub content: String,
}

/// Tool input fields for the WebFetch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebFetchToolInput {
    pub url: String,
    pub prompt: String,
}

/// Tool input fields for the WebSearch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebSearchToolInput {
    pub query: String,
}

/// Tool input fields for the NotebookEdit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct NotebookEditToolInput {
    pub notebook_path: String,
    pub new_source: String,
    #[serde(default)]
    pub cell_type: Option<String>,
    #[serde(default)]
    pub edit_mode: Option<String>,
}

/// Tool input fields for the Skill tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillToolInput {
    pub skill: String,
    #[serde(default)]
    pub args: Option<String>,
}

/// Tool input fields for the ExitPlanMode tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModeToolInput {
    #[serde(default, rename = "allowedPrompts")]
    pub allowed_prompts: Vec<ExitPlanModePrompt>,
}

/// A single allowed prompt within an ExitPlanMode request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModePrompt {
    pub tool: String,
    pub prompt: String,
}

/// Tool data from Claude Code permission requests.
/// Uses internally-tagged format so #[serde(other)] can ignore unknown tool_input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "tool_name")]
pub enum ClaudePermissionTool {
    Edit {
        tool_input: EditToolInput,
    },
    AskUserQuestion {
        tool_input: AskUserQuestionToolInput,
    },
    Bash {
        tool_input: BashToolInput,
    },
    Write {
        tool_input: WriteToolInput,
    },
    WebFetch {
        tool_input: WebFetchToolInput,
    },
    WebSearch {
        tool_input: WebSearchToolInput,
    },
    NotebookEdit {
        tool_input: NotebookEditToolInput,
    },
    Skill {
        tool_input: SkillToolInput,
    },
    ExitPlanMode {
        tool_input: ExitPlanModeToolInput,
    },
    #[serde(other)]
    Unknown,
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

/// Claude-specific structured output (internally tagged by "type")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClaudeStructuredOutput {
    /// User message to the agent
    UserMessage {
        content: String,
        timestamp: String,
        uuid: String,
    },
    /// Assistant response (text content)
    AssistantMessage {
        content: String,
        timestamp: String,
        uuid: String,
    },
    /// Permission request from agent
    PermissionRequest { tool: ClaudePermissionTool },
    /// Unknown entry type (forward-compatibility)
    #[serde(other)]
    Unknown,
}

/// Wrapper enum for structured output, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredOutput {
    Claude(ClaudeStructuredOutput),
}

/// Claude-specific structured input
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClaudeStructuredInput {
    /// Response to a permission request
    PermissionResponse(PermissionResponse),
    /// Submit a message (text input with trailing carriage return)
    SubmitMessage { data: Vec<u8> },
}

/// Wrapper enum for structured input, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredInput {
    Claude(ClaudeStructuredInput),
}

/// Protocol-level errors that can be returned in response messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ProtocolError {
    /// Generic server error with message
    ServerError(String),
    /// The proposed link name is already in use
    LinkNameTaken,
    /// Invalid or missing authentication credentials
    InvalidCredentials,
    /// The proposed link name is invalid (e.g., contains "." which is the route separator)
    InvalidLinkName,
    /// Protocol version mismatch between client and server
    VersionMismatch {
        server_version: u32,
        client_version: u32,
    },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::ServerError(msg) => write!(f, "{}", msg),
            ProtocolError::LinkNameTaken => write!(f, "Link name already in use"),
            ProtocolError::InvalidCredentials => write!(f, "Invalid or missing credentials"),
            ProtocolError::InvalidLinkName => {
                write!(f, "Invalid link name (must not contain '.')")
            }
            ProtocolError::VersionMismatch {
                server_version,
                client_version,
            } => {
                write!(
                    f,
                    "amux upgrade required (protocol v{}, client v{})",
                    server_version, client_version
                )
            }
        }
    }
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
        data: StructuredOutput,
    },
    StructuredInput {
        agent_id: Uuid,
        data: StructuredInput,
    },
    SubscriptionClosed {
        agent_id: Uuid,
    },
}

/// Messages that are handled directly by the receiving server (no routing).
/// Used for peer-to-peer protocol messages (Connect, Announce, Withdraw).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DirectMessage {
    Connect {
        link_name: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        token: Option<String>,
        #[serde(default)]
        version: u32,
    },
    ConnectResult {
        error: Option<ProtocolError>,
    },
    AnnounceAgent {
        agent_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        route: Route,
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
    ListAgentsResult { agents: Vec<Agent> },
    ResolveAgent { identifier: String },
    ResolveAgentResult { agent: Option<Agent> },
    Shutdown,
    ShutdownNotification(ShutdownReason),
    Debug,
    DebugResult { info: ServerDebugInfo },
    ConnectToServer { address: String },
    ConnectToServerResult { error: Option<ProtocolError> },
    HandleHook { hook: Hook },
    HandleHookResult { error: Option<ProtocolError> },
}

/// All protocol messages between client and server
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Routable {
        src: Route,
        dst: Route,
        message: RoutableMessage,
    },
    Direct(DirectMessage),
    Command(Command),
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

    #[test]
    fn test_message_roundtrip_list_agents() {
        let msg = Message::Command(Command::ListAgents);
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(decoded, Message::Command(Command::ListAgents)));
    }

    #[test]
    fn test_message_roundtrip_create_agent() {
        let test_uuid = Uuid::new_v4();
        let msg = Message::Routable {
            src: Route::from_link("term-abc"),
            dst: Route::empty(),
            message: RoutableMessage::CreateAgent(CreateAgentRequest {
                agent_id: test_uuid,
                name: Some("test".to_string()),
                agent_type: AgentType::Claude,
                working_dir: PathBuf::from("/home/user/project"),
                terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            }),
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Routable {
            message: RoutableMessage::CreateAgent(req),
            ..
        } = decoded
        {
            assert_eq!(req.agent_id, test_uuid);
            assert_eq!(req.name, Some("test".to_string()));
            assert_eq!(req.agent_type, AgentType::Claude);
            assert_eq!(req.working_dir, PathBuf::from("/home/user/project"));
            assert_eq!(req.terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[test]
    fn test_message_roundtrip_subscribe_raw_result() {
        let msg = Message::Routable {
            src: Route::from_link("host-a"),
            dst: Route::from_link("host-b"),
            message: RoutableMessage::SubscribeRawResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Routable {
            message: RoutableMessage::SubscribeRawResult { error, .. },
            ..
        } = decoded
        {
            assert!(error.is_none());
        } else {
            panic!("Expected SubscribeRawResult");
        }
    }

    #[test]
    fn test_agent_info_roundtrip() {
        let test_uuid = Uuid::new_v4();
        let info = Agent {
            id: test_uuid,
            name: Some("claude-1".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
        };
        let encoded = rmp_serde::to_vec(&info).unwrap();
        let decoded: Agent = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded.id, test_uuid);
        assert_eq!(decoded.name, Some("claude-1".to_string()));
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
        let msg = Message::Command(Command::HandleHook { hook });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command(Command::HandleHook { hook: decoded_hook }) = decoded else {
            panic!("Expected HandleHook");
        };
        match decoded_hook {
            Hook::Claude(ClaudeHook::SessionStart(session)) => {
                assert_eq!(session.session_id, test_uuid);
                assert_eq!(session.transcript_path, "/tmp/transcript.jsonl");
            }
            _ => panic!("Expected SessionStart hook"),
        }
    }

    #[test]
    fn test_message_roundtrip_announce_agent() {
        let test_uuid = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::AnnounceAgent {
            agent_id: test_uuid,
            name: Some("my-agent".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/home/user"),
            route: Route::empty(),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::AnnounceAgent {
            agent_id,
            name,
            command,
            working_dir,
            route,
        }) = decoded
        {
            assert_eq!(agent_id, test_uuid);
            assert_eq!(name, Some("my-agent".to_string()));
            assert_eq!(command, "claude");
            assert_eq!(working_dir, PathBuf::from("/home/user"));
            assert_eq!(route, Route::empty());
        } else {
            panic!("Expected AnnounceAgent");
        }
    }

    #[test]
    fn test_message_roundtrip_announce_agent_with_route() {
        let test_uuid = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::AnnounceAgent {
            agent_id: test_uuid,
            name: None,
            command: "bash".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::from_link("host-a"),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::AnnounceAgent { route, .. }) = decoded {
            let mut route = route;
            assert_eq!(route.pop(), Some("host-a".to_string()));
            assert_eq!(route.pop(), None);
        } else {
            panic!("Expected AnnounceAgent");
        }
    }

    #[test]
    fn test_message_roundtrip_withdraw_agent() {
        let test_uuid = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::WithdrawAgent {
            agent_id: test_uuid,
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::WithdrawAgent { agent_id }) = decoded {
            assert_eq!(agent_id, test_uuid);
        } else {
            panic!("Expected WithdrawAgent");
        }
    }

    #[test]
    fn test_agent_info_with_route_roundtrip() {
        let test_uuid = Uuid::new_v4();
        let info = Agent {
            id: test_uuid,
            name: Some("remote-agent".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::from_link("host-a"),
        };
        let encoded = rmp_serde::to_vec_named(&info).unwrap();
        let decoded: Agent = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded.id, test_uuid);
        assert!(decoded.is_remote());
    }

    #[test]
    fn test_agent_info_local_route_roundtrip() {
        let test_uuid = Uuid::new_v4();
        let info = Agent {
            id: test_uuid,
            name: None,
            command: "bash".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
        };
        let encoded = rmp_serde::to_vec_named(&info).unwrap();
        let decoded: Agent = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded.id, test_uuid);
        assert!(decoded.route.is_empty());
    }

    #[test]
    fn test_message_roundtrip_connect_with_version() {
        let msg = Message::Direct(DirectMessage::Connect {
            link_name: "test-link".to_string(),
            token: None,
            version: PROTOCOL_VERSION,
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::Connect {
            link_name, version, ..
        }) = decoded
        {
            assert_eq!(link_name, "test-link");
            assert_eq!(version, PROTOCOL_VERSION);
        } else {
            panic!("Expected Connect");
        }
    }

    #[test]
    fn test_connect_without_version_defaults_to_zero() {
        // Simulate old client: encode Connect without version field
        // by encoding a struct that lacks the version field, then decoding
        // with the new format. The #[serde(default)] should give version=0.
        #[derive(Serialize)]
        enum OldDirectMessage {
            Connect {
                link_name: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                token: Option<String>,
            },
        }
        #[derive(Serialize)]
        enum OldMessage {
            Direct(OldDirectMessage),
        }
        let old_msg = OldMessage::Direct(OldDirectMessage::Connect {
            link_name: "old-client".to_string(),
            token: None,
        });
        let encoded = rmp_serde::to_vec_named(&old_msg).unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::Connect {
            link_name, version, ..
        }) = decoded
        {
            assert_eq!(link_name, "old-client");
            assert_eq!(
                version, 0,
                "old client without version field should default to 0"
            );
        } else {
            panic!("Expected Connect");
        }
    }

    #[test]
    fn test_message_roundtrip_version_mismatch() {
        let msg = Message::Direct(DirectMessage::ConnectResult {
            error: Some(ProtocolError::VersionMismatch {
                server_version: 2,
                client_version: 1,
            }),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::ConnectResult {
            error:
                Some(ProtocolError::VersionMismatch {
                    server_version,
                    client_version,
                }),
        }) = decoded
        {
            assert_eq!(server_version, 2);
            assert_eq!(client_version, 1);
        } else {
            panic!("Expected ConnectResult with VersionMismatch");
        }
    }

    #[test]
    fn test_message_roundtrip_shutdown_notification() {
        // Test ProtocolMismatch reason
        let msg = Message::Command(Command::ShutdownNotification(
            ShutdownReason::ProtocolMismatch,
        ));
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command(Command::ShutdownNotification(reason)) = decoded else {
            panic!("Expected ShutdownNotification");
        };
        assert_eq!(reason, ShutdownReason::ProtocolMismatch);
        assert_eq!(reason.to_string(), "amux upgrade required");

        // Test UserRequested reason
        let msg = Message::Command(Command::ShutdownNotification(ShutdownReason::UserRequested));
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command(Command::ShutdownNotification(reason)) = decoded else {
            panic!("Expected ShutdownNotification");
        };
        assert_eq!(reason, ShutdownReason::UserRequested);
        assert_eq!(reason.to_string(), "server shutting down");
    }

    #[test]
    fn test_unknown_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "SomeNewTool",
            "tool_input": {"foo": "bar"}
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        assert!(matches!(p.tool, ClaudePermissionTool::Unknown));
    }

    #[test]
    fn test_ask_user_question_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which library should we use?",
                    "header": "Library",
                    "options": [
                        {"label": "reqwest", "description": "HTTP client"},
                        {"label": "ureq", "description": "Blocking HTTP client"}
                    ],
                    "multiSelect": false
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert_eq!(tool_input.questions.len(), 1);
        let q = &tool_input.questions[0];
        assert_eq!(q.question, "Which library should we use?");
        assert_eq!(q.header, "Library");
        assert!(!q.multi_select);
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].label, "reqwest");
        assert_eq!(q.options[1].label, "ureq");
    }

    #[test]
    fn test_ask_user_question_multi_select() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which features?",
                    "header": "Features",
                    "options": [
                        {"label": "Auth", "description": "Authentication"},
                        {"label": "Cache", "description": "Caching layer"}
                    ],
                    "multiSelect": true
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert!(tool_input.questions[0].multi_select);
    }

    #[test]
    fn test_known_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Edit",
            "tool_input": {
                "file_path": "/tmp/test.rs",
                "old_string": "foo",
                "new_string": "bar"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Edit { tool_input } = p.tool else {
            panic!("Expected Edit tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/test.rs");
        assert_eq!(tool_input.old_string, "foo");
        assert_eq!(tool_input.new_string, "bar");
        assert!(!tool_input.replace_all);
    }

    #[test]
    fn test_bash_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "cargo test",
                "description": "Run tests",
                "timeout": 30000
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "cargo test");
        assert_eq!(tool_input.description.as_deref(), Some("Run tests"));
        assert_eq!(tool_input.timeout, Some(30000));
    }

    #[test]
    fn test_bash_tool_optional_fields() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "ls");
        assert!(tool_input.description.is_none());
        assert!(tool_input.timeout.is_none());
    }

    #[test]
    fn test_write_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/output.txt",
                "content": "hello world"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Write { tool_input } = p.tool else {
            panic!("Expected Write tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/output.txt");
        assert_eq!(tool_input.content, "hello world");
    }

    #[test]
    fn test_web_search_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "WebSearch",
            "tool_input": {
                "query": "rust serde tutorial"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::WebSearch { tool_input } = p.tool else {
            panic!("Expected WebSearch tool");
        };
        assert_eq!(tool_input.query, "rust serde tutorial");
    }

    #[test]
    fn test_skill_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Skill",
            "tool_input": {
                "skill": "commit",
                "args": "-m 'Fix bug'"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Skill { tool_input } = p.tool else {
            panic!("Expected Skill tool");
        };
        assert_eq!(tool_input.skill, "commit");
        assert_eq!(tool_input.args.as_deref(), Some("-m 'Fix bug'"));
    }

    #[test]
    fn test_exit_plan_mode_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "ExitPlanMode",
            "tool_input": {
                "allowedPrompts": [
                    {"tool": "Bash", "prompt": "run tests"}
                ]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::ExitPlanMode { tool_input } = p.tool else {
            panic!("Expected ExitPlanMode tool");
        };
        assert_eq!(tool_input.allowed_prompts.len(), 1);
        assert_eq!(tool_input.allowed_prompts[0].tool, "Bash");
        assert_eq!(tool_input.allowed_prompts[0].prompt, "run tests");
    }

    #[test]
    fn test_message_roundtrip_announce_host() {
        let host_id = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::AnnounceHost {
            id: host_id,
            name: "my-laptop".to_string(),
            route: Route::empty(),
            version: "0.1.0".to_string(),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::AnnounceHost {
            id,
            name,
            route,
            version,
        }) = decoded
        {
            assert_eq!(id, host_id);
            assert_eq!(name, "my-laptop");
            assert_eq!(route, Route::empty());
            assert_eq!(version, "0.1.0");
        } else {
            panic!("Expected AnnounceHost");
        }
    }

    #[test]
    fn test_message_roundtrip_announce_host_with_route() {
        let host_id = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::AnnounceHost {
            id: host_id,
            name: "remote-server".to_string(),
            route: Route::from_link("peer-a"),
            version: "0.2.0".to_string(),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::AnnounceHost { route, .. }) = decoded {
            let mut route = route;
            assert_eq!(route.pop(), Some("peer-a".to_string()));
            assert_eq!(route.pop(), None);
        } else {
            panic!("Expected AnnounceHost");
        }
    }

    #[test]
    fn test_message_roundtrip_withdraw_host() {
        let host_id = Uuid::new_v4();
        let msg = Message::Direct(DirectMessage::WithdrawHost { id: host_id });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::Direct(DirectMessage::WithdrawHost { id }) = decoded {
            assert_eq!(id, host_id);
        } else {
            panic!("Expected WithdrawHost");
        }
    }

    #[test]
    fn test_version_mismatch_display() {
        let err = ProtocolError::VersionMismatch {
            server_version: 2,
            client_version: 1,
        };
        assert_eq!(
            err.to_string(),
            "amux upgrade required (protocol v2, client v1)"
        );
    }
}
