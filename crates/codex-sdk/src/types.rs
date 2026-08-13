use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::approval::RequestId;
use crate::config::{ApprovalPolicy, ApprovalsReviewer, InputItem, ReasoningEffort, SandboxPolicy};

// ── Thread ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInfo {
    #[serde(alias = "threadId")]
    pub id: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default)]
    pub model_provider: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub status: ThreadStatus,
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default)]
    pub cli_version: String,
    #[serde(default)]
    pub source: SessionSource,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub git_info: Option<GitInfo>,
    pub name: Option<String>,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatus {
    #[default]
    NotLoaded,
    Idle,
    SystemError,
    Active {
        #[serde(alias = "activeFlags")]
        #[serde(default)]
        active_flags: Vec<ThreadActiveFlag>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadActiveFlag {
    WaitingOnApproval,
    WaitingOnUserInput,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionSource {
    Named(NamedSessionSource),
    SubAgent {
        #[serde(rename = "subAgent")]
        sub_agent: SubAgentSource,
    },
}

impl Default for SessionSource {
    fn default() -> Self {
        Self::Named(NamedSessionSource::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NamedSessionSource {
    Cli,
    Vscode,
    Exec,
    AppServer,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubAgentSource {
    Named(NamedSubAgentSource),
    ThreadSpawn {
        #[serde(rename = "thread_spawn")]
        thread_spawn: ThreadSpawnSource,
    },
    Other {
        other: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedSubAgentSource {
    Review,
    Compact,
    MemoryConsolidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSpawnSource {
    pub parent_thread_id: String,
    pub depth: u32,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub origin_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSessionInfo {
    pub thread: ThreadInfo,
    pub model: String,
    pub model_provider: String,
    pub service_tier: Option<ServiceTier>,
    pub cwd: PathBuf,
    pub approval_policy: ApprovalPolicy,
    pub approvals_reviewer: ApprovalsReviewer,
    pub sandbox: SandboxPolicy,
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    Fast,
    Flex,
}

// ── Turn ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    #[serde(default)]
    pub items: Vec<ThreadItem>,
    #[serde(default)]
    pub status: TurnStatus,
    pub error: Option<TurnError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    #[serde(rename = "inProgress")]
    #[default]
    Active,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    pub codex_error_info: Option<serde_json::Value>,
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartResponse {
    pub turn: Turn,
    pub review_thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResponse {
    pub turn_id: String,
}

// ── ThreadItem ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename = "userMessage")]
    UserMessage {
        id: String,
        #[serde(default)]
        content: Vec<InputItem>,
    },
    #[serde(rename = "agentMessage")]
    AgentMessage {
        id: String,
        #[serde(default)]
        text: String,
        phase: Option<MessagePhase>,
    },
    #[serde(rename = "plan")]
    Plan {
        id: String,
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default)]
        content: Vec<String>,
    },
    #[serde(rename = "commandExecution")]
    CommandExecution {
        id: String,
        #[serde(default)]
        command: String,
        cwd: Option<PathBuf>,
        #[serde(alias = "processId")]
        process_id: Option<String>,
        #[serde(default)]
        status: CommandExecutionStatus,
        #[serde(alias = "commandActions", default)]
        command_actions: Vec<CommandAction>,
        #[serde(alias = "aggregatedOutput")]
        aggregated_output: Option<String>,
        #[serde(alias = "exitCode")]
        exit_code: Option<i32>,
        #[serde(alias = "durationMs")]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "fileChange")]
    FileChange {
        id: String,
        #[serde(default)]
        changes: Vec<FileChangeInfo>,
        #[serde(default)]
        status: PatchApplyStatus,
    },
    #[serde(rename = "mcpToolCall")]
    McpToolCall {
        id: String,
        #[serde(default)]
        server: String,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        status: McpToolCallStatus,
        #[serde(default)]
        arguments: serde_json::Value,
        result: Option<serde_json::Value>,
        error: Option<serde_json::Value>,
        #[serde(alias = "durationMs")]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "dynamicToolCall")]
    DynamicToolCall {
        id: String,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        status: DynamicToolCallStatus,
        #[serde(alias = "contentItems")]
        content_items: Option<Vec<serde_json::Value>>,
        success: Option<bool>,
        #[serde(alias = "durationMs")]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "collabAgentToolCall")]
    CollabAgentToolCall {
        id: String,
        #[serde(default)]
        tool: serde_json::Value,
        #[serde(default)]
        status: CollabAgentToolCallStatus,
        #[serde(alias = "senderThreadId")]
        sender_thread_id: String,
        #[serde(alias = "receiverThreadIds", default)]
        receiver_thread_ids: Vec<String>,
        prompt: Option<String>,
        model: Option<String>,
        #[serde(alias = "reasoningEffort")]
        reasoning_effort: Option<ReasoningEffort>,
        #[serde(alias = "agentsStates")]
        agents_states: Option<serde_json::Value>,
    },
    #[serde(rename = "webSearch")]
    WebSearch {
        id: String,
        #[serde(default)]
        query: String,
        action: Option<serde_json::Value>,
    },
    #[serde(rename = "imageView")]
    ImageView { id: String, path: PathBuf },
    #[serde(rename = "imageGeneration")]
    ImageGeneration {
        id: String,
        #[serde(default)]
        status: String,
        #[serde(alias = "revisedPrompt")]
        revised_prompt: Option<String>,
        #[serde(default)]
        result: String,
    },
    #[serde(rename = "enteredReviewMode")]
    EnteredReviewMode {
        id: String,
        #[serde(default)]
        review: String,
    },
    #[serde(rename = "exitedReviewMode")]
    ExitedReviewMode {
        id: String,
        #[serde(default)]
        review: String,
    },
    #[serde(rename = "contextCompaction")]
    ContextCompaction { id: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CommandAction {
    Read {
        command: String,
        name: String,
        path: String,
    },
    ListFiles {
        command: String,
        path: Option<String>,
    },
    Search {
        command: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        command: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandExecutionStatus {
    #[serde(rename = "inProgress")]
    #[default]
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpToolCallStatus {
    #[serde(rename = "inProgress")]
    #[default]
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DynamicToolCallStatus {
    #[serde(rename = "inProgress")]
    #[default]
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollabAgentToolCallStatus {
    #[serde(rename = "inProgress")]
    #[default]
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchApplyStatus {
    #[serde(rename = "inProgress")]
    #[default]
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeInfo {
    pub path: String,
    #[serde(default, deserialize_with = "deserialize_kind")]
    pub kind: Option<PatchChangeKind>,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchChangeKind {
    Add,
    Delete,
    Modify,
}

fn deserialize_kind<'de, D>(deserializer: D) -> Result<Option<PatchChangeKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = Option::<serde_json::Value>::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => parse_patch_change_kind(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(serde_json::Value::Object(obj)) => obj
            .get("type")
            .and_then(|v| v.as_str())
            .map(|value| {
                parse_patch_change_kind(value)
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            })
            .unwrap_or(Ok(None)),
        Some(other) => Err(serde::de::Error::custom(format!(
            "unsupported patch change kind: {other}"
        ))),
    }
}

fn parse_patch_change_kind(value: &str) -> Result<PatchChangeKind, String> {
    match value {
        "add" | "create" => Ok(PatchChangeKind::Add),
        "delete" => Ok(PatchChangeKind::Delete),
        "modify" | "update" => Ok(PatchChangeKind::Modify),
        other => Err(format!("unknown patch change kind: {other}")),
    }
}

// ── Token usage ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    pub total: TokenUsageBreakdown,
    pub last: TokenUsageBreakdown,
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
}

// ── Model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub upgrade: Option<String>,
    pub upgrade_info: Option<ModelUpgradeInfo>,
    pub availability_nux: Option<ModelAvailabilityNux>,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub input_modalities: Vec<InputModality>,
    #[serde(default)]
    pub supports_personality: bool,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUpgradeInfo {
    pub model: String,
    pub upgrade_copy: Option<String>,
    pub model_link: Option<String>,
    pub migration_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAvailabilityNux {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: ReasoningEffort,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputModality {
    Text,
    Image,
}

// ── Model list response ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    #[serde(default)]
    pub data: Vec<Model>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub include_hidden: Option<bool>,
}

// ── List/Read responses ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    #[serde(default)]
    pub data: Vec<ThreadInfo>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResponse {
    pub thread: ThreadInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreadsParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub sort_key: Option<String>,
    pub model_providers: Option<Vec<String>>,
    pub source_kinds: Option<Vec<String>>,
    pub archived: Option<bool>,
    pub cwd: Option<String>,
    pub search_term: Option<String>,
    #[serde(skip)]
    pub status: Option<String>,
}

// ── Exec command ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandParams {
    pub command: Vec<String>,
    pub process_id: Option<String>,
    pub tty: Option<bool>,
    pub stream_stdin: Option<bool>,
    pub stream_stdout_stderr: Option<bool>,
    pub output_bytes_cap: Option<u64>,
    pub disable_output_cap: Option<bool>,
    pub disable_timeout: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub cwd: Option<PathBuf>,
    pub env: Option<std::collections::HashMap<String, Option<String>>>,
    pub size: Option<CommandExecTerminalSize>,
    pub sandbox_policy: Option<SandboxPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecTerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandWriteParams {
    pub process_id: String,
    pub delta_base64: Option<String>,
    pub close_stdin: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandResizeParams {
    pub process_id: String,
    pub size: CommandExecTerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandTerminateParams {
    pub process_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandResult {
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecOutputDelta {
    pub process_id: String,
    pub stream: OutputStream,
    pub delta_base64: String,
    #[serde(default)]
    pub cap_reached: bool,
}

// ── Review ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ReviewTarget {
    UncommittedChanges,
    BaseBranch { branch: String },
    Commit { sha: String, title: Option<String> },
    Custom { instructions: String },
    // Legacy compatibility shims.
    LastTurn,
    Turn(String),
}

// ── Hook info ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInfo {
    pub id: String,
    pub event_name: String,
    pub handler_type: String,
    pub execution_mode: String,
    pub scope: String,
    pub source_path: PathBuf,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub display_order: i64,
    #[serde(default)]
    pub entries: Vec<HookOutputEntry>,
    #[serde(default)]
    pub status: String,
    pub status_message: Option<String>,
    #[serde(default = "unknown_hook_source")]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutputEntry {
    pub kind: String,
    pub text: String,
}

fn unknown_hook_source() -> String {
    "unknown".to_owned()
}

// ── Output stream (for command output deltas) ─────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

// ── Plan step ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    #[serde(alias = "text")]
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStepStatus {
    Pending,
    #[serde(rename = "inProgress")]
    InProgress,
    Completed,
}

impl PlanStep {
    pub fn is_completed(&self) -> bool {
        self.status == PlanStepStatus::Completed
    }
}

// ── Question (for user-input approvals) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    pub options: Option<Vec<QuestionOption>>,
    #[serde(skip)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

// ── Account ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_layers: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResponse {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Account {
    ApiKey,
    Chatgpt {
        email: Option<String>,
        plan_type: PlanType,
    },
    AmazonBedrock {
        #[serde(default)]
        uses_codex_managed_credentials: bool,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallRequest {
    pub request_id: RequestId,
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub namespace: Option<String>,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallResponse {
    pub content_items: Vec<serde_json::Value>,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdate {
    pub auth_mode: Option<AuthMode>,
    pub plan_type: Option<PlanType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode {
    #[serde(rename = "apikey")]
    ApiKey,
    #[serde(rename = "chatgpt")]
    Chatgpt,
    #[serde(rename = "chatgptAuthTokens")]
    ChatgptAuthTokens,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanType {
    #[serde(rename = "free")]
    Free,
    #[serde(rename = "go")]
    Go,
    #[serde(rename = "plus")]
    Plus,
    #[serde(rename = "pro")]
    Pro,
    #[serde(rename = "prolite")]
    Prolite,
    #[serde(rename = "team")]
    Team,
    #[serde(rename = "business")]
    Business,
    #[serde(rename = "self_serve_business_prolite")]
    SelfServeBusinessProlite,
    #[serde(rename = "self_serve_business_usage_based")]
    SelfServeBusinessUsageBased,
    #[serde(rename = "ent26")]
    Ent26,
    #[serde(rename = "enterprise_cbp_automation")]
    EnterpriseCbpAutomation,
    #[serde(rename = "enterprise_cbp_usage_based")]
    EnterpriseCbpUsageBased,
    #[serde(rename = "enterprise")]
    Enterprise,
    #[serde(rename = "edu")]
    Edu,
    #[serde(rename = "unknown")]
    Unknown,
}
