//! Claude's stream-JSON feed. Provider block identity and task lifecycle are
//! preserved independently of the terminal transcript's inferred turns.

mod answer;
mod asks;
mod condition;
mod fold;
mod input;
mod session;
pub(crate) mod update;

use std::collections::VecDeque;

pub use asks::{
    Ask, AskKind, AskState, AskWhy, DialogChoice, DialogChoices, ElicitationField,
    ElicitationFieldKind, ElicitationForm, dialog_choices, dialog_payload_summary,
};
use chrono::{DateTime, Utc};
pub(crate) use condition::check_projection_invariant;
pub use condition::{SdkPhase, SendGate, phase, send_gate};
pub use input::{
    ClaudeSdkCommand, ClaudeSdkInput, DialogAnswer, ElicitationAnswer, PermissionAnswer,
    PlanAnswer, QuestionAnswer, SdkAnswer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use session::{ContextMeter, ContextMeterSource, ContextUsage, McpServerFact, SessionFacts};
pub use update::{InFlightInput, InputFailure, PromptEcho};

use crate::AgentMessageKind;
use crate::claude::facts::ToolInvocation;
use crate::claude::runs;

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
    /// The tool use whose subagent produced this entry, when it was not the
    /// session's own. Stream-JSON carries a subagent's rows on the parent's
    /// stream with this id set. Kept apart from `block` so a row that
    /// arrives without its block — a result-only tail — still says whose
    /// it was.
    pub parent_tool_use_id: Option<String>,
    /// Payload clipping is separate from missing earlier feed entries.
    pub content_truncated: bool,
    final_row_id: Option<String>,
}

impl FeedEntry {
    /// The tool use whose subagent produced this entry, when it was not
    /// the session's own.
    pub fn parent_tool_use_id(&self) -> Option<&str> {
        self.parent_tool_use_id.as_deref()
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

/// This feed's exploration runs, projected over its native entries.
pub type FeedItem<'a> = runs::FeedItem<'a, FeedEntry>;
/// The lazy walk that yields them.
pub type FeedItems<'a> = runs::FeedItems<'a, FeedEntry>;

impl runs::RunEntry for FeedEntry {
    fn run_id(&self) -> u64 {
        self.id
    }

    fn exploration(&self) -> Option<&ToolInvocation> {
        // A subagent's reads are its own timeline, not the session's
        // exploration; they paint as attributed lines and never fold.
        if self.parent_tool_use_id().is_some() {
            return None;
        }
        let FeedEntryKind::Tool(tool) = &self.kind else {
            return None;
        };
        runs::groupable(&tool.invocation).then_some(&tool.invocation)
    }

    fn groups_with_previous(&self) -> bool {
        matches!(&self.kind, FeedEntryKind::Tool(tool) if tool.group_with_previous)
    }
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
    /// Grouping fact: this and the entry immediately before it are both
    /// read-only exploration. Stated by the fold from the tool's own
    /// name, never by renderer layout introspection.
    pub group_with_previous: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub details: Option<Value>,
    /// Set when the result is an Edit's or a Write's: which file moved,
    /// by how much, and the patch. Read from the same provider JSON the
    /// terminal chat reads.
    pub edit: Option<crate::claude::facts::LandedEdit>,
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
    /// The `Task`/`Agent` tool use that launched it. The lifecycle rows
    /// carry it, so the launch row and the task are one entry rather
    /// than two rows naming the same subagent.
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
    in_flight: Option<InFlightInput>,
    echo: Option<PromptEcho>,
    last_input_failure: Option<InputFailure>,
    session: SessionFacts,
    todos: crate::claude::todos::ClaudeTodos,
    cursor: u64,
    context_breakdown: Option<Box<ContextUsage>>,
    attachments: crate::attachments::AttachmentIndex,
}

impl ClaudeSdkLayer {
    pub fn todos(&self) -> Option<&crate::provider::TaskList> {
        self.todos.current()
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

    pub fn attachments(&self) -> &crate::attachments::AttachmentIndex {
        &self.attachments
    }

    pub(crate) fn attachments_mut(&mut self) -> &mut crate::attachments::AttachmentIndex {
        &mut self.attachments
    }

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
        self.clear_inputs("session stream closed");
    }

    pub(crate) fn invalidate(&mut self) {
        self.interrupt_streams();
        self.stale = true;
        self.clear_inputs("session stream closed");
    }

    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }

    /// The feed in file order with consecutive exploration entries grouped
    /// under their first entry id. A lone read or search remains an entry.
    pub fn feed_items(&self) -> FeedItems<'_> {
        FeedItems::new(&self.entries)
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
        self.cursor = self.cursor.max(seq);
        if row["parent_tool_use_id"].is_null()
            && matches!(
                row["type"].as_str(),
                Some("amux.claude_sdk.ready" | "conversation_reset")
            )
        {
            self.todos = crate::claude::todos::ClaudeTodos::default();
        }
        session::observe(self, row);
        self.attachments.observe_row(row);
        self.observe_input(row);
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
