//! The ask card, docked where the composer was. One anatomy for every
//! kind: what it wants and "1 of 3", the subject verbatim, then choices
//! stated as outcomes, the likely one first. Stop is always in the menu:
//! it is the interrupt, which ends the turn and leaves the agent live.
//!
//! [`AskUi`] is this client's state for the head ask (the highlighted
//! choice, a note, question picks, form fields); it resets whenever the
//! head ask changes, so nothing is carried from one ask to another.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use ui_view::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, Pick, QuestionView, Scope,
    answer_input, question_answer, with_form_content,
};

use crate::editor::Editor;
use crate::text::{self, push, push_right};
use crate::theme::Theme;

/// Diff or argument lines shown inline before "f" opens the whole thing.
const BODY_LINES: usize = 8;

/// What a key on the card asks the chat to do.
#[derive(Clone, Debug, PartialEq)]
pub enum AskAction {
    None,
    /// Send this answer.
    Answer(Box<wire::Input>),
    /// Stop: the Interrupt input.
    Interrupt,
    /// The answer was not confirmed: send it again, or forget it.
    Resend,
    Discard,
    /// Open the whole diff, plan or arguments in the reader.
    Read {
        title: String,
        text: String,
    },
    /// Hand the terminal to the agent's own interface.
    Attach,
    /// A plan decision made of other inputs, sent in order; the card is
    /// done with once they go.
    Compose {
        key: String,
        inputs: Vec<wire::Input>,
    },
}

/// One entry of the card's menu.
#[derive(Clone, Debug, PartialEq)]
enum Entry {
    Choice(usize),
    Stop,
    Attach,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Stage {
    #[default]
    Menu,
    /// Writing the note a choice takes.
    Note(usize),
    /// Typing a "Something else…" answer for the current question.
    Other,
    /// Every answer on one screen before sending.
    Review,
    /// Editing a form field.
    Field,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct QuestionPick {
    selected: Vec<u32>,
    other: Option<String>,
}

impl QuestionPick {
    fn answered(&self) -> bool {
        !self.selected.is_empty() || self.other.as_ref().is_some_and(|o| !o.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
enum FieldKind {
    Text,
    Number { integer: bool },
    Toggle,
    Choice(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
struct Field {
    name: String,
    title: String,
    required: bool,
    kind: FieldKind,
    /// Text, number and choice values; "true"/"false" for a toggle.
    value: String,
}

/// This client's state for the head ask.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AskUi {
    key: String,
    stage: Stage,
    selected: usize,
    note: Editor,
    step: usize,
    picks: Vec<QuestionPick>,
    other: Editor,
    fields: Vec<Field>,
    /// Whether the person has the ask's keys, or has handed them to the
    /// composer for a moment to keep drafting.
    pub drafting: bool,
    /// The boxed ask's command or arguments shown whole.
    show_all: bool,
    /// The choice last sent, for the line the box shows until the agent
    /// confirms it.
    sent: Option<usize>,
    /// The boxed ask's deny note is open for typing.
    noting: bool,
    /// A question ask's "Reply instead": the composer has the keys and what
    /// is sent goes as the reply.
    replying: bool,
}

fn scope_words(scope: &Scope) -> String {
    match scope {
        Scope::Session => "for this session".into(),
        Scope::Project => "in this project".into(),
        Scope::ProjectShared => "in this project, for everyone".into(),
        Scope::User => "in every project".into(),
        Scope::Other(other) => other.clone(),
    }
}

/// A choice in words: what happens, never rule syntax.
pub fn choice_label(choice: &Choice) -> String {
    let label = match &choice.outcome {
        ChoiceOutcome::AllowOnce => "Allow once".to_owned(),
        ChoiceOutcome::AllowAlways {
            subjects,
            directories,
            mode,
            scope,
            label,
        } => {
            if !subjects.is_empty() {
                format!(
                    "Always allow {} {}",
                    subjects.join(", "),
                    scope_words(scope)
                )
            } else if !directories.is_empty() {
                format!(
                    "Allow access to {} {}",
                    directories.join(", "),
                    scope_words(scope)
                )
            } else if !mode.is_empty() {
                format!("Switch to {mode} mode")
            } else if !label.is_empty() {
                label.clone()
            } else {
                format!("Always allow {}", scope_words(scope))
            }
        }
        ChoiceOutcome::AllowForSession => "Allow for this session".to_owned(),
        ChoiceOutcome::AllowSimilar { prefix } => {
            format!("Allow commands starting with {}", prefix.join(" "))
        }
        ChoiceOutcome::AllowNetwork { hosts } if hosts.is_empty() => {
            "Allow network access".to_owned()
        }
        ChoiceOutcome::AllowNetwork { hosts } => {
            format!("Allow network access to {}", hosts.join(", "))
        }
        ChoiceOutcome::Deny { stops: true } => "Deny and stop".to_owned(),
        ChoiceOutcome::Deny { stops: false } => "Deny".to_owned(),
        ChoiceOutcome::DenyAndStop => "Deny and stop".to_owned(),
        ChoiceOutcome::ApprovePlan {
            auto_accept_edits: true,
        } => "Approve and accept edits without asking".to_owned(),
        ChoiceOutcome::ApprovePlan {
            auto_accept_edits: false,
        } => "Approve".to_owned(),
        ChoiceOutcome::SendBack => "Send back".to_owned(),
        ChoiceOutcome::Submit => "Submit".to_owned(),
        ChoiceOutcome::Decline => "Decline".to_owned(),
        ChoiceOutcome::OpenLink => "I opened it".to_owned(),
        ChoiceOutcome::GrantForTurn => "Grant for this turn".to_owned(),
        ChoiceOutcome::GrantForSession => "Grant for this session".to_owned(),
    };
    if choice.takes_note {
        format!("{label}…")
    } else {
        label
    }
}

/// What the card says it wants.
pub fn headline(card: &AskCard) -> String {
    match &card.body {
        AskBody::Command { .. } => "Wants to run a command".into(),
        AskBody::Edit { files, .. } if *files > 1 => format!("Wants to edit {files} files"),
        AskBody::Edit { .. } => "Wants to edit a file".into(),
        AskBody::Tool { server, tool, .. } if server.is_empty() => format!("Wants to use {tool}"),
        AskBody::Tool { server, tool, .. } => format!("Wants to use {server} · {tool}"),
        AskBody::Question(questions) if questions.len() > 1 => {
            format!("{} questions", questions.len())
        }
        AskBody::Question(_) => "Question".into(),
        AskBody::Plan { .. } => "Plan".into(),
        AskBody::Form { server, .. } => format!("{server} needs details"),
        AskBody::Link { server, .. } => format!("{server} wants you to open a link"),
        AskBody::Access { .. } => "Wants more access".into(),
        AskBody::Unanswerable { .. } => "Can't answer this here".into(),
    }
}

fn form_fields(schema_json: &str) -> Vec<Field> {
    let schema: Value = serde_json::from_str(schema_json).unwrap_or(Value::Null);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| {
            let kind = if let Some(options) = property.get("enum").and_then(Value::as_array) {
                FieldKind::Choice(
                    options
                        .iter()
                        .map(|option| match option {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect(),
                )
            } else {
                match property.get("type").and_then(Value::as_str) {
                    Some("boolean") => FieldKind::Toggle,
                    Some("number") => FieldKind::Number { integer: false },
                    Some("integer") => FieldKind::Number { integer: true },
                    _ => FieldKind::Text,
                }
            };
            let value = match (property.get("default"), &kind) {
                (Some(Value::String(s)), _) => s.clone(),
                (Some(Value::Bool(b)), _) => b.to_string(),
                (Some(Value::Number(n)), _) => n.to_string(),
                (_, FieldKind::Toggle) => "false".into(),
                (_, FieldKind::Choice(options)) => options.first().cloned().unwrap_or_default(),
                _ => String::new(),
            };
            Field {
                title: property
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_owned(),
                required: required.contains(&name.as_str()),
                name: name.clone(),
                kind,
                value,
            }
        })
        .collect()
}

impl Field {
    fn json(&self) -> Option<Value> {
        match &self.kind {
            FieldKind::Toggle => Some(Value::Bool(self.value == "true")),
            _ if self.value.is_empty() => None,
            FieldKind::Number { integer: true } => self.value.parse::<i64>().ok().map(Value::from),
            FieldKind::Number { integer: false } => self.value.parse::<f64>().ok().map(Value::from),
            FieldKind::Text | FieldKind::Choice(_) => Some(Value::String(self.value.clone())),
        }
    }

    fn valid(&self) -> bool {
        match (&self.kind, self.value.is_empty()) {
            (FieldKind::Toggle, _) => true,
            (_, true) => !self.required,
            (FieldKind::Number { .. }, false) => self.json().is_some(),
            _ => true,
        }
    }
}

impl AskUi {
    /// Keeps this state if it belongs to `card`'s ask, else starts fresh.
    pub fn sync(&mut self, card: &AskCard) {
        if self.key == card.key {
            return;
        }
        *self = AskUi {
            key: card.key.clone(),
            ..AskUi::default()
        };
        match &card.body {
            AskBody::Question(questions) => {
                self.picks = vec![QuestionPick::default(); questions.len()];
                self.other.secret = questions.first().is_some_and(|q| q.secret);
            }
            AskBody::Form { schema_json, .. } => self.fields = form_fields(schema_json),
            _ => {}
        }
    }

    fn entries(&self, card: &AskCard, attach: bool) -> Vec<Entry> {
        let mut entries: Vec<Entry> = (0..card.choices.len()).map(Entry::Choice).collect();
        entries.push(Entry::Stop);
        if attach && matches!(card.body, AskBody::Unanswerable { .. }) {
            entries.push(Entry::Attach);
        }
        entries
    }

    /// Whether the card holds a text field with the cursor in it, so the
    /// frame shows a cursor and Ctrl+C clears that field.
    pub fn editing(&self) -> bool {
        matches!(self.stage, Stage::Note(_) | Stage::Other | Stage::Field)
    }

    /// Whether this state's field has the keys on `card`: the field is
    /// open, and the card is this ask's and still takes answers. A field
    /// left open when its answer went out does not outlive the ask.
    pub fn editing_on(&self, card: &AskCard) -> bool {
        self.key == card.key
            && matches!(card.state, CardState::Open | CardState::Rejected(_))
            && self.editing()
    }

    /// The editor of the field being edited.
    fn field(&mut self) -> Option<&mut Editor> {
        match self.stage {
            Stage::Note(_) => Some(&mut self.note),
            Stage::Other | Stage::Field => Some(&mut self.other),
            Stage::Menu | Stage::Review => None,
        }
    }

    /// Whether the field with the keys on `card` holds something, for
    /// Ctrl+C.
    pub fn field_text(&self, card: &AskCard) -> bool {
        self.editing_on(card)
            && match self.stage {
                Stage::Note(_) => !self.note.is_empty(),
                _ => !self.other.is_empty(),
            }
    }

    /// Clears the field being edited, as a kill.
    pub fn kill_field(&mut self) -> bool {
        self.field().is_some_and(Editor::kill_all)
    }

    /// Whether Esc on `card` is the card's: it leaves a field, the review,
    /// or a later question, rather than moving the chat.
    pub fn takes_escape(&self, card: &AskCard) -> bool {
        self.editing()
            || matches!(card.body, AskBody::Question(_))
                && (self.stage == Stage::Review || self.stage == Stage::Menu && self.step > 0)
    }

    /// A bracketed paste on `card`: into the open field, or starting a
    /// "Something else…" answer where typing would. Anywhere else on the
    /// card there is nothing to paste into.
    pub fn paste(&mut self, card: &AskCard, text: &str) {
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return;
        }
        if self.on_other(card)
            && let AskBody::Question(questions) = &card.body
        {
            self.open_other(&questions[self.step]);
        }
        if let Some(field) = self.field() {
            field.insert_str(text);
        }
    }

    /// Whether the menu's highlight is on the current question's
    /// "Something else…", where typing starts the answer.
    fn on_other(&self, card: &AskCard) -> bool {
        let AskBody::Question(questions) = &card.body else {
            return false;
        };
        self.stage == Stage::Menu
            && questions.get(self.step).is_some_and(|question| {
                question.allow_other && self.selected == question.options.len()
            })
    }

    fn open_other(&mut self, question: &QuestionView) {
        self.other = Editor::default();
        self.other.secret = question.secret;
        self.stage = Stage::Other;
    }

    fn reader(card: &AskCard) -> Option<AskAction> {
        let (title, text) = match &card.body {
            AskBody::Edit { path, diff, .. } => (path.clone(), diff.clone()),
            AskBody::Plan { plan } => ("plan".to_owned(), plan.clone()),
            AskBody::Tool {
                server,
                tool,
                arguments,
            } => (format!("{server} · {tool}"), arguments.clone()),
            AskBody::Command { command, .. } => ("command".to_owned(), command.clone()),
            AskBody::Question(questions) => (
                "preview".to_owned(),
                questions
                    .iter()
                    .flat_map(|q| q.options.iter())
                    .map(|o| o.preview.as_str())
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            _ => return None,
        };
        (!text.is_empty()).then_some(AskAction::Read { title, text })
    }

    /// One key on the card. Ctrl+X is the chat's and never reaches here.
    pub fn key(&mut self, card: &AskCard, key: KeyEvent, attach: bool) -> AskAction {
        match card.state {
            CardState::Sending | CardState::Dismissed => return AskAction::None,
            CardState::NotConfirmed => {
                return match key.code {
                    KeyCode::Char('r') => AskAction::Resend,
                    KeyCode::Char('d') => AskAction::Discard,
                    _ => AskAction::None,
                };
            }
            CardState::Open | CardState::Rejected(_) => {}
        }
        if key.code == KeyCode::Char('f') && !self.editing() && !self.on_other(card) {
            return Self::reader(card).unwrap_or(AskAction::None);
        }
        if let AskBody::Question(questions) = &card.body {
            return self.question_key(card, questions, key);
        }
        if matches!(card.body, AskBody::Form { .. })
            && !self.fields.is_empty()
            && let Some(action) = self.form_key(card, key)
        {
            return action;
        }
        match self.stage.clone() {
            Stage::Note(index) => match key.code {
                KeyCode::Esc => {
                    self.stage = Stage::Menu;
                    AskAction::None
                }
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    let choice = &card.choices[index];
                    // Sending a plan back needs the reason; a denial's note
                    // is optional.
                    if choice.outcome == ChoiceOutcome::SendBack
                        && self.note.text().trim().is_empty()
                    {
                        return AskAction::None;
                    }
                    match answer_input(card, &choice.answer, self.note.text().trim()) {
                        Some(input) => AskAction::Answer(Box::new(input)),
                        None => AskAction::None,
                    }
                }
                _ => {
                    self.note.key(key);
                    AskAction::None
                }
            },
            _ => self.menu_key(card, key, attach),
        }
    }

    fn menu_key(&mut self, card: &AskCard, key: KeyEvent, attach: bool) -> AskAction {
        let entries = self.entries(card, attach);
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(entries.len() - 1),
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < entries.len() {
                    self.selected = index;
                }
            }
            KeyCode::Enter => {
                return match entries.get(self.selected) {
                    Some(Entry::Choice(index)) => self.pick(card, *index),
                    Some(Entry::Stop) => AskAction::Interrupt,
                    Some(Entry::Attach) => AskAction::Attach,
                    None => AskAction::None,
                };
            }
            _ => {}
        }
        AskAction::None
    }

    fn pick(&mut self, card: &AskCard, index: usize) -> AskAction {
        let choice = &card.choices[index];
        if choice.takes_note {
            self.stage = Stage::Note(index);
            return AskAction::None;
        }
        let answer = match (&card.body, &choice.outcome) {
            (AskBody::Form { .. }, ChoiceOutcome::Submit) => {
                if !self.fields.iter().all(Field::valid) {
                    return AskAction::None;
                }
                with_content(&choice.answer, &self.fields)
            }
            _ => choice.answer.clone(),
        };
        match answer_input(card, &answer, "") {
            Some(input) => AskAction::Answer(Box::new(input)),
            None => AskAction::None,
        }
    }

    /// Form keys: ↑↓ move through the fields then the menu; Enter edits a
    /// field; Space flips a toggle or steps a choice.
    fn form_key(&mut self, card: &AskCard, key: KeyEvent) -> Option<AskAction> {
        let fields = self.fields.len();
        if self.stage == Stage::Field {
            let field = &mut self.fields[self.selected];
            match key.code {
                KeyCode::Enter | KeyCode::Esc => {
                    field.value = self.other.text().to_owned();
                    self.other = Editor::default();
                    self.stage = Stage::Menu;
                }
                _ => {
                    self.other.key(key);
                }
            }
            return Some(AskAction::None);
        }
        let entries = self.entries(card, false).len();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(fields + entries - 1),
            KeyCode::Char(' ') | KeyCode::Enter if self.selected < fields => {
                let field = &mut self.fields[self.selected];
                match &field.kind {
                    FieldKind::Toggle => {
                        field.value = if field.value == "true" {
                            "false"
                        } else {
                            "true"
                        }
                        .into();
                    }
                    FieldKind::Choice(options) => {
                        let at = options.iter().position(|o| *o == field.value).unwrap_or(0);
                        field.value = options[(at + 1) % options.len()].clone();
                    }
                    FieldKind::Text | FieldKind::Number { .. } => {
                        self.other = Editor::default();
                        self.other.set(&field.value, vec![]);
                        self.stage = Stage::Field;
                    }
                }
            }
            KeyCode::Enter => {
                let entry = self.selected - fields;
                return Some(match self.entries(card, false).get(entry) {
                    Some(Entry::Choice(index)) => self.pick(card, *index),
                    Some(Entry::Stop) => AskAction::Interrupt,
                    _ => AskAction::None,
                });
            }
            _ => {}
        }
        Some(AskAction::None)
    }

    /// Question keys. One pick-one question without previews answers on
    /// Enter; every other shape collects picks and sends from the review.
    fn question_key(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        key: KeyEvent,
    ) -> AskAction {
        let count = questions.len();
        match self.stage {
            Stage::Other => {
                match key.code {
                    KeyCode::Esc => self.stage = Stage::Menu,
                    KeyCode::Enter => {
                        let typed = self.other.text().trim().to_owned();
                        if typed.is_empty() {
                            return AskAction::None;
                        }
                        self.picks[self.step] = QuestionPick {
                            selected: vec![],
                            other: Some(typed),
                        };
                        self.stage = Stage::Menu;
                        return self.advance(card, questions);
                    }
                    _ => {
                        self.other.key(key);
                    }
                }
                return AskAction::None;
            }
            Stage::Note(_) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => self.stage = Stage::Review,
                    _ => {
                        self.note.key(key);
                    }
                }
                return AskAction::None;
            }
            Stage::Review => {
                match key.code {
                    KeyCode::Esc | KeyCode::Left | KeyCode::BackTab => {
                        self.stage = Stage::Menu;
                        self.step = count - 1;
                    }
                    KeyCode::Char('n') if card.question_note => self.stage = Stage::Note(0),
                    KeyCode::Char(c @ '1'..='9') => {
                        let index = c as usize - '1' as usize;
                        if index < count {
                            self.step = index;
                            self.stage = Stage::Menu;
                        }
                    }
                    KeyCode::Enter => return self.send_questions(card),
                    _ => {}
                }
                return AskAction::None;
            }
            Stage::Menu | Stage::Field => {}
        }
        let question = &questions[self.step];
        let options = question.options.len() + usize::from(question.allow_other);
        // The menu is the options, then Stop.
        let rows = options + 1;
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(rows - 1),
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < rows {
                    self.selected = index;
                }
            }
            KeyCode::Tab | KeyCode::Right if count > 1 => {
                self.goto(questions, (self.step + 1).min(count));
            }
            KeyCode::BackTab | KeyCode::Left if count > 1 => {
                self.goto(questions, self.step.saturating_sub(1));
            }
            KeyCode::Esc if self.step > 0 => self.goto(questions, self.step - 1),
            KeyCode::Char(' ')
                if question.multi_select && self.selected < question.options.len() =>
            {
                let option = self.selected as u32;
                let pick = &mut self.picks[self.step];
                pick.other = None;
                if let Some(at) = pick.selected.iter().position(|s| *s == option) {
                    pick.selected.remove(at);
                } else {
                    pick.selected.push(option);
                    pick.selected.sort_unstable();
                }
            }
            KeyCode::Enter => {
                if self.selected == rows - 1 {
                    return AskAction::Interrupt;
                }
                if self.selected == question.options.len() {
                    self.open_other(question);
                    return AskAction::None;
                }
                if question.multi_select {
                    if !self.picks[self.step].answered() {
                        return AskAction::None;
                    }
                } else {
                    self.picks[self.step] = QuestionPick {
                        selected: vec![self.selected as u32],
                        other: None,
                    };
                }
                return self.advance(card, questions);
            }
            // Typing on "Something else…" starts the answer with what was
            // typed; the digits stay the menu's.
            KeyCode::Char(_)
                if question.allow_other
                    && self.selected == question.options.len()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.open_other(question);
                self.other.key(key);
            }
            _ => {}
        }
        AskAction::None
    }

    fn goto(&mut self, questions: &[QuestionView], step: usize) {
        if step >= questions.len() {
            self.stage = Stage::Review;
            return;
        }
        self.step = step;
        self.selected = 0;
        self.other.secret = questions[step].secret;
    }

    fn advance(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        let direct = questions.len() == 1
            && !questions[0].multi_select
            && questions[0].options.iter().all(|o| o.preview.is_empty());
        if direct {
            return self.send_questions(card);
        }
        if self.step + 1 < questions.len() {
            self.goto(questions, self.step + 1);
        } else {
            self.stage = Stage::Review;
        }
        AskAction::None
    }

    fn send_questions(&mut self, card: &AskCard) -> AskAction {
        if !self.picks.iter().all(QuestionPick::answered) {
            return AskAction::None;
        }
        let picks: Vec<Pick> = self
            .picks
            .iter()
            .map(|pick| match &pick.other {
                Some(other) => Pick::Other(other.clone()),
                None => Pick::Options(pick.selected.clone()),
            })
            .collect();
        let note = self.note.text().trim().to_owned();
        let answer = question_answer(card, &picks, &note);
        match answer_input(card, &answer, &note) {
            Some(input) => AskAction::Answer(Box::new(input)),
            None => AskAction::None,
        }
    }
}

/// A form's Submit with the fields as its content.
fn with_content(answer: &Answer, fields: &[Field]) -> Answer {
    let mut content = Map::new();
    for field in fields {
        if let Some(value) = field.json() {
            content.insert(field.name.clone(), value);
        }
    }
    let bytes = serde_json::to_vec(&Value::Object(content)).unwrap_or_default();
    with_form_content(answer, bytes)
}

fn indent(text: impl Into<String>, style: Style, width: usize) -> Line<'static> {
    let mut line = Line::from(Span::raw("    "));
    push(&mut line, text.into(), style, width);
    line
}

fn menu_line(
    number: usize,
    label: &str,
    selected: bool,
    primary: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = Line::from(Span::styled(
        if selected { "  › " } else { "    " },
        theme.accent(),
    ));
    let style = if selected {
        theme.emphasis()
    } else if primary {
        theme.text()
    } else {
        theme.muted()
    };
    push(&mut line, format!("{number}. {label}"), style, width);
    line
}

fn editor_line(
    editor: &Editor,
    prompt: &str,
    width: usize,
    theme: Theme,
) -> (Line<'static>, usize) {
    let shown: String = if editor.secret {
        editor.text().chars().map(|_| '•').collect()
    } else {
        editor.text().replace('\n', " ")
    };
    let mut line = Line::from(Span::raw("    "));
    push(&mut line, prompt, theme.muted(), width);
    let column = text::line_width(&line) + editor.cursor_chars();
    push(&mut line, shown, theme.text(), width);
    (line, column)
}

/// What the card draws, and where the cursor goes when a field is open.
pub struct CardLines {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<(usize, usize)>,
}

impl AskUi {
    /// The card's lines at `width`: head, subject, body, menu and hints.
    pub fn render(
        &self,
        card: &AskCard,
        agent: &str,
        attach: bool,
        width: usize,
        theme: Theme,
    ) -> CardLines {
        let mut lines = Vec::new();
        let mut cursor = None;
        let mut head = Line::from(Span::styled("  ● ", theme.accent()));
        push(&mut head, headline(card), theme.emphasis(), width);
        if card.count > 1 {
            push_right(
                &mut head,
                &format!("{} of {}", card.position, card.count),
                theme.muted(),
                width,
            );
        }
        match &card.state {
            CardState::Sending => {
                let mut line = Line::from(Span::styled("  ◌ ", theme.muted()));
                push(
                    &mut line,
                    format!("Sending your answer · {}", headline(card)),
                    theme.muted(),
                    width,
                );
                return CardLines {
                    lines: vec![line],
                    cursor: None,
                };
            }
            CardState::Dismissed => {
                lines.push(head);
                lines.push(indent(
                    "The agent exited with this open; resume it to carry on.",
                    theme.muted(),
                    width,
                ));
                return CardLines {
                    lines,
                    cursor: None,
                };
            }
            CardState::NotConfirmed => {
                lines.push(head);
                lines.push(indent(
                    "Your answer was not confirmed: the connection dropped before the agent replied.",
                    theme.warn(),
                    width,
                ));
                lines.push(indent(
                    "r resend · d discard · ctrl+x stop",
                    theme.muted(),
                    width,
                ));
                return CardLines {
                    lines,
                    cursor: None,
                };
            }
            CardState::Rejected(reason) => {
                lines.push(head);
                lines.push(indent(format!("Not sent: {reason}"), theme.error(), width));
            }
            CardState::Open => lines.push(head),
        }
        self.body(card, agent, width, theme, &mut lines);
        let mut hint = String::from("↑↓/1-9 select · enter confirm");
        match &card.body {
            AskBody::Question(questions) => {
                cursor = self.question_menu(questions, width, theme, &mut lines);
                hint = match self.stage {
                    Stage::Review if card.question_note => {
                        "1-9 change · n add a note · enter send · esc back".into()
                    }
                    Stage::Review => "1-9 change · enter send · esc back".into(),
                    Stage::Other | Stage::Note(_) => "enter done · esc back".into(),
                    _ if questions[self.step].multi_select => {
                        let picked = self.picks[self.step].selected.len();
                        format!("space toggle · enter next ({picked} selected) · tab next question")
                    }
                    _ if questions.len() > 1 => {
                        "↑↓/1-9 select · enter pick · tab next question".into()
                    }
                    _ => hint,
                };
            }
            _ => {
                let offset = if matches!(card.body, AskBody::Form { .. }) {
                    cursor = self.form_lines(width, theme, &mut lines);
                    self.fields.len()
                } else {
                    0
                };
                for (i, entry) in self.entries(card, attach).iter().enumerate() {
                    let (label, primary) = match entry {
                        Entry::Choice(index) => {
                            let choice = &card.choices[*index];
                            (choice_label(choice), choice.primary)
                        }
                        Entry::Stop => ("Stop the turn".to_owned(), false),
                        Entry::Attach => ("Attach a terminal to answer it".to_owned(), false),
                    };
                    lines.push(menu_line(
                        i + 1,
                        &label,
                        i + offset == self.selected,
                        primary,
                        width,
                        theme,
                    ));
                }
                if let Stage::Note(index) = self.stage {
                    let prompt = if card.choices[index].outcome == ChoiceOutcome::SendBack {
                        "What should change: "
                    } else {
                        "Tell it why (optional): "
                    };
                    let (line, column) = editor_line(&self.note, prompt, width, theme);
                    cursor = Some((lines.len(), column));
                    lines.push(line);
                    hint = "enter send · esc back".into();
                }
            }
        }
        if Self::reader(card).is_some() && !self.editing() {
            hint.push_str(" · f open");
        }
        hint.push_str(" · ctrl+x stop");
        lines.push(indent(hint, theme.muted(), width));
        CardLines { lines, cursor }
    }

    fn body(
        &self,
        card: &AskCard,
        agent: &str,
        width: usize,
        theme: Theme,
        lines: &mut Vec<Line<'static>>,
    ) {
        match &card.body {
            AskBody::Command {
                command,
                cwd,
                reason,
                description,
            } => {
                for part in command.lines().take(BODY_LINES) {
                    lines.push(indent(format!("$ {part}"), theme.code(), width));
                }
                let mut about = Vec::new();
                for words in [description, reason] {
                    if !words.is_empty() {
                        about.push(words.clone());
                    }
                }
                if !cwd.is_empty() {
                    about.push(format!("in {cwd}"));
                }
                if !about.is_empty() {
                    lines.push(indent(about.join(" · "), theme.muted(), width));
                }
            }
            AskBody::Edit {
                path,
                added,
                removed,
                diff,
                reason,
                ..
            } => {
                lines.push(indent(
                    format!("{path}  +{added} −{removed}"),
                    theme.code(),
                    width,
                ));
                let diff_lines: Vec<&str> = diff.lines().collect();
                for part in diff_lines.iter().take(BODY_LINES) {
                    let style = if part.starts_with("@@") {
                        theme.diff_meta()
                    } else if part.starts_with('+') {
                        theme.diff_added()
                    } else if part.starts_with('-') {
                        theme.diff_removed()
                    } else {
                        theme.diff_context()
                    };
                    lines.push(indent(*part, style, width));
                }
                if diff_lines.len() > BODY_LINES {
                    lines.push(indent(
                        format!(
                            "⋮ {} more lines · f whole diff",
                            diff_lines.len() - BODY_LINES
                        ),
                        theme.muted(),
                        width,
                    ));
                }
                if !reason.is_empty() {
                    lines.push(indent(reason.clone(), theme.muted(), width));
                }
            }
            AskBody::Tool { arguments, .. } => {
                for part in arguments.lines().take(BODY_LINES) {
                    lines.push(indent(part, theme.code(), width));
                }
            }
            AskBody::Plan { plan } => {
                for part in plan
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(BODY_LINES)
                {
                    lines.push(indent(part, theme.text(), width));
                }
                lines.push(indent("f read the plan", theme.muted(), width));
            }
            AskBody::Question(_) => {}
            AskBody::Form { message, .. } => {
                if !message.is_empty() {
                    lines.push(indent(message.clone(), theme.text(), width));
                }
            }
            AskBody::Link { message, url, .. } => {
                if !message.is_empty() {
                    lines.push(indent(message.clone(), theme.text(), width));
                }
                lines.push(indent(url.clone(), theme.code(), width));
            }
            AskBody::Access {
                reason,
                read,
                write,
                network,
                hosts,
            } => {
                if !reason.is_empty() {
                    lines.push(indent(reason.clone(), theme.text(), width));
                }
                for path in write {
                    lines.push(indent(format!("Write to {path}"), theme.code(), width));
                }
                for path in read {
                    lines.push(indent(format!("Read {path}"), theme.code(), width));
                }
                if *network {
                    let who = if hosts.is_empty() {
                        "any host".to_owned()
                    } else {
                        hosts.join(", ")
                    };
                    lines.push(indent(
                        format!("Network access · {who}"),
                        theme.code(),
                        width,
                    ));
                }
            }
            AskBody::Unanswerable { reason } => {
                let words = if reason.is_empty() {
                    format!(
                        "{agent} is showing a menu this build can't read. Attach from a terminal to answer it, or stop the turn."
                    )
                } else {
                    reason.clone()
                };
                for part in text::wrap(&words, width.saturating_sub(4)) {
                    lines.push(indent(part, theme.text(), width));
                }
            }
        }
    }

    fn question_menu(
        &self,
        questions: &[QuestionView],
        width: usize,
        theme: Theme,
        lines: &mut Vec<Line<'static>>,
    ) -> Option<(usize, usize)> {
        if questions.len() > 1 {
            let mut chips = Line::from(Span::raw("    "));
            for (i, question) in questions.iter().enumerate() {
                let label = if question.header.is_empty() {
                    format!("{}", i + 1)
                } else {
                    question.header.clone()
                };
                let marker = if self.picks[i].answered() { "✔" } else { " " };
                let style = if self.stage != Stage::Review && i == self.step {
                    theme.emphasis()
                } else {
                    theme.muted()
                };
                push(&mut chips, format!("[{label}{marker}] "), style, width);
            }
            let style = if self.stage == Stage::Review {
                theme.emphasis()
            } else {
                theme.muted()
            };
            push(&mut chips, "[review]", style, width);
            lines.push(chips);
        }
        if self.stage == Stage::Review || matches!(self.stage, Stage::Note(_)) {
            for (i, (question, pick)) in questions.iter().zip(&self.picks).enumerate() {
                let answer = match (&pick.other, pick.selected.is_empty()) {
                    (Some(_), _) if question.secret => "answered (hidden)".to_owned(),
                    (Some(other), _) => format!("\"{other}\""),
                    (None, true) => "unanswered".to_owned(),
                    (None, false) => pick
                        .selected
                        .iter()
                        .filter_map(|s| question.options.get(*s as usize))
                        .map(|o| o.label.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                };
                let label = if question.header.is_empty() {
                    &question.question
                } else {
                    &question.header
                };
                let style = if pick.answered() {
                    theme.text()
                } else {
                    theme.warn()
                };
                lines.push(indent(
                    format!("{}. {label} · {answer}", i + 1),
                    style,
                    width,
                ));
            }
            if let Stage::Note(_) = self.stage {
                let (line, column) = editor_line(&self.note, "Note for the agent: ", width, theme);
                let at = lines.len();
                lines.push(line);
                return Some((at, column));
            }
            if !self.note.is_empty() {
                lines.push(indent(
                    format!("Note: {}", self.note.text()),
                    theme.muted(),
                    width,
                ));
            }
            return None;
        }
        let question = &questions[self.step];
        lines.push(indent(question.question.clone(), theme.text(), width));
        if let Some(preview) = question
            .options
            .get(self.selected)
            .map(|o| o.preview.as_str())
            .filter(|p| !p.is_empty())
        {
            for part in preview.lines().take(BODY_LINES) {
                lines.push(indent(format!("  {part}"), theme.code(), width));
            }
        }
        let pick = &self.picks[self.step];
        for (i, option) in question.options.iter().enumerate() {
            let mark = if question.multi_select {
                if pick.selected.contains(&(i as u32)) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            let mut label = format!("{mark}{}", option.label);
            if option.recommended {
                label.push_str("  recommended");
            }
            let mut line = menu_line(i + 1, &label, i == self.selected, true, width, theme);
            if !option.description.is_empty() {
                push(
                    &mut line,
                    format!("  {}", option.description),
                    theme.muted(),
                    width,
                );
            }
            lines.push(line);
        }
        let mut next = question.options.len();
        let mut cursor = None;
        if question.allow_other {
            // With no options to pick, typing is the only answer, not something else.
            let (typed, prompt) = if question.options.is_empty() {
                ("Answer", "Type the answer…")
            } else {
                ("Something else", "Something else…")
            };
            if self.stage == Stage::Other {
                let (line, column) = editor_line(&self.other, &format!("{typed}: "), width, theme);
                cursor = Some((lines.len(), column));
                lines.push(line);
            } else {
                let label = match &pick.other {
                    Some(other) if !question.secret => format!("{typed}: \"{other}\""),
                    Some(_) => format!("{typed}: (hidden)"),
                    None => prompt.to_owned(),
                };
                lines.push(menu_line(
                    next + 1,
                    &label,
                    self.selected == next,
                    false,
                    width,
                    theme,
                ));
            }
            next += 1;
        }
        lines.push(menu_line(
            next + 1,
            "Stop the turn",
            self.selected == next,
            false,
            width,
            theme,
        ));
        cursor
    }

    fn form_lines(
        &self,
        width: usize,
        theme: Theme,
        lines: &mut Vec<Line<'static>>,
    ) -> Option<(usize, usize)> {
        let mut cursor = None;
        for (i, field) in self.fields.iter().enumerate() {
            let required = if field.required { " *" } else { "" };
            let prompt = format!("{}{required}: ", field.title);
            if self.stage == Stage::Field && i == self.selected {
                let (line, column) = editor_line(&self.other, &prompt, width, theme);
                cursor = Some((lines.len(), column));
                lines.push(line);
                continue;
            }
            let value = match &field.kind {
                FieldKind::Toggle => if field.value == "true" { "[x]" } else { "[ ]" }.to_owned(),
                FieldKind::Choice(_) => format!("{} ⌄", field.value),
                _ => field.value.clone(),
            };
            let mut line = Line::from(Span::styled(
                if i == self.selected { "  › " } else { "    " },
                theme.accent(),
            ));
            push(
                &mut line,
                prompt,
                if field.valid() {
                    theme.muted()
                } else {
                    theme.warn()
                },
                width,
            );
            push(&mut line, value, theme.text(), width);
            lines.push(line);
        }
        cursor
    }
}

/// Lines of a command or arguments shown before "… [Show all]".
const SUBJECT_LINES: usize = 6;
/// Diff lines shown before "[Full diff]".
const DIFF_LINES: usize = 7;

/// What a click in the boxed ask reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSpot {
    /// A choice, by its place in the box's list.
    Choice(usize),
    /// A question's tab, or the Review tab after the last.
    Tab(usize),
    ShowAll,
    FullDiff,
}

/// A place in the boxed ask a click reaches: its line, columns and what.
pub type Spot = (usize, (usize, usize), BoxSpot);

/// The boxed ask drawn: its lines inside the box, where the cursor sits
/// when the deny note has it, and what clicks reach.
pub struct BoxLines {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<(usize, usize)>,
    pub spots: Vec<Spot>,
}

/// Whether the redesigned chat draws `card` in the composer's box: the
/// permission kinds and plans, from the moment they open until the agent
/// confirms the answer. Other kinds keep their card.
pub fn boxed(card: &AskCard) -> bool {
    matches!(
        card.body,
        AskBody::Command { .. }
            | AskBody::Edit { .. }
            | AskBody::Tool { .. }
            | AskBody::Plan { .. }
            | AskBody::Question(_)
    ) && !matches!(card.state, CardState::Dismissed)
}

/// Whether a choice is the box's one way to refuse: a denial, or sending a
/// plan back.
fn refuses(outcome: &ChoiceOutcome) -> bool {
    matches!(
        outcome,
        ChoiceOutcome::Deny { .. } | ChoiceOutcome::SendBack
    )
}

/// The box's choices, as indices into the card's: the ways to allow in the
/// agent's order, then the one way to refuse, last. Stopping is Ctrl+X's,
/// so "deny and stop" is not offered where denying lets the agent carry on.
fn box_choices(card: &AskCard) -> Vec<usize> {
    let mut list = Vec::new();
    let mut deny = None;
    for (i, choice) in card.choices.iter().enumerate() {
        match choice.outcome {
            ChoiceOutcome::Deny { .. } | ChoiceOutcome::SendBack => {
                deny.get_or_insert(i);
            }
            ChoiceOutcome::DenyAndStop => {}
            _ => list.push(i),
        }
    }
    list.extend(deny);
    list
}

/// A choice in the box, worded as what happens: "Yes", "Yes, and always
/// allow cargo test in this project", "No, and stop".
fn box_label(choice: &Choice) -> String {
    match &choice.outcome {
        ChoiceOutcome::AllowOnce => "Yes".to_owned(),
        ChoiceOutcome::AllowForSession => "Yes, and don't ask again this session".to_owned(),
        ChoiceOutcome::AllowAlways { mode, .. } if !mode.is_empty() => format!(
            "Yes, and switch to {}",
            crate::words::mode_name(&ui_view::ModeValue::Claude(mode.clone())).to_lowercase()
        ),
        ChoiceOutcome::Deny { stops: true } | ChoiceOutcome::DenyAndStop => {
            "No, and stop".to_owned()
        }
        ChoiceOutcome::Deny { stops: false } => "No".to_owned(),
        ChoiceOutcome::ApprovePlan {
            auto_accept_edits: false,
        } => "Yes, start building".to_owned(),
        ChoiceOutcome::ApprovePlan {
            auto_accept_edits: true,
        } => "Yes, and accept edits without asking".to_owned(),
        ChoiceOutcome::SendBack => "No, keep planning".to_owned(),
        _ => {
            let words = choice_label(choice);
            let mut chars = words.chars();
            let first = chars
                .next()
                .map(|c| c.to_lowercase().to_string())
                .unwrap_or_default();
            format!("Yes, and {first}{}", chars.as_str())
        }
    }
}

/// "cargo test -p ui-runtime…": the subject short enough for one line.
fn short_subject(card: &AskCard) -> String {
    let subject = match &card.body {
        AskBody::Command { command, .. } => text::first_line(command).to_owned(),
        AskBody::Edit { path, .. } => path.clone(),
        AskBody::Tool { server, tool, .. } => format!("{server} {tool}").trim().to_owned(),
        _ => String::new(),
    };
    text::ellipsize(&subject, 48)
}

/// The step's asking verb.
fn asking_verb(card: &AskCard) -> &'static str {
    match &card.body {
        AskBody::Command { .. } => "Wants to run",
        AskBody::Edit { created: true, .. } => "Wants to create",
        AskBody::Edit { .. } => "Wants to edit",
        AskBody::Plan { .. } => "Plan ready",
        _ => "Wants to use",
    }
}

impl AskUi {
    /// Whether the boxed ask's note holds something, for Ctrl+C.
    pub fn box_note_text(&self, card: &AskCard) -> bool {
        self.in_box_note(card) && !(self.note.is_empty() && self.other.is_empty())
    }

    /// Clears the boxed ask's note, as a kill.
    pub fn kill_box_note(&mut self) -> bool {
        let note = self.note.kill_all();
        let other = self.other.kill_all();
        note || other
    }

    /// A paste into the boxed ask's note, when it has the keys.
    pub fn paste_box_note(&mut self, card: &AskCard, text: &str) {
        if self.in_box_note(card) {
            if matches!(card.body, AskBody::Question(_)) {
                self.other.insert_str(text);
            } else {
                self.note.insert_str(text);
            }
        }
    }

    /// Whether the highlight is on a refusal that can carry a note.
    pub fn on_noted_deny(&self, card: &AskCard) -> bool {
        let list = box_choices(card);
        list.len().checked_sub(1) == Some(self.selected)
            && list
                .last()
                .and_then(|i| card.choices.get(*i))
                .is_some_and(|choice| refuses(&choice.outcome) && choice.takes_note)
    }

    /// Whether the deny note is open, with the keys.
    fn box_note(&self, card: &AskCard) -> bool {
        self.noting && self.on_noted_deny(card)
    }

    /// Whether the boxed ask's deny note, or a question's "Something else",
    /// has the keys.
    pub fn in_box_note(&self, card: &AskCard) -> bool {
        matches!(card.state, CardState::Open | CardState::Rejected(_))
            && match &card.body {
                AskBody::Question(questions) => !self.replying && self.on_something_else(questions),
                _ => self.box_note(card),
            }
    }

    /// The whole diff, for the reader.
    fn full_diff(card: &AskCard) -> AskAction {
        Self::reader(card).unwrap_or(AskAction::None)
    }

    /// Sends the box's choice at `at`, the deny with its note.
    fn box_send(&mut self, card: &AskCard, at: usize) -> AskAction {
        let Some(&index) = box_choices(card).get(at) else {
            return AskAction::None;
        };
        let choice = &card.choices[index];
        let note = if choice.takes_note {
            self.note.text().trim().to_owned()
        } else {
            String::new()
        };
        if let Some(step) = ui_view::composed(&choice.answer) {
            self.sent = Some(index);
            return AskAction::Compose {
                key: card.key.clone(),
                inputs: ui_view::plan_inputs(card, step, &note),
            };
        }
        match answer_input(card, &choice.answer, &note) {
            Some(input) => {
                self.sent = Some(index);
                AskAction::Answer(Box::new(input))
            }
            None => AskAction::None,
        }
    }

    /// One key on a boxed ask. ↑/↓ and j/k move, 1–9 answer at once,
    /// Enter answers with the highlighted choice. On a refusal that can
    /// carry a note, Tab opens the note; in it, Enter sends and Esc clears
    /// it and closes it. Esc at the list does nothing. Ctrl+X is the chat's.
    pub fn box_key(&mut self, card: &AskCard, key: KeyEvent) -> AskAction {
        match card.state {
            CardState::Open | CardState::Rejected(_) => {}
            _ => return self.key(card, key, false),
        }
        if let AskBody::Question(questions) = &card.body {
            return self.question_key_boxed(card, questions, key);
        }
        let count = box_choices(card).len();
        if self.box_note(card) {
            match key.code {
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    return self.box_send(card, self.selected);
                }
                KeyCode::Esc => {
                    self.note = Editor::default();
                    self.noting = false;
                }
                _ => {
                    self.note.key(key);
                }
            }
            return AskAction::None;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char(c @ '1'..='9') => {
                let at = c as usize - '1' as usize;
                if at < count {
                    self.selected = at;
                    return self.box_send(card, at);
                }
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                return self.box_send(card, self.selected);
            }
            KeyCode::Tab if self.on_noted_deny(card) => self.noting = true,
            KeyCode::Char('f') => match card.body {
                AskBody::Edit { .. } => return Self::full_diff(card),
                // The plan is in the feed, whole.
                AskBody::Plan { .. } => {}
                _ => self.show_all = !self.show_all,
            },
            _ => {}
        }
        AskAction::None
    }

    /// A click on a boxed ask.
    pub fn box_click(&mut self, card: &AskCard, spot: BoxSpot) -> AskAction {
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return AskAction::None;
        }
        if let AskBody::Question(questions) = &card.body {
            return self.question_click(card, questions, spot);
        }
        match spot {
            BoxSpot::Tab(_) => AskAction::None,
            BoxSpot::Choice(at) => {
                self.selected = at;
                self.box_send(card, at)
            }
            BoxSpot::ShowAll => {
                self.show_all = !self.show_all;
                AskAction::None
            }
            BoxSpot::FullDiff => Self::full_diff(card),
        }
    }

    /// The boxed ask's lines, `width` columns wide: what it wants, the
    /// subject verbatim, the agent's reason, then the choices.
    pub fn box_lines(&self, card: &AskCard, width: usize, theme: Theme) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let line = |words: String, style: Style| {
            let mut line = Line::default();
            push(&mut line, words, style, width);
            line
        };
        match &card.state {
            // Answered: one line until the agent confirms.
            CardState::Sending => {
                let chose = self
                    .sent
                    .and_then(|i| card.choices.get(i))
                    .map(|choice| &choice.outcome);
                let words = match chose {
                    _ if matches!(card.body, AskBody::Question(_)) => "Sending…".to_owned(),
                    Some(ChoiceOutcome::Deny { .. } | ChoiceOutcome::DenyAndStop) => {
                        "Denying…".to_owned()
                    }
                    Some(ChoiceOutcome::SendBack) => "Sending the plan back…".to_owned(),
                    Some(ChoiceOutcome::ApprovePlan { .. }) => "Approving the plan…".to_owned(),
                    _ => format!("Allowing {}…", short_subject(card)),
                };
                out.lines.push(line(words, theme.faint()));
                return out;
            }
            CardState::NotConfirmed => {
                out.lines.push(line(
                    "Your answer was not confirmed: the connection dropped before the agent replied."
                        .into(),
                    theme.warn(),
                ));
                out.lines
                    .push(line("r resend · d discard".into(), theme.faint()));
                return out;
            }
            _ => {}
        }
        if let AskBody::Question(questions) = &card.body {
            return self.question_lines(questions, width, theme);
        }
        let mut head = Line::from(Span::styled("● ", theme.accent()));
        push(&mut head, asking_verb(card), theme.text(), width);
        if card.count > 1 {
            push_right(
                &mut head,
                &format!("{} of {}", card.position, card.count),
                theme.faint(),
                width,
            );
        }
        out.lines.push(head);
        if let CardState::Rejected(reason) = &card.state {
            out.lines
                .push(line(format!("Not sent: {reason}"), theme.error()));
        }

        // The subject, verbatim in the code colour: never cut short
        // without saying so.
        let capped = |out: &mut BoxLines, words: &str, show_all: bool| {
            let wrapped: Vec<String> = words
                .lines()
                .flat_map(|l| text::wrap(l, width.max(1)))
                .collect();
            let cut = !show_all && wrapped.len() > SUBJECT_LINES;
            let take = if cut {
                SUBJECT_LINES - 1
            } else {
                wrapped.len()
            };
            for part in wrapped.iter().take(take) {
                out.lines.push(line(part.clone(), theme.code()));
            }
            if cut {
                let mut more = Line::default();
                let hidden = wrapped.len() - take;
                let s = if hidden == 1 { "" } else { "s" };
                push(
                    &mut more,
                    format!("… {hidden} more line{s} "),
                    theme.faint(),
                    width,
                );
                let from = text::line_width(&more);
                push(&mut more, "[Show all]", theme.muted(), width);
                out.spots
                    .push((out.lines.len(), (from, from + 10), BoxSpot::ShowAll));
                out.lines.push(more);
            }
        };
        let reason = match &card.body {
            AskBody::Command {
                command,
                reason,
                description,
                ..
            } => {
                capped(&mut out, command, self.show_all);
                if reason.is_empty() {
                    description.clone()
                } else {
                    reason.clone()
                }
            }
            AskBody::Tool {
                server,
                tool,
                arguments,
            } => {
                let mut name = Line::default();
                if !server.is_empty() {
                    push(&mut name, format!("{server} "), theme.code(), width);
                }
                push(&mut name, tool.clone(), theme.code(), width);
                out.lines.push(name);
                if !arguments.is_empty() {
                    capped(&mut out, arguments, self.show_all);
                }
                String::new()
            }
            AskBody::Edit {
                path,
                files,
                diff,
                reason,
                ..
            } => {
                // The path and where in the file, then why, then the patch.
                let mut name = Line::default();
                push(&mut name, path.clone(), theme.code(), width);
                if *files > 1 {
                    push(
                        &mut name,
                        format!(" and {} more files", files - 1),
                        theme.faint(),
                        width,
                    );
                }
                if let Some(line) = first_hunk_line(diff) {
                    push(&mut name, format!(" · line {line}"), theme.faint(), width);
                }
                out.lines.push(name);
                for part in text::wrap(reason, width.max(1)) {
                    out.lines.push(line(part, theme.faint()));
                }
                // The patch's first lines as the review page draws them.
                let lines: Vec<&str> = diff
                    .lines()
                    .filter(|l| {
                        !l.starts_with("@@") && !l.starts_with("---") && !l.starts_with("+++")
                    })
                    .collect();
                let cut = lines.len() > DIFF_LINES;
                let take = if cut { DIFF_LINES - 1 } else { lines.len() };
                for l in lines.iter().take(take) {
                    let (mark, style) = match l.chars().next() {
                        Some('+') => ('+', theme.diff_added()),
                        Some('-') => ('-', theme.diff_removed()),
                        _ => (' ', theme.diff_context()),
                    };
                    let body = l.get(1..).unwrap_or_default();
                    let mut row = Line::default();
                    push(
                        &mut row,
                        format!("{mark} {}", text::ellipsize(body, width.saturating_sub(2))),
                        style,
                        width,
                    );
                    text::fill(&mut row, style, width);
                    out.lines.push(row);
                }
                if cut {
                    let hidden = lines.len() - take;
                    let s = if hidden == 1 { "" } else { "s" };
                    let mut more = Line::default();
                    push(
                        &mut more,
                        format!("… {hidden} more line{s} "),
                        theme.faint(),
                        width,
                    );
                    let from = text::line_width(&more);
                    push(&mut more, "[Full diff]", theme.muted(), width);
                    out.spots
                        .push((out.lines.len(), (from, from + 11), BoxSpot::FullDiff));
                    out.lines.push(more);
                }
                String::new()
            }
            _ => String::new(),
        };
        if !reason.is_empty() {
            for part in text::wrap(&reason, width.max(1)) {
                out.lines.push(line(part, theme.faint()));
            }
        }
        out.lines.push(Line::default());

        // The choices, numbered, the highlighted one bright. The refusal is
        // last, and where it can carry a note its line is the note's field.
        let list = box_choices(card);
        for (at, index) in list.iter().enumerate() {
            let choice = &card.choices[*index];
            let lit = at == self.selected;
            let ink = if lit { theme.bright() } else { theme.text() };
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.accent(),
                width,
            );
            push(&mut row, format!("{}. ", at + 1), ink, width);
            let deny = refuses(&choice.outcome);
            let said = box_label(choice);
            let typed = self.note.text();
            if deny && choice.takes_note && (self.noting || !typed.is_empty()) {
                // "No: <note>", the note as typed.
                push(&mut row, format!("{said}: "), ink, width);
                let at_col = text::line_width(&row);
                let shown = text_tail(typed, width.saturating_sub(at_col + 1));
                push(&mut row, shown.clone(), ink, width);
                if lit && self.noting {
                    out.cursor = Some((out.lines.len(), at_col + text::str_width(&shown)));
                }
            } else {
                push(&mut row, said, ink, width);
                if deny && choice.takes_note {
                    push(&mut row, " · tab to add a note", theme.faint(), width);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        out
    }
}

/// The new file's first line a patch touches: "@@ -10,3 +12,4 @@" is 12.
fn first_hunk_line(diff: &str) -> Option<u32> {
    let header = diff.lines().find(|line| line.starts_with("@@"))?;
    let new = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))?;
    new[1..].split(',').next()?.parse().ok()
}

/// The end of `words` in at most `max` columns, "…" where it is cut.
fn text_tail(words: &str, max: usize) -> String {
    if text::str_width(words) <= max {
        return words.to_owned();
    }
    let keep = max.saturating_sub(1);
    let chars: Vec<char> = words.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut used = 0;
    for c in chars.iter().rev() {
        let w = text::str_width(c.encode_utf8(&mut [0; 4]));
        if used + w > keep {
            break;
        }
        used += w;
        out.push(*c);
    }
    out.reverse();
    format!("…{}", out.into_iter().collect::<String>())
}

/// Lines of a highlighted option's preview shown under the options.
const PREVIEW_LINES: usize = 8;

/// A question's rows in the box: its options, "Something else" when it
/// takes one, then "Reply instead".
struct QuestionRows {
    options: usize,
    other: Option<usize>,
    reply: usize,
}

impl QuestionRows {
    fn of(question: &QuestionView) -> QuestionRows {
        let options = question.options.len();
        let other = question.allow_other.then_some(options);
        QuestionRows {
            options,
            other,
            reply: options + usize::from(question.allow_other),
        }
    }
}

/// "The shell", "The fleet, The chat", "\"tmux style\"": an answer in words,
/// empty when the question was skipped.
fn pick_words(question: &QuestionView, pick: &QuestionPick) -> String {
    if let Some(other) = pick.other.as_ref().filter(|other| !other.is_empty()) {
        return other.clone();
    }
    pick.selected
        .iter()
        .filter_map(|i| question.options.get(*i as usize))
        .map(|option| option.label.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A question's name on its tab and in the review: its header, else its
/// place.
fn question_name(question: &QuestionView, at: usize) -> String {
    if question.header.is_empty() {
        format!("Question {}", at + 1)
    } else {
        question.header.clone()
    }
}

impl AskUi {
    /// Whether the composer has the keys to reply instead of answering.
    pub fn replying(&self) -> bool {
        self.replying
    }

    /// Back from the reply to the questions.
    pub fn stop_replying(&mut self) {
        self.replying = false;
    }

    /// The reply, sent as the question ask's refusal with what was answered.
    pub fn reply_input(&mut self, card: &AskCard, words: &str) -> Option<wire::Input> {
        let input = ui_view::question_reply(card, &self.question_picks(), words)?;
        self.replying = false;
        Some(input)
    }

    fn in_review(&self, questions: &[QuestionView]) -> bool {
        questions.len() > 1 && self.step >= questions.len()
    }

    /// The highlight is on the current question's "Something else".
    fn on_other_row(&self, questions: &[QuestionView]) -> bool {
        !self.in_review(questions)
            && questions
                .get(self.step)
                .is_some_and(|question| QuestionRows::of(question).other == Some(self.selected))
    }

    /// "Something else" is open as a field and has the keys: opened by Tab,
    /// or by Enter while empty, and closed by Esc, as a permission's note.
    fn on_something_else(&self, questions: &[QuestionView]) -> bool {
        self.noting && self.on_other_row(questions)
    }

    fn question_picks(&self) -> Vec<Pick> {
        self.picks
            .iter()
            .map(
                |pick| match pick.other.as_ref().filter(|other| !other.is_empty()) {
                    Some(other) => Pick::Other(other.clone()),
                    None => Pick::Options(pick.selected.clone()),
                },
            )
            .collect()
    }

    /// To question `step`, or to the review past the last. The highlight
    /// starts on what was picked, else the first option; in the review, on
    /// "Send answers".
    fn question_goto(&mut self, questions: &[QuestionView], step: usize) {
        let count = questions.len();
        self.stage = Stage::Menu;
        if step >= count && count > 1 {
            self.step = count;
            self.selected = count;
            return;
        }
        self.step = step.min(count.saturating_sub(1));
        let pick = &self.picks[self.step];
        let rows = QuestionRows::of(&questions[self.step]);
        self.selected = match (&pick.other, pick.selected.first()) {
            (Some(other), _) if !other.is_empty() => rows.other.unwrap_or(0),
            (_, Some(first)) => *first as usize,
            _ => 0,
        };
        self.noting = false;
        self.other = Editor::default();
        self.other.secret = questions[self.step].secret;
        if let Some(other) = &pick.other {
            self.other.insert_str(other);
        }
    }

    /// After a question is answered: a lone question sends; otherwise on to
    /// the next one not yet answered, or the review when none is left.
    fn question_answered(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        let count = questions.len();
        if count == 1 {
            return self.send_boxed_questions(card);
        }
        let next = (1..count)
            .map(|ahead| (self.step + ahead) % count)
            .find(|at| !self.picks[*at].answered());
        self.question_goto(questions, next.unwrap_or(count));
        AskAction::None
    }

    /// Sends the answers, a skipped question with none.
    fn send_boxed_questions(&mut self, card: &AskCard) -> AskAction {
        let answer = question_answer(card, &self.question_picks(), "");
        match answer_input(card, &answer, "") {
            Some(input) => AskAction::Answer(Box::new(input)),
            None => AskAction::None,
        }
    }

    /// Acts on row `at` of the current question: picks an option (toggles
    /// one of several), opens "Something else", or turns to replying.
    /// `confirm` is Enter, which also finishes a question of several picks.
    fn question_row(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        at: usize,
        confirm: bool,
    ) -> AskAction {
        let question = &questions[self.step];
        let rows = QuestionRows::of(question);
        self.selected = at;
        if at < rows.options {
            let option = at as u32;
            let pick = &mut self.picks[self.step];
            if !question.multi_select {
                *pick = QuestionPick {
                    selected: vec![option],
                    other: None,
                };
                return self.question_answered(card, questions);
            }
            if confirm {
                if !pick.answered() && questions.len() == 1 {
                    return AskAction::None;
                }
                return self.question_answered(card, questions);
            }
            pick.other = None;
            match pick.selected.iter().position(|s| *s == option) {
                Some(found) => {
                    pick.selected.remove(found);
                }
                None => {
                    pick.selected.push(option);
                    pick.selected.sort_unstable();
                }
            }
            return AskAction::None;
        }
        if rows.other == Some(at) {
            let typed = self.other.text().trim().to_owned();
            if confirm && typed.is_empty() || !confirm {
                // Enter on an empty field, or its digit, opens it to type.
                self.noting = true;
                return AskAction::None;
            }
            if confirm && !typed.is_empty() {
                self.picks[self.step] = QuestionPick {
                    selected: vec![],
                    other: Some(typed),
                };
                return self.question_answered(card, questions);
            }
            return AskAction::None;
        }
        if at == rows.reply {
            self.replying = true;
        }
        AskAction::None
    }

    /// One key on a boxed question ask. ←/→ move between questions and the
    /// review, ↑/↓ (or j/k) between rows, 1–9 act on a row at once, Space
    /// toggles one of several picks, Enter acts on the highlighted row. On
    /// "Something else" the line is a field: typing goes there, Enter
    /// answers with it, Esc clears it.
    fn question_key_boxed(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        key: KeyEvent,
    ) -> AskAction {
        let count = questions.len();
        let several = count > 1;
        let enter = key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT);
        if self.in_review(questions) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(count)
                }
                KeyCode::Left => self.question_goto(questions, count - 1),
                _ if enter && self.selected < count => self.question_goto(questions, self.selected),
                _ if enter => return self.send_boxed_questions(card),
                _ => {}
            }
            return AskAction::None;
        }
        let rows = QuestionRows::of(&questions[self.step]);
        let last = rows.reply;
        if self.on_something_else(questions) {
            match key.code {
                KeyCode::Esc => {
                    self.other = Editor::default();
                    self.noting = false;
                    self.picks[self.step].other = None;
                }
                _ if enter => {
                    if self.other.text().trim().is_empty() {
                        return AskAction::None;
                    }
                    return self.question_row(card, questions, self.selected, true);
                }
                _ => {
                    self.other.key(key);
                }
            }
            return AskAction::None;
        }
        match key.code {
            KeyCode::Left if several => self.question_goto(questions, self.step.saturating_sub(1)),
            KeyCode::Right if several => self.question_goto(questions, self.step + 1),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            // "Reply instead" is not numbered: digits reach the options
            // and "Something else" only.
            KeyCode::Char(c @ '1'..='9') => {
                let at = c as usize - '1' as usize;
                if at < last {
                    return self.question_row(card, questions, at, false);
                }
            }
            KeyCode::Tab if self.on_other_row(questions) => self.noting = true,
            KeyCode::Char(' ') if self.selected < rows.options => {
                return self.question_row(card, questions, self.selected, false);
            }
            _ if enter => return self.question_row(card, questions, self.selected, true),
            _ => {}
        }
        AskAction::None
    }

    /// A click on a boxed question: a tab moves there, a row acts as its
    /// digit would.
    fn question_click(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        spot: BoxSpot,
    ) -> AskAction {
        match spot {
            BoxSpot::Tab(at) => {
                self.question_goto(questions, at);
                AskAction::None
            }
            BoxSpot::Choice(at) if self.in_review(questions) => {
                if at < questions.len() {
                    self.question_goto(questions, at);
                    AskAction::None
                } else {
                    self.send_boxed_questions(card)
                }
            }
            BoxSpot::Choice(at) => self.question_row(card, questions, at, false),
            _ => AskAction::None,
        }
    }

    /// The hint line's words while a boxed question has the keys.
    pub fn question_hint(&self, card: &AskCard, leader: char) -> Option<String> {
        let AskBody::Question(questions) = &card.body else {
            return None;
        };
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return None;
        }
        if self.replying {
            return Some("enter send reply · esc back to the questions · ctrl+x stop".into());
        }
        let several = questions.len() > 1;
        let tabs = if several { " · ←/→ questions" } else { "" };
        if self.in_review(questions) {
            return Some(format!(
                "enter send{tabs} · ctrl+x stop · ctrl+{leader} more"
            ));
        }
        if self.on_something_else(questions) {
            return Some("enter answer · esc clear · ctrl+x stop".into());
        }
        if self.on_other_row(questions) {
            return Some(format!("tab type{tabs} · ctrl+x stop · ctrl+{leader} more"));
        }
        if self.selected == QuestionRows::of(&questions[self.step]).reply {
            return Some(format!(
                "enter reply{tabs} · ctrl+x stop · ctrl+{leader} more"
            ));
        }
        // What Enter does next: send a lone question, or move on to the next
        // unanswered one, or to the review when none is left.
        let others_open =
            (0..questions.len()).any(|at| at != self.step && !self.picks[at].answered());
        let enter = if !several {
            "enter send"
        } else if others_open {
            "enter next"
        } else {
            "enter review"
        };
        if questions[self.step].multi_select {
            // Four at most: the stop stays, the leader goes.
            Some(format!("space select · {enter}{tabs} · ctrl+x stop"))
        } else {
            Some(format!(
                "enter choose{tabs} · ctrl+x stop · ctrl+{leader} more"
            ))
        }
    }

    /// The boxed question ask's lines: the tabs (or the lone question's
    /// name), the question, its rows; or the review.
    fn question_lines(&self, questions: &[QuestionView], width: usize, theme: Theme) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let count = questions.len();
        let review = self.in_review(questions);

        // The head: a tab per question, then Review; or one question's name.
        let mut head = Line::from(Span::styled("● ", theme.accent()));
        if count == 1 {
            push(
                &mut head,
                question_name(&questions[0], 0),
                theme.bright(),
                width,
            );
        } else {
            let names: Vec<String> = questions
                .iter()
                .enumerate()
                .map(|(at, question)| question_name(question, at))
                .chain(std::iter::once("Review".to_owned()))
                .collect();
            let current = self.step.min(count);
            // Each tab is its name with a space either side, so the current
            // one's chip fits around it; ‹ and › at the ends say ←/→ move.
            // Always drawn, so the row never shifts as the current tab does.
            let marks: usize = self.picks.iter().filter(|pick| pick.answered()).count() * 2;
            let room = width.saturating_sub(2 + 4 + marks + 3 * names.len());
            let natural: usize = names.iter().map(|name| text::str_width(name)).sum();
            let cap = if natural <= room {
                usize::MAX
            } else {
                (room / names.len()).max(4)
            };
            let chip = theme
                .row_surface()
                .unwrap_or_else(|| theme.user_surface())
                .patch(theme.bright());
            push(&mut head, "‹ ", theme.faint(), width);
            for (at, name) in names.iter().enumerate() {
                if at > 0 {
                    push(&mut head, " ", theme.faint(), width);
                }
                let answered = self.picks.get(at).is_some_and(QuestionPick::answered);
                let style = if at == current {
                    chip
                } else if answered {
                    theme.faint()
                } else {
                    theme.text()
                };
                let from = text::line_width(&head);
                let mark = if answered { "✓ " } else { "" };
                push(
                    &mut head,
                    format!(" {mark}{} ", text::ellipsize(name, cap)),
                    style,
                    width,
                );
                out.spots
                    .push((0, (from, text::line_width(&head)), BoxSpot::Tab(at)));
            }
            push(&mut head, " ›", theme.faint(), width);
        }
        out.lines.push(head);
        out.lines.push(Line::default());

        let row_line = |at: usize, lit: bool| {
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.accent(),
                width,
            );
            push(
                &mut row,
                format!("{}. ", at + 1),
                if lit { theme.bright() } else { theme.text() },
                width,
            );
            row
        };

        if review {
            // Each question as asked, faint, then "→ answer" under it; the
            // highlighted one's lines brighten.
            for (at, question) in questions.iter().enumerate() {
                let lit = self.selected == at;
                for (i, part) in text::wrap(&question.question, width.saturating_sub(2).max(1))
                    .into_iter()
                    .enumerate()
                {
                    let mut row = Line::default();
                    push(
                        &mut row,
                        if lit && i == 0 { "› " } else { "  " },
                        theme.accent(),
                        width,
                    );
                    push(
                        &mut row,
                        part,
                        if lit { theme.text() } else { theme.faint() },
                        width,
                    );
                    out.spots
                        .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                    out.lines.push(row);
                }
                let words = pick_words(question, &self.picks[at]);
                let (words, ink) = if words.is_empty() {
                    ("not answered".to_owned(), theme.faint())
                } else if lit {
                    (words, theme.bright())
                } else {
                    (words, theme.text())
                };
                for (i, part) in text::wrap(&words, width.saturating_sub(6).max(1))
                    .into_iter()
                    .enumerate()
                {
                    let mut row = Line::default();
                    push(
                        &mut row,
                        if i == 0 { "    → " } else { "      " },
                        theme.faint(),
                        width,
                    );
                    push(&mut row, part, ink, width);
                    out.spots
                        .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                    out.lines.push(row);
                }
            }
            out.lines.push(Line::default());
            let lit = self.selected >= count;
            let mut send = Line::default();
            push(
                &mut send,
                if lit { "› " } else { "  " },
                theme.accent(),
                width,
            );
            push(
                &mut send,
                "Send answers",
                if lit { theme.bright() } else { theme.text() },
                width,
            );
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(count)));
            out.lines.push(send);
            return out;
        }

        let question = &questions[self.step];
        let pick = &self.picks[self.step];
        let rows = QuestionRows::of(question);
        for part in text::wrap(&question.question, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.text(), width);
            out.lines.push(line);
        }
        if question.multi_select {
            let mut line = Line::default();
            push(&mut line, "Select all that apply", theme.faint(), width);
            out.lines.push(line);
        }
        out.lines.push(Line::default());

        for (at, option) in question.options.iter().enumerate() {
            let lit = self.selected == at;
            let mut row = row_line(at, lit);
            if question.multi_select {
                let on = pick.selected.contains(&(at as u32));
                push(
                    &mut row,
                    if on { "[✓] " } else { "[ ] " },
                    theme.muted(),
                    width,
                );
            }
            let indent = text::line_width(&row);
            push(
                &mut row,
                option.label.clone(),
                if lit { theme.bright() } else { theme.text() },
                width,
            );
            if option.recommended {
                push(&mut row, " · recommended", theme.faint(), width);
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            // The description after the label when it fits, else under it,
            // hanging at the label.
            let description = option.description.trim();
            if !description.is_empty() {
                let used = text::line_width(&row);
                if used + 3 + text::str_width(description) <= width {
                    push(&mut row, format!(" · {description}"), theme.faint(), width);
                    out.lines.push(row);
                } else {
                    out.lines.push(row);
                    for part in text::wrap(description, width.saturating_sub(indent).max(1)) {
                        let mut more = Line::from(Span::raw(" ".repeat(indent)));
                        push(&mut more, part, theme.faint(), width);
                        out.spots
                            .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                        out.lines.push(more);
                    }
                }
            } else {
                out.lines.push(row);
            }
        }
        if let Some(at) = rows.other {
            let lit = self.selected == at;
            let mut row = row_line(at, lit);
            let ink = if lit { theme.bright() } else { theme.text() };
            let typed = self.other.text();
            let open = lit && self.noting;
            if open || !typed.is_empty() {
                push(&mut row, "Something else: ", ink, width);
                let col = text::line_width(&row);
                let shown = text_tail(typed, width.saturating_sub(col + 1));
                push(&mut row, shown.clone(), ink, width);
                if open {
                    out.cursor = Some((out.lines.len(), col + text::str_width(&shown)));
                }
            } else {
                push(&mut row, "Something else", ink, width);
                if lit {
                    push(&mut row, " · tab to type", theme.faint(), width);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        // "Reply instead" is a footer, not an answer: apart, unnumbered,
        // faint until highlighted.
        out.lines.push(Line::default());
        let lit = self.selected == rows.reply;
        let mut row = Line::default();
        push(
            &mut row,
            if lit { "› " } else { "  " },
            theme.accent(),
            width,
        );
        push(
            &mut row,
            "Reply instead",
            if lit { theme.bright() } else { theme.faint() },
            width,
        );
        out.spots
            .push((out.lines.len(), (0, width), BoxSpot::Choice(rows.reply)));
        out.lines.push(row);

        // The highlighted option's preview, as the card drew it.
        if let Some(preview) = question
            .options
            .get(self.selected)
            .map(|option| option.preview.as_str())
            .filter(|preview| !preview.is_empty())
        {
            out.lines.push(Line::default());
            for part in preview.lines().take(PREVIEW_LINES) {
                let mut line = Line::default();
                push(&mut line, text::ellipsize(part, width), theme.code(), width);
                out.lines.push(line);
            }
        }
        out
    }
}
