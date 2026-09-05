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

use amux_ui::claude::facts::ask_document;
use amux_ui::claude::{AcceptedPlan, ToolInvocation};
use amux_ui::claude_sdk::{
    Ask, AskKind, AskState, ClaudeSdkCommand, FeedEntryKind, Finality, PermissionAnswer, PlanAnswer,
    SdkAnswer, SdkPhase, SendGate,
};
use amux_ui::{AgentId, Command, Model, OpId, OpOutcome};
use chrono::{DateTime, Utc};
pub(crate) use keys::{handle_chat_key, handle_chat_paste};
pub(crate) use render::claude_sdk_frame_parts;
use serde::{Deserialize, Serialize};

use crate::chat::claude_shared::ask_ui::{AskUi, PanelAnswer};
use crate::chat::claude_shared::draft::{self, ReviewDraft};
use crate::chat::claude_shared::reader::{ReaderContext, ReaderSource, ReaderView};
use crate::chat::claude_shared::{AnswerSummary, SharedAsk, SharedAskKind, SharedAskState};
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

/// A dispatched ask answer being watched for a SYNCHRONOUS refusal: the
/// asynchronous path flips the ask's own state in the Model, but a
/// reducer refusal never touches the ask, so the view states it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingAnswer {
    op: OpId,
    ask: u64,
}

/// A dispatched diff request being watched for the frozen patch it will
/// return. The review page opens over the result, so nothing about it
/// exists until the op finishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingDiff {
    op: OpId,
}

/// A dispatched context-breakdown request. The answer arrives as a row
/// like any other, but the op is what says WHEN it was asked for, and the
/// overlay states the age of what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingBreakdown {
    op: OpId,
}

/// This session's ask, as the shared panels and reader read it.
///
/// The session names each obligation outright — a plan is a plan row, not
/// a tool use to recognize — so the only thing derived here is the
/// ask-time document: this layer keeps the request's own input rather
/// than a second copy of the diff computed from it.
pub(crate) fn shared_ask(ask: &Ask) -> SharedAsk<'_> {
    let mut document = None;
    let kind = match &ask.kind {
        AskKind::Permission {
            tool_name,
            invocation,
            suggestions,
        } => {
            document = ask_document(Some(tool_name), &ask.input).map(Cow::Owned);
            SharedAskKind::Permission {
                tool_name: Some(tool_name),
                invocation,
                suggestions,
            }
        }
        // The plan's own file path travels with the answer, not on
        // screen: a person reviews the plan, not where it was written.
        AskKind::Plan { plan, .. } => SharedAskKind::Plan {
            plan: plan.as_deref(),
        },
        AskKind::Question { questions } => SharedAskKind::Question { questions },
        AskKind::Elicitation {
            server,
            message,
            form,
        } => SharedAskKind::Elicitation {
            server: server.as_deref(),
            message,
            form,
        },
        AskKind::Dialog {
            dialog_kind,
            payload,
        } => SharedAskKind::Dialog {
            dialog_kind,
            payload,
        },
    };
    SharedAsk {
        id: ask.id,
        kind,
        document,
        state: match &ask.state {
            AskState::Pending => SharedAskState::Pending,
            AskState::AnsweredOptimistic { answer, .. } => SharedAskState::Answered {
                summary: answer_summary(answer),
            },
            AskState::SendFailed { message } => SharedAskState::Failed { message },
        },
        // Every ask this session raises can be answered from here: the
        // provider takes a typed decision, so there is no menu shape to
        // recognize and no refusal to state.
        refusal: None,
    }
}

/// What an in-flight answer was, in the words the collapsed marker uses.
fn answer_summary(answer: &SdkAnswer) -> AnswerSummary {
    use amux_ui::claude_sdk::{DialogAnswer, ElicitationAnswer};
    match answer {
        SdkAnswer::Permission(PermissionAnswer::AllowOnce) => AnswerSummary::AllowedOnce,
        SdkAnswer::Permission(PermissionAnswer::AllowScoped { .. }) => AnswerSummary::AllowedScoped,
        SdkAnswer::Permission(PermissionAnswer::Deny { .. }) => AnswerSummary::Denied,
        SdkAnswer::Plan(PlanAnswer::ApproveAuto) => AnswerSummary::PlanApprovedAuto,
        SdkAnswer::Plan(PlanAnswer::ApproveManual) => AnswerSummary::PlanApprovedManual,
        SdkAnswer::Plan(PlanAnswer::RequestChanges { .. }) => AnswerSummary::ChangesRequested,
        SdkAnswer::Question(_) => AnswerSummary::QuestionAnswered,
        SdkAnswer::Elicitation(ElicitationAnswer::Accept { .. }) => AnswerSummary::FormSent,
        SdkAnswer::Elicitation(ElicitationAnswer::Decline) => AnswerSummary::FormDeclined,
        SdkAnswer::Elicitation(ElicitationAnswer::Cancel) => AnswerSummary::Cancelled,
        SdkAnswer::Dialog(DialogAnswer::Choose { .. }) => AnswerSummary::DialogChosen,
        SdkAnswer::Dialog(DialogAnswer::Cancel) => AnswerSummary::Cancelled,
    }
}

/// The command a finished panel answer becomes, addressed to the agent
/// whose ask it is. The parent's chat reaches this too when it hosts a
/// child's ask, so both places send the identical command.
pub(crate) fn answer_command(agent: AgentId, ask: u64, answer: PanelAnswer) -> Command {
    let answer = match answer {
        PanelAnswer::Elicitation(answer) => SdkAnswer::Elicitation(answer),
        PanelAnswer::Dialog(answer) => SdkAnswer::Dialog(answer),
        PanelAnswer::Claude(answer) => match answer {
            amux_ui::claude::answer::AskAnswer::Permission(answer) => SdkAnswer::Permission(answer),
            amux_ui::claude::answer::AskAnswer::Plan(answer) => SdkAnswer::Plan(answer),
            amux_ui::claude::answer::AskAnswer::Question(response) => {
                SdkAnswer::Question(response.answers)
            }
        },
    };
    Command::ClaudeSdk(ClaudeSdkCommand::AnswerAsk {
        agent,
        ask,
        answer,
    })
}

/// Whether this client may answer the ask on screen at all: the session
/// takes an answer only while it is waiting for one, and a read-only
/// observer never answers.
pub(crate) fn allows_answer(model: &Model, chat: &View) -> bool {
    !chat.read_only(model)
        && amux_ui::claude_sdk::send_gate(model, chat.agent) == SendGate::NeedsYou
}

/// Renderer-local chat state. Never persisted, never authoritative.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub agent: AgentId,
    pub composer: Composer,
    pub(crate) scroll_intent: Option<ScrollIntent>,
    pending_send: Option<PendingSend>,
    pending_answer: Option<PendingAnswer>,
    /// A failed send or a diff that could not be read, stated until the
    /// next keypress dismisses it.
    pub(crate) send_failure: Option<String>,
    /// A refused answer dispatch, stated in the panel until dismissed.
    pub(crate) ask_failure: Option<String>,
    /// Panel state for the current ask head; `None` when nothing is
    /// docked.
    pub(crate) ask_ui: Option<AskUi>,
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
    /// The context breakdown is on screen, over the whole frame.
    pub(crate) context_open: bool,
    pending_breakdown: Option<PendingBreakdown>,
    /// When the breakdown on screen was fetched. `None` when it arrived
    /// from somebody else's request and this client never asked.
    context_fetched: Option<DateTime<Utc>>,
}

impl View {
    pub fn open(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            composer: Composer::default(),
            scroll_intent: None,
            pending_send: None,
            pending_answer: None,
            send_failure: None,
            ask_failure: None,
            ask_ui: None,
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
            context_open: false,
            pending_breakdown: None,
            context_fetched: None,
        }
    }

    /// How old what the overlay shows is, in the words its footer uses.
    pub(crate) fn context_age(&self, now: DateTime<Utc>) -> String {
        if self.pending_breakdown.is_some() {
            return "fetching…".to_string();
        }
        let Some(fetched) = self.context_fetched else {
            // The numbers came from somebody else's request, or from a
            // client that has since been reopened: the overlay can say
            // what it shows is a snapshot, but not how old.
            return "a snapshot".to_string();
        };
        let seconds = (now - fetched).num_seconds().max(0);
        match seconds {
            0..=9 => "fetched just now".to_string(),
            10..=59 => format!("fetched {seconds}s ago"),
            _ => format!("fetched {}m ago", seconds / 60),
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
        self.help || self.reader.is_some() || self.review_open() || self.context_open
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
            Command::ClaudeSdk(amux_ui::claude_sdk::ClaudeSdkCommand::AnswerAsk {
                agent,
                ask,
                ..
            }) if *agent == self.agent => {
                self.pending_answer = Some(PendingAnswer { op, ask: *ask });
            }
            Command::ClaudeSdk(
                amux_ui::claude_sdk::ClaudeSdkCommand::RequestContextBreakdown { agent },
            ) if *agent == self.agent => {
                self.pending_breakdown = Some(PendingBreakdown { op });
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
        self.reconcile_answer(model);
        self.reconcile_breakdown(model);
        self.reconcile_diff(model);
        self.sync_ask(model);
        crate::chat::inline::reconcile(model, self.agent, &mut self.inline_ask);
    }

    /// A dispatched answer the reducer refused outright: the panel states
    /// why, while the ask it was for is still the one on screen.
    fn reconcile_answer(&mut self, model: &Model) {
        let Some(pending) = self.pending_answer.clone() else {
            return;
        };
        let Some(finished) = model.finished_op(pending.op) else {
            return;
        };
        self.pending_answer = None;
        if let OpOutcome::Error { error } = &finished.outcome {
            let still_pending = model
                .claude_sdk(self.agent)
                .and_then(|layer| layer.ask_head())
                .is_some_and(|head| {
                    head.id == pending.ask && matches!(head.state, AskState::Pending)
                });
            if still_pending {
                self.ask_failure = Some(error.message());
                // A refusal collected from the reader has to land
                // somewhere visible: the reader closes to the docked
                // panel, which states it.
                if self.ask_reader_open() {
                    self.reader = None;
                }
            }
        }
    }

    /// The requested breakdown settled: stamp when it was fetched, or
    /// state why nothing came back. The overlay stays open either way —
    /// closing it would hide the answer it was opened for.
    fn reconcile_breakdown(&mut self, model: &Model) {
        let Some(pending) = self.pending_breakdown.clone() else {
            return;
        };
        let Some(finished) = model.finished_op(pending.op) else {
            return;
        };
        self.pending_breakdown = None;
        match &finished.outcome {
            OpOutcome::Error { error } => self.send_failure = Some(error.message()),
            // The clock is the Model's: a frame renders against the same
            // instant, so the stated age never disagrees with it.
            _ => self.context_fetched = model.now(),
        }
    }

    /// Sync panel and reader to the session's ask head. Idempotent; also
    /// called at key time, because a chat may render before its first
    /// reconcile.
    pub(crate) fn sync_ask(&mut self, model: &Model) {
        let Some(ask) = model
            .claude_sdk(self.agent)
            .and_then(|layer| layer.ask_head())
        else {
            // The ask resolved — here or on another client; the feed
            // carries what became of it.
            self.ask_ui = None;
            self.ask_failure = None;
            if self.ask_reader_open() {
                self.reader = None;
            }
            return;
        };
        if self.ask_ui.as_ref().map(|ui| ui.ask_id) != Some(ask.id) {
            // A new head gets a fresh panel; the old ask's typed state,
            // stated failure and reader die with it.
            self.ask_ui = Some(AskUi::for_ask(&shared_ask(ask)));
            self.ask_failure = None;
            if self.ask_reader_open() {
                self.reader = None;
            }
            // A plan is read before it is approved, so it opens the
            // reader directly. Read-only chats get the fact panel, with
            // `f` to read it.
            if !self.read_only(model)
                && matches!(ask.kind, AskKind::Plan { .. })
                && matches!(ask.state, AskState::Pending)
            {
                self.reader = Some(ReaderView::ask());
            }
        }
        // Once the answer is in flight the reader closes: the collapsed
        // pending marker renders docked.
        if !matches!(ask.state, AskState::Pending) && self.ask_reader_open() {
            self.reader = None;
        }
    }

    /// The ask this session is waiting on, when it is waiting on one.
    pub(crate) fn ask_head<'m>(&self, model: &'m Model) -> Option<&'m Ask> {
        model
            .claude_sdk(self.agent)
            .and_then(|layer| layer.ask_head())
    }

    /// The reader is open on the pending ask's document, as opposed to a
    /// plan this session already got through.
    pub(crate) fn ask_reader_open(&self) -> bool {
        matches!(
            self.reader,
            Some(ReaderView {
                source: ReaderSource::Ask,
                ..
            })
        )
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
pub(crate) fn reader_context<'m>(model: &'m Model, chat: &'m View) -> Option<ReaderContext<'m>> {
    let reader = chat.reader.as_ref()?;
    let layer = model.claude_sdk(chat.agent)?;
    Some(ReaderContext {
        reader,
        ask: layer.ask_head().map(shared_ask),
        ask_ui: chat.ask_ui.as_ref(),
        can_answer: allows_answer(model, chat),
        accepted_plans: Cow::Owned(accepted_plans(model, chat.agent)),
        attachments: layer.attachments(),
        quit_guard_armed: chat.quit_guard.is_armed(),
    })
}

/// Whether the open reader's ask can be answered from here.
pub(crate) fn reader_actionable(model: &Model, chat: &View) -> bool {
    reader_context(model, chat)
        .is_some_and(|ctx| crate::chat::claude_shared::reader::answer_actionable(&ctx))
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
