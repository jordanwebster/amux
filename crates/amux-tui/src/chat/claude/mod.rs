//! The chat screen (`docs/CHAT.md`): the structured conversation view over
//! a Claude session, rendered inside the existing chrome.
//!
//! A screen within the chrome, not a fork of it: same alternate-screen
//! frame, same pure `render(Model, ViewState, FrameContext)` discipline,
//! same Command-only write surface. This module owns the chat's renderer-
//! local state — draft, cursor, kill buffer, panels — while the outer
//! `ChatView` owns the shared feed viewport. Every derivation a view
//! wants (phase, send gate, magnitudes, counts) comes from the Model; the
//! code here formats.
//!
//! Phase 4 scope: feed + composer. Ask panels, the reader, and read-only
//! chats are Phase 5; fleet entry bindings, the chrome-wide Ctrl+C guard,
//! and the `?` overlay are Phase 6 — [`crate::view::ViewState::open_chat`]
//! is the seam Phase 6's fleet binding will invoke.

pub(crate) mod ask_ui;
pub mod diff;
pub(crate) mod draft;
mod keys;
pub(crate) mod panel;
mod reader;
mod render;

use amux_ui::claude::{Ask, AskKind, AskState, ChatPhase, ToolInvocation};
use amux_ui::{AgentId, Attention, Command, Model, OpId, OpOutcome};
use ask_ui::AskUi;
use draft::ReviewDraft;
pub(crate) use keys::{handle_chat_key, handle_chat_paste};
use reader::{ReaderSource, ReaderView};
pub(crate) use render::{ask_panel_lines, claude_frame_parts};
use serde::{Deserialize, Serialize};

use crate::chat::inline::InlineAsk;
use crate::chat::viewport::ScrollIntent;
use crate::composer::Composer;
use crate::view::QuitGuard;

/// A dispatched prompt send being watched for its outcome (C5): the
/// finished op carries the failure fact, and the draft resurfaces from
/// here — send failures have no transcript record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingSend {
    op: OpId,
    text: String,
}

/// A dispatched ask answer being watched for a SYNCHRONOUS refusal (the
/// asynchronous path flips `AskState` in the Model; a reducer refusal
/// never touches the ask, so the view states it).
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

/// Renderer-local chat state. Never persisted, never authoritative.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub agent: AgentId,
    pub composer: Composer,
    pub(crate) scroll_intent: Option<ScrollIntent>,
    pending_send: Option<PendingSend>,
    pending_answer: Option<PendingAnswer>,
    /// A failed send, stated until the next keypress dismisses it (the
    /// Model keeps the outcome; dismissal is view state).
    send_failure: Option<String>,
    /// A refused answer dispatch, stated in the panel until dismissed.
    ask_failure: Option<String>,
    /// Panel state for the current ask head (C1); `None` when nothing is
    /// docked.
    ask_ui: Option<AskUi>,
    /// The fullscreen reader, when open.
    reader: Option<ReaderView>,
    /// The chrome-wide two-press quit guard (chat side — same type, same
    /// rule as the fleet's).
    pub quit_guard: QuitGuard,
    /// The configured leader character (view-config, copied at open):
    /// chat never shadows the leader, and the chords compose here.
    pub leader: char,
    /// A leader press is pending its chord key (`<leader> s` fleet,
    /// `<leader> d` shell).
    pub pending_leader: bool,
    /// Whether the kitty probe succeeded (view-config, copied at open):
    /// the `?` overlay's tier gate.
    pub kitty: bool,
    /// The `?` help overlay is open (any key closes it).
    pub help: bool,
    /// Whether completions show their whole body (`<leader> m`). Closed
    /// by default: a child's last message is a report, and a chat that
    /// opens every report it receives stops being readable at the exact
    /// moment several children finish at once.
    pub reports_open: bool,
    /// A child's ask docked where the composer would be (`<leader> a`,
    /// U2). The child's layer owns the panel and the answer; this chat
    /// owns only the decision to show it.
    pub(crate) inline_ask: Option<InlineAsk>,
    /// The one review this chat is drafting (`<leader> r`), page and token
    /// together. One per chat: a second review would need a second token
    /// and a second frozen diff, and a person reviewing two things at once
    /// is writing two messages.
    ///
    /// Boxed so a chat with no review stays small: the chat view enums are
    /// size-linted, and a whole frozen patch inline would dominate them.
    pub(crate) review: Option<Box<ReviewDraft>>,
    /// A dispatched diff request being watched for its result.
    pending_diff: Option<PendingDiff>,
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

    pub(crate) fn send_failure(&self) -> Option<&str> {
        self.send_failure.as_deref()
    }

    pub(crate) fn overlay_open(&self) -> bool {
        self.help || self.reader.is_some() || self.review_open()
    }

    /// The runtime edge minted an op for a dispatched command: remember
    /// prompt sends so the failed-op fact can resurface the draft, and
    /// ask answers so a synchronous refusal can be stated. Called by the
    /// run loop right after dispatch (the key handler returns the
    /// Command; the shell owns op identity).
    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        match command {
            Command::Claude(amux_ui::ClaudeCommand::SendPrompt { agent, text })
                if *agent == self.agent =>
            {
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
            Command::Claude(amux_ui::ClaudeCommand::AnswerAsk { agent, ask, .. })
                if *agent == self.agent =>
            {
                self.pending_answer = Some(PendingAnswer { op, ask: *ask });
            }
            Command::RequestDiff { agent, .. } if *agent == self.agent => {
                self.pending_diff = Some(PendingDiff { op });
            }
            _ => {}
        }
    }

    /// Reconcile view state against the Model after a fold: finished send
    /// ops resurface the draft with the failure stated (C5/D1, never
    /// clobbering newer text); finished answer ops state synchronous
    /// refusals; and the panel/reader sync to the current ask head —
    /// remote resolution dismisses, a new head gets a fresh panel, plan
    /// review opens the reader directly (C3).
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
                // The restored draft carries the review token back, so the
                // review behind it has to survive the failure too.
            } else {
                // The review left with the prompt; a new one starts frozen
                // against whatever the repository looks like then.
                self.review = None;
            }
            self.pending_send = None;
        }
        if let Some(pending) = &self.pending_answer
            && let Some(finished) = model.finished_op(pending.op)
        {
            if let OpOutcome::Error { error } = &finished.outcome {
                // Only state it while the ask still pends — a refusal for
                // an ask that resolved remotely explains nothing.
                let still_pending = model
                    .claude(self.agent)
                    .and_then(|layer| layer.ask_head())
                    .is_some_and(|head| {
                        head.id == pending.ask && matches!(head.state, AskState::Pending)
                    });
                if still_pending {
                    self.ask_failure = Some(error.message());
                    // An answer submitted FROM the reader that was refused
                    // synchronously must state its failure somewhere
                    // visible: the reader closes to the docked panel,
                    // which renders it — the same drop an async
                    // SendFailed takes, so no lost outcome ever hides
                    // behind the overlay.
                    if self.ask_reader_open() {
                        self.reader = None;
                    }
                }
            }
            self.pending_answer = None;
        }
        self.reconcile_diff(model);
        self.sync_ask(model);
        crate::chat::inline::reconcile(model, self.agent, &mut self.inline_ask);
    }

    /// A requested diff came back: freeze it into a review and put the page
    /// on screen. A patch the review core cannot read, and a repository
    /// that could not produce one at all, both state why in the footer
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
        // `b` freezes the same work against another base, and that is a new
        // review: a comment's line numbers mean nothing in a different
        // patch, so the old ones stay behind with the diff they were
        // written on, and the draft loses the token that stood for them.
        if let Some(previous) = self.review.take()
            && let Some(slot) = previous.slot
        {
            self.composer.remove_token(slot);
        }
        self.review = Some(Box::new(ReviewDraft::opened(
            crate::review::ReviewView::new(core, draft::BRANCH_BASE),
        )));
    }

    /// Sync panel and reader to the Model's ask head. Idempotent; also
    /// called defensively at key time (a chat may render before its first
    /// reconcile).
    pub(crate) fn sync_ask(&mut self, model: &Model) {
        let Some(ask) = model.claude(self.agent).and_then(|layer| layer.ask_head()) else {
            // Remote resolution (or local confirmation) dismisses the
            // panel; the B5 fact renders in the feed (C5).
            self.ask_ui = None;
            self.ask_failure = None;
            if self.ask_reader_open() {
                self.reader = None;
            }
            return;
        };
        if self.ask_ui.as_ref().map(|ui| ui.ask_id) != Some(ask.id) {
            // A new head gets a fresh panel; the old ask's typed state,
            // stated failure, and reader die with it.
            self.ask_ui = Some(AskUi::for_ask(ask));
            self.ask_failure = None;
            if self.ask_reader_open() {
                self.reader = None;
            }
            // Plan review opens the reader directly (C3): the full plan
            // is the point. Read-only chats render the fact panel
            // instead; `f` opens the reader.
            if !self.read_only(model)
                && ask_ui::is_plan(ask)
                && matches!(ask.state, AskState::Pending)
            {
                self.reader = Some(ReaderView::ask());
            }
        }
        // Once the answer is in flight the reader closes — the collapsed
        // pending marker renders docked (C5).
        if !matches!(ask.state, AskState::Pending) && self.ask_reader_open() {
            self.reader = None;
        }
    }

    /// Read-only chats render write affordances as absent, not disabled
    /// (F1) — one derivation, read wherever keys or frames branch.
    pub(crate) fn read_only(&self, model: &Model) -> bool {
        model
            .agent(self.agent)
            .is_some_and(|card| card.agent.readonly)
    }

    /// The pending ask head, when one is docked.
    pub(crate) fn ask_head<'m>(&self, model: &'m Model) -> Option<&'m Ask> {
        model.claude(self.agent).and_then(|layer| layer.ask_head())
    }

    /// The reader is open on the pending ask's document (as opposed to a
    /// resolved plan) — the form every ask-lifecycle rule dismisses.
    /// Open the fullscreen reader on a text attachment from the feed.
    pub(crate) fn open_text_reader(&mut self, name: String, body: String) {
        self.reader = Some(ReaderView {
            source: ReaderSource::Text { name, body },
            scroll: 0,
        });
    }

    fn ask_reader_open(&self) -> bool {
        matches!(
            self.reader,
            Some(ReaderView {
                source: ReaderSource::Ask,
                ..
            })
        )
    }

    /// The 1 Hz tick is needed only while something time-dependent is on
    /// screen (`docs/UI.md`): the working line's spinner and elapsed time,
    /// or a fresh optimistic echo aging toward its attention cap.
    pub fn needs_tick(&self, model: &Model) -> bool {
        let phase_is_working = matches!(
            amux_ui::claude::phase(model, self.agent),
            ChatPhase::Working
        );
        let fresh_echo_is_working = model
            .claude(self.agent)
            .is_some_and(|layer| !layer.pending_echoes().is_empty())
            && model
                .agent(self.agent)
                .is_some_and(|card| model.effective_attention(card) == Attention::Working);
        phase_is_working || fresh_echo_is_working
    }
}

/// The current entry watermark for an agent's feed: `evicted + retained`,
/// which equals the layer's next entry id (invariant-checked in amux-ui).
/// The paused rule's `N new entries` derives from the difference.
pub fn entry_watermark(model: &Model, agent: AgentId) -> u64 {
    model
        .claude(agent)
        .map(|layer| layer.evicted_entries() + layer.entry_count() as u64)
        .unwrap_or(0)
}

/// The one line a Claude ask reduces to when it is being reported in
/// somebody else's chat (U1): the act that is blocked, in this layer's
/// own words. A command says what would run, a question says what is
/// being asked, and everything else falls back to the identity the
/// panel's own header states — so a banner and the panel never name the
/// same ask two different ways.
pub(crate) fn ask_detail(model: &Model, agent: AgentId) -> Option<String> {
    let ask = model.claude(agent)?.ask_head()?;
    Some(match &ask.kind {
        AskKind::Permission {
            invocation:
                ToolInvocation::Bash {
                    command: Some(command),
                    ..
                },
            ..
        } => head_line(command),
        AskKind::Question { questions } => questions
            .first()
            .and_then(|question| question.question.as_deref().or(question.header.as_deref()))
            .map(head_line)
            .unwrap_or_else(|| "a question".to_string()),
        _ => panel::ask_identity(ask),
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
