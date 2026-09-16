//! Claude SDK UI facade.
//!
//! Provider-derived feed and standing facts live in `fold`; this layer keeps
//! reducer-local answer, input, attachment, and connection overlays.

mod answer;
mod asks;
mod condition;
mod input;
mod session;
pub(crate) mod update;

use std::collections::VecDeque;

pub use ::fold::claude_sdk::{
    AgentMessageEntry, AskKind, AskWhy, BlockId, BoundaryEntry, CompactionEntry, ElicitationField,
    ElicitationFieldKind, ElicitationForm, FeedEntry, FeedEntryKind, Finality, MessageEntry,
    PromptEntry, StatusEntry, TaskEntry, TaskState, TaskUsage, ThinkingEntry, TokenUsage,
    ToolEntry, ToolResult, TurnEntry, UnrecognizedEntry,
};
pub use asks::{
    Ask, AskState, DialogChoice, DialogChoices, dialog_choices, dialog_payload_summary,
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

use crate::claude::facts::ToolInvocation;
use crate::claude::runs;

pub const PROTOCOL: &str = "claude_sdk_v1";
pub use ::fold::claude_sdk::{CONTENT_BYTES_RETAINED, FEED_RETAINED};

pub type FeedItem<'a> = runs::FeedItem<'a, FeedEntry>;
pub type FeedItems<'a> = runs::FeedItems<'a, FeedEntry>;

impl runs::RunEntry for FeedEntry {
    fn run_id(&self) -> u64 {
        self.id
    }

    fn exploration(&self) -> Option<&ToolInvocation> {
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeSdkLayer {
    observation: ::fold::claude_sdk::Observation,
    pub(super) asks: VecDeque<Ask>,
    pub(super) next_ask_id: u64,
    pub(super) stale: bool,
    pub(super) exited: bool,
    pub(super) in_flight: Option<InFlightInput>,
    pub(super) echo: Option<PromptEcho>,
    pub(super) last_input_failure: Option<InputFailure>,
    attachments: crate::attachments::AttachmentIndex,
}

impl ClaudeSdkLayer {
    pub fn todos(&self) -> Option<&crate::provider::TaskList> {
        self.observation.todos()
    }
    pub fn cursor(&self) -> u64 {
        self.observation.cursor()
    }
    pub fn session(&self) -> &SessionFacts {
        self.observation.session()
    }
    pub fn context_breakdown(&self) -> Option<&ContextUsage> {
        self.observation.context_breakdown()
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
    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.observation.entries()
    }
    pub fn feed_items(&self) -> FeedItems<'_> {
        FeedItems::new(self.observation.entries_deque())
    }
    pub fn entry_count(&self) -> usize {
        self.observation.entry_count()
    }
    pub fn tasks(&self) -> impl Iterator<Item = &TaskEntry> {
        self.observation.tasks()
    }
    pub fn history_truncated(&self) -> bool {
        self.observation.history_truncated()
    }
    pub fn evicted_entries(&self) -> u64 {
        self.observation.evicted_entries()
    }

    pub(crate) fn observe_exit(&mut self) {
        self.observation.interrupt_streams();
        self.exited = true;
        self.asks.clear();
        self.clear_inputs("session stream closed");
    }

    pub(crate) fn invalidate(&mut self) {
        self.observation.interrupt_streams();
        self.stale = true;
        self.clear_inputs("session stream closed");
    }

    pub(crate) fn begin_window(&mut self, truncated: bool) {
        let exited = self.exited;
        *self = Self::default();
        self.exited = exited;
        self.observation.begin_window(truncated);
    }

    pub(crate) fn observe(&mut self, seq: u64, _at: DateTime<Utc>, row: &Value) {
        self.attachments.observe_row(row);
        self.observe_input(row);
        condition::observe(self, row);
        asks::observe(self, row);
        self.observation.observe(seq, row);
    }

    pub(super) fn observation(&self) -> &::fold::claude_sdk::Observation {
        &self.observation
    }
}
