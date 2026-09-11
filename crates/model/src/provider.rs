//! Concrete provider payload values shared without provider runtime dependencies.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CLAUDE_PTY_TRANSCRIPT_V1: &str = "claude_pty_transcript_v1";
pub const CLAUDE_SDK_V1: &str = "claude_sdk_v1";
pub const CODEX_SDK_V1: &str = "codex_sdk_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePtyTranscriptV1Args {
    pub terminal_size: Option<crate::TerminalSize>,
    pub replay_query: Option<ClaudePtyTranscriptV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudePtyTranscriptV1ReplayQuery {
    Since { seq_id: u64 },
    Tail { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudePtyTranscriptV1Output {
    pub seq_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClaudePtyIntent {
    Prompt { text: String },
    Interrupt,
    CyclePermissionMode,
    Answer { ask_id: String, answer: AskAnswer },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeSdkInput {
    Prompt { text: String },
    Interrupt,
    SetPermissionMode { mode: String },
    SetModel { model: Option<String> },
    RequestContextBreakdown,
    PermissionDecision { request_id: String, decision: Value },
    ElicitationDecision { request_id: String, result: Value },
    DialogDecision { request_id: String, result: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeSdkV1Args {
    pub replay_query: Option<ClaudeSdkV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeSdkV1ReplayQuery {
    Since { seq_id: u64 },
    Tail { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSdkV1Output {
    pub seq_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "codex_input", rename_all = "snake_case")]
pub enum CodexSdkInput {
    UserTurn {
        input: Vec<u8>,
    },
    Steer {
        turn_id: String,
        input: Vec<u8>,
    },
    Interrupt {
        turn_id: String,
    },
    ApprovalDecision {
        request_id: Vec<u8>,
        decision: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexSdkV1Args {
    pub replay_query: Option<CodexSdkV1ReplayQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSdkV1ReplayQuery {
    Since { seq: u64 },
    Tail { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSdkV1Output {
    pub seq: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum AskAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question(QuestionResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionAnswer {
    AllowOnce,
    AllowScoped { suggestion: usize },
    Deny { feedback: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanAnswer {
    ApproveAuto,
    ApproveManual,
    RequestChanges { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionResponse {
    pub answers: Vec<QuestionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub selected: Vec<usize>,
    pub other: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMeter {
    pub used_tokens: u64,
    pub window_tokens: Option<u64>,
    pub source: ContextMeterSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMeterSource {
    AssistantUsage,
    ResultUsage,
    AssistantContextUsage,
    CompactBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerFact {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageCategory {
    pub name: String,
    pub tokens: u64,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_deferred: Option<bool>,
    #[serde(flatten)]
    pub extensions: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub categories: Vec<ContextUsageCategory>,
    pub total_tokens: u64,
    pub max_tokens: u64,
    pub raw_max_tokens: u64,
    pub percentage: f64,
    pub grid_rows: Vec<Vec<Value>>,
    pub model: String,
    pub memory_files: Vec<Value>,
    pub mcp_tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_builtin_tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_sections: Option<Vec<Value>>,
    pub agents: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    pub is_auto_compact_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_breakdown: Option<Value>,
    pub api_usage: Option<HashMap<String, u64>>,
    #[serde(flatten)]
    pub extensions: HashMap<String, Value>,
}

pub const AGENT_TOOL_SERVER_NAME: &str = "amux";
pub const AGENT_TOOL_NAMES: &[&str] = &["agents", "send", "spawn", "stop", "status", "attach"];
