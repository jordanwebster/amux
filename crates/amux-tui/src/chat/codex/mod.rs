//! Codex-native chat view state. It consumes only `amux_ui::codex` facts;
//! no Claude entry, ask, phase, or document type crosses this module.

pub(crate) mod keys;
pub(crate) mod render;

use amux_ui::codex::{AskContext, CodexCommand, CodexPhase};
use amux_ui::{AgentId, Command, Model, OpId, OpOutcome};
pub(crate) use keys::{handle_chat_key, handle_chat_paste};
pub(crate) use render::codex_frame_parts;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::inline::InlineAsk;
use crate::chat::viewport::ScrollIntent;
use crate::composer::Composer;
use crate::view::QuitGuard;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingSend {
    op: OpId,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PendingAnswer {
    op: OpId,
    request_id: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct View {
    pub agent: AgentId,
    pub composer: Composer,
    pub(crate) scroll_intent: Option<ScrollIntent>,
    pending_send: Option<PendingSend>,
    pending_queue: Option<OpId>,
    pending_answer: Option<PendingAnswer>,
    pub(crate) send_failure: Option<String>,
    pub(crate) answer_failure: Option<String>,
    pub(crate) approval_request: Option<Value>,
    pub(crate) approval_cursor: usize,
    pub quit_guard: QuitGuard,
    pub leader: char,
    pub pending_leader: bool,
    pub kitty: bool,
    pub help: bool,
    /// Creation choices supplied by the CLI for the initial structured view.
    pub(crate) configuration_label: Option<String>,
    /// Whether completions show their whole body (`<leader> m`). Closed
    /// by default: a child's last message is a report, and a chat that
    /// opens every report it receives stops being readable at the exact
    /// moment several children finish at once.
    pub reports_open: bool,
    /// A child's ask docked where the composer would be (`<leader> a`,
    /// U2). The child's layer owns the panel and the answer; this chat
    /// owns only the decision to show it.
    pub(crate) inline_ask: Option<InlineAsk>,
}

impl View {
    pub(crate) fn open(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            composer: Composer::default(),
            scroll_intent: None,
            pending_send: None,
            pending_queue: None,
            pending_answer: None,
            send_failure: None,
            answer_failure: None,
            approval_request: None,
            approval_cursor: 0,
            quit_guard: QuitGuard::default(),
            leader,
            pending_leader: false,
            kitty,
            help: false,
            configuration_label: None,
            reports_open: false,
            inline_ask: None,
        }
    }

    pub(crate) fn read_only(&self, model: &Model) -> bool {
        model
            .agent(self.agent)
            .is_some_and(|card| card.agent.readonly)
    }

    pub(crate) fn overlay_open(&self) -> bool {
        self.help
    }

    pub(crate) fn reconcile(&mut self, model: &Model) {
        super::queue::reconcile(
            model,
            &mut self.pending_queue,
            &mut self.composer,
            &mut self.send_failure,
        );
        if let Some(pending) = &self.pending_send
            && let Some(finished) = model.finished_op(pending.op)
        {
            if let OpOutcome::Error { error } = &finished.outcome {
                self.send_failure = Some(error.message());
                // The command carries the EXPORTED text; a draft with
                // tokens is not that, so the composer's own set-aside copy
                // wins when it has one.
                if self.composer.is_empty() && !self.composer.restore_sent() {
                    self.composer.restore(&pending.text);
                }
            }
            self.pending_send = None;
        }
        if let Some(pending) = &self.pending_answer
            && let Some(finished) = model.finished_op(pending.op)
        {
            if let OpOutcome::Error { error } = &finished.outcome
                && model
                    .codex(self.agent)
                    .and_then(|layer| layer.ask_head())
                    .is_some_and(|ask| ask.request_id == pending.request_id)
            {
                self.answer_failure = Some(error.message());
            }
            self.pending_answer = None;
        }

        let request = model
            .codex(self.agent)
            .and_then(|layer| layer.ask_head())
            .map(|ask| ask.request_id.clone());
        if request != self.approval_request {
            self.approval_request = request;
            self.approval_cursor = 0;
            self.answer_failure = None;
        }
        if let Some(count) = model
            .codex(self.agent)
            .and_then(|layer| layer.ask_head())
            .map(|ask| ask.actions.len())
        {
            self.approval_cursor = self.approval_cursor.min(count.saturating_sub(1));
        }
        crate::chat::inline::reconcile(model, self.agent, &mut self.inline_ask);
    }

    pub(crate) fn note_dispatched(&mut self, op: OpId, command: &Command) {
        if matches!(command, Command::Queue(_)) {
            self.pending_queue = Some(op);
        }
        match command {
            Command::Codex(
                CodexCommand::Prompt { agent, text } | CodexCommand::Steer { agent, text },
            ) if *agent == self.agent => {
                self.pending_send = Some(PendingSend {
                    op,
                    text: text.clone(),
                });
            }
            Command::SendPromptWithAttachments { agent, text, .. } if *agent == self.agent => {
                self.pending_send = Some(PendingSend {
                    op,
                    text: text.clone(),
                });
            }
            Command::Codex(CodexCommand::Answer {
                agent, request_id, ..
            }) if *agent == self.agent => {
                self.pending_answer = Some(PendingAnswer {
                    op,
                    request_id: request_id.clone(),
                });
            }
            _ => {}
        }
    }

    pub(crate) fn needs_tick(&self, model: &Model) -> bool {
        matches!(
            amux_ui::codex::phase(model, self.agent),
            CodexPhase::Thinking | CodexPhase::Responding { .. } | CodexPhase::Executing { .. }
        )
    }
}

/// The one line a Codex ask reduces to when it is reported in somebody
/// else's chat (U1). Each context says the act it is blocked on in this
/// layer's own vocabulary: a command names itself, a file change names
/// the first file and counts the rest, a dynamic tool names the tool the
/// way its own row does.
pub(crate) fn ask_detail(model: &Model, agent: AgentId) -> Option<String> {
    let ask = model.codex(agent)?.ask_head()?;
    Some(match &ask.context {
        AskContext::Command { command, .. } => {
            let mut lines = command.lines();
            let head = lines.next().unwrap_or_default().trim().to_string();
            if lines.any(|line| !line.trim().is_empty()) {
                format!("{head} …")
            } else {
                head
            }
        }
        AskContext::FileChange { changes, .. } => match changes.split_first() {
            Some((first, [])) => first.path.clone(),
            Some((first, rest)) => format!("{} · +{} more", first.path, rest.len()),
            None => "a file change".to_string(),
        },
        AskContext::Permissions { .. } => "a permission change".to_string(),
        AskContext::DynamicTool {
            tool, namespace, ..
        } => match namespace {
            Some(namespace) => format!("{namespace}/{tool}"),
            None => tool.clone(),
        },
    })
}
