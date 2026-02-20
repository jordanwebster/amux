use serde::{Deserialize, Serialize};

/// Structured log entry for rich clients (WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum StructuredLog {
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
    PermissionRequest { tool: PermissionTool },
}

/// The specific tool being requested permission for
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PermissionTool {
    Edit {
        file_path: String,
        old_string: String,
        new_string: String,
    },
    Unknown,
}
