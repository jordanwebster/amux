//! A new agent's settings before it exists: its name, which agent, its
//! model, effort and mode, where it works and on which machine. The
//! flyover that changes one of them, which a running chat also uses for its
//! model and effort. And the short form that starts an agent to be used in
//! its own terminal.
//!
//! A person sees two agents, Claude and Codex, everywhere. Where they chat
//! with it decides what runs behind it: in amux's chat, headless Claude or
//! Codex's app server; in the agents' own terminals, Claude in its terminal,
//! or Codex's app server with a terminal attached that runs its own
//! interface on the same thread.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ui_state::FleetState;
use ui_view::ModeValue;
use wire::{ClaudeCreateConfig, CodexCreateConfig, CreateAgentRequest, Input, Kind, Presence};

use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// Where a person chats with their agents: a preference for this
/// installation, asked once (in onboarding, when it exists) and changed in
/// settings. It decides what a new agent runs on and how starting it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatIn {
    /// In amux's own chat.
    #[default]
    Amux,
    /// In each agent's own terminal, attached on start.
    Terminal,
}

/// The two agents a person chooses between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

const AGENTS: [Agent; 2] = [Agent::Claude, Agent::Codex];

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    /// What runs it where the person chats.
    fn kind(self, chat_in: ChatIn) -> Kind {
        match (self, chat_in) {
            (Agent::Claude, ChatIn::Amux) => Kind::ClaudeSdk,
            (Agent::Claude, ChatIn::Terminal) => Kind::ClaudePty,
            (Agent::Codex, _) => Kind::Codex,
        }
    }

    fn of(kind: Kind) -> Agent {
        match kind {
            Kind::Codex => Agent::Codex,
            _ => Agent::Claude,
        }
    }
}

/// Models offered before an agent exists to list its own: Claude's
/// aliases, and Codex's current models.
const CLAUDE_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
const CODEX_MODELS: [&str; 2] = ["gpt-5-codex", "gpt-5"];
const CLAUDE_EFFORTS: [&str; 3] = ["low", "medium", "high"];
const CODEX_EFFORTS: [&str; 4] = ["minimal", "low", "medium", "high"];

/// The modes shift+tab moves through: those that still ask before acting.
const CLAUDE_MODES: [&str; 3] = ["default", "acceptEdits", "plan"];
const CODEX_MODES: [(&str, &str, &str); 3] = [
    ("auto", "on-request", "workspace-write"),
    ("read-only", "on-request", "read-only"),
    ("full-access", "never", "danger-full-access"),
];

/// One of the settings, as the composer's edge shows it and a flyover
/// changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Name,
    Kind,
    Model,
    Effort,
    Mode,
    Folder,
    Worktree,
    Host,
}

/// What a new agent will be.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// None: named for the person automatically.
    pub name: Option<String>,
    pub agent: Agent,
    pub chat_in: ChatIn,
    /// None runs the provider's default.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<ModeValue>,
    /// Where it works, as the request names it.
    pub folder: String,
    pub host: Vec<u8>,
    /// Start it in a new worktree of the folder's repository.
    pub worktree: bool,
}

impl Setup {
    /// From home: the defaults. No memory per folder: that would start a
    /// notion of projects nobody has designed.
    pub fn defaults(chat_in: ChatIn, working_dir: &str, local_host: &[u8]) -> Setup {
        Setup {
            name: None,
            agent: Agent::Claude,
            chat_in,
            model: None,
            effort: None,
            mode: None,
            folder: working_dir.to_owned(),
            host: local_host.to_vec(),
            worktree: false,
        }
    }

    /// A running agent's settings, to start a sibling where the person
    /// chats now. The agent's own terminal sets model, effort and mode, so
    /// those are copied only for amux's chat.
    pub fn sibling(mut self, kind: Kind, chat_in: ChatIn) -> Setup {
        self.agent = Agent::of(kind);
        self.chat_in = chat_in;
        if chat_in == ChatIn::Terminal {
            self.model = None;
            self.effort = None;
            self.mode = None;
        }
        self
    }

    /// What runs it, as the request names it.
    pub fn kind(&self) -> Kind {
        self.agent.kind(self.chat_in)
    }

    fn claude(&self) -> bool {
        self.agent == Agent::Claude
    }

    /// The composer's edge, in groups: its name, what runs it, where, and
    /// on which machine. Without a name the first item invites one; "new
    /// worktree" shows only when it is on.
    pub fn edge(&self, fleet: &FleetState) -> Vec<Vec<(Item, String)>> {
        let mut model = self
            .model
            .as_deref()
            .map_or_else(|| "Default model".to_owned(), model_label);
        if let Some(effort) = &self.effort {
            model.push_str(&format!(" ({effort})"));
        }
        let mode = self
            .mode
            .as_ref()
            .map_or_else(|| "Default".to_owned(), crate::words::mode_name);
        let mut place = vec![(Item::Folder, text::tilde(&self.folder))];
        if self.worktree {
            place.push((Item::Worktree, "new worktree".to_owned()));
        }
        let name = self.name.clone().unwrap_or_else(|| "+ Name".to_owned());
        vec![
            vec![(Item::Name, name)],
            vec![
                (Item::Kind, self.agent.name().to_owned()),
                (Item::Model, model),
                (Item::Mode, mode),
            ],
            place,
            vec![(Item::Host, host_name(fleet, &self.host))],
        ]
    }

    /// The choices for one setting, the current one marked.
    pub fn choices(&self, item: Item, fleet: &FleetState) -> Vec<Choice> {
        let choice = |label: String, value: String, current: bool| Choice {
            label,
            detail: String::new(),
            value,
            current,
            disabled: false,
        };
        match item {
            Item::Kind => AGENTS
                .iter()
                .enumerate()
                .map(|(at, agent)| choice(agent.name().to_owned(), at.to_string(), *agent == self.agent))
                .collect(),
            Item::Model => {
                let models: &[&str] = if self.claude() {
                    &CLAUDE_MODELS
                } else {
                    &CODEX_MODELS
                };
                std::iter::once(choice(
                    "Default model".into(),
                    String::new(),
                    self.model.is_none(),
                ))
                .chain(models.iter().map(|model| {
                    choice(
                        model_label(model),
                        (*model).to_owned(),
                        self.model.as_deref() == Some(*model),
                    )
                }))
                .collect()
            }
            Item::Effort => {
                let efforts: &[&str] = if self.claude() {
                    &CLAUDE_EFFORTS
                } else {
                    &CODEX_EFFORTS
                };
                std::iter::once(choice(
                    "Default effort".into(),
                    String::new(),
                    self.effort.is_none(),
                ))
                .chain(efforts.iter().map(|effort| {
                    choice(
                        (*effort).to_owned(),
                        (*effort).to_owned(),
                        self.effort.as_deref() == Some(*effort),
                    )
                }))
                .collect()
            }
            Item::Mode => self
                .modes()
                .into_iter()
                .enumerate()
                .map(|(at, mode)| {
                    let current = match &self.mode {
                        Some(chosen) => *chosen == mode,
                        None => at == 0,
                    };
                    choice(crate::words::mode_name(&mode), at.to_string(), current)
                })
                .collect(),
            Item::Folder => recent_folders(fleet, &self.host, &self.folder)
                .into_iter()
                .map(|folder| {
                    let current = folder == self.folder;
                    choice(text::tilde(&folder), folder, current)
                })
                .collect(),
            Item::Host => hosts(fleet, &self.host),
            Item::Name | Item::Worktree => Vec::new(),
        }
    }

    /// The modes shift+tab and the mode flyover move through.
    fn modes(&self) -> Vec<ModeValue> {
        if self.claude() {
            CLAUDE_MODES
                .iter()
                .map(|mode| ModeValue::Claude((*mode).to_owned()))
                .collect()
        } else {
            CODEX_MODES
                .iter()
                .map(|(preset, approval, sandbox)| ModeValue::Codex {
                    preset: Some((*preset).to_owned()),
                    approval_policy: (*approval).to_owned(),
                    sandbox: (*sandbox).to_owned(),
                })
                .collect()
        }
    }

    /// Shift+Tab: the next mode.
    pub fn next_mode(&mut self) {
        let modes = self.modes();
        let at = self
            .mode
            .as_ref()
            .and_then(|mode| modes.iter().position(|m| m == mode))
            .unwrap_or(0);
        self.mode = modes.get((at + 1) % modes.len().max(1)).cloned();
    }

    /// Takes a picked value for `item`. Another agent starts its model,
    /// effort and mode afresh: they do not carry across providers.
    pub fn pick(&mut self, item: Item, value: &str) {
        let some = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        match item {
            Item::Name => self.name = some(value.trim()),
            Item::Kind => {
                if let Some(agent) = value.parse::<usize>().ok().and_then(|at| AGENTS.get(at))
                    && *agent != self.agent
                {
                    self.agent = *agent;
                    self.model = None;
                    self.effort = None;
                    self.mode = None;
                }
            }
            Item::Model => self.model = some(value),
            Item::Effort => self.effort = some(value),
            Item::Mode => {
                self.mode = value
                    .parse::<usize>()
                    .ok()
                    .and_then(|at| self.modes().get(at).cloned());
            }
            Item::Folder => {
                if !value.trim().is_empty() {
                    self.folder = expand_tilde(value.trim());
                }
            }
            Item::Host => self.host = value.as_bytes().to_vec(),
            Item::Worktree => self.worktree = !self.worktree,
        }
    }

    /// The create request, with the first prompt when there is one.
    pub fn request(&self, agent_id: Vec<u8>, prompt: Option<Input>) -> CreateAgentRequest {
        let (approval_policy, sandbox_policy) = match &self.mode {
            Some(ModeValue::Codex {
                approval_policy,
                sandbox,
                ..
            }) => (Some(approval_policy.clone()), Some(sandbox.clone())),
            _ => (None, None),
        };
        let permission_mode = match &self.mode {
            Some(ModeValue::Claude(mode)) => Some(mode.clone()),
            _ => None,
        };
        let config = if self.claude() {
            wire::create_agent_request::Config::Claude(ClaudeCreateConfig {
                args: Vec::new(),
                model: self.model.clone(),
                permission_mode,
                effort: self.effort.clone(),
            })
        } else {
            wire::create_agent_request::Config::Codex(CodexCreateConfig {
                model: self.model.clone(),
                approval_policy,
                sandbox_policy,
                effort: self.effort.clone(),
                resume_thread_id: None,
            })
        };
        CreateAgentRequest {
            agent_id,
            host_id: Some(self.host.clone()),
            name: self.name.clone(),
            cwd: self.folder.clone(),
            kind: self.kind() as i32,
            initial_prompt: prompt,
            config: Some(config),
            ..CreateAgentRequest::default()
        }
    }
}

/// Stand-in until the wire can ask for one: the agent would start in a new
/// worktree on this branch, named from its name or the first words of its
/// prompt. The request cannot carry it yet, so the agent starts in the
/// folder itself and the caller says so.
pub fn worktree_stand_in(setup: &Setup, prompt: &str) -> Option<String> {
    if !setup.worktree {
        return None;
    }
    let source = setup.name.as_deref().unwrap_or(prompt);
    let words: Vec<String> = source
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
        .map(str::to_lowercase)
        .collect();
    Some(if words.is_empty() {
        "agent".to_owned()
    } else {
        words.join("-")
    })
}

/// A model by the name a person reads: an alias capitalised, an id tidied.
fn model_label(model: &str) -> String {
    if model.chars().all(|c| c.is_ascii_lowercase()) {
        let mut chars = model.chars();
        return chars
            .next()
            .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default();
    }
    crate::words::model_name(model)
}

fn host_name(fleet: &FleetState, host: &[u8]) -> String {
    fleet
        .host(host)
        .map(|host| host.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// The hosts an agent can start on, the chosen one first; one away cannot
/// take a new agent, so it is listed but not picked.
fn hosts(fleet: &FleetState, chosen: &[u8]) -> Vec<Choice> {
    let mut hosts: Vec<&wire::HostEntry> = fleet
        .hosts()
        .filter(|host| host.trust() == wire::Trust::Trusted || host.host_id == chosen)
        .collect();
    hosts.sort_by_key(|host| host.host_id != chosen);
    hosts
        .into_iter()
        .map(|host| {
            let (detail, away) = match host.presence() {
                Presence::Online | Presence::Unspecified => (String::new(), false),
                Presence::Away => ("away".to_owned(), true),
                Presence::Offline => ("offline".to_owned(), true),
            };
            Choice {
                label: host_name(fleet, &host.host_id),
                detail,
                value: String::from_utf8_lossy(&host.host_id).into_owned(),
                current: host.host_id == chosen,
                disabled: away && host.host_id != chosen,
            }
        })
        .collect()
}

/// The folders agents on `host` work in, the most recently busy first,
/// with `current` among them.
fn recent_folders(fleet: &FleetState, host: &[u8], current: &str) -> Vec<String> {
    let mut agents: Vec<&wire::Agent> = fleet
        .agents()
        .filter(|agent| agent.host_id == host && !agent.cwd.is_empty())
        .collect();
    agents.sort_by_key(|agent| std::cmp::Reverse(agent.last_activity_ms));
    let mut folders = vec![current.to_owned()];
    for agent in agents {
        if !folders.contains(&agent.cwd) {
            folders.push(agent.cwd.clone());
        }
    }
    folders
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", std::env::var("HOME").unwrap_or_default())
        }
        _ => path.to_owned(),
    }
}

/// One choice of a flyover.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    /// Faint after the label: "away", "default".
    pub detail: String,
    /// What picking it sets.
    pub value: String,
    pub current: bool,
    /// Shown but not picked: a host that is away.
    pub disabled: bool,
}

/// What a key on a flyover did.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    None,
    Close,
    /// The value picked, or typed text (a name, a folder's path).
    Value(String),
}

/// What a flyover holds: a list to pick from, which long lists filter as
/// you type and a folder's also takes as a typed path; or a single text
/// field, for a name.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    List,
    Folder,
    Text,
}

/// The panel that changes one setting: it opens upward from the setting's
/// place on the composer's edge, anchored over it, with the chat still
/// visible around it.
#[derive(Debug)]
pub struct Picker {
    pub item: Item,
    title: String,
    choices: Vec<Choice>,
    selected: usize,
    field: Editor,
    shape: Shape,
}

/// Lists this long filter as you type.
const FILTERS_FROM: usize = 6;
/// Choices shown at once.
const SHOWN: usize = 8;
/// The narrowest a flyover is drawn, inside its border.
const FLYOVER_MIN: usize = 24;

impl Picker {
    pub fn new(item: Item, title: &str, choices: Vec<Choice>) -> Picker {
        let selected = choices.iter().position(|c| c.current).unwrap_or(0);
        Picker {
            item,
            title: title.to_owned(),
            choices,
            selected,
            field: Editor::default(),
            shape: if item == Item::Folder {
                Shape::Folder
            } else {
                Shape::List
            },
        }
    }

    /// A single text field, holding `text` to begin with.
    pub fn text(item: Item, title: &str, text: &str) -> Picker {
        let mut field = Editor::default();
        field.set(text, Vec::new());
        Picker {
            item,
            title: title.to_owned(),
            choices: Vec::new(),
            selected: 0,
            field,
            shape: Shape::Text,
        }
    }

    fn filtering(&self) -> bool {
        self.shape != Shape::List || self.choices.len() >= FILTERS_FROM
    }

    /// The choices the filter keeps, and a typed path as the last.
    fn shown(&self) -> Vec<Choice> {
        if self.shape == Shape::Text {
            return Vec::new();
        }
        let needle = self.field.text().trim().to_lowercase();
        let mut shown: Vec<Choice> = self
            .choices
            .iter()
            .filter(|c| needle.is_empty() || c.label.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        if self.shape == Shape::Folder
            && !needle.is_empty()
            && !shown.iter().any(|c| c.label.to_lowercase() == needle)
        {
            shown.push(Choice {
                label: format!("Use {}", self.field.text().trim()),
                detail: String::new(),
                value: self.field.text().trim().to_owned(),
                current: false,
                disabled: false,
            });
        }
        shown
    }

    /// The next choice from `from` in `step`'s direction that can be
    /// picked, or `from` when there is none.
    fn step(shown: &[Choice], from: usize, step: isize) -> usize {
        let mut at = from as isize;
        loop {
            at += step;
            if at < 0 || at as usize >= shown.len() {
                return from;
            }
            if !shown[at as usize].disabled {
                return at as usize;
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Pick {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.shape == Shape::Text {
            return match key.code {
                KeyCode::Esc => Pick::Close,
                KeyCode::Enter => Pick::Value(self.field.text().trim().to_owned()),
                _ => {
                    if !ctrl || matches!(key.code, KeyCode::Char('a' | 'e' | 'k' | 'u' | 'w')) {
                        self.field.key(key);
                    }
                    Pick::None
                }
            };
        }
        let shown = self.shown();
        let selected = self.selected.min(shown.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc => return Pick::Close,
            KeyCode::Up => self.selected = Self::step(&shown, selected, -1),
            KeyCode::Down => self.selected = Self::step(&shown, selected, 1),
            KeyCode::Char('k') if !self.filtering() => {
                self.selected = Self::step(&shown, selected, -1)
            }
            KeyCode::Char('j') if !self.filtering() => {
                self.selected = Self::step(&shown, selected, 1)
            }
            KeyCode::Char(c @ '1'..='9') if !self.filtering() => {
                let at = c as usize - '1' as usize;
                if let Some(choice) = shown.get(at).filter(|c| !c.disabled) {
                    return Pick::Value(choice.value.clone());
                }
            }
            // A folder: the best match fills the field, to go on typing.
            KeyCode::Tab if self.shape == Shape::Folder => {
                if let Some(choice) = shown.get(selected).filter(|c| !c.label.starts_with("Use ")) {
                    let label = choice.label.clone();
                    self.field.set(&label, Vec::new());
                    self.selected = 0;
                }
            }
            KeyCode::Enter => {
                if let Some(choice) = shown.get(selected).filter(|c| !c.disabled) {
                    return Pick::Value(choice.value.clone());
                }
            }
            _ if self.filtering() && !ctrl => {
                self.field.key(key);
                self.selected = 0;
            }
            _ => {}
        }
        Pick::None
    }

    /// The flyover's lines inside its border, which choice each line shows,
    /// and the cursor's (column, line) when a field has the keys.
    #[allow(clippy::type_complexity)]
    fn body(
        &self,
        theme: Theme,
    ) -> (Vec<Line<'static>>, Vec<Option<usize>>, Option<(usize, usize)>) {
        let wide = usize::MAX / 4;
        let mut lines = Vec::new();
        let mut rows = Vec::new();
        let mut cursor = None;
        if self.filtering() {
            // A name's field is the prompt; a filter sits above choices whose
            // own pointer is the one that moves.
            let mark = if self.shape == Shape::Text { "› " } else { "  " };
            let mut field = Line::from(Span::styled(mark, theme.muted()));
            let at = text::line_width(&field);
            if self.field.is_empty() {
                let hint = match self.shape {
                    Shape::Text => "named automatically",
                    Shape::Folder => "type a path or filter",
                    Shape::List => "type to filter",
                };
                cursor = Some((at, 0));
                push(&mut field, hint, theme.faint(), wide);
            } else {
                cursor = Some((at + self.field.cursor_chars(), 0));
                push(&mut field, self.field.text(), theme.text(), wide);
            }
            lines.push(field);
            rows.push(None);
        }
        if self.shape == Shape::Text {
            return (lines, rows, cursor);
        }
        let shown = self.shown();
        let selected = self.selected.min(shown.len().saturating_sub(1));
        let from = selected.saturating_sub(SHOWN - 1);
        for (at, choice) in shown.iter().enumerate().skip(from).take(SHOWN) {
            let lit = at == selected && !choice.disabled;
            let mut row = Line::from(Span::styled(if lit { "› " } else { "  " }, theme.accent()));
            if !self.filtering() {
                push(&mut row, format!("{}. ", at + 1), theme.muted(), wide);
            }
            let ink = if choice.disabled {
                theme.faint()
            } else if lit {
                theme.bright()
            } else {
                theme.text()
            };
            push(&mut row, choice.label.clone(), ink, wide);
            if !choice.detail.is_empty() {
                push(&mut row, format!(" · {}", choice.detail), theme.faint(), wide);
            }
            if choice.current {
                push(&mut row, " ✓", theme.faint(), wide);
            }
            lines.push(row);
            rows.push(Some(at));
        }
        if shown.is_empty() {
            let mut none = Line::from(Span::raw("  "));
            push(&mut none, "nothing matches", theme.faint(), wide);
            lines.push(none);
            rows.push(None);
        }
        (lines, rows, cursor)
    }

    /// A click on the `at`th choice shown.
    pub fn click(&mut self, at: usize) -> Pick {
        match self.shown().get(at) {
            Some(choice) if !choice.disabled => Pick::Value(choice.value.clone()),
            _ => Pick::None,
        }
    }

    /// The flyover in its hairline border, its title on the top edge, at
    /// most `room` columns wide. Returns its lines and the cursor's (column,
    /// line) within them.
    #[allow(clippy::type_complexity)]
    fn panel(
        &self,
        room: usize,
        theme: Theme,
    ) -> (Vec<Line<'static>>, Vec<Option<usize>>, Option<(usize, usize)>) {
        let (body, rows, cursor) = self.body(theme);
        let inner = body
            .iter()
            .map(text::line_width)
            .chain([text::str_width(&self.title) + 3])
            .max()
            .unwrap_or(0)
            .max(FLYOVER_MIN)
            .min(room.saturating_sub(4).max(8));
        let edge = theme.hairline();
        let mut top = Line::from(Span::styled("╭─ ", edge));
        push(&mut top, self.title.clone(), theme.muted(), inner + 4);
        push(&mut top, " ", edge, inner + 4);
        let used = text::line_width(&top);
        push(&mut top, "─".repeat((inner + 3).saturating_sub(used)), edge, inner + 4);
        push(&mut top, "╮", edge, inner + 4);
        let mut lines = vec![top];
        for line in body {
            let mut row = Line::from(Span::styled("│ ", edge));
            let mut clipped = Line::default();
            for span in line.spans {
                push(&mut clipped, &span.content, span.style, inner);
            }
            row.spans.extend(clipped.spans);
            text::pad_to(&mut row, inner + 2);
            push(&mut row, " │", edge, inner + 4);
            lines.push(row);
        }
        let mut bottom = Line::from(Span::styled("╰", edge));
        push(&mut bottom, "─".repeat(inner + 2), edge, inner + 4);
        push(&mut bottom, "╯", edge, inner + 4);
        lines.push(bottom);
        let rows = std::iter::once(None).chain(rows).chain([None]).collect();
        (
            lines,
            rows,
            cursor.map(|(col, line)| ((col + 2).min(inner + 1), line + 1)),
        )
    }

    /// The keys while it is open, as (key, action) pairs.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.shape {
            Shape::Text => vec![("enter", "save"), ("esc", "back")],
            Shape::Folder => vec![
                ("enter", "pick"),
                ("tab", "fill"),
                ("↑↓", "move"),
                ("esc", "back"),
            ],
            Shape::List => vec![("enter", "pick"), ("↑↓", "move"), ("esc", "back")],
        }
    }

    pub fn hint(&self) -> String {
        self.hints()
            .iter()
            .map(|(key, action)| format!("{key} {action}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A flyover as drawn: where it is, where its choices are, and where the
/// cursor goes when a field has the keys.
#[derive(Clone, Debug, Default)]
pub struct Flyover {
    pub rect: Rect,
    /// Each choice's screen row and the index a click picks.
    pub choices: Vec<(u16, usize)>,
    pub cursor: Option<Position>,
}

impl Flyover {
    /// The choice at a screen position, when the flyover covers it.
    pub fn choice_at(&self, x: u16, y: u16) -> Option<usize> {
        let inside = (self.rect.x..self.rect.x + self.rect.width).contains(&x);
        self.choices
            .iter()
            .find(|(row, _)| inside && *row == y)
            .map(|(_, at)| *at)
    }

    pub fn covers(&self, x: u16, y: u16) -> bool {
        (self.rect.x..self.rect.x + self.rect.width).contains(&x)
            && (self.rect.y..self.rect.y + self.rect.height).contains(&y)
    }
}

/// Draws `picker` as a flyover whose foot rests on the row above `above`,
/// its left edge under `anchor` (the setting's column) where it fits, inside
/// `area`.
pub fn draw_flyover(
    paint: &mut Paint<'_>,
    picker: &Picker,
    anchor: u16,
    above: u16,
    area: Rect,
    theme: Theme,
) -> Flyover {
    // Two columns of margin either side, like everything else.
    let room = usize::from(area.width).saturating_sub(4);
    let (lines, rows, cursor) = picker.panel(room, theme);
    let width = lines.iter().map(text::line_width).max().unwrap_or(0) as u16;
    let height = (lines.len() as u16).min(above.saturating_sub(area.y));
    if height == 0 || width == 0 {
        return Flyover::default();
    }
    let right = area.x + area.width.saturating_sub(2);
    // The border sits two columns left of the setting's words, so the
    // choices line up under them.
    let x = anchor
        .saturating_sub(2)
        .min(right.saturating_sub(width))
        .max(area.x + 2);
    let y = above - height;
    let skip = lines.len() - usize::from(height);
    let rect = Rect {
        x,
        y,
        width: width.min(right.saturating_sub(x)),
        height,
    };
    paint.render_widget(Clear, rect);
    paint.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        rect,
    );
    Flyover {
        rect,
        choices: rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .filter_map(|(line, at)| at.map(|at| (y + (line - skip) as u16, at)))
            .collect(),
        cursor: cursor
            .filter(|(_, line)| *line >= skip)
            .map(|(col, line)| Position::new(x + col as u16, y + (line - skip) as u16)),
    }
}

/// A field of the form for an agent used in its own terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Kind,
    Folder,
    Host,
    Worktree,
}

const FIELDS: [Field; 5] = [
    Field::Name,
    Field::Kind,
    Field::Folder,
    Field::Host,
    Field::Worktree,
];

/// What a click on the form lands on.
#[derive(Clone, Debug, PartialEq)]
pub enum FormHit {
    Field(Field),
    Agent(usize),
    Host(String),
    Folder(String),
    Worktree,
}

/// What a key on the form did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormOutcome {
    None,
    Start,
    Close,
}

/// The form that starts an agent to be used in its own terminal: no
/// prompt and no model, effort or mode, which that terminal sets itself.
/// Every choice sits in the form, changed where it is.
#[derive(Debug)]
pub struct Form {
    pub field: Field,
    name: Editor,
    folder: Editor,
}

/// The form's label column.
const LABEL: usize = 10;

impl Form {
    pub fn new(setup: &Setup) -> Form {
        let mut name = Editor::default();
        name.set(setup.name.as_deref().unwrap_or(""), Vec::new());
        let mut folder = Editor::default();
        folder.set(&text::tilde(&setup.folder), Vec::new());
        Form {
            field: Field::Name,
            name,
            folder,
        }
    }

    /// The typed name and folder into `setup`, before it starts.
    pub fn apply(&self, setup: &mut Setup) {
        setup.pick(Item::Name, self.name.text());
        setup.pick(Item::Folder, self.folder.text());
    }

    /// The folders matching what is typed, the typed one first when it is
    /// one of them.
    fn folders(&self, setup: &Setup, fleet: &FleetState) -> Vec<String> {
        let typed = self.folder.text().trim().to_lowercase();
        recent_folders(fleet, &setup.host, &setup.folder)
            .into_iter()
            .map(|folder| text::tilde(&folder))
            .filter(|folder| typed.is_empty() || folder.to_lowercase().contains(&typed))
            .collect()
    }

    fn move_field(&mut self, step: isize) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let next = (at + step).clamp(0, FIELDS.len() as isize - 1) as usize;
        self.field = FIELDS[next];
    }

    pub fn key(&mut self, setup: &mut Setup, fleet: &FleetState, key: KeyEvent) -> FormOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let typing = matches!(self.field, Field::Name | Field::Folder);
        match key.code {
            KeyCode::Esc => return FormOutcome::Close,
            KeyCode::Enter => {
                self.apply(setup);
                return FormOutcome::Start;
            }
            KeyCode::Up | KeyCode::BackTab => self.move_field(-1),
            KeyCode::Down => self.move_field(1),
            KeyCode::Tab if self.field == Field::Folder => {
                let folders = self.folders(setup, fleet);
                match folders.first() {
                    Some(top) if *top != self.folder.text().trim() => {
                        self.folder.set(top, Vec::new())
                    }
                    _ => self.move_field(1),
                }
            }
            KeyCode::Tab => self.move_field(1),
            KeyCode::Char('k') if !typing && !ctrl => self.move_field(-1),
            KeyCode::Char('j') if !typing && !ctrl => self.move_field(1),
            KeyCode::Left | KeyCode::Right if !typing => {
                let step = if key.code == KeyCode::Left { -1 } else { 1 };
                match self.field {
                    Field::Kind => {
                        let at = AGENTS.iter().position(|a| *a == setup.agent).unwrap_or(0) as isize;
                        let next = (at + step).rem_euclid(AGENTS.len() as isize);
                        setup.pick(Item::Kind, &next.to_string());
                    }
                    Field::Host => {
                        let choices = hosts(fleet, &setup.host);
                        let at = choices.iter().position(|c| c.current).unwrap_or(0);
                        let next = Picker::step(&choices, at, step);
                        if let Some(choice) = choices.get(next) {
                            setup.pick(Item::Host, &choice.value);
                        }
                    }
                    Field::Worktree => setup.pick(Item::Worktree, ""),
                    _ => {}
                }
            }
            KeyCode::Char(' ') if self.field == Field::Worktree => {
                setup.pick(Item::Worktree, "")
            }
            _ if self.field == Field::Name => {
                self.name.key(key);
            }
            _ if self.field == Field::Folder => {
                self.folder.key(key);
            }
            _ => {}
        }
        FormOutcome::None
    }

    /// A click on the form.
    pub fn click(&mut self, setup: &mut Setup, hit: FormHit) {
        match hit {
            FormHit::Field(field) => self.field = field,
            FormHit::Agent(at) => {
                self.field = Field::Kind;
                setup.pick(Item::Kind, &at.to_string());
            }
            FormHit::Host(value) => {
                self.field = Field::Host;
                setup.pick(Item::Host, &value);
            }
            FormHit::Folder(folder) => {
                self.field = Field::Folder;
                self.folder.set(&folder, Vec::new());
            }
            FormHit::Worktree => {
                self.field = Field::Worktree;
                setup.pick(Item::Worktree, "");
            }
        }
    }

    /// Its lines from the left margin, each with its click targets as
    /// (from, to, hit) columns, and the cursor's (column, line) when a text
    /// field has the keys.
    #[allow(clippy::type_complexity)]
    pub fn lines(
        &self,
        setup: &Setup,
        fleet: &FleetState,
        width: usize,
        theme: Theme,
    ) -> (
        Vec<(Line<'static>, Vec<(usize, usize, FormHit)>)>,
        Option<(usize, usize)>,
    ) {
        let mut out = Vec::new();
        let mut cursor = None;
        for field in FIELDS {
            let focused = self.field == field;
            let mut line = Line::from(Span::styled(
                if focused { "› " } else { "  " },
                theme.accent(),
            ));
            let label = match field {
                Field::Name => "Name",
                Field::Kind => "Agent",
                Field::Folder => "Folder",
                Field::Host => "Host",
                Field::Worktree => "Worktree",
            };
            let label_ink = if focused { theme.bright() } else { theme.muted() };
            push(&mut line, format!("{label:<LABEL$}"), label_ink, width);
            let mut spots = vec![(0, text::line_width(&line), FormHit::Field(field))];
            let mut choice = |line: &mut Line<'static>, words: String, on: bool, off: bool, hit: FormHit| {
                let from = text::line_width(line);
                let mark = if on { "● " } else { "○ " };
                let ink = if off {
                    theme.faint()
                } else if on {
                    theme.bright()
                } else {
                    theme.text()
                };
                if !off {
                    push(line, mark, if on { theme.accent() } else { theme.muted() }, width);
                }
                push(line, words, ink, width);
                spots.push((from, text::line_width(line), hit));
                push(line, "   ", theme.muted(), width);
            };
            match field {
                Field::Name => {
                    let at = text::line_width(&line);
                    if self.name.is_empty() {
                        push(&mut line, "named automatically", theme.faint(), width);
                        if focused {
                            cursor = Some((at, out.len()));
                        }
                    } else {
                        push(&mut line, self.name.text(), theme.text(), width);
                        if focused {
                            cursor = Some((at + self.name.cursor_chars(), out.len()));
                        }
                    }
                }
                Field::Kind => {
                    for (at, agent) in AGENTS.iter().enumerate() {
                        choice(
                            &mut line,
                            agent.name().to_owned(),
                            *agent == setup.agent,
                            false,
                            FormHit::Agent(at),
                        );
                    }
                }
                Field::Folder => {
                    let at = text::line_width(&line);
                    push(&mut line, self.folder.text(), theme.text(), width);
                    if focused {
                        cursor = Some((at + self.folder.cursor_chars(), out.len()));
                    }
                }
                Field::Host => {
                    for host in hosts(fleet, &setup.host) {
                        let words = if host.detail.is_empty() {
                            host.label.clone()
                        } else {
                            format!("{} · {}", host.label, host.detail)
                        };
                        choice(
                            &mut line,
                            words,
                            host.current,
                            host.disabled,
                            FormHit::Host(host.value.clone()),
                        );
                    }
                }
                Field::Worktree => {
                    let from = text::line_width(&line);
                    let mark = if setup.worktree { "[✓] " } else { "[ ] " };
                    push(&mut line, mark, theme.text(), width);
                    push(&mut line, "new worktree", theme.text(), width);
                    spots.push((from, text::line_width(&line), FormHit::Worktree));
                }
            }
            out.push((line, spots));
            // The focused folder: the folders it could be, under it.
            if field == Field::Folder && focused {
                let folders = self.folders(setup, fleet);
                let mut more = Line::from(Span::raw(" ".repeat(2 + LABEL)));
                let mut spots = Vec::new();
                for (i, folder) in folders.iter().take(4).enumerate() {
                    if i > 0 {
                        push(&mut more, " · ", theme.faint(), width);
                    }
                    let from = text::line_width(&more);
                    let ink = if i == 0 { theme.muted() } else { theme.faint() };
                    push(&mut more, folder.clone(), ink, width);
                    spots.push((from, text::line_width(&more), FormHit::Folder(folder.clone())));
                }
                if !folders.is_empty() {
                    out.push((more, spots));
                }
            }
        }
        (out, cursor)
    }

    /// The keys for the focused field, as (key, action) pairs.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut hints = vec![("enter", "start")];
        match self.field {
            Field::Kind | Field::Host => hints.push(("←→", "choose")),
            Field::Worktree => hints.push(("space", "toggle")),
            Field::Folder => hints.push(("tab", "fill")),
            Field::Name => {}
        }
        hints.push(("↑↓", "field"));
        hints.push(("esc", "back"));
        hints
    }
}
