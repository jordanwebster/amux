//! A new agent's settings before it exists: which agent, its model, effort
//! and mode, where it works and on which machine. And the picker that
//! changes one of them, which a running chat also uses for its model and
//! effort.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ui_state::FleetState;
use ui_view::ModeValue;
use wire::{ClaudeCreateConfig, CodexCreateConfig, CreateAgentRequest, Input, Kind, Presence};

use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// The kinds a new agent can be, in the order the picker lists them.
const KINDS: [(Kind, &str, &str); 3] = [
    (Kind::ClaudePty, "Claude (terminal)", "Claude in its own terminal"),
    (Kind::ClaudeSdk, "Claude (headless)", "Claude without a terminal"),
    (Kind::Codex, "Codex", "Codex"),
];

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

/// One of the settings, as the composer's edge shows it and a picker
/// changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
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
    pub kind: Kind,
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
    pub fn defaults(working_dir: &str, local_host: &[u8]) -> Setup {
        Setup {
            kind: KINDS[0].0,
            model: None,
            effort: None,
            mode: None,
            folder: working_dir.to_owned(),
            host: local_host.to_vec(),
            worktree: false,
        }
    }

    fn claude(&self) -> bool {
        self.kind != Kind::Codex
    }

    /// The composer's edge, in groups: what runs it, where, and on which
    /// machine. "new worktree" shows only when it is on.
    pub fn edge(&self, fleet: &FleetState) -> Vec<Vec<(Item, String)>> {
        let kind = KINDS
            .iter()
            .find(|(kind, _, _)| *kind == self.kind)
            .map_or("Claude", |(_, edge, _)| *edge);
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
        vec![
            vec![
                (Item::Kind, kind.to_owned()),
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
        };
        match item {
            Item::Kind => KINDS
                .iter()
                .map(|(kind, _, label)| {
                    choice(
                        (*label).to_owned(),
                        (*kind as i32).to_string(),
                        *kind == self.kind,
                    )
                })
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
            Item::Host => {
                let mut hosts: Vec<&wire::HostEntry> = fleet
                    .hosts()
                    .filter(|host| host.trust() == wire::Trust::Trusted || host.host_id == self.host)
                    .collect();
                hosts.sort_by_key(|host| host.host_id != self.host);
                hosts
                    .into_iter()
                    .map(|host| Choice {
                        label: host_name(fleet, &host.host_id),
                        detail: match host.presence() {
                            Presence::Online | Presence::Unspecified => String::new(),
                            Presence::Away => "away".into(),
                            Presence::Offline => "offline".into(),
                        },
                        value: String::from_utf8_lossy(&host.host_id).into_owned(),
                        current: host.host_id == self.host,
                    })
                    .collect()
            }
            Item::Worktree => Vec::new(),
        }
    }

    /// The modes shift+tab and the mode picker move through.
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

    /// Takes a picked value for `item`. A new kind starts its model,
    /// effort and mode afresh: they do not carry across providers.
    pub fn pick(&mut self, item: Item, value: &str) {
        let some = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        match item {
            Item::Kind => {
                if let Some((kind, _, _)) = KINDS
                    .iter()
                    .find(|(kind, _, _)| (*kind as i32).to_string() == value)
                    && *kind != self.kind
                {
                    self.kind = *kind;
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

    /// The create request, with the first prompt.
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
            cwd: self.folder.clone(),
            kind: self.kind as i32,
            initial_prompt: prompt,
            config: Some(config),
            ..CreateAgentRequest::default()
        }
    }
}

/// Stand-in until the wire can ask for one: the agent would start in a new
/// worktree on this branch, named from the first words of its prompt. The
/// request cannot carry it yet, so the agent starts in the folder itself
/// and the caller says so.
pub fn worktree_stand_in(setup: &Setup, prompt: &str) -> Option<String> {
    if !setup.worktree {
        return None;
    }
    let words: Vec<String> = prompt
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

/// One choice of a picker.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    /// Faint after the label: "away", "default".
    pub detail: String,
    /// What picking it sets.
    pub value: String,
    pub current: bool,
}

/// What a key on a picker did.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    None,
    Close,
    /// The value picked, or a typed path.
    Value(String),
}

/// A list to choose one setting from, drawn above the composer like an
/// ask's choices. Long lists filter as you type; a folder's also takes a
/// typed path.
#[derive(Debug)]
pub struct Picker {
    pub item: Item,
    title: String,
    choices: Vec<Choice>,
    selected: usize,
    filter: Editor,
    /// The typed text itself is a choice (a folder's path).
    typed: bool,
}

/// Lists this long filter as you type.
const FILTERS_FROM: usize = 6;
/// Choices shown at once.
const SHOWN: usize = 8;

impl Picker {
    pub fn new(item: Item, title: &str, choices: Vec<Choice>) -> Picker {
        let selected = choices.iter().position(|c| c.current).unwrap_or(0);
        Picker {
            item,
            title: title.to_owned(),
            choices,
            selected,
            filter: Editor::default(),
            typed: item == Item::Folder,
        }
    }

    fn filtering(&self) -> bool {
        self.typed || self.choices.len() >= FILTERS_FROM
    }

    /// The choices the filter keeps, and a typed path as the last.
    fn shown(&self) -> Vec<Choice> {
        let needle = self.filter.text().trim().to_lowercase();
        let mut shown: Vec<Choice> = self
            .choices
            .iter()
            .filter(|c| needle.is_empty() || c.label.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        if self.typed && !needle.is_empty() && !shown.iter().any(|c| c.label.to_lowercase() == needle)
        {
            shown.push(Choice {
                label: format!("Use {}", self.filter.text().trim()),
                detail: String::new(),
                value: self.filter.text().trim().to_owned(),
                current: false,
            });
        }
        shown
    }

    pub fn key(&mut self, key: KeyEvent) -> Pick {
        let shown = self.shown();
        let last = shown.len().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Pick::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(last),
            KeyCode::Char('k') if !self.filtering() => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Char('j') if !self.filtering() => self.selected = (self.selected + 1).min(last),
            KeyCode::Char(c @ '1'..='9') if !self.filtering() => {
                let at = c as usize - '1' as usize;
                if let Some(choice) = shown.get(at) {
                    return Pick::Value(choice.value.clone());
                }
            }
            KeyCode::Enter => {
                if let Some(choice) = shown.get(self.selected.min(last)) {
                    return Pick::Value(choice.value.clone());
                }
            }
            _ if self.filtering() && !ctrl => {
                self.filter.key(key);
                self.selected = 0;
            }
            _ => {}
        }
        Pick::None
    }

    /// Its lines: the title, the filter when it has one, then the choices,
    /// numbered where they are few, the current one marked.
    pub fn lines(&self, width: usize, theme: Theme) -> (Vec<Line<'static>>, Option<usize>) {
        let mut lines = Vec::new();
        let mut head = Line::from(Span::raw("  "));
        push(&mut head, self.title.clone(), theme.muted(), width);
        let mut cursor = None;
        if self.filtering() {
            push(&mut head, "  ", theme.muted(), width);
            let at = text::line_width(&head);
            if self.filter.is_empty() {
                let hint = if self.typed {
                    "type a path or filter"
                } else {
                    "type to filter"
                };
                push(&mut head, hint, theme.faint(), width);
                cursor = Some(at);
            } else {
                push(&mut head, self.filter.text(), theme.text(), width);
                cursor = Some(at + self.filter.cursor_chars());
            }
        }
        lines.push(head);
        let shown = self.shown();
        let selected = self.selected.min(shown.len().saturating_sub(1));
        let from = selected.saturating_sub(SHOWN - 1);
        for (at, choice) in shown.iter().enumerate().skip(from).take(SHOWN) {
            let lit = at == selected;
            let mut row = Line::from(Span::styled(
                if lit { "  › " } else { "    " },
                theme.accent(),
            ));
            if !self.filtering() {
                push(&mut row, format!("{}. ", at + 1), theme.muted(), width);
            }
            let ink = if lit { theme.bright() } else { theme.text() };
            push(&mut row, choice.label.clone(), ink, width);
            if !choice.detail.is_empty() {
                push(&mut row, format!(" · {}", choice.detail), theme.faint(), width);
            }
            if choice.current {
                push(&mut row, " ✓", theme.faint(), width);
            }
            lines.push(row);
        }
        if shown.is_empty() {
            let mut none = Line::from(Span::raw("    "));
            push(&mut none, "nothing matches", theme.faint(), width);
            lines.push(none);
        }
        (lines, cursor)
    }

    pub fn hint(&self) -> &'static str {
        "↑↓ move · enter pick · esc back"
    }
}
