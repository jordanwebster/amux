use crate::structured_log::StructuredLog;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Hook {
    Claude(ClaudeHook),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClaudeHook {
    SessionStart { transcript_path: String },
}

/// All protocol messages between client and server
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    // Client -> Server
    /// List all running agents
    ListAgents,

    /// Create a new agent with the given command
    CreateAgent {
        agent_id: String,
        command: String,
        working_dir: PathBuf,
        rows: u16,
        cols: u16,
    },

    /// Subscribe to an agent's output stream (routable)
    Subscribe {
        src_host: String,
        dst_host: String,
        agent_id: String,
        rows: u16,
        cols: u16,
    },

    /// Unsubscribe from the current agent
    Unsubscribe,

    /// Send input bytes to the subscribed agent (routable)
    Input {
        src_host: String,
        dst_host: String,
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
        error: Option<String>,
    },

    /// Response to Subscribe (routable)
    SubscribeResult {
        src_host: String,
        dst_host: String,
        agent_id: String,
        success: bool,
        error: Option<String>,
    },

    /// Output bytes from the agent (routable)
    Output {
        src_host: String,
        dst_host: String,
        agent_id: String,
        data: Vec<u8>,
    },

    /// Agent session has ended
    AgentEnded,

    /// Generic error response
    Error { code: u32, message: String },

    // Client -> Server: remote connection management
    /// Request local server to connect to a remote amux server
    ConnectToServer { address: String },

    /// Response to ConnectToServer
    ConnectToServerResult {
        success: bool,
        error: Option<String>,
    },

    // Handshake (unified for client-server and server-server)
    /// Sent to initiate connection handshake
    Connect { host_id: String },

    /// Response to Connect
    ConnectResponse {
        success: bool,
        error: Option<String>,
        host_id: String,
    },

    // Hook events
    /// Hook event from CLI hook handler (e.g., Claude Code SessionStart)
    HookEvent { hook: Hook },

    /// Acknowledgement of HookEvent
    HookEventResult {
        success: bool,
        error: Option<String>,
    },

    // Structured output for WebSocket clients
    /// Structured log entry from agent (for WebSocket subscribers)
    StructuredOutput {
        src_host: String,
        dst_host: String,
        agent_id: String,
        entry: StructuredLog,
    },
}

/// Information about a running agent
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub agent_id: String,
    pub command: String,
    pub working_dir: PathBuf,
}

impl Message {
    /// Encode message to bytes using bincode
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Decode message from bytes using bincode
    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
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
        let msg = Message::CreateAgent {
            agent_id: "test".to_string(),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/home/user/project"),
            rows: 24,
            cols: 80,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::CreateAgent {
            agent_id,
            command,
            working_dir,
            rows,
            cols,
        } = decoded
        {
            assert_eq!(agent_id, "test");
            assert_eq!(command, "claude");
            assert_eq!(working_dir, PathBuf::from("/home/user/project"));
            assert_eq!(rows, 24);
            assert_eq!(cols, 80);
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[test]
    fn test_message_roundtrip_subscribe_result() {
        let msg = Message::SubscribeResult {
            src_host: "host-a".to_string(),
            dst_host: "host-b".to_string(),
            agent_id: "test".to_string(),
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
        let info = AgentInfo {
            agent_id: "claude-1".to_string(),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };
        let encoded = bincode::serialize(&info).unwrap();
        let decoded: AgentInfo = bincode::deserialize(&encoded).unwrap();
        assert_eq!(decoded.agent_id, "claude-1");
        assert_eq!(decoded.command, "claude");
        assert_eq!(decoded.working_dir, PathBuf::from("/tmp"));
    }

    #[test]
    fn test_message_roundtrip_hook_event() {
        let hook = Hook::Claude(ClaudeHook::SessionStart {
            transcript_path: "/tmp/transcript.jsonl".to_string(),
        });
        let msg = Message::HookEvent { hook };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::HookEvent { hook: decoded_hook } = decoded else {
            panic!("Expected HookEvent");
        };
        let Hook::Claude(ClaudeHook::SessionStart { transcript_path }) = decoded_hook;
        assert_eq!(transcript_path, "/tmp/transcript.jsonl");
    }
}
