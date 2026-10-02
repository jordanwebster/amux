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
    /// Hand the terminal to the agent's own interface.
    Attach,
    /// Open a link in the person's browser; the ask stays.
    OpenUrl(String),
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
    Number {
        integer: bool,
    },
    Toggle,
    Choice(Vec<String>),
    /// An array of enum values: several picks.
    Many(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
struct Field {
    name: String,
    title: String,
    required: bool,
    kind: FieldKind,
    /// Text, number and choice values; "true"/"false" for a toggle.
    value: String,
    description: String,
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
    /// The boxed ask's command, arguments or diff shown whole.
    show_all: bool,
    /// How far an opened diff taller than the box can hold is scrolled.
    diff_scroll: usize,
    /// The choice last sent, for the line the box shows until the agent
    /// confirms it.
    sent: Option<usize>,
    /// The boxed ask's deny note is open for typing.
    noting: bool,
    /// The terminal's rows, so a question's preview leaves the feed room.
    room: usize,
    /// The agent's own terminal can be attached, for what this client
    /// cannot answer.
    attach: bool,
    /// The open field's text was refused (a form's number that is not one).
    invalid: bool,
    /// Per question, what was typed in its field but not answered with:
    /// kept when Tab moves on, so coming back shows it.
    drafts: Vec<String>,
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

/// A JSON object's members in the order written. A parsed `Value` keeps
/// its keys sorted, and a form asks its fields in its schema's order.
struct Members(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for Members {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Members, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Members;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Members, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Members(members))
            }
        }
        deserializer.deserialize_map(Visit)
    }
}

/// A form schema's fields, in the schema's own order.
fn form_fields(schema_json: &str) -> Vec<Field> {
    #[derive(serde::Deserialize)]
    struct Schema {
        #[serde(default)]
        properties: Option<Members>,
        #[serde(default)]
        required: Vec<String>,
    }
    let Ok(Schema {
        properties: Some(Members(properties)),
        required,
    }) = serde_json::from_str::<Schema>(schema_json)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| {
            let names = |options: &Vec<Value>| {
                options
                    .iter()
                    .map(|option| match option {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            let many = property
                .get("items")
                .and_then(|items| items.get("enum"))
                .and_then(Value::as_array)
                .filter(|_| property.get("type").and_then(Value::as_str) == Some("array"));
            let kind = if let Some(options) = many {
                FieldKind::Many(names(options))
            } else if let Some(options) = property.get("enum").and_then(Value::as_array) {
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
                required: required.contains(name),
                name: name.clone(),
                kind,
                value,
                description: property
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
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
            FieldKind::Many(_) => Some(Value::Array(
                self.value
                    .split('\u{1f}')
                    .map(|v| Value::String(v.to_owned()))
                    .collect(),
            )),
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
                self.noting = questions
                    .first()
                    .is_some_and(|q| q.options.is_empty() && q.allow_other);
            }
            AskBody::Form { schema_json, .. } => {
                self.fields = form_fields(schema_json);
                self.picks = vec![QuestionPick::default(); self.fields.len()];
                self.noting = form_questions(&self.fields)
                    .first()
                    .is_some_and(|q| q.options.is_empty() && q.allow_other);
            }
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
                    FieldKind::Text | FieldKind::Number { .. } | FieldKind::Many(_) => {
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
                    theme.warning(),
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
                    theme.warning()
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
                    theme.warning()
                },
                width,
            );
            push(&mut line, value, theme.text(), width);
            lines.push(line);
        }
        cursor
    }
}

/// Lines of a command or arguments shown before "… [Show All]".
const SUBJECT_LINES: usize = 6;
/// Diff lines shown before "[Full Diff]".
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
            | AskBody::Form { .. }
            | AskBody::Link { .. }
            | AskBody::Access { .. }
            | AskBody::Unanswerable { .. }
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
        ChoiceOutcome::GrantForTurn => "Allow for this turn".to_owned(),
        ChoiceOutcome::GrantForSession => "Allow for this session".to_owned(),
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
        AskBody::Access {
            read,
            write,
            network,
            hosts,
            ..
        } => access_words(read, write, *network, hosts),
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
        AskBody::Access { .. } => "Wants access to",
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
            if matches!(card.body, AskBody::Question(_) | AskBody::Form { .. }) {
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
                AskBody::Question(questions) => self.on_something_else(questions),
                AskBody::Form { .. } => self.on_something_else(&form_questions(&self.fields)),
                _ => self.box_note(card),
            }
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
        match &card.body {
            AskBody::Question(questions) => return self.question_key_boxed(card, questions, key),
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_key_boxed(card, &questions, key);
            }
            AskBody::Link { .. } => return self.link_key(card, key),
            AskBody::Unanswerable { .. } => return self.unanswerable_key(key),
            _ => {}
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
            // Esc only points at the way out, the refusal; Enter takes it.
            KeyCode::Esc => self.selected = count.saturating_sub(1),
            // F shows the whole command, arguments or diff in the box, and
            // cuts it again. The plan is in the feed, whole.
            KeyCode::Char('f') if !matches!(card.body, AskBody::Plan { .. }) => {
                self.show_all = !self.show_all;
                self.diff_scroll = 0;
            }
            _ => {}
        }
        AskAction::None
    }

    /// A click on a boxed ask.
    pub fn box_click(&mut self, card: &AskCard, spot: BoxSpot) -> AskAction {
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return AskAction::None;
        }
        match &card.body {
            AskBody::Question(questions) => return self.question_click(card, questions, spot),
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_click(card, &questions, spot);
            }
            AskBody::Link { .. } => return self.link_click(card, spot),
            AskBody::Unanswerable { .. } => {
                return if self.attach && matches!(spot, BoxSpot::Choice(0)) {
                    AskAction::Attach
                } else {
                    AskAction::None
                };
            }
            _ => {}
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
            BoxSpot::FullDiff => {
                self.show_all = !self.show_all;
                self.diff_scroll = 0;
                AskAction::None
            }
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
                    _ if matches!(
                        card.body,
                        AskBody::Question(_) | AskBody::Form { .. } | AskBody::Link { .. }
                    ) =>
                    {
                        "Sending…".to_owned()
                    }
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
                    theme.warning(),
                ));
                out.lines
                    .push(line("r resend · d discard".into(), theme.faint()));
                return out;
            }
            _ => {}
        }
        match &card.body {
            AskBody::Question(questions) => {
                return self.question_lines(card, questions, width, theme);
            }
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_lines(card, &questions, width, theme);
            }
            AskBody::Link {
                server,
                message,
                url,
            } => {
                return self.link_lines(server, message, url, width, theme);
            }
            AskBody::Unanswerable { reason } => {
                return self.unanswerable_lines(reason, width, theme);
            }
            _ => {}
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
                push(&mut more, "[Show All]", theme.muted(), width);
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
            AskBody::Access {
                reason,
                read,
                write,
                network,
                hosts,
            } => {
                for part in text::wrap(&access_words(read, write, *network, hosts), width.max(1)) {
                    out.lines.push(line(part, theme.code()));
                }
                reason.clone()
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
                // Opened ([Full Diff] or f), the whole patch shows in the
                // box, which grows to fill the chat and scrolls (wheel,
                // PgUp/PgDn) when even that is too little.
                let opened = self.show_all && lines.len() > DIFF_LINES;
                let cut = lines.len() > DIFF_LINES && !opened;
                let rows = if self.room == 0 { 36 } else { self.room };
                let fits = rows
                    .saturating_sub(DIFF_CHROME + box_choices(card).len())
                    .max(DIFF_LINES);
                let scroll = if opened {
                    self.diff_scroll.min(lines.len().saturating_sub(fits))
                } else {
                    0
                };
                let take = if cut {
                    DIFF_LINES - 1
                } else if opened {
                    fits.min(lines.len())
                } else {
                    lines.len()
                };
                for l in lines.iter().skip(scroll).take(take) {
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
                    push(&mut more, "[Full Diff]", theme.muted(), width);
                    out.spots
                        .push((out.lines.len(), (from, from + 11), BoxSpot::FullDiff));
                    out.lines.push(more);
                } else if opened {
                    let mut less = Line::default();
                    if take < lines.len() {
                        push(
                            &mut less,
                            format!(
                                "lines {}–{} of {} · wheel or pgup/pgdn  ",
                                scroll + 1,
                                scroll + take,
                                lines.len()
                            ),
                            theme.faint(),
                            width,
                        );
                    }
                    let from = text::line_width(&less);
                    push(&mut less, "[Show Less]", theme.muted(), width);
                    out.spots
                        .push((out.lines.len(), (from, from + 11), BoxSpot::FullDiff));
                    out.lines.push(less);
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
                if deny {
                    push(&mut row, "  esc", theme.faint(), width);
                }
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

/// A question's rows in the box: its options, "Something else" when it
/// takes one, then the way out when the box has one.
struct QuestionRows {
    options: usize,
    other: Option<usize>,
    out: Option<usize>,
}

impl QuestionRows {
    fn of(question: &QuestionView, way_out: bool) -> QuestionRows {
        let options = question.options.len();
        let other = question.allow_other.then_some(options);
        let numbered = options + usize::from(question.allow_other);
        QuestionRows {
            options,
            other,
            out: way_out.then_some(numbered),
        }
    }

    /// The rows digits reach: the options and "Something else".
    fn numbered(&self) -> usize {
        self.options + usize::from(self.other.is_some())
    }

    /// The last row the highlight can reach.
    fn last(&self) -> usize {
        self.out.unwrap_or(self.numbered().saturating_sub(1))
    }
}

/// Whether a boxed question or form offers a way out besides answering: a
/// tool server's form can be declined; an agent's questions only where its
/// agent takes a decline.
fn way_out(card: &AskCard) -> bool {
    match card.body {
        AskBody::Form { .. } => true,
        AskBody::Question(_) => crate::pending::declines_questions(card.kind),
        _ => false,
    }
}

/// "The shell", "The fleet, The chat", "\"tmux style\"": an answer in words,
/// empty when the question was skipped.
fn pick_words(question: &QuestionView, pick: &QuestionPick) -> String {
    if question.secret && pick.answered() {
        return "answered (hidden)".to_owned();
    }
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
    fn in_review(&self, questions: &[QuestionView]) -> bool {
        questions.len() > 1 && self.step >= questions.len()
    }

    /// The highlight is on the current question's "Something else".
    fn on_other_row(&self, questions: &[QuestionView]) -> bool {
        !self.in_review(questions)
            && questions.get(self.step).is_some_and(|question| {
                QuestionRows::of(question, false).other == Some(self.selected)
            })
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
        // What was typed stays with its question, unanswered, as a draft.
        self.drafts.resize(count, String::new());
        if self.step < count {
            self.drafts[self.step] = self.other.text().to_owned();
        }
        if step >= count && count > 1 {
            self.step = count;
            self.selected = count;
            return;
        }
        self.step = step.min(count.saturating_sub(1));
        let pick = &self.picks[self.step];
        let rows = QuestionRows::of(&questions[self.step], false);
        let draft = self.drafts[self.step].clone();
        self.selected = match (&pick.other, pick.selected.first()) {
            (Some(other), _) if !other.is_empty() => rows.other.unwrap_or(0),
            (_, Some(first)) => *first as usize,
            _ if !draft.is_empty() => rows.other.unwrap_or(0),
            _ => 0,
        };
        // A tab that is only a text field has it live from the start.
        let question = &questions[self.step];
        self.noting = question.options.is_empty() && question.allow_other;
        self.invalid = false;
        self.other = Editor::default();
        self.other.secret = questions[self.step].secret;
        match &pick.other {
            Some(other) if !other.is_empty() => self.other.insert_str(other),
            _ => self.other.insert_str(&draft),
        }
    }

    /// After a question is answered: a lone question sends; otherwise on to
    /// the next one not yet answered, or the review when none is left.
    fn question_answered(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        let count = questions.len();
        if count == 1 {
            return self.send_boxed(card, questions);
        }
        let next = (1..count)
            .map(|ahead| (self.step + ahead) % count)
            .find(|at| !self.picks[*at].answered());
        self.question_goto(questions, next.unwrap_or(count));
        AskAction::None
    }

    /// Sends the answers, or submits the form; a question that must be
    /// answered and is not (a form's required field, or any question where
    /// the agent takes none unanswered) goes there instead.
    fn send_boxed(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        if let Some(missing) = self.missing(card) {
            self.question_goto(questions, missing);
            return AskAction::None;
        }
        if matches!(card.body, AskBody::Form { .. }) {
            return self.submit_form(card);
        }
        self.send_boxed_questions(card)
    }

    /// The first question that must be answered before sending and is
    /// not: a form's required field, or with an agent that takes no
    /// question unanswered, any.
    fn missing(&self, card: &AskCard) -> Option<usize> {
        match card.body {
            AskBody::Form { .. } => self.missing_field(),
            AskBody::Question(_) if !crate::pending::skips_questions(card.kind) => {
                self.picks.iter().position(|pick| !pick.answered())
            }
            _ => None,
        }
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
    /// one of several), opens "Something else", or takes the way out.
    /// `confirm` is Enter, which also finishes a question of several picks.
    fn question_row(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        at: usize,
        confirm: bool,
    ) -> AskAction {
        let question = &questions[self.step];
        let rows = QuestionRows::of(question, way_out(card));
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
            if !self.field_takes(&typed) {
                self.invalid = true;
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
        if rows.out == Some(at) {
            return Self::choose(card, &ChoiceOutcome::Decline);
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
        // Tab and Shift+Tab move between tabs from anywhere, a field's
        // text included, skipping what is not answered.
        match key.code {
            KeyCode::Tab if several => {
                self.question_goto(questions, (self.step + 1).min(count));
                return AskAction::None;
            }
            KeyCode::BackTab if several => {
                self.question_goto(questions, self.step.saturating_sub(1));
                return AskAction::None;
            }
            _ => {}
        }
        if self.in_review(questions) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(count)
                }
                KeyCode::Left => self.question_goto(questions, count - 1),
                _ if enter && self.selected < count => self.question_goto(questions, self.selected),
                _ if enter => return self.send_boxed(card, questions),
                _ => {}
            }
            return AskAction::None;
        }
        let question = &questions[self.step];
        let rows = QuestionRows::of(question, way_out(card));
        let last = rows.last();
        // A tab that is only a text field has it live; ↓ steps out to the
        // way out and ↑ back in.
        let text_only = question.options.is_empty() && rows.other.is_some();
        if self.on_something_else(questions) {
            match key.code {
                KeyCode::Esc => {
                    let empty = self.other.text().is_empty();
                    self.other = Editor::default();
                    self.picks[self.step].other = None;
                    if let Some(draft) = self.drafts.get_mut(self.step) {
                        draft.clear();
                    }
                    self.invalid = false;
                    if !text_only {
                        self.noting = false;
                    } else if empty && let Some(out) = rows.out {
                        // An empty live field: Esc points at the way out.
                        self.noting = false;
                        self.selected = out;
                    }
                }
                KeyCode::Down if text_only && rows.out.is_some() => {
                    self.noting = false;
                    self.selected = last;
                }
                _ if enter => {
                    if self.other.text().trim().is_empty() {
                        return AskAction::None;
                    }
                    return self.question_row(card, questions, self.selected, true);
                }
                _ => {
                    self.invalid = false;
                    self.other.key(key);
                }
            }
            return AskAction::None;
        }
        match key.code {
            KeyCode::Left if several => self.question_goto(questions, self.step.saturating_sub(1)),
            KeyCode::Right if several => self.question_goto(questions, self.step + 1),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                if text_only {
                    self.noting = true;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            // The way out is not numbered: digits reach the options and
            // "Something else" only.
            KeyCode::Char(c @ '1'..='9') => {
                let at = c as usize - '1' as usize;
                if at < rows.numbered() {
                    return self.question_row(card, questions, at, false);
                }
            }
            // Esc only points at the way out; Enter takes it.
            KeyCode::Esc => {
                if let Some(out) = rows.out {
                    self.selected = out;
                }
            }
            KeyCode::Char('f')
                if question
                    .options
                    .iter()
                    .any(|option| !option.preview.is_empty()) =>
            {
                self.show_all = !self.show_all;
            }
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
                    self.send_boxed(card, questions)
                }
            }
            BoxSpot::Choice(at) => self.question_row(card, questions, at, false),
            BoxSpot::ShowAll => {
                self.show_all = !self.show_all;
                AskAction::None
            }
            _ => AskAction::None,
        }
    }

    /// The hint line's words while a boxed question has the keys.
    pub fn question_hint(&self, card: &AskCard, leader: char) -> Option<String> {
        let form = form_questions(&self.fields);
        let questions: &[QuestionView] = match &card.body {
            AskBody::Question(questions) => questions,
            AskBody::Form { .. } => &form,
            AskBody::Link { .. }
                if matches!(card.state, CardState::Open | CardState::Rejected(_)) =>
            {
                let enter = if self.selected == 2 {
                    "enter decline"
                } else {
                    "enter choose"
                };
                return Some(format!("{enter} · ctrl+x stop · ctrl+{leader} more"));
            }
            AskBody::Unanswerable { .. } => {
                return Some(if self.attach {
                    "enter open the terminal · ctrl+x stop".to_owned()
                } else {
                    "ctrl+x stop".to_owned()
                });
            }
            _ => return None,
        };
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return None;
        }
        let several = questions.len() > 1;
        let tabs = if several { " · ←/→ questions" } else { "" };
        if self.in_review(questions) {
            if let Some(at) = self.missing(card) {
                let name = question_name(&questions[at], at);
                return Some(format!("enter go to {name}{tabs} · ctrl+x stop"));
            }
            return Some(format!(
                "enter send{tabs} · ctrl+x stop · ctrl+{leader} more"
            ));
        }
        let form = matches!(card.body, AskBody::Form { .. });
        if self.on_something_else(questions) {
            // Typing: ←/→ are the text's, Tab still changes tabs.
            let enter = match (several, form) {
                (true, _) => "enter answer",
                (false, true) => "enter submit",
                (false, false) => "enter send",
            };
            let tab = if several { " · tab next" } else { "" };
            return Some(format!("{enter}{tab} · esc clear · ctrl+x stop"));
        }
        if self.on_other_row(questions) {
            // A draft kept from earlier answers on Enter.
            let enter = if self.other.text().trim().is_empty() {
                "enter to type"
            } else {
                "enter answer"
            };
            return Some(format!("{enter}{tabs} · ctrl+x stop · ctrl+{leader} more"));
        }
        if QuestionRows::of(&questions[self.step], way_out(card)).out == Some(self.selected) {
            let enter = if form { "enter decline" } else { "enter reply" };
            return Some(format!("{enter}{tabs} · ctrl+x stop · ctrl+{leader} more"));
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
    fn question_lines(
        &self,
        card: &AskCard,
        questions: &[QuestionView],
        width: usize,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let count = questions.len();
        let review = self.in_review(questions);
        let form = match &card.body {
            AskBody::Form {
                server, message, ..
            } => Some((server.as_str(), message.as_str())),
            _ => None,
        };

        // A form says who asks and why first; its fields are the tabs.
        if let Some((server, message)) = form {
            let mut top = Line::from(Span::styled("● ", theme.accent()));
            push(
                &mut top,
                format!("{server} needs details"),
                theme.text(),
                width,
            );
            out.lines.push(top);
            for part in text::wrap(message, width.max(1)) {
                let mut line = Line::default();
                push(&mut line, part, theme.text(), width);
                out.lines.push(line);
            }
            out.lines.push(Line::default());
        }

        // The head: a tab per question, then Review; or one question's name.
        let head_row = out.lines.len();
        let mut head = Line::from(Span::styled(
            if form.is_some() { "" } else { "● " },
            theme.accent(),
        ));
        if count == 1 && form.is_some() {
            // One field: no tabs, its label says it all.
        } else if count == 1 {
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
                    .push((head_row, (from, text::line_width(&head)), BoxSpot::Tab(at)));
            }
            push(&mut head, " ›", theme.faint(), width);
        }
        if !(count == 1 && form.is_some()) {
            out.lines.push(head);
            out.lines.push(Line::default());
        }

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
            // Nothing goes with a required field empty, nor, where the
            // agent takes none unanswered, a question: the line says which,
            // and Enter goes there.
            let missing = self.missing(card);
            push(
                &mut send,
                if form.is_some() {
                    "Submit"
                } else {
                    "Send answers"
                },
                if missing.is_some() {
                    theme.faint()
                } else if lit {
                    theme.bright()
                } else {
                    theme.text()
                },
                width,
            );
            if let Some(at) = missing {
                push(
                    &mut send,
                    format!(
                        " · {} {}",
                        question_name(&questions[at], at),
                        if form.is_some() {
                            "is required"
                        } else {
                            "is not answered"
                        }
                    ),
                    theme.faint(),
                    width,
                );
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(count)));
            out.lines.push(send);
            return out;
        }

        let question = &questions[self.step];
        let rows = QuestionRows::of(question, way_out(card));
        for part in text::wrap(&question.question, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.text(), width);
            out.lines.push(line);
        }
        // A form's field: required, and what it is for.
        if let Some(field) = form.and(self.fields.get(self.step)) {
            if field.required
                && let Some(last) = out.lines.last_mut()
            {
                push(last, " · required", theme.faint(), width);
            }
            for part in text::wrap(&field.description, width.max(1)) {
                let mut line = Line::default();
                push(&mut line, part, theme.faint(), width);
                out.lines.push(line);
            }
        }
        if question.multi_select {
            let mut line = Line::default();
            push(&mut line, "Select all that apply", theme.faint(), width);
            out.lines.push(line);
        }
        out.lines.push(Line::default());

        // With previews, the options and the highlighted one's preview side
        // by side where the box is wide enough, else the preview under them.
        let previews = question.options.iter().any(|o| !o.preview.is_empty());
        let side = previews && width >= SIDE_BY_SIDE;
        let left_width = if side {
            let widest = question
                .options
                .iter()
                .map(|o| text::str_width(&o.label))
                .max()
                .unwrap_or(0);
            (widest + 6).clamp(LEFT_MIN, LEFT_MAX)
        } else {
            width
        };
        let footer = if form.is_some() {
            "Decline"
        } else {
            "Reply instead"
        };
        let footer = rows.out.map(|_| footer);
        let left = self.question_rows(question, &rows, left_width, !previews, footer, theme);
        let preview = question
            .options
            .get(self.selected)
            .map(|option| option.preview.as_str())
            .filter(|preview| !preview.is_empty());
        // Tall previews are cut so the box leaves the feed room; shown whole,
        // one still stops where the box would leave the screen.
        let room = if self.room == 0 { 36 } else { self.room };
        // The rows the rest of the screen needs: the chat's header, the
        // box's edges, the hints, a little feed, and (under the options)
        // the box's own lines above the preview.
        let above = out.lines.len() + if side { 0 } else { left.lines.len() + 1 };
        let fits = room.saturating_sub(10 + above).max(3);
        let cap = if self.show_all {
            fits
        } else {
            (room / 2).saturating_sub(6).clamp(3, fits.max(3))
        };
        let control = if self.show_all {
            "[Show Less]"
        } else {
            "[Show All]"
        };
        let base = out.lines.len();
        if side {
            let right_width = width.saturating_sub(left_width + 3).max(1);
            let (right, more) = match preview {
                Some(preview) => preview_lines(preview, right_width, cap, theme),
                None => (Vec::new(), 0),
            };
            let height = left.lines.len().max(right.len() + usize::from(more > 0));
            for i in 0..height {
                let mut line = left.lines.get(i).cloned().unwrap_or_default();
                text::pad_to(&mut line, left_width);
                push(&mut line, " │ ", theme.hairline(), width);
                if let Some(part) = right.get(i) {
                    line.spans.extend(part.spans.iter().cloned());
                } else if more > 0 && i == right.len() {
                    push_more(&mut line, more, control, theme, width);
                    let end = text::line_width(&line);
                    out.spots
                        .push((base + i, (end - control.len(), end), BoxSpot::ShowAll));
                }
                out.lines.push(line);
            }
        } else {
            out.lines.extend(left.lines.iter().cloned());
            if let Some(preview) = preview {
                out.lines.push(Line::default());
                let (lines, more) = preview_lines(preview, width, cap, theme);
                out.lines.extend(lines);
                if more > 0 {
                    let mut line = Line::default();
                    push_more(&mut line, more, control, theme, width);
                    let end = text::line_width(&line);
                    out.spots.push((
                        out.lines.len(),
                        (end - control.len(), end),
                        BoxSpot::ShowAll,
                    ));
                    out.lines.push(line);
                }
            }
        }
        for (row, cols, spot) in left.spots {
            out.spots.push((base + row, cols, spot));
        }
        if let Some((row, col)) = left.cursor {
            out.cursor = Some((base + row, col));
        }
        out
    }
}

/// Boxes this wide put a question's options and a preview side by side.
const SIDE_BY_SIDE: usize = 82;
/// The options' column beside a preview.
const LEFT_MIN: usize = 22;
const LEFT_MAX: usize = 34;

/// "… 12 more lines [Show All]".
fn push_more(line: &mut Line<'static>, hidden: usize, control: &str, theme: Theme, width: usize) {
    let s = if hidden == 1 { "" } else { "s" };
    push(
        line,
        format!("… {hidden} more line{s} "),
        theme.faint(),
        width,
    );
    push(line, control, theme.muted(), width);
}

/// Whether a preview is drawn rather than written: a line with box-drawing
/// characters, two spaces between words, or a leading indent. Such a
/// preview keeps its spacing, line for line, never reflowed.
fn preformatted(preview: &str) -> bool {
    !preview.contains("```")
        && preview.lines().any(|line| {
            let body = line.trim_end();
            body.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
                || body.trim_start().starts_with(['+', '|'])
                || body.starts_with("  ")
                || body.trim().contains("  ")
        })
}

/// A preview's lines at `width`, at most `cap` of them, and how many more
/// there are. Drawn previews are kept as drawn, cut at the width; anything
/// else is markdown, with code coloured.
fn preview_lines(
    preview: &str,
    width: usize,
    cap: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, usize) {
    let lines: Vec<Line<'static>> = if preformatted(preview) {
        preview
            .trim_end()
            .lines()
            .map(|line| {
                let mut out = Line::default();
                push(&mut out, text::ellipsize(line, width), theme.text(), width);
                out
            })
            .collect()
    } else {
        crate::markdown::markdown_rows(preview.trim_end(), width, theme)
            .into_iter()
            .map(Line::from)
            .collect()
    };
    if lines.len() <= cap {
        return (lines, 0);
    }
    let keep = cap.saturating_sub(1).max(1);
    let hidden = lines.len() - keep;
    (lines.into_iter().take(keep).collect(), hidden)
}

/// The rows an opened diff leaves to the rest of the chat: the header, a
/// little feed, the box's own lines around the diff, and the hint.
const DIFF_CHROME: usize = 16;

impl AskUi {
    /// Scrolls an opened diff in the box by `lines`; false when none is
    /// open, so the feed scrolls instead.
    pub fn scroll_diff(&mut self, card: &AskCard, lines: isize) -> bool {
        if !self.show_all || !matches!(card.body, AskBody::Edit { .. }) {
            return false;
        }
        self.diff_scroll = self.diff_scroll.saturating_add_signed(lines);
        true
    }

    /// How many rows the terminal has, so previews leave the feed room.
    pub fn set_room(&mut self, rows: usize, attach: bool) {
        self.room = rows;
        self.attach = attach;
    }

    /// A question's rows at `width`: its options (with their descriptions
    /// when `descriptions`), "Something else", then the way out as a
    /// footer, when there is one.
    fn question_rows(
        &self,
        question: &QuestionView,
        rows: &QuestionRows,
        width: usize,
        descriptions: bool,
        footer: Option<&str>,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let pick = &self.picks[self.step];
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
            // "recommended" after the label where it fits, else under it.
            let mut under = None;
            if option.recommended {
                if text::line_width(&row) + 14 <= width {
                    push(&mut row, " · recommended", theme.faint(), width);
                } else {
                    let mut line = Line::from(Span::raw(" ".repeat(indent)));
                    push(&mut line, "recommended", theme.faint(), width);
                    under = Some(line);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            if let Some(line) = under {
                out.lines.push(row);
                out.spots
                    .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                row = line;
            }
            // The description after the label when it fits, else under it,
            // hanging at the label.
            let description = option.description.trim();
            if descriptions && !description.is_empty() {
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
        if let Some(at) = rows.other.filter(|_| question.options.is_empty()) {
            // A tab that is only a text field: the field itself, live.
            let lit = self.selected == at;
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.accent(),
                width,
            );
            let typed = if question.secret {
                "•".repeat(self.other.text().chars().count())
            } else {
                self.other.text().to_owned()
            };
            let col = text::line_width(&row);
            let shown = text_tail(&typed, width.saturating_sub(col + 1));
            if shown.is_empty() && !(lit && self.noting) {
                push(&mut row, "type an answer", theme.faint(), width);
            } else {
                push(&mut row, shown.clone(), theme.bright(), width);
            }
            if lit && self.noting {
                out.cursor = Some((out.lines.len(), col + text::str_width(&shown)));
                if self.invalid {
                    push(&mut row, " · a number", theme.faint(), width);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        } else if let Some(at) = rows.other {
            let lit = self.selected == at;
            let mut row = row_line(at, lit);
            let ink = if lit { theme.bright() } else { theme.text() };
            let name = "Something else";
            let typed = if question.secret {
                "•".repeat(self.other.text().chars().count())
            } else {
                self.other.text().to_owned()
            };
            let open = lit && self.noting;
            if open || !typed.is_empty() {
                push(&mut row, format!("{name}: "), ink, width);
                let col = text::line_width(&row);
                let shown = text_tail(&typed, width.saturating_sub(col + 1));
                push(&mut row, shown.clone(), ink, width);
                if open {
                    out.cursor = Some((out.lines.len(), col + text::str_width(&shown)));
                }
                if open && self.invalid {
                    push(&mut row, " · a number", theme.faint(), width);
                }
            } else {
                push(&mut row, name, ink, width);
                if lit {
                    push(&mut row, " · enter to type", theme.faint(), width);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        // The way out is a footer, not an answer: apart, unnumbered, faint
        // until highlighted.
        let (Some(footer), Some(at)) = (footer, rows.out) else {
            return out;
        };
        out.lines.push(Line::default());
        let lit = self.selected == at;
        let mut row = Line::default();
        push(
            &mut row,
            if lit { "› " } else { "  " },
            theme.accent(),
            width,
        );
        push(
            &mut row,
            footer,
            if lit { theme.bright() } else { theme.faint() },
            width,
        );
        push(&mut row, "  esc", theme.faint(), width);
        out.spots
            .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
        out.lines.push(row);
        out
    }
}

/// A form's fields asked the way questions are: a choice is single choice,
/// a yes-or-no is "Yes" / "No", several picks are checkboxes, and text or a
/// number is typed.
fn form_questions(fields: &[Field]) -> Vec<QuestionView> {
    let option = |label: &str| ui_view::OptionView {
        label: label.to_owned(),
        description: String::new(),
        preview: String::new(),
        recommended: false,
    };
    fields
        .iter()
        .map(|field| {
            let (options, multi_select): (Vec<&str>, bool) = match &field.kind {
                FieldKind::Choice(options) => (options.iter().map(String::as_str).collect(), false),
                FieldKind::Many(options) => (options.iter().map(String::as_str).collect(), true),
                FieldKind::Toggle => (vec!["Yes", "No"], false),
                FieldKind::Text | FieldKind::Number { .. } => (Vec::new(), false),
            };
            QuestionView {
                header: field.title.clone(),
                question: field.title.clone(),
                multi_select,
                allow_other: options.is_empty(),
                options: options.into_iter().map(option).collect(),
                secret: false,
            }
        })
        .collect()
}

/// "write target/, read ~/.cargo · the network (api.github.com)": what an
/// access grant asks for.
pub(crate) fn access_words(
    read: &[String],
    write: &[String],
    network: bool,
    hosts: &[String],
) -> String {
    let mut parts = Vec::new();
    if !write.is_empty() {
        parts.push(format!("write {}", write.join(", ")));
    }
    if !read.is_empty() {
        parts.push(format!("read {}", read.join(", ")));
    }
    if network {
        parts.push(if hosts.is_empty() {
            "the network".to_owned()
        } else {
            format!("the network ({})", hosts.join(", "))
        });
    }
    if parts.is_empty() {
        "more access".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn middle_cut(text: &str, max: usize) -> String {
    text::ellipsize_middle(text, max)
}

impl AskUi {
    /// The first required field of a form still empty.
    fn missing_field(&self) -> Option<usize> {
        self.fields
            .iter()
            .zip(&self.picks)
            .position(|(field, pick)| field.required && !pick.answered())
    }

    /// Whether the open field takes `typed`: a form's number must be one.
    fn field_takes(&self, typed: &str) -> bool {
        match self.fields.get(self.step).map(|field| &field.kind) {
            Some(FieldKind::Number { integer: true }) => typed.parse::<i64>().is_ok(),
            Some(FieldKind::Number { integer: false }) => typed.parse::<f64>().is_ok(),
            _ => true,
        }
    }

    /// The answer of the card's choice with `outcome`.
    fn choose(card: &AskCard, outcome: &ChoiceOutcome) -> AskAction {
        card.choices
            .iter()
            .find(|choice| &choice.outcome == outcome)
            .and_then(|choice| answer_input(card, &choice.answer, ""))
            .map_or(AskAction::None, |input| AskAction::Answer(Box::new(input)))
    }

    /// Submits the form with what was given for each field.
    fn submit_form(&mut self, card: &AskCard) -> AskAction {
        let mut content = Map::new();
        for (field, pick) in self.fields.iter().zip(&self.picks) {
            if !pick.answered() {
                continue;
            }
            let typed = pick.other.clone().unwrap_or_default();
            let at = |i: &u32| *i as usize;
            let value = match &field.kind {
                FieldKind::Choice(options) => pick
                    .selected
                    .first()
                    .and_then(|i| options.get(at(i)))
                    .map(|v| Value::String(v.clone())),
                FieldKind::Many(options) => Some(Value::Array(
                    pick.selected
                        .iter()
                        .filter_map(|i| options.get(at(i)))
                        .map(|v| Value::String(v.clone()))
                        .collect(),
                )),
                FieldKind::Toggle => Some(Value::Bool(pick.selected.first() == Some(&0))),
                FieldKind::Number { integer: true } => typed.parse::<i64>().ok().map(Value::from),
                FieldKind::Number { integer: false } => typed.parse::<f64>().ok().map(Value::from),
                FieldKind::Text => Some(Value::String(typed)),
            };
            if let Some(value) = value {
                content.insert(field.name.clone(), value);
            }
        }
        let Some(choice) = card
            .choices
            .iter()
            .find(|choice| choice.outcome == ChoiceOutcome::Submit)
        else {
            return AskAction::None;
        };
        let bytes = serde_json::to_vec(&Value::Object(content)).unwrap_or_default();
        let answer = with_form_content(&choice.answer, bytes);
        answer_input(card, &answer, "")
            .map_or(AskAction::None, |input| AskAction::Answer(Box::new(input)))
    }

    /// A link's rows: open it, say it is done, and the Decline footer.
    fn link_act(&mut self, card: &AskCard, at: usize) -> AskAction {
        self.selected = at;
        match at {
            0 => match &card.body {
                AskBody::Link { url, .. } => AskAction::OpenUrl(url.clone()),
                _ => AskAction::None,
            },
            1 => Self::choose(card, &ChoiceOutcome::OpenLink),
            _ => Self::choose(card, &ChoiceOutcome::Decline),
        }
    }

    fn link_key(&mut self, card: &AskCard, key: KeyEvent) -> AskAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(2),
            KeyCode::Char(c @ '1'..='2') => return self.link_act(card, c as usize - '1' as usize),
            KeyCode::Esc => self.selected = 2,
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                return self.link_act(card, self.selected);
            }
            _ => {}
        }
        AskAction::None
    }

    fn link_click(&mut self, card: &AskCard, spot: BoxSpot) -> AskAction {
        match spot {
            BoxSpot::Choice(at) => self.link_act(card, at),
            BoxSpot::FullDiff => self.link_act(card, 0),
            _ => AskAction::None,
        }
    }

    /// A tool server's link: who wants it and why, the link itself (a
    /// click opens it), then the ways on.
    fn link_lines(
        &self,
        server: &str,
        message: &str,
        url: &str,
        width: usize,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let mut head = Line::from(Span::styled("● ", theme.accent()));
        push(
            &mut head,
            format!("{server} needs you to sign in"),
            theme.text(),
            width,
        );
        out.lines.push(head);
        out.lines.push(Line::default());
        for part in text::wrap(message, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.text(), width);
            out.lines.push(line);
        }
        let mut link = Line::default();
        let shown = middle_cut(url, width);
        push(&mut link, shown.clone(), theme.code(), width);
        out.spots.push((
            out.lines.len(),
            (0, text::str_width(&shown)),
            BoxSpot::FullDiff,
        ));
        out.lines.push(link);
        out.lines.push(Line::default());
        for (at, words) in ["Open the link", "I'm signed in"].iter().enumerate() {
            let lit = self.selected == at;
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.accent(),
                width,
            );
            let ink = if lit { theme.bright() } else { theme.text() };
            push(&mut row, format!("{}. {words}", at + 1), ink, width);
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        out.lines.push(Line::default());
        let lit = self.selected == 2;
        let mut row = Line::default();
        push(
            &mut row,
            if lit { "› " } else { "  " },
            theme.accent(),
            width,
        );
        push(
            &mut row,
            "Decline",
            if lit { theme.bright() } else { theme.faint() },
            width,
        );
        push(&mut row, "  esc", theme.faint(), width);
        out.spots
            .push((out.lines.len(), (0, width), BoxSpot::Choice(2)));
        out.lines.push(row);
        out
    }

    /// What this client cannot answer: Enter opens the agent's own
    /// terminal where that is possible; otherwise only Ctrl+X ends it.
    fn unanswerable_key(&mut self, key: KeyEvent) -> AskAction {
        if self.attach && key.code == KeyCode::Enter {
            return AskAction::Attach;
        }
        AskAction::None
    }

    fn unanswerable_lines(&self, reason: &str, width: usize, theme: Theme) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let mut head = Line::from(Span::styled("● ", theme.accent()));
        push(&mut head, "Can't answer this here", theme.text(), width);
        out.lines.push(head);
        out.lines.push(Line::default());
        let reason = if reason.is_empty() {
            "The agent is showing something this build cannot read."
        } else {
            reason
        };
        for part in text::wrap(reason, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.muted(), width);
            out.lines.push(line);
        }
        if self.attach {
            out.lines.push(Line::default());
            let mut row = Line::default();
            push(&mut row, "› ", theme.accent(), width);
            push(&mut row, "Open Claude's terminal", theme.bright(), width);
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(0)));
            out.lines.push(row);
        }
        out
    }
}
