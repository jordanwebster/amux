//! Ask-panel ViewState: the C2/C3/C4 stage machines (`docs/CHAT.md`
//! §Asks, §State transitions).
//!
//! Renderer-local state only — the Ask itself (kind, payload, lifecycle
//! state) lives in the Model; this module holds what the user is doing
//! with the panel: the selection cursor, the question-form tabs and
//! drafts, the optional text stages. Esc steps back one stage and floors
//! at the menu — the panel is not dismissible while its ask pends — and
//! every stage-back preserves typed form state verbatim (P8). Keys
//! produce typed [`AskAnswer`]s; bytes never appear here (the daemon
//! chooses the keystrokes that carry an answer).

use amux_ui::claude::answer::{
    AskAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};
use amux_ui::claude::{Ask, AskKind, QuestionFact, ToolInvocation};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::composer::{self, Composer};

/// Panel state for one ask (keyed by `ask_id`: a new head gets a fresh
/// panel; the old ask's typed state dies with its ask).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AskUi {
    pub ask_id: u64,
    pub stage: AskStage,
    /// Deny-feedback draft (C2): survives Esc back to the menu.
    pub deny_feedback: Composer,
    /// Request-changes draft (C3): survives Esc back to the action row.
    pub plan_feedback: Composer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum AskStage {
    /// The numbered action list — permission actions, or the plan's
    /// three-way review.
    Menu { cursor: usize },
    /// The optional one-line deny feedback (C2); Enter with empty text is
    /// a plain deny.
    DenyFeedback,
    /// Plan request-changes feedback (C3): mandatory, will not submit
    /// empty. Renders in the reader when it is open, docked otherwise.
    PlanFeedback,
    /// The question form (C4).
    Question(QuestionUi),
}

/// What a panel keypress asked for.
#[derive(Debug, PartialEq)]
pub(crate) enum AskKeyOutcome {
    /// Consumed; view state may have changed.
    Handled,
    /// Dispatch this typed answer for the ask.
    Answer(AskAnswer),
    /// Open the reader on the ask's document (`f`).
    OpenReader,
    /// Not a panel key — the caller may route it elsewhere (feed
    /// scrolling).
    NotHandled,
}

/// Is this ask the plan-review permission variant?
pub(crate) fn is_plan(ask: &Ask) -> bool {
    matches!(
        &ask.kind,
        AskKind::Permission {
            invocation: ToolInvocation::Plan { .. },
            ..
        }
    )
}

/// Whether the ask carries anything the reader can show (`f`'s liveness:
/// hints never advertise dead keys).
pub(crate) fn has_readable(ask: &Ask) -> bool {
    ask.document.is_some()
        || matches!(
            &ask.kind,
            AskKind::Permission {
                invocation: ToolInvocation::Plan { plan: Some(_), .. },
                ..
            }
        )
}

impl AskUi {
    pub fn for_ask(ask: &Ask) -> Self {
        let stage = match &ask.kind {
            AskKind::Question { questions } => AskStage::Question(QuestionUi::new(questions)),
            _ => AskStage::Menu { cursor: 0 },
        };
        Self {
            ask_id: ask.id,
            stage,
            deny_feedback: Composer::default(),
            plan_feedback: Composer::default(),
        }
    }

    /// The currently focused text field, if a text stage is open — the
    /// paste target (P2: printables belong to the draft).
    pub fn active_field(&mut self) -> Option<&mut Composer> {
        match &mut self.stage {
            AskStage::DenyFeedback => Some(&mut self.deny_feedback),
            AskStage::PlanFeedback => Some(&mut self.plan_feedback),
            AskStage::Question(form) if form.editing_other => {
                form.drafts.get_mut(form.tab).map(|draft| &mut draft.other)
            }
            _ => None,
        }
    }

    /// The action-list cursor: the menu stage's, or the top row when
    /// another stage is open (the list still renders under text stages
    /// in some frames).
    pub fn menu_cursor(&self) -> usize {
        match &self.stage {
            AskStage::Menu { cursor } => *cursor,
            _ => 0,
        }
    }

    /// Esc: one stage back toward the menu, typed state preserved; `false`
    /// at the floor (the panel stays — it is not dismissible while its ask
    /// pends).
    pub fn step_back(&mut self) -> bool {
        match &mut self.stage {
            AskStage::Menu { .. } => false,
            AskStage::DenyFeedback => {
                self.stage = AskStage::Menu { cursor: 2 };
                true
            }
            AskStage::PlanFeedback => {
                self.stage = AskStage::Menu { cursor: 2 };
                true
            }
            AskStage::Question(question) => {
                if question.editing_other {
                    question.editing_other = false;
                    return true;
                }
                false
            }
        }
    }

    /// One panel keypress. `menu_up_down`: whether ↑/↓ move the action
    /// cursor here (false inside the plan reader, where they scroll the
    /// plan). Esc and Ctrl+X never reach this — the chat handler owns
    /// them.
    pub fn handle_key(&mut self, ask: &Ask, key: &KeyEvent, menu_up_down: bool) -> AskKeyOutcome {
        if let AskStage::Question(form) = &mut self.stage {
            let AskKind::Question { questions } = &ask.kind else {
                return AskKeyOutcome::NotHandled;
            };
            return form.handle_key(questions, key);
        }
        if matches!(self.stage, AskStage::DenyFeedback) {
            if field_key(&mut self.deny_feedback, key) {
                return AskKeyOutcome::Handled;
            }
            if key.code == KeyCode::Enter {
                let text = self.deny_feedback.text();
                let feedback = (!text.trim().is_empty()).then_some(text);
                return AskKeyOutcome::Answer(AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback,
                }));
            }
            return AskKeyOutcome::NotHandled;
        }
        if matches!(self.stage, AskStage::PlanFeedback) {
            if field_key(&mut self.plan_feedback, key) {
                return AskKeyOutcome::Handled;
            }
            if key.code == KeyCode::Enter {
                let text = self.plan_feedback.text();
                if text.trim().is_empty() {
                    // C3: request-changes will not submit empty.
                    return AskKeyOutcome::Handled;
                }
                return AskKeyOutcome::Answer(AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: text,
                }));
            }
            return AskKeyOutcome::NotHandled;
        }

        let plan = is_plan(ask);
        let AskStage::Menu { cursor } = &mut self.stage else {
            return AskKeyOutcome::NotHandled;
        };
        match key.code {
            KeyCode::Char(digit @ '1'..='3') => {
                // Digits select, never submit (P8).
                *cursor = digit as usize - '1' as usize;
                AskKeyOutcome::Handled
            }
            KeyCode::Up if menu_up_down => {
                *cursor = cursor.saturating_sub(1);
                AskKeyOutcome::Handled
            }
            KeyCode::Down if menu_up_down => {
                *cursor = (*cursor + 1).min(2);
                AskKeyOutcome::Handled
            }
            KeyCode::Enter => match (*cursor, plan) {
                (0, false) => {
                    AskKeyOutcome::Answer(AskAnswer::Permission(PermissionAnswer::AllowOnce))
                }
                (1, false) => {
                    // The verified permission menu carries exactly one
                    // suggestion, and this row is it.
                    AskKeyOutcome::Answer(AskAnswer::Permission(PermissionAnswer::AllowScoped {
                        suggestion: 0,
                    }))
                }
                (2, false) => {
                    self.stage = AskStage::DenyFeedback;
                    AskKeyOutcome::Handled
                }
                (0, true) => AskKeyOutcome::Answer(AskAnswer::Plan(PlanAnswer::ApproveAuto)),
                (1, true) => AskKeyOutcome::Answer(AskAnswer::Plan(PlanAnswer::ApproveManual)),
                (_, true) => {
                    self.stage = AskStage::PlanFeedback;
                    AskKeyOutcome::Handled
                }
                _ => AskKeyOutcome::Handled,
            },
            KeyCode::Char('f') => {
                if has_readable(ask) {
                    AskKeyOutcome::OpenReader
                } else {
                    AskKeyOutcome::Handled
                }
            }
            _ => AskKeyOutcome::NotHandled,
        }
    }
}

// --- the question form (C4) --------------------------------------------------

/// Per-question draft state. `other` holds the free text persistently —
/// tabbing away and back never loses it (P8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct QuestionDraft {
    /// Selected option indexes (single-select: at most one).
    pub selected: Vec<usize>,
    /// The appended `Other…` option is chosen.
    pub other_chosen: bool,
    pub other: Composer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct QuestionUi {
    /// Current tab: a question index, or `questions.len()` for the submit
    /// tab (tabbed forms only).
    pub tab: usize,
    /// Option-row cursor on the current question, `options.len()` being
    /// the `Other…` row.
    pub cursor: usize,
    /// The `Other…` inline editor is open on the current tab.
    pub editing_other: bool,
    pub drafts: Vec<QuestionDraft>,
}

/// Tabs (and the mandatory submit step) exist whenever there is more than
/// one question or any multi-select (C4).
pub(crate) fn tabbed(questions: &[QuestionFact]) -> bool {
    questions.len() > 1 || questions.iter().any(|question| question.multi_select)
}

/// The Other field carries an answer: the ENCODER's emptiness rule
/// (`menu_text` trims before refusing), applied here as the one rule —
/// whitespace-only text must not count as answered and then refuse at
/// dispatch.
pub(crate) fn other_present(draft: &QuestionDraft) -> bool {
    !draft.other.text().trim().is_empty()
}

/// A question is answered when a selection or a chosen non-blank Other
/// exists.
pub(crate) fn answered(draft: &QuestionDraft) -> bool {
    !draft.selected.is_empty() || (draft.other_chosen && other_present(draft))
}

impl QuestionUi {
    pub fn new(questions: &[QuestionFact]) -> Self {
        Self {
            tab: 0,
            cursor: 0,
            editing_other: false,
            drafts: vec![QuestionDraft::default(); questions.len()],
        }
    }

    pub fn on_submit_tab(&self, questions: &[QuestionFact]) -> bool {
        tabbed(questions) && self.tab >= questions.len()
    }

    pub fn all_answered(&self) -> bool {
        self.drafts.iter().all(answered)
    }

    fn answer(&self) -> AskAnswer {
        AskAnswer::Question(QuestionResponse {
            answers: self
                .drafts
                .iter()
                .map(|draft| QuestionAnswer {
                    selected: draft.selected.clone(),
                    other: (draft.other_chosen && other_present(draft)).then(|| draft.other.text()),
                })
                .collect(),
        })
    }

    fn goto_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.editing_other = false;
        self.cursor = self
            .drafts
            .get(tab)
            .and_then(|draft| draft.selected.first().copied())
            .unwrap_or(0);
    }

    fn cycle_tab(&mut self, questions: &[QuestionFact], forward: bool) {
        if !tabbed(questions) {
            return;
        }
        let count = questions.len() + 1; // + submit
        let next = if forward {
            (self.tab + 1) % count
        } else {
            (self.tab + count - 1) % count
        };
        self.goto_tab(next);
    }

    /// Advance after answering the current question: the next tab (or the
    /// submit tab), or — on the untabbed single-question single-select
    /// form — submit right away (claude's own form submits on selection;
    /// our Enter is the deliberate confirm).
    fn advance_or_submit(&mut self, questions: &[QuestionFact]) -> AskKeyOutcome {
        if !tabbed(questions) {
            return AskKeyOutcome::Answer(self.answer());
        }
        let next = (self.tab + 1).min(questions.len());
        self.goto_tab(next);
        AskKeyOutcome::Handled
    }

    fn handle_key(&mut self, questions: &[QuestionFact], key: &KeyEvent) -> AskKeyOutcome {
        // The submit tab: a review list; Enter submits when complete.
        if self.on_submit_tab(questions) {
            return match key.code {
                KeyCode::Enter => {
                    if self.all_answered() {
                        AskKeyOutcome::Answer(self.answer())
                    } else {
                        // The review list states the unanswered items in
                        // error color; submitting would only be refused.
                        AskKeyOutcome::Handled
                    }
                }
                KeyCode::Tab | KeyCode::Right => {
                    self.cycle_tab(questions, true);
                    AskKeyOutcome::Handled
                }
                KeyCode::BackTab | KeyCode::Left => {
                    self.cycle_tab(questions, false);
                    AskKeyOutcome::Handled
                }
                _ => AskKeyOutcome::NotHandled,
            };
        }

        let Some(question) = questions.get(self.tab) else {
            return AskKeyOutcome::NotHandled;
        };
        let options = question.options.len();
        let other_row = options;
        let draft = &mut self.drafts[self.tab];

        // The open Other editor owns printables (P2).
        if self.editing_other {
            if field_key(&mut draft.other, key) {
                return AskKeyOutcome::Handled;
            }
            if key.code == KeyCode::Enter {
                self.editing_other = false;
                // The encoder's emptiness rule: whitespace-only text is
                // no answer — committing it chooses nothing.
                draft.other_chosen = other_present(draft);
                if draft.other_chosen && !question.multi_select {
                    // The Other IS the selection on single-select.
                    draft.selected.clear();
                    return self.advance_or_submit(questions);
                }
                return AskKeyOutcome::Handled;
            }
            return AskKeyOutcome::NotHandled;
        }

        match key.code {
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index > other_row {
                    return AskKeyOutcome::Handled;
                }
                self.cursor = index;
                if !question.multi_select && index < options {
                    // Digits select (never submit); Enter confirms.
                    draft.selected = vec![index];
                    draft.other_chosen = false;
                } else if index == other_row {
                    self.editing_other = true;
                }
                AskKeyOutcome::Handled
            }
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                AskKeyOutcome::Handled
            }
            KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(other_row);
                AskKeyOutcome::Handled
            }
            KeyCode::Char(' ') if question.multi_select => {
                if self.cursor == other_row {
                    if draft.other_chosen {
                        draft.other_chosen = false;
                    } else if !other_present(draft) {
                        // Nothing (or only whitespace) to check: open the
                        // editor instead of a box the encoder would
                        // refuse.
                        self.editing_other = true;
                    } else {
                        draft.other_chosen = true;
                    }
                } else if let Some(at) = draft.selected.iter().position(|s| *s == self.cursor) {
                    draft.selected.remove(at);
                } else {
                    draft.selected.push(self.cursor);
                    draft.selected.sort_unstable();
                }
                AskKeyOutcome::Handled
            }
            KeyCode::Enter => {
                if question.multi_select {
                    // Space toggles; Enter advances (one meaning per key).
                    return self.advance_or_submit(questions);
                }
                if self.cursor == other_row {
                    self.editing_other = true;
                    return AskKeyOutcome::Handled;
                }
                draft.selected = vec![self.cursor];
                draft.other_chosen = false;
                self.advance_or_submit(questions)
            }
            KeyCode::Tab | KeyCode::Right => {
                self.cycle_tab(questions, true);
                AskKeyOutcome::Handled
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.cycle_tab(questions, false);
                AskKeyOutcome::Handled
            }
            _ => AskKeyOutcome::NotHandled,
        }
    }
}

// --- text fields -------------------------------------------------------------

/// A panel's one-line text field: the shared readline set
/// ([`composer::readline_key`] — P6 applies to every text field) with the
/// panel's own frame around it. Returns `true` when the key was consumed;
/// Enter and Esc are left to the caller (submit / stage-back). Ctrl+C
/// never reaches here — the chrome-wide guard intercepts it in
/// `handle_chat_key` (clear-as-kill on a non-empty field, via the same
/// `active_field` derivation; arm-then-quit otherwise).
pub(crate) fn field_key(field: &mut Composer, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter | KeyCode::Esc => false,
        KeyCode::Char('x') if ctrl => false, // interrupt is handled above
        // The feed stays scrollable behind an open text stage.
        KeyCode::PageUp | KeyCode::PageDown => false,
        // The shared readline set (printables type here — `q`, `f`,
        // digits, `?` — P2). Whatever it leaves — no newline (Ctrl+J), no
        // history, no row motion on a one-line field — is a no-op, still
        // consumed (a chord must not leak into menu navigation).
        _ => {
            composer::readline_key(field, key);
            true
        }
    }
}
