use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::types::{
    CompactTrigger, ConfigChangeSource, Extensions, PermissionMode, PermissionResult,
    PermissionUpdate, RawFrame, SessionStartSource, SetupTrigger,
};

// ── QueryOptions ──────────────────────────────────────────────────

pub struct QueryOptions {
    /// Model override. `None` truthfully leaves model selection to Claude Code.
    pub model: Option<String>,

    // CLI binary path (defaults to "claude" on PATH)
    pub cli_path: Option<PathBuf>,
    /// Replace the subprocess environment when set; otherwise inherit it.
    pub env: Option<HashMap<String, String>>,
    /// Extra Claude Code flags, without the leading `--`.
    pub extra_args: HashMap<String, Option<String>>,

    // Session behavior
    pub cwd: Option<PathBuf>,
    pub system_prompt: Option<SystemPrompt>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub effort: Option<Effort>,
    pub thinking: Option<ThinkingConfig>,
    pub include_partial_messages: bool,
    pub persist_session: Option<bool>,
    pub session_id: Option<String>,
    pub resume: Option<String>,
    pub fork_session: bool,
    pub prompt_suggestions: bool,
    pub agent_progress_summaries: Option<bool>,
    pub forward_subagent_text: Option<bool>,
    pub fallback_model: Option<String>,
    pub enable_file_checkpointing: bool,
    pub debug: bool,
    pub debug_file: Option<PathBuf>,
    pub output_format: Option<OutputFormat>,
    pub title: Option<String>,
    pub resume_session_at: Option<String>,
    pub resume_drops_turn: Option<String>,

    // Permissions
    pub permission_mode: Option<PermissionMode>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub supported_dialog_kinds: Vec<String>,
    pub per_task_stop_affordance: Option<bool>,
    pub allow_dangerously_skip_permissions: bool,
    pub permission_prompt_tool_name: Option<String>,

    // Tools & MCP
    pub tools: Option<ToolsConfig>,
    pub tool_config: Option<ToolConfig>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub strict_mcp_config: bool,

    // Agents
    pub agent: Option<String>,
    pub agents: HashMap<String, AgentDefinition>,
    pub tool_aliases: HashMap<String, String>,
    pub skills: Option<SkillsConfig>,

    // Settings & directories
    pub additional_directories: Vec<PathBuf>,
    pub setting_sources: Vec<SettingSource>,

    pub settings: Option<SettingsConfig>,
    pub managed_settings: Option<serde_json::Value>,

    // Sandbox
    pub sandbox: Option<SandboxSettings>,

    // Plugins
    pub plugins: Vec<SdkPluginConfig>,

    // Betas
    pub betas: Vec<SdkBeta>,

    // Hooks
    pub hook_subscriptions: Vec<HookSubscription>,
    pub include_hook_events: bool,
    pub plan_mode_instructions: Option<String>,
}

impl QueryOptions {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            cli_path: None,
            env: None,
            extra_args: HashMap::new(),
            cwd: None,
            system_prompt: None,
            max_turns: None,
            max_budget_usd: None,
            effort: None,
            thinking: None,
            include_partial_messages: false,
            persist_session: None,
            session_id: None,
            resume: None,
            fork_session: false,
            prompt_suggestions: false,
            agent_progress_summaries: None,
            forward_subagent_text: None,
            fallback_model: None,
            enable_file_checkpointing: false,
            debug: false,
            debug_file: None,
            output_format: None,
            title: None,
            resume_session_at: None,
            resume_drops_turn: None,
            permission_mode: Some(PermissionMode::Default),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            supported_dialog_kinds: Vec::new(),
            per_task_stop_affordance: None,
            allow_dangerously_skip_permissions: false,
            permission_prompt_tool_name: None,
            tools: None,
            tool_config: None,
            mcp_servers: HashMap::new(),
            strict_mcp_config: false,
            agent: None,
            agents: HashMap::new(),
            tool_aliases: HashMap::new(),
            skills: None,
            additional_directories: Vec::new(),
            setting_sources: Vec::new(),
            settings: None,
            managed_settings: None,
            sandbox: None,
            plugins: Vec::new(),
            betas: Vec::new(),
            hook_subscriptions: Vec::new(),
            include_hook_events: false,
            plan_mode_instructions: None,
        }
    }

    pub fn validate(&self) -> Result<(), crate::sdk::error::Error> {
        use crate::sdk::error::Error;

        if self.permission_mode == Some(PermissionMode::BypassPermissions)
            && !self.allow_dangerously_skip_permissions
        {
            return Err(Error::InvalidOptions(
                "permission_mode bypassPermissions requires allow_dangerously_skip_permissions"
                    .into(),
            ));
        }
        if self.fork_session && self.resume.is_none() {
            return Err(Error::InvalidOptions("fork_session requires resume".into()));
        }
        if self.session_id.is_some() && self.resume.is_some() && !self.fork_session {
            return Err(Error::InvalidOptions(
                "session_id cannot be combined with resume unless fork_session is set".into(),
            ));
        }
        if self.resume_session_at.is_some() && self.resume.is_none() {
            return Err(Error::InvalidOptions(
                "resume_session_at requires resume".into(),
            ));
        }
        if self.resume_drops_turn.is_some() && self.resume_session_at.is_none() {
            return Err(Error::InvalidOptions(
                "resume_drops_turn requires resume_session_at".into(),
            ));
        }
        for (name, config) in &self.mcp_servers {
            if let Some(server) = config.sdk_server()
                && server.configured_name() != name
            {
                return Err(Error::InvalidOptions(format!(
                    "SDK MCP server map key `{name}` must match configured name `{}`",
                    server.configured_name()
                )));
            }
        }
        for (agent_name, agent) in &self.agents {
            if let Some(mcp_servers) = &agent.mcp_servers {
                for spec in mcp_servers {
                    if let AgentMcpServerSpec::Inline(servers) = spec
                        && servers.values().any(|config| config.sdk_server().is_some())
                    {
                        return Err(Error::InvalidOptions(format!(
                            "agent `{agent_name}` cannot contain an in-process SDK MCP server"
                        )));
                    }
                }
            }
        }
        if let (Some(model), Some(fallback)) = (&self.model, &self.fallback_model)
            && model == fallback
        {
            return Err(Error::InvalidOptions(
                "fallback_model must differ from model".into(),
            ));
        }
        for (name, value) in [
            ("session_id", self.session_id.as_deref()),
            ("resume", self.resume.as_deref()),
            ("resume_session_at", self.resume_session_at.as_deref()),
            ("resume_drops_turn", self.resume_drops_turn.as_deref()),
        ] {
            if let Some(value) = value
                && uuid::Uuid::parse_str(value).is_err()
            {
                return Err(Error::InvalidOptions(format!(
                    "{name} must be a valid UUID"
                )));
            }
        }
        if self.max_turns == Some(0) {
            return Err(Error::InvalidOptions("max_turns must be positive".into()));
        }
        if self
            .max_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget < 0.0)
        {
            return Err(Error::InvalidOptions(
                "max_budget_usd must be finite and non-negative".into(),
            ));
        }
        if self.settings.as_ref().is_some_and(
            |settings| matches!(settings, SettingsConfig::Inline(value) if !value.is_object()),
        ) {
            return Err(Error::InvalidOptions(
                "inline settings must be a JSON object".into(),
            ));
        }
        if self
            .managed_settings
            .as_ref()
            .is_some_and(|settings| !settings.is_object())
        {
            return Err(Error::InvalidOptions(
                "managed_settings must be a JSON object".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum SettingsConfig {
    Inline(serde_json::Value),
    Path(PathBuf),
}

impl Default for QueryOptions {
    fn default() -> Self {
        let mut options = Self::new("");
        options.model = None;
        options
    }
}

// ── SystemPrompt ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Custom(String),
    Blocks(Vec<String>),
    Preset {
        preset: SystemPromptPreset,
        append: Option<String>,
        #[serde(default)]
        exclude_dynamic_sections: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPromptPreset {
    ClaudeCode,
}

// ── ThinkingConfig ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingConfig {
    Adaptive {
        display: Option<ThinkingDisplay>,
    },
    Enabled {
        budget_tokens: Option<u32>,
        display: Option<ThinkingDisplay>,
    },
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

// ── Effort ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

// ── ToolsConfig ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolsConfig {
    List(Vec<String>),
    Preset { preset: ToolsPreset },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsPreset {
    ClaudeCode,
}

// ── ToolConfig ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    pub ask_user_question: Option<AskUserQuestionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestionConfig {
    pub preview_format: Option<PreviewFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    Markdown,
    Html,
}

// ── OutputFormat ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputFormat {
    pub r#type: OutputFormatType,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormatType {
    JsonSchema,
}

// ── McpServerConfig ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpSseServerConfig),
    Http(McpHttpServerConfig),
    Sdk(crate::sdk::mcp::SdkMcpServer),
}

impl McpServerConfig {
    pub(crate) fn sdk_server(&self) -> Option<&crate::sdk::mcp::SdkMcpServer> {
        match self {
            Self::Sdk(server) => Some(server),
            _ => None,
        }
    }
}

impl Serialize for McpServerConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Stdio(config) => TaggedMcpServerConfigRef::Stdio(config).serialize(serializer),
            Self::Sse(config) => TaggedMcpServerConfigRef::Sse(config).serialize(serializer),
            Self::Http(config) => TaggedMcpServerConfigRef::Http(config).serialize(serializer),
            Self::Sdk(server) => serde_json::json!({
                "type": "sdk",
                "name": server.configured_name(),
            })
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match TaggedMcpServerConfig::deserialize(deserializer)? {
            TaggedMcpServerConfig::Stdio(config) => Ok(Self::Stdio(config)),
            TaggedMcpServerConfig::Sse(config) => Ok(Self::Sse(config)),
            TaggedMcpServerConfig::Http(config) => Ok(Self::Http(config)),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedMcpServerConfigRef<'a> {
    Stdio(&'a McpStdioServerConfig),
    Sse(&'a McpSseServerConfig),
    Http(&'a McpHttpServerConfig),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedMcpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpSseServerConfig),
    Http(McpHttpServerConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    pub command: String,
    /// Always serialized, even when empty.
    ///
    /// The published type makes this optional, and Claude Code accepts it
    /// missing when a server is configured at startup. The control that
    /// replaces the server set on a running session does not: it spreads the
    /// argument list without defaulting it first, and answers a config that
    /// omitted the key with `Spread syntax requires ...iterable not be null or
    /// undefined` - an error naming nothing a caller could act on. Sending the
    /// empty list means the same thing and avoids it.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSseServerConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpServerConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

// ── AgentDefinition ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub description: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disallowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<AgentMcpServerSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observer_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMcpServerSpec {
    Name(String),
    Inline(HashMap<String, McpServerConfig>),
}

// ── SdkBeta ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkBeta {
    #[serde(rename = "context-1m-2025-08-07")]
    Context1M,
}

#[derive(Debug, Clone)]
pub enum SkillsConfig {
    All,
    Selected(Vec<String>),
}

// ── SettingSource ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    User,
    Project,
    Local,
}

// ── SdkPluginConfig ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkPluginConfig {
    pub r#type: PluginType,
    pub path: PathBuf,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "skipMcpDiscovery"
    )]
    pub skip_mcp_discovery: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginType {
    Local,
}

// ── SandboxSettings ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    pub enabled: Option<bool>,
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    pub excluded_commands: Vec<String>,
    pub allow_unsandboxed_commands: Option<bool>,
    pub network: Option<SandboxNetworkConfig>,
    pub filesystem: Option<SandboxFilesystemConfig>,
    pub ignore_violations: HashMap<String, Vec<String>>,
    pub enable_weaker_nested_sandbox: Option<bool>,
    pub ripgrep: Option<RipgrepConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxNetworkConfig {
    pub allowed_domains: Vec<String>,
    pub allow_managed_domains_only: Option<bool>,
    pub allow_local_binding: Option<bool>,
    pub allow_unix_sockets: Vec<String>,
    pub allow_all_unix_sockets: Option<bool>,
    pub http_proxy_port: Option<u16>,
    pub socks_proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxFilesystemConfig {
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
    pub deny_read: Vec<String>,
}

// ── HookEvent ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    Notification,
    UserPromptSubmit,
    UserPromptExpansion,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    PermissionRequest,
    PermissionDenied,
    Setup,
    TeammateIdle,
    TaskCreated,
    TaskCompleted,
    Elicitation,
    ElicitationResult,
    ConfigChange,
    WorktreeCreate,
    WorktreeRemove,
    InstructionsLoaded,
    CwdChanged,
    FileChanged,
    DirectoryAdded,
    MessageDisplay,
}

impl HookEvent {
    pub(crate) fn wire_name(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::PostToolBatch => "PostToolBatch",
            HookEvent::Notification => "Notification",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::UserPromptExpansion => "UserPromptExpansion",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::Stop => "Stop",
            HookEvent::StopFailure => "StopFailure",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
            HookEvent::PermissionRequest => "PermissionRequest",
            HookEvent::PermissionDenied => "PermissionDenied",
            HookEvent::Setup => "Setup",
            HookEvent::TeammateIdle => "TeammateIdle",
            HookEvent::TaskCreated => "TaskCreated",
            HookEvent::TaskCompleted => "TaskCompleted",
            HookEvent::Elicitation => "Elicitation",
            HookEvent::ElicitationResult => "ElicitationResult",
            HookEvent::ConfigChange => "ConfigChange",
            HookEvent::WorktreeCreate => "WorktreeCreate",
            HookEvent::WorktreeRemove => "WorktreeRemove",
            HookEvent::InstructionsLoaded => "InstructionsLoaded",
            HookEvent::CwdChanged => "CwdChanged",
            HookEvent::FileChanged => "FileChanged",
            HookEvent::DirectoryAdded => "DirectoryAdded",
            HookEvent::MessageDisplay => "MessageDisplay",
        }
    }
}

/// A hook matcher Claude Code should forward as an [`SdkEvent`](crate::sdk::SdkEvent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSubscription {
    pub event: HookEvent,
    pub matcher: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookCallbackContext {
    pub request_id: String,
    pub tool_use_id: Option<String>,
    pub extensions: Extensions,
}

// ── HookInput ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub prompt_id: Option<String>,
    pub permission_mode: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub effort: Option<HookEffort>,
    pub event: HookEventData,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEffort {
    pub level: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookEventData {
    PreToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
    },
    PostToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_response: serde_json::Value,
        tool_use_id: String,
    },
    PostToolUseFailure {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
        error: String,
        is_interrupt: Option<bool>,
    },
    PostToolBatch {
        tool_calls: Vec<PostToolBatchToolCall>,
    },
    Notification {
        message: String,
        title: Option<String>,
        notification_type: String,
    },
    UserPromptSubmit {
        prompt: String,
    },
    UserPromptExpansion {
        expansion_type: String,
        command_name: String,
        command_args: String,
        command_source: Option<String>,
        prompt: String,
    },
    SessionStart {
        source: SessionStartSource,
        model: Option<String>,
    },
    SessionEnd {
        reason: String,
    },
    Stop {
        stop_hook_active: bool,
        last_assistant_message: Option<String>,
    },
    StopFailure {
        error: String,
        error_details: Option<String>,
        last_assistant_message: Option<String>,
    },
    SubagentStart {
        agent_id: String,
        agent_type: String,
    },
    SubagentStop {
        stop_hook_active: bool,
        agent_id: String,
        agent_transcript_path: String,
        agent_type: String,
        last_assistant_message: Option<String>,
    },
    PreCompact {
        trigger: CompactTrigger,
        custom_instructions: Option<String>,
    },
    PostCompact {
        trigger: CompactTrigger,
        compact_summary: String,
    },
    PermissionRequest {
        tool_name: String,
        tool_input: serde_json::Value,
        permission_suggestions: Option<Vec<PermissionUpdate>>,
    },
    PermissionDenied {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
        reason: String,
    },
    Setup {
        trigger: SetupTrigger,
    },
    TeammateIdle {
        teammate_name: String,
        team_name: String,
    },
    TaskCreated {
        task_id: String,
        task_subject: String,
        task_description: Option<String>,
        teammate_name: Option<String>,
        team_name: Option<String>,
    },
    TaskCompleted {
        task_id: String,
        task_subject: String,
        task_description: Option<String>,
        teammate_name: Option<String>,
        team_name: Option<String>,
    },
    Elicitation {
        mcp_server_name: String,
        message: String,
        mode: Option<ElicitationMode>,
        url: Option<String>,
        elicitation_id: Option<String>,
        requested_schema: Option<serde_json::Value>,
    },
    ElicitationResult {
        mcp_server_name: String,
        elicitation_id: Option<String>,
        mode: Option<ElicitationMode>,
        action: String,
        content: Option<serde_json::Value>,
    },
    ConfigChange {
        source: ConfigChangeSource,
        file_path: Option<String>,
    },
    WorktreeCreate {
        name: String,
    },
    WorktreeRemove {
        worktree_path: String,
    },
    InstructionsLoaded {
        file_path: String,
        memory_type: String,
        load_reason: String,
        globs: Option<Vec<String>>,
        trigger_file_path: Option<String>,
        parent_file_path: Option<String>,
    },
    CwdChanged {
        old_cwd: String,
        new_cwd: String,
    },
    FileChanged {
        file_path: String,
        event: String,
    },
    DirectoryAdded {
        directory: String,
        source: String,
    },
    MessageDisplay {
        turn_id: String,
        message_id: String,
        index: u64,
        final_delta: bool,
        delta: String,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolBatchToolCall {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── HookOutput ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HookOutput {
    Async {
        timeout: Option<std::time::Duration>,
    },
    Sync(SyncHookOutput),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncHookOutput {
    pub r#continue: Option<bool>,
    pub suppress_output: Option<bool>,
    pub stop_reason: Option<String>,
    pub decision: Option<HookDecision>,
    pub system_message: Option<String>,
    pub reason: Option<String>,
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    Approve,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookSpecificOutput {
    PreToolUse {
        permission_decision: Option<HookPermissionDecision>,
        permission_decision_reason: Option<String>,
        updated_input: Option<serde_json::Value>,
        additional_context: Option<String>,
    },
    UserPromptSubmit {
        additional_context: Option<String>,
    },
    UserPromptExpansion {
        additional_context: Option<String>,
        suppress_original_prompt: Option<bool>,
    },
    SessionStart {
        additional_context: Option<String>,
    },
    Setup {
        additional_context: Option<String>,
    },
    SubagentStart {
        additional_context: Option<String>,
    },
    Stop {
        additional_context: Option<String>,
    },
    SubagentStop {
        additional_context: Option<String>,
    },
    PostToolUse {
        additional_context: Option<String>,
        updated_mcp_tool_output: Option<serde_json::Value>,
    },
    PostToolUseFailure {
        additional_context: Option<String>,
    },
    PostToolBatch {
        additional_context: Option<String>,
    },
    Notification {
        additional_context: Option<String>,
    },
    PermissionRequest {
        decision: PermissionResult,
    },
    PermissionDenied {
        retry: Option<bool>,
    },
    Elicitation {
        action: Option<String>,
        content: Option<serde_json::Value>,
    },
    ElicitationResult {
        action: Option<String>,
        content: Option<serde_json::Value>,
    },
    CwdChanged {
        watch_paths: Option<Vec<String>>,
    },
    FileChanged {
        watch_paths: Option<Vec<String>>,
    },
    WorktreeCreate {
        worktree_path: String,
    },
    MessageDisplay {
        display_content: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPermissionDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationRequest {
    pub server_name: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ElicitationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationMode {
    Form,
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ElicitationResult {
    Accept {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Decline {
        #[serde(flatten)]
        extensions: Extensions,
    },
    Cancel {
        #[serde(flatten)]
        extensions: Extensions,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDialogRequest {
    pub dialog_kind: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum UserDialogResult {
    Completed {
        result: serde_json::Value,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Cancelled {
        #[serde(flatten)]
        extensions: Extensions,
    },
}
