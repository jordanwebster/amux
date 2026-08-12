//! The chat screen (`docs/CHAT.md`): the structured conversation view over
//! a Claude session, rendered inside the existing chrome.
//!
//! A screen within the chrome, not a fork of it: same alternate-screen
//! frame, same pure `render(Model, ViewState, FrameContext)` discipline,
//! same Command-only write surface. This module owns the chat's renderer-
//! local state — draft, cursor, kill buffer, scroll/follow — exactly the
//! set `docs/UI.md` allows a renderer to keep. Every derivation a view
//! wants (phase, send gate, magnitudes, counts) comes from the Model; the
//! code here formats.
//!
//! Phase 4 scope: feed + composer. Ask panels, the reader, and read-only
//! chats are Phase 5; fleet entry bindings, the chrome-wide Ctrl+C guard,
//! and the `?` overlay are Phase 6 — [`crate::view::ViewState::open_chat`]
//! is the seam Phase 6's fleet binding will invoke.

mod ask_ui;
pub mod composer;
pub mod diff;
mod keys;
mod markdown;
mod panel;
mod reader;
mod render;

pub use keys::{handle_chat_key, handle_chat_paste};
pub(crate) use render::build_chat_lines;

use amux_ui::claude::{Ask, AskState, ChatPhase};
use amux_ui::{AgentId, Command, Model, OpId, OpOutcome};

use ask_ui::AskUi;
use composer::Composer;
use reader::{ReaderSource, ReaderView};

/// Feed scroll state: sticky-bottom following until the user scrolls back
/// (`docs/CHAT.md` §Wireframes, scrolled-back frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedScroll {
    /// Pinned to the newest content; new entries keep the view at the
    /// bottom.
    Following,
    /// Scrolled back: the viewport is anchored `top_line` display rows from
    /// the feed's start (clamped at render — a stale anchor is tolerance
    /// territory, never an assertion).
    Paused {
        top_line: usize,
        /// The layer's entry watermark (`evicted + count`) when following
        /// paused — the honest `N new entries` counter's base.
        entry_watermark: u64,
    },
}

/// A dispatched prompt send being watched for its outcome (C5): the
/// finished op carries the failure fact, and the draft resurfaces from
/// here — send failures have no transcript artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSend {
    op: OpId,
    text: String,
}

/// A dispatched ask answer being watched for a SYNCHRONOUS refusal (the
/// asynchronous path flips `AskState` in the Model; a reducer refusal
/// never touches the ask, so the view states it).
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingAnswer {
    op: OpId,
    ask: u64,
}

/// Renderer-local chat state. Never serialized, never authoritative.
#[derive(Clone, Debug)]
pub struct ChatView {
    pub agent: AgentId,
    pub composer: Composer,
    pub scroll: FeedScroll,
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
}

impl ChatView {
    pub fn open(agent: AgentId) -> Self {
        Self {
            agent,
            composer: Composer::default(),
            scroll: FeedScroll::Following,
            pending_send: None,
            pending_answer: None,
            send_failure: None,
            ask_failure: None,
            ask_ui: None,
            reader: None,
        }
    }

    pub(crate) fn send_failure(&self) -> Option<&str> {
        self.send_failure.as_deref()
    }

    /// The runtime edge minted an op for a dispatched command: remember
    /// prompt sends so the failed-op fact can resurface the draft, and
    /// ask answers so a synchronous refusal can be stated. Called by the
    /// run loop right after dispatch (the key handler returns the
    /// Command; the shell owns op identity).
    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        match command {
            Command::SendPrompt { agent, text } if *agent == self.agent => {
                self.pending_send = Some(PendingSend {
                    op,
                    text: text.clone(),
                });
            }
            Command::AnswerAsk { agent, ask, .. } if *agent == self.agent => {
                self.pending_answer = Some(PendingAnswer { op, ask: *ask });
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
                self.send_failure = Some(error.message.clone());
                if self.composer.is_empty() {
                    self.composer.restore(&pending.text);
                }
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
                    self.ask_failure = Some(error.message.clone());
                    // An answer submitted FROM the reader that was refused
                    // synchronously must state its failure somewhere
                    // visible: the reader closes to the docked panel,
                    // which renders it — the same drop an async
                    // SendFailed takes, so no lost outcome ever hides
                    // behind the overlay.
                    if matches!(
                        self.reader,
                        Some(ReaderView {
                            source: ReaderSource::Ask,
                            ..
                        })
                    ) {
                        self.reader = None;
                    }
                }
            }
            self.pending_answer = None;
        }
        self.sync_ask(model);
    }

    /// Sync panel and reader to the Model's ask head. Idempotent; also
    /// called defensively at key time (a chat may render before its first
    /// reconcile).
    pub(crate) fn sync_ask(&mut self, model: &Model) {
        let readonly = self.read_only(model);
        let head_exists = {
            let head = model.claude(self.agent).and_then(|layer| layer.ask_head());
            match head {
                None => false,
                Some(ask) => {
                    let fresh = self.ask_ui.as_ref().map(|ui| ui.ask_id) != Some(ask.id);
                    if fresh {
                        self.ask_ui = Some(AskUi::for_ask(ask));
                        self.ask_failure = None;
                        // A reader left open for the previous ask is stale.
                        if matches!(
                            self.reader,
                            Some(ReaderView {
                                source: ReaderSource::Ask,
                                ..
                            })
                        ) {
                            self.reader = None;
                        }
                        // Plan review opens the reader directly (C3): the
                        // full plan is the point. Read-only chats render
                        // the fact panel instead; `f` opens the reader.
                        if !readonly
                            && ask_ui::is_plan(ask)
                            && matches!(ask.state, AskState::Pending)
                        {
                            self.reader = Some(ReaderView {
                                source: ReaderSource::Ask,
                                scroll: 0,
                            });
                        }
                    }
                    // Once the answer is in flight the reader closes — the
                    // collapsed pending marker renders docked (C5).
                    if !matches!(ask.state, AskState::Pending)
                        && matches!(
                            self.reader,
                            Some(ReaderView {
                                source: ReaderSource::Ask,
                                ..
                            })
                        )
                    {
                        self.reader = None;
                    }
                    true
                }
            }
        };
        if !head_exists {
            // Remote resolution (or local confirmation) dismisses the
            // panel; the B5 fact renders in the feed (C5).
            self.ask_ui = None;
            self.ask_failure = None;
            if matches!(
                self.reader,
                Some(ReaderView {
                    source: ReaderSource::Ask,
                    ..
                })
            ) {
                self.reader = None;
            }
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

    /// The 1 Hz tick is needed only while something time-dependent is on
    /// screen (`docs/UI.md`): the working line's spinner and elapsed time.
    pub fn needs_tick(&self, model: &Model) -> bool {
        matches!(model.claude_phase(self.agent), ChatPhase::Working)
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
