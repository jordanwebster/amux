//! Claude's stream-JSON feed. Provider block identity and task lifecycle are
//! preserved independently of the terminal transcript's inferred turns.

mod asks;
mod condition;
mod fold;

use std::collections::VecDeque;

pub use asks::{Ask, AskKind, AskWhy, ElicitationField, ElicitationFieldKind, ElicitationForm};
use chrono::{DateTime, Utc};
pub(crate) use condition::check_projection_invariant;
pub use condition::{SdkPhase, SendGate, phase, send_gate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentMessageKind;
use crate::claude::facts::ToolInvocation;

pub const PROTOCOL: &str = "claude_sdk_v1";
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
    /// Payload clipping is separate from missing earlier feed entries.
    pub content_truncated: bool,
    final_row_id: Option<String>,
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
    /// None if input was absent, still streaming, or exceeded the payload cap.
    pub input: Option<Value>,
    pub input_json: String,
    pub finality: Finality,
    pub result: Option<ToolResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub details: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Running,
    Completed,
    Failed,
    Stopped,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskEntry {
    pub task_id: String,
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeSdkLayer {
    entries: VecDeque<FeedEntry>,
    next_entry_id: u64,
    evicted: u64,
    truncated_start: bool,
    cursors: VecDeque<MessageCursor>,
    asks: VecDeque<Ask>,
    next_ask_id: u64,
    turn: condition::TurnState,
    gap: bool,
    stale: bool,
    exited: bool,
    interrupted: bool,
    input_in_flight: bool,
}

impl ClaudeSdkLayer {
    pub fn asks(&self) -> impl Iterator<Item = &Ask> {
        self.asks.iter()
    }

    pub fn ask_head(&self) -> Option<&Ask> {
        self.asks.front()
    }

    pub fn ask_count(&self) -> usize {
        self.asks.len()
    }

    pub(crate) fn observe_exit(&mut self) {
        self.interrupt_streams();
        self.exited = true;
        self.asks.clear();
        self.input_in_flight = false;
    }

    pub(crate) fn invalidate(&mut self) {
        self.interrupt_streams();
        self.stale = true;
        self.input_in_flight = false;
    }

    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn tasks(&self) -> impl Iterator<Item = &TaskEntry> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            FeedEntryKind::Task(task) => Some(task),
            _ => None,
        })
    }

    pub fn history_truncated(&self) -> bool {
        self.truncated_start || self.evicted > 0
    }

    pub fn evicted_entries(&self) -> u64 {
        self.evicted
    }

    pub(crate) fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            // A replayed ready row predates the observed process exit.
            exited: self.exited,
            truncated_start: truncated,
            ..Self::default()
        };
    }

    pub(crate) fn observe(&mut self, seq: u64, _at: DateTime<Utc>, row: &Value) {
        condition::observe(self, row);
        asks::observe(self, row);
        fold::observe(self, seq, row);
    }

    pub(crate) fn interrupt_streams(&mut self) {
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

fn finality_mut(kind: &mut FeedEntryKind) -> Option<&mut Finality> {
    match kind {
        FeedEntryKind::Message(entry) => Some(&mut entry.finality),
        FeedEntryKind::Thinking(entry) => Some(&mut entry.finality),
        FeedEntryKind::Tool(entry) => Some(&mut entry.finality),
        _ => None,
    }
}
