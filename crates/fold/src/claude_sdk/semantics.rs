//! Durable Claude SDK fold semantics.
//!
//! Stream rows are transient JSON. The persisted tip retains only provider
//! identities needed to correlate later deltas and finals, plus compact
//! summary knowledge; feed bodies live exclusively in emitted entries.

use std::mem::size_of;

use chrono::{DateTime, Utc};
use model::{
    AgentMessageKind, AgentPhase, Attention, ContextMeter, StructuredProtocol, Summary,
    SummaryField, TodoProgress, Why,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TaskState;
use crate::claude_tasks::TaskRegistry;
use crate::{
    Baseline, Changes, Component, ComponentSource, Components, Entry, EntryKey, FieldPatch, Input,
    JsonBytes, MergeDefect, Mutation, Order, Patch, PostcardSafe, Promotion, ProviderFold,
    RestoreRow, Revision, SegmentId, TIP_MAX_BYTES, TIP_MAX_OPEN_ENTRIES, VersionedField,
};

const TEXT_MAX_BYTES: usize = 64 * 1024;
const VALUE_MAX_BYTES: usize = 64 * 1024;

/// SDK forms whose creating row cannot always carry a provider-native key.
pub const DELIVERY_KEYED_VARIANTS: &[&str] = &[
    "claude_sdk.prompt_without_uuid",
    "claude_sdk.stream_block_without_message_start",
    "claude_sdk.occurrence_without_uuid",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeSdkEntryKind {
    Prompt,
    Message,
    Thinking,
    Tool,
    Task,
    Turn,
    Compaction,
    AgentMessage,
    Status,
    Boundary,
    ApiError,
    #[default]
    Unrecognized,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeSdkBody {
    #[default]
    None,
    Prompt {
        uuid: Option<String>,
        image_count: usize,
        synthetic: bool,
        replay: bool,
    },
    Thinking {
        redacted: bool,
    },
    Tool {
        tool_use_id: String,
        parent_tool_use_id: Option<String>,
    },
    Task {
        task_id: String,
    },
    Turn {
        uuid: Option<String>,
        outcome: String,
        is_error: bool,
        stop_reason: Option<String>,
    },
    Compaction {
        trigger: Option<String>,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },
    AgentMessage {
        id: Option<String>,
        context: Option<String>,
        from: String,
        kind: AgentMessageKind,
        delivery: Option<String>,
    },
    Status {
        status: String,
    },
    Boundary {
        boundary: String,
        session_id: Option<String>,
    },
    ApiError,
    Unrecognized {
        row_type: String,
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalComponents {
    pub through: u64,
    pub revision: Revision,
    pub values: Vec<Component<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSdkPartial {
    pub kind: FieldPatch<ClaudeSdkEntryKind>,
    pub body: FieldPatch<ClaudeSdkBody>,
    pub text: FieldPatch<String>,
    pub components: Vec<Component<String>>,
    pub final_components: Option<FinalComponents>,
    pub finality: FieldPatch<String>,
    pub parent_tool_use_id: FieldPatch<String>,
    pub tool_name: FieldPatch<String>,
    pub tool_input: FieldPatch<JsonBytes>,
    pub tool_outcome: FieldPatch<JsonBytes>,
    pub task_description: FieldPatch<String>,
    pub task_tool_use_id: FieldPatch<String>,
    pub task_subagent: FieldPatch<String>,
    pub task_state: FieldPatch<TaskState>,
    pub task_last_tool: FieldPatch<String>,
    pub task_summary: FieldPatch<String>,
    pub task_usage: FieldPatch<JsonBytes>,
    pub clipped: FieldPatch<bool>,
    pub incomplete: FieldPatch<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSdkEntry {
    kind: VersionedField<ClaudeSdkEntryKind>,
    body: VersionedField<ClaudeSdkBody>,
    text: VersionedField<String>,
    components: Components<String>,
    finality: VersionedField<String>,
    parent_tool_use_id: VersionedField<String>,
    tool_name: VersionedField<String>,
    tool_input: VersionedField<JsonBytes>,
    tool_outcome: VersionedField<JsonBytes>,
    task_description: VersionedField<String>,
    task_tool_use_id: VersionedField<String>,
    task_subagent: VersionedField<String>,
    task_state: VersionedField<TaskState>,
    task_last_tool: VersionedField<String>,
    task_summary: VersionedField<String>,
    task_usage: VersionedField<JsonBytes>,
    clipped: VersionedField<bool>,
    incomplete: VersionedField<bool>,
    rendered_text: String,
}

impl ClaudeSdkEntry {
    pub fn entry_kind(&self) -> Option<ClaudeSdkEntryKind> {
        self.kind.value().copied()
    }

    pub fn body(&self) -> Option<&ClaudeSdkBody> {
        self.body.value()
    }

    pub fn components(&self) -> &Components<String> {
        &self.components
    }

    pub fn finality(&self) -> Option<&str> {
        self.finality.value().map(String::as_str)
    }

    pub fn parent_tool_use_id(&self) -> Option<&str> {
        self.parent_tool_use_id.value().map(String::as_str)
    }

    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.value().map(String::as_str)
    }

    pub fn tool_input(&self) -> Option<&JsonBytes> {
        self.tool_input.value()
    }

    pub fn tool_outcome(&self) -> Option<&JsonBytes> {
        self.tool_outcome.value()
    }

    pub fn task_description(&self) -> Option<&str> {
        self.task_description.value().map(String::as_str)
    }

    pub fn task_tool_use_id(&self) -> Option<&str> {
        self.task_tool_use_id.value().map(String::as_str)
    }

    pub fn task_state(&self) -> Option<&TaskState> {
        self.task_state.value()
    }

    pub fn task_subagent(&self) -> Option<&str> {
        self.task_subagent.value().map(String::as_str)
    }

    pub fn task_last_tool(&self) -> Option<&str> {
        self.task_last_tool.value().map(String::as_str)
    }

    pub fn task_summary(&self) -> Option<&str> {
        self.task_summary.value().map(String::as_str)
    }

    pub fn task_usage(&self) -> Option<&JsonBytes> {
        self.task_usage.value()
    }

    pub fn is_incomplete(&self) -> bool {
        self.incomplete.value().copied().unwrap_or(false)
    }

    fn rebuild_text(&mut self) {
        if self.components.values().is_empty() {
            self.rendered_text = self.text.value().cloned().unwrap_or_default();
        } else {
            let mut components = self.components.values().to_vec();
            components.sort_by_key(|component| component.observed_at);
            self.rendered_text = components
                .into_iter()
                .map(|component| component.value)
                .collect::<Vec<_>>()
                .join("");
        }
        if (self.clipped.value() == Some(&true) || self.components.is_clipped())
            && !self.rendered_text.ends_with("… clipped …")
        {
            self.rendered_text.push_str("\n… clipped …");
        }
    }

    fn fill_unknown(&mut self, source: &Self) -> Result<(), MergeDefect> {
        self.kind.fill_unknown_from(&source.kind);
        self.body.fill_unknown_from(&source.body);
        self.text.fill_unknown_from(&source.text);
        self.finality.fill_unknown_from(&source.finality);
        self.parent_tool_use_id
            .fill_unknown_from(&source.parent_tool_use_id);
        self.tool_name.fill_unknown_from(&source.tool_name);
        self.tool_input.fill_unknown_from(&source.tool_input);
        self.tool_outcome.fill_unknown_from(&source.tool_outcome);
        self.task_description
            .fill_unknown_from(&source.task_description);
        self.task_tool_use_id
            .fill_unknown_from(&source.task_tool_use_id);
        self.task_subagent.fill_unknown_from(&source.task_subagent);
        self.task_state.fill_unknown_from(&source.task_state);
        self.task_last_tool
            .fill_unknown_from(&source.task_last_tool);
        self.task_summary.fill_unknown_from(&source.task_summary);
        self.task_usage.fill_unknown_from(&source.task_usage);
        self.clipped.fill_unknown_from(&source.clipped);
        self.incomplete.fill_unknown_from(&source.incomplete);
        for component in source.components.values() {
            self.components.merge(component.clone())?;
        }
        Ok(())
    }
}

impl Entry for ClaudeSdkEntry {
    type Partial = ClaudeSdkPartial;

    fn kind(&self) -> &'static str {
        match self.entry_kind() {
            Some(ClaudeSdkEntryKind::Prompt) => "prompt",
            Some(ClaudeSdkEntryKind::Message) => "message",
            Some(ClaudeSdkEntryKind::Thinking) => "thinking",
            Some(ClaudeSdkEntryKind::Tool) => "tool",
            Some(ClaudeSdkEntryKind::Task) => "task",
            Some(ClaudeSdkEntryKind::Turn) => "turn",
            Some(ClaudeSdkEntryKind::Compaction) => "compaction",
            Some(ClaudeSdkEntryKind::AgentMessage) => "agent_message",
            Some(ClaudeSdkEntryKind::Status) => "status",
            Some(ClaudeSdkEntryKind::Boundary) => "boundary",
            Some(ClaudeSdkEntryKind::ApiError) => "api_error",
            Some(ClaudeSdkEntryKind::Unrecognized) | None => "unrecognized",
        }
    }

    fn text(&self) -> Option<&str> {
        (!self.rendered_text.is_empty()).then_some(self.rendered_text.as_str())
    }

    fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect> {
        let preserves_task = self.entry_kind() == Some(ClaudeSdkEntryKind::Task)
            && matches!(
                &patch.kind,
                Patch::Set {
                    value: ClaudeSdkEntryKind::Tool,
                    ..
                }
            );
        if !preserves_task {
            self.kind.merge("kind", &patch.kind)?;
            self.body.merge("body", &patch.body)?;
        }
        self.text.merge("text", &patch.text)?;
        self.finality.merge("finality", &patch.finality)?;
        self.parent_tool_use_id
            .merge("parent_tool_use_id", &patch.parent_tool_use_id)?;
        self.tool_name.merge("tool_name", &patch.tool_name)?;
        self.tool_input.merge("tool_input", &patch.tool_input)?;
        self.tool_outcome
            .merge("tool_outcome", &patch.tool_outcome)?;
        self.task_description
            .merge("task_description", &patch.task_description)?;
        self.task_tool_use_id
            .merge("task_tool_use_id", &patch.task_tool_use_id)?;
        self.task_subagent
            .merge("task_subagent", &patch.task_subagent)?;
        self.task_state.merge("task_state", &patch.task_state)?;
        self.task_last_tool
            .merge("task_last_tool", &patch.task_last_tool)?;
        self.task_summary
            .merge("task_summary", &patch.task_summary)?;
        self.task_usage.merge("task_usage", &patch.task_usage)?;
        self.clipped.merge("clipped", &patch.clipped)?;
        self.incomplete.merge("incomplete", &patch.incomplete)?;
        for component in &patch.components {
            self.components.merge(component.clone())?;
        }
        if let Some(final_components) = &patch.final_components {
            self.components.replace_final(
                final_components.through,
                final_components.revision,
                final_components.values.clone(),
            )?;
        }
        self.rebuild_text();
        Ok(())
    }

    fn from_partial(patch: &Self::Partial) -> Result<Self, MergeDefect> {
        let mut entry = Self::default();
        entry.merge(patch)?;
        Ok(entry)
    }

    fn merge_alias(
        &mut self,
        source: &Self,
        promotion: Option<Promotion>,
    ) -> Result<(), MergeDefect> {
        if promotion == Some(Promotion::ToolToTask)
            && source.entry_kind() == Some(ClaudeSdkEntryKind::Task)
        {
            self.kind.clone_from(&source.kind);
            self.body.clone_from(&source.body);
        }
        self.fill_unknown(source)?;
        self.rebuild_text();
        Ok(())
    }

    fn promote(&mut self, _promotion: Option<Promotion>) -> Result<(), MergeDefect> {
        Ok(())
    }

    fn clip(&mut self, budget: usize) {
        self.components.clip_by(256, budget / 2, |component| {
            component.value.len() + component.after.capacity() * size_of::<ComponentSource>()
        });
        truncate_versioned_string(&mut self.text, TEXT_MAX_BYTES);
        truncate_versioned_string(&mut self.parent_tool_use_id, 512);
        truncate_versioned_bytes(&mut self.tool_input, VALUE_MAX_BYTES);
        truncate_versioned_bytes(&mut self.tool_outcome, VALUE_MAX_BYTES);
        truncate_versioned_string(&mut self.task_description, TEXT_MAX_BYTES);
        truncate_versioned_string(&mut self.task_tool_use_id, 512);
        truncate_versioned_string(&mut self.task_summary, TEXT_MAX_BYTES);
        self.rebuild_text();
        if self.bytes() > budget {
            self.rendered_text = clipped_text(&self.rendered_text, budget / 4);
        }
    }

    fn bytes(&self) -> usize {
        postcard::to_allocvec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BlockRef {
    index: u64,
    key: EntryKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MessageCursor {
    message_id: String,
    parent_tool_use_id: Option<String>,
    next_final_index: u64,
    streaming: bool,
    blocks: Vec<BlockRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingTodo {
    tool_use_id: String,
    progress: TodoProgress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingAsk {
    channel: String,
    request_id: String,
    row: RestoreRow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSdkFold {
    segment: SegmentId,
    baseline: Baseline,
    through: u64,
    cursors: Vec<MessageCursor>,
    pending_todos: Vec<PendingTodo>,
    tasks: TaskRegistry,
    asks: Vec<PendingAsk>,
    in_history: bool,
    attention: Attention,
    phase: AgentPhase,
    last_activity: Option<DateTime<Utc>>,
    todo: Option<TodoProgress>,
    context: Option<ContextMeter>,
    model: Option<String>,
    known_attention: bool,
    known_phase: bool,
    known_last_activity: bool,
    known_todo: bool,
    known_context: bool,
    known_model: bool,
    known_outstanding: bool,
    overflowed: bool,
}

impl Default for ClaudeSdkFold {
    fn default() -> Self {
        Self {
            segment: 0,
            baseline: Baseline::Start,
            through: 0,
            cursors: Vec::new(),
            pending_todos: Vec::new(),
            tasks: TaskRegistry::default(),
            asks: Vec::new(),
            in_history: false,
            attention: Attention::Unknown,
            phase: AgentPhase::Running,
            last_activity: None,
            todo: None,
            context: None,
            model: None,
            known_attention: false,
            known_phase: false,
            known_last_activity: false,
            known_todo: false,
            known_context: false,
            known_model: false,
            known_outstanding: true,
            overflowed: false,
        }
    }
}

impl ClaudeSdkFold {
    pub fn segment(&self) -> SegmentId {
        self.segment
    }

    pub fn through(&self) -> u64 {
        self.through
    }

    pub fn restored_attention(&self) -> Option<Attention> {
        self.known_attention.then_some(self.attention)
    }

    pub fn restored_outstanding_known(&self) -> bool {
        self.known_outstanding
    }

    pub fn restored_obligations(&self) -> impl Iterator<Item = &RestoreRow> {
        self.asks.iter().map(|ask| &ask.row)
    }

    fn row(
        &mut self,
        seq: u64,
        activity_at: Option<DateTime<Utc>>,
        historical: bool,
        row: &Value,
    ) -> Vec<Mutation<ClaudeSdkEntry>> {
        self.through = self.through.max(seq);
        if !self.known_phase {
            self.phase = AgentPhase::Running;
            self.known_phase = true;
        }
        if let Some(at) = activity_at {
            self.last_activity = Some(at);
            self.known_last_activity = true;
        }

        let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(
            kind,
            "amux.claude_sdk.history_begin" | "history_begin" | "HistoryBegin"
        ) {
            self.in_history = true;
            return Vec::new();
        }
        if matches!(
            kind,
            "amux.claude_sdk.history_complete" | "history_complete" | "HistoryComplete"
        ) {
            self.in_history = false;
            return Vec::new();
        }

        let historical = historical || self.in_history;
        let revision = Revision::row(seq);
        let mut mutations = Vec::new();
        match kind {
            "assistant" => self.assistant(seq, revision, row, historical, &mut mutations),
            "stream_event" => self.stream(seq, revision, row, historical, &mut mutations),
            "user" => self.user(seq, revision, row, historical, &mut mutations),
            "result" => {
                self.interrupt_open(seq, revision, &mut mutations);
                self.turn(seq, revision, row, &mut mutations);
                if !historical {
                    self.attention = Attention::NeedsYou { why: Why::Finished };
                    self.known_attention = true;
                    self.known_outstanding = true;
                    self.asks.clear();
                }
            }
            "amux.claude_sdk.ready" => {
                self.interrupt_open(seq, revision, &mut mutations);
                self.cursors.clear();
                self.asks.clear();
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
                let key = occurrence_key("ready", row, seq, 0);
                let body = ClaudeSdkBody::Boundary {
                    boundary: "ready".into(),
                    session_id: id(row, "session_id").map(bounded_id),
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(ClaudeSdkEntryKind::Boundary, body, None, revision),
                ));
            }
            "amux.claude_sdk.gap" => {
                self.interrupt_open(seq, revision, &mut mutations);
                self.cursors.clear();
                self.attention = Attention::Unknown;
                self.known_attention = false;
                self.known_outstanding = false;
                let key = occurrence_key("gap", row, seq, 0);
                let body = ClaudeSdkBody::Boundary {
                    boundary: "gap".into(),
                    session_id: id(row, "resumed_session_id").map(bounded_id),
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(ClaudeSdkEntryKind::Boundary, body, None, revision),
                ));
            }
            "conversation_reset" => {
                self.interrupt_open(seq, revision, &mut mutations);
                self.cursors.clear();
                self.pending_todos.clear();
                self.tasks.clear();
                self.asks.clear();
                self.todo = None;
                self.known_todo = true;
                self.context = None;
                self.known_context = false;
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
                let key = occurrence_key("conversation_reset", row, seq, 0);
                let body = ClaudeSdkBody::Boundary {
                    boundary: "conversation_reset".into(),
                    session_id: id(row, "new_conversation_id").map(bounded_id),
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(ClaudeSdkEntryKind::Boundary, body, None, revision),
                ));
            }
            "amux.claude_sdk.message" => self.agent_message(seq, revision, row, &mut mutations),
            "system" => self.system(seq, revision, row, historical, &mut mutations),
            "amux.claude_sdk.session_facts" if !historical => self.session_facts(row),
            "amux.claude_sdk.context_breakdown" if !historical => {
                if let Ok(context) = serde_json::from_value(row["usage"].clone()) {
                    self.context = Some(context);
                    self.known_context = true;
                }
            }
            "amux.claude_sdk.permission_required"
            | "amux.claude_sdk.elicitation_required"
            | "amux.claude_sdk.dialog_required" => {
                if !historical {
                    let channel = kind
                        .strip_prefix("amux.claude_sdk.")
                        .and_then(|kind| kind.strip_suffix("_required"))
                        .unwrap_or_default();
                    let request =
                        id(row, "request_id").unwrap_or_else(|| format!("delivery:{seq}"));
                    if !self
                        .asks
                        .iter()
                        .any(|ask| ask.channel == channel && ask.request_id == request)
                    {
                        self.asks.push(PendingAsk {
                            channel: channel.to_owned(),
                            request_id: request,
                            row: RestoreRow {
                                seq,
                                payload: JsonBytes(
                                    serde_json::to_vec(row).unwrap_or_else(|_| b"null".to_vec()),
                                ),
                            },
                        });
                    }
                    self.attention = Attention::NeedsYou {
                        why: if kind == "amux.claude_sdk.permission_required" {
                            Why::Permission
                        } else {
                            Why::Question
                        },
                    };
                    self.known_attention = true;
                }
            }
            "amux.claude_sdk.permission_resolved"
            | "amux.claude_sdk.elicitation_resolved"
            | "amux.claude_sdk.dialog_resolved" => {
                if !historical && let Some(request_id) = id(row, "request_id") {
                    let channel = kind
                        .strip_prefix("amux.claude_sdk.")
                        .and_then(|kind| kind.strip_suffix("_resolved"))
                        .unwrap_or_default();
                    self.asks
                        .retain(|ask| ask.channel != channel || ask.request_id != request_id);
                    if self.asks.is_empty() {
                        self.attention = Attention::Working;
                        self.known_attention = true;
                    }
                }
            }
            "rate_limit_event" | "tool_progress" | "tool_use_summary" | "auth_status" => {
                self.status(seq, revision, kind, row, &mut mutations)
            }
            "amux.claude_sdk.input_result" => {
                if row.get("outcome").and_then(Value::as_str) != Some("ok") {
                    self.status(seq, revision, "input_error", row, &mut mutations);
                }
            }
            "amux.attachments" => {}
            _ => {
                let key = occurrence_key("raw", row, seq, 0);
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(
                        ClaudeSdkEntryKind::Unrecognized,
                        ClaudeSdkBody::Unrecognized {
                            row_type: clipped_text(kind, 512),
                            detail: "row is not recognized".into(),
                        },
                        None,
                        revision,
                    ),
                ));
            }
        }
        self.enforce_tip(seq, revision, &mut mutations);
        mutations
    }

    fn session_facts(&mut self, row: &Value) {
        if let Some(model) = string(row, "model") {
            self.model = Some(model);
            self.known_model = true;
        }
        if let Ok(context) = serde_json::from_value(row["context"].clone()) {
            self.context = Some(context);
            self.known_context = true;
        }
    }

    fn assistant(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        historical: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let Some(row_id) = id(row, "uuid") else {
            mutations.push(unrecognized(
                seq,
                0,
                revision,
                "assistant",
                "missing row uuid",
            ));
            return;
        };
        let message = row.get("message").unwrap_or(&Value::Null);
        let message_id = id(message, "id");
        let parent = id(row, "parent_tool_use_id");
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            mutations.push(unrecognized(
                seq,
                0,
                revision,
                "assistant",
                "missing content blocks",
            ));
            return;
        };

        if !historical {
            self.attention = Attention::Working;
            self.known_attention = true;
        }
        let cursor_index = message_id.as_deref().and_then(|message_id| {
            self.cursors.iter().position(|cursor| {
                cursor.message_id == message_id && cursor.parent_tool_use_id == parent
            })
        });
        let start = if blocks.len() > 1 {
            0
        } else {
            cursor_index
                .map(|index| self.cursors[index].next_final_index)
                .unwrap_or(0)
        };
        let retained = retained_block_count(blocks.len());
        for (slot, block) in blocks.iter().take(retained).enumerate() {
            let slot = slot as u16;
            let block_index = if blocks.len() == 1 {
                start
            } else {
                u64::from(slot)
            };
            match block.get("type").and_then(Value::as_str) {
                Some("text" | "thinking" | "redacted_thinking") => {
                    let key = final_key(&row_id, slot, seq);
                    let text = block
                        .get(
                            if block.get("type").and_then(Value::as_str) == Some("text") {
                                "text"
                            } else {
                                "thinking"
                            },
                        )
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let kind = if block.get("type").and_then(Value::as_str) == Some("text") {
                        ClaudeSdkEntryKind::Message
                    } else {
                        ClaudeSdkEntryKind::Thinking
                    };
                    let body = if kind == ClaudeSdkEntryKind::Thinking {
                        ClaudeSdkBody::Thinking {
                            redacted: block.get("type").and_then(Value::as_str)
                                == Some("redacted_thinking"),
                        }
                    } else if row.get("is_api_error").and_then(Value::as_bool) == Some(true)
                        || row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
                    {
                        ClaudeSdkBody::ApiError
                    } else {
                        ClaudeSdkBody::None
                    };
                    let mut patch = partial(kind, body, None, revision);
                    patch.finality = Patch::set("complete".into(), revision);
                    if let Some(parent) = parent.clone() {
                        patch.parent_tool_use_id = Patch::set(bounded_id(parent), revision);
                    }
                    patch.incomplete = Patch::set(false, revision);
                    patch.final_components = Some(FinalComponents {
                        through: seq,
                        revision,
                        values: vec![Component {
                            source: ComponentSource::Sequence { seq, slot },
                            observed_at: seq,
                            after: Vec::new(),
                            value: clipped_text(text, TEXT_MAX_BYTES),
                        }],
                    });
                    if text.len() > TEXT_MAX_BYTES {
                        patch.clipped = Patch::set(true, revision);
                    }
                    let provisional = cursor_index
                        .and_then(|cursor_index| {
                            self.cursors[cursor_index]
                                .blocks
                                .iter()
                                .find(|block| block.index == block_index)
                                .map(|block| block.key.clone())
                        })
                        .filter(|from| from != &key);
                    mutations.push(upsert(
                        provisional.clone().unwrap_or_else(|| key.clone()),
                        seq,
                        slot,
                        revision,
                        patch,
                    ));
                    if let Some(from) = provisional {
                        mutations.push(Mutation::Alias {
                            from,
                            to: key,
                            revision,
                            promote: None,
                        });
                    }
                }
                Some("tool_use" | "server_tool_use") => self.tool_use(
                    (seq, slot, revision),
                    parent.clone(),
                    block,
                    "complete",
                    mutations,
                ),
                other => mutations.push(unrecognized(
                    seq,
                    slot,
                    revision,
                    "assistant.content",
                    other.unwrap_or("unrecognized content block"),
                )),
            }
        }
        if blocks.len() > 1024 {
            mutations.push(clipped_marker(seq, revision, "assistant row clipped"));
        }
        if let Some(index) = cursor_index {
            self.cursors[index].next_final_index = start.saturating_add(blocks.len() as u64);
        }
    }

    fn stream(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        historical: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let event = row.get("event").unwrap_or(&Value::Null);
        let parent = id(row, "parent_tool_use_id");
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "message_start" {
            let Some(message_id) = event.get("message").and_then(|message| id(message, "id"))
            else {
                mutations.push(unrecognized(
                    seq,
                    0,
                    revision,
                    "message_start",
                    "missing message id",
                ));
                return;
            };
            for cursor in &mut self.cursors {
                if cursor.parent_tool_use_id == parent {
                    cursor.streaming = false;
                }
            }
            let index = self.cursor(message_id, parent);
            self.cursors[index].streaming = true;
            if !historical {
                self.attention = Attention::Working;
                self.known_attention = true;
            }
            return;
        }

        let active = self
            .cursors
            .iter()
            .rposition(|cursor| cursor.streaming && cursor.parent_tool_use_id == parent);
        match kind {
            "content_block_start" => {
                let Some(block_index) = event.get("index").and_then(Value::as_u64) else {
                    mutations.push(unrecognized(seq, 0, revision, kind, "missing block index"));
                    return;
                };
                let content_block = event.get("content_block").unwrap_or(&Value::Null);
                let native_tool_key = matches!(
                    content_block.get("type").and_then(Value::as_str),
                    Some("tool_use" | "server_tool_use")
                )
                .then(|| id(content_block, "id"))
                .flatten()
                .map(|tool_id| namespaced_key("tool", &tool_id, seq, 0));
                let (cursor_index, key, incomplete) = if let Some(cursor_index) = active {
                    let key = native_tool_key.unwrap_or_else(|| {
                        namespaced_key(
                            "blk",
                            &format!("{}:{block_index}", self.cursors[cursor_index].message_id),
                            seq,
                            0,
                        )
                    });
                    (cursor_index, key, false)
                } else {
                    let incomplete = native_tool_key.is_none();
                    let key = native_tool_key.unwrap_or_else(|| delivery_key(seq, 0));
                    let cursor_index = self.cursor(String::new(), parent.clone());
                    self.cursors[cursor_index].streaming = true;
                    (cursor_index, key, incomplete)
                };
                self.cursors[cursor_index]
                    .blocks
                    .retain(|block| block.index != block_index);
                self.cursors[cursor_index].blocks.push(BlockRef {
                    index: block_index,
                    key: key.clone(),
                });
                self.stream_start(
                    (seq, revision),
                    key,
                    parent,
                    content_block,
                    incomplete,
                    mutations,
                );
            }
            "content_block_delta" | "content_block_stop" => {
                let Some(cursor_index) = active else {
                    mutations.push(unrecognized(
                        seq,
                        0,
                        revision,
                        kind,
                        "block start is unavailable",
                    ));
                    return;
                };
                let Some(block_index) = event.get("index").and_then(Value::as_u64) else {
                    mutations.push(unrecognized(seq, 0, revision, kind, "missing block index"));
                    return;
                };
                let Some(key) = self.cursors[cursor_index]
                    .blocks
                    .iter()
                    .find(|block| block.index == block_index)
                    .map(|block| block.key.clone())
                else {
                    mutations.push(unrecognized(
                        seq,
                        0,
                        revision,
                        kind,
                        "block start is unavailable",
                    ));
                    return;
                };
                let mut patch = ClaudeSdkPartial::default();
                if kind == "content_block_stop" {
                    patch.finality = Patch::set("stopped".into(), revision);
                } else {
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    let text = match delta.get("type").and_then(Value::as_str) {
                        Some("text_delta") => delta.get("text").and_then(Value::as_str),
                        Some("thinking_delta") => delta.get("thinking").and_then(Value::as_str),
                        Some("input_json_delta") => {
                            delta.get("partial_json").and_then(Value::as_str)
                        }
                        Some("signature_delta") => None,
                        _ => None,
                    };
                    if let Some(text) = text {
                        patch.components.push(Component {
                            source: ComponentSource::Sequence { seq, slot: 0 },
                            observed_at: seq,
                            after: Vec::new(),
                            value: clipped_text(text, TEXT_MAX_BYTES),
                        });
                        if text.len() > TEXT_MAX_BYTES {
                            patch.clipped = Patch::set(true, revision);
                        }
                    }
                }
                mutations.push(upsert(key, seq, 0, revision, patch));
            }
            "message_stop" => {
                if let Some(cursor_index) = active {
                    let keys = self.cursors[cursor_index]
                        .blocks
                        .iter()
                        .map(|block| block.key.clone())
                        .collect::<Vec<_>>();
                    for key in keys {
                        mutations.push(upsert(
                            key,
                            seq,
                            0,
                            revision,
                            ClaudeSdkPartial {
                                finality: Patch::set("stopped".into(), revision),
                                ..ClaudeSdkPartial::default()
                            },
                        ));
                    }
                    self.cursors[cursor_index].streaming = false;
                }
            }
            "message_delta" => {}
            _ => mutations.push(unrecognized(
                seq,
                0,
                revision,
                "stream_event",
                "unrecognized stream event",
            )),
        }
    }

    fn stream_start(
        &mut self,
        position: (u64, Revision),
        key: EntryKey,
        parent: Option<String>,
        block: &Value,
        incomplete: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let (seq, revision) = position;
        match block.get("type").and_then(Value::as_str) {
            Some("text" | "thinking" | "redacted_thinking") => {
                let field = if block.get("type").and_then(Value::as_str) == Some("text") {
                    "text"
                } else {
                    "thinking"
                };
                let text = block.get(field).and_then(Value::as_str).unwrap_or_default();
                let kind = if field == "text" {
                    ClaudeSdkEntryKind::Message
                } else {
                    ClaudeSdkEntryKind::Thinking
                };
                let body = if kind == ClaudeSdkEntryKind::Thinking {
                    ClaudeSdkBody::Thinking {
                        redacted: block.get("type").and_then(Value::as_str)
                            == Some("redacted_thinking"),
                    }
                } else {
                    ClaudeSdkBody::None
                };
                let mut patch = partial(kind, body, None, revision);
                patch.finality = Patch::set("streaming".into(), revision);
                if let Some(parent) = parent {
                    patch.parent_tool_use_id = Patch::set(bounded_id(parent), revision);
                }
                patch.incomplete = Patch::set(incomplete, revision);
                if !text.is_empty() {
                    patch.components.push(Component {
                        source: ComponentSource::Sequence { seq, slot: 0 },
                        observed_at: seq,
                        after: Vec::new(),
                        value: clipped_text(text, TEXT_MAX_BYTES),
                    });
                }
                mutations.push(upsert(key, seq, 0, revision, patch));
            }
            Some("tool_use" | "server_tool_use") => {
                self.tool_use((seq, 0, revision), parent, block, "streaming", mutations)
            }
            other => mutations.push(unrecognized(
                seq,
                0,
                revision,
                "content_block_start",
                other.unwrap_or("unrecognized content block"),
            )),
        }
    }

    fn tool_use(
        &mut self,
        position: (u64, u16, Revision),
        parent: Option<String>,
        block: &Value,
        finality: &str,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let (seq, slot, revision) = position;
        let Some(tool_id) = id(block, "id") else {
            mutations.push(unrecognized(
                seq,
                slot,
                revision,
                "assistant.tool_use",
                "missing tool id",
            ));
            return;
        };
        let name = string(block, "name").unwrap_or_default();
        let key = namespaced_key("tool", &tool_id, seq, slot);
        if name == "TodoWrite"
            && let Some(progress) = todo_progress(block.get("input").unwrap_or(&Value::Null))
        {
            if let Some(pending) = self
                .pending_todos
                .iter_mut()
                .find(|pending| pending.tool_use_id == tool_id)
            {
                pending.progress = progress;
            } else {
                self.pending_todos.push(PendingTodo {
                    tool_use_id: tool_id,
                    progress,
                });
            }
            mutations.push(Mutation::Delete { key, revision });
            return;
        }
        self.tasks
            .observe_invocation(&tool_id, &name, block.get("input").unwrap_or(&Value::Null));
        let mut patch = partial(
            ClaudeSdkEntryKind::Tool,
            ClaudeSdkBody::Tool {
                tool_use_id: bounded_id(tool_id),
                parent_tool_use_id: parent.clone().map(bounded_id),
            },
            None,
            revision,
        );
        patch.tool_name = Patch::set(name, revision);
        patch.tool_input = Patch::set(
            JsonBytes(bounded_json(block.get("input").unwrap_or(&Value::Null))),
            revision,
        );
        patch.finality = Patch::set(finality.into(), revision);
        if let Some(parent) = parent {
            patch.parent_tool_use_id = Patch::set(bounded_id(parent), revision);
        }
        mutations.push(upsert(key, seq, slot, revision, patch));
    }

    fn user(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        historical: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let uuid = id(row, "uuid");
        let parent = id(row, "parent_tool_use_id");
        let content = row.pointer("/message/content").unwrap_or(&Value::Null);
        let mut text = String::new();
        let mut images = 0usize;
        let mut prompt_slot = 0;
        let mut prompt_slot_known = false;
        if let Some(value) = content.as_str() {
            append_text(&mut text, value, TEXT_MAX_BYTES);
        }
        if let Some(blocks) = content.as_array() {
            for (slot, block) in blocks
                .iter()
                .take(retained_block_count(blocks.len()))
                .enumerate()
            {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if !prompt_slot_known {
                            prompt_slot = slot as u16;
                            prompt_slot_known = true;
                        }
                        if !text.is_empty() {
                            append_text(&mut text, "\n", TEXT_MAX_BYTES);
                        }
                        if let Some(part) = block.get("text").and_then(Value::as_str) {
                            append_text(&mut text, part, TEXT_MAX_BYTES);
                        }
                    }
                    Some("image") => {
                        if !prompt_slot_known {
                            prompt_slot = slot as u16;
                            prompt_slot_known = true;
                        }
                        images += 1;
                    }
                    Some("tool_result") => self.tool_result(
                        (seq, slot as u16, revision),
                        row,
                        block,
                        historical,
                        mutations,
                    ),
                    other => mutations.push(unrecognized(
                        seq,
                        slot as u16,
                        revision,
                        "user.content",
                        other.unwrap_or("unrecognized content block"),
                    )),
                }
            }
            if blocks.len() > 1024 {
                mutations.push(clipped_marker(seq, revision, "user row clipped"));
            }
        } else if !content.is_string() {
            mutations.push(unrecognized(
                seq,
                0,
                revision,
                "user",
                "missing message content",
            ));
            return;
        }

        if parent.is_some() || text.is_empty() && images == 0 {
            return;
        }
        if let Some(message) = crate::claude_pty::facts::inbound_message(&text) {
            let key = message
                .id
                .as_deref()
                .map(|id| namespaced_key("env", id, seq, prompt_slot))
                .unwrap_or_else(|| {
                    namespaced_or_delivery("user", uuid.as_deref(), seq, prompt_slot)
                });
            let body = ClaudeSdkBody::AgentMessage {
                id: message.id,
                context: message.context,
                from: message.from,
                kind: message.kind,
                delivery: None,
            };
            mutations.push(upsert(
                key,
                seq,
                prompt_slot,
                revision,
                partial(
                    ClaudeSdkEntryKind::AgentMessage,
                    body,
                    Some(message.text),
                    revision,
                ),
            ));
            return;
        }
        if matches!(
            text.as_str(),
            "[Request interrupted by user]" | "[Request interrupted by user for tool use]"
        ) {
            let key = occurrence_key("interrupt", row, seq, prompt_slot);
            mutations.push(upsert(
                key,
                seq,
                prompt_slot,
                revision,
                partial(
                    ClaudeSdkEntryKind::Status,
                    ClaudeSdkBody::Status {
                        status: "interrupted".into(),
                    },
                    Some(text),
                    revision,
                ),
            ));
            if !historical {
                self.attention = Attention::NeedsYou { why: Why::Finished };
                self.known_attention = true;
                self.known_outstanding = true;
                self.asks.clear();
            }
            return;
        }
        let key = namespaced_or_delivery("user", uuid.as_deref(), seq, prompt_slot);
        let body = ClaudeSdkBody::Prompt {
            uuid: uuid.map(bounded_id),
            image_count: images,
            synthetic: row.get("isSynthetic").and_then(Value::as_bool) == Some(true),
            replay: row.get("isReplay").and_then(Value::as_bool) == Some(true),
        };
        mutations.push(upsert(
            key,
            seq,
            prompt_slot,
            revision,
            partial(ClaudeSdkEntryKind::Prompt, body, Some(text), revision),
        ));
        if !historical
            && row.get("isSynthetic").and_then(Value::as_bool) != Some(true)
            && row.get("isReplay").and_then(Value::as_bool) != Some(true)
        {
            self.attention = Attention::Working;
            self.known_attention = true;
        }
    }

    fn tool_result(
        &mut self,
        position: (u64, u16, Revision),
        row: &Value,
        block: &Value,
        historical: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let (seq, slot, revision) = position;
        let Some(tool_id) = id(block, "tool_use_id") else {
            mutations.push(unrecognized(
                seq,
                slot,
                revision,
                "user.tool_result",
                "missing tool id",
            ));
            return;
        };
        if let Some(progress) = self.tasks.observe_result(&tool_id, block) {
            self.todo = Some(progress);
            self.known_todo = true;
        }
        if let Some(index) = self
            .pending_todos
            .iter()
            .position(|todo| todo.tool_use_id == tool_id)
        {
            let pending = self.pending_todos.remove(index);
            if block.get("is_error").and_then(Value::as_bool) != Some(true) {
                self.todo = Some(pending.progress);
                self.known_todo = true;
            }
            mutations.push(Mutation::Delete {
                key: namespaced_key("tool", &tool_id, seq, slot),
                revision,
            });
            return;
        }
        let mut text = String::new();
        if let Some(content) = block.get("content").and_then(Value::as_str) {
            append_text(&mut text, content, TEXT_MAX_BYTES);
        }
        for value in block
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(part) = value.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    append_text(&mut text, "\n", TEXT_MAX_BYTES);
                }
                append_text(&mut text, part, TEXT_MAX_BYTES);
            }
        }
        let key = namespaced_key("tool", &tool_id, seq, slot);
        let mut patch = partial(
            ClaudeSdkEntryKind::Tool,
            ClaudeSdkBody::Tool {
                tool_use_id: bounded_id(tool_id),
                parent_tool_use_id: id(row, "parent_tool_use_id").map(bounded_id),
            },
            None,
            revision,
        );
        patch.tool_outcome = Patch::set(
            JsonBytes(bounded_json(&serde_json::json!({
                "text": text,
                "is_error": block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                "details": row.get("tool_use_result")
            }))),
            revision,
        );
        patch.finality = Patch::set("complete".into(), revision);
        mutations.push(upsert(key, seq, slot, revision, patch));
        if !historical && !self.asks.is_empty() {
            self.attention = Attention::Working;
            self.known_attention = true;
        }
    }

    fn turn(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let Some(uuid) = id(row, "uuid") else {
            mutations.push(unrecognized(seq, 0, revision, "result", "missing uuid"));
            return;
        };
        let key = namespaced_or_delivery("turn", Some(&uuid), seq, 0);
        let body = ClaudeSdkBody::Turn {
            uuid: Some(bounded_id(uuid)),
            outcome: string(row, "subtype").unwrap_or_else(|| "unknown".into()),
            is_error: row.get("is_error").and_then(Value::as_bool) == Some(true),
            stop_reason: string(row, "stop_reason"),
        };
        let mut patch = partial(
            ClaudeSdkEntryKind::Turn,
            body,
            string(row, "result"),
            revision,
        );
        patch.tool_outcome = Patch::set(JsonBytes(bounded_json(row)), revision);
        mutations.push(upsert(key, seq, 0, revision, patch));
    }

    fn system(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        historical: bool,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        match row.get("subtype").and_then(Value::as_str).unwrap_or("") {
            "compact_boundary" => {
                let metadata = row.get("compact_metadata").unwrap_or(&Value::Null);
                let key = occurrence_key("compact", row, seq, 0);
                let body = ClaudeSdkBody::Compaction {
                    trigger: string(metadata, "trigger"),
                    pre_tokens: metadata.get("pre_tokens").and_then(Value::as_u64),
                    post_tokens: metadata.get("post_tokens").and_then(Value::as_u64),
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(ClaudeSdkEntryKind::Compaction, body, None, revision),
                ));
            }
            "task_started" | "task_progress" | "task_updated" | "task_notification" => {
                self.task(seq, 0, revision, row, mutations)
            }
            "background_tasks_changed" => {
                if let Some(tasks) = row.get("tasks").and_then(Value::as_array) {
                    for (slot, task) in tasks.iter().take(1024).enumerate() {
                        self.task(seq, slot as u16, revision, task, mutations);
                    }
                }
            }
            "status" => {
                let status = row.get("status").and_then(Value::as_str).unwrap_or("ready");
                self.status(seq, revision, status, row, mutations);
                if !historical && status == "compacting" {
                    self.attention = Attention::Working;
                    self.known_attention = true;
                }
            }
            "init" if !historical => {
                if let Some(model) = string(row, "model") {
                    self.model = Some(model);
                    self.known_model = true;
                }
            }
            "init" | "thinking_tokens" => {}
            subtype => mutations.push(unrecognized(seq, 0, revision, "system", subtype)),
        }
    }

    fn task(
        &mut self,
        seq: u64,
        slot: u16,
        revision: Revision,
        row: &Value,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let Some(task_id) = id(row, "task_id") else {
            mutations.push(unrecognized(
                seq,
                slot,
                revision,
                "system.task",
                "missing task id",
            ));
            return;
        };
        let tool_use_id = id(row, "tool_use_id");
        let task_key = namespaced_key("task", &task_id, seq, slot);
        let mut patch = partial(
            ClaudeSdkEntryKind::Task,
            ClaudeSdkBody::Task {
                task_id: bounded_id(task_id),
            },
            None,
            revision,
        );
        let fields = if row.get("subtype").and_then(Value::as_str) == Some("task_updated") {
            row.get("patch").unwrap_or(&Value::Null)
        } else {
            row
        };
        if let Some(value) = string(fields, "description") {
            patch.task_description = Patch::set(value, revision);
        }
        if let Some(value) = tool_use_id.clone() {
            patch.task_tool_use_id = Patch::set(value, revision);
        }
        if let Some(value) = string(fields, "subagent_type") {
            patch.task_subagent = Patch::set(value, revision);
        }
        if let Some(value) =
            string(fields, "last_tool_name").or_else(|| string(fields, "last_tool"))
        {
            patch.task_last_tool = Patch::set(value, revision);
        }
        if let Some(value) = string(fields, "summary") {
            patch.task_summary = Patch::set(value, revision);
        }
        if let Some(value) = string(fields, "status") {
            patch.task_state = Patch::set(parse_task_state(value), revision);
        }
        if let Some(usage) = fields.get("usage").filter(|value| value.is_object()) {
            patch.task_usage = Patch::set(JsonBytes(bounded_json(usage)), revision);
        }
        mutations.push(upsert(task_key.clone(), seq, slot, revision, patch));
        if let Some(tool_use_id) = tool_use_id {
            mutations.push(Mutation::Alias {
                from: task_key,
                to: namespaced_key("tool", &tool_use_id, seq, slot),
                revision,
                promote: Some(Promotion::ToolToTask),
            });
        }
    }

    fn agent_message(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let envelope = row.get("envelope").unwrap_or(&Value::Null);
        let Some(text) = string(envelope, "text") else {
            mutations.push(unrecognized(
                seq,
                0,
                revision,
                "amux.claude_sdk.message",
                "missing envelope text",
            ));
            return;
        };
        let Some(envelope_id) = id(envelope, "id") else {
            mutations.push(unrecognized(
                seq,
                0,
                revision,
                "amux.claude_sdk.message",
                "missing envelope id",
            ));
            return;
        };
        let key = namespaced_key("env", &envelope_id, seq, 0);
        let from = envelope.get("from").unwrap_or(&Value::Null);
        let sender = if from.get("type").and_then(Value::as_str) == Some("human") {
            "human".into()
        } else {
            string(from, "name")
                .or_else(|| id(from, "agent_id"))
                .or_else(|| from.as_str().map(|value| clipped_text(value, 512)))
                .unwrap_or_else(|| "unknown".into())
        };
        let body = ClaudeSdkBody::AgentMessage {
            id: Some(bounded_id(envelope_id)),
            context: id(envelope, "context").map(bounded_id),
            from: sender,
            kind: AgentMessageKind::read(envelope.get("kind").and_then(Value::as_str)),
            delivery: string(row, "delivery"),
        };
        mutations.push(upsert(
            key,
            seq,
            0,
            revision,
            partial(ClaudeSdkEntryKind::AgentMessage, body, Some(text), revision),
        ));
    }

    fn status(
        &mut self,
        seq: u64,
        revision: Revision,
        status: &str,
        row: &Value,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let key = occurrence_key(status, row, seq, 0);
        let mut patch = partial(
            ClaudeSdkEntryKind::Status,
            ClaudeSdkBody::Status {
                status: clipped_text(status, 512),
            },
            None,
            revision,
        );
        patch.tool_outcome = Patch::set(JsonBytes(bounded_json(row)), revision);
        mutations.push(upsert(key, seq, 0, revision, patch));
    }

    fn cursor(&mut self, message_id: String, parent_tool_use_id: Option<String>) -> usize {
        if let Some(index) = self.cursors.iter().position(|cursor| {
            cursor.message_id == message_id && cursor.parent_tool_use_id == parent_tool_use_id
        }) {
            return index;
        }
        self.cursors.push(MessageCursor {
            message_id,
            parent_tool_use_id,
            next_final_index: 0,
            streaming: false,
            blocks: Vec::new(),
        });
        self.cursors.len() - 1
    }

    fn interrupt_open(
        &mut self,
        seq: u64,
        revision: Revision,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        let keys = self
            .cursors
            .iter_mut()
            .filter(|cursor| cursor.streaming)
            .flat_map(|cursor| {
                cursor.streaming = false;
                cursor.blocks.iter().map(|block| block.key.clone())
            })
            .collect::<Vec<_>>();
        for key in keys {
            mutations.push(upsert(
                key,
                seq,
                0,
                revision,
                ClaudeSdkPartial {
                    finality: Patch::set("interrupted".into(), revision),
                    ..ClaudeSdkPartial::default()
                },
            ));
        }
    }

    fn enforce_tip(
        &mut self,
        seq: u64,
        revision: Revision,
        mutations: &mut Vec<Mutation<ClaudeSdkEntry>>,
    ) {
        while self.cursors.len() > TIP_MAX_OPEN_ENTRIES || self.tip_bytes() > TIP_MAX_BYTES {
            if !self.cursors.is_empty() {
                let cursor = self.cursors.remove(0);
                for block in cursor.blocks {
                    mutations.push(upsert(
                        block.key,
                        seq,
                        0,
                        revision,
                        ClaudeSdkPartial {
                            incomplete: Patch::set(true, revision),
                            clipped: Patch::set(true, revision),
                            ..ClaudeSdkPartial::default()
                        },
                    ));
                }
            } else if !self.pending_todos.is_empty() {
                self.pending_todos.remove(0);
                self.known_outstanding = false;
            } else if !self.asks.is_empty() {
                self.asks.remove(0);
                self.known_outstanding = false;
            } else {
                self.overflowed = true;
                self.known_outstanding = false;
                break;
            }
        }
        while self.pending_todos.len() + self.asks.len() > TIP_MAX_OPEN_ENTRIES {
            if !self.pending_todos.is_empty() {
                self.pending_todos.remove(0);
            } else {
                self.asks.remove(0);
            }
            self.known_outstanding = false;
        }
    }
}

impl ProviderFold for ClaudeSdkFold {
    type Entry = ClaudeSdkEntry;

    const PROTOCOL: StructuredProtocol = StructuredProtocol::ClaudeSdk;
    const ENTRY_VERSION: u32 = 2;
    const TIP_VERSION: u32 = 3;
    const TIP_BUDGET: usize = TIP_MAX_BYTES;

    fn begin(&mut self, segment: SegmentId, baseline: Baseline) {
        self.segment = segment;
        self.baseline = baseline;
        self.cursors.clear();
        self.pending_todos.clear();
        self.tasks.clear_pending();
        self.asks.clear();
        self.in_history = false;
        self.overflowed = false;
        if baseline == Baseline::Start {
            self.through = 0;
            self.attention = Attention::Unknown;
            self.phase = AgentPhase::Running;
            self.last_activity = None;
            self.todo = None;
            self.context = None;
            self.model = None;
            self.known_attention = false;
            self.known_phase = false;
            self.known_last_activity = false;
            self.known_todo = false;
            self.known_context = false;
            self.known_model = false;
            self.known_outstanding = true;
            self.tasks.clear();
        } else {
            self.attention = Attention::Unknown;
            self.known_attention = false;
            self.known_outstanding = false;
        }
    }

    fn apply(&mut self, input: Input<'_>) -> Changes<Self::Entry> {
        let mutations = match input {
            Input::Row {
                seq,
                activity_at,
                historical,
                payload,
                ..
            } => match serde_json::from_slice::<Value>(payload) {
                Ok(row) if !self.overflowed => self.row(seq, activity_at, historical, &row),
                Ok(_) => {
                    self.through = self.through.max(seq);
                    Vec::new()
                }
                Err(_) => {
                    self.through = self.through.max(seq);
                    vec![unrecognized(
                        seq,
                        0,
                        Revision::row(seq),
                        "invalid_json",
                        "row payload is not valid JSON",
                    )]
                }
            },
            Input::ReplayComplete { through, .. } => {
                self.through = self.through.max(through);
                Vec::new()
            }
            Input::ProcessExited { exit_code, .. } => {
                self.phase = AgentPhase::Exited { exit_code };
                self.known_phase = true;
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
                self.cursors.clear();
                self.pending_todos.clear();
                self.tasks.clear_pending();
                self.asks.clear();
                Vec::new()
            }
            Input::ObserverLost { .. } => {
                self.attention = Attention::Unknown;
                self.known_attention = false;
                self.known_outstanding = false;
                self.cursors.clear();
                self.pending_todos.clear();
                self.tasks.clear_pending();
                self.asks.clear();
                Vec::new()
            }
            Input::Tick { .. } => Vec::new(),
        };
        Changes {
            summary: Some(self.summary()),
            through: self.through,
            mutations,
        }
    }

    fn summary(&self) -> Summary {
        let mut unknown = Vec::new();
        if !self.known_attention {
            unknown.push(SummaryField::Attention);
        }
        if !self.known_phase {
            unknown.push(SummaryField::Phase);
        }
        if !self.known_last_activity {
            unknown.push(SummaryField::LastActivity);
        }
        if !self.known_todo {
            unknown.push(SummaryField::Todo);
        }
        if !self.known_context {
            unknown.push(SummaryField::Context);
        }
        if !self.known_model {
            unknown.push(SummaryField::Model);
        }
        if !self.known_outstanding {
            unknown.push(SummaryField::Outstanding);
        }
        Summary {
            attention: if self.known_attention {
                self.attention
            } else {
                Attention::Unknown
            },
            phase: self.phase.clone(),
            last_activity: self.last_activity,
            todo: self.todo.clone(),
            context: self.context.clone(),
            model: self.model.clone(),
            unknown,
        }
    }

    fn tip_bytes(&self) -> usize {
        let cursors = self
            .cursors
            .iter()
            .map(|cursor| {
                cursor.message_id.capacity()
                    + cursor
                        .parent_tool_use_id
                        .as_ref()
                        .map_or(0, String::capacity)
                    + cursor.blocks.capacity() * size_of::<BlockRef>()
                    + cursor
                        .blocks
                        .iter()
                        .map(|block| block.key.as_str().len())
                        .sum::<usize>()
            })
            .sum::<usize>();
        let todos = self
            .pending_todos
            .iter()
            .map(|todo| {
                todo.tool_use_id.capacity()
                    + todo.progress.current.as_ref().map_or(0, String::capacity)
            })
            .sum::<usize>();
        size_of::<Self>()
            + self.cursors.capacity() * size_of::<MessageCursor>()
            + self.pending_todos.capacity() * size_of::<PendingTodo>()
            + self.tasks.tip_bytes()
            + self.asks.capacity() * size_of::<PendingAsk>()
            + self
                .asks
                .iter()
                .map(|ask| {
                    ask.channel.capacity()
                        + ask.request_id.capacity()
                        + ask.row.payload.0.capacity()
                })
                .sum::<usize>()
            + self.model.as_ref().map_or(0, String::capacity)
            + cursors
            + todos
    }
}

fn partial(
    kind: ClaudeSdkEntryKind,
    body: ClaudeSdkBody,
    text: Option<String>,
    revision: Revision,
) -> ClaudeSdkPartial {
    ClaudeSdkPartial {
        kind: Patch::set(kind, revision),
        body: Patch::set(body, revision),
        text: text.map_or(Patch::Unchanged, |text| Patch::set(text, revision)),
        ..ClaudeSdkPartial::default()
    }
}

fn upsert(
    key: EntryKey,
    seq: u64,
    slot: u16,
    revision: Revision,
    entry: ClaudeSdkPartial,
) -> Mutation<ClaudeSdkEntry> {
    Mutation::Upsert {
        key,
        order: Order::new(seq, slot.min(1023)).expect("bounded SDK slot"),
        revision,
        entry,
    }
}

fn unrecognized(
    seq: u64,
    slot: u16,
    revision: Revision,
    row_type: &str,
    detail: &str,
) -> Mutation<ClaudeSdkEntry> {
    upsert(
        delivery_key(seq, slot),
        seq,
        slot,
        revision,
        partial(
            ClaudeSdkEntryKind::Unrecognized,
            ClaudeSdkBody::Unrecognized {
                row_type: clipped_text(row_type, 512),
                detail: clipped_text(detail, 512),
            },
            None,
            revision,
        ),
    )
}

fn clipped_marker(seq: u64, revision: Revision, detail: &str) -> Mutation<ClaudeSdkEntry> {
    let mut patch = partial(
        ClaudeSdkEntryKind::Unrecognized,
        ClaudeSdkBody::Unrecognized {
            row_type: "clipped".into(),
            detail: detail.into(),
        },
        None,
        revision,
    );
    patch.clipped = Patch::set(true, revision);
    upsert(delivery_key(seq, 1023), seq, 1023, revision, patch)
}

fn retained_block_count(blocks: usize) -> usize {
    if blocks > 1024 { 1023 } else { blocks }
}

fn delivery_key(seq: u64, slot: u16) -> EntryKey {
    EntryKey::new(format!("d:{seq}:{slot}")).expect("delivery key is bounded")
}

fn namespaced_key(namespace: &str, id: &str, seq: u64, slot: u16) -> EntryKey {
    EntryKey::new(format!("{namespace}:{id}")).unwrap_or_else(|_| delivery_key(seq, slot))
}

fn namespaced_or_delivery(namespace: &str, id: Option<&str>, seq: u64, slot: u16) -> EntryKey {
    id.map(|id| namespaced_key(namespace, id, seq, slot))
        .unwrap_or_else(|| delivery_key(seq, slot))
}

fn final_key(row_id: &str, slot: u16, seq: u64) -> EntryKey {
    EntryKey::new(format!("final:{row_id}:{slot}")).unwrap_or_else(|_| delivery_key(seq, slot))
}

fn occurrence_key(kind: &str, row: &Value, seq: u64, slot: u16) -> EntryKey {
    let id = row
        .get("uuid")
        .or_else(|| row.get("id"))
        .and_then(Value::as_str);
    id.map(|id| namespaced_key("occ", &format!("{kind}:{id}"), seq, slot))
        .unwrap_or_else(|| delivery_key(seq, slot))
}

fn string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| clipped_text(value, TEXT_MAX_BYTES))
}

fn id(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn bounded_id(value: String) -> String {
    clipped_text(&value, 512)
}

fn parse_task_state(value: String) -> TaskState {
    match value.as_str() {
        "running" | "in_progress" | "pending" => TaskState::Running,
        "completed" => TaskState::Completed,
        "failed" => TaskState::Failed,
        "stopped" | "killed" => TaskState::Stopped,
        _ => TaskState::Unknown(value),
    }
}

fn todo_progress(input: &Value) -> Option<TodoProgress> {
    let todos = input.get("todos")?.as_array()?;
    let mut done = 0;
    let mut current = None;
    for todo in todos {
        match todo.get("status").and_then(Value::as_str)? {
            "completed" => done += 1,
            "in_progress" if current.is_none() => {
                current = todo
                    .get("activeForm")
                    .or_else(|| todo.get("content"))
                    .and_then(Value::as_str)
                    .map(|value| clipped_text(value, 4096));
            }
            "pending" | "in_progress" => {}
            _ => return None,
        }
    }
    Some(TodoProgress {
        done,
        total: todos.len(),
        current,
    })
}

fn bounded_json(value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    if bytes.len() <= VALUE_MAX_BYTES {
        bytes
    } else {
        serde_json::to_vec(&serde_json::json!({"clipped":true,"bytes":bytes.len()}))
            .expect("marker serializes")
    }
}

fn append_text(out: &mut String, text: &str, max: usize) -> bool {
    let mut take = max.saturating_sub(out.len()).min(text.len());
    while !text.is_char_boundary(take) {
        take -= 1;
    }
    out.push_str(&text[..take]);
    take < text.len()
}

fn clipped_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let clipped = append_text(&mut out, text, max);
    if clipped {
        out.push_str("\n… clipped …");
    }
    out
}

fn truncate_versioned_string(field: &mut VersionedField<String>, max: usize) {
    if let Some(value) = field.value_mut() {
        *value = clipped_text(value, max);
    }
}

fn truncate_versioned_bytes(field: &mut VersionedField<JsonBytes>, max: usize) {
    if let Some(value) = field.value_mut()
        && value.0.len() > max
    {
        value.0 = serde_json::to_vec(&serde_json::json!({"clipped":true,"bytes":value.0.len()}))
            .expect("marker serializes");
    }
}

impl crate::private::Sealed for TaskState {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: TaskState| match value {
            TaskState::Running | TaskState::Completed | TaskState::Failed | TaskState::Stopped => {}
            TaskState::Unknown(value) => crate::assert_value_safe(&value),
        };
    }
}
impl PostcardSafe for TaskState {}

impl crate::private::Sealed for ClaudeSdkEntryKind {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ClaudeSdkEntryKind| match value {
            ClaudeSdkEntryKind::Prompt
            | ClaudeSdkEntryKind::Message
            | ClaudeSdkEntryKind::Thinking
            | ClaudeSdkEntryKind::Tool
            | ClaudeSdkEntryKind::Task
            | ClaudeSdkEntryKind::Turn
            | ClaudeSdkEntryKind::Compaction
            | ClaudeSdkEntryKind::AgentMessage
            | ClaudeSdkEntryKind::Status
            | ClaudeSdkEntryKind::Boundary
            | ClaudeSdkEntryKind::ApiError
            | ClaudeSdkEntryKind::Unrecognized => {}
        };
    }
}
impl PostcardSafe for ClaudeSdkEntryKind {}

impl crate::private::Sealed for ClaudeSdkBody {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ClaudeSdkBody| match value {
            ClaudeSdkBody::None | ClaudeSdkBody::ApiError => {}
            ClaudeSdkBody::Prompt {
                uuid,
                image_count,
                synthetic,
                replay,
            } => {
                crate::assert_value_safe(&uuid);
                crate::assert_value_safe(&image_count);
                crate::assert_value_safe(&synthetic);
                crate::assert_value_safe(&replay);
            }
            ClaudeSdkBody::Thinking { redacted } => crate::assert_value_safe(&redacted),
            ClaudeSdkBody::Tool {
                tool_use_id,
                parent_tool_use_id,
            } => {
                crate::assert_value_safe(&tool_use_id);
                crate::assert_value_safe(&parent_tool_use_id);
            }
            ClaudeSdkBody::Task { task_id } => {
                crate::assert_value_safe(&task_id);
            }
            ClaudeSdkBody::Turn {
                uuid,
                outcome,
                is_error,
                stop_reason,
            } => {
                crate::assert_value_safe(&uuid);
                crate::assert_value_safe(&outcome);
                crate::assert_value_safe(&is_error);
                crate::assert_value_safe(&stop_reason);
            }
            ClaudeSdkBody::Compaction {
                trigger,
                pre_tokens,
                post_tokens,
            } => {
                crate::assert_value_safe(&trigger);
                crate::assert_value_safe(&pre_tokens);
                crate::assert_value_safe(&post_tokens);
            }
            ClaudeSdkBody::AgentMessage {
                id,
                context,
                from,
                kind,
                delivery,
            } => {
                crate::assert_value_safe(&id);
                crate::assert_value_safe(&context);
                crate::assert_value_safe(&from);
                crate::assert_value_safe(&kind);
                crate::assert_value_safe(&delivery);
            }
            ClaudeSdkBody::Status { status } => crate::assert_value_safe(&status),
            ClaudeSdkBody::Boundary {
                boundary,
                session_id,
            } => {
                crate::assert_value_safe(&boundary);
                crate::assert_value_safe(&session_id);
            }
            ClaudeSdkBody::Unrecognized { row_type, detail } => {
                crate::assert_value_safe(&row_type);
                crate::assert_value_safe(&detail);
            }
        };
    }
}
impl PostcardSafe for ClaudeSdkBody {}

impl crate::private::Sealed for FinalComponents {
    fn assert_fields_are_postcard_safe() {
        let _ = |FinalComponents {
                     through,
                     revision,
                     values,
                 }: FinalComponents| {
            crate::assert_value_safe(&through);
            crate::assert_value_safe(&revision);
            crate::assert_value_safe(&values);
        };
    }
}
impl PostcardSafe for FinalComponents {}

impl crate::private::Sealed for ClaudeSdkPartial {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudeSdkPartial {
                     kind,
                     body,
                     text,
                     components,
                     final_components,
                     finality,
                     parent_tool_use_id,
                     tool_name,
                     tool_input,
                     tool_outcome,
                     task_description,
                     task_tool_use_id,
                     task_subagent,
                     task_state,
                     task_last_tool,
                     task_summary,
                     task_usage,
                     clipped,
                     incomplete,
                 }: ClaudeSdkPartial| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&final_components);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&parent_tool_use_id);
            crate::assert_value_safe(&tool_name);
            crate::assert_value_safe(&tool_input);
            crate::assert_value_safe(&tool_outcome);
            crate::assert_value_safe(&task_description);
            crate::assert_value_safe(&task_tool_use_id);
            crate::assert_value_safe(&task_subagent);
            crate::assert_value_safe(&task_state);
            crate::assert_value_safe(&task_last_tool);
            crate::assert_value_safe(&task_summary);
            crate::assert_value_safe(&task_usage);
            crate::assert_value_safe(&clipped);
            crate::assert_value_safe(&incomplete);
        };
    }
}
impl PostcardSafe for ClaudeSdkPartial {}

impl crate::private::Sealed for ClaudeSdkEntry {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudeSdkEntry {
                     kind,
                     body,
                     text,
                     components,
                     finality,
                     parent_tool_use_id,
                     tool_name,
                     tool_input,
                     tool_outcome,
                     task_description,
                     task_tool_use_id,
                     task_subagent,
                     task_state,
                     task_last_tool,
                     task_summary,
                     task_usage,
                     clipped,
                     incomplete,
                     rendered_text,
                 }: ClaudeSdkEntry| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&parent_tool_use_id);
            crate::assert_value_safe(&tool_name);
            crate::assert_value_safe(&tool_input);
            crate::assert_value_safe(&tool_outcome);
            crate::assert_value_safe(&task_description);
            crate::assert_value_safe(&task_tool_use_id);
            crate::assert_value_safe(&task_subagent);
            crate::assert_value_safe(&task_state);
            crate::assert_value_safe(&task_last_tool);
            crate::assert_value_safe(&task_summary);
            crate::assert_value_safe(&task_usage);
            crate::assert_value_safe(&clipped);
            crate::assert_value_safe(&incomplete);
            crate::assert_value_safe(&rendered_text);
        };
    }
}
impl PostcardSafe for ClaudeSdkEntry {}

impl crate::private::Sealed for BlockRef {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<u64>();
        crate::assert_postcard_safe::<EntryKey>();
    }
}
impl PostcardSafe for BlockRef {}

impl crate::private::Sealed for MessageCursor {
    fn assert_fields_are_postcard_safe() {
        let _ = |MessageCursor {
                     message_id,
                     parent_tool_use_id,
                     next_final_index,
                     streaming,
                     blocks,
                 }: MessageCursor| {
            crate::assert_value_safe(&message_id);
            crate::assert_value_safe(&parent_tool_use_id);
            crate::assert_value_safe(&next_final_index);
            crate::assert_value_safe(&streaming);
            crate::assert_value_safe(&blocks);
        };
    }
}
impl PostcardSafe for MessageCursor {}

impl crate::private::Sealed for PendingTodo {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<TodoProgress>();
    }
}
impl PostcardSafe for PendingTodo {}

impl crate::private::Sealed for PendingAsk {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<RestoreRow>();
    }
}
impl PostcardSafe for PendingAsk {}

impl crate::private::Sealed for ClaudeSdkFold {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudeSdkFold {
                     segment,
                     baseline,
                     through,
                     cursors,
                     pending_todos,
                     tasks,
                     asks,
                     in_history,
                     attention,
                     phase,
                     last_activity,
                     todo,
                     context,
                     model,
                     known_attention,
                     known_phase,
                     known_last_activity,
                     known_todo,
                     known_context,
                     known_model,
                     known_outstanding,
                     overflowed,
                 }: ClaudeSdkFold| {
            crate::assert_value_safe(&segment);
            crate::assert_value_safe(&baseline);
            crate::assert_value_safe(&through);
            crate::assert_value_safe(&cursors);
            crate::assert_value_safe(&pending_todos);
            crate::assert_value_safe(&tasks);
            crate::assert_value_safe(&asks);
            crate::assert_value_safe(&in_history);
            crate::assert_value_safe(&attention);
            crate::assert_value_safe(&phase);
            crate::assert_value_safe(&last_activity);
            crate::assert_value_safe(&todo);
            crate::assert_value_safe(&context);
            crate::assert_value_safe(&model);
            crate::assert_value_safe(&known_attention);
            crate::assert_value_safe(&known_phase);
            crate::assert_value_safe(&known_last_activity);
            crate::assert_value_safe(&known_todo);
            crate::assert_value_safe(&known_context);
            crate::assert_value_safe(&known_model);
            crate::assert_value_safe(&known_outstanding);
            crate::assert_value_safe(&overflowed);
        };
    }
}
impl PostcardSafe for ClaudeSdkFold {}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::MutationOracle;

    const CONVERSE: &str =
        include_str!("../../../ui-state/tests/spec/fixtures/claude_sdk/converse.rows.jsonl");
    const TASK_TOOL_CAPTURE: &str = include_str!("../../fixtures/claude-task-tools-2.1.273.jsonl");

    fn at(seq: u64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_760_000_000 + seq as i64, 0)
            .single()
            .unwrap()
    }

    fn rows(raw: &str) -> Vec<Vec<u8>> {
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.as_bytes().to_vec())
            .collect()
    }

    fn apply_row(
        fold: &mut ClaudeSdkFold,
        oracle: &mut MutationOracle<ClaudeSdkEntry>,
        seq: u64,
        historical: bool,
        payload: &[u8],
    ) {
        let changes = fold.apply(Input::Row {
            seq,
            published_at: at(seq),
            activity_at: Some(at(seq)),
            historical,
            payload,
        });
        oracle.apply_changes(&changes).unwrap();
        assert!(
            fold.tip_bytes() <= ClaudeSdkFold::TIP_BUDGET,
            "tip grew to {} bytes after row {seq}",
            fold.tip_bytes()
        );
    }

    fn fold_rows(input: &[Vec<u8>]) -> (ClaudeSdkFold, MutationOracle<ClaudeSdkEntry>) {
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().enumerate() {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, false, payload);
        }
        (fold, oracle)
    }

    fn keys(oracle: &MutationOracle<ClaudeSdkEntry>) -> Vec<String> {
        oracle
            .entries()
            .into_iter()
            .map(|entry| entry.key.into_string())
            .collect()
    }

    #[test]
    fn claude_sdk_continuation_matches_uninterrupted_at_every_corpus_cut() {
        let input = rows(CONVERSE);
        let (expected_fold, expected_oracle) = fold_rows(&input);
        let expected_entries = expected_oracle.entries();
        let mut prefix_fold = ClaudeSdkFold::default();
        prefix_fold.begin(1, Baseline::Start);
        let mut prefix_oracle = MutationOracle::default();
        for cut in 0..=input.len() {
            if cut > 0 {
                apply_row(
                    &mut prefix_fold,
                    &mut prefix_oracle,
                    cut as u64,
                    false,
                    &input[cut - 1],
                );
            }
            let bytes = postcard::to_allocvec(&prefix_fold).unwrap();
            let mut resumed: ClaudeSdkFold = postcard::from_bytes(&bytes).unwrap();
            let mut materialized = prefix_oracle.clone();
            for (index, payload) in input.iter().enumerate().skip(cut) {
                apply_row(
                    &mut resumed,
                    &mut materialized,
                    index as u64 + 1,
                    false,
                    payload,
                );
            }
            assert_eq!(resumed, expected_fold, "tip differs at cut {cut}");
            assert_eq!(
                resumed.summary(),
                expected_fold.summary(),
                "summary at {cut}"
            );
            assert_eq!(materialized.entries(), expected_entries, "entries at {cut}");
            assert_eq!(
                materialized.redirects(),
                expected_oracle.redirects(),
                "redirects at {cut}"
            );
        }
    }

    #[test]
    fn claude_sdk_singleton_and_whole_finals_use_row_identity() {
        let input = rows(
            r#"
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"hel"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}},"parent_tool_use_id":null}
{"type":"assistant","uuid":"r1","parent_tool_use_id":null,"message":{"id":"m","content":[{"type":"text","text":"hello"}]}}
{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"why"}},"parent_tool_use_id":null}
{"type":"assistant","uuid":"r2","parent_tool_use_id":null,"message":{"id":"m","content":[{"type":"thinking","thinking":"why"}]}}
{"type":"assistant","uuid":"r3","parent_tool_use_id":null,"message":{"id":"whole","content":[{"type":"text","text":"a"},{"type":"thinking","thinking":"b"}]}}
"#,
        );
        let (_, oracle) = fold_rows(&input);
        assert_eq!(
            keys(&oracle),
            ["final:r1:0", "final:r2:0", "final:r3:0", "final:r3:1"]
        );
        assert_eq!(
            oracle
                .redirects()
                .iter()
                .map(|(from, to)| (from.as_str(), to.as_str()))
                .collect::<Vec<_>>(),
            [("blk:m:0", "final:r1:0"), ("blk:m:1", "final:r2:0")]
        );
        assert_eq!(oracle.entries()[0].entry.text(), Some("hello"));
    }

    #[test]
    fn claude_sdk_final_only_clients_converge_on_a19_canonical_identity() {
        let prefix = rows(
            r#"
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"answer"}},"parent_tool_use_id":null}
"#,
        );
        let final_row = serde_json::to_vec(&json!({
            "type":"assistant","uuid":"row-4001","parent_tool_use_id":null,
            "message":{"id":"m","content":[{"type":"text","text":"answer"}]}
        }))
        .unwrap();
        let mut warm_fold = ClaudeSdkFold::default();
        warm_fold.begin(1, Baseline::Start);
        let mut warm = MutationOracle::default();
        apply_row(&mut warm_fold, &mut warm, 3_988, false, &prefix[0]);
        apply_row(&mut warm_fold, &mut warm, 3_990, false, &prefix[1]);
        apply_row(&mut warm_fold, &mut warm, 4_001, false, &final_row);
        let mut replay_fold = ClaudeSdkFold::default();
        replay_fold.begin(1, Baseline::Truncated { from: 3_200 });
        let mut replay = MutationOracle::default();
        apply_row(&mut replay_fold, &mut replay, 3_988, false, &prefix[0]);
        apply_row(&mut replay_fold, &mut replay, 3_990, false, &prefix[1]);
        apply_row(&mut replay_fold, &mut replay, 4_001, false, &final_row);
        let mut cold_fold = ClaudeSdkFold::default();
        cold_fold.begin(1, Baseline::Truncated { from: 4_001 });
        let mut cold = MutationOracle::default();
        apply_row(&mut cold_fold, &mut cold, 4_001, false, &final_row);
        assert_eq!(keys(&warm), ["final:row-4001:0"]);
        assert_eq!(keys(&replay), ["final:row-4001:0"]);
        assert_eq!(keys(&cold), ["final:row-4001:0"]);
        assert_eq!(warm.entries()[0].order, Order::new(3_990, 0).unwrap());
        assert_eq!(replay.entries()[0].order, Order::new(3_990, 0).unwrap());
        assert_eq!(cold.entries()[0].order, Order::new(4_001, 0).unwrap());
        assert_eq!(warm.entries()[0].entry, replay.entries()[0].entry);
        assert_eq!(warm.entries()[0].entry, cold.entries()[0].entry);
    }

    #[test]
    fn claude_sdk_whole_final_keeps_streamed_text_before_later_tool() {
        let input = rows(
            r#"
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}},"parent_tool_use_id":null}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"answer"}},"parent_tool_use_id":null}
{"type":"assistant","uuid":"row","parent_tool_use_id":null,"message":{"id":"m","content":[{"type":"text","text":"answer"},{"type":"tool_use","id":"tool-1","name":"Read","input":{}}]}}
"#,
        );
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        apply_row(&mut fold, &mut oracle, 10, false, &input[0]);
        apply_row(&mut fold, &mut oracle, 12, false, &input[1]);

        let changes = fold.apply(Input::Row {
            seq: 20,
            published_at: at(20),
            activity_at: Some(at(20)),
            historical: false,
            payload: &input[2],
        });
        assert!(matches!(
            &changes.mutations[..],
            [
                Mutation::Upsert { key, .. },
                Mutation::Alias { from, to, .. },
                Mutation::Upsert { .. }
            ] if key.as_str() == "blk:m:0"
                && from.as_str() == "blk:m:0"
                && to.as_str() == "final:row:0"
        ));
        oracle.apply_changes(&changes).unwrap();

        let entries = oracle.entries();
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.key.as_str(), entry.order))
                .collect::<Vec<_>>(),
            [
                ("final:row:0", Order::new(12, 0).unwrap()),
                ("tool:tool-1", Order::new(20, 1).unwrap()),
            ]
        );
    }

    #[test]
    fn claude_sdk_missing_cursor_and_prompt_uuid_are_delivery_keyed() {
        let input = rows(
            r#"
{"type":"stream_event","event":{"type":"content_block_start","index":4,"content_block":{"type":"text","text":"orphan"}},"parent_tool_use_id":null}
{"type":"user","parent_tool_use_id":null,"message":{"content":"hello"}}
"#,
        );
        let (_, oracle) = fold_rows(&input);
        assert_eq!(keys(&oracle), ["d:1:0", "d:2:0"]);
        assert!(oracle.entries()[0].entry.is_incomplete());
        assert_eq!(
            DELIVERY_KEYED_VARIANTS,
            [
                "claude_sdk.prompt_without_uuid",
                "claude_sdk.stream_block_without_message_start",
                "claude_sdk.occurrence_without_uuid"
            ]
        );
    }

    #[test]
    fn claude_sdk_delivery_slots_are_unique_with_mixed_and_clipped_user_blocks() {
        let mixed = serde_json::to_vec(&json!({
            "type": "user",
            "message": {"content": [
                {"type": "future_block"},
                {"type": "text", "text": "hello"}
            ]}
        }))
        .unwrap();
        let (_, mixed_oracle) = fold_rows(&[mixed]);
        assert_eq!(keys(&mixed_oracle), ["d:1:0", "d:1:1"]);

        let clipped = serde_json::to_vec(&json!({
            "type": "user",
            "message": {
                "content": (0..1025)
                    .map(|_| json!({"type": "future_block"}))
                    .collect::<Vec<_>>()
            }
        }))
        .unwrap();
        let (_, clipped_oracle) = fold_rows(&[clipped]);
        let entries = clipped_oracle.entries();
        assert_eq!(entries.len(), 1024);
        assert_eq!(entries[1022].key.as_str(), "d:1:1022");
        assert_eq!(entries[1023].key.as_str(), "d:1:1023");
        assert!(matches!(
            entries[1023].entry.body(),
            Some(ClaudeSdkBody::Unrecognized { row_type, detail })
                if row_type == "clipped" && detail == "user row clipped"
        ));
    }

    #[test]
    fn claude_sdk_identity_covers_every_a5_creating_form() {
        let cases = [
            (
                json!({"type":"user","uuid":"u","message":{"content":"hello"}}),
                "user:u",
            ),
            (
                json!({"type":"assistant","uuid":"r","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"Read","input":{}}]}}),
                "tool:t",
            ),
            (
                json!({"type":"user","uuid":"result","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
                "tool:t",
            ),
            (
                json!({"type":"system","subtype":"task_started","task_id":"task","status":"running"}),
                "task:task",
            ),
            (
                json!({"type":"result","uuid":"turn","subtype":"success"}),
                "turn:turn",
            ),
            (
                json!({"type":"amux.claude_sdk.message","envelope":{"id":"e","from":{"type":"human"},"text":"hi"}}),
                "env:e",
            ),
            (
                json!({"type":"user","uuid":"agent-user","message":{"content":"<agent-message from=\"worker/local\" kind=\"message\">hi</agent-message>"}}),
                "user:agent-user",
            ),
            (
                json!({"type":"system","subtype":"compact_boundary","uuid":"c","compact_metadata":{}}),
                "occ:compact:c",
            ),
            (
                json!({"type":"system","subtype":"status","status":"compacting","uuid":"s"}),
                "occ:compacting:s",
            ),
            (
                json!({"type":"amux.claude_sdk.ready","uuid":"b","session_id":"s"}),
                "occ:ready:b",
            ),
            (json!({"type":"future","uuid":"z"}), "occ:raw:z"),
        ];
        for (row, expected) in cases {
            let (_, oracle) = fold_rows(&[serde_json::to_vec(&row).unwrap()]);
            assert_eq!(keys(&oracle), [expected], "{row}");
        }

        let streamed = rows(
            r#"
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"hi"}}}
"#,
        );
        let (_, oracle) = fold_rows(&streamed);
        assert_eq!(keys(&oracle), ["blk:m:0"]);
    }

    #[test]
    fn claude_sdk_task_patch_is_row_addressable_and_launch_adopts_standalone() {
        let input = rows(
            r#"
{"type":"system","subtype":"task_started","task_id":"task-1","description":"inspect","status":"running"}
{"type":"system","subtype":"task_updated","task_id":"task-1","patch":{"status":"completed","summary":"done"}}
{"type":"assistant","uuid":"launch-row","message":{"id":"m","content":[{"type":"tool_use","id":"launch-1","name":"Task","input":{"description":"inspect"}}]}}
{"type":"system","subtype":"task_progress","task_id":"task-1","tool_use_id":"launch-1","last_tool":"Read"}
"#,
        );
        let (_, oracle) = fold_rows(&input);
        assert_eq!(keys(&oracle), ["tool:launch-1"]);
        let task = &oracle.entries()[0].entry;
        assert_eq!(task.entry_kind(), Some(ClaudeSdkEntryKind::Task));
        assert_eq!(task.task_description(), Some("inspect"));
        assert_eq!(task.task_state(), Some(&TaskState::Completed));
        assert_eq!(
            oracle
                .redirects()
                .iter()
                .map(|(from, to)| (from.as_str(), to.as_str()))
                .collect::<Vec<_>>(),
            [("task:task-1", "tool:launch-1")]
        );

        let late_launch = rows(
            r#"
{"type":"system","subtype":"task_started","task_id":"task-2","tool_use_id":"launch-2","description":"inspect","status":"running"}
{"type":"assistant","uuid":"launch-row-2","message":{"id":"m2","content":[{"type":"tool_use","id":"launch-2","name":"Task","input":{"description":"inspect"}}]}}
{"type":"system","subtype":"task_updated","task_id":"task-2","patch":{"status":"completed"}}
"#,
        );
        let (_, oracle) = fold_rows(&late_launch);
        assert_eq!(keys(&oracle), ["tool:launch-2"]);
        assert_eq!(
            oracle.entries()[0].entry.entry_kind(),
            Some(ClaudeSdkEntryKind::Task)
        );
        assert_eq!(
            oracle.entries()[0].entry.task_tool_use_id(),
            Some("launch-2")
        );
        assert_eq!(
            oracle.entries()[0].entry.task_state(),
            Some(&TaskState::Completed)
        );
    }

    #[test]
    fn claude_sdk_historical_rows_restore_facts_without_obligations_and_ready_keeps_todos() {
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        let history = [
            json!({"type":"amux.claude_sdk.history_begin"}),
            json!({"type":"assistant","uuid":"todo-row","message":{"id":"m","content":[{"type":"tool_use","id":"todo","name":"TodoWrite","input":{"todos":[{"content":"ship","activeForm":"shipping","status":"in_progress"}]}}]}}),
            json!({"type":"user","uuid":"result","message":{"content":[{"type":"tool_result","tool_use_id":"todo","content":"ok"}]}}),
            json!({"type":"assistant","uuid":"old-permission-row","message":{"id":"old-permission-message","content":[{"type":"tool_use","id":"old-write","name":"Write","input":{"file_path":"/tmp/old","content":"old"}}]}}),
            json!({"type":"amux.claude_sdk.history_complete"}),
            json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":true}),
        ];
        for (index, row) in history.iter().enumerate() {
            apply_row(
                &mut fold,
                &mut oracle,
                index as u64 + 1,
                index > 0 && index < 4,
                &serde_json::to_vec(row).unwrap(),
            );
        }
        assert_eq!(
            fold.summary().todo.as_ref().unwrap().current.as_deref(),
            Some("shipping")
        );
        assert_eq!(fold.summary().attention, Attention::Idle);
        assert!(fold.asks.is_empty());
        assert!(!fold.summary().unknown.contains(&SummaryField::Outstanding));
        let entries = oracle.entries();
        let old_permission = entries
            .iter()
            .find(|entry| entry.key.as_str() == "tool:old-write")
            .expect("historical permission tool remains visible");
        assert_eq!(
            old_permission.entry.entry_kind(),
            Some(ClaudeSdkEntryKind::Tool)
        );
    }

    #[test]
    fn claude_sdk_real_task_tool_history_survives_resume_ready_and_resets() {
        let input = rows(TASK_TOOL_CAPTURE);
        assert!(
            input.iter().all(|row| {
                serde_json::from_slice::<Value>(row).unwrap()["version"] == "2.1.273"
            })
        );

        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().take(2).enumerate() {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, true, payload);
        }
        let bytes = postcard::to_allocvec(&fold).unwrap();
        let mut fold: ClaudeSdkFold = postcard::from_bytes(&bytes).unwrap();
        for (index, payload) in input.iter().enumerate().skip(2) {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, true, payload);
        }
        let ready = serde_json::to_vec(
            &json!({"type":"amux.claude_sdk.ready","session_id":"captured","resumed":true}),
        )
        .unwrap();
        apply_row(
            &mut fold,
            &mut oracle,
            input.len() as u64 + 1,
            false,
            &ready,
        );

        assert_eq!(
            fold.summary().todo,
            Some(TodoProgress {
                done: 0,
                total: 1,
                current: Some("Resuming from the live checklist".into()),
            })
        );
        let denied = oracle
            .entries()
            .into_iter()
            .find(|entry| entry.entry.tool_name() == Some("Write"))
            .expect("captured Write remains visible");
        let outcome: Value =
            serde_json::from_slice(&denied.entry.tool_outcome().unwrap().0).unwrap();
        assert_eq!(outcome["is_error"], true);

        let reset = serde_json::to_vec(&json!({"type":"conversation_reset"})).unwrap();
        apply_row(
            &mut fold,
            &mut oracle,
            input.len() as u64 + 2,
            false,
            &reset,
        );
        assert_eq!(fold.summary().todo, None);
        assert!(!fold.summary().unknown.contains(&SummaryField::Todo));
    }

    #[test]
    fn claude_sdk_lifecycle_and_tip_budget_follow_the_shared_contract() {
        let huge = "x".repeat(200_000);
        let row = serde_json::to_vec(&json!({
            "type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}
        }))
        .unwrap();
        let block = serde_json::to_vec(&json!({
            "type":"stream_event","event":{"type":"content_block_start","index":0,
                "content_block":{"type":"text","text":huge}}
        }))
        .unwrap();
        let (mut fold, oracle) = fold_rows(&[row, block]);
        assert!(fold.tip_bytes() < 64 * 1024, "feed body leaked into tip");
        assert!(oracle.entries()[0].entry.text().unwrap().len() < 70 * 1024);
        let bytes = postcard::to_allocvec(&fold).unwrap();
        let decoded: ClaudeSdkFold = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, fold);
        let entry_bytes = postcard::to_allocvec(&oracle.entries()[0].entry).unwrap();
        let _: ClaudeSdkEntry = postcard::from_bytes(&entry_bytes).unwrap();

        fold.apply(Input::ObserverLost { at: at(10) });
        assert!(fold.summary().unknown.contains(&SummaryField::Attention));
        assert!(fold.summary().unknown.contains(&SummaryField::Outstanding));
        fold.apply(Input::ProcessExited {
            exit_code: Some(9),
            at: at(11),
        });
        assert_eq!(
            fold.summary().phase,
            AgentPhase::Exited { exit_code: Some(9) }
        );
        assert_eq!(fold.summary().attention, Attention::Idle);
    }
}
