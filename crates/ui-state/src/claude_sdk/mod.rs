//! Claude SDK UI facade.
//!
//! Provider-derived standing facts live in `fold`; drawable history lives in
//! the store window, and this layer keeps reducer-local answer, input,
//! attachment, and connection overlays.

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

pub const PROTOCOL: &str = "claude_sdk_v1";
pub use ::fold::claude_sdk::CONTENT_BYTES_RETAINED;

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
    pub fn tasks(&self) -> impl Iterator<Item = &TaskEntry> {
        self.observation.tasks()
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

    pub(crate) fn restore_head(&mut self, tip: &::fold::claude_sdk::ClaudeSdkFold) {
        let attention = tip.restored_attention();
        let turn = match attention {
            Some(model::Attention::Idle) => ::fold::claude_sdk::TurnState::Idle,
            Some(model::Attention::Working) => ::fold::claude_sdk::TurnState::Working,
            Some(model::Attention::NeedsYou {
                why: model::Why::Finished,
            }) => ::fold::claude_sdk::TurnState::Finished,
            Some(model::Attention::NeedsYou { .. }) => ::fold::claude_sdk::TurnState::Working,
            Some(model::Attention::Unknown) | None => ::fold::claude_sdk::TurnState::Unknown,
        };
        self.observation.restore_condition(tip.through(), turn);
        for row in tip.restored_obligations() {
            if let Ok(payload) = serde_json::from_slice(&row.payload.0) {
                asks::observe(self, &payload);
            }
        }
        if !tip.restored_outstanding_known()
            || matches!(attention, Some(model::Attention::NeedsYou { .. })) && self.asks.is_empty()
        {
            self.stale = true;
        }
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
