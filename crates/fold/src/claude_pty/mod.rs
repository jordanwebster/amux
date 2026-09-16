//! Claude PTY transcript observation shared by clients and the daemon.
//!
//! This module deliberately knows nothing about reducer commands, optimistic
//! input, answer dispatch, or renderer state. It classifies provider rows and
//! retains the small provider-owned facts that both sides of the stream must
//! derive identically.

use std::collections::VecDeque;

use chrono::{DateTime, TimeDelta, Utc};
use model::{AgentMessageKind, Why};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TODO_WRITES_RETAINED: usize = 4096;
const PENDING_TODO_WRITES_RETAINED: usize = 256;

const STRUCTURED_PATCH_HUNKS_RETAINED: usize = 16;
const STRUCTURED_PATCH_LINES_RETAINED: usize = 64;
const STRUCTURED_PATCH_BYTES_RETAINED: usize = 8 * 1024;

pub mod facts;

/// Top-level Claude PTY row classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Attachments,
    TranscriptReady,
    Keymap,
    InputResult,
    HookStop,
    HookPermissionRequest,
    HookPreToolUse,
    HookPostToolUse,
    HookNotification,
    User,
    Assistant,
    System,
    Attachment,
    FileHistorySnapshot,
    FileHistoryDelta,
    Mode,
    PermissionMode,
    AiTitle,
    AgentName,
    AtisLatch,
    BridgeSession,
    CustomTitle,
    LastPrompt,
    QueueOperation,
    Unknown,
}

pub fn classify_row(row: &Value) -> RowKind {
    match row.get("type").and_then(Value::as_str) {
        Some("amux.attachments") => RowKind::Attachments,
        Some("amux.transcript_ready") => RowKind::TranscriptReady,
        Some("amux.claude.keymap") => RowKind::Keymap,
        Some("amux.claude.input_result") => RowKind::InputResult,
        Some("hook.stop") => RowKind::HookStop,
        Some("hook.permission_request") => RowKind::HookPermissionRequest,
        Some("hook.pre_tool_use") => RowKind::HookPreToolUse,
        Some("hook.post_tool_use") => RowKind::HookPostToolUse,
        Some("hook.notification") => RowKind::HookNotification,
        Some("user") => RowKind::User,
        Some("assistant") => RowKind::Assistant,
        Some("system") => RowKind::System,
        Some("attachment") => RowKind::Attachment,
        Some("file-history-snapshot") => RowKind::FileHistorySnapshot,
        Some("file-history-delta") => RowKind::FileHistoryDelta,
        Some("mode") => RowKind::Mode,
        Some("permission-mode") => RowKind::PermissionMode,
        Some("ai-title") => RowKind::AiTitle,
        Some("agent-name") => RowKind::AgentName,
        Some("atis-latch") => RowKind::AtisLatch,
        Some("bridge-session") => RowKind::BridgeSession,
        Some("custom-title") => RowKind::CustomTitle,
        Some("last-prompt") => RowKind::LastPrompt,
        Some("queue-operation") => RowKind::QueueOperation,
        _ => RowKind::Unknown,
    }
}

/// Latest-wins facts carried by Claude PTY rows rather than reducer actions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFacts {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub context_used_tokens: Option<u64>,
    pub ai_title: Option<String>,
    pub agent_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageEntry {
    pub id: Option<String>,
    pub context: Option<String>,
    pub from: String,
    pub kind: AgentMessageKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry<C> {
    pub id: u64,
    pub seq: u64,
    pub kind: FeedEntryKind<C>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum FeedEntryKind<C> {
    Prompt(PromptEntry<C>),
    Message(MessageEntry<C>),
    Thinking(ThinkingEntry),
    Turn(TurnEntry),
    Compaction(CompactionEntry),
    CompactSummary(CompactSummaryEntry),
    Tool(ToolEntry),
    TaskNotification(TaskNotificationEntry),
    AgentMessage(AgentMessageEntry),
    Interruption(InterruptionEntry),
    ApiError(ApiErrorEntry),
    Unrecognized(UnrecognizedEntry),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptEntry<C> {
    pub text: String,
    pub content: Vec<C>,
    pub source: PromptSource,
    pub prompt_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry<C> {
    pub message_id: String,
    pub segments: Vec<String>,
    pub content: Vec<C>,
    pub finality: MessageFinality,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum PromptSource {
    Typed,
    Queued,
    SuggestionAccepted,
    Human,
    Other { label: String },
    Unstated,
}

pub fn prompt_source(row: &Value) -> PromptSource {
    let origin_kind = row.pointer("/origin/kind").and_then(Value::as_str);
    match row.get("promptSource").and_then(Value::as_str) {
        Some("typed") => PromptSource::Typed,
        Some("queued") => PromptSource::Queued,
        Some("suggestion_accepted") => PromptSource::SuggestionAccepted,
        Some(other) => PromptSource::Other {
            label: other.to_owned(),
        },
        None if origin_kind == Some("human") => PromptSource::Human,
        None => PromptSource::Unstated,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finality", rename_all = "snake_case")]
pub enum MessageFinality {
    Open,
    Final { stop_reason: String },
    Interrupted,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingEntry {
    pub duration_ms: Option<i64>,
    pub redacted: bool,
}

pub fn thinking_entry(
    previous: Option<DateTime<Utc>>,
    at: Option<DateTime<Utc>>,
    redacted: bool,
) -> ThinkingEntry {
    ThinkingEntry {
        duration_ms: match (previous, at) {
            (Some(previous), Some(at)) => Some((at - previous).num_milliseconds().max(0)),
            _ => None,
        },
        redacted,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEntry {
    pub duration: TurnDuration,
    pub message_count: Option<u64>,
    pub pending_background_agents: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "duration", rename_all = "snake_case")]
pub enum TurnDuration {
    Measured { ms: u64 },
    SincePrompt { ms: i64 },
}

pub fn measured_turn(row: &Value) -> Option<TurnEntry> {
    Some(TurnEntry {
        duration: TurnDuration::Measured {
            ms: row.get("durationMs")?.as_u64()?,
        },
        message_count: row.get("messageCount").and_then(Value::as_u64),
        pending_background_agents: row
            .get("pendingBackgroundAgentCount")
            .and_then(Value::as_u64),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

pub fn compaction_entry(row: &Value) -> CompactionEntry {
    let metadata = row.get("compactMetadata").unwrap_or(&Value::Null);
    CompactionEntry {
        trigger: string_of(metadata, "trigger"),
        pre_tokens: metadata.get("preTokens").and_then(Value::as_u64),
        post_tokens: metadata.get("postTokens").and_then(Value::as_u64),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryEntry {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolEntry {
    pub tool_use_id: String,
    pub name: Option<String>,
    pub invocation: facts::ToolInvocation,
    pub outcome: ToolOutcome,
    pub message_final: bool,
    pub group_with_previous: bool,
    pub message_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    Pending,
    Success { facts: SuccessFacts },
    Denied { kind: Option<String> },
    Failed { message: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "facts", rename_all = "snake_case")]
pub enum SuccessFacts {
    Edit {
        file_path: String,
        added: u64,
        removed: u64,
        document: crate::diff::Document,
    },
    Answers {
        answers: Vec<QuestionAnswer>,
    },
    TaskCompleted {
        agent_id: Option<String>,
        duration_ms: Option<u64>,
        tool_count: Option<u64>,
    },
    TaskLaunched {
        agent_id: Option<String>,
    },
    PlanApproved {
        plan_file_path: Option<String>,
    },
    Output {
        head: String,
        truncated: bool,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question: String,
    pub answer: String,
}

pub fn compact_summary_entry(text: &str) -> CompactSummaryEntry {
    CompactSummaryEntry {
        text: text.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotificationEntry {
    pub text: String,
}

pub fn task_notification_entry(text: &str) -> TaskNotificationEntry {
    TaskNotificationEntry {
        text: text.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptionEntry {
    pub kind: InterruptionKind,
    pub interrupted_message_id: Option<String>,
}

pub fn interruption_entry(kind: InterruptionKind, row: &Value) -> InterruptionEntry {
    InterruptionEntry {
        kind,
        interrupted_message_id: string_of(row, "interruptedMessageId"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionKind {
    Turn,
    ToolUse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEntry {
    pub error: Option<String>,
    pub text: Option<String>,
}

pub fn api_error_entry(row: &Value) -> ApiErrorEntry {
    ApiErrorEntry {
        error: string_of(row, "error"),
        text: row
            .pointer("/message/content")
            .and_then(Value::as_array)
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find_map(|block| block.get("text").and_then(Value::as_str))
            })
            .map(str::to_owned),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnrecognizedEntry {
    pub row_type: Option<String>,
    pub detail: Option<String>,
}

pub fn unrecognized_entry(
    row_type: Option<&str>,
    detail: Option<impl Into<String>>,
) -> UnrecognizedEntry {
    UnrecognizedEntry {
        row_type: row_type.map(str::to_owned),
        detail: detail.map(Into::into),
    }
}

impl SessionFacts {
    /// Observe the session-state part of a top-level classified row.
    pub fn observe_row(&mut self, kind: RowKind, row: &Value) {
        match kind {
            RowKind::PermissionMode => {
                self.permission_mode = string_of(row, "permissionMode");
            }
            RowKind::AiTitle => self.ai_title = string_of(row, "aiTitle"),
            RowKind::AgentName => self.agent_name = string_of(row, "agentName"),
            _ => {}
        }
    }

    /// Hook payloads are the live source for a mid-session permission-mode
    /// cycle, which does not emit a transcript permission-mode row.
    pub fn observe_hook(&mut self, row: &Value) {
        if let Some(mode) = row.get("permission_mode").and_then(Value::as_str) {
            self.permission_mode = Some(mode.to_owned());
        }
    }

    /// Assistant messages carry the model and the three context-cost terms.
    pub fn observe_assistant(&mut self, row: &Value) {
        let Some(message) = row.get("message") else {
            return;
        };
        if let Some(name) = message.get("model").and_then(Value::as_str) {
            self.model = Some(name.to_owned());
        }
        if let Some(usage) = message.get("usage") {
            let tokens = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            self.context_used_tokens = Some(
                tokens("input_tokens")
                    .saturating_add(tokens("cache_read_input_tokens"))
                    .saturating_add(tokens("cache_creation_input_tokens")),
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    pub items: Vec<(String, TodoState)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeTodos {
    current: Option<TaskList>,
    writes: VecDeque<TodoWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TodoWrite {
    id: String,
    pending: Option<TaskList>,
}

pub enum TodoDisposition {
    Other,
    Absorbed,
    Failed,
}

impl ClaudeTodos {
    pub fn current(&self) -> Option<&TaskList> {
        self.current.as_ref()
    }

    /// Consume a native `tool_use` or `tool_result` content block.
    pub fn observe(&mut self, block: &Value) -> TodoDisposition {
        match block["type"].as_str() {
            Some("tool_use") if block["name"] == "TodoWrite" => {
                let Some(id) = block["id"].as_str().filter(|id| !id.is_empty()) else {
                    return TodoDisposition::Other;
                };
                if self.writes.iter().any(|write| write.id == id) {
                    return TodoDisposition::Absorbed;
                }
                let Some(list) = task_list(&block["input"]) else {
                    return TodoDisposition::Other;
                };
                self.writes.push_back(TodoWrite {
                    id: id.to_owned(),
                    pending: Some(list),
                });
                if self.writes.len() > TODO_WRITES_RETAINED {
                    self.writes.pop_front();
                }
                if self
                    .writes
                    .iter()
                    .filter(|write| write.pending.is_some())
                    .count()
                    > PENDING_TODO_WRITES_RETAINED
                    && let Some(index) =
                        self.writes.iter().position(|write| write.pending.is_some())
                {
                    self.writes.remove(index);
                }
                TodoDisposition::Absorbed
            }
            Some("tool_result") => {
                let Some(write) = self
                    .writes
                    .iter_mut()
                    .find(|write| block["tool_use_id"].as_str() == Some(write.id.as_str()))
                else {
                    return TodoDisposition::Other;
                };
                let Some(list) = write.pending.take() else {
                    return TodoDisposition::Absorbed;
                };
                if block["is_error"].as_bool() == Some(true) {
                    TodoDisposition::Failed
                } else {
                    self.current = Some(list);
                    TodoDisposition::Absorbed
                }
            }
            _ => TodoDisposition::Other,
        }
    }
}

fn task_list(input: &Value) -> Option<TaskList> {
    let mut items = Vec::new();
    let mut current = None;
    let mut done = 0;
    for todo in input["todos"].as_array()? {
        let text = todo["content"].as_str()?;
        let state = match todo["status"].as_str()? {
            "pending" => TodoState::Pending,
            "in_progress" => TodoState::InProgress,
            "completed" => TodoState::Completed,
            _ => return None,
        };
        if state == TodoState::Completed {
            done += 1;
        }
        if state == TodoState::InProgress && current.is_none() {
            current = Some(todo["activeForm"].as_str().unwrap_or(text).to_owned());
        }
        items.push((text.to_owned(), state));
    }
    Some(TaskList {
        done,
        total: items.len(),
        current,
        items,
    })
}

/// Reducer-independent stream coverage presented to the attention observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Unavailable,
    Replaying,
    Live,
}

/// Provider facts needed to derive attention and phase. Local optimistic
/// state is intentionally absent; the UI facade applies it after observing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionInput {
    pub process_exited: bool,
    pub observer_stale: bool,
    pub stream: StreamState,
    pub live: bool,
    pub transcript_rows_seen: bool,
    pub truncated_start: bool,
    pub ask: Option<Why>,
    pub error_live: bool,
    pub turn_open: bool,
    pub stop_presignal: bool,
    pub working_stale: bool,
    pub turn_closed: Option<TurnClosure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnClosure {
    Authority { at: Option<DateTime<Utc>> },
    Interrupt { at: Option<DateTime<Utc>> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionObservation {
    Exited,
    Unknown,
    Replaying,
    AskPending { why: Why },
    Resting,
    Errored,
    TurnWorking,
    TurnFinished { fresh: bool },
    TurnInterrupted { fresh: bool },
}

pub fn observe_attention(
    input: AttentionInput,
    now: Option<DateTime<Utc>>,
) -> AttentionObservation {
    if input.process_exited {
        return AttentionObservation::Exited;
    }
    match input.stream {
        StreamState::Replaying => return AttentionObservation::Replaying,
        StreamState::Unavailable => return AttentionObservation::Unknown,
        StreamState::Live => {}
    }
    if input.observer_stale {
        AttentionObservation::Unknown
    } else if !input.live && (input.transcript_rows_seen || input.truncated_start) {
        AttentionObservation::Replaying
    } else if let Some(why) = input.ask {
        AttentionObservation::AskPending { why }
    } else if !input.live {
        AttentionObservation::Resting
    } else if input.error_live {
        AttentionObservation::Errored
    } else if input.turn_open {
        if input.stop_presignal {
            AttentionObservation::TurnFinished { fresh: false }
        } else if input.working_stale {
            AttentionObservation::Unknown
        } else {
            AttentionObservation::TurnWorking
        }
    } else if let Some(closed) = input.turn_closed {
        let at = match closed {
            TurnClosure::Authority { at } | TurnClosure::Interrupt { at } => at,
        };
        let fresh = match (now, at) {
            (Some(now), Some(at)) => now - at <= TimeDelta::seconds(60),
            _ => true,
        };
        match closed {
            TurnClosure::Authority { .. } => AttentionObservation::TurnFinished { fresh },
            TurnClosure::Interrupt { .. } => AttentionObservation::TurnInterrupted { fresh },
        }
    } else if input.truncated_start {
        AttentionObservation::Unknown
    } else {
        AttentionObservation::Resting
    }
}

fn string_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn classifies_rows_and_observes_session_facts() {
        let row = json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-5",
                "usage": {
                    "input_tokens": 11,
                    "cache_read_input_tokens": 20,
                    "cache_creation_input_tokens": 3
                }
            }
        });
        assert_eq!(classify_row(&row), RowKind::Assistant);
        let mut facts = SessionFacts::default();
        facts.observe_assistant(&row);
        assert_eq!(facts.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(facts.context_used_tokens, Some(34));
    }

    #[test]
    fn todo_write_becomes_current_only_after_success() {
        let mut todos = ClaudeTodos::default();
        let use_block = json!({
            "type": "tool_use",
            "name": "TodoWrite",
            "id": "toolu_1",
            "input": {"todos": [{
                "content": "move observation",
                "activeForm": "Moving observation",
                "status": "in_progress"
            }]}
        });
        assert!(matches!(
            todos.observe(&use_block),
            TodoDisposition::Absorbed
        ));
        assert!(todos.current().is_none());
        let result = json!({
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "is_error": false
        });
        assert!(matches!(todos.observe(&result), TodoDisposition::Absorbed));
        assert_eq!(
            todos.current().and_then(|list| list.current.as_deref()),
            Some("Moving observation")
        );
    }
}
