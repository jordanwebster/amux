//! Claude SDK running-fact observation shared by clients and the daemon.
//!
//! Reducer commands, optimistic input, answer dispatch, attachments, and
//! renderer state deliberately remain outside this module. The observation
//! owns only facts derivable from provider rows.

use std::collections::VecDeque;

use model::{AgentMessageKind, ContextMeter, ContextUsage, McpServerFact, ModelFact};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude_pty::facts::{
    LandedEdit, QuestionFact, SuggestionDestination, SuggestionFact, SuggestionKind,
    ToolInvocation, invocation,
};
use crate::claude_pty::{ClaudeTodos, TodoDisposition};

mod semantics;

pub use semantics::{
    ClaudeSdkBody, ClaudeSdkEntry, ClaudeSdkEntryKind, ClaudeSdkFold, ClaudeSdkPartial,
    DELIVERY_KEYED_VARIANTS,
};

/// A single streaming block cannot grow without bound while the feed is idle.
pub const CONTENT_BYTES_RETAINED: usize = 64 * 1024;
const ID_BYTES_RETAINED: usize = 512;
/// Task lifecycle state is independently bounded because it remains useful to
/// the activity line after presentation moved to the store window.
const TASKS_RETAINED: usize = 1000;
const TASK_LAUNCHES_RETAINED: usize = 1000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub id: u64,
    /// Stream position used by renderers to order and diff restored rows.
    pub seq: u64,
    pub kind: FeedEntryKind,
    /// The tool use whose subagent produced this entry, when it was not the
    /// session's own. Stream-JSON carries a subagent's rows on the parent's
    /// stream with this id set, so a result-only tail still says whose it was.
    pub parent_tool_use_id: Option<String>,
    /// Payload clipping is separate from missing earlier feed entries.
    pub content_truncated: bool,
}

impl FeedEntry {
    /// The tool use whose subagent produced this entry, when it was not the
    /// session's own.
    pub fn parent_tool_use_id(&self) -> Option<&str> {
        self.parent_tool_use_id.as_deref()
    }

    /// Rebuild a renderer-facing entry from its durable semantic body.
    pub fn restored(
        id: u64,
        kind: FeedEntryKind,
        parent_tool_use_id: Option<String>,
        content_truncated: bool,
    ) -> Self {
        Self {
            id,
            seq: 0,
            kind,
            parent_tool_use_id,
            content_truncated,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "entry", rename_all = "snake_case")]
pub enum FeedEntryKind {
    Prompt(PromptEntry),
    Message(MessageEntry),
    Thinking(ThinkingEntry),
    Tool(ToolEntry),
    Task(TaskEntry),
    Turn(TurnEntry),
    Compaction(CompactionEntry),
    AgentMessage(AgentMessageEntry),
    Status(StatusEntry),
    Boundary(BoundaryEntry),
    Unrecognized(UnrecognizedEntry),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finality {
    Streaming,
    /// The block stopped; its authoritative assistant row may still follow.
    Stopped,
    Complete,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub uuid: Option<String>,
    pub text: String,
    pub image_count: usize,
    pub synthetic: bool,
    pub replay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEntry {
    pub text: String,
    pub finality: Finality,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingEntry {
    pub text: String,
    pub redacted: bool,
    pub finality: Finality,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolEntry {
    pub tool_use_id: String,
    pub name: String,
    pub invocation: ToolInvocation,
    pub input: Option<Value>,
    pub input_json: String,
    pub finality: Finality,
    pub result: Option<ToolResult>,
    pub group_with_previous: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub details: Option<Value>,
    pub edit: Option<LandedEdit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Running,
    Completed,
    Failed,
    Stopped,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskEntry {
    pub task_id: String,
    /// The `Task`/`Agent` tool use that launched it. The lifecycle rows carry
    /// it, so the launch row and the task are one entry rather than two rows
    /// naming the same subagent.
    pub tool_use_id: Option<String>,
    pub description: String,
    pub subagent_type: Option<String>,
    pub state: TaskState,
    pub last_tool: Option<String>,
    pub summary: Option<String>,
    pub usage: Option<TaskUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUsage {
    pub total_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEntry {
    pub uuid: Option<String>,
    pub outcome: String,
    pub is_error: bool,
    pub stop_reason: Option<String>,
    pub result: Option<String>,
    pub errors: Vec<String>,
    pub usage: TokenUsage,
    pub model_usage: Option<Value>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub duration_api_ms: Option<u64>,
    pub num_turns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageEntry {
    pub id: Option<String>,
    pub context: Option<String>,
    pub from: String,
    pub kind: AgentMessageKind,
    pub text: String,
    pub delivery: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusEntry {
    pub status: String,
    pub details: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum BoundaryEntry {
    Ready {
        session_id: Option<String>,
        resumed: bool,
    },
    Gap {
        resumed_session_id: Option<String>,
    },
    ConversationReset {
        conversation_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnrecognizedEntry {
    pub row_type: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TaskLaunch {
    tool_use_id: String,
    description: Option<String>,
    subagent_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnState {
    #[default]
    Unknown,
    Idle,
    Working,
    Finished,
    Errored,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskWhy {
    Permission,
    Plan,
    Question,
    Elicitation,
    Dialog,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskKind {
    Permission {
        tool_name: String,
        invocation: ToolInvocation,
        suggestions: Vec<SuggestionFact>,
    },
    Plan {
        plan: Option<String>,
        plan_file_path: Option<String>,
    },
    Question {
        questions: Vec<QuestionFact>,
    },
    Elicitation {
        server: Option<String>,
        message: String,
        form: ElicitationForm,
    },
    Dialog {
        dialog_kind: String,
        payload: Value,
    },
}

impl AskKind {
    pub fn why(&self) -> AskWhy {
        match self {
            Self::Permission { .. } => AskWhy::Permission,
            Self::Plan { .. } => AskWhy::Plan,
            Self::Question { .. } => AskWhy::Question,
            Self::Elicitation { .. } => AskWhy::Elicitation,
            Self::Dialog { .. } => AskWhy::Dialog,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "content", rename_all = "snake_case")]
pub enum ElicitationForm {
    Fields(Vec<ElicitationField>),
    Unsupported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElicitationField {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub required: bool,
    pub kind: ElicitationFieldKind,
    pub default: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "values", rename_all = "snake_case")]
pub enum ElicitationFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    Enum(Vec<Value>),
}

impl ElicitationForm {
    pub fn from_schema(schema: &Value) -> Self {
        match elicitation_fields(schema) {
            Ok(fields) => Self::Fields(fields),
            Err(reason) => Self::Unsupported { reason },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AskObservation {
    Required {
        channel: &'static str,
        request_id: String,
        kind: AskKind,
        input: Value,
        suggestions: Vec<Value>,
    },
    Resolved {
        channel: &'static str,
        request_id: String,
    },
}

pub fn observe_ask(row: &Value) -> Option<AskObservation> {
    let tag = row["type"].as_str()?.strip_prefix("amux.claude_sdk.")?;
    let (channel, action) = tag.rsplit_once('_')?;
    if !matches!(channel, "permission" | "elicitation" | "dialog") {
        return None;
    }
    let request_id = row["request_id"].as_str()?.to_owned();
    let channel = match channel {
        "permission" => "permission",
        "elicitation" => "elicitation",
        "dialog" => "dialog",
        _ => unreachable!(),
    };
    if action == "resolved" {
        return Some(AskObservation::Resolved {
            channel,
            request_id,
        });
    }
    if action != "required" {
        return None;
    }
    let suggestions: Vec<Value> = row["suggestions"].as_array().cloned().unwrap_or_default();
    let input = row["input"].clone();
    let kind = match channel {
        "permission" => {
            let tool_name = ask_text(row, "tool_name").unwrap_or_default();
            match invocation(&tool_name, &input) {
                ToolInvocation::Plan {
                    plan,
                    plan_file_path,
                } => AskKind::Plan {
                    plan,
                    plan_file_path,
                },
                ToolInvocation::Question { questions } => AskKind::Question { questions },
                invocation => AskKind::Permission {
                    tool_name,
                    invocation,
                    suggestions: suggestions
                        .iter()
                        .map(|value| SuggestionFact {
                            kind: value["type"].as_str().map(SuggestionKind::from_wire),
                            destination: value["destination"]
                                .as_str()
                                .map(SuggestionDestination::from_wire),
                            directories: value["directories"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect(),
                        })
                        .collect(),
                },
            }
        }
        "elicitation" => AskKind::Elicitation {
            server: ask_text(row, "server"),
            message: ask_text(row, "message").unwrap_or_default(),
            form: ElicitationForm::from_schema(&row["schema"]),
        },
        "dialog" => AskKind::Dialog {
            dialog_kind: ask_text(row, "dialog_kind").unwrap_or_default(),
            payload: row["payload"].clone(),
        },
        _ => unreachable!(),
    };
    Some(AskObservation::Required {
        channel,
        request_id,
        kind,
        input,
        suggestions,
    })
}

/// One field per property of the schema, ordered by field name. A JSON
/// object's keys do not keep the order they were written in once the row has
/// been read, so the declaration order the server intended is not available
/// here; name order is the one order stable across every reading of the same
/// schema.
fn elicitation_fields(schema: &Value) -> Result<Vec<ElicitationField>, String> {
    let root = schema.as_object().ok_or("form schema is not an object")?;
    if schema["type"] != "object" {
        return Err("form schema must describe an object".into());
    }
    for key in root.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "properties"
                | "required"
                | "title"
                | "description"
                | "$schema"
                | "additionalProperties"
        ) {
            return Err(format!("unsupported form schema keyword: {key}"));
        }
    }
    if root
        .get("additionalProperties")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("additional property schemas are not supported".into());
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("form schema has no properties object")?;
    let required = match root.get("required") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().ok_or("required field name is not text"))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("required fields must be an array".into()),
    };
    if required.iter().any(|name| !properties.contains_key(*name)) {
        return Err("required field has no property schema".into());
    }
    properties
        .iter()
        .map(|(name, property)| {
            let object = property
                .as_object()
                .ok_or_else(|| format!("{name}: property schema is not an object"))?;
            for key in object.keys() {
                if !matches!(
                    key.as_str(),
                    "type" | "title" | "description" | "default" | "enum"
                ) {
                    return Err(format!("{name}: unsupported field keyword: {key}"));
                }
            }
            let kind = match property["type"].as_str() {
                Some("string") => ElicitationFieldKind::String,
                Some("number") => ElicitationFieldKind::Number,
                Some("integer") => ElicitationFieldKind::Integer,
                Some("boolean") => ElicitationFieldKind::Boolean,
                _ => {
                    return Err(format!(
                        "{name}: only text, number, boolean and enum fields are supported"
                    ));
                }
            };
            let accepts = |value: &Value| match kind {
                ElicitationFieldKind::String => value.is_string(),
                ElicitationFieldKind::Number => value.is_number(),
                ElicitationFieldKind::Integer => value.is_i64() || value.is_u64(),
                ElicitationFieldKind::Boolean => value.is_boolean(),
                ElicitationFieldKind::Enum(_) => unreachable!(),
            };
            let choices = match object.get("enum") {
                None => None,
                Some(Value::Array(values)) if !values.is_empty() && values.iter().all(&accepts) => {
                    Some(values.clone())
                }
                Some(_) => return Err(format!("{name}: enum choices do not match the field type")),
            };
            let default = object.get("default").cloned();
            if default.as_ref().is_some_and(|value| {
                !accepts(value)
                    || choices
                        .as_ref()
                        .is_some_and(|choices| !choices.contains(value))
            }) {
                return Err(format!("{name}: default does not match the field"));
            }
            Ok(ElicitationField {
                name: name.clone(),
                title: ask_text(property, "title"),
                description: ask_text(property, "description"),
                required: required.contains(&name.as_str()),
                kind: choices.map(ElicitationFieldKind::Enum).unwrap_or(kind),
                default,
            })
        })
        .collect()
}

fn ask_text(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(str::to_owned)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Unavailable,
    Replaying,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionInput {
    pub exited: bool,
    pub stale: bool,
    pub stream: StreamState,
    pub gap: bool,
    pub turn: TurnState,
    pub ask: Option<(u64, AskWhy)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionObservation {
    Exited,
    Replaying,
    Unknown,
    Idle,
    Working,
    Finished,
    Errored,
    Interrupted,
    AskPending { id: u64, why: AskWhy },
}

pub fn observe_attention(input: AttentionInput) -> AttentionObservation {
    if input.exited {
        return AttentionObservation::Exited;
    }
    match input.stream {
        // A layer with no usable stream has observed provider state but cannot
        // claim any of it is current. The UI reserves `Unavailable` for there
        // being no SDK layer at all.
        StreamState::Unavailable => return AttentionObservation::Unknown,
        StreamState::Replaying => return AttentionObservation::Replaying,
        StreamState::Live => {}
    }
    if input.stale || input.gap {
        AttentionObservation::Unknown
    } else if let Some((id, why)) = input.ask {
        AttentionObservation::AskPending { id, why }
    } else {
        match input.turn {
            TurnState::Unknown => AttentionObservation::Unknown,
            TurnState::Idle => AttentionObservation::Idle,
            TurnState::Working => AttentionObservation::Working,
            TurnState::Finished => AttentionObservation::Finished,
            TurnState::Errored => AttentionObservation::Errored,
            TurnState::Interrupted => AttentionObservation::Interrupted,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFacts {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub models: Vec<ModelFact>,
    pub permission_mode: Option<String>,
    pub context: Option<ContextMeter>,
    pub mcp_servers: Vec<McpServerFact>,
    pub slash_commands: Vec<String>,
    pub terminal_slash_commands: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    tasks: VecDeque<TaskEntry>,
    task_launches: VecDeque<TaskLaunch>,
    todos: ClaudeTodos,
    cursor: u64,
    session: SessionFacts,
    context_breakdown: Option<Box<ContextUsage>>,
    turn: TurnState,
    gap: bool,
    interrupted: bool,
}

// Keeps the extracted row fold readable while its public facade uses the
// provider-neutral name above.
type ClaudeSdkLayer = Observation;

impl Observation {
    pub fn tasks(&self) -> impl Iterator<Item = &TaskEntry> {
        self.tasks.iter()
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn session(&self) -> &SessionFacts {
        &self.session
    }
    pub fn context_breakdown(&self) -> Option<&ContextUsage> {
        self.context_breakdown.as_deref()
    }
    pub fn todos(&self) -> Option<&crate::claude_pty::TaskList> {
        self.todos.current()
    }
    pub fn turn(&self) -> TurnState {
        self.turn
    }
    pub fn has_gap(&self) -> bool {
        self.gap
    }

    pub fn begin_window(&mut self, _truncated: bool) {
        *self = Self::default();
    }

    /// Restore only the cursor-relative condition carried by a durable tip.
    /// Feed entries remain owned by the store-backed window.
    pub fn restore_condition(&mut self, cursor: u64, turn: TurnState) {
        self.cursor = cursor;
        self.turn = turn;
    }

    pub fn observe(&mut self, seq: u64, row: &Value) {
        self.cursor = self.cursor.max(seq);
        if row["parent_tool_use_id"].is_null() && row["type"].as_str() == Some("conversation_reset")
        {
            self.todos = ClaudeTodos::default();
        }
        observe_session(self, row);
        observe_turn(self, row);
        observe_running_facts(self, row);
    }
}

fn observe_session(layer: &mut Observation, row: &Value) {
    if !row["parent_tool_use_id"].is_null() {
        return;
    }
    match row["type"].as_str().unwrap_or("") {
        "amux.claude_sdk.session_facts" => {
            layer.session.model = row["model"].as_str().map(str::to_owned);
            layer.session.effort = row["effort"].as_str().map(str::to_owned);
            layer.session.models =
                serde_json::from_value(row["models"].clone()).unwrap_or_default();
            layer.session.terminal_slash_commands =
                serde_json::from_value(row["terminal_slash_commands"].clone()).unwrap_or_default();
            layer.session.slash_commands =
                serde_json::from_value(row["slash_commands"].clone()).unwrap_or_default();
            layer.session.permission_mode = row["permission_mode"].as_str().map(str::to_owned);
            layer.session.context = serde_json::from_value(row["context"].clone()).ok();
            layer.session.mcp_servers =
                serde_json::from_value(row["mcp_servers"].clone()).unwrap_or_default();
        }
        "system" if row["subtype"] == "init" => {
            layer.session.model = row["model"].as_str().map(str::to_owned);
            layer.session.permission_mode = row["permissionMode"].as_str().map(str::to_owned);
            layer.session.mcp_servers =
                serde_json::from_value(row["mcp_servers"].clone()).unwrap_or_default();
            layer.session.slash_commands =
                serde_json::from_value(row["slash_commands"].clone()).unwrap_or_default();
            layer.session.terminal_slash_commands =
                serde_json::from_value(row["terminal_slash_commands"].clone()).unwrap_or_default();
        }
        "amux.claude_sdk.context_breakdown" => {
            layer.context_breakdown = serde_json::from_value(row["usage"].clone()).ok();
        }
        "amux.claude_sdk.ready" => {
            layer.session = SessionFacts::default();
            layer.context_breakdown = None;
        }
        "conversation_reset" => {
            layer.session.context = None;
            layer.context_breakdown = None;
        }
        "system" if row["subtype"] == "compact_boundary" => {
            layer.session.context = None;
            layer.context_breakdown = None;
        }
        _ => {}
    }
}

fn observe_turn(layer: &mut Observation, row: &Value) {
    let parent = !row["parent_tool_use_id"].is_null();
    match row["type"].as_str().unwrap_or("") {
        "amux.claude_sdk.ready" | "conversation_reset" => {
            layer.turn = TurnState::Idle;
            layer.gap = false;
            layer.interrupted = false;
        }
        "amux.claude_sdk.gap" => {
            layer.gap = true;
            layer.turn = TurnState::Unknown;
        }
        "result" if !parent => {
            layer.turn = if layer.interrupted {
                TurnState::Interrupted
            } else if row["is_error"] == true {
                TurnState::Errored
            } else {
                TurnState::Finished
            };
            layer.interrupted = false;
        }
        "assistant" if !parent && row["message"]["id"].is_string() => working(layer),
        "stream_event"
            if !parent
                && row["event"]["type"] == "message_start"
                && row["event"]["message"]["id"].is_string() =>
        {
            working(layer)
        }
        "user" if !parent => {
            let content = &row["message"]["content"];
            let texts: Vec<_> = content
                .as_str()
                .into_iter()
                .chain(
                    content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| block["text"].as_str()),
                )
                .collect();
            if texts.contains(&"[Request interrupted by user]") {
                layer.interrupted = true;
            } else if !texts.is_empty() && row["isSynthetic"] != true && row["isReplay"] != true {
                working(layer);
            }
        }
        "amux.claude_sdk.permission_required"
        | "amux.claude_sdk.elicitation_required"
        | "amux.claude_sdk.dialog_required" => working(layer),
        "system" if row["subtype"] == "status" && row["status"] == "compacting" => working(layer),
        _ => {}
    }
}

fn working(layer: &mut Observation) {
    layer.turn = TurnState::Working;
    layer.interrupted = false;
}

fn observe_running_facts(layer: &mut ClaudeSdkLayer, row: &Value) {
    match row["type"].as_str() {
        Some("assistant") => observe_assistant_facts(layer, row),
        Some("user") => observe_user_facts(layer, row),
        Some("system") => observe_system_facts(layer, row),
        _ => {}
    }
}

fn observe_assistant_facts(layer: &mut ClaudeSdkLayer, row: &Value) {
    let parent = row["parent_tool_use_id"].as_str();
    let Some(blocks) = row["message"]["content"].as_array() else {
        return;
    };
    for block in blocks {
        if parent.is_none() && matches!(layer.todos.observe(block), TodoDisposition::Absorbed) {
            continue;
        }
        if !matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use")) {
            continue;
        }
        let (Some(tool_use_id), Some(name)) = (id(block, "id"), block["name"].as_str()) else {
            continue;
        };
        if !matches!(name, "Task" | "Agent") {
            continue;
        }
        let input = &block["input"];
        let input_oversized = input.to_string().len() > CONTENT_BYTES_RETAINED;
        layer
            .task_launches
            .retain(|launch| launch.tool_use_id != tool_use_id);
        layer.task_launches.push_back(TaskLaunch {
            tool_use_id,
            description: (!input_oversized)
                .then(|| string(input, "description"))
                .flatten(),
            subagent_type: (!input_oversized)
                .then(|| string(input, "subagent_type"))
                .flatten(),
        });
        if layer.task_launches.len() > TASK_LAUNCHES_RETAINED {
            layer.task_launches.pop_front();
        }
    }
}

fn observe_user_facts(layer: &mut ClaudeSdkLayer, row: &Value) {
    if !row["parent_tool_use_id"].is_null() {
        return;
    }
    for block in row["message"]["content"].as_array().into_iter().flatten() {
        if block["type"] == "tool_result" {
            layer.todos.observe(block);
        }
    }
}

fn observe_system_facts(layer: &mut ClaudeSdkLayer, row: &Value) {
    match row["subtype"].as_str() {
        Some("task_started" | "task_progress" | "task_updated" | "task_notification") => {
            task(layer, row)
        }
        Some("background_tasks_changed") => {
            if let Some(tasks) = row["tasks"].as_array() {
                for row in tasks {
                    task(layer, row);
                }
            }
        }
        _ => {}
    }
}

fn task(layer: &mut ClaudeSdkLayer, row: &Value) {
    let Some(task_id) = id(row, "task_id") else {
        return;
    };
    let tool_use_id = id(row, "tool_use_id");
    let index = layer.tasks.iter().position(|task| task.task_id == task_id);
    if index.is_none() {
        layer
            .tasks
            .push_back(new_task(task_id.clone(), tool_use_id.clone()));
        if layer.tasks.len() > TASKS_RETAINED {
            layer.tasks.pop_front();
        }
    }
    let Some(task) = layer.tasks.iter_mut().find(|task| task.task_id == task_id) else {
        return;
    };
    if task.tool_use_id.is_none() {
        task.tool_use_id = tool_use_id.clone();
    }
    if let Some(launch) = tool_use_id.as_ref().and_then(|tool_use_id| {
        layer
            .task_launches
            .iter()
            .find(|launch| &launch.tool_use_id == tool_use_id)
    }) {
        if task.description.is_empty()
            && let Some(description) = &launch.description
        {
            task.description = description.clone();
        }
        if task.subagent_type.is_none() {
            task.subagent_type = launch.subagent_type.clone();
        }
    }
    let fields = if row["subtype"] == "task_updated" {
        &row["patch"]
    } else {
        row
    };
    if let Some(description) = string(fields, "description") {
        task.description = description;
    }
    if let Some(subagent) = string(fields, "subagent_type") {
        task.subagent_type = Some(subagent);
    }
    if let Some(tool) = string(fields, "last_tool_name").or_else(|| string(fields, "last_tool")) {
        task.last_tool = Some(tool);
    }
    if let Some(summary) = string(fields, "summary") {
        task.summary = Some(summary);
    }
    if let Some(state) = string(fields, "status") {
        task.state = match state.as_str() {
            "running" | "in_progress" | "pending" => TaskState::Running,
            "completed" => TaskState::Completed,
            "failed" => TaskState::Failed,
            "stopped" | "killed" => TaskState::Stopped,
            _ => TaskState::Unknown(state),
        };
    }
    if let Some(usage) = fields.get("usage").filter(|v| v.is_object()) {
        task.usage = Some(TaskUsage {
            total_tokens: usage["total_tokens"].as_u64(),
            tool_uses: usage["tool_uses"].as_u64(),
            duration_ms: usage["duration_ms"].as_u64(),
        });
    }
}

fn new_task(task_id: String, tool_use_id: Option<String>) -> TaskEntry {
    TaskEntry {
        task_id,
        tool_use_id,
        description: String::new(),
        subagent_type: None,
        state: TaskState::Running,
        last_tool: None,
        summary: None,
        usage: None,
    }
}

fn id(value: &Value, field: &str) -> Option<String> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= ID_BYTES_RETAINED)
        .map(str::to_owned)
}

fn string(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(clipped)
}

/// Whether an error string the session collected is a diagnostic it
/// wrote for its own authors rather than a sentence for the person at
/// the keyboard.
///
/// The provider mixes both into one list: `Reached maximum number of
/// turns (1)` explains itself, while `[ede_diagnostic] result_type=user
/// last_content_type=n/a stop_reason=null` is a tag followed by internal
/// key/value pairs and tells a reader nothing about their own turn. The
/// shape is the tell — every token after an optional bracketed tag is a
/// `key=value` pair with no spaces — so prose is never mistaken for one.
pub fn is_internal_diagnostic(error: &str) -> bool {
    let rest = match error.trim().strip_prefix('[') {
        Some(tagged) => match tagged.split_once(']') {
            Some((_, rest)) => rest,
            None => return false,
        },
        None => error.trim(),
    };
    let mut tokens = rest.split_whitespace().peekable();
    tokens.peek().is_some()
        && tokens.all(|token| {
            token
                .split_once('=')
                .is_some_and(|(key, _)| !key.is_empty() && !key.contains(':'))
        })
}

fn clipped(text: &str) -> String {
    let mut out = String::new();
    append(&mut out, text);
    out
}

fn append(out: &mut String, text: &str) -> bool {
    let mut take = CONTENT_BYTES_RETAINED
        .saturating_sub(out.len())
        .min(text.len());
    while !text.is_char_boundary(take) {
        take -= 1;
    }
    out.push_str(&text[..take]);
    take < text.len()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn observation_tracks_session_blocks_tasks_and_todos_without_ui_state() {
        let mut observation = Observation::default();
        observation.observe(
            1,
            &json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":false}),
        );
        observation.observe(
            2,
            &json!({
                "type":"amux.claude_sdk.session_facts",
                "model":"claude-test",
                "permission_mode":"default"
            }),
        );
        observation.observe(
            3,
            &json!({
                "type":"stream_event",
                "event":{"type":"message_start","message":{"id":"m"}}
            }),
        );
        observation.observe(
            4,
            &json!({
                "type":"stream_event",
                "event":{"type":"content_block_start","index":0,
                    "content_block":{"type":"text","text":"hel"}}
            }),
        );
        observation.observe(
            5,
            &json!({
                "type":"stream_event",
                "event":{"type":"content_block_delta","index":0,
                    "delta":{"type":"text_delta","text":"lo"}}
            }),
        );
        observation.observe(
            6,
            &json!({
                "type":"assistant","uuid":"row-message",
                "message":{"id":"m","content":[{"type":"text","text":"hello"}]}
            }),
        );
        observation.observe(
            7,
            &json!({
                "type":"system","subtype":"task_started","task_id":"task-1",
                "description":"Inspect the tree","status":"running"
            }),
        );
        observation.observe(
            8,
            &json!({
                "type":"assistant","uuid":"row-todo",
                "message":{"id":"todo-message","content":[{
                    "type":"tool_use","id":"todo-1","name":"TodoWrite",
                    "input":{"todos":[{"content":"Ship it","activeForm":"Shipping it","status":"in_progress"}]}
                }]}
            }),
        );
        observation.observe(
            9,
            &json!({
                "type":"user","message":{"content":[{
                    "type":"tool_result","tool_use_id":"todo-1","content":"ok","is_error":false
                }]}
            }),
        );

        assert_eq!(observation.cursor(), 9);
        assert_eq!(observation.session().model.as_deref(), Some("claude-test"));
        assert_eq!(observation.tasks().next().unwrap().task_id, "task-1");
        assert_eq!(
            observation.todos().unwrap().current.as_deref(),
            Some("Shipping it")
        );
        observation.observe(
            10,
            &json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":true}),
        );
        assert_eq!(
            observation.todos().unwrap().current.as_deref(),
            Some("Shipping it")
        );
        observation.observe(11, &json!({"type":"conversation_reset"}));
        assert!(observation.todos().is_none());
    }

    #[test]
    fn ask_and_attention_observers_are_reducer_independent() {
        let row = json!({
            "type":"amux.claude_sdk.permission_required",
            "request_id":"permission-1",
            "tool_name":"Write",
            "input":{"file_path":"src/lib.rs","content":"new"},
            "suggestions":[]
        });
        let Some(AskObservation::Required {
            request_id, kind, ..
        }) = observe_ask(&row)
        else {
            panic!("permission row should produce an ask");
        };
        assert_eq!(request_id, "permission-1");
        assert!(matches!(kind, AskKind::Permission { .. }));
        assert_eq!(
            observe_attention(AttentionInput {
                exited: false,
                stale: false,
                stream: StreamState::Live,
                gap: false,
                turn: TurnState::Working,
                ask: Some((4, AskWhy::Permission)),
            }),
            AttentionObservation::AskPending {
                id: 4,
                why: AskWhy::Permission,
            }
        );
    }
}
