use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::stream::types::Extensions;

/// Complete response to the stream-JSON `initialize` control request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializationResult {
    pub commands: Vec<SlashCommand>,
    pub agents: Vec<AgentInfo>,
    pub output_style: String,
    pub available_output_styles: Vec<String>,
    pub models: Vec<ModelInfo>,
    pub account: AccountInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode_state: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode_disabled_reason: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_effort_levels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_adaptive_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_fast_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_auto_mode: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_provider: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageCategory {
    pub name: String,
    pub tokens: u64,
    #[serde(default = "crate::absent::color")]
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_deferred: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// What `get_settings` says the session runs with now: the model and
/// effort Claude applied from every settings source and the session's own
/// changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedSettings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Lossless typed top-level response from `Query::get_context_usage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub categories: Vec<ContextUsageCategory>,
    pub total_tokens: u64,
    pub max_tokens: u64,
    #[serde(default = "crate::absent::raw_max_tokens")]
    pub raw_max_tokens: u64,
    /// As written: Claude writes a whole percentage without a fraction.
    #[serde(default = "crate::absent::percentage")]
    pub percentage: serde_json::Number,
    #[serde(default = "crate::absent::grid_rows")]
    pub grid_rows: Vec<Vec<serde_json::Value>>,
    #[serde(default = "crate::absent::model")]
    pub model: String,
    #[serde(default = "crate::absent::memory_files")]
    pub memory_files: Vec<serde_json::Value>,
    #[serde(default = "crate::absent::mcp_tools")]
    pub mcp_tools: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_builtin_tools: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_tools: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_sections: Option<Vec<serde_json::Value>>,
    #[serde(default = "crate::absent::agents")]
    pub agents: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    #[serde(default = "crate::absent::is_auto_compact_enabled")]
    pub is_auto_compact_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_breakdown: Option<serde_json::Value>,
    #[serde(default = "crate::absent::api_usage")]
    pub api_usage: Option<HashMap<String, u64>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}
