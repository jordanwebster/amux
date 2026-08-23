use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::agent_tools::AgentToolRequest;

/// Environment variables forwarded from an agent's hook invocation.
pub(crate) type HookEnvironment = HashMap<String, String>;

/// The provider-specific launch policy a same-kind child inherits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpawnInheritance {
    pub(crate) claude_permission_args: Vec<String>,
    pub(crate) codex_approval_policy: Option<String>,
    pub(crate) codex_sandbox_policy: Option<String>,
}

/// In-process route from a model-facing tool call to ClientService.
///
/// Defined beside the other agent vocabulary rather than with the session
/// runtime because the seam a client-only build holds — an absent local agent
/// host — still has to name it.
#[async_trait]
pub(crate) trait AgentToolExecutor: Send + Sync {
    async fn execute(&self, caller: Uuid, request: AgentToolRequest) -> Result<Value>;
}

pub(crate) const AGENT_TYPE_CLAUDE: &str = "claude";
#[cfg(all(feature = "local-agents", unix))]
pub(crate) const AGENT_TYPE_CODEX: &str = "codex";

#[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
pub(crate) const AGENT_TYPE_TEST_AGENT: &str = "test-agent";

/// Type of agent to spawn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentType {
    /// Claude Code agent.
    Claude,
    /// Codex agent backed by an app-server thread.
    Codex {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_policy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_policy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_thread_id: Option<String>,
    },
    /// Test agent for E2E tests.
    #[cfg(all(feature = "local-agents", any(debug_assertions, test)))]
    TestAgent { command: String },
}

/// Terminal dimensions for PTY creation and resizing.
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

/// Request to create a new agent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreateAgentRequest {
    pub agent_id: Uuid,
    #[serde(default)]
    pub host_id: Option<Uuid>,
    pub name: Option<String>,
    pub agent_type: AgentType,
    pub working_dir: PathBuf,
    /// Terminal dimensions. None means use defaults.
    #[serde(default)]
    pub terminal_size: Option<TerminalSize>,
    /// Extra arguments passed to the agent command.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
}

/// Request to rename an existing agent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RenameAgentRequest {
    pub agent_id: Uuid,
    pub name: String,
}

/// The owning agent and host for a child agent.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentParent {
    pub agent_id: Uuid,
    pub host_id: Uuid,
}

/// A concise description of an agent's current task and when it changed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WorkingOn {
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

/// Client-visible agent DTO used by service responses and inventory streams.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub host_id: Uuid,
    pub name: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub agent_type: String,
    #[serde(default)]
    pub io_protocols: Vec<String>,
    pub readonly: bool,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_on: Option<WorkingOn>,
}
