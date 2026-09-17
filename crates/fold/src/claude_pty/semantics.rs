//! Durable Claude PTY fold semantics.
//!
//! The observation module above remains the renderer-facing vocabulary. This
//! module owns the smaller postcard-safe tip and the partial entries written
//! to the shared store. Feed bodies never live in the tip: only identities
//! which can still be amended, the dedupe ring, and summary facts do.

use std::mem::size_of;

use chrono::{DateTime, Utc};
use model::{
    AgentMessageKind, AgentPhase, Attention, ContextMeter, ContextMeterSource, StructuredProtocol,
    Summary, SummaryField, TodoProgress, Why,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RowKind, classify_row, facts, prompt_source};
use crate::claude_tasks::TaskRegistry;
use crate::{
    Baseline, Changes, Component, ComponentSource, Components, Entry, EntryKey, FieldPatch, Input,
    JsonBytes, MergeDefect, Mutation, Order, Patch, PostcardSafe, ProviderFold, RestoreRow,
    Revision, SegmentId, TIP_MAX_BYTES, TIP_MAX_OPEN_ENTRIES, VersionedField,
};

const OUTPUT_HEAD_BYTES: usize = 4096;

/// PTY forms whose creating row cannot always carry a provider-native key.
/// The cross-provider identity gate prints these names verbatim.
pub const DELIVERY_KEYED_VARIANTS: &[&str] = &[
    "claude_pty.message_without_message_id",
    "claude_pty.unrecognized_without_uuid",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeEntryKind {
    Prompt,
    Message,
    Thinking,
    Tool,
    Turn,
    Compaction,
    CompactSummary,
    TaskNotification,
    Interruption,
    AgentMessage,
    ApiError,
    #[default]
    Unrecognized,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeBody {
    #[default]
    None,
    Prompt {
        source: String,
        prompt_id: Option<String>,
    },
    Thinking {
        duration_ms: Option<i64>,
        redacted: bool,
    },
    Turn {
        duration_ms: i64,
        inferred: bool,
        message_count: Option<u64>,
        pending_background_agents: Option<u64>,
    },
    Compaction {
        trigger: Option<String>,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },
    Tool {
        tool_use_id: String,
    },
    AgentMessage {
        id: Option<String>,
        context: Option<String>,
        from: String,
        kind: AgentMessageKind,
    },
    ApiError {
        error: Option<String>,
    },
    Unrecognized {
        row_type: Option<String>,
        detail: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudePartial {
    pub kind: FieldPatch<ClaudeEntryKind>,
    pub body: FieldPatch<ClaudeBody>,
    pub text: FieldPatch<String>,
    pub components: Vec<Component<String>>,
    pub finality: FieldPatch<String>,
    pub tool_name: FieldPatch<String>,
    pub tool_input: FieldPatch<JsonBytes>,
    pub tool_outcome: FieldPatch<JsonBytes>,
    pub message_final: FieldPatch<bool>,
    pub clipped: FieldPatch<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeEntry {
    kind: VersionedField<ClaudeEntryKind>,
    body: VersionedField<ClaudeBody>,
    text: VersionedField<String>,
    components: Components<String>,
    finality: VersionedField<String>,
    tool_name: VersionedField<String>,
    tool_input: VersionedField<JsonBytes>,
    tool_outcome: VersionedField<JsonBytes>,
    message_final: VersionedField<bool>,
    clipped: VersionedField<bool>,
    rendered_text: String,
}

impl ClaudeEntry {
    pub fn entry_kind(&self) -> Option<ClaudeEntryKind> {
        self.kind.value().copied()
    }

    pub fn body(&self) -> Option<&ClaudeBody> {
        self.body.value()
    }

    pub fn components(&self) -> &Components<String> {
        &self.components
    }

    pub fn finality(&self) -> Option<&str> {
        self.finality.value().map(String::as_str)
    }

    pub fn tool_outcome(&self) -> Option<&JsonBytes> {
        self.tool_outcome.value()
    }

    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.value().map(String::as_str)
    }

    pub fn tool_input(&self) -> Option<&JsonBytes> {
        self.tool_input.value()
    }

    pub fn message_final(&self) -> bool {
        self.message_final.value().copied().unwrap_or(false)
    }

    pub fn is_clipped(&self) -> bool {
        self.clipped.value().copied().unwrap_or(false) || self.components.is_clipped()
    }

    fn rebuild_text(&mut self) {
        if !self.components.values().is_empty() {
            self.rendered_text = ordered_components(self.components.values()).join("\n\n");
        } else {
            self.rendered_text = self.text.value().cloned().unwrap_or_default();
        }
        if self.is_clipped() && !self.rendered_text.ends_with("… clipped …") {
            self.rendered_text.push_str("\n… clipped …");
        }
    }
}

impl Entry for ClaudeEntry {
    type Partial = ClaudePartial;

    fn kind(&self) -> &'static str {
        match self.kind.value() {
            Some(ClaudeEntryKind::Prompt) => "prompt",
            Some(ClaudeEntryKind::Message) => "message",
            Some(ClaudeEntryKind::Thinking) => "thinking",
            Some(ClaudeEntryKind::Tool) => "tool",
            Some(ClaudeEntryKind::Turn) => "turn",
            Some(ClaudeEntryKind::Compaction) => "compaction",
            Some(ClaudeEntryKind::CompactSummary) => "compact_summary",
            Some(ClaudeEntryKind::TaskNotification) => "task_notification",
            Some(ClaudeEntryKind::Interruption) => "interruption",
            Some(ClaudeEntryKind::AgentMessage) => "agent_message",
            Some(ClaudeEntryKind::ApiError) => "api_error",
            Some(ClaudeEntryKind::Unrecognized) | None => "unrecognized",
        }
    }

    fn text(&self) -> Option<&str> {
        (!self.rendered_text.is_empty()).then_some(self.rendered_text.as_str())
    }

    fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect> {
        self.kind.merge("kind", &patch.kind)?;
        self.body.merge("body", &patch.body)?;
        self.text.merge("text", &patch.text)?;
        self.finality.merge("finality", &patch.finality)?;
        self.tool_name.merge("tool_name", &patch.tool_name)?;
        self.tool_input.merge("tool_input", &patch.tool_input)?;
        self.tool_outcome
            .merge("tool_outcome", &patch.tool_outcome)?;
        self.message_final
            .merge("message_final", &patch.message_final)?;
        self.clipped.merge("clipped", &patch.clipped)?;
        for component in &patch.components {
            self.components.merge(component.clone())?;
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
        _promotion: Option<crate::Promotion>,
    ) -> Result<(), MergeDefect> {
        self.kind.fill_unknown_from(&source.kind);
        self.body.fill_unknown_from(&source.body);
        self.text.fill_unknown_from(&source.text);
        self.finality.fill_unknown_from(&source.finality);
        self.tool_name.fill_unknown_from(&source.tool_name);
        self.tool_input.fill_unknown_from(&source.tool_input);
        self.tool_outcome.fill_unknown_from(&source.tool_outcome);
        self.message_final.fill_unknown_from(&source.message_final);
        self.clipped.fill_unknown_from(&source.clipped);
        for component in source.components.values() {
            self.components.merge(component.clone())?;
        }
        self.rebuild_text();
        Ok(())
    }

    fn promote(&mut self, _promotion: Option<crate::Promotion>) -> Result<(), MergeDefect> {
        Ok(())
    }

    fn clip(&mut self, budget: usize) {
        self.components.clip_by(256, budget / 2, |component| {
            component.value.len()
                + component.after.capacity() * size_of::<ComponentSource>()
                + component_source_bytes(&component.source)
        });
        truncate_versioned_string(&mut self.text, 64 * 1024);
        truncate_versioned_bytes(&mut self.tool_input, 64 * 1024);
        truncate_versioned_bytes(&mut self.tool_outcome, 64 * 1024);
        self.rebuild_text();
        if self.bytes() > budget {
            let revision = self
                .kind
                .revision()
                .or(self.body.revision())
                .unwrap_or(Revision::row(0));
            let _ = self.clipped.merge("clipped", &Patch::set(true, revision));
            self.rendered_text = clipped_head(&self.rendered_text, budget / 4);
        }
    }

    fn bytes(&self) -> usize {
        postcard::to_allocvec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OpenMessage {
    id: String,
    key: EntryKey,
    last_component: Option<ComponentSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingTodo {
    tool_use_id: String,
    progress: TodoProgress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingAsk {
    row: RestoreRow,
    tool_use_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeFold {
    segment: SegmentId,
    baseline: Baseline,
    through: u64,
    messages: Vec<OpenMessage>,
    open_tools: Vec<(String, EntryKey, Option<String>)>,
    pending_todos: Vec<PendingTodo>,
    tasks: TaskRegistry,
    asks: Vec<PendingAsk>,
    inferred_turn: Option<EntryKey>,
    previous_row_at: Option<DateTime<Utc>>,
    prompt_at: Option<DateTime<Utc>>,
    turn_closed_at: Option<DateTime<Utc>>,
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

impl Default for ClaudeFold {
    fn default() -> Self {
        Self {
            segment: 0,
            baseline: Baseline::Start,
            through: 0,
            messages: Vec::new(),
            open_tools: Vec::new(),
            pending_todos: Vec::new(),
            tasks: TaskRegistry::default(),
            asks: Vec::new(),
            inferred_turn: None,
            previous_row_at: None,
            prompt_at: None,
            turn_closed_at: None,
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

impl ClaudeFold {
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

    pub fn restored_obligations(&self) -> impl Iterator<Item = (&RestoreRow, Option<&str>)> {
        self.asks
            .iter()
            .map(|ask| (&ask.row, ask.tool_use_id.as_deref()))
    }

    fn row(
        &mut self,
        seq: u64,
        activity_at: Option<DateTime<Utc>>,
        row: &Value,
    ) -> Vec<Mutation<ClaudeEntry>> {
        self.through = self.through.max(seq);
        if !self.known_phase {
            self.phase = AgentPhase::Running;
            self.known_phase = true;
        }
        if let Some(at) = activity_at {
            self.last_activity = Some(at);
            self.known_last_activity = true;
        }

        let kind = classify_row(row);
        let breaks_duration_chain = match kind {
            RowKind::User => row
                .pointer("/message/content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        matches!(
                            block.get("text").and_then(Value::as_str),
                            Some(
                                "[Request interrupted by user]"
                                    | "[Request interrupted by user for tool use]"
                            )
                        )
                    })
                }),
            RowKind::System => {
                row.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
            }
            _ => false,
        };
        let revision = Revision::row(seq);
        let mut mutations = Vec::new();
        match kind {
            RowKind::TranscriptReady => {
                self.asks.clear();
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
            }
            RowKind::HookStop => {
                self.asks.clear();
                self.attention = Attention::NeedsYou { why: Why::Finished };
                self.known_attention = true;
                self.known_outstanding = true;
                self.turn_closed_at = activity_at;
            }
            RowKind::HookPermissionRequest => {
                self.asks.push(PendingAsk {
                    row: RestoreRow {
                        seq,
                        payload: JsonBytes(
                            serde_json::to_vec(row).unwrap_or_else(|_| b"null".to_vec()),
                        ),
                    },
                    tool_use_id: string(row, "tool_use_id"),
                });
                let why = if string(row, "tool_name").as_deref() == Some("AskUserQuestion") {
                    Why::Question
                } else {
                    Why::Permission
                };
                self.attention = Attention::NeedsYou { why };
                self.known_attention = true;
            }
            RowKind::User => self.fold_user(seq, revision, row, activity_at, &mut mutations),
            RowKind::Assistant => {
                self.fold_assistant(seq, revision, row, activity_at, &mut mutations)
            }
            RowKind::System => self.fold_system(seq, revision, row, activity_at, &mut mutations),
            RowKind::PermissionMode => {}
            RowKind::AiTitle
            | RowKind::AgentName
            | RowKind::Attachments
            | RowKind::Keymap
            | RowKind::InputResult
            | RowKind::HookPreToolUse
            | RowKind::HookPostToolUse
            | RowKind::HookNotification
            | RowKind::Attachment
            | RowKind::FileHistorySnapshot
            | RowKind::FileHistoryDelta
            | RowKind::Mode
            | RowKind::AtisLatch
            | RowKind::BridgeSession
            | RowKind::CustomTitle
            | RowKind::LastPrompt
            | RowKind::QueueOperation => {}
            RowKind::Unknown => {
                let slot = 0;
                let key = raw_key(row, seq, slot);
                mutations.push(upsert(
                    key,
                    seq,
                    slot,
                    revision,
                    partial(
                        ClaudeEntryKind::Unrecognized,
                        ClaudeBody::Unrecognized {
                            row_type: string(row, "type"),
                            detail: None,
                        },
                        None,
                        revision,
                    ),
                ));
            }
        }
        self.previous_row_at = if breaks_duration_chain {
            None
        } else {
            activity_at.or(self.previous_row_at)
        };
        self.enforce_tip(&mut mutations, revision);
        mutations
    }

    fn fold_user(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        activity_at: Option<DateTime<Utc>>,
        mutations: &mut Vec<Mutation<ClaudeEntry>>,
    ) {
        let uuid = string(row, "uuid");
        let content = row.pointer("/message/content");
        if let Some(text) = content.and_then(Value::as_str) {
            let Some(uuid) = uuid.as_deref() else {
                mutations.push(unrecognized(row, seq, 0, revision, "user", "missing uuid"));
                return;
            };
            if let Some(message) = facts::inbound_message(text) {
                let key = namespaced_or_delivery("user", Some(uuid), seq, 0);
                let body = ClaudeBody::AgentMessage {
                    id: message.id,
                    context: message.context,
                    from: message.from,
                    kind: message.kind,
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(
                        ClaudeEntryKind::AgentMessage,
                        body,
                        Some(message.text),
                        revision,
                    ),
                ));
                return;
            }
            if row.get("isMeta").and_then(Value::as_bool) == Some(true)
                || text.starts_with("<command-")
                || text.starts_with("<local-command-")
            {
                return;
            }
            let (kind, body) = if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            {
                (ClaudeEntryKind::CompactSummary, ClaudeBody::None)
            } else if row.pointer("/origin/kind").and_then(Value::as_str)
                == Some("task-notification")
                || row.get("promptSource").and_then(Value::as_str) == Some("system")
            {
                (ClaudeEntryKind::TaskNotification, ClaudeBody::None)
            } else {
                let source = format!("{:?}", prompt_source(row));
                (
                    ClaudeEntryKind::Prompt,
                    ClaudeBody::Prompt {
                        source,
                        prompt_id: string(row, "promptId"),
                    },
                )
            };
            let key = namespaced_or_delivery("user", Some(uuid), seq, 0);
            mutations.push(upsert(
                key,
                seq,
                0,
                revision,
                partial(kind, body, Some(text.to_owned()), revision),
            ));
            if kind == ClaudeEntryKind::Prompt {
                self.asks.clear();
                self.attention = Attention::Working;
                self.known_attention = true;
                self.prompt_at = activity_at;
                self.turn_closed_at = None;
            }
            return;
        }

        let Some(blocks) = content.and_then(Value::as_array) else {
            let key = raw_key(row, seq, 0);
            mutations.push(upsert(
                key,
                seq,
                0,
                revision,
                partial(
                    ClaudeEntryKind::Unrecognized,
                    ClaudeBody::Unrecognized {
                        row_type: Some("user".into()),
                        detail: Some("no message content".into()),
                    },
                    None,
                    revision,
                ),
            ));
            return;
        };
        let retained = if blocks.len() > 1024 { 1023 } else { 1024 };
        for (index, block) in blocks.iter().take(retained).enumerate() {
            let slot = index as u16;
            match block.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    self.fold_tool_result(seq, slot, revision, row, block, mutations)
                }
                Some("text")
                    if matches!(
                        block.get("text").and_then(Value::as_str),
                        Some(
                            "[Request interrupted by user]"
                                | "[Request interrupted by user for tool use]"
                        )
                    ) =>
                {
                    let Some(uuid) = uuid.as_deref() else {
                        mutations.push(unrecognized(
                            row,
                            seq,
                            slot,
                            revision,
                            "user",
                            "interruption without uuid",
                        ));
                        continue;
                    };
                    let key = namespaced_or_delivery("user", Some(uuid), seq, slot);
                    mutations.push(upsert(
                        key,
                        seq,
                        slot,
                        revision,
                        partial(
                            ClaudeEntryKind::Interruption,
                            ClaudeBody::None,
                            block.get("text").and_then(Value::as_str).map(str::to_owned),
                            revision,
                        ),
                    ));
                    {
                        let turn = namespaced_or_delivery("turn", Some(uuid), seq, slot);
                        let ms = match (self.prompt_at, activity_at) {
                            (Some(start), Some(end)) => (end - start).num_milliseconds().max(0),
                            _ => 0,
                        };
                        mutations.push(upsert(
                            turn.clone(),
                            seq,
                            // The marker and inferred turn are distinct
                            // presentation facts from one provider block.
                            // Keep their provider order deterministic rather
                            // than letting entry-key sorting put the turn first.
                            slot.saturating_add(1),
                            revision,
                            partial(
                                ClaudeEntryKind::Turn,
                                ClaudeBody::Turn {
                                    duration_ms: ms,
                                    inferred: true,
                                    message_count: None,
                                    pending_background_agents: None,
                                },
                                None,
                                revision,
                            ),
                        ));
                        self.inferred_turn = Some(turn);
                    }
                    self.attention = Attention::Idle;
                    self.asks.clear();
                    self.known_attention = true;
                    self.known_outstanding = true;
                    self.turn_closed_at = activity_at;
                }
                other => {
                    let key = raw_key(row, seq, slot);
                    mutations.push(upsert(
                        key,
                        seq,
                        slot,
                        revision,
                        partial(
                            ClaudeEntryKind::Unrecognized,
                            ClaudeBody::Unrecognized {
                                row_type: Some("user".into()),
                                detail: other.map(str::to_owned),
                            },
                            None,
                            revision,
                        ),
                    ));
                }
            }
        }
        if blocks.len() > 1024 {
            let slot = 1023;
            let key = raw_key(row, seq, slot);
            let mut patch = partial(
                ClaudeEntryKind::Unrecognized,
                ClaudeBody::Unrecognized {
                    row_type: Some("user".into()),
                    detail: Some("row clipped after 1023 blocks".into()),
                },
                None,
                revision,
            );
            patch.clipped = Patch::set(true, revision);
            mutations.push(upsert(key, seq, slot, revision, patch));
        }
    }

    fn fold_assistant(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        activity_at: Option<DateTime<Utc>>,
        mutations: &mut Vec<Mutation<ClaudeEntry>>,
    ) {
        let uuid = string(row, "uuid");
        if row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
            let Some(uuid) = uuid.as_deref() else {
                mutations.push(unrecognized(
                    row,
                    seq,
                    0,
                    revision,
                    "assistant",
                    "api error without uuid",
                ));
                return;
            };
            let key = namespaced_or_delivery("err", Some(uuid), seq, 0);
            let text = row
                .pointer("/message/content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find_map(|block| block.get("text").and_then(Value::as_str))
                })
                .map(str::to_owned);
            mutations.push(upsert(
                key,
                seq,
                0,
                revision,
                partial(
                    ClaudeEntryKind::ApiError,
                    ClaudeBody::ApiError {
                        error: string(row, "error"),
                    },
                    text,
                    revision,
                ),
            ));
            return;
        }
        let message = row.get("message").unwrap_or(&Value::Null);
        let message_id = string(message, "id");
        if let Some(model) = string(message, "model") {
            self.model = Some(model);
            self.known_model = true;
        }
        if let Some(usage) = message.get("usage") {
            let used = [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .into_iter()
            .map(|key| usage.get(key).and_then(Value::as_u64).unwrap_or(0))
            .sum();
            self.context = Some(ContextMeter {
                used_tokens: used,
                window_tokens: None,
                source: ContextMeterSource::AssistantUsage,
            });
            self.known_context = true;
        }
        let stop = message
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.attention = Attention::Working;
        self.known_attention = true;
        let blocks: Vec<Value> = match message.get("content") {
            Some(Value::Array(blocks)) => blocks.clone(),
            Some(Value::String(text)) => vec![serde_json::json!({"type":"text", "text":text})],
            _ => Vec::new(),
        };
        let retained = if blocks.len() > 1024 { 1023 } else { 1024 };
        for (index, block) in blocks.iter().take(retained).enumerate() {
            let slot = index as u16;
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let key = namespaced_or_delivery("msg", message_id.as_deref(), seq, slot);
                    let source = uuid
                        .as_deref()
                        .map(|id| ComponentSource::Native {
                            id: id.to_owned(),
                            slot,
                        })
                        .unwrap_or(ComponentSource::Sequence { seq, slot });
                    let after = message_id
                        .as_deref()
                        .and_then(|id| self.messages.iter().find(|message| message.id == id))
                        .and_then(|message| message.last_component.clone())
                        .into_iter()
                        .collect();
                    let source_text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let component_text =
                        clipped_head(source_text, crate::STREAMING_BLOCK_MAX_BYTES);
                    let mut patch =
                        partial(ClaudeEntryKind::Message, ClaudeBody::None, None, revision);
                    patch.components.push(Component {
                        source: source.clone(),
                        observed_at: seq,
                        after,
                        value: component_text,
                    });
                    patch.finality =
                        Patch::set(stop.clone().unwrap_or_else(|| "open".into()), revision);
                    if source_text.len() > crate::STREAMING_BLOCK_MAX_BYTES {
                        patch.clipped = Patch::set(true, revision);
                    }
                    mutations.push(upsert(key.clone(), seq, slot, revision, patch));
                    if let Some(id) = &message_id {
                        self.remember_message_component(id, key, source);
                    }
                }
                Some("thinking" | "redacted_thinking") => {
                    let Some(uuid) = uuid.as_deref() else {
                        mutations.push(unrecognized(
                            row,
                            seq,
                            slot,
                            revision,
                            "assistant",
                            "thinking block without uuid",
                        ));
                        continue;
                    };
                    let key = namespaced_slot_or_delivery("think", Some(uuid), seq, slot);
                    let duration_ms = match (self.previous_row_at, activity_at) {
                        (Some(previous), Some(at)) => {
                            Some((at - previous).num_milliseconds().max(0))
                        }
                        _ => None,
                    };
                    let body = ClaudeBody::Thinking {
                        duration_ms,
                        redacted: block.get("type").and_then(Value::as_str)
                            == Some("redacted_thinking"),
                    };
                    mutations.push(upsert(
                        key,
                        seq,
                        slot,
                        revision,
                        partial(ClaudeEntryKind::Thinking, body, None, revision),
                    ));
                }
                Some("tool_use") => self.fold_tool_use(
                    seq,
                    slot,
                    revision,
                    row,
                    (message_id.as_deref(), stop.is_some()),
                    block,
                    mutations,
                ),
                other => {
                    let key = raw_key(row, seq, slot);
                    mutations.push(upsert(
                        key,
                        seq,
                        slot,
                        revision,
                        partial(
                            ClaudeEntryKind::Unrecognized,
                            ClaudeBody::Unrecognized {
                                row_type: Some("assistant".into()),
                                detail: other.map(str::to_owned),
                            },
                            None,
                            revision,
                        ),
                    ));
                }
            }
        }
        if blocks.len() > 1024 {
            let slot = 1023;
            let key = raw_key(row, seq, slot);
            let mut patch = partial(
                ClaudeEntryKind::Unrecognized,
                ClaudeBody::Unrecognized {
                    row_type: Some("assistant".into()),
                    detail: Some("row clipped after 1023 blocks".into()),
                },
                None,
                revision,
            );
            patch.clipped = Patch::set(true, revision);
            mutations.push(upsert(key, seq, slot, revision, patch));
        }
        if stop.is_some()
            && let Some(id) = message_id.as_deref()
        {
            if let Some(open) = self.messages.iter().find(|message| message.id == id)
                && open.last_component.is_some()
            {
                let message_patch = ClaudePartial {
                    finality: Patch::set(stop.clone().unwrap(), revision),
                    ..ClaudePartial::default()
                };
                mutations.push(upsert(open.key.clone(), seq, 0, revision, message_patch));
            }
            for (_, key, _) in self
                .open_tools
                .iter()
                .filter(|(_, _, tool_message)| tool_message.as_deref() == Some(id))
            {
                let patch = ClaudePartial {
                    message_final: Patch::set(true, revision),
                    ..ClaudePartial::default()
                };
                mutations.push(upsert(key.clone(), seq, 0, revision, patch));
            }
            self.messages.retain(|message| message.id != id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fold_tool_use(
        &mut self,
        seq: u64,
        slot: u16,
        revision: Revision,
        row: &Value,
        message: (Option<&str>, bool),
        block: &Value,
        mutations: &mut Vec<Mutation<ClaudeEntry>>,
    ) {
        let id = string(block, "id");
        let name = string(block, "name");
        let input = block.get("input").unwrap_or(&Value::Null);
        let Some(id) = id else {
            mutations.push(unrecognized(
                row,
                seq,
                slot,
                revision,
                "assistant",
                "tool_use without id",
            ));
            return;
        };
        if name.as_deref() == Some("TodoWrite")
            && let Some(progress) = todo_progress(input)
        {
            if let Some(pending) = self
                .pending_todos
                .iter_mut()
                .find(|todo| todo.tool_use_id == id)
            {
                pending.progress = progress;
            } else {
                self.pending_todos.push(PendingTodo {
                    tool_use_id: id,
                    progress,
                });
            }
            return;
        }
        self.tasks
            .observe_invocation(&id, name.as_deref().unwrap_or_default(), input);
        let ask_key = stable_hash(
            format!("{}\u{1f}{}", name.as_deref().unwrap_or_default(), input).as_bytes(),
        );
        if let Some(ask) = self.asks.iter_mut().find(|ask| {
            serde_json::from_slice::<Value>(&ask.row.payload.0)
                .ok()
                .is_some_and(|row| {
                    stable_hash(
                        format!(
                            "{}\u{1f}{}",
                            string(&row, "tool_name").unwrap_or_default(),
                            row.get("tool_input").unwrap_or(&Value::Null)
                        )
                        .as_bytes(),
                    ) == ask_key
                })
        }) {
            ask.tool_use_id = Some(id.clone());
        } else if message.1 && matches!(name.as_deref(), Some("AskUserQuestion" | "ExitPlanMode")) {
            let row = serde_json::json!({
                "type": "hook.permission_request",
                "tool_name": name,
                "tool_input": input,
                "tool_use_id": id,
            });
            self.asks.push(PendingAsk {
                row: RestoreRow {
                    seq,
                    payload: JsonBytes(serde_json::to_vec(&row).expect("synthetic ask serializes")),
                },
                tool_use_id: string(&row, "tool_use_id"),
            });
        }
        let key = namespaced_or_delivery("tool", Some(&id), seq, slot);
        let mut patch = partial(
            ClaudeEntryKind::Tool,
            ClaudeBody::Tool {
                tool_use_id: id.clone(),
            },
            None,
            revision,
        );
        patch.tool_name = match name {
            Some(name) => Patch::set(name, revision),
            None => Patch::clear(revision),
        };
        patch.tool_input = Patch::set(JsonBytes(bounded_json(input, 64 * 1024)), revision);
        patch.message_final = Patch::set(message.1, revision);
        mutations.push(upsert(key.clone(), seq, slot, revision, patch));
        if let Some(tool) = self
            .open_tools
            .iter_mut()
            .find(|(present, _, _)| present == &id)
        {
            tool.1 = key.clone();
            tool.2 = message.0.map(str::to_owned);
        } else {
            self.open_tools
                .push((id, key.clone(), message.0.map(str::to_owned)));
        }
    }

    fn fold_tool_result(
        &mut self,
        seq: u64,
        slot: u16,
        revision: Revision,
        row: &Value,
        block: &Value,
        mutations: &mut Vec<Mutation<ClaudeEntry>>,
    ) {
        let id = string(block, "tool_use_id");
        let Some(id) = id else {
            let key = raw_key(row, seq, slot);
            mutations.push(upsert(
                key,
                seq,
                slot,
                revision,
                partial(
                    ClaudeEntryKind::Unrecognized,
                    ClaudeBody::Unrecognized {
                        row_type: Some("user".into()),
                        detail: Some("tool_result without tool_use_id".into()),
                    },
                    None,
                    revision,
                ),
            ));
            return;
        };
        self.asks
            .retain(|ask| ask.tool_use_id.as_deref() != Some(id.as_str()));
        if let Some(progress) = self.tasks.observe_result(&id, block) {
            self.todo = Some(progress);
            self.known_todo = true;
        }
        if let Some(index) = self
            .pending_todos
            .iter()
            .position(|todo| todo.tool_use_id == id)
        {
            let pending = self.pending_todos.remove(index);
            if block.get("is_error").and_then(Value::as_bool) != Some(true) {
                self.todo = Some(pending.progress);
                self.known_todo = true;
                return;
            }
        }
        let key = namespaced_or_delivery("tool", Some(&id), seq, slot);
        let mut stored_outcome = block.clone();
        if let Some(object) = stored_outcome.as_object_mut() {
            if let Some(sidecar) = row.get("toolUseResult") {
                object.insert("amux_tool_use_result".into(), sidecar.clone());
            }
            if let Some(denial) = row.get("toolDenialKind") {
                object.insert("amux_tool_denial_kind".into(), denial.clone());
            }
        }
        let patch = ClaudePartial {
            kind: Patch::set(ClaudeEntryKind::Tool, revision),
            body: Patch::set(
                ClaudeBody::Tool {
                    tool_use_id: id.clone(),
                },
                revision,
            ),
            tool_outcome: Patch::set(
                JsonBytes(bounded_json(&stored_outcome, OUTPUT_HEAD_BYTES)),
                revision,
            ),
            ..ClaudePartial::default()
        };
        mutations.push(upsert(key, seq, slot, revision, patch));
        self.open_tools.retain(|(tool_id, _, _)| tool_id != &id);
        if matches!(
            self.attention,
            Attention::NeedsYou {
                why: Why::Permission | Why::Question
            }
        ) {
            self.attention = Attention::Working;
            self.known_attention = true;
        }
    }

    fn fold_system(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        activity_at: Option<DateTime<Utc>>,
        mutations: &mut Vec<Mutation<ClaudeEntry>>,
    ) {
        let uuid = string(row, "uuid");
        match row.get("subtype").and_then(Value::as_str) {
            Some("turn_duration") if row.get("durationMs").and_then(Value::as_u64).is_some() => {
                let Some(uuid) = uuid.as_deref() else {
                    mutations.push(unrecognized(
                        row,
                        seq,
                        0,
                        revision,
                        "system",
                        "turn_duration without uuid",
                    ));
                    return;
                };
                let key = namespaced_or_delivery("turn", Some(uuid), seq, 0);
                let body = ClaudeBody::Turn {
                    duration_ms: row.get("durationMs").and_then(Value::as_u64).unwrap_or(0) as i64,
                    inferred: false,
                    message_count: row.get("messageCount").and_then(Value::as_u64),
                    pending_background_agents: row
                        .get("pendingBackgroundAgentCount")
                        .and_then(Value::as_u64),
                };
                mutations.push(upsert(
                    key.clone(),
                    seq,
                    0,
                    revision,
                    partial(ClaudeEntryKind::Turn, body, None, revision),
                ));
                if let Some(from) = self.inferred_turn.take()
                    && from != key
                {
                    mutations.push(Mutation::Alias {
                        from,
                        to: key,
                        revision,
                        promote: None,
                    });
                }
                self.attention = Attention::NeedsYou { why: Why::Finished };
                self.known_attention = true;
                self.known_outstanding = true;
                self.turn_closed_at = activity_at;
                self.prompt_at = None;
            }
            Some("compact_boundary") => {
                let Some(uuid) = uuid.as_deref() else {
                    mutations.push(unrecognized(
                        row,
                        seq,
                        0,
                        revision,
                        "system",
                        "compact_boundary without uuid",
                    ));
                    return;
                };
                let key = namespaced_or_delivery("sys", Some(uuid), seq, 0);
                let meta = row.get("compactMetadata").unwrap_or(&Value::Null);
                let body = ClaudeBody::Compaction {
                    trigger: string(meta, "trigger"),
                    pre_tokens: meta.get("preTokens").and_then(Value::as_u64),
                    post_tokens: meta.get("postTokens").and_then(Value::as_u64),
                };
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(ClaudeEntryKind::Compaction, body, None, revision),
                ));
                self.previous_row_at = None;
                self.prompt_at = None;
                self.inferred_turn = None;
            }
            Some("stop_hook_summary" | "away_summary" | "local_command") => {}
            subtype => {
                let key = raw_key(row, seq, 0);
                mutations.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    partial(
                        ClaudeEntryKind::Unrecognized,
                        ClaudeBody::Unrecognized {
                            row_type: Some("system".into()),
                            detail: subtype.map(str::to_owned),
                        },
                        None,
                        revision,
                    ),
                ));
            }
        }
    }

    fn remember_message_component(&mut self, id: &str, key: EntryKey, source: ComponentSource) {
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.key = key;
            message.last_component = Some(source);
        } else {
            self.messages.push(OpenMessage {
                id: id.to_owned(),
                key,
                last_component: Some(source),
            });
        }
    }

    fn enforce_tip(&mut self, mutations: &mut Vec<Mutation<ClaudeEntry>>, revision: Revision) {
        while self.messages.len() + self.open_tools.len() + self.asks.len() > TIP_MAX_OPEN_ENTRIES
            || self.tip_bytes() > TIP_MAX_BYTES
        {
            if !self.messages.is_empty() {
                let evicted = self.messages.remove(0);
                let patch = ClaudePartial {
                    clipped: Patch::set(true, revision),
                    ..ClaudePartial::default()
                };
                mutations.push(upsert(evicted.key, revision.seq, 0, revision, patch));
            } else if !self.open_tools.is_empty() {
                let (_, key, _) = self.open_tools.remove(0);
                let patch = ClaudePartial {
                    clipped: Patch::set(true, revision),
                    ..ClaudePartial::default()
                };
                mutations.push(upsert(key, revision.seq, 0, revision, patch));
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
    }
}

impl ProviderFold for ClaudeFold {
    type Entry = ClaudeEntry;

    const PROTOCOL: StructuredProtocol = StructuredProtocol::ClaudePtyTranscript;
    const ENTRY_VERSION: u32 = 1;
    const TIP_VERSION: u32 = 4;
    const TIP_BUDGET: usize = TIP_MAX_BYTES;

    fn begin(&mut self, segment: SegmentId, baseline: Baseline) {
        self.segment = segment;
        self.baseline = baseline;
        self.messages.clear();
        self.open_tools.clear();
        self.pending_todos.clear();
        self.tasks.clear_pending();
        self.asks.clear();
        self.inferred_turn = None;
        self.previous_row_at = None;
        self.prompt_at = None;
        self.turn_closed_at = None;
        self.overflowed = false;
        if baseline == Baseline::Start {
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
                payload,
                ..
            } => match serde_json::from_slice::<Value>(payload) {
                Ok(row) if !self.overflowed => self.row(seq, activity_at, &row),
                Ok(_) => {
                    self.through = self.through.max(seq);
                    Vec::new()
                }
                Err(_) => {
                    self.through = self.through.max(seq);
                    let revision = Revision::row(seq);
                    let key = delivery_key(seq, 0);
                    vec![upsert(
                        key,
                        seq,
                        0,
                        revision,
                        partial(
                            ClaudeEntryKind::Unrecognized,
                            ClaudeBody::Unrecognized {
                                row_type: None,
                                detail: Some("invalid json".into()),
                            },
                            None,
                            revision,
                        ),
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
                self.messages.clear();
                self.open_tools.clear();
                self.pending_todos.clear();
                self.tasks.clear_pending();
                self.asks.clear();
                Vec::new()
            }
            Input::ObserverLost { .. } => {
                self.attention = Attention::Unknown;
                self.known_attention = false;
                self.known_outstanding = false;
                Vec::new()
            }
            Input::Tick { now } => {
                if self
                    .turn_closed_at
                    .is_some_and(|at| now.signed_duration_since(at).num_seconds() > 60)
                    && matches!(self.attention, Attention::NeedsYou { why: Why::Finished })
                {
                    self.attention = Attention::Idle;
                }
                Vec::new()
            }
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
        let strings = self
            .messages
            .iter()
            .map(|message| {
                message.id.capacity()
                    + message.key.as_str().len()
                    + message
                        .last_component
                        .as_ref()
                        .map_or(0, component_source_bytes)
            })
            .sum::<usize>()
            + self
                .open_tools
                .iter()
                .map(|(id, key, message_id)| {
                    id.capacity()
                        + key.as_str().len()
                        + message_id.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + self
                .pending_todos
                .iter()
                .map(|todo| {
                    todo.tool_use_id.capacity()
                        + todo.progress.current.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + self.model.as_ref().map_or(0, String::capacity);
        size_of::<Self>()
            + self.messages.capacity() * size_of::<OpenMessage>()
            + self.open_tools.capacity() * size_of::<(String, EntryKey, Option<String>)>()
            + self.pending_todos.capacity() * size_of::<PendingTodo>()
            + self.tasks.tip_bytes()
            + self.asks.capacity() * size_of::<PendingAsk>()
            + self
                .asks
                .iter()
                .map(|ask| {
                    ask.row.payload.0.capacity()
                        + ask.tool_use_id.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + strings
    }
}

fn partial(
    kind: ClaudeEntryKind,
    body: ClaudeBody,
    text: Option<String>,
    revision: Revision,
) -> ClaudePartial {
    ClaudePartial {
        kind: Patch::set(kind, revision),
        body: Patch::set(body, revision),
        text: text.map_or(Patch::Unchanged, |text| Patch::set(text, revision)),
        ..ClaudePartial::default()
    }
}

fn upsert(
    key: EntryKey,
    seq: u64,
    slot: u16,
    revision: Revision,
    entry: ClaudePartial,
) -> Mutation<ClaudeEntry> {
    Mutation::Upsert {
        key,
        order: Order::new(seq, slot.min(1023)).expect("bounded PTY slot"),
        revision,
        entry,
    }
}

fn delivery_key(seq: u64, slot: u16) -> EntryKey {
    EntryKey::new(format!("d:{seq}:{slot}")).expect("delivery key is bounded")
}

fn unrecognized(
    row: &Value,
    seq: u64,
    slot: u16,
    revision: Revision,
    row_type: &str,
    detail: &str,
) -> Mutation<ClaudeEntry> {
    upsert(
        raw_key(row, seq, slot),
        seq,
        slot,
        revision,
        partial(
            ClaudeEntryKind::Unrecognized,
            ClaudeBody::Unrecognized {
                row_type: Some(row_type.into()),
                detail: Some(detail.into()),
            },
            None,
            revision,
        ),
    )
}

fn namespaced_or_delivery(namespace: &str, id: Option<&str>, seq: u64, slot: u16) -> EntryKey {
    id.and_then(|id| EntryKey::new(format!("{namespace}:{id}")).ok())
        .unwrap_or_else(|| delivery_key(seq, slot))
}

fn namespaced_slot_or_delivery(namespace: &str, id: Option<&str>, seq: u64, slot: u16) -> EntryKey {
    id.and_then(|id| EntryKey::new(format!("{namespace}:{id}:{slot}")).ok())
        .unwrap_or_else(|| delivery_key(seq, slot))
}

fn raw_key(row: &Value, seq: u64, slot: u16) -> EntryKey {
    namespaced_slot_or_delivery("raw", row.get("uuid").and_then(Value::as_str), seq, slot)
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bounded_json(value: &Value, max: usize) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    if bytes.len() <= max {
        bytes
    } else {
        serde_json::to_vec(&serde_json::json!({"clipped":true,"bytes":bytes.len()}))
            .expect("marker serializes")
    }
}

fn clipped_head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… clipped …", &text[..end])
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
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
                    .map(str::to_owned)
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

fn ordered_components(components: &[Component<String>]) -> Vec<String> {
    let mut pending = components.iter().collect::<Vec<_>>();
    let mut emitted = Vec::<ComponentSource>::new();
    let mut text = Vec::new();
    while !pending.is_empty() {
        let eligible = pending
            .iter()
            .enumerate()
            .filter(|(_, component)| {
                component
                    .after
                    .iter()
                    .all(|source| emitted.contains(source))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let index = eligible.first().copied().unwrap_or(0);
        if !emitted.is_empty()
            && (eligible.is_empty() || eligible.len() > 1 && pending[index].after.is_empty())
        {
            text.push("… incomplete component order …".to_owned());
        }
        let component = pending.remove(index);
        emitted.push(component.source.clone());
        text.push(component.value.clone());
    }
    text
}

fn component_source_bytes(source: &ComponentSource) -> usize {
    match source {
        ComponentSource::Sequence { .. } => size_of::<ComponentSource>(),
        ComponentSource::Native { id, .. } => size_of::<ComponentSource>() + id.capacity(),
    }
}

fn truncate_versioned_string(field: &mut VersionedField<String>, max: usize) {
    if let Some(value) = field.value_mut() {
        *value = clipped_head(value, max);
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

impl crate::private::Sealed for ClaudeEntryKind {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ClaudeEntryKind| match value {
            ClaudeEntryKind::Prompt
            | ClaudeEntryKind::Message
            | ClaudeEntryKind::Thinking
            | ClaudeEntryKind::Tool
            | ClaudeEntryKind::Turn
            | ClaudeEntryKind::Compaction
            | ClaudeEntryKind::CompactSummary
            | ClaudeEntryKind::TaskNotification
            | ClaudeEntryKind::Interruption
            | ClaudeEntryKind::AgentMessage
            | ClaudeEntryKind::ApiError
            | ClaudeEntryKind::Unrecognized => {}
        };
    }
}
impl PostcardSafe for ClaudeEntryKind {}

impl crate::private::Sealed for AgentMessageKind {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: AgentMessageKind| match value {
            AgentMessageKind::Message
            | AgentMessageKind::Completed
            | AgentMessageKind::Exited
            | AgentMessageKind::Unstated => {}
            AgentMessageKind::Other { label } => crate::assert_value_safe(&label),
        };
    }
}
impl PostcardSafe for AgentMessageKind {}

impl crate::private::Sealed for ClaudeBody {
    fn assert_fields_are_postcard_safe() {
        let _ = |value: ClaudeBody| match value {
            ClaudeBody::None => {}
            ClaudeBody::Prompt { source, prompt_id } => {
                crate::assert_value_safe(&source);
                crate::assert_value_safe(&prompt_id);
            }
            ClaudeBody::Thinking {
                duration_ms,
                redacted,
            } => {
                crate::assert_value_safe(&duration_ms);
                crate::assert_value_safe(&redacted);
            }
            ClaudeBody::Turn {
                duration_ms,
                inferred,
                message_count,
                pending_background_agents,
            } => {
                crate::assert_value_safe(&duration_ms);
                crate::assert_value_safe(&inferred);
                crate::assert_value_safe(&message_count);
                crate::assert_value_safe(&pending_background_agents);
            }
            ClaudeBody::Compaction {
                trigger,
                pre_tokens,
                post_tokens,
            } => {
                crate::assert_value_safe(&trigger);
                crate::assert_value_safe(&pre_tokens);
                crate::assert_value_safe(&post_tokens);
            }
            ClaudeBody::Tool { tool_use_id } => crate::assert_value_safe(&tool_use_id),
            ClaudeBody::AgentMessage {
                id,
                context,
                from,
                kind,
            } => {
                crate::assert_value_safe(&id);
                crate::assert_value_safe(&context);
                crate::assert_value_safe(&from);
                crate::assert_value_safe(&kind);
            }
            ClaudeBody::ApiError { error } => crate::assert_value_safe(&error),
            ClaudeBody::Unrecognized { row_type, detail } => {
                crate::assert_value_safe(&row_type);
                crate::assert_value_safe(&detail);
            }
        };
    }
}
impl PostcardSafe for ClaudeBody {}

impl crate::private::Sealed for ClaudePartial {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudePartial {
                     kind,
                     body,
                     text,
                     components,
                     finality,
                     tool_name,
                     tool_input,
                     tool_outcome,
                     message_final,
                     clipped,
                 }: ClaudePartial| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&tool_name);
            crate::assert_value_safe(&tool_input);
            crate::assert_value_safe(&tool_outcome);
            crate::assert_value_safe(&message_final);
            crate::assert_value_safe(&clipped);
        };
    }
}
impl PostcardSafe for ClaudePartial {}

impl crate::private::Sealed for ClaudeEntry {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudeEntry {
                     kind,
                     body,
                     text,
                     components,
                     finality,
                     tool_name,
                     tool_input,
                     tool_outcome,
                     message_final,
                     clipped,
                     rendered_text,
                 }: ClaudeEntry| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&tool_name);
            crate::assert_value_safe(&tool_input);
            crate::assert_value_safe(&tool_outcome);
            crate::assert_value_safe(&message_final);
            crate::assert_value_safe(&clipped);
            crate::assert_value_safe(&rendered_text);
        };
    }
}
impl PostcardSafe for ClaudeEntry {}

impl crate::private::Sealed for OpenMessage {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<EntryKey>();
        crate::assert_postcard_safe::<Option<ComponentSource>>();
    }
}
impl PostcardSafe for OpenMessage {}

impl crate::private::Sealed for PendingTodo {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<TodoProgress>();
    }
}
impl PostcardSafe for PendingTodo {}

impl crate::private::Sealed for PendingAsk {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<RestoreRow>();
        crate::assert_postcard_safe::<Option<String>>();
    }
}
impl PostcardSafe for PendingAsk {}

impl crate::private::Sealed for ClaudeFold {
    fn assert_fields_are_postcard_safe() {
        let _ = |ClaudeFold {
                     segment,
                     baseline,
                     through,
                     messages,
                     open_tools,
                     pending_todos,
                     tasks,
                     asks,
                     inferred_turn,
                     previous_row_at,
                     prompt_at,
                     turn_closed_at,
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
                 }: ClaudeFold| {
            crate::assert_value_safe(&segment);
            crate::assert_value_safe(&baseline);
            crate::assert_value_safe(&through);
            crate::assert_value_safe(&messages);
            crate::assert_value_safe(&open_tools);
            crate::assert_value_safe(&pending_todos);
            crate::assert_value_safe(&tasks);
            crate::assert_value_safe(&asks);
            crate::assert_value_safe(&inferred_turn);
            crate::assert_value_safe(&previous_row_at);
            crate::assert_value_safe(&prompt_at);
            crate::assert_value_safe(&turn_closed_at);
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
impl PostcardSafe for ClaudeFold {}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::MutationOracle;

    const CORPORA: &[(&str, &str)] = &[
        (
            "pong",
            include_str!("../../../claude-specs/fixtures/claude-pty/pong.rows.jsonl"),
        ),
        (
            "tools",
            include_str!("../../../claude-specs/fixtures/claude-pty/tools.rows.jsonl"),
        ),
        (
            "interrupt",
            include_str!("../../../claude-specs/fixtures/claude-pty/interrupt.rows.jsonl"),
        ),
        (
            "compact",
            include_str!("../../../claude-specs/fixtures/claude-pty/compact.rows.jsonl"),
        ),
    ];
    const TASK_TOOL_CAPTURE: &str = include_str!("../../fixtures/claude-task-tools-2.1.273.jsonl");

    fn rows(raw: &str) -> Vec<Vec<u8>> {
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.as_bytes().to_vec())
            .collect()
    }

    fn at(seq: u64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_760_000_000 + seq as i64, 0)
            .single()
            .unwrap()
    }

    fn activity(payload: &[u8]) -> Option<DateTime<Utc>> {
        serde_json::from_slice::<Value>(payload)
            .ok()?
            .get("timestamp")?
            .as_str()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.with_timezone(&Utc))
    }

    fn apply_row(
        fold: &mut ClaudeFold,
        oracle: &mut MutationOracle<ClaudeEntry>,
        seq: u64,
        payload: &[u8],
    ) {
        let changes = fold.apply(Input::Row {
            seq,
            published_at: at(seq),
            activity_at: activity(payload),
            historical: false,
            payload,
        });
        oracle.apply_changes(&changes).unwrap();
        assert!(
            fold.tip_bytes() <= ClaudeFold::TIP_BUDGET,
            "tip grew to {} bytes after row {seq}",
            fold.tip_bytes()
        );
    }

    fn fold_rows(input: &[Vec<u8>]) -> (ClaudeFold, MutationOracle<ClaudeEntry>) {
        let mut fold = ClaudeFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().enumerate() {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, payload);
        }
        (fold, oracle)
    }

    #[test]
    fn claude_pty_continuation_matches_uninterrupted_at_every_corpus_cut() {
        for (name, raw) in CORPORA {
            let input = rows(raw);
            let (expected_fold, expected_oracle) = fold_rows(&input);
            let expected_entries = expected_oracle.entries();
            let mut prefix_fold = ClaudeFold::default();
            prefix_fold.begin(1, Baseline::Start);
            let mut prefix_oracle = MutationOracle::default();
            for cut in 0..=input.len() {
                if cut > 0 {
                    apply_row(
                        &mut prefix_fold,
                        &mut prefix_oracle,
                        cut as u64,
                        &input[cut - 1],
                    );
                }
                let bytes = postcard::to_allocvec(&prefix_fold).unwrap();
                let mut resumed: ClaudeFold = postcard::from_bytes(&bytes).unwrap();
                let mut materialized = prefix_oracle.clone();
                for (index, payload) in input.iter().enumerate().skip(cut) {
                    apply_row(&mut resumed, &mut materialized, index as u64 + 1, payload);
                }
                assert_eq!(resumed, expected_fold, "{name}: tip differs at cut {cut}");
                assert_eq!(
                    resumed.summary(),
                    expected_fold.summary(),
                    "{name}: summary differs at cut {cut}"
                );
                assert_eq!(
                    materialized.entries(),
                    expected_entries,
                    "{name}: entries differ at cut {cut}"
                );
                assert_eq!(
                    materialized.redirects(),
                    expected_oracle.redirects(),
                    "{name}: aliases differ at cut {cut}"
                );
            }
        }
    }

    fn one(row: Value) -> (ClaudeFold, MutationOracle<ClaudeEntry>) {
        fold_rows(&[serde_json::to_vec(&row).unwrap()])
    }

    fn keys(oracle: &MutationOracle<ClaudeEntry>) -> Vec<String> {
        oracle
            .entries()
            .into_iter()
            .map(|stored| stored.key.into_string())
            .collect()
    }

    #[test]
    fn claude_pty_identity_covers_every_a5_creating_form() {
        let prompt = one(
            json!({"type":"user","uuid":"u1","message":{"content":"hello"},"origin":{"kind":"human"}}),
        );
        assert_eq!(keys(&prompt.1), ["user:u1"]);

        let message = one(
            json!({"type":"assistant","uuid":"r1","message":{"id":"m1","content":[{"type":"text","text":"hi"}],"stop_reason":null}}),
        );
        assert_eq!(keys(&message.1), ["msg:m1"]);
        let message_without_id = one(
            json!({"type":"assistant","uuid":"r2","message":{"content":[{"type":"text","text":"hi"}],"stop_reason":null}}),
        );
        assert_eq!(keys(&message_without_id.1), ["d:1:0"]);

        let thinking = one(
            json!({"type":"assistant","uuid":"r3","message":{"id":"m2","content":[{"type":"thinking","thinking":"x"}],"stop_reason":null}}),
        );
        assert_eq!(keys(&thinking.1), ["think:r3:0"]);

        let tool = one(
            json!({"type":"assistant","uuid":"r4","message":{"id":"m3","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"x"}}],"stop_reason":"tool_use"}}),
        );
        assert_eq!(keys(&tool.1), ["tool:t1"]);
        let result_only = one(
            json!({"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}),
        );
        assert_eq!(keys(&result_only.1), ["tool:t1"]);

        let turn =
            one(json!({"type":"system","subtype":"turn_duration","uuid":"s1","durationMs":12}));
        assert_eq!(keys(&turn.1), ["turn:s1"]);
        let interruption = one(
            json!({"type":"user","uuid":"u3","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
        );
        assert_eq!(keys(&interruption.1), ["user:u3", "turn:u3"]);
        assert_eq!(interruption.0.summary().attention, Attention::Idle);
        let compact = one(json!({"type":"system","subtype":"compact_boundary","uuid":"s2"}));
        assert_eq!(keys(&compact.1), ["sys:s2"]);
        let compact_summary = one(
            json!({"type":"user","uuid":"u4","isCompactSummary":true,"message":{"content":"summary"}}),
        );
        assert_eq!(keys(&compact_summary.1), ["user:u4"]);
        let task = one(
            json!({"type":"user","uuid":"u5","origin":{"kind":"task-notification"},"message":{"content":"done"}}),
        );
        assert_eq!(keys(&task.1), ["user:u5"]);
        let agent = one(
            json!({"type":"user","uuid":"u6","message":{"content":"<agent-message from=\"worker/local\" kind=\"message\">hello</agent-message>"}}),
        );
        assert_eq!(keys(&agent.1), ["user:u6"]);
        let error = one(
            json!({"type":"assistant","uuid":"a1","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"bad"}]}}),
        );
        assert_eq!(keys(&error.1), ["err:a1"]);
        let raw = one(json!({"type":"future","uuid":"z1"}));
        assert_eq!(keys(&raw.1), ["raw:z1:0"]);
        let raw_without_uuid = one(json!({"type":"future"}));
        assert_eq!(keys(&raw_without_uuid.1), ["d:1:0"]);
        assert_eq!(
            DELIVERY_KEYED_VARIANTS,
            [
                "claude_pty.message_without_message_id",
                "claude_pty.unrecognized_without_uuid"
            ]
        );
    }

    #[test]
    fn interruption_precedes_its_turn_and_breaks_thinking_duration_chain() {
        let input = [
            json!({
                "type": "user",
                "uuid": "prompt",
                "timestamp": "2026-08-12T09:00:00Z",
                "message": {"content": "go"},
                "origin": {"kind": "human"}
            }),
            json!({
                "type": "assistant",
                "uuid": "thinking-before",
                "timestamp": "2026-08-12T09:00:03Z",
                "message": {"id": "m1", "content": [{"type": "thinking", "thinking": "x"}]}
            }),
            json!({
                "type": "user",
                "uuid": "interrupt",
                "timestamp": "2026-08-12T09:00:06Z",
                "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "thinking-after",
                "timestamp": "2026-08-12T09:00:20Z",
                "message": {"id": "m2", "content": [{"type": "thinking", "thinking": "x"}]}
            }),
        ]
        .map(|row| serde_json::to_vec(&row).unwrap());

        let (_, oracle) = fold_rows(&input);
        let entries = oracle.entries();
        assert_eq!(
            entries
                .iter()
                .map(|stored| stored.entry.kind())
                .collect::<Vec<_>>(),
            ["prompt", "thinking", "interruption", "turn", "thinking"]
        );
        let durations = entries
            .iter()
            .filter_map(|stored| match stored.entry.body() {
                Some(ClaudeBody::Thinking { duration_ms, .. }) => Some(*duration_ms),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(durations, [Some(3_000), None]);
    }

    #[test]
    fn claude_pty_message_components_keep_native_relative_order_across_republication() {
        let input = vec![
            serde_json::to_vec(&json!({"type":"assistant","uuid":"z-row","message":{"id":"m","content":[{"type":"text","text":"first"}],"stop_reason":null}})).unwrap(),
            serde_json::to_vec(&json!({"type":"assistant","uuid":"a-row","message":{"id":"m","content":[{"type":"text","text":"second"}],"stop_reason":"end_turn"}})).unwrap(),
        ];
        let (_, oracle) = fold_rows(&input);
        let entry = &oracle.entries()[0].entry;
        assert_eq!(entry.text(), Some("first\n\nsecond"));
        assert_eq!(
            entry.components().values()[0].source,
            ComponentSource::Native {
                id: "a-row".into(),
                slot: 0
            }
        );
    }

    #[test]
    fn claude_pty_tool_result_amends_after_the_tool_leaves_the_tip() {
        let mut fold = ClaudeFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for index in 0..300u64 {
            let row = serde_json::to_vec(&json!({"type":"assistant","uuid":format!("row-{index}"),"message":{"id":"message","content":[{"type":"tool_use","id":format!("tool-{index}"),"name":"Read","input":{"file_path":"x"}}],"stop_reason":null}})).unwrap();
            apply_row(&mut fold, &mut oracle, index + 1, &row);
        }
        assert!(!fold.open_tools.iter().any(|(id, _, _)| id == "tool-0"));
        let result = serde_json::to_vec(&json!({"type":"user","uuid":"result","message":{"content":[{"type":"tool_result","tool_use_id":"tool-0","content":"late"}]}})).unwrap();
        apply_row(&mut fold, &mut oracle, 301, &result);
        let stored = oracle
            .entries()
            .into_iter()
            .find(|entry| entry.key.as_str() == "tool:tool-0")
            .unwrap();
        assert!(stored.entry.tool_outcome().is_some());
    }

    #[test]
    fn claude_pty_inferred_turn_aliases_only_while_correlation_is_in_tip() {
        let input = vec![
            serde_json::to_vec(&json!({"type":"user","uuid":"interrupt","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}})).unwrap(),
            serde_json::to_vec(&json!({"type":"system","subtype":"turn_duration","uuid":"authority","durationMs":9})).unwrap(),
        ];
        let (_, oracle) = fold_rows(&input);
        assert_eq!(keys(&oracle), ["user:interrupt", "turn:authority"]);
        assert_eq!(
            oracle
                .redirects()
                .iter()
                .map(|(from, to)| (from.as_str(), to.as_str()))
                .collect::<Vec<_>>(),
            [("turn:interrupt", "turn:authority")]
        );
    }

    fn todo_rows(label: &str) -> Vec<Vec<u8>> {
        vec![
            serde_json::to_vec(&json!({"type":"assistant","uuid":format!("use-{label}"),"message":{"id":format!("msg-{label}"),"content":[{"type":"tool_use","id":format!("todo-{label}"),"name":"TodoWrite","input":{"todos":[{"content":label,"activeForm":format!("doing {label}"),"status":"in_progress"}]}}],"stop_reason":"tool_use"}})).unwrap(),
            serde_json::to_vec(&json!({"type":"user","uuid":format!("result-{label}"),"message":{"content":[{"type":"tool_result","tool_use_id":format!("todo-{label}"),"content":"ok"}]}})).unwrap(),
        ]
    }

    #[test]
    fn claude_pty_real_task_tool_capture_derives_todo_and_keeps_denied_write() {
        let input = rows(TASK_TOOL_CAPTURE);
        assert!(
            input.iter().all(|row| {
                serde_json::from_slice::<Value>(row).unwrap()["version"] == "2.1.273"
            })
        );

        let mut fold = ClaudeFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().take(2).enumerate() {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, payload);
        }
        let bytes = postcard::to_allocvec(&fold).unwrap();
        let mut fold: ClaudeFold = postcard::from_bytes(&bytes).unwrap();
        for (index, payload) in input.iter().enumerate().skip(2) {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, payload);
        }
        apply_row(
            &mut fold,
            &mut oracle,
            input.len() as u64 + 1,
            br#"{"type":"amux.transcript_ready"}"#,
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
        assert!(
            outcome["content"]
                .as_str()
                .unwrap()
                .starts_with("Permission for this tool use was denied")
        );

        fold.begin(2, Baseline::Start);
        assert!(fold.summary().todo.is_none());
        assert!(fold.summary().unknown.contains(&SummaryField::Todo));
    }

    #[test]
    fn claude_pty_two_prefixes_are_not_erased_by_an_identical_suffix() {
        let suffix = vec![serde_json::to_vec(&json!({"type":"amux.transcript_ready"})).unwrap()];
        let mut left = todo_rows("left");
        left.extend(suffix.clone());
        let mut right = todo_rows("right");
        right.extend(suffix);
        let (left, _) = fold_rows(&left);
        let (right, _) = fold_rows(&right);
        assert_ne!(left.summary(), right.summary());
        assert_eq!(
            left.summary().todo.unwrap().current.as_deref(),
            Some("doing left")
        );
        assert_eq!(
            right.summary().todo.unwrap().current.as_deref(),
            Some("doing right")
        );
    }

    #[test]
    fn claude_pty_lifecycle_inputs_obey_the_knowledge_table_without_refreshing_activity() {
        let row = serde_json::to_vec(&json!({"type":"amux.transcript_ready"})).unwrap();
        let (mut fold, _) = fold_rows(&[row]);
        let before = fold.summary().last_activity;
        fold.apply(Input::Tick { now: at(100) });
        assert_eq!(fold.summary().last_activity, before);
        fold.apply(Input::ObserverLost { at: at(101) });
        assert!(fold.summary().unknown.contains(&SummaryField::Attention));
        assert!(fold.summary().unknown.contains(&SummaryField::Outstanding));
        fold.apply(Input::ProcessExited {
            exit_code: Some(7),
            at: at(102),
        });
        assert_eq!(
            fold.summary().phase,
            AgentPhase::Exited { exit_code: Some(7) }
        );
        assert_eq!(fold.summary().attention, Attention::Idle);
        assert!(!fold.summary().unknown.contains(&SummaryField::Outstanding));
    }

    #[test]
    fn claude_pty_tip_is_feed_free_postcard_safe_and_bounded_after_every_input() {
        let huge = "x".repeat(200_000);
        let input = serde_json::to_vec(&json!({"type":"assistant","uuid":"huge-row","message":{"id":"huge-message","content":[{"type":"text","text":huge}],"stop_reason":null}})).unwrap();
        let (fold, oracle) = fold_rows(&[input]);
        assert!(
            fold.tip_bytes() < 64 * 1024,
            "feed body leaked into tip: {}",
            fold.tip_bytes()
        );
        assert!(oracle.entries()[0].entry.text().unwrap().len() < 70 * 1024);
        assert!(oracle.entries()[0].entry.is_clipped());
        let encoded = postcard::to_allocvec(&fold).unwrap();
        let decoded: ClaudeFold = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, fold);
        let entry_encoded = postcard::to_allocvec(&oracle.entries()[0].entry).unwrap();
        let _: ClaudeEntry = postcard::from_bytes(&entry_encoded).unwrap();
    }
}
