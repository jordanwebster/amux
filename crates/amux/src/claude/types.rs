//! Claude Code integration types.
//!
//! This module contains all types specific to Claude Code integration:
//! hook events, permission requests, tool input structs, structured
//! input/output envelopes, and permission responses.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Hook {
    Claude(ClaudeHook),
}

/// Claude Code hook event - uses Claude's tagged JSON format
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
    PermissionRequest(ClaudePermissionRequest),
    PreToolUse(ClaudePreToolUse),
    PostToolUse(Box<ClaudePostToolUse>),
    Stop(ClaudeStop),
    #[serde(other)]
    Unknown,
}

/// SessionStart hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeSessionStart {
    pub session_id: Uuid,
    pub transcript_path: String,
}

/// Stop hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeStop {
    pub session_id: Uuid,
    pub stop_hook_active: bool,
    pub last_assistant_message: String,
}

/// PermissionRequest hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudePermissionRequest {
    pub session_id: Uuid,
    #[serde(flatten)]
    pub tool: ClaudePermissionTool,
}

/// PreToolUse hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudePreToolUse {
    pub session_id: Uuid,
    pub tool_use_id: String,
    #[serde(flatten)]
    pub tool: PreToolUse,
}

/// PostToolUse hook data from Claude Code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudePostToolUse {
    pub session_id: Uuid,
    pub tool_use_id: String,
    #[serde(flatten)]
    pub tool: PostToolUse,
}

/// Tool input fields for the Edit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EditToolInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Tool input fields for the AskUserQuestion tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionToolInput {
    pub questions: Vec<AskUserQuestionItem>,
}

/// A single question within an AskUserQuestion request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionItem {
    pub question: String,
    pub header: String,
    pub options: Vec<AskUserQuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

/// An option within an AskUserQuestion question
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub preview: Option<String>,
}

/// Tool input fields for the Bash tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BashToolInput {
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default, rename = "run_in_background")]
    pub run_in_background: Option<bool>,
    #[serde(default, rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
}

/// Tool input fields for the Agent tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentToolInput {
    pub description: String,
    pub prompt: String,
    pub subagent_type: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default, rename = "run_in_background")]
    pub run_in_background: Option<bool>,
    #[serde(default)]
    pub max_turns: Option<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub isolation: Option<String>,
}

/// Tool input fields for the TaskOutput tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TaskOutputToolInput {
    pub task_id: String,
    pub block: bool,
    pub timeout: u64,
}

/// Tool input fields for the Read tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ReadToolInput {
    pub file_path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub pages: Option<String>,
}

/// Tool input fields for the Write tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WriteToolInput {
    pub file_path: String,
    pub content: String,
}

/// Tool input fields for the Glob tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GlobToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Tool input fields for the Grep tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GrepToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub output_mode: Option<String>,
}

/// Tool input fields for the TaskStop tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TaskStopToolInput {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub shell_id: Option<String>,
}

/// Tool input fields for the WebFetch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebFetchToolInput {
    pub url: String,
    pub prompt: String,
}

/// Tool input fields for the WebSearch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebSearchToolInput {
    pub query: String,
    #[serde(default)]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default)]
    pub blocked_domains: Option<Vec<String>>,
}

/// Tool input fields for the NotebookEdit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct NotebookEditToolInput {
    pub notebook_path: String,
    #[serde(default)]
    pub cell_id: Option<String>,
    pub new_source: String,
    #[serde(default)]
    pub cell_type: Option<String>,
    #[serde(default)]
    pub edit_mode: Option<String>,
}

/// Tool input fields for the TodoWrite tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TodoWriteToolInput {
    pub todos: Vec<TodoItem>,
}

/// Tool input fields for the Skill tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillToolInput {
    pub skill: String,
    #[serde(default)]
    pub args: Option<String>,
}

/// Tool input fields for the ExitPlanMode tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModeToolInput {
    #[serde(default, rename = "allowedPrompts")]
    pub allowed_prompts: Vec<ExitPlanModePrompt>,
}

/// A single allowed prompt within an ExitPlanMode request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModePrompt {
    pub tool: String,
    pub prompt: String,
}

/// A todo item used by the TodoWrite tool.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
}

/// Tool input fields for the ListMcpResources tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ListMcpResourcesToolInput {
    #[serde(default)]
    pub server: Option<String>,
}

/// Tool input fields for the ReadMcpResource tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ReadMcpResourceToolInput {
    pub server: String,
    pub uri: String,
}

/// Tool input fields for the Config tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ConfigToolInput {
    pub setting: String,
    #[serde(default)]
    pub value: Option<Value>,
}

/// Tool input fields for the EnterWorktree tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EnterWorktreeToolInput {
    #[serde(default)]
    pub name: Option<String>,
}

/// Typed tool input from Claude Code hooks (PreToolUse / PermissionRequest).
/// Uses internally-tagged format so #[serde(other)] can ignore unknown tool_input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "tool_name")]
pub enum PreToolUse {
    #[serde(alias = "Task")]
    Agent {
        tool_input: AgentToolInput,
    },
    Edit {
        tool_input: EditToolInput,
    },
    AskUserQuestion {
        tool_input: AskUserQuestionToolInput,
    },
    Bash {
        tool_input: BashToolInput,
    },
    TaskOutput {
        tool_input: TaskOutputToolInput,
    },
    Read {
        tool_input: ReadToolInput,
    },
    Write {
        tool_input: WriteToolInput,
    },
    Glob {
        tool_input: GlobToolInput,
    },
    Grep {
        tool_input: GrepToolInput,
    },
    TaskStop {
        tool_input: TaskStopToolInput,
    },
    WebFetch {
        tool_input: WebFetchToolInput,
    },
    WebSearch {
        tool_input: WebSearchToolInput,
    },
    NotebookEdit {
        tool_input: NotebookEditToolInput,
    },
    TodoWrite {
        tool_input: TodoWriteToolInput,
    },
    Skill {
        tool_input: SkillToolInput,
    },
    ExitPlanMode {
        tool_input: ExitPlanModeToolInput,
    },
    ListMcpResources {
        tool_input: ListMcpResourcesToolInput,
    },
    ReadMcpResource {
        tool_input: ReadMcpResourceToolInput,
    },
    Config {
        tool_input: ConfigToolInput,
    },
    EnterWorktree {
        tool_input: EnterWorktreeToolInput,
    },
    #[serde(other)]
    Unknown,
}

/// Backward-compatibility alias
pub type ClaudePermissionTool = PreToolUse;

impl std::fmt::Display for PreToolUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreToolUse::Agent { tool_input } => {
                write!(f, "Agent {}", tool_input.subagent_type)
            }
            PreToolUse::Edit { tool_input } => {
                write!(f, "Edit {}", tool_input.file_path)
            }
            PreToolUse::AskUserQuestion { tool_input } => {
                write!(
                    f,
                    "AskUserQuestion ({} question(s))",
                    tool_input.questions.len()
                )
            }
            PreToolUse::Bash { tool_input } => {
                write!(f, "Bash `{}`", tool_input.command)
            }
            PreToolUse::TaskOutput { tool_input } => {
                write!(f, "TaskOutput {}", tool_input.task_id)
            }
            PreToolUse::Read { tool_input } => {
                write!(f, "Read {}", tool_input.file_path)
            }
            PreToolUse::Write { tool_input } => {
                write!(f, "Write {}", tool_input.file_path)
            }
            PreToolUse::Glob { tool_input } => {
                write!(f, "Glob {}", tool_input.pattern)
            }
            PreToolUse::Grep { tool_input } => {
                write!(f, "Grep {}", tool_input.pattern)
            }
            PreToolUse::TaskStop { tool_input } => {
                write!(
                    f,
                    "TaskStop {}",
                    tool_input.task_id.as_deref().unwrap_or("current task")
                )
            }
            PreToolUse::WebFetch { tool_input } => {
                write!(f, "WebFetch {}", tool_input.url)
            }
            PreToolUse::WebSearch { tool_input } => {
                write!(f, "WebSearch {}", tool_input.query)
            }
            PreToolUse::NotebookEdit { tool_input } => {
                write!(f, "NotebookEdit {}", tool_input.notebook_path)
            }
            PreToolUse::TodoWrite { tool_input } => {
                write!(f, "TodoWrite ({} todo(s))", tool_input.todos.len())
            }
            PreToolUse::Skill { tool_input } => {
                write!(f, "Skill {}", tool_input.skill)
            }
            PreToolUse::ExitPlanMode { .. } => write!(f, "ExitPlanMode"),
            PreToolUse::ListMcpResources { tool_input } => {
                write!(
                    f,
                    "ListMcpResources {}",
                    tool_input.server.as_deref().unwrap_or("all")
                )
            }
            PreToolUse::ReadMcpResource { tool_input } => {
                write!(f, "ReadMcpResource {}", tool_input.uri)
            }
            PreToolUse::Config { tool_input } => {
                write!(f, "Config {}", tool_input.setting)
            }
            PreToolUse::EnterWorktree { tool_input } => {
                write!(
                    f,
                    "EnterWorktree {}",
                    tool_input.name.as_deref().unwrap_or("temporary")
                )
            }
            PreToolUse::Unknown => write!(f, "Unknown tool"),
        }
    }
}

/// A single hunk in a unified diff patch (Edit/Write tool results).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// Typed tool result data from Claude Code PostToolUse hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "tool_name")]
pub enum PostToolUse {
    #[serde(alias = "Task")]
    Agent {
        tool_input: AgentToolInput,
        tool_response: AgentToolOutput,
    },
    AskUserQuestion {
        tool_input: AskUserQuestionToolInput,
        tool_response: AskUserQuestionToolOutput,
    },
    Bash {
        tool_input: BashToolInput,
        tool_response: BashToolOutput,
    },
    Config {
        tool_input: ConfigToolInput,
        tool_response: ConfigToolOutput,
    },
    EnterWorktree {
        tool_input: EnterWorktreeToolInput,
        tool_response: EnterWorktreeToolOutput,
    },
    ExitPlanMode {
        tool_input: ExitPlanModeToolInput,
        tool_response: ExitPlanModeToolOutput,
    },
    Edit {
        tool_input: EditToolInput,
        tool_response: EditToolOutput,
    },
    Read {
        tool_input: ReadToolInput,
        tool_response: ReadToolOutput,
    },
    Write {
        tool_input: WriteToolInput,
        tool_response: WriteToolOutput,
    },
    Glob {
        tool_input: GlobToolInput,
        tool_response: GlobToolOutput,
    },
    Grep {
        tool_input: GrepToolInput,
        tool_response: GrepToolOutput,
    },
    ListMcpResources {
        tool_input: ListMcpResourcesToolInput,
        tool_response: Vec<McpResourceInfo>,
    },
    NotebookEdit {
        tool_input: NotebookEditToolInput,
        tool_response: NotebookEditToolOutput,
    },
    ReadMcpResource {
        tool_input: ReadMcpResourceToolInput,
        tool_response: ReadMcpResourceToolOutput,
    },
    TaskOutput {
        tool_input: TaskOutputToolInput,
        tool_response: BashToolOutput,
    },
    TaskStop {
        tool_input: TaskStopToolInput,
        tool_response: TaskStopToolOutput,
    },
    TodoWrite {
        tool_input: TodoWriteToolInput,
        tool_response: TodoWriteToolOutput,
    },
    WebFetch {
        tool_input: WebFetchToolInput,
        tool_response: WebFetchToolOutput,
    },
    WebSearch {
        tool_input: WebSearchToolInput,
        tool_response: WebSearchToolOutput,
    },
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for PostToolUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PostToolUse::Agent { tool_input, .. } => {
                write!(f, "Agent {}", tool_input.subagent_type)
            }
            PostToolUse::AskUserQuestion { tool_input, .. } => {
                write!(
                    f,
                    "AskUserQuestion ({} question(s))",
                    tool_input.questions.len()
                )
            }
            PostToolUse::Bash { tool_input, .. } => {
                write!(f, "Bash `{}`", tool_input.command)
            }
            PostToolUse::Config { tool_input, .. } => {
                write!(f, "Config {}", tool_input.setting)
            }
            PostToolUse::EnterWorktree { tool_input, .. } => {
                write!(
                    f,
                    "EnterWorktree {}",
                    tool_input.name.as_deref().unwrap_or("temporary")
                )
            }
            PostToolUse::ExitPlanMode { .. } => write!(f, "ExitPlanMode"),
            PostToolUse::Edit { tool_input, .. } => {
                write!(f, "Edit {}", tool_input.file_path)
            }
            PostToolUse::Read { tool_input, .. } => {
                write!(f, "Read {}", tool_input.file_path)
            }
            PostToolUse::Write { tool_input, .. } => {
                write!(f, "Write {}", tool_input.file_path)
            }
            PostToolUse::Glob { tool_input, .. } => {
                write!(f, "Glob {}", tool_input.pattern)
            }
            PostToolUse::Grep { tool_input, .. } => {
                write!(f, "Grep {}", tool_input.pattern)
            }
            PostToolUse::ListMcpResources { tool_input, .. } => {
                write!(
                    f,
                    "ListMcpResources {}",
                    tool_input.server.as_deref().unwrap_or("all")
                )
            }
            PostToolUse::NotebookEdit { tool_input, .. } => {
                write!(f, "NotebookEdit {}", tool_input.notebook_path)
            }
            PostToolUse::ReadMcpResource { tool_input, .. } => {
                write!(f, "ReadMcpResource {}", tool_input.uri)
            }
            PostToolUse::TaskOutput { tool_input, .. } => {
                write!(f, "TaskOutput {}", tool_input.task_id)
            }
            PostToolUse::TaskStop { tool_input, .. } => {
                write!(
                    f,
                    "TaskStop {}",
                    tool_input.task_id.as_deref().unwrap_or("current task")
                )
            }
            PostToolUse::TodoWrite { tool_input, .. } => {
                write!(f, "TodoWrite ({} todo(s))", tool_input.todos.len())
            }
            PostToolUse::WebFetch { tool_input, .. } => {
                write!(f, "WebFetch {}", tool_input.url)
            }
            PostToolUse::WebSearch { tool_input, .. } => {
                write!(f, "WebSearch {}", tool_input.query)
            }
            PostToolUse::Unknown => write!(f, "Unknown tool"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDiff {
    pub filename: String,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub changes: usize,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditToolOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<PatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(default, rename = "gitDiff")]
    pub git_diff: Option<GitDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteToolOutput {
    #[serde(rename = "type")]
    pub write_type: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<PatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(default, rename = "gitDiff")]
    pub git_diff: Option<GitDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ReadToolOutput {
    #[serde(rename = "text")]
    Text { file: TextReadFileOutput },
    #[serde(rename = "image")]
    Image { file: ImageReadFileOutput },
    #[serde(rename = "notebook")]
    Notebook { file: NotebookReadFileOutput },
    #[serde(rename = "pdf")]
    Pdf { file: PdfReadFileOutput },
    #[serde(rename = "parts")]
    Parts { file: PartsReadFileOutput },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextReadFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageReadFileOutput {
    pub base64: String,
    #[serde(rename = "type")]
    pub media_type: String,
    #[serde(rename = "originalSize")]
    pub original_size: usize,
    #[serde(default)]
    pub dimensions: Option<ImageDimensions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageDimensions {
    #[serde(default, rename = "originalWidth")]
    pub original_width: Option<usize>,
    #[serde(default, rename = "originalHeight")]
    pub original_height: Option<usize>,
    #[serde(default, rename = "displayWidth")]
    pub display_width: Option<usize>,
    #[serde(default, rename = "displayHeight")]
    pub display_height: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotebookReadFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub cells: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PdfReadFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub base64: String,
    #[serde(rename = "originalSize")]
    pub original_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartsReadFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "originalSize")]
    pub original_size: usize,
    pub count: usize,
    #[serde(rename = "outputDir")]
    pub output_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserQuestionToolOutput {
    pub questions: Vec<AskUserQuestionItem>,
    pub answers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BashToolOutput {
    pub stdout: String,
    pub stderr: String,
    #[serde(default, rename = "rawOutputPath")]
    pub raw_output_path: Option<String>,
    pub interrupted: bool,
    #[serde(default, rename = "isImage")]
    pub is_image: Option<bool>,
    #[serde(default, rename = "backgroundTaskId")]
    pub background_task_id: Option<String>,
    #[serde(default, rename = "backgroundedByUser")]
    pub backgrounded_by_user: Option<bool>,
    #[serde(default, rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
    #[serde(default, rename = "returnCodeInterpretation")]
    pub return_code_interpretation: Option<String>,
    #[serde(default, rename = "structuredContent")]
    pub structured_content: Option<Vec<Value>>,
    #[serde(default, rename = "persistedOutputPath")]
    pub persisted_output_path: Option<String>,
    #[serde(default, rename = "persistedOutputSize")]
    pub persisted_output_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobToolOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrepToolOutput {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(default, rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(default, rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(default, rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchToolOutput {
    pub bytes: usize,
    pub code: u16,
    #[serde(rename = "codeText")]
    pub code_text: String,
    pub result: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchToolOutput {
    pub query: String,
    pub results: Vec<Value>,
    #[serde(rename = "durationSeconds")]
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExitPlanModeToolOutput {
    pub plan: Option<String>,
    #[serde(rename = "isAgent")]
    pub is_agent: bool,
    #[serde(default, rename = "filePath")]
    pub file_path: Option<String>,
    #[serde(default, rename = "hasTaskTool")]
    pub has_task_tool: Option<bool>,
    #[serde(default, rename = "awaitingLeaderApproval")]
    pub awaiting_leader_approval: Option<bool>,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotebookEditToolOutput {
    pub new_source: String,
    #[serde(default)]
    pub cell_id: Option<String>,
    pub cell_type: String,
    pub language: String,
    pub edit_mode: String,
    #[serde(default)]
    pub error: Option<String>,
    pub notebook_path: String,
    pub original_file: String,
    pub updated_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum AgentToolOutput {
    #[serde(rename = "completed")]
    Completed {
        #[serde(rename = "agentId")]
        agent_id: String,
        content: Vec<Value>,
        #[serde(rename = "totalToolUseCount")]
        total_tool_use_count: usize,
        #[serde(rename = "totalDurationMs")]
        total_duration_ms: u64,
        #[serde(rename = "totalTokens")]
        total_tokens: usize,
        usage: Value,
        prompt: String,
    },
    #[serde(rename = "async_launched")]
    AsyncLaunched {
        #[serde(rename = "agentId")]
        agent_id: String,
        description: String,
        prompt: String,
        #[serde(rename = "outputFile")]
        output_file: String,
        #[serde(default, rename = "canReadOutputFile")]
        can_read_output_file: Option<bool>,
    },
    #[serde(rename = "sub_agent_entered")]
    SubAgentEntered {
        description: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStopToolOutput {
    pub message: String,
    pub task_id: String,
    pub task_type: String,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoWriteToolOutput {
    #[serde(rename = "oldTodos")]
    pub old_todos: Vec<TodoItem>,
    #[serde(rename = "newTodos")]
    pub new_todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadMcpResourceToolOutput {
    pub contents: Vec<McpResourceContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigToolOutput {
    pub success: bool,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub setting: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default, rename = "previousValue")]
    pub previous_value: Option<Value>,
    #[serde(default, rename = "newValue")]
    pub new_value: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnterWorktreeToolOutput {
    #[serde(rename = "worktreePath")]
    pub worktree_path: String,
    #[serde(default, rename = "worktreeBranch")]
    pub worktree_branch: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ClaudeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeHook::SessionStart(s) => {
                write!(f, "session {} at {}", s.session_id, s.transcript_path)
            }
            ClaudeHook::PermissionRequest(p) => {
                write!(f, "session {} {}", p.session_id, p.tool)
            }
            ClaudeHook::PreToolUse(p) => {
                write!(f, "session {} pre-tool {}", p.session_id, p.tool)
            }
            ClaudeHook::PostToolUse(p) => {
                write!(f, "session {} post-tool {}", p.session_id, p.tool)
            }
            ClaudeHook::Stop(s) => {
                write!(f, "session {} stopped", s.session_id)
            }
            ClaudeHook::Unknown => write!(f, "unknown hook"),
        }
    }
}

/// Response to a permission request (sent from client to server)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PermissionResponse {
    /// Press "1" - accept this edit
    Yes,
    /// Press "2" - accept all edits
    YesAll,
    /// Press "3" - deny
    No,
}

/// Claude-specific structured output (internally tagged by "type")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClaudeStructuredOutput {
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
    PermissionRequest {
        #[serde(flatten)]
        tool: PreToolUse,
    },
    /// Tool about to be used
    PreToolUseEvent {
        tool_use_id: String,
        #[serde(flatten)]
        tool: PreToolUse,
        timestamp: String,
    },
    /// Tool use completed
    PostToolUseEvent {
        tool_use_id: String,
        #[serde(flatten)]
        tool: PostToolUse,
        timestamp: String,
    },
    /// Agent has stopped and is waiting for input
    AgentStopped,
    /// Unknown entry type (forward-compatibility)
    #[serde(other)]
    Unknown,
}

/// Wrapper enum for structured output, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredOutput {
    Claude(ClaudeStructuredOutput),
}

/// Response to an AskUserQuestion tool call.
/// Matches Claude Code's tool_result format: echoes back the questions
/// and provides answers as label strings (or custom text for "Other").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserQuestionResponse {
    /// Echo of the original questions (determines question type + option indices)
    pub questions: Vec<AskUserQuestionItem>,
    /// Map from question text to selected label (or custom text for "Other").
    /// Multi-select: comma-separated labels (e.g. "Auth, Cache").
    pub answers: HashMap<String, String>,
    /// Question text where "Chat about this" was selected.
    /// Answered questions are processed first, then we navigate
    /// to this question's page to select ChatAboutThis.
    /// Ends the tool immediately (no submit step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_about_this: Option<String>,
}

/// Claude-specific structured input
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClaudeStructuredInput {
    /// Response to a permission request
    PermissionResponse(PermissionResponse),
    /// Submit a message (text input with trailing carriage return)
    SubmitMessage { data: Vec<u8> },
    /// Response to an AskUserQuestion tool call
    AskUserQuestionResponse(AskUserQuestionResponse),
}

/// Wrapper enum for structured input, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredInput {
    Claude(ClaudeStructuredInput),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_tool_input(command: &str) -> BashToolInput {
        BashToolInput {
            command: command.to_string(),
            description: None,
            timeout: None,
            run_in_background: None,
            dangerously_disable_sandbox: None,
        }
    }

    fn bash_tool_output(stdout: &str) -> BashToolOutput {
        BashToolOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: None,
            backgrounded_by_user: None,
            dangerously_disable_sandbox: None,
            return_code_interpretation: None,
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
        }
    }

    #[test]
    fn test_unknown_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "SomeNewTool",
            "tool_input": {"foo": "bar"}
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        assert!(matches!(p.tool, ClaudePermissionTool::Unknown));
    }

    #[test]
    fn test_ask_user_question_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which library should we use?",
                    "header": "Library",
                    "options": [
                        {"label": "reqwest", "description": "HTTP client"},
                        {"label": "ureq", "description": "Blocking HTTP client"}
                    ],
                    "multiSelect": false
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert_eq!(tool_input.questions.len(), 1);
        let q = &tool_input.questions[0];
        assert_eq!(q.question, "Which library should we use?");
        assert_eq!(q.header, "Library");
        assert!(!q.multi_select);
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].label, "reqwest");
        assert_eq!(q.options[1].label, "ureq");
    }

    #[test]
    fn test_ask_user_question_multi_select() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which features?",
                    "header": "Features",
                    "options": [
                        {"label": "Auth", "description": "Authentication"},
                        {"label": "Cache", "description": "Caching layer"}
                    ],
                    "multiSelect": true
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert!(tool_input.questions[0].multi_select);
    }

    #[test]
    fn test_known_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Edit",
            "tool_input": {
                "file_path": "/tmp/test.rs",
                "old_string": "foo",
                "new_string": "bar"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Edit { tool_input } = p.tool else {
            panic!("Expected Edit tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/test.rs");
        assert_eq!(tool_input.old_string, "foo");
        assert_eq!(tool_input.new_string, "bar");
        assert!(!tool_input.replace_all);
    }

    #[test]
    fn test_bash_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "cargo test",
                "description": "Run tests",
                "timeout": 30000
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "cargo test");
        assert_eq!(tool_input.description.as_deref(), Some("Run tests"));
        assert_eq!(tool_input.timeout, Some(30000));
    }

    #[test]
    fn test_bash_tool_optional_fields() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "ls");
        assert!(tool_input.description.is_none());
        assert!(tool_input.timeout.is_none());
    }

    #[test]
    fn test_write_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/output.txt",
                "content": "hello world"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Write { tool_input } = p.tool else {
            panic!("Expected Write tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/output.txt");
        assert_eq!(tool_input.content, "hello world");
    }

    #[test]
    fn test_web_search_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "WebSearch",
            "tool_input": {
                "query": "rust serde tutorial"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::WebSearch { tool_input } = p.tool else {
            panic!("Expected WebSearch tool");
        };
        assert_eq!(tool_input.query, "rust serde tutorial");
    }

    #[test]
    fn test_skill_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Skill",
            "tool_input": {
                "skill": "commit",
                "args": "-m 'Fix bug'"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Skill { tool_input } = p.tool else {
            panic!("Expected Skill tool");
        };
        assert_eq!(tool_input.skill, "commit");
        assert_eq!(tool_input.args.as_deref(), Some("-m 'Fix bug'"));
    }

    #[test]
    fn test_exit_plan_mode_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "ExitPlanMode",
            "tool_input": {
                "allowedPrompts": [
                    {"tool": "Bash", "prompt": "run tests"}
                ]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::ExitPlanMode { tool_input } = p.tool else {
            panic!("Expected ExitPlanMode tool");
        };
        assert_eq!(tool_input.allowed_prompts.len(), 1);
        assert_eq!(tool_input.allowed_prompts[0].tool, "Bash");
        assert_eq!(tool_input.allowed_prompts[0].prompt, "run tests");
    }

    #[test]
    fn test_claude_permission_tool_display() {
        let tool = ClaudePermissionTool::Bash {
            tool_input: bash_tool_input("cargo test"),
        };
        assert_eq!(tool.to_string(), "Bash `cargo test`");

        let tool = ClaudePermissionTool::Edit {
            tool_input: EditToolInput {
                file_path: "/tmp/test.rs".to_string(),
                old_string: "a".to_string(),
                new_string: "b".to_string(),
                replace_all: false,
            },
        };
        assert_eq!(tool.to_string(), "Edit /tmp/test.rs");

        assert_eq!(ClaudePermissionTool::Unknown.to_string(), "Unknown tool");
    }

    #[test]
    fn test_ask_user_question_option_without_preview() {
        let json = r#"{"label": "reqwest", "description": "HTTP client"}"#;
        let opt: AskUserQuestionOption = serde_json::from_str(json).unwrap();
        assert_eq!(opt.label, "reqwest");
        assert_eq!(opt.description, "HTTP client");
        assert!(opt.preview.is_none());
    }

    #[test]
    fn test_ask_user_question_option_with_preview() {
        let json = r#"{
            "label": "Layout A",
            "description": "Side-by-side layout",
            "preview": "```\n+-----+-----+\n| A   | B   |\n+-----+-----+\n```"
        }"#;
        let opt: AskUserQuestionOption = serde_json::from_str(json).unwrap();
        assert_eq!(opt.label, "Layout A");
        assert!(opt.preview.is_some());
        assert!(opt.preview.unwrap().contains("+-----+"));
    }

    #[test]
    fn test_ask_user_question_response_roundtrip() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![
                    AskUserQuestionOption {
                        label: "reqwest".to_string(),
                        description: "HTTP client".to_string(),
                        preview: None,
                    },
                    AskUserQuestionOption {
                        label: "ureq".to_string(),
                        description: "Blocking HTTP".to_string(),
                        preview: None,
                    },
                ],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "reqwest".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_custom_answer() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![AskUserQuestionOption {
                    label: "reqwest".to_string(),
                    description: "HTTP client".to_string(),
                    preview: None,
                }],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "my custom lib".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_multi_select_answer() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which features?".to_string(),
                header: "Features".to_string(),
                options: vec![
                    AskUserQuestionOption {
                        label: "Auth".to_string(),
                        description: "Authentication".to_string(),
                        preview: None,
                    },
                    AskUserQuestionOption {
                        label: "Cache".to_string(),
                        description: "Caching".to_string(),
                        preview: None,
                    },
                ],
                multi_select: true,
            }],
            answers: HashMap::from([("Which features?".to_string(), "Auth, Cache".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_as_structured_input() {
        let input = ClaudeStructuredInput::AskUserQuestionResponse(AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![AskUserQuestionOption {
                    label: "reqwest".to_string(),
                    description: "HTTP client".to_string(),
                    preview: None,
                }],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "reqwest".to_string())]),
            chat_about_this: None,
        });
        let serialized = serde_json::to_string(&input).unwrap();
        let deserialized: ClaudeStructuredInput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_claude_hook_display() {
        let hook = ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
        });
        assert_eq!(
            hook.to_string(),
            "session 00000000-0000-0000-0000-000000000001 at /tmp/transcript.jsonl"
        );

        let hook = ClaudeHook::PermissionRequest(ClaudePermissionRequest {
            session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            tool: ClaudePermissionTool::Bash {
                tool_input: bash_tool_input("ls"),
            },
        });
        assert_eq!(
            hook.to_string(),
            "session 00000000-0000-0000-0000-000000000002 Bash `ls`"
        );
    }

    #[test]
    fn test_pre_tool_use_serde_roundtrip() {
        let tool = PreToolUse::Edit {
            tool_input: EditToolInput {
                file_path: "/tmp/test.rs".to_string(),
                old_string: "foo".to_string(),
                new_string: "bar".to_string(),
                replace_all: false,
            },
        };
        let json = serde_json::to_string(&tool).unwrap();
        let deserialized: PreToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(tool, deserialized);
    }

    #[test]
    fn test_post_tool_use_edit_serde_roundtrip() {
        let post = PostToolUse::Edit {
            tool_input: EditToolInput {
                file_path: "/tmp/test.rs".to_string(),
                old_string: "foo".to_string(),
                new_string: "bar".to_string(),
                replace_all: false,
            },
            tool_response: EditToolOutput {
                file_path: "/tmp/test.rs".to_string(),
                old_string: "foo".to_string(),
                new_string: "bar".to_string(),
                original_file: "foo\n".to_string(),
                structured_patch: vec![PatchHunk {
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 4,
                    lines: vec![" line1".to_string(), "-old".to_string(), "+new".to_string()],
                }],
                user_modified: false,
                replace_all: false,
                git_diff: None,
            },
        };
        let json = serde_json::to_string(&post).unwrap();
        let deserialized: PostToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(post, deserialized);
    }

    #[test]
    fn test_post_tool_use_bash_serde_roundtrip() {
        let post = PostToolUse::Bash {
            tool_input: bash_tool_input("echo hello"),
            tool_response: bash_tool_output("hello world\n"),
        };
        let json = serde_json::to_string(&post).unwrap();
        let deserialized: PostToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(post, deserialized);
    }

    #[test]
    fn test_post_tool_use_ask_user_question_serde_roundtrip() {
        let questions = vec![AskUserQuestionItem {
            question: "Which library?".to_string(),
            header: "Library".to_string(),
            options: vec![AskUserQuestionOption {
                label: "reqwest".to_string(),
                description: "HTTP client".to_string(),
                preview: None,
            }],
            multi_select: false,
        }];
        let post = PostToolUse::AskUserQuestion {
            tool_input: AskUserQuestionToolInput {
                questions: questions.clone(),
            },
            tool_response: AskUserQuestionToolOutput {
                questions,
                answers: HashMap::from([("Which library?".to_string(), "reqwest".to_string())]),
            },
        };
        let json = serde_json::to_string(&post).unwrap();
        let deserialized: PostToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(post, deserialized);
    }

    #[test]
    fn test_post_tool_use_read_serde_roundtrip() {
        let post = PostToolUse::Read {
            tool_input: ReadToolInput {
                file_path: "/tmp/test.rs".to_string(),
                offset: Some(10),
                limit: Some(5),
                pages: None,
            },
            tool_response: ReadToolOutput::Text {
                file: TextReadFileOutput {
                    file_path: "/tmp/test.rs".to_string(),
                    content: "fn main() {}\n".to_string(),
                    num_lines: 1,
                    start_line: 1,
                    total_lines: 1,
                },
            },
        };
        let json = serde_json::to_string(&post).unwrap();
        let deserialized: PostToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(post, deserialized);
    }

    #[test]
    fn test_claude_pre_tool_use_deserializes_from_hook_json() {
        let json = r#"{
            "hook_event_name": "PreToolUse",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_abc123",
            "tool_name": "Bash",
            "tool_input": {
                "command": "cargo test",
                "description": "Run tests"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PreToolUse(pre) = hook else {
            panic!("Expected PreToolUse, got {hook:?}");
        };
        assert_eq!(pre.tool_use_id, "toolu_abc123");
        let PreToolUse::Bash { tool_input } = pre.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "cargo test");
    }

    #[test]
    fn test_claude_pre_tool_use_read_deserializes_from_hook_json() {
        let json = r#"{
            "hook_event_name": "PreToolUse",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_read123",
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/tmp/test.rs",
                "offset": 10,
                "limit": 5
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PreToolUse(pre) = hook else {
            panic!("Expected PreToolUse");
        };
        let PreToolUse::Read { tool_input } = pre.tool else {
            panic!("Expected Read tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/test.rs");
        assert_eq!(tool_input.offset, Some(10));
        assert_eq!(tool_input.limit, Some(5));
    }

    #[test]
    fn test_claude_post_tool_use_deserializes_write_output_from_hook_json() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_abc123",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/test.rs",
                "content": "bar"
            },
            "tool_response": {
                "success": true,
                "type": "create",
                "filePath": "/tmp/test.rs",
                "content": "bar",
                "structuredPatch": [{
                    "oldStart": 1,
                    "oldLines": 0,
                    "newStart": 1,
                    "newLines": 1,
                    "lines": ["-foo", "+bar"]
                }],
                "originalFile": null
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PostToolUse(post) = hook else {
            panic!("Expected PostToolUse");
        };
        assert_eq!(post.tool_use_id, "toolu_abc123");

        let PostToolUse::Write {
            tool_input,
            tool_response,
        } = &post.tool
        else {
            panic!("Expected Write post tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/test.rs");
        assert_eq!(tool_response.file_path, "/tmp/test.rs");
        assert_eq!(tool_response.structured_patch.len(), 1);
    }

    #[test]
    fn test_claude_post_tool_use_deserializes_read_output_from_hook_json() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_read456",
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/tmp/test.rs"
            },
            "tool_response": {
                "type": "text",
                "file": {
                    "filePath": "/tmp/test.rs",
                    "content": "fn main() {}\n",
                    "numLines": 1,
                    "startLine": 1,
                    "totalLines": 1
                }
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PostToolUse(post) = hook else {
            panic!("Expected PostToolUse");
        };
        let PostToolUse::Read {
            tool_response: ReadToolOutput::Text { file },
            ..
        } = &post.tool
        else {
            panic!("Expected text read output");
        };
        assert_eq!(file.file_path, "/tmp/test.rs");
        assert_eq!(file.total_lines, 1);
    }

    #[test]
    fn test_post_tool_use_unknown_tool_from_json() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_unknown",
            "tool_name": "SomeFutureTool",
            "tool_input": {},
            "tool_response": {
                "whatever": "data"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PostToolUse(post) = hook else {
            panic!("Expected PostToolUse");
        };
        assert!(matches!(post.tool, PostToolUse::Unknown));
    }

    #[test]
    fn test_claude_post_tool_use_missing_tool_response() {
        let json = r#"{
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_use_id": "toolu_abc789",
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/tmp/test.rs"
            }
        }"#;
        assert!(serde_json::from_str::<ClaudeHook>(json).is_err());
    }

    #[test]
    fn test_hook_pre_tool_use_msgpack_roundtrip() {
        let hook = Hook::Claude(ClaudeHook::PreToolUse(ClaudePreToolUse {
            session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            tool_use_id: "toolu_abc".to_string(),
            tool: PreToolUse::Bash {
                tool_input: bash_tool_input("ls"),
            },
        }));
        let encoded = rmp_serde::to_vec_named(&hook).unwrap();
        let decoded: Hook = rmp_serde::from_slice(&encoded).unwrap();
        let Hook::Claude(ClaudeHook::PreToolUse(pre)) = decoded else {
            panic!("Expected PreToolUse, got {decoded:?}");
        };
        assert_eq!(pre.tool_use_id, "toolu_abc");
    }

    #[test]
    fn test_handle_hook_message_msgpack_roundtrip() {
        use crate::message::{Command, Message};

        let agent_id = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::PreToolUse(ClaudePreToolUse {
            session_id: Uuid::new_v4(),
            tool_use_id: "toolu_xyz".to_string(),
            tool: PreToolUse::Bash {
                tool_input: BashToolInput {
                    description: Some("Run tests".to_string()),
                    ..bash_tool_input("cargo test")
                },
            },
        }));
        let msg = Message::Command(Command::HandleHook {
            agent_id,
            hook: Box::new(hook),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command(Command::HandleHook {
            agent_id: decoded_id,
            hook: decoded_hook,
        }) = decoded
        else {
            panic!("Expected HandleHook, got {decoded:?}");
        };
        assert_eq!(decoded_id, agent_id);
        let Hook::Claude(ClaudeHook::PreToolUse(pre)) = *decoded_hook else {
            panic!("Expected PreToolUse");
        };
        assert_eq!(pre.tool_use_id, "toolu_xyz");
    }

    #[test]
    fn test_post_tool_use_hook_msgpack_roundtrip() {
        use crate::message::{Command, Message};

        let agent_id = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::PostToolUse(Box::new(ClaudePostToolUse {
            session_id: Uuid::new_v4(),
            tool_use_id: "toolu_post".to_string(),
            tool: PostToolUse::Bash {
                tool_input: bash_tool_input("ls"),
                tool_response: bash_tool_output("file.txt\n"),
            },
        })));
        let msg = Message::Command(Command::HandleHook {
            agent_id,
            hook: Box::new(hook),
        });
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command(Command::HandleHook {
            hook: decoded_hook, ..
        }) = decoded
        else {
            panic!("Expected HandleHook");
        };
        let Hook::Claude(ClaudeHook::PostToolUse(post)) = *decoded_hook else {
            panic!("Expected PostToolUse");
        };
        assert_eq!(post.tool_use_id, "toolu_post");
        assert!(matches!(post.tool, PostToolUse::Bash { .. }));
    }

    #[test]
    fn test_permission_request_structured_output_flattens_tool_shape() {
        let output = ClaudeStructuredOutput::PermissionRequest {
            tool: PreToolUse::Bash {
                tool_input: bash_tool_input("cargo test"),
            },
        };

        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["type"], "PermissionRequest");
        assert_eq!(value["tool_name"], "Bash");
        assert_eq!(value["tool_input"]["command"], "cargo test");
        assert!(value.get("tool").is_none());
    }

    #[test]
    fn test_post_tool_use_structured_output_flattens_tool_shape() {
        let output = ClaudeStructuredOutput::PostToolUseEvent {
            tool_use_id: "toolu_read456".to_string(),
            tool: PostToolUse::Read {
                tool_input: ReadToolInput {
                    file_path: "/tmp/test.rs".to_string(),
                    offset: None,
                    limit: None,
                    pages: None,
                },
                tool_response: ReadToolOutput::Text {
                    file: TextReadFileOutput {
                        file_path: "/tmp/test.rs".to_string(),
                        content: "fn main() {}\n".to_string(),
                        num_lines: 1,
                        start_line: 1,
                        total_lines: 1,
                    },
                },
            },
            timestamp: "2026-03-20T12:00:00Z".to_string(),
        };

        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["type"], "PostToolUseEvent");
        assert_eq!(value["tool_use_id"], "toolu_read456");
        assert_eq!(value["tool_name"], "Read");
        assert_eq!(value["tool_input"]["file_path"], "/tmp/test.rs");
        assert_eq!(value["tool_response"]["type"], "text");
        assert_eq!(value["tool_response"]["file"]["filePath"], "/tmp/test.rs");
        assert!(value.get("tool").is_none());

        let roundtrip: ClaudeStructuredOutput = serde_json::from_value(value).unwrap();
        assert_eq!(output, roundtrip);
    }
}
