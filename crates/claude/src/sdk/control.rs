use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::sdk::init::{AgentInfo, SlashCommand};
use crate::sdk::options::{AgentDefinition, McpServerConfig};
use crate::sdk::types::{Extensions, PermissionMode};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ControlRequest<T> {
    pub r#type: &'static str,
    pub request_id: String,
    pub request: T,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitializeRequestBody {
    pub subtype: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_mcp_servers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<BTreeMap<String, Vec<HookMatcherConfig>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_mode_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_aliases: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_dynamic_sections: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<BTreeMap<String, AgentDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_suggestions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_progress_summaries: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_subagent_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_dialog_kinds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_task_stop_affordance: Option<bool>,
}

impl Default for InitializeRequestBody {
    fn default() -> Self {
        Self {
            subtype: "initialize",
            sdk_mcp_servers: None,
            hooks: None,
            json_schema: None,
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            tool_aliases: None,
            exclude_dynamic_sections: None,
            agents: None,
            title: None,
            skills: None,
            prompt_suggestions: None,
            agent_progress_summaries: None,
            forward_subagent_text: None,
            supported_dialog_kinds: None,
            per_task_stop_affordance: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookMatcherConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub hook_callback_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(crate) enum ControlRequestBody {
    Interrupt {
        #[serde(skip_serializing_if = "Option::is_none")]
        cancel_queued: Option<bool>,
    },
    SetPermissionMode {
        mode: PermissionMode,
    },
    SetMcpPermissionModeOverride {
        #[serde(rename = "serverName")]
        server_name: String,
        mode: Option<McpPermissionMode>,
    },
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    ApplyFlagSettings {
        settings: serde_json::Value,
    },
    McpStatus,
    GetContextUsage,
    ReloadPlugins,
    ReloadSkills,
    RewindFiles {
        user_message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        dry_run: Option<bool>,
    },
    SeedReadState {
        path: String,
        mtime: u64,
    },
    McpReconnect {
        #[serde(rename = "serverName")]
        server_name: String,
    },
    McpToggle {
        #[serde(rename = "serverName")]
        server_name: String,
        enabled: bool,
    },
    McpSetServers {
        servers: HashMap<String, McpServerConfig>,
    },
    StopTask {
        task_id: String,
    },
    BackgroundTasks {
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpPermissionMode {
    Default,
    Auto,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ControlResponseEnvelope {
    pub response: ControlResponseInner,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ControlResponseInner {
    pub subtype: String,
    pub request_id: String,
    #[serde(default)]
    pub response: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptResult {
    pub still_queued: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<Vec<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPermissionModeOverrideResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSetServersResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub errors: HashMap<String, String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindFilesResult {
    pub can_rewind: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insertions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_links: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadPluginsResult {
    pub commands: Vec<SlashCommand>,
    pub agents: Vec<AgentInfo>,
    pub plugins: Vec<PluginInfo>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<McpServerStatus>,
    pub error_count: u64,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadSkillsResult {
    pub skills: Vec<SlashCommand>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskSummary {
    pub id: String,
    pub r#type: String,
    pub status: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}
