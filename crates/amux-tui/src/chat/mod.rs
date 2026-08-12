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

pub mod composer;
mod keys;
mod markdown;
mod render;

pub use keys::{handle_chat_key, handle_chat_paste};
pub(crate) use render::build_chat_lines;

use amux_ui::claude::ChatPhase;
use amux_ui::{AgentId, Command, Model, OpId, OpOutcome};

use composer::Composer;

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

/// Renderer-local chat state. Never serialized, never authoritative.
#[derive(Clone, Debug)]
pub struct ChatView {
    pub agent: AgentId,
    pub composer: Composer,
    pub scroll: FeedScroll,
    pending_send: Option<PendingSend>,
    /// A failed send, stated until the next keypress dismisses it (the
    /// Model keeps the outcome; dismissal is view state).
    send_failure: Option<String>,
}

impl ChatView {
    pub fn open(agent: AgentId) -> Self {
        Self {
            agent,
            composer: Composer::default(),
            scroll: FeedScroll::Following,
            pending_send: None,
            send_failure: None,
        }
    }

    pub(crate) fn send_failure(&self) -> Option<&str> {
        self.send_failure.as_deref()
    }

    /// The runtime edge minted an op for a dispatched command: remember
    /// prompt sends so the failed-op fact can resurface the draft. Called
    /// by the run loop right after dispatch (the key handler returns the
    /// Command; the shell owns op identity).
    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        if let Command::SendPrompt { agent, text } = command
            && *agent == self.agent
        {
            self.pending_send = Some(PendingSend {
                op,
                text: text.clone(),
            });
        }
    }

    /// Reconcile view state against the Model after a fold: a finished
    /// send op either confirms (echo carries on until its transcript row)
    /// or fails — the failure is stated and the draft resurfaces (C5/D1).
    /// Never clobbers text the user typed in the meantime.
    pub fn reconcile(&mut self, model: &Model) {
        let Some(pending) = &self.pending_send else {
            return;
        };
        let Some(finished) = model.finished_op(pending.op) else {
            return;
        };
        if let OpOutcome::Error { error } = &finished.outcome {
            self.send_failure = Some(error.message.clone());
            if self.composer.is_empty() {
                self.composer.restore(&pending.text);
            }
        }
        self.pending_send = None;
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
