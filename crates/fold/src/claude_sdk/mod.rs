//! Claude SDK stream observation shared by clients and the daemon. Provider
//! block identity and task lifecycle are preserved independently of the
//! terminal transcript's inferred turns.
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
    ToolInvocation, inbound_message, invocation, landed_edit,
};
use crate::claude_pty::{ClaudeTodos, TodoDisposition};

mod semantics;

pub use semantics::{
    ClaudeSdkBody, ClaudeSdkEntry, ClaudeSdkEntryKind, ClaudeSdkFold, ClaudeSdkPartial,
    DELIVERY_KEYED_VARIANTS,
};

pub const FEED_RETAINED: usize = 1000;
/// A single streaming block cannot grow without bound while the feed is idle.
pub const CONTENT_BYTES_RETAINED: usize = 64 * 1024;
const ID_BYTES_RETAINED: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub id: u64,
    pub seq: u64,
    pub kind: FeedEntryKind,
    /// The provider message and block this entry represents, when applicable.
    pub block: Option<BlockId>,
    /// The tool use whose subagent produced this entry, when it was not the
    /// session's own. Stream-JSON carries a subagent's rows on the parent's
    /// stream with this id set. Kept apart from `block` so a row that arrives
    /// without its block — a result-only tail — still says whose it was.
    pub parent_tool_use_id: Option<String>,
    /// Payload clipping is separate from missing earlier feed entries.
    pub content_truncated: bool,
    final_row_id: Option<String>,
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
            block: None,
            parent_tool_use_id,
            content_truncated,
            final_row_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockId {
    pub message_id: String,
    pub parent_tool_use_id: Option<String>,
    pub index: u64,
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
struct MessageCursor {
    message_id: String,
    parent_tool_use_id: Option<String>,
    next_final_index: u64,
    streaming: bool,
    placeholder_entry_id: Option<u64>,
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
    Unavailable,
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
    entries: VecDeque<FeedEntry>,
    next_entry_id: u64,
    evicted: u64,
    truncated_start: bool,
    cursors: VecDeque<MessageCursor>,
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
    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }
    pub fn entries_deque(&self) -> &VecDeque<FeedEntry> {
        &self.entries
    }
    pub fn tasks(&self) -> impl Iterator<Item = &TaskEntry> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            FeedEntryKind::Task(task) => Some(task),
            _ => None,
        })
    }
    pub fn entry_count(&self) -> usize {
        self.entries.len()
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
    pub fn history_truncated(&self) -> bool {
        self.truncated_start || self.evicted > 0
    }
    pub fn evicted_entries(&self) -> u64 {
        self.evicted
    }
    pub fn turn(&self) -> TurnState {
        self.turn
    }
    pub fn has_gap(&self) -> bool {
        self.gap
    }

    pub fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            truncated_start: truncated,
            ..Self::default()
        };
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
        observe(self, seq, row);
    }

    pub fn interrupt_streams(&mut self) {
        for entry in &mut self.entries {
            if let Some(finality) = finality_mut(&mut entry.kind)
                && matches!(finality, Finality::Streaming | Finality::Stopped)
            {
                *finality = Finality::Interrupted;
            }
        }
        for cursor in &mut self.cursors {
            cursor.streaming = false;
        }
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

fn finality_mut(kind: &mut FeedEntryKind) -> Option<&mut Finality> {
    match kind {
        FeedEntryKind::Message(entry) => Some(&mut entry.finality),
        FeedEntryKind::Thinking(entry) => Some(&mut entry.finality),
        FeedEntryKind::Tool(entry) => Some(&mut entry.finality),
        _ => None,
    }
}

fn observe(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let kind = row["type"].as_str().unwrap_or("<missing type>");
    match kind {
        "assistant" => assistant(layer, seq, row),
        "stream_event" => stream(layer, seq, row),
        "user" => user(layer, seq, row),
        "result" => {
            layer.interrupt_streams();
            let usage = &row["usage"];
            push(
                layer,
                seq,
                FeedEntryKind::Turn(TurnEntry {
                    uuid: id(row, "uuid"),
                    outcome: string(row, "subtype").unwrap_or_else(|| "unknown".into()),
                    is_error: row["is_error"].as_bool().unwrap_or(false),
                    stop_reason: string(row, "stop_reason"),
                    result: string(row, "result"),
                    errors: row["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(64)
                        .filter_map(Value::as_str)
                        .filter(|error| !is_internal_diagnostic(error))
                        .map(clipped)
                        .collect(),
                    usage: TokenUsage {
                        input_tokens: usage["input_tokens"].as_u64(),
                        output_tokens: usage["output_tokens"].as_u64(),
                        cache_read_input_tokens: usage["cache_read_input_tokens"].as_u64(),
                        cache_creation_input_tokens: usage["cache_creation_input_tokens"].as_u64(),
                    },
                    model_usage: bounded_value(&row["modelUsage"]),
                    total_cost_usd: row["total_cost_usd"].as_f64(),
                    duration_ms: row["duration_ms"].as_u64(),
                    duration_api_ms: row["duration_api_ms"].as_u64(),
                    num_turns: row["num_turns"].as_u64(),
                }),
                oversized(row),
            );
        }
        "amux.claude_sdk.ready" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::Ready {
                    session_id: id(row, "session_id"),
                    resumed: row["resumed"] == true,
                }),
                false,
            );
        }
        "amux.claude_sdk.gap" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            layer.truncated_start = true;
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::Gap {
                    resumed_session_id: id(row, "resumed_session_id"),
                }),
                false,
            );
        }
        "conversation_reset" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::ConversationReset {
                    conversation_id: id(row, "new_conversation_id"),
                }),
                false,
            );
        }
        "amux.claude_sdk.message" => agent_message(layer, seq, row),
        "system" => system(layer, seq, row),
        "rate_limit_event" | "tool_progress" | "tool_use_summary" | "auth_status" => {
            status(layer, seq, kind, row);
        }
        // These have their own session/ask/write state, separate from content.
        "amux.claude_sdk.session_facts"
        | "amux.claude_sdk.context_breakdown"
        | "amux.claude_sdk.permission_required"
        | "amux.claude_sdk.permission_resolved"
        | "amux.claude_sdk.elicitation_required"
        | "amux.claude_sdk.elicitation_resolved"
        | "amux.claude_sdk.dialog_required"
        | "amux.claude_sdk.dialog_resolved"
        | "amux.attachments" => {}
        "amux.claude_sdk.input_result" => {
            if row["outcome"] != "ok" {
                status(layer, seq, "input_error", row);
            }
        }
        _ => unknown(layer, seq, kind, "row is not recognized"),
    }
}

fn assistant(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let message = &row["message"];
    let Some(message_id) = id(message, "id") else {
        unknown(layer, seq, "assistant", "missing message id");
        return;
    };
    let Some(blocks) = message["content"].as_array() else {
        unknown(layer, seq, "assistant", "missing content blocks");
        return;
    };
    let row_id = id(row, "uuid");
    if row_id.is_some() && layer.entries.iter().any(|e| e.final_row_id == row_id) {
        return;
    }
    let parent = id(row, "parent_tool_use_id");
    let cursor = cursor(layer, &message_id, &parent);
    // Claude emits one final row per block. A whole-message snapshot uses
    // array indices instead, so siblings never overwrite one another.
    let start = if blocks.len() > 1 {
        0
    } else {
        layer.cursors[cursor].next_final_index
    };
    let mut next = start;
    for (offset, block) in blocks.iter().enumerate() {
        let index = if blocks.len() == 1 {
            // A replay tail may start at block 1 or later. Match the retained
            // stream block instead of assigning the absent block 0 to it.
            layer
                .entries
                .iter()
                .filter(|e| e.final_row_id.is_none())
                .filter(|e| block_matches(&e.kind, block))
                .filter_map(|e| e.block.as_ref())
                .filter(|b| {
                    b.message_id == message_id && b.parent_tool_use_id == parent && b.index >= start
                })
                .map(|b| b.index)
                .min()
                .unwrap_or(start)
        } else {
            offset as u64
        };
        next = index + 1;
        let key = BlockId {
            message_id: message_id.clone(),
            parent_tool_use_id: parent.clone(),
            index,
        };
        upsert_block(layer, seq, key, block, Finality::Complete, row_id.clone());
    }
    layer.cursors[cursor].next_final_index = next;
}

fn stream(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let event = &row["event"];
    let parent = id(row, "parent_tool_use_id");
    let kind = event["type"].as_str().unwrap_or("<missing event type>");
    if kind == "message_start" {
        let Some(message_id) = id(&event["message"], "id") else {
            unknown(layer, seq, kind, "missing message id");
            return;
        };
        // A parent and each subagent have independent streaming channels.
        for old in &mut layer.cursors {
            if old.parent_tool_use_id == parent {
                old.streaming = false;
            }
        }
        let index = cursor(layer, &message_id, &parent);
        layer.cursors[index].streaming = true;
        let key = BlockId {
            message_id,
            parent_tool_use_id: parent,
            index: 0,
        };
        if !layer.entries.iter().any(|e| e.block.as_ref() == Some(&key)) {
            upsert_block(
                layer,
                seq,
                key,
                &serde_json::json!({"type":"text","text":""}),
                Finality::Streaming,
                None,
            );
            layer.cursors[index].placeholder_entry_id = layer.entries.back().map(|e| e.id);
        }
        return;
    }
    let Some(cursor) = layer
        .cursors
        .iter()
        .rposition(|c| c.streaming && c.parent_tool_use_id == parent)
    else {
        unknown(layer, seq, kind, "stream start is unavailable");
        return;
    };
    let message_id = layer.cursors[cursor].message_id.clone();
    match kind {
        "content_block_start" => {
            let Some(index) = event["index"].as_u64() else {
                unknown(layer, seq, kind, "missing block index");
                return;
            };
            if let Some(placeholder) = layer.cursors[cursor].placeholder_entry_id.take()
                && let Some(entry) = layer
                    .entries
                    .iter_mut()
                    .find(|e| e.id == placeholder && e.final_row_id.is_none())
                && let Some(block) = &mut entry.block
            {
                block.index = index;
            }
            upsert_block(
                layer,
                seq,
                BlockId {
                    message_id,
                    parent_tool_use_id: parent,
                    index,
                },
                &event["content_block"],
                Finality::Streaming,
                None,
            );
        }
        "content_block_delta" | "content_block_stop" => {
            let Some(index) = event["index"].as_u64() else {
                unknown(layer, seq, kind, "missing block index");
                return;
            };
            let key = BlockId {
                message_id,
                parent_tool_use_id: parent,
                index,
            };
            let Some(entry) = layer
                .entries
                .iter_mut()
                .find(|e| e.block.as_ref() == Some(&key))
            else {
                unknown(layer, seq, kind, "block start is unavailable");
                return;
            };
            let Some(finality) = finality_mut(&mut entry.kind) else {
                return;
            };
            if matches!(finality, Finality::Complete | Finality::Interrupted) {
                return;
            }
            if kind == "content_block_stop" {
                *finality = Finality::Stopped;
                return;
            }
            let delta = &event["delta"];
            let delta_type = delta["type"].as_str().unwrap_or("");
            let target = match (&mut entry.kind, delta_type) {
                (FeedEntryKind::Message(m), "text_delta") => Some((&mut m.text, "text")),
                (FeedEntryKind::Thinking(t), "thinking_delta") => Some((&mut t.text, "thinking")),
                (FeedEntryKind::Tool(t), "input_json_delta") => {
                    Some((&mut t.input_json, "partial_json"))
                }
                (FeedEntryKind::Thinking(_), "signature_delta") => return,
                _ => None,
            };
            if let Some((text, field)) = target
                && let Some(part) = delta[field].as_str()
            {
                entry.content_truncated |= append(text, part);
            } else {
                unknown(layer, seq, kind, "unrecognized or mismatched block delta");
            }
        }
        "message_stop" => {
            for entry in &mut layer.entries {
                if entry
                    .block
                    .as_ref()
                    .is_some_and(|b| b.message_id == message_id && b.parent_tool_use_id == parent)
                    && let Some(finality) = finality_mut(&mut entry.kind)
                    && *finality == Finality::Streaming
                {
                    *finality = Finality::Stopped;
                }
            }
            // Keep the cursor to ignore late deltas against completed blocks.
        }
        "message_delta" => {}
        _ => unknown(layer, seq, kind, "unrecognized stream event"),
    }
}

/// The entry this block follows: the one before it where it already
/// sits, otherwise the feed's last entry, since a new block is appended.
/// Only an entry from the same context counts — a subagent's last read
/// and the session's next one are adjacent on the stream, but they are
/// not one exploration.
fn predecessor<'a>(
    layer: &'a ClaudeSdkLayer,
    existing: Option<usize>,
    parent: &Option<String>,
) -> Option<&'a FeedEntry> {
    let index = match existing {
        Some(index) => index.checked_sub(1)?,
        None => layer.entries.len().checked_sub(1)?,
    };
    let entry = layer.entries.get(index)?;
    (entry.parent_tool_use_id() == parent.as_deref()).then_some(entry)
}

fn block_matches(kind: &FeedEntryKind, block: &Value) -> bool {
    match kind {
        FeedEntryKind::Message(_) => block["type"] == "text",
        FeedEntryKind::Thinking(_) => matches!(
            block["type"].as_str(),
            Some("thinking" | "redacted_thinking")
        ),
        FeedEntryKind::Tool(tool) => block["id"].as_str() == Some(tool.tool_use_id.as_str()),
        // The launch row that became its task still answers for its block.
        FeedEntryKind::Task(task) => block["id"].as_str() == task.tool_use_id.as_deref(),
        _ => false,
    }
}

fn upsert_block(
    layer: &mut ClaudeSdkLayer,
    seq: u64,
    key: BlockId,
    block: &Value,
    finality: Finality,
    row_id: Option<String>,
) {
    let mut existing = layer
        .entries
        .iter()
        .position(|e| e.block.as_ref() == Some(&key));
    // A replay tail can start at the task rows, so the task may stand
    // without the block that launched it. The block arriving now is that
    // launch: attach it to the task rather than adding a tool beside it.
    if existing.is_none()
        && block["type"] == "tool_use"
        && let Some(tool_use_id) = block["id"].as_str()
        && let Some(index) = layer.entries.iter().position(|e| {
            e.block.is_none()
                && matches!(&e.kind, FeedEntryKind::Task(t) if t.tool_use_id.as_deref() == Some(tool_use_id))
        })
    {
        layer.entries[index].block = Some(key.clone());
        layer.entries[index].parent_tool_use_id = key.parent_tool_use_id.clone();
        existing = Some(index);
    }
    if finality != Finality::Complete
        && existing.is_some_and(|i| {
            matches!(
                &layer.entries[i].kind,
                FeedEntryKind::Message(MessageEntry {
                    finality: Finality::Complete,
                    ..
                }) | FeedEntryKind::Thinking(ThinkingEntry {
                    finality: Finality::Complete,
                    ..
                }) | FeedEntryKind::Tool(ToolEntry {
                    finality: Finality::Complete,
                    ..
                })
            )
        })
    {
        return;
    }
    if key.parent_tool_use_id.is_none()
        && finality == Finality::Complete
        && matches!(layer.todos.observe(block), TodoDisposition::Absorbed)
    {
        if let Some(index) = existing {
            layer.entries.remove(index);
        }
        return;
    }
    let kind = match block["type"].as_str() {
        Some("text") if block["text"].is_string() => FeedEntryKind::Message(MessageEntry {
            text: string(block, "text").unwrap_or_default(),
            finality,
        }),
        Some("thinking" | "redacted_thinking") => FeedEntryKind::Thinking(ThinkingEntry {
            text: string(block, "thinking").unwrap_or_default(),
            redacted: block["type"] == "redacted_thinking",
            finality,
        }),
        Some("tool_use" | "server_tool_use") => {
            let (Some(tool_use_id), Some(name)) = (id(block, "id"), id(block, "name")) else {
                unknown(layer, seq, "assistant.tool_use", "missing tool id or name");
                return;
            };
            let input = bounded_value(&block["input"]);
            let invocation = invocation(&name, input.as_ref().unwrap_or(&Value::Null));
            // A tool's classification comes from its name, so a block
            // still streaming its input already knows whether it explores.
            // Recomputed on every upsert: the entry ahead of this one may
            // have arrived, or been rewritten, since the block opened.
            let group_with_previous = invocation.is_exploration()
                && matches!(
                    predecessor(layer, existing, &key.parent_tool_use_id).map(|entry| &entry.kind),
                    Some(FeedEntryKind::Tool(previous)) if previous.invocation.is_exploration()
                );
            FeedEntryKind::Tool(ToolEntry {
                tool_use_id,
                name: name.clone(),
                invocation,
                input,
                input_json: String::new(),
                finality,
                result: existing.and_then(|i| match &layer.entries[i].kind {
                    FeedEntryKind::Tool(t) => t.result.clone(),
                    _ => None,
                }),
                group_with_previous,
            })
        }
        _ => {
            unknown(
                layer,
                seq,
                "assistant.content",
                "unrecognized content block",
            );
            return;
        }
    };
    if let Some(index) = existing {
        let entry = &mut layer.entries[index];
        // A launch row that has already become its task stays the task:
        // the lifecycle rows own it from the first one onward, and a late
        // final row for the tool block adds nothing they do not state.
        if matches!(entry.kind, FeedEntryKind::Task(_)) {
            // The task's own rows may already have been clipped; the
            // launch block's size does not undo that.
            entry.content_truncated |= oversized(block);
        } else {
            entry.kind = kind;
            entry.content_truncated = oversized(block);
        }
        entry.final_row_id = row_id;
    } else {
        push(layer, seq, kind, oversized(block));
        let entry = layer.entries.back_mut().expect("just pushed");
        entry.parent_tool_use_id = key.parent_tool_use_id.clone();
        entry.block = Some(key);
        entry.final_row_id = row_id;
    }
}

fn cursor(layer: &mut ClaudeSdkLayer, message_id: &str, parent: &Option<String>) -> usize {
    if let Some(index) = layer
        .cursors
        .iter()
        .position(|c| c.message_id == message_id && &c.parent_tool_use_id == parent)
    {
        return index;
    }
    if layer.cursors.len() == FEED_RETAINED {
        layer.cursors.pop_front();
    }
    layer.cursors.push_back(MessageCursor {
        message_id: message_id.into(),
        parent_tool_use_id: parent.clone(),
        next_final_index: 0,
        streaming: false,
        placeholder_entry_id: None,
    });
    layer.cursors.len() - 1
}

fn user(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let uuid = id(row, "uuid");
    if uuid.is_some() && layer.entries.iter().any(|e| e.final_row_id == uuid) {
        return;
    }
    let content = &row["message"]["content"];
    if !content.is_string() && !content.is_array() {
        unknown(layer, seq, "user", "missing message content");
        return;
    }
    let mut text = String::new();
    let mut images = 0;
    let mut truncated = false;
    if let Some(value) = content.as_str() {
        truncated |= append(&mut text, value);
    }
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                if !text.is_empty() {
                    truncated |= append(&mut text, "\n");
                }
                if let Some(value) = block["text"].as_str() {
                    truncated |= append(&mut text, value);
                }
            }
            Some("image") => images += 1,
            Some("tool_result") => tool_result(layer, seq, row, block),
            _ => unknown(layer, seq, "user.content", "unrecognized content block"),
        }
    }
    // A subagent's user rows are its tool results and the prompt its
    // parent gave it; the results pair with its tool entries above, and
    // the prompt is what the task block already states as its description.
    // Neither is something the person said.
    if !row["parent_tool_use_id"].is_null() {
        return;
    }
    if text.is_empty() && images == 0 {
        return;
    }
    let kind = if let Some(message) = inbound_message(&text) {
        FeedEntryKind::AgentMessage(AgentMessageEntry {
            id: message.id,
            context: message.context,
            from: message.from,
            kind: message.kind,
            text: message.text,
            delivery: None,
        })
    } else if text == "[Request interrupted by user]" {
        FeedEntryKind::Status(StatusEntry {
            status: text,
            details: None,
        })
    } else {
        FeedEntryKind::Prompt(PromptEntry {
            uuid: uuid.clone(),
            text,
            image_count: images,
            synthetic: row["isSynthetic"] == true,
            replay: row["isReplay"] == true,
        })
    };
    push(layer, seq, kind, truncated);
    layer.entries.back_mut().expect("just pushed").final_row_id = uuid;
}

fn tool_result(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value, block: &Value) {
    let Some(tool_id) = id(block, "tool_use_id") else {
        unknown(layer, seq, "user.tool_result", "missing tool id");
        return;
    };
    let parent = id(row, "parent_tool_use_id");
    let todo_failed = if parent.is_none() {
        match layer.todos.observe(block) {
            TodoDisposition::Absorbed => return,
            TodoDisposition::Failed => true,
            TodoDisposition::Other => false,
        }
    } else {
        false
    };
    let content = &block["content"];
    let mut text = String::new();
    let mut truncated = false;
    if let Some(value) = content.as_str() {
        truncated |= append(&mut text, value);
    }
    for value in content.as_array().into_iter().flatten() {
        if let Some(part) = value["text"].as_str() {
            if !text.is_empty() {
                truncated |= append(&mut text, "\n");
            }
            truncated |= append(&mut text, part);
        }
    }
    let details = bounded_value(&row["tool_use_result"]);
    truncated |= oversized(&row["tool_use_result"]);
    let result = ToolResult {
        text,
        is_error: block["is_error"] == true,
        details,
        edit: landed_edit(&row["tool_use_result"]),
    };
    if let Some(entry) = layer
        .entries
        .iter_mut()
        .rev()
        .find(|entry| match &entry.kind {
            FeedEntryKind::Tool(t) => {
                t.tool_use_id == tool_id
                    && entry
                        .block
                        .as_ref()
                        .is_none_or(|b| b.parent_tool_use_id == parent)
            }
            FeedEntryKind::Task(t) => t.tool_use_id.as_deref() == Some(tool_id.as_str()),
            _ => false,
        })
    {
        // A task's outcome is what its lifecycle rows report; the launch
        // tool's own result ("Agent launched.") adds nothing to it.
        if let FeedEntryKind::Tool(tool) = &mut entry.kind {
            tool.result = Some(result);
        }
        entry.content_truncated |= truncated;
    } else {
        // A tail may start at the result. Preserve it without inventing an
        // invocation, and keep whose it was: a subagent's result stays the
        // subagent's even with no launch block to say so.
        push(
            layer,
            seq,
            FeedEntryKind::Tool(ToolEntry {
                tool_use_id: tool_id,
                name: if todo_failed {
                    "TodoWrite".into()
                } else {
                    String::new()
                },
                invocation: ToolInvocation::Other,
                input: None,
                input_json: String::new(),
                finality: Finality::Complete,
                result: Some(result),
                group_with_previous: false,
            }),
            truncated,
        );
        layer
            .entries
            .back_mut()
            .expect("just pushed")
            .parent_tool_use_id = parent;
    }
}

fn system(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    match row["subtype"].as_str().unwrap_or("<missing subtype>") {
        "compact_boundary" => {
            let metadata = &row["compact_metadata"];
            push(
                layer,
                seq,
                FeedEntryKind::Compaction(CompactionEntry {
                    trigger: string(metadata, "trigger"),
                    pre_tokens: metadata["pre_tokens"].as_u64(),
                    post_tokens: metadata["post_tokens"].as_u64(),
                }),
                false,
            );
        }
        "task_started" | "task_progress" | "task_updated" | "task_notification" => {
            task(layer, seq, row)
        }
        "background_tasks_changed" => {
            if let Some(tasks) = row["tasks"].as_array() {
                for row in tasks {
                    task(layer, seq, row);
                }
            } else {
                unknown(
                    layer,
                    seq,
                    "system.background_tasks_changed",
                    "missing task list",
                );
            }
        }
        "status" => status(layer, seq, row["status"].as_str().unwrap_or("ready"), row),
        "init" | "thinking_tokens" => {}
        subtype => unknown(
            layer,
            seq,
            &format!("system.{subtype}"),
            "unrecognized system row",
        ),
    }
}

fn task(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let Some(task_id) = id(row, "task_id") else {
        unknown(layer, seq, "system.task", "missing task id");
        return;
    };
    let tool_use_id = id(row, "tool_use_id");
    let existing = layer
        .entries
        .iter()
        .position(|e| matches!(&e.kind, FeedEntryKind::Task(t) if t.task_id == task_id));
    // The `Task`/`Agent` tool use that launched this task is the same
    // subagent: the lifecycle rows take that row over where it sits,
    // starting from what the launch already said, so the feed shows one
    // entry per subagent rather than a launch and a task. The task list
    // row can name a task before any row carries its launch id, so a task
    // that already stands on its own moves into the launch row when the
    // id arrives.
    let launch = tool_use_id.as_ref().and_then(|tool_use_id| {
        layer.entries.iter().position(
            |e| matches!(&e.kind, FeedEntryKind::Tool(t) if &t.tool_use_id == tool_use_id),
        )
    });
    let index = match (existing, launch) {
        (Some(existing), None) => existing,
        (Some(existing), Some(launch)) => {
            let standalone = layer.entries.remove(existing).expect("indexed entry");
            let launch = if launch > existing {
                launch - 1
            } else {
                launch
            };
            let FeedEntryKind::Task(mut task) = standalone.kind else {
                unreachable!("existing task entry")
            };
            adopt_launch(&mut task, &layer.entries[launch].kind);
            layer.entries[launch].kind = FeedEntryKind::Task(task);
            layer.entries[launch].content_truncated |= standalone.content_truncated;
            launch
        }
        (None, Some(launch)) => {
            let mut task = new_task(task_id, tool_use_id.clone());
            adopt_launch(&mut task, &layer.entries[launch].kind);
            layer.entries[launch].kind = FeedEntryKind::Task(task);
            launch
        }
        (None, None) => {
            push(
                layer,
                seq,
                FeedEntryKind::Task(new_task(task_id, tool_use_id.clone())),
                false,
            );
            layer.entries.len() - 1
        }
    };
    let entry = &mut layer.entries[index];
    entry.content_truncated |= oversized(row);
    let FeedEntryKind::Task(task) = &mut entry.kind else {
        unreachable!()
    };
    if task.tool_use_id.is_none() {
        task.tool_use_id = tool_use_id;
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

/// What the launch already said about the subagent, where the task rows
/// have not said it yet.
fn adopt_launch(task: &mut TaskEntry, launch: &FeedEntryKind) {
    if let FeedEntryKind::Tool(tool) = launch
        && let ToolInvocation::Task {
            description,
            subagent_type,
            ..
        } = &tool.invocation
    {
        if task.description.is_empty()
            && let Some(description) = description
        {
            task.description = description.clone();
        }
        if task.subagent_type.is_none() {
            task.subagent_type = subagent_type.clone();
        }
    }
}

fn agent_message(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let envelope = &row["envelope"];
    let Some(text) = string(envelope, "text") else {
        unknown(
            layer,
            seq,
            "amux.claude_sdk.message",
            "missing envelope text",
        );
        return;
    };
    let message_id = id(envelope, "id");
    if message_id.is_some()
        && layer.entries.iter().any(|entry| {
            matches!(&entry.kind,
        FeedEntryKind::AgentMessage(m) if m.id == message_id)
        })
    {
        return;
    }
    let from = &envelope["from"];
    let sender = if from["type"] == "human" {
        "human".into()
    } else {
        string(from, "name")
            .or_else(|| id(from, "agent_id"))
            .or_else(|| from.as_str().map(clipped))
            .unwrap_or_else(|| "unknown".into())
    };
    let message_kind = string(envelope, "kind");
    push(
        layer,
        seq,
        FeedEntryKind::AgentMessage(AgentMessageEntry {
            id: message_id,
            context: id(envelope, "context"),
            from: sender,
            kind: AgentMessageKind::read(message_kind.as_deref()),
            text,
            delivery: string(row, "delivery"),
        }),
        oversized(row),
    );
}

fn status(layer: &mut ClaudeSdkLayer, seq: u64, name: &str, row: &Value) {
    push(
        layer,
        seq,
        FeedEntryKind::Status(StatusEntry {
            status: clipped(name),
            details: bounded_value(row),
        }),
        oversized(row),
    );
}

fn unknown(layer: &mut ClaudeSdkLayer, seq: u64, kind: &str, detail: &str) {
    push(
        layer,
        seq,
        FeedEntryKind::Unrecognized(UnrecognizedEntry {
            row_type: clipped(kind),
            detail: clipped(detail),
        }),
        kind.len() > CONTENT_BYTES_RETAINED || detail.len() > CONTENT_BYTES_RETAINED,
    );
}

fn push(layer: &mut ClaudeSdkLayer, seq: u64, kind: FeedEntryKind, content_truncated: bool) {
    if layer.entries.len() == FEED_RETAINED {
        layer.entries.pop_front();
        layer.evicted += 1;
    }
    layer.entries.push_back(FeedEntry {
        id: layer.next_entry_id,
        seq,
        kind,
        block: None,
        parent_tool_use_id: None,
        content_truncated,
        final_row_id: None,
    });
    layer.next_entry_id += 1;
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
fn is_internal_diagnostic(error: &str) -> bool {
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

fn oversized(value: &Value) -> bool {
    value.to_string().len() > CONTENT_BYTES_RETAINED
}

fn bounded_value(value: &Value) -> Option<Value> {
    (!value.is_null() && !oversized(value)).then(|| value.clone())
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
        assert!(observation.entries().any(|entry| matches!(
            &entry.kind,
            FeedEntryKind::Message(MessageEntry { text, finality: Finality::Complete }) if text == "hello"
        )));
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
