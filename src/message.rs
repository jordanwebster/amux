use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    /// Subscribe to an agent's output stream
    Subscribe {
        agent_id: String,
        rows: u16,
        cols: u16,
    },

    /// Unsubscribe from the current agent
    Unsubscribe,

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

    /// Response to Subscribe
    SubscribeResult {
        success: bool,
        error: Option<String>,
    },

    /// Agent session has ended
    AgentEnded,

    /// Generic error response
    Error { code: u32, message: String },
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
            success: true,
            error: None,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        if let Message::SubscribeResult { success, error } = decoded {
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
}
