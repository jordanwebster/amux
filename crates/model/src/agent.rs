use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentKind, ClaudeDriver};

/// Environment variables forwarded from an agent's hook invocation.
pub type HookEnvironment = HashMap<String, String>;

/// The provider-specific launch policy a same-kind child inherits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnInheritance {
    pub claude_permission_args: Vec<String>,
    pub codex_approval_policy: Option<String>,
    pub codex_sandbox_policy: Option<String>,
}

pub const AGENT_TYPE_CLAUDE: &str = "claude";

pub const AGENT_TYPE_CODEX: &str = "codex";

pub const AGENT_TYPE_TEST_AGENT: &str = "test-agent";

/// Type of agent to spawn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentType {
    /// Claude Code agent.
    Claude { driver: ClaudeDriver },
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub host_id: Uuid,
    pub name: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub kind: AgentKind,
    pub readonly: bool,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub parent: Option<AgentParent>,
    pub working_on: Option<WorkingOn>,
    /// Durable host-local ordering for authoritative inventory changes.
    pub inventory_revision: u64,
}

#[derive(Serialize, Deserialize)]
struct HumanAgent {
    id: Uuid,
    host_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    kind: AgentKind,
    readonly: bool,
    args: Vec<String>,
    created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<AgentParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_on: Option<WorkingOn>,
    #[serde(default, skip_serializing_if = "is_zero")]
    inventory_revision: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Serialize, Deserialize)]
struct BinaryAgent {
    id: Uuid,
    host_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    kind: AgentKind,
    readonly: bool,
    args: Vec<String>,
    created_at: DateTime<Utc>,
    parent: Option<AgentParent>,
    working_on: Option<WorkingOn>,
    inventory_revision: u64,
}

macro_rules! impl_agent_conversion {
    ($representation:ty) => {
        impl From<Agent> for $representation {
            fn from(value: Agent) -> Self {
                Self {
                    id: value.id,
                    host_id: value.host_id,
                    name: value.name,
                    command: value.command,
                    working_dir: value.working_dir,
                    kind: value.kind,
                    readonly: value.readonly,
                    args: value.args,
                    created_at: value.created_at,
                    parent: value.parent,
                    working_on: value.working_on,
                    inventory_revision: value.inventory_revision,
                }
            }
        }

        impl From<$representation> for Agent {
            fn from(value: $representation) -> Self {
                Self {
                    id: value.id,
                    host_id: value.host_id,
                    name: value.name,
                    command: value.command,
                    working_dir: value.working_dir,
                    kind: value.kind,
                    readonly: value.readonly,
                    args: value.args,
                    created_at: value.created_at,
                    parent: value.parent,
                    working_on: value.working_on,
                    inventory_revision: value.inventory_revision,
                }
            }
        }
    };
}

impl_agent_conversion!(HumanAgent);
impl_agent_conversion!(BinaryAgent);

impl Serialize for Agent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            HumanAgent::from(self.clone()).serialize(serializer)
        } else {
            BinaryAgent::from(self.clone()).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Agent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            return HumanAgent::deserialize(deserializer).map(Into::into);
        }
        BinaryAgent::deserialize(deserializer).map(Into::into)
    }
}
