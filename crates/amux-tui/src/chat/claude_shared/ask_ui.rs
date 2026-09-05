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

use amux_ui::claude::QuestionFact;
use amux_ui::claude::answer::{
    AskAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};
use amux_ui::claude_sdk::{
    DialogAnswer, ElicitationAnswer, ElicitationField, ElicitationFieldKind, ElicitationForm,
    dialog_choices,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::chat::claude_shared::{SharedAsk, SharedAskKind};
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
    /// The elicitation form: a server's own fields over its schema.
    Elicitation(ElicitationUi),
}

/// A complete answer a panel collected. The three kinds both transports
/// carry travel as the provider-shaped [`AskAnswer`]; the two only a
/// session can raise travel beside it, because the terminal transport has
/// no keystrokes that could express them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PanelAnswer {
    Claude(AskAnswer),
    Elicitation(ElicitationAnswer),
    Dialog(DialogAnswer),
}

impl PanelAnswer {
    /// The answer as the shared Claude vocabulary, when it is one of the
    /// three kinds both transports carry.
    pub(crate) fn claude(self) -> Option<AskAnswer> {
        match self {
            PanelAnswer::Claude(answer) => Some(answer),
            _ => None,
        }
    }
}

/// What a panel keypress asked for.
#[derive(Debug, PartialEq)]
pub(crate) enum AskKeyOutcome {
    /// Consumed; view state may have changed.
    Handled,
    /// Dispatch this typed answer for the ask.
    Answer(PanelAnswer),
    /// Open the reader on the ask's document (`f`).
    OpenReader,
    /// Not a panel key — the caller may route it elsewhere (feed
    /// scrolling).
    NotHandled,
}

impl AskUi {
    pub fn for_ask(ask: &SharedAsk<'_>) -> Self {
        let stage = match &ask.kind {
            SharedAskKind::Question { questions } => AskStage::Question(QuestionUi::new(questions)),
            SharedAskKind::Elicitation { form, .. } => {
                AskStage::Elicitation(ElicitationUi::new(form))
            }
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
            AskStage::Elicitation(form) => form.focused_text(),
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
            AskStage::Elicitation(form) => form.step_back(),
        }
    }

    /// One panel keypress. `menu_up_down`: whether ↑/↓ move the action
    /// cursor here (false inside the plan reader, where they scroll the
    /// plan). Esc and Ctrl+X never reach this — the chat handler owns
    /// them.
    pub fn handle_key(
        &mut self,
        ask: &SharedAsk<'_>,
        key: &KeyEvent,
        menu_up_down: bool,
    ) -> AskKeyOutcome {
        if let AskStage::Question(form) = &mut self.stage {
            let SharedAskKind::Question { questions } = &ask.kind else {
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
                return AskKeyOutcome::Answer(PanelAnswer::Claude(AskAnswer::Permission(
                    PermissionAnswer::Deny { feedback },
                )));
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
                return AskKeyOutcome::Answer(PanelAnswer::Claude(AskAnswer::Plan(
                    PlanAnswer::RequestChanges { feedback: text },
                )));
            }
            return AskKeyOutcome::NotHandled;
        }

        if let AskStage::Elicitation(form) = &mut self.stage {
            let SharedAskKind::Elicitation { form: schema, .. } = &ask.kind else {
                return AskKeyOutcome::NotHandled;
            };
            return form.handle_key(schema, key);
        }

        let rows = menu_rows(ask);
        let AskStage::Menu { cursor } = &mut self.stage else {
            return AskKeyOutcome::NotHandled;
        };
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Char(digit @ '1'..='9') => {
                // Digits select, never submit (P8).
                let index = digit as usize - '1' as usize;
                if index <= last {
                    *cursor = index;
                }
                AskKeyOutcome::Handled
            }
            KeyCode::Up if menu_up_down => {
                *cursor = cursor.saturating_sub(1);
                AskKeyOutcome::Handled
            }
            KeyCode::Down if menu_up_down => {
                *cursor = (*cursor + 1).min(last);
                AskKeyOutcome::Handled
            }
            KeyCode::Enter => match rows.get(*cursor) {
                Some(MenuRow::AllowOnce) => AskKeyOutcome::Answer(PanelAnswer::Claude(
                    AskAnswer::Permission(PermissionAnswer::AllowOnce),
                )),
                // The verified permission menu carries exactly one
                // suggestion, and this row is it.
                Some(MenuRow::AllowScoped) => AskKeyOutcome::Answer(PanelAnswer::Claude(
                    AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
                )),
                Some(MenuRow::Deny) => {
                    self.stage = AskStage::DenyFeedback;
                    AskKeyOutcome::Handled
                }
                Some(MenuRow::ApproveAuto) => AskKeyOutcome::Answer(PanelAnswer::Claude(
                    AskAnswer::Plan(PlanAnswer::ApproveAuto),
                )),
                Some(MenuRow::ApproveManual) => AskKeyOutcome::Answer(PanelAnswer::Claude(
                    AskAnswer::Plan(PlanAnswer::ApproveManual),
                )),
                Some(MenuRow::RequestChanges) => {
                    self.stage = AskStage::PlanFeedback;
                    AskKeyOutcome::Handled
                }
                Some(MenuRow::DialogOption { index }) => {
                    AskKeyOutcome::Answer(PanelAnswer::Dialog(DialogAnswer::Choose {
                        option: *index,
                    }))
                }
                Some(MenuRow::DialogCancel) => {
                    AskKeyOutcome::Answer(PanelAnswer::Dialog(DialogAnswer::Cancel))
                }
                None => AskKeyOutcome::Handled,
            },
            KeyCode::Char('f') => {
                if ask.has_readable() {
                    AskKeyOutcome::OpenReader
                } else {
                    AskKeyOutcome::Handled
                }
            }
            _ => AskKeyOutcome::NotHandled,
        }
    }
}

// --- the numbered menu -------------------------------------------------------

/// One row of a menu-stage ask's numbered action list. The panel labels
/// exactly these rows in this order and Enter answers exactly the row
/// under the cursor, so the list a person reads and the answer they get
/// cannot come apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuRow {
    AllowOnce,
    AllowScoped,
    Deny,
    ApproveAuto,
    ApproveManual,
    RequestChanges,
    /// One of the dialog payload's own choices, by index into it.
    DialogOption {
        index: usize,
    },
    DialogCancel,
}

/// The action rows a menu-stage ask offers.
pub(crate) fn menu_rows(ask: &SharedAsk<'_>) -> Vec<MenuRow> {
    if ask.is_plan() {
        return vec![
            MenuRow::ApproveAuto,
            MenuRow::ApproveManual,
            MenuRow::RequestChanges,
        ];
    }
    match &ask.kind {
        SharedAskKind::Dialog { payload, .. } => match dialog_choices(payload) {
            // A payload this build cannot answer offers the one honest
            // action there is; nothing about it looks like agreement.
            None => vec![MenuRow::DialogCancel],
            Some(choices) => (0..choices.options.len())
                .map(|index| MenuRow::DialogOption { index })
                .chain(std::iter::once(MenuRow::DialogCancel))
                .collect(),
        },
        _ => vec![MenuRow::AllowOnce, MenuRow::AllowScoped, MenuRow::Deny],
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
            return AskKeyOutcome::Answer(PanelAnswer::Claude(self.answer()));
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
                        AskKeyOutcome::Answer(PanelAnswer::Claude(self.answer()))
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

// --- the elicitation form ----------------------------------------------------

/// What a person has entered against one schema field. Text, number and
/// integer fields share the one-line editor; booleans and enums are
/// chosen, not typed, so nothing about them can fail to parse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum FieldDraft {
    Text(Composer),
    Boolean(bool),
    /// An index into the field's own choices.
    Choice(usize),
}

/// Elicitation form state: one focus ring over the fields and the action
/// list beneath them. A field is edited where it sits — there is no
/// second editing mode to enter and leave — so the ring position is the
/// whole story of where a keystroke goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ElicitationUi {
    /// A field index, or `drafts.len() + n` for action row `n`.
    pub cursor: usize,
    pub drafts: Vec<FieldDraft>,
}

/// The actions an elicitation offers. A form nobody can fill offers only
/// the two answers that need no fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormAction {
    Send,
    Decline,
    Cancel,
}

pub(crate) fn form_actions(form: &ElicitationForm) -> Vec<FormAction> {
    match form {
        ElicitationForm::Fields(_) => {
            vec![FormAction::Send, FormAction::Decline, FormAction::Cancel]
        }
        ElicitationForm::Unsupported { .. } => vec![FormAction::Decline, FormAction::Cancel],
    }
}

/// The fields a form has, empty for one this build cannot express.
pub(crate) fn form_fields(form: &ElicitationForm) -> &[ElicitationField] {
    match form {
        ElicitationForm::Fields(fields) => fields,
        ElicitationForm::Unsupported { .. } => &[],
    }
}

impl ElicitationUi {
    pub fn new(form: &ElicitationForm) -> Self {
        Self {
            cursor: 0,
            drafts: form_fields(form)
                .iter()
                .map(FieldDraft::for_field)
                .collect(),
        }
    }

    /// Where the ring sits, given the form it is over.
    pub fn on_actions(&self) -> bool {
        self.cursor >= self.drafts.len()
    }

    /// The selected action row, when the ring is on the action list.
    pub fn action(&self, form: &ElicitationForm) -> usize {
        let actions = form_actions(form).len();
        self.cursor
            .saturating_sub(self.drafts.len())
            .min(actions.saturating_sub(1))
    }

    /// The focused one-line editor, when the ring sits on a text field —
    /// the paste target and what the guarded Ctrl+C clears.
    pub fn focused_text(&mut self) -> Option<&mut Composer> {
        match self.drafts.get_mut(self.cursor) {
            Some(FieldDraft::Text(field)) => Some(field),
            _ => None,
        }
    }

    /// Esc: back from the action list to the last field, keeping every
    /// answer typed so far; `false` at the floor.
    fn step_back(&mut self) -> bool {
        if self.on_actions() && !self.drafts.is_empty() {
            self.cursor = self.drafts.len() - 1;
            return true;
        }
        false
    }

    fn ring_len(&self, form: &ElicitationForm) -> usize {
        self.drafts.len() + form_actions(form).len()
    }

    fn step(&mut self, form: &ElicitationForm, forward: bool) {
        let len = self.ring_len(form);
        if len == 0 {
            return;
        }
        self.cursor = if forward {
            (self.cursor + 1) % len
        } else {
            (self.cursor + len - 1) % len
        };
    }

    /// The content this form would send, or why it cannot be sent yet.
    /// The panel states the reason and Send refuses on the same rule, so
    /// nothing is offered that dispatch would reject.
    pub fn content(&self, form: &ElicitationForm) -> Result<Value, String> {
        let fields = form_fields(form);
        let mut content = Map::new();
        for (field, draft) in fields.iter().zip(&self.drafts) {
            let name = field.title.as_deref().unwrap_or(&field.name);
            let value = match (&field.kind, draft) {
                (ElicitationFieldKind::Boolean, FieldDraft::Boolean(value)) => {
                    Some(Value::Bool(*value))
                }
                (ElicitationFieldKind::Enum(choices), FieldDraft::Choice(index)) => {
                    choices.get(*index).cloned()
                }
                (kind, FieldDraft::Text(entry)) => {
                    let text = entry.text();
                    let text = text.trim();
                    if text.is_empty() {
                        None
                    } else {
                        match kind {
                            ElicitationFieldKind::Integer => Some(Value::from(
                                text.parse::<i64>()
                                    .map_err(|_| format!("{name} must be a whole number"))?,
                            )),
                            ElicitationFieldKind::Number => Some(Value::from(
                                text.parse::<f64>()
                                    .map_err(|_| format!("{name} must be a number"))?,
                            )),
                            _ => Some(Value::String(text.to_string())),
                        }
                    }
                }
                _ => None,
            };
            match value {
                Some(value) => {
                    content.insert(field.name.clone(), value);
                }
                None if field.required => return Err(format!("{name} is required")),
                None => {}
            }
        }
        Ok(Value::Object(content))
    }

    fn handle_key(&mut self, form: &ElicitationForm, key: &KeyEvent) -> AskKeyOutcome {
        let actions = form_actions(form);
        if self.on_actions() {
            let action = self.action(form);
            return match key.code {
                KeyCode::Char(digit @ '1'..='9') => {
                    let index = digit as usize - '1' as usize;
                    if index < actions.len() {
                        self.cursor = self.drafts.len() + index;
                    }
                    AskKeyOutcome::Handled
                }
                KeyCode::Up | KeyCode::BackTab => {
                    self.step(form, false);
                    AskKeyOutcome::Handled
                }
                KeyCode::Down | KeyCode::Tab => {
                    self.step(form, true);
                    AskKeyOutcome::Handled
                }
                KeyCode::Enter => match actions.get(action) {
                    Some(FormAction::Send) => match self.content(form) {
                        // The panel names what is still missing; sending
                        // would only be refused.
                        Err(_) => AskKeyOutcome::Handled,
                        Ok(content) => AskKeyOutcome::Answer(PanelAnswer::Elicitation(
                            ElicitationAnswer::Accept { content },
                        )),
                    },
                    Some(FormAction::Decline) => {
                        AskKeyOutcome::Answer(PanelAnswer::Elicitation(ElicitationAnswer::Decline))
                    }
                    Some(FormAction::Cancel) => {
                        AskKeyOutcome::Answer(PanelAnswer::Elicitation(ElicitationAnswer::Cancel))
                    }
                    None => AskKeyOutcome::Handled,
                },
                _ => AskKeyOutcome::NotHandled,
            };
        }

        let fields = form_fields(form);
        let Some(field) = fields.get(self.cursor) else {
            return AskKeyOutcome::NotHandled;
        };
        // Ring motion is read before the field sees anything: a one-line
        // editor consumes arrows as no-ops, and a swallowed Tab would
        // strand the person on the first field.
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
                self.step(form, true);
                return AskKeyOutcome::Handled;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.step(form, false);
                return AskKeyOutcome::Handled;
            }
            _ => {}
        }
        let choices = match &field.kind {
            ElicitationFieldKind::Enum(choices) => choices.len(),
            _ => 0,
        };
        match &mut self.drafts[self.cursor] {
            FieldDraft::Boolean(value) => match key.code {
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                    *value = !*value;
                    AskKeyOutcome::Handled
                }
                _ => AskKeyOutcome::NotHandled,
            },
            FieldDraft::Choice(index) => match key.code {
                KeyCode::Right | KeyCode::Char(' ') if choices > 0 => {
                    *index = (*index + 1) % choices;
                    AskKeyOutcome::Handled
                }
                KeyCode::Left if choices > 0 => {
                    *index = (*index + choices - 1) % choices;
                    AskKeyOutcome::Handled
                }
                _ => AskKeyOutcome::NotHandled,
            },
            FieldDraft::Text(entry) => {
                if field_key(entry, key) {
                    AskKeyOutcome::Handled
                } else {
                    AskKeyOutcome::NotHandled
                }
            }
        }
    }
}

impl FieldDraft {
    /// A fresh draft for one schema field, prefilled from its `default`.
    fn for_field(field: &ElicitationField) -> Self {
        match (&field.kind, &field.default) {
            (ElicitationFieldKind::Boolean, default) => {
                FieldDraft::Boolean(default.as_ref().and_then(Value::as_bool).unwrap_or(false))
            }
            (ElicitationFieldKind::Enum(choices), default) => FieldDraft::Choice(
                default
                    .as_ref()
                    .and_then(|value| choices.iter().position(|choice| choice == value))
                    .unwrap_or(0),
            ),
            (_, default) => {
                let mut entry = Composer::default();
                if let Some(text) = default.as_ref().map(render_default) {
                    entry.restore(&text);
                }
                FieldDraft::Text(entry)
            }
        }
    }

    /// What the row shows: the typed text, the checkbox, the choice.
    pub(crate) fn display(&self, field: &ElicitationField, focused: bool) -> String {
        match self {
            FieldDraft::Text(entry) => {
                if focused {
                    entry.display_with_cursor()
                } else {
                    entry.text()
                }
            }
            FieldDraft::Boolean(value) => {
                if *value {
                    "[x] yes".into()
                } else {
                    "[ ] no".into()
                }
            }
            FieldDraft::Choice(index) => match &field.kind {
                ElicitationFieldKind::Enum(choices) => {
                    choices.get(*index).map(render_default).unwrap_or_default()
                }
                _ => String::new(),
            },
        }
    }
}

/// A schema value as a person reads it: text without its quotes,
/// everything else as JSON writes it.
pub(crate) fn render_default(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
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
