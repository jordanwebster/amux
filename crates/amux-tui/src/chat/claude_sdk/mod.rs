//! The chat screen of a Claude session driven over stream-JSON.
//!
//! Claude is one provider behind two transports, so this screen is the
//! Claude chat: the same frame shell, the same block vocabulary, the same
//! reader and review page, drawn from the rows the session emits instead
//! of from a terminal transcript. What differs is what the session can
//! say — a model and a permission mode it reports outright, subagent
//! tasks it names, message blocks that arrive a token at a time — and
//! this module is where those facts become rows.
//!
//! It owns renderer-local state only: the draft, the reader, the review
//! being written, the leader. Everything authoritative comes from the
//! session layer in `amux_ui::claude_sdk`.

pub(crate) mod keys;
mod render;

use std::borrow::Cow;

use amux_ui::claude::{AcceptedPlan, ToolInvocation};
use amux_ui::claude_sdk::{FeedEntryKind, Finality, SdkPhase};
use amux_ui::{AgentId, Command, Model, OpId, OpOutcome};
pub(crate) use keys::{handle_chat_key, handle_chat_paste};
pub(crate) use render::claude_sdk_frame_parts;
use serde::{Deserialize, Serialize};

use crate::chat::claude_shared::draft::{self, ReviewDraft};
use crate::chat::claude_shared::reader::{ReaderContext, ReaderSource, ReaderView};
use crate::chat::inline::InlineAsk;
use crate::chat::viewport::ScrollIntent;
use crate::composer::Composer;
use crate::view::QuitGuard;

/// A dispatched prompt send being watched for its outcome: the finished
/// op carries the failure fact, and the draft resurfaces from here —
/// a send that never left has no row in the session to state it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingSend {
    op: OpId,
    text: String,
}

/// A dispatched diff request being watched for the frozen patch it will
/// return. The review page opens over the result, so nothing about it
/// exists until the op finishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingDiff {
    op: OpId,
}

/// Renderer-local chat state. Never persisted, never authoritative.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub agent: AgentId,
    pub composer: Composer,
    pub(crate) scroll_intent: Option<ScrollIntent>,
    pending_send: Option<PendingSend>,
    /// A failed send or a diff that could not be read, stated until the
    /// next keypress dismisses it.
    pub(crate) send_failure: Option<String>,
    /// The fullscreen reader, when open.
    pub(crate) reader: Option<ReaderView>,
    /// The chrome-wide two-press quit guard.
    pub quit_guard: QuitGuard,
    /// The configured leader character (view-config, copied at open).
    pub leader: char,
    /// A leader press is pending its chord key.
    pub pending_leader: bool,
    /// Whether the kitty probe succeeded: the `?` overlay's tier gate.
    pub kitty: bool,
    /// The `?` help overlay is open (any key closes it).
    pub help: bool,
    /// Whether completions show their whole body (`<leader> m`).
    pub reports_open: bool,
    /// A child's ask docked where the composer would be (`<leader> a`).
    pub(crate) inline_ask: Option<InlineAsk>,
    /// The one review this chat is drafting (`<leader> r`), page and
    /// token together. Boxed so a chat with no review stays small.
    pub(crate) review: Option<Box<ReviewDraft>>,
    pending_diff: Option<PendingDiff>,
}

impl View {
    pub fn open(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            composer: Composer::default(),
            scroll_intent: None,
            pending_send: None,
            send_failure: None,
            reader: None,
            quit_guard: QuitGuard::default(),
            leader,
            pending_leader: false,
            kitty,
            help: false,
            reports_open: false,
            inline_ask: None,
            review: None,
            pending_diff: None,
        }
    }

    /// A diff request is in flight; a second `<leader> r` must not queue
    /// another one behind it.
    pub(crate) fn diff_pending(&self) -> bool {
        self.pending_diff.is_some()
    }

    /// The review page is on screen, over the whole frame.
    pub(crate) fn review_open(&self) -> bool {
        self.review.as_ref().is_some_and(|draft| draft.open)
    }

    pub(crate) fn open_review_mut(&mut self) -> Option<&mut crate::review::ReviewView> {
        self.review
            .as_mut()
            .filter(|draft| draft.open)
            .map(|draft| &mut draft.view)
    }

    pub(crate) fn send_failure(&self) -> Option<&str> {
        self.send_failure.as_deref()
    }

    pub(crate) fn overlay_open(&self) -> bool {
        self.help || self.reader.is_some() || self.review_open()
    }

    /// Read-only chats render write affordances as absent, not disabled.
    pub(crate) fn read_only(&self, model: &Model) -> bool {
        model
            .agent(self.agent)
            .is_some_and(|card| card.agent.readonly)
    }

    /// Open the fullscreen reader on a text attachment from the feed.
    pub(crate) fn open_text_reader(&mut self, name: String, body: String) {
        self.reader = Some(ReaderView {
            source: ReaderSource::Text { name, body },
            scroll: 0,
        });
    }

    /// Open the fullscreen reader on a review someone sent.
    pub(crate) fn open_review_reader(
        &mut self,
        header: amux_ui::review::ReviewHeader,
        comments: Vec<amux_ui::review::ReviewComment>,
    ) {
        self.reader = Some(ReaderView {
            source: ReaderSource::Review {
                header: Box::new(header),
                comments,
            },
            scroll: 0,
        });
    }

    /// The runtime edge minted an op for a dispatched command: remember
    /// prompt sends so the failed-op fact can resurface the draft, and
    /// diff requests so the review page can open over the patch.
    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        match command {
            Command::ClaudeSdk(amux_ui::claude_sdk::ClaudeSdkCommand::SendPrompt {
                agent,
                text,
            }) if *agent == self.agent => {
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
            Command::RequestDiff { agent, .. } if *agent == self.agent => {
                self.pending_diff = Some(PendingDiff { op });
            }
            _ => {}
        }
    }

    /// Reconcile view state against the Model after a fold: a finished
    /// send op resurfaces the draft with the failure stated (never
    /// clobbering newer text), and a finished diff op freezes its patch
    /// into the review page.
    pub fn reconcile(&mut self, model: &Model) {
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
            } else {
                // The review left with the prompt; a new one starts frozen
                // against whatever the repository looks like then.
                self.review = None;
            }
            self.pending_send = None;
        }
        self.reconcile_diff(model);
        crate::chat::inline::reconcile(model, self.agent, &mut self.inline_ask);
    }

    /// A requested diff came back: freeze it into a review and put the
    /// page on screen. A patch the review core cannot read, and a
    /// repository that could not produce one at all, both state why
    /// rather than opening an empty page.
    fn reconcile_diff(&mut self, model: &Model) {
        let Some(pending) = self.pending_diff.clone() else {
            return;
        };
        let Some(finished) = model.finished_op(pending.op) else {
            return;
        };
        self.pending_diff = None;
        let response = match &finished.outcome {
            OpOutcome::DiffReady { response } => response,
            OpOutcome::Error { error } => {
                self.send_failure = Some(error.message());
                return;
            }
            _ => return,
        };
        let document = match amux_ui::review::parse_patch(
            &response.patch,
            response.identity.clone(),
            &response.files,
        ) {
            Ok(document) => document,
            Err(error) => {
                self.send_failure = Some(format!("the diff could not be read: {error}"));
                return;
            }
        };
        let core = amux_ui::review::Review::new(document, response.artifact.id.clone());
        // Freezing the same work against another base is a new review: a
        // comment's line numbers mean nothing in a different patch.
        if let Some(previous) = self.review.take()
            && let Some(slot) = previous.slot
        {
            self.composer.remove_token(slot);
        }
        self.review = Some(Box::new(ReviewDraft::opened(
            crate::review::ReviewView::new(core, draft::BRANCH_BASE),
        )));
    }

    /// The 1 Hz tick is needed only while something time-dependent is on
    /// screen: the working line's spinner, or a prompt still waiting for
    /// the session to echo it back.
    pub fn needs_tick(&self, model: &Model) -> bool {
        matches!(
            amux_ui::claude_sdk::phase(model, self.agent),
            SdkPhase::Working
        ) || model
            .claude_sdk(self.agent)
            .is_some_and(|layer| layer.pending_echo().is_some())
    }
}

/// Everything the shared reader needs from this chat. `None` when
/// nothing is being read.
///
/// Asks do not reach the reader from here yet: this session's obligations
/// carry kinds the shared panel does not describe, and half a panel would
/// offer actions it cannot take.
pub(crate) fn reader_context<'m>(model: &'m Model, chat: &'m View) -> Option<ReaderContext<'m>> {
    let reader = chat.reader.as_ref()?;
    let layer = model.claude_sdk(chat.agent)?;
    Some(ReaderContext {
        reader,
        ask: None,
        ask_ui: None,
        can_answer: false,
        accepted_plans: Cow::Owned(accepted_plans(model, chat.agent)),
        attachments: layer.attachments(),
        quit_guard_armed: chat.quit_guard.is_armed(),
    })
}

/// The plans this session put up for review and got through, oldest
/// first — what Ctrl+T steps back through.
///
/// Derived from the feed rather than retained beside it: a plan is a tool
/// use that succeeded, and the tool row already carries both the text and
/// the outcome, so a second copy could only disagree with the first.
pub(crate) fn accepted_plans(model: &Model, agent: AgentId) -> Vec<AcceptedPlan> {
    let Some(layer) = model.claude_sdk(agent) else {
        return Vec::new();
    };
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .filter(|tool| tool.result.as_ref().is_some_and(|result| !result.is_error))
        .filter_map(|tool| match &tool.invocation {
            ToolInvocation::Plan {
                plan: Some(plan), ..
            } => Some(AcceptedPlan {
                tool_use_id: tool.tool_use_id.clone(),
                plan: plan.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// The current entry watermark for this session's feed: `evicted +
/// retained`. The paused rule's `N new entries` derives from the
/// difference.
pub fn entry_watermark(model: &Model, agent: AgentId) -> u64 {
    model
        .claude_sdk(agent)
        .map(|layer| layer.evicted_entries() + layer.entry_count() as u64)
        .unwrap_or(0)
}

/// Whether any completion in this chat has a body behind its first line
/// — the exact condition under which `<leader> m` changes the screen.
pub(crate) fn has_foldable_completion(model: &Model, agent: AgentId) -> bool {
    model.claude_sdk(agent).is_some_and(|layer| {
        layer.entries().any(|entry| match &entry.kind {
            FeedEntryKind::AgentMessage(message) => {
                matches!(
                    message.kind.presentation(),
                    amux_ui::AgentMessagePresentation::Finished
                ) && amux_ui::message_digest(&message.text).hidden_lines > 0
            }
            _ => false,
        })
    })
}

/// Whether a streaming block is still open — the finality states a
/// person reads as "still arriving".
pub(crate) fn is_open(finality: Finality) -> bool {
    matches!(finality, Finality::Streaming | Finality::Stopped)
}

/// The one line this session's ask reduces to when it is being reported
/// in somebody else's chat: the act that is blocked, in plain words. A
/// command says what would run, a question says what is being asked, and
/// the rest name the obligation they are.
pub(crate) fn ask_detail(model: &Model, agent: AgentId) -> Option<String> {
    let ask = model.claude_sdk(agent)?.ask_head()?;
    Some(match &ask.kind {
        amux_ui::claude_sdk::AskKind::Permission {
            invocation:
                ToolInvocation::Bash {
                    command: Some(command),
                    ..
                },
            ..
        } => head_line(command),
        amux_ui::claude_sdk::AskKind::Permission { tool_name, .. } => tool_name.clone(),
        amux_ui::claude_sdk::AskKind::Plan { .. } => "a plan to review".to_string(),
        amux_ui::claude_sdk::AskKind::Question { questions } => questions
            .first()
            .and_then(|question| question.question.as_deref().or(question.header.as_deref()))
            .map(head_line)
            .unwrap_or_else(|| "a question".to_string()),
        amux_ui::claude_sdk::AskKind::Elicitation {
            server, message, ..
        } => match server {
            Some(server) => format!("{server}: {}", head_line(message)),
            None => head_line(message),
        },
        amux_ui::claude_sdk::AskKind::Dialog { dialog_kind, .. } => dialog_kind.clone(),
    })
}

/// The first line, marked when there were more.
fn head_line(text: &str) -> String {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or_default().trim().to_string();
    if lines.any(|line| !line.trim().is_empty()) {
        format!("{head} …")
    } else {
        head
    }
}
