//! Home: a top line, the list of agents, and one line of hints. No frame
//! and no composer; a new agent starts as an empty chat whose composer
//! names what it will be.
//!
//! The list puts the families that need you under one heading and
//! everything else newest first beneath it, with families left idle or
//! exited for a day folded into one line. Order moves only when an agent's
//! attention changes — a send, a turn starting or ending — never while it
//! streams, so nothing moves under the cursor or the mouse. Hover and
//! selection are one highlight: moving the mouse over a row selects it,
//! and keys take over until the mouse moves again.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{AgentKey, Attention, Connection, FleetState};
use wire::{Agent, Kind, Presence, Trust};

use crate::chat::composer::editor_lines;
use crate::editor::Editor;
use crate::fleet::FleetEffect;
use crate::hosts;
use crate::setup::{ChatIn, Form, FormHit, FormOutcome, Item as Setting, Pick, Picker, Setup};
use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Blank columns at each side of the screen.
const MARGIN: usize = 2;
/// Where section headings start, and the `›` of a highlight without a tint.
const HEAD_COL: usize = MARGIN;
/// Where a row's mark sits, and the text after it.
const MARK_COL: usize = MARGIN + 2;
const NAME_COL: usize = MARK_COL + 2;
/// From this height on, rows keep a blank line between them.
const ROOMY: u16 = 30;
/// The composer takes at most this share of the draft screen's height.
const COMPOSER_SHARE: usize = 2;
/// Exit causes that are the person's own act or a clean end, not a failure.
const CLEAN_EXITS: [&str; 5] = ["stopped", "finished", "exited", "aborted", "killed"];
const FINISHED: &str = "finished";
/// The highlighted row's action: stop, or delete once exited. Brackets mark
/// what can be clicked.
const CLOSE: &str = "[x]";

/// Something the list can highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    New,
    Agent(AgentKey),
    /// A section's heading, which folds and unfolds it.
    Section(Section),
}

/// The list's sections, by what the agents in them are doing. A family
/// sits in the section of its loudest member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Section {
    NeedsYou,
    Running,
    Exited,
}

impl Section {
    fn words(self) -> &'static str {
        match self {
            Section::NeedsYou => "Needs you",
            Section::Running => "Running",
            Section::Exited => "Exited",
        }
    }

    /// Exited is history, rarely opened, so it starts folded.
    fn folded_by_default(self) -> bool {
        self == Section::Exited
    }
}

/// What a click at a place does.
#[derive(Clone, Debug)]
enum Hit {
    /// A whole row: hovering highlights it, clicking also opens it.
    Row(Target),
    /// The highlighted row's `×`: stop, or delete once exited.
    Close(AgentKey),
    /// A family's fold marker.
    Fold(AgentKey),
    /// A hint: the key it names.
    Key(KeyEvent),
    /// One of the draft's settings on the composer's edge.
    Setting(Setting),
}

/// A clickable span of one screen row.
#[derive(Clone, Debug)]
struct Spot {
    y: u16,
    x: (u16, u16),
    hit: Hit,
}

/// One laid-out line and what clicking along it does. `spots` hold
/// column ranges; a `None` range spans the whole line.
#[derive(Default)]
struct Laid {
    line: Line<'static>,
    spots: Vec<(Option<(usize, usize)>, Hit)>,
}

impl Laid {
    fn plain(line: Line<'static>) -> Self {
        Laid {
            line,
            spots: Vec::new(),
        }
    }

    fn row(line: Line<'static>, target: Target) -> Self {
        Laid {
            line,
            spots: vec![(None, Hit::Row(target))],
        }
    }
}

/// A new agent before it exists: the first prompt and what will run it.
#[derive(Debug, Default)]
pub struct Draft {
    pub editor: Editor,
    /// What will run it and where; filled from the defaults when it first
    /// shows.
    pub setup: Option<Setup>,
    /// A setting being chosen, in a flyover over its place on the edge.
    picker: Option<Picker>,
    /// For an agent used in its own terminal, the form that starts it in
    /// place of the composer.
    form: Option<Form>,
    /// The flyover as last drawn, for clicks.
    flyover: crate::setup::Flyover,
    /// The modal's click targets as last drawn: (row, columns, hit).
    modal: Vec<(u16, (u16, u16), FormHit)>,
    /// Ctrl+S was pressed: the next letter names a setting.
    prefix: bool,
    /// The edge item under the pointer.
    hover: Option<Setting>,
    /// On screen. Leaving keeps the text for the next time.
    open: bool,
    /// The create call is in flight.
    starting: bool,
}

#[derive(Debug)]
enum Overlay {
    Hosts,
    Rename {
        agent: AgentKey,
        editor: Editor,
    },
    Confirm {
        agent: AgentKey,
        name: String,
        delete: bool,
    },
}

/// What home remembers between frames. Nothing here outlives the process.
#[derive(Debug, Default)]
pub struct Home {
    selected: Option<Target>,
    expanded: HashSet<Vec<u8>>,
    /// Sections the person folded or unfolded against their default.
    toggled: HashSet<Section>,
    /// The filter's text while one is typed or kept.
    filter: Option<Editor>,
    /// The top line has the keys.
    filtering: bool,
    overlay: Option<Overlay>,
    pub draft: Draft,
    /// The first list line drawn.
    top: usize,
    /// A key moved the highlight, so the list scrolls to keep it in view.
    /// Hover and the wheel leave the list where it is.
    reveal: bool,
    /// Clickable places on the last frame, the most specific first.
    spots: Vec<Spot>,
    /// A modal's buttons on the last frame: (row, columns, what it does).
    overlay_spots: Vec<(u16, (u16, u16), Confirmed)>,
    /// Renaming: the name field's cursor as (column, list line) on the last
    /// frame, when its row is in view.
    rename_at: Option<(usize, usize)>,
}

/// A modal's two answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirmed {
    Yes,
    No,
}

/// One block of the list.
enum Item {
    New,
    Heading(Section, usize),
    /// A blank line, unless the list already ends in one.
    Gap,
    /// A blank line, always.
    Space,
    Agent(Box<Entry>),
    Note(&'static str),
}

struct Entry {
    agent: Agent,
    key: AgentKey,
    depth: usize,
    children: usize,
    expanded: bool,
    /// A folded family's member that needs you, standing in on its head's
    /// second line: its name and what it is waiting on.
    loud: Option<(String, String)>,
}

struct Family {
    head: AgentKey,
    members: Vec<AgentKey>,
    attention: Attention,
    order: i64,
}

fn name_of(agent: &Agent) -> &str {
    agent
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or("unnamed")
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudePty => "claude",
        Kind::ClaudeSdk => "claude sdk",
        Kind::Codex => "codex",
        Kind::Unspecified => "agent",
    }
}

/// The last path component: the project an agent works in.
fn project(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
}

fn plain_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl Home {
    pub fn select(&mut self, agent: AgentKey) {
        self.selected = Some(Target::Agent(agent));
        self.reveal = true;
    }

    /// The create call came back: the draft is spent.
    pub fn started(&mut self) {
        // The next agent keeps these settings but not this one's name.
        let mut setup = self.draft.setup.take();
        if let Some(setup) = &mut setup {
            setup.name = None;
        }
        self.draft = Draft {
            setup,
            ..Draft::default()
        };
    }

    /// The create call failed: the draft is the person's again.
    /// Opens the new agent's screen with `setup`: a chat's own settings,
    /// copied to start a sibling.
    pub fn new_agent(&mut self, setup: Setup) {
        self.draft.setup = Some(setup);
        self.draft.picker = None;
        self.draft.form = None;
        self.draft.prefix = false;
        self.draft.open = true;
    }

    /// The hosts modal, from a chat's `ctrl+a p`.
    pub fn open_hosts(&mut self) {
        self.draft.open = false;
        self.overlay = Some(Overlay::Hosts);
    }

    pub fn start_failed(&mut self) {
        self.draft.starting = false;
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self) -> bool {
        if self.draft.open {
            return !self.draft.editor.is_empty();
        }
        match (&self.overlay, &self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => !editor.is_empty(),
            (None, Some(filter)) if self.filtering => !filter.is_empty(),
            _ => false,
        }
    }

    pub fn kill_field(&mut self) -> bool {
        if self.draft.open {
            return self.draft.editor.kill_all();
        }
        match (&mut self.overlay, &mut self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => editor.kill_all(),
            (None, Some(filter)) if self.filtering => filter.kill_all(),
            _ => false,
        }
    }

    /// A bracketed paste types into whichever field has the keys. A name
    /// and a filter are one line.
    pub fn paste(&mut self, pasted: &str) {
        if self.draft.open && !self.draft.starting {
            self.draft.editor.paste(pasted);
            return;
        }
        let one_line = pasted
            .split(['\r', '\n'])
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        match (&mut self.overlay, &mut self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => editor.insert_str(&one_line),
            (None, Some(filter)) if self.filtering => filter.insert_str(&one_line),
            _ => {}
        }
    }

    fn needle(&self) -> Option<String> {
        self.filter
            .as_ref()
            .map(|filter| filter.text().trim().to_lowercase())
            .filter(|needle| !needle.is_empty())
    }

    fn matches(fleet: &FleetState, agent: &Agent, needle: &str) -> bool {
        let host = fleet.host(&agent.host_id).map(|host| host.name.as_str());
        [
            Some(name_of(agent)),
            Some(agent.cwd.as_str()),
            host,
            agent.exit_cause.as_deref(),
            Some(kind_word(agent.kind())),
        ]
        .into_iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(needle))
    }

    /// Every family, newest first (as the seam orders them), each with its
    /// members in family order.
    fn families(&self, fleet: &FleetState) -> Vec<Family> {
        let mut families: Vec<Family> = fleet
            .roots()
            .map(|root| {
                let head = ui_state::agent_key(root);
                let mut members = vec![head.clone()];
                let mut at = 0;
                while at < members.len() {
                    let children: Vec<AgentKey> =
                        fleet.families().children(&members[at]).cloned().collect();
                    members.extend(children);
                    at += 1;
                }
                Family {
                    attention: fleet.family_attention(&head).unwrap_or(Attention::Exited),
                    order: crate::pending::home_order(root),
                    head,
                    members,
                }
            })
            .collect();
        families.sort_by(|a, b| b.order.cmp(&a.order).then(a.head.cmp(&b.head)));
        families
    }

    fn items(&self, fleet: &FleetState) -> Vec<Item> {
        let needle = self.needle();
        let families: Vec<Family> = self
            .families(fleet)
            .into_iter()
            .filter(|family| {
                needle.as_ref().is_none_or(|needle| {
                    family
                        .members
                        .iter()
                        .filter_map(|member| fleet.agent(member))
                        .any(|agent| Self::matches(fleet, agent, needle))
                })
            })
            .collect();
        let mut items = vec![Item::New];
        if families.is_empty() {
            items.push(Item::Gap);
            items.push(Item::Note(match (&needle, fleet.caught_up()) {
                (Some(_), _) => "nothing matches",
                (None, true) => "no agents yet",
                (None, false) => "loading…",
            }));
            return items;
        }
        let section_of = |family: &Family| match family.attention {
            Attention::NeedsYou => Section::NeedsYou,
            Attention::Exited => Section::Exited,
            _ => Section::Running,
        };
        for section in [Section::NeedsYou, Section::Running, Section::Exited] {
            let members: Vec<&Family> = families
                .iter()
                .filter(|family| section_of(family) == section)
                .collect();
            if members.is_empty() {
                continue;
            }
            // Two blank lines between sections, one under a heading.
            items.push(Item::Gap);
            items.push(Item::Space);
            items.push(Item::Heading(section, members.len()));
            if self.folded(section) {
                continue;
            }
            items.push(Item::Gap);
            for family in members {
                self.push_agent(fleet, &family.head, 0, family, &mut items);
            }
        }
        items
    }

    /// Whether a section's agents are hidden. A filter looks through every
    /// section, folded or not.
    fn folded(&self, section: Section) -> bool {
        self.needle().is_none() && section.folded_by_default() != self.toggled.contains(&section)
    }

    fn toggle_section(&mut self, section: Section) {
        if !self.toggled.remove(&section) {
            self.toggled.insert(section);
        }
        self.reveal = true;
    }

    fn push_agent(
        &self,
        fleet: &FleetState,
        key: &AgentKey,
        depth: usize,
        family: &Family,
        items: &mut Vec<Item>,
    ) {
        let Some(agent) = fleet.agent(key) else {
            return;
        };
        let mut children: Vec<AgentKey> = fleet.families().children(key).cloned().collect();
        children.sort_by_key(|child| {
            std::cmp::Reverse(fleet.agent(child).map_or(0, crate::pending::home_order))
        });
        let expanded = self.expanded.contains(&key.agent) && !children.is_empty();
        let loud = if !expanded
            && !children.is_empty()
            && ui_state::attention(agent) != Attention::NeedsYou
        {
            family
                .members
                .iter()
                .filter(|member| *member != key)
                .filter_map(|member| fleet.agent(member))
                .find(|member| ui_state::attention(member) == Attention::NeedsYou)
                .map(|member| {
                    let waiting =
                        crate::pending::home_summary(member).unwrap_or_else(|| "needs you".into());
                    (name_of(member).to_owned(), waiting)
                })
        } else {
            None
        };
        items.push(Item::Agent(Box::new(Entry {
            agent: agent.clone(),
            key: key.clone(),
            depth,
            children: children.len(),
            expanded,
            loud,
        })));
        if expanded {
            for child in &children {
                self.push_agent(fleet, child, depth + 1, family, items);
            }
        }
    }

    fn targets(items: &[Item]) -> Vec<Target> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::New => Some(Target::New),
                Item::Agent(entry) => Some(Target::Agent(entry.key.clone())),
                Item::Heading(section, _) => Some(Target::Section(*section)),
                _ => None,
            })
            .collect()
    }

    /// The highlight, kept on something still listed: when its agent left,
    /// the first agent, else "+ New Agent".
    fn settle(&mut self, targets: &[Target]) -> Target {
        if let Some(selected) = self.selected.as_ref().filter(|t| targets.contains(t)) {
            return selected.clone();
        }
        let fallback = targets
            .iter()
            .find(|target| matches!(target, Target::Agent(_)))
            .or(targets.first())
            .cloned()
            .unwrap_or(Target::New);
        self.selected = Some(fallback.clone());
        fallback
    }

    fn step(&mut self, targets: &[Target], by: isize) {
        let current = self.settle(targets);
        let at = targets.iter().position(|t| *t == current).unwrap_or(0);
        let next = at
            .saturating_add_signed(by)
            .min(targets.len().saturating_sub(1));
        self.selected = targets.get(next).cloned();
        self.reveal = true;
    }

    fn activate(&mut self, target: Target) -> Vec<FleetEffect> {
        match target {
            Target::New => self.draft.open = true,
            Target::Agent(agent) => return vec![FleetEffect::Open(agent)],
            Target::Section(section) => self.toggle_section(section),
        }
        vec![]
    }

    /// Asks before stopping a live agent or deleting an exited one.
    fn confirm_close(&mut self, fleet: &FleetState, agent: AgentKey, delete: bool) {
        if let Some(entry) = fleet.agent(&agent) {
            let exited = ui_state::attention(entry) == Attention::Exited;
            if delete || !exited {
                self.overlay = Some(Overlay::Confirm {
                    name: name_of(entry).to_owned(),
                    agent,
                    delete: delete || exited,
                });
            }
        }
    }

    fn toggle(&mut self, agent: &AgentKey) {
        if !self.expanded.remove(&agent.agent) {
            self.expanded.insert(agent.agent.clone());
        }
    }

    pub fn key(&mut self, fleet: &FleetState, key: KeyEvent, attach: bool) -> Vec<FleetEffect> {
        if self.draft.open {
            return self.draft_key(fleet, key);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
        }
        let items = self.items(fleet);
        let targets = Self::targets(&items);
        if self.filtering {
            self.filter_key(&targets, key);
            return vec![];
        }
        let current = self.settle(&targets);
        let agent = match &current {
            Target::Agent(agent) => Some(agent.clone()),
            _ => None,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return vec![FleetEffect::Quit],
            KeyCode::Char('?') => return vec![FleetEffect::Help],
            KeyCode::Up | KeyCode::Char('k') => self.step(&targets, -1),
            KeyCode::Down | KeyCode::Char('j') => self.step(&targets, 1),
            KeyCode::Home | KeyCode::Char('g') => self.step(&targets, isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.step(&targets, isize::MAX),
            KeyCode::Enter | KeyCode::Char('a')
                if attach && (ctrl || key.code != KeyCode::Enter) =>
            {
                return agent.map(FleetEffect::Attach).into_iter().collect();
            }
            KeyCode::Enter => return self.activate(current),
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.get_or_insert_with(Editor::default);
            }
            KeyCode::Esc => self.filter = None,
            KeyCode::Char('n') => self.draft.open = true,
            KeyCode::Right => {
                if let Some(agent) = agent.filter(|agent| {
                    fleet.families().children(agent).next().is_some()
                        && !self.expanded.contains(&agent.agent)
                }) {
                    self.toggle(&agent);
                }
            }
            KeyCode::Left => {
                if let Some(agent) = agent
                    && !self.expanded.remove(&agent.agent)
                    && let Some(parent) = fleet.parent(&agent)
                {
                    let parent = ui_state::agent_key(parent);
                    self.expanded.remove(&parent.agent);
                    self.select(parent);
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('z') => {
                if let Some(agent) =
                    agent.filter(|agent| fleet.families().children(agent).next().is_some())
                {
                    self.toggle(&agent);
                }
            }
            KeyCode::Char('p') => self.overlay = Some(Overlay::Hosts),
            // Detach: leave to the shell; the agents keep running.
            KeyCode::Char('d') => return vec![FleetEffect::Quit],
            KeyCode::Char('r') => {
                if let Some(agent) = agent
                    && let Some(entry) = fleet.agent(&agent)
                {
                    let mut editor = Editor::default();
                    editor.set(name_of(entry), vec![]);
                    self.overlay = Some(Overlay::Rename { agent, editor });
                }
            }
            KeyCode::Char('s') => {
                if let Some(agent) = agent {
                    self.confirm_close(fleet, agent, false);
                }
            }
            KeyCode::Char('x') => {
                if let Some(agent) = agent {
                    self.confirm_close(fleet, agent, true);
                }
            }
            _ => {}
        }
        vec![]
    }

    fn filter_key(&mut self, targets: &[Target], key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter = None;
                self.filtering = false;
            }
            KeyCode::Enter => {
                self.filtering = false;
                if self.needle().is_none() {
                    self.filter = None;
                }
            }
            KeyCode::Up => self.step(targets, -1),
            KeyCode::Down => self.step(targets, 1),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.key(key);
                }
                self.top = 0;
            }
        }
    }

    fn overlay_key(&mut self, overlay: Overlay, key: KeyEvent) -> Vec<FleetEffect> {
        match overlay {
            Overlay::Hosts => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p')
                ) {
                    self.overlay = Some(Overlay::Hosts);
                }
                vec![]
            }
            Overlay::Rename { agent, mut editor } => match key.code {
                KeyCode::Esc => vec![],
                KeyCode::Enter => {
                    let name = editor.text().trim().to_owned();
                    if name.is_empty() {
                        self.overlay = Some(Overlay::Rename { agent, editor });
                        return vec![];
                    }
                    vec![FleetEffect::Rename { agent, name }]
                }
                _ => {
                    editor.key(key);
                    self.overlay = Some(Overlay::Rename { agent, editor });
                    vec![]
                }
            },
            Overlay::Confirm {
                agent,
                name,
                delete,
            } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter if delete => vec![FleetEffect::Delete(agent)],
                KeyCode::Char('y') | KeyCode::Enter => vec![FleetEffect::Stop(agent)],
                KeyCode::Char('n') | KeyCode::Esc => vec![],
                _ => {
                    self.overlay = Some(Overlay::Confirm {
                        agent,
                        name,
                        delete,
                    });
                    vec![]
                }
            },
        }
    }

    fn draft_key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.draft.starting {
            if key.code == KeyCode::Esc {
                self.draft.open = false;
            }
            return vec![];
        }
        // An agent used in its own terminal: the form has the keys.
        if self
            .draft
            .setup
            .as_ref()
            .is_some_and(|setup| setup.chat_in == ChatIn::Terminal)
        {
            let (Some(setup), Some(form)) = (&mut self.draft.setup, &mut self.draft.form) else {
                return vec![];
            };
            return match form.key(setup, fleet, key) {
                FormOutcome::None => vec![],
                FormOutcome::Close => {
                    self.draft.open = false;
                    vec![]
                }
                FormOutcome::Start => {
                    self.draft.starting = true;
                    vec![FleetEffect::Start {
                        setup: Box::new(setup.clone()),
                        text: String::new(),
                        attachments: Vec::new(),
                        open: true,
                    }]
                }
            };
        }
        if let Some(picker) = &mut self.draft.picker {
            match picker.key(key) {
                Pick::None => {}
                Pick::Close => self.draft.picker = None,
                Pick::Value(value) => {
                    let item = picker.item;
                    if let Some(setup) = &mut self.draft.setup {
                        setup.pick(item, &value);
                    }
                    self.draft.picker = None;
                }
            }
            return vec![];
        }
        // Ctrl+S, then a letter: the setting it names.
        if std::mem::take(&mut self.draft.prefix) {
            let item = match key.code {
                KeyCode::Char('n') => Some(Setting::Name),
                KeyCode::Char('m') => Some(Setting::Model),
                KeyCode::Char('e') => Some(Setting::Effort),
                KeyCode::Char('d') => Some(Setting::Folder),
                KeyCode::Char('h') => Some(Setting::Host),
                KeyCode::Char('w') if crate::pending::offers_worktree() => Some(Setting::Worktree),
                KeyCode::Char('a') => Some(Setting::Kind),
                _ => None,
            };
            if let Some(item) = item {
                self.open_setting(fleet, item);
            }
            return vec![];
        }
        match key.code {
            KeyCode::Esc => self.draft.open = false,
            KeyCode::Char('s') if ctrl => self.draft.prefix = true,
            KeyCode::BackTab => {
                if let Some(setup) = &mut self.draft.setup {
                    setup.next_mode();
                }
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.draft.editor.text().trim().is_empty() {
                    return vec![];
                }
                let Some(setup) = self.draft.setup.clone() else {
                    return vec![];
                };
                self.draft.starting = true;
                return vec![FleetEffect::Start {
                    setup: Box::new(setup),
                    text: self.draft.editor.text().to_owned(),
                    attachments: self.draft.editor.attachments().to_vec(),
                    open: !ctrl,
                }];
            }
            _ => {
                self.draft.editor.key(key);
            }
        }
        vec![]
    }

    /// A setting from the edge or Ctrl+S: its picker, or for the worktree,
    /// on or off at once.
    fn open_setting(&mut self, fleet: &FleetState, item: Setting) {
        let Some(setup) = &mut self.draft.setup else {
            return;
        };
        if item == Setting::Worktree {
            setup.pick(Setting::Worktree, "");
            return;
        }
        if item == Setting::Name {
            let name = setup.name.clone().unwrap_or_default();
            self.draft.picker = Some(Picker::text(Setting::Name, "Name", &name));
            return;
        }
        let title = match item {
            Setting::Name => "Name",
            Setting::Kind => "Agent",
            Setting::Model => "Model",
            Setting::Effort => "Effort",
            Setting::Mode => "Mode",
            Setting::Folder => "Folder",
            Setting::Host => "Host",
            Setting::Worktree => "Worktree",
        };
        let choices = setup.choices(item, fleet);
        let typed = setup.takes_typed(item);
        self.draft.picker = Some(Picker::new(item, title, choices, typed));
    }

    pub fn mouse(
        &mut self,
        fleet: &FleetState,
        event: MouseEvent,
        attach: bool,
    ) -> Vec<FleetEffect> {
        let (x, y) = (event.column, event.row);
        let hit = |rows_only: bool| {
            self.spots
                .iter()
                .find(|spot| {
                    spot.y == y
                        && (spot.x.0..spot.x.1).contains(&x)
                        && (!rows_only || matches!(spot.hit, Hit::Row(_)))
                })
                .map(|spot| spot.hit.clone())
        };
        let listing = self.overlay.is_none() && !self.draft.open;
        // A modal asking before a stop or delete: its buttons act; the rest
        // of home under it does nothing.
        if let Some(Overlay::Confirm { agent, delete, .. }) = &self.overlay {
            let answer = match event.kind {
                MouseEventKind::Down(MouseButton::Left) => self
                    .overlay_spots
                    .iter()
                    .find(|(row, (from, to), _)| *row == y && (*from..*to).contains(&x))
                    .map(|(_, _, answer)| *answer),
                _ => None,
            };
            let effect = match answer {
                Some(Confirmed::Yes) if *delete => vec![FleetEffect::Delete(agent.clone())],
                Some(Confirmed::Yes) => vec![FleetEffect::Stop(agent.clone())],
                _ => vec![],
            };
            if answer.is_some() {
                self.overlay = None;
            }
            return effect;
        }
        if matches!(self.overlay, Some(Overlay::Hosts)) {
            return vec![];
        }
        // The new agent's modal has the pointer: its places act, the rest
        // of home under it does nothing.
        if self.draft.open && self.draft.form.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = event.kind
                && !self.draft.starting
                && let Some(hit) = self
                    .draft
                    .modal
                    .iter()
                    .find(|(row, (from, to), _)| *row == y && (*from..*to).contains(&x))
                    .map(|(_, _, hit)| hit.clone())
                && let (Some(setup), Some(form)) = (&mut self.draft.setup, &mut self.draft.form)
            {
                match form.click(setup, hit) {
                    FormOutcome::None => {}
                    FormOutcome::Close => self.draft.open = false,
                    FormOutcome::Start => {
                        self.draft.starting = true;
                        return vec![FleetEffect::Start {
                            setup: Box::new(setup.clone()),
                            text: String::new(),
                            attachments: Vec::new(),
                            open: true,
                        }];
                    }
                }
            }
            return vec![];
        }
        // A flyover is open: a click on a choice picks it, anywhere else
        // closes it.
        if self.draft.open
            && self.draft.picker.is_some()
            && let MouseEventKind::Down(MouseButton::Left) = event.kind
        {
            let pick = match self.draft.flyover.choice_at(x, y) {
                Some(at) => self.draft.picker.as_mut().map(|picker| picker.click(at)),
                None if self.draft.flyover.covers(x, y) => None,
                None => Some(Pick::Close),
            };
            match pick {
                Some(Pick::Value(value)) => {
                    let item = self.draft.picker.as_ref().map(|picker| picker.item);
                    if let (Some(setup), Some(item)) = (&mut self.draft.setup, item) {
                        setup.pick(item, &value);
                    }
                    self.draft.picker = None;
                }
                Some(Pick::Close) => self.draft.picker = None,
                _ => {}
            }
            return vec![];
        }
        match event.kind {
            MouseEventKind::Moved if self.draft.open => {
                self.draft.hover = match hit(false) {
                    Some(Hit::Setting(item)) => Some(item),
                    _ => None,
                };
                vec![]
            }
            MouseEventKind::Moved => {
                if listing && let Some(Hit::Row(target)) = hit(true) {
                    self.selected = Some(target);
                    self.reveal = false;
                }
                vec![]
            }
            MouseEventKind::ScrollUp if listing => {
                self.top = self
                    .top
                    .saturating_sub(crate::wheel::lines(crate::wheel::Direction::Up));
                self.reveal = false;
                vec![]
            }
            MouseEventKind::ScrollDown if listing => {
                self.top += crate::wheel::lines(crate::wheel::Direction::Down);
                self.reveal = false;
                vec![]
            }
            MouseEventKind::Down(MouseButton::Left) => match hit(false) {
                Some(Hit::Key(key)) => self.key(fleet, key, attach),
                Some(Hit::Setting(item)) if !self.draft.starting => {
                    self.draft.prefix = false;
                    self.open_setting(fleet, item);
                    vec![]
                }
                Some(Hit::Close(agent)) if listing => {
                    self.confirm_close(fleet, agent, false);
                    vec![]
                }
                Some(Hit::Fold(agent)) if listing => {
                    self.toggle(&agent);
                    vec![]
                }
                Some(Hit::Row(target)) if listing => {
                    self.selected = Some(target.clone());
                    self.activate(target)
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        fleet: &FleetState,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
        place: &Place<'_>,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        if height < 3 || width < 2 * MARGIN + 8 {
            return;
        }
        // A blank line above the top line keeps it off the terminal's edge.
        let mut laid: Vec<Laid> = vec![Laid::default()];
        let mut cursor;
        // Where the new agent's composer box starts, for its flyover.
        let mut box_top = None;
        if self.draft.open && self.draft.setup.is_none() {
            self.draft.setup = Some(Setup::defaults(
                place.chat_in,
                place.working_dir,
                place.local_host,
                place.defaults,
            ));
        }
        // An agent used in its own terminal starts from a modal over home;
        // one chatted with in amux, from its own composer.
        let modal = self.draft.open
            && self
                .draft
                .setup
                .as_ref()
                .is_some_and(|setup| setup.chat_in == ChatIn::Terminal);
        if modal && self.draft.form.is_none() {
            self.draft.form = self.draft.setup.as_ref().map(Form::new);
        }
        if self.draft.open && !modal {
            laid.push(Laid::plain(self.draft_top(width, theme)));
            let (composer, at) = self.composer(fleet, place, width, height, theme);
            let body = height.saturating_sub(3 + composer.len());
            laid.resize_with(2 + body, Laid::default);
            box_top = Some(laid.len());
            cursor = at
                .filter(|_| self.draft.picker.is_none())
                .map(|(col, row)| (col, laid.len() + row));
            laid.extend(composer);
        } else {
            let (top, at) = self.top_line(fleet, width, theme, place);
            laid.push(Laid::plain(top));
            cursor = at.map(|col| (col, 1));
            laid.push(Laid::default());
            // A blank, the top line and a blank above the list; a blank and
            // the hints below it.
            let room = height.saturating_sub(5);
            let list_top = laid.len();
            laid.extend(self.list(fleet, now_ms, width, room, area.height, theme, place));
            // Renaming: the name is a field on its own row.
            if let Some((col, row)) = self.rename_at {
                cursor = Some((col, list_top + row));
            }
        }
        laid.resize_with(height - 1, Laid::default);
        let (hint, at) = self.hint_line(footer, width, theme, place.attach);
        if let Some(col) = at {
            cursor = Some((col, height - 1));
        }
        laid.push(hint);

        self.spots.clear();
        let mut lines = Vec::with_capacity(height);
        for (row, Laid { line, spots }) in laid.into_iter().enumerate() {
            let y = area.y + row as u16;
            // Specific places first, so a click finds them before the row.
            let (whole, specific): (Vec<_>, Vec<_>) =
                spots.into_iter().partition(|(x, _)| x.is_none());
            for (x, hit) in specific.into_iter().chain(whole) {
                let (from, to) = x.unwrap_or((0, width));
                self.spots.push(Spot {
                    y,
                    x: (area.x + from as u16, area.x + to.min(width) as u16),
                    hit,
                });
            }
            lines.push(line);
        }
        self.spots
            .sort_by_key(|spot| matches!(spot.hit, Hit::Row(_)));
        paint.render_widget(Paragraph::new(lines), area);
        if let Some((x, y)) = cursor {
            paint.set_cursor_position(Position::new(
                area.x + x.min(width - 1) as u16,
                area.y + y as u16,
            ));
        }
        // A setting being chosen: its flyover rises from the setting's place
        // on the edge, over the box. Effort lives in the model's words.
        if let (Some(picker), Some(top)) = (&self.draft.picker, box_top) {
            let item = match picker.item {
                Setting::Effort => Setting::Model,
                item => item,
            };
            let anchor = self
                .spots
                .iter()
                .find(|spot| matches!(&spot.hit, Hit::Setting(at) if *at == item))
                .map_or(area.x + MARGIN as u16, |spot| spot.x.0);
            let above = area.y + top as u16;
            self.draft.flyover =
                crate::setup::draw_flyover(paint, picker, anchor, above, area, theme);
            if let Some(at) = self.draft.flyover.cursor {
                paint.set_cursor_position(at);
            }
        }
        // Hosts, and asking before a stop or delete: modals over home.
        self.overlay_spots.clear();
        match &self.overlay {
            Some(Overlay::Hosts) => {
                let rows = hosts::modal_rows(fleet, place.local_host, theme);
                let inner = crate::panel::content_width(&rows)
                    .max(40)
                    .min(width.saturating_sub(8));
                let lines = crate::panel::bordered("Hosts", rows, inner, &[], None, theme);
                crate::panel::centre(paint, lines, area);
            }
            Some(Overlay::Confirm { name, delete, .. }) => {
                let (sentence, verb) = if *delete {
                    (format!("Delete {name} and its history?"), "Delete")
                } else {
                    (format!("Stop {name}? It can be resumed later."), "Stop")
                };
                let (buttons, spots) = crate::panel::buttons(
                    &[(verb, Confirmed::Yes), ("Cancel", Confirmed::No)],
                    theme,
                );
                let rows = vec![
                    Line::default(),
                    Line::from(Span::styled(sentence, theme.text())),
                    Line::default(),
                    buttons,
                    crate::panel::legend(
                        &[("enter", &verb.to_lowercase()), ("esc", "cancel")],
                        theme,
                    ),
                ];
                let inner = crate::panel::content_width(&rows)
                    .max(36)
                    .min(width.saturating_sub(8));
                let title = if *delete { "Delete" } else { "Stop" };
                let lines = crate::panel::bordered(title, rows, inner, &[], None, theme);
                let rect = crate::panel::centre(paint, lines, area);
                // The buttons' row: the border, a blank, the sentence, a
                // blank, then the buttons; two columns of border and padding.
                self.overlay_spots = spots
                    .into_iter()
                    .map(|(from, to, hit)| {
                        (
                            rect.y + 4,
                            (rect.x + 2 + from as u16, rect.x + 2 + to as u16),
                            hit,
                        )
                    })
                    .collect();
            }
            _ => {}
        }
        // The new agent's modal, centred over home.
        self.draft.modal.clear();
        if modal && let (Some(setup), Some(form)) = (&self.draft.setup, &self.draft.form) {
            let (lines, hits, at) = form.modal(setup, fleet, width, theme);
            let w = lines.iter().map(text::line_width).max().unwrap_or(0) as u16;
            let h = (lines.len() as u16).min(area.height);
            let x = area.x + area.width.saturating_sub(w) / 2;
            // A little above the middle, where the eye already is.
            let y = area.y + area.height.saturating_sub(h) / 3;
            let rect = Rect {
                x,
                y,
                width: w.min(area.width),
                height: h,
            };
            paint.render_widget(ratatui::widgets::Clear, rect);
            paint.render_widget(Paragraph::new(lines), rect);
            self.draft.modal = hits
                .into_iter()
                .map(|(line, from, to, hit)| {
                    (y + line as u16, (x + from as u16, x + to as u16), hit)
                })
                .collect();
            if let Some((col, line)) = at.filter(|_| !self.draft.starting) {
                paint.set_cursor_position(Position::new(x + col as u16, y + line as u16));
            }
        }
    }

    /// Where you are and what needs you; the filter while one is typed or
    /// kept. Returns the cursor's column while the filter has the keys.
    fn top_line(
        &self,
        fleet: &FleetState,
        width: usize,
        theme: Theme,
        place: &Place<'_>,
    ) -> (Line<'static>, Option<usize>) {
        let local_host = place.local_host;
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        if let Some(filter) = self
            .filter
            .as_ref()
            .filter(|_| self.filtering || self.needle().is_some())
        {
            push(&mut line, "/ ", theme.accent(), width);
            let at = text::line_width(&line);
            push(&mut line, filter.text(), theme.text(), width);
            let cursor = self.filtering.then(|| at + filter.cursor_chars());
            let right = match self.needle() {
                Some(needle) => {
                    let shown = fleet
                        .agents()
                        .filter(|agent| Self::matches(fleet, agent, &needle))
                        .count();
                    format!("{shown} match{}", if shown == 1 { "" } else { "es" })
                }
                None => {
                    push(&mut line, "name, project or host", theme.muted(), width);
                    String::new()
                }
            };
            place_right(
                &mut line,
                Line::from(Span::styled(right, theme.muted())),
                width,
            );
            return (line, cursor);
        }
        // Home is the whole fleet, across hosts and directories, so the top
        // line names no directory: where a new agent starts belongs to
        // starting one.
        push(&mut line, "amux", theme.emphasis(), width);
        let need = fleet
            .roots()
            .filter(|agent| {
                fleet.family_attention(&ui_state::agent_key(agent)) == Some(Attention::NeedsYou)
            })
            .count();
        let working = fleet
            .agents()
            .filter(|agent| {
                matches!(
                    ui_state::attention(agent),
                    Attention::Working | Attention::Starting
                )
            })
            .count();
        let away: Vec<&str> = fleet
            .hosts()
            .filter(|host| host.trust() == Trust::Trusted && host.host_id != local_host)
            .filter(|host| host.presence() != Presence::Online || host.revoked == Some(true))
            .map(|host| host.name.as_str())
            .collect();
        let mut parts: Vec<(String, Style)> = Vec::new();
        // A daemon that restarted into a newer build keeps serving this
        // older client; only the person can restart it.
        if let Some(running) = fleet
            .host(local_host)
            .and_then(|host| host.version.as_deref())
            .filter(|running| !place.version.is_empty() && *running != place.version)
        {
            parts.push((
                format!("amux {running} running · restart to update"),
                theme.warning(),
            ));
        }
        // Signed out, hosts out of reach can only be reached again by
        // signing in.
        if !away.is_empty() && ui_view::signed_out(fleet, local_host) {
            parts.push(("signed out · amux login".into(), theme.warning()));
        }
        match fleet.connection() {
            Connection::Live => {}
            Connection::Connecting => parts.push(("connecting".into(), theme.muted())),
            Connection::Reconnecting => {
                parts.push(("reconnecting to amux".into(), theme.warning()));
            }
        }
        match away.as_slice() {
            [] => {}
            [one] => parts.push((format!("{one} away"), theme.muted())),
            many => parts.push((format!("{} hosts away", many.len()), theme.muted())),
        }
        if working > 0 {
            parts.push((format!("{working} working"), theme.muted()));
        }
        if need > 0 {
            parts.push((
                format!("{need} need{} you", if need == 1 { "s" } else { "" }),
                theme.accent(),
            ));
        }
        let mut right = Line::default();
        for (i, (words, style)) in parts.into_iter().enumerate() {
            if i > 0 {
                right.spans.push(Span::styled(" · ", theme.muted()));
            }
            right.spans.push(Span::styled(words, style));
        }
        place_right(&mut line, right, width);
        (line, None)
    }

    fn draft_top(&self, width: usize, theme: Theme) -> Line<'static> {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut line, "amux", theme.emphasis(), width);
        push(&mut line, "  new agent", theme.muted(), width);
        line
    }

    /// The list's lines for `room` rows, scrolled to keep the highlight in
    /// view after a key moved it.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        fleet: &FleetState,
        now_ms: i64,
        width: usize,
        room: usize,
        height: u16,
        theme: Theme,
        place: &Place<'_>,
    ) -> Vec<Laid> {
        let items = self.items(fleet);
        let targets = Self::targets(&items);
        let selected = self.settle(&targets);
        let roomy = height >= ROOMY;
        let mut laid: Vec<Laid> = Vec::new();
        // The lines the highlight covers, and the lines scrolling keeps in
        // view with it: a highlighted heading brings its section along, so
        // opening one shows what it holds.
        let mut highlight = (0, 0);
        let mut in_view = (0, 0);
        let mut section_open = false;
        let mut rename_line = None;
        for item in &items {
            let start = laid.len();
            match item {
                Item::New => {
                    // A control, not a card: highlighted, its words brighten
                    // rather than the row filling in, and only the words
                    // answer the mouse.
                    let chosen = selected == Target::New;
                    let (mark, words) = if chosen {
                        (theme.emphasis(), theme.emphasis())
                    } else {
                        (theme.faint(), theme.muted())
                    };
                    // Laid out like a heading: the `+` on the screen's left
                    // edge with the top line and the headings' arrows, the
                    // words on the column the headings' labels use.
                    let mut line = Line::default();
                    pad_to(&mut line, HEAD_COL);
                    push(&mut line, "+", mark, width);
                    pad_to(&mut line, MARK_COL);
                    push(&mut line, "New Agent", words, width);
                    let to = text::line_width(&line);
                    laid.push(Laid {
                        line,
                        spots: vec![(Some((HEAD_COL, to)), Hit::Row(Target::New))],
                    });
                }
                Item::Heading(section, count) => {
                    // A fold marker, the label, its count, and a faint rule
                    // to the right edge, so a section's end is visible at a
                    // glance. The label lines up with the rows' marks.
                    // Like "+ New Agent", a control: highlighted, the marker
                    // and label brighten, and only they answer the mouse.
                    let target = Target::Section(*section);
                    let chosen = selected == target;
                    let mut line = Line::default();
                    pad_to(&mut line, HEAD_COL);
                    let marker = if self.folded(*section) { "▸" } else { "▾" };
                    let (marker_style, label) = match (chosen, section) {
                        (true, _) => (theme.emphasis(), theme.emphasis()),
                        (false, Section::NeedsYou) => {
                            (theme.faint(), theme.accent().add_modifier(Modifier::BOLD))
                        }
                        (false, _) => (theme.faint(), theme.muted().add_modifier(Modifier::BOLD)),
                    };
                    push(&mut line, marker, marker_style, width);
                    pad_to(&mut line, MARK_COL);
                    push(&mut line, section.words(), label, width);
                    let to = text::line_width(&line);
                    push(&mut line, format!(" {count} "), theme.faint(), width);
                    let end = width.saturating_sub(MARGIN);
                    let rule = end.saturating_sub(text::line_width(&line));
                    push(&mut line, "─".repeat(rule), theme.hairline(), width);
                    laid.push(Laid {
                        line,
                        spots: vec![(Some((HEAD_COL, to)), Hit::Row(target))],
                    });
                }
                Item::Gap => blank(&mut laid),
                Item::Space => laid.push(Laid::default()),
                Item::Note(words) => {
                    let mut line = Line::default();
                    pad_to(&mut line, NAME_COL);
                    push(&mut line, *words, theme.muted(), width);
                    laid.push(Laid::plain(line));
                }
                Item::Agent(entry) => {
                    let chosen = selected == Target::Agent(entry.key.clone());
                    let renaming = match &self.overlay {
                        Some(Overlay::Rename { agent, editor }) if *agent == entry.key => {
                            Some(editor)
                        }
                        _ => None,
                    };
                    if let Some(editor) = renaming {
                        rename_line = Some((
                            laid.len(),
                            NAME_COL + 2 * entry.depth + editor.cursor_chars(),
                        ));
                    }
                    laid.extend(agent_lines(
                        fleet, entry, chosen, renaming, now_ms, width, theme, place,
                    ));
                    if roomy {
                        blank(&mut laid);
                    }
                }
            }
            let is_selected = match item {
                Item::New => selected == Target::New,
                Item::Heading(section, _) => selected == Target::Section(*section),
                Item::Agent(entry) => selected == Target::Agent(entry.key.clone()),
                _ => false,
            };
            if is_selected {
                highlight = (start, laid.len());
                in_view = highlight;
            }
            if matches!(item, Item::Agent(_)) && section_open {
                in_view.1 = laid.len();
            }
            if let Item::Heading(section, _) = item {
                section_open = selected == Target::Section(*section);
            }
        }
        while laid
            .last()
            .is_some_and(|laid| laid.spots.is_empty() && laid.line.spans.is_empty())
        {
            laid.pop();
        }
        if matches!(selected, Target::Agent(_)) {
            pad_highlight(&mut laid, highlight, width, theme);
        }
        let highlight = in_view;
        if self.reveal {
            if highlight.0 < self.top {
                self.top = highlight.0;
            } else if highlight.1 > self.top + room {
                // Never scroll the highlight's own first line out of view.
                self.top = (highlight.1 - room).min(highlight.0);
            }
            // The first rows sit under "+ New Agent" and a heading: keep
            // those in view with them.
            if targets.iter().position(|t| *t == selected).unwrap_or(0) <= 1 {
                self.top = 0;
            }
            self.reveal = false;
        }
        self.top = self.top.min(laid.len().saturating_sub(room));
        self.rename_at = rename_line
            .filter(|(line, _)| (self.top..self.top + room).contains(line))
            .map(|(line, col)| (col, line - self.top));
        let mut shown: Vec<Laid> = laid.into_iter().skip(self.top).take(room).collect();
        // A card's padding shows only with its card.
        if let Some(first) = shown.first_mut()
            && text::is_padding(&first.line, '▀')
        {
            *first = Laid::default();
        }
        if let Some(last) = shown.last_mut()
            && text::is_padding(&last.line, '▄')
        {
            *last = Laid::default();
        }
        shown
    }

    /// The composer of a new agent's chat, boxed, with what the agent will
    /// be on its bottom edge. Returns the lines and the cursor as (column,
    /// line).
    fn composer(
        &self,
        fleet: &FleetState,
        place: &Place<'_>,
        width: usize,
        height: usize,
        theme: Theme,
    ) -> (Vec<Laid>, Option<(usize, usize)>) {
        let inner = width - 2 * MARGIN - 4;
        let (mut body, (row, col)) = editor_lines(
            &self.draft.editor,
            "What should the new agent work on?",
            inner,
            theme,
        );
        // The box already says where to type, so the composer's own edge
        // mark becomes a prompt.
        for (i, line) in body.iter_mut().enumerate() {
            if let Some(first) = line.spans.first_mut() {
                *first = Span::styled(if i == 0 { "› " } else { "  " }, theme.muted());
            }
        }
        let most = (height / COMPOSER_SHARE).saturating_sub(2).max(1);
        let skip = body.len().saturating_sub(most).min(row);
        let edge = theme.muted();
        let mut out = Vec::new();
        let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut top, "╭", edge, width);
        push(&mut top, "─".repeat(width - 2 * MARGIN - 2), edge, width);
        push(&mut top, "╮", edge, width);
        out.push(Laid::plain(top));
        for line in body.into_iter().skip(skip).take(most) {
            let mut boxed = Line::from(Span::raw(" ".repeat(MARGIN)));
            push(&mut boxed, "│ ", edge, width);
            boxed.spans.extend(line.spans);
            pad_to(&mut boxed, width - MARGIN - 1);
            push(&mut boxed, "│", edge, width);
            out.push(Laid::plain(boxed));
        }
        // What the agent will be, on the bottom edge: what runs it, where it
        // works, and on which machine. Each item is a click target, lit
        // as a chip under the pointer.
        let setup = self.draft.setup.clone().unwrap_or_else(|| {
            Setup::defaults(
                place.chat_in,
                place.working_dir,
                place.local_host,
                place.defaults,
            )
        });
        let mut groups = setup.edge(fleet);
        let room = (width - 2 * MARGIN - 2).saturating_sub(6);
        let measure = |groups: &Vec<Vec<(Setting, String)>>| {
            groups
                .iter()
                .map(|group| {
                    group.iter().map(|(_, w)| text::str_width(w)).sum::<usize>()
                        + 3 * group.len().saturating_sub(1)
                })
                .sum::<usize>()
                + 3 * groups.len().saturating_sub(1)
        };
        // Too long for the edge: the mode drops, then the model (ctrl+s
        // still reaches them), then the folder shortens in the middle.
        for dropped in [Setting::Mode, Setting::Model] {
            if measure(&groups) > room {
                for group in &mut groups {
                    group.retain(|(item, _)| *item != dropped);
                }
                groups.retain(|group| !group.is_empty());
            }
        }
        let over = measure(&groups).saturating_sub(room);
        if over > 0 {
            for group in &mut groups {
                for (item, words) in group.iter_mut() {
                    if *item == Setting::Folder {
                        let keep = text::str_width(words).saturating_sub(over).max(12);
                        *words = text::ellipsize_middle(words, keep);
                    }
                }
            }
        }
        let label = measure(&groups);
        let rule = (width - 2 * MARGIN - 2).saturating_sub(label + 3);
        let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut bottom, "╰", edge, width);
        push(&mut bottom, "─".repeat(rule.max(1)), edge, width);
        push(&mut bottom, " ", edge, width);
        let mut spots = Vec::new();
        for (g, group) in groups.into_iter().enumerate() {
            if g > 0 {
                push(&mut bottom, " │ ", edge, width);
            }
            for (i, (item, words)) in group.into_iter().enumerate() {
                if i > 0 {
                    push(&mut bottom, " · ", edge, width);
                }
                let from = text::line_width(&bottom);
                let ink = if self.draft.hover == Some(item) {
                    theme.chip()
                } else {
                    theme.text()
                };
                push(&mut bottom, words, ink, width);
                spots.push((Some((from, text::line_width(&bottom))), Hit::Setting(item)));
            }
        }
        push(&mut bottom, " ─", edge, width);
        pad_to(&mut bottom, width - MARGIN - 1);
        push(&mut bottom, "╯", edge, width);
        out.push(Laid {
            line: bottom,
            spots,
        });
        let cursor = (!self.draft.starting).then_some((MARGIN + 2 + col, 1 + row - skip));
        (out, cursor)
    }

    /// The bottom line: an open question or a notice when there is one,
    /// else the keys for what is highlighted. Returns the cursor's column
    /// while the rename field has the keys.
    fn hint_line(
        &self,
        footer: Option<Line<'static>>,
        width: usize,
        theme: Theme,
        attach: bool,
    ) -> (Laid, Option<usize>) {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        let key = |code| Hit::Key(plain_key(code));
        let hints: Vec<(&str, &str, Hit)> = match &self.overlay {
            // Renaming in place: only its keys.
            Some(Overlay::Rename { .. }) => vec![
                ("enter", "save", key(KeyCode::Enter)),
                ("esc", "cancel", key(KeyCode::Esc)),
            ],
            // A modal carries its own keys.
            Some(Overlay::Confirm { .. }) => return (Laid::plain(line), None),
            Some(Overlay::Hosts) => return (Laid::plain(line), None),
            None if footer.is_some() => {
                // The app's words start with their own margin.
                let footer = footer.unwrap_or_default();
                let mut spans = footer.spans.into_iter().peekable();
                while spans
                    .peek()
                    .is_some_and(|span| span.content.trim().is_empty())
                {
                    spans.next();
                }
                line.spans.extend(spans);
                return (Laid::plain(line), None);
            }
            None if self.draft.open && self.draft.starting => {
                push(&mut line, "starting the agent…", theme.muted(), width);
                return (Laid::plain(line), None);
            }
            // The modal carries its own keys.
            None if self.draft.open && self.draft.form.is_some() => {
                return (Laid::plain(line), None);
            }
            None if self.draft.open => match (&self.draft.picker, self.draft.prefix) {
                (Some(picker), _) => picker
                    .hints()
                    .into_iter()
                    .map(|(keys, action)| {
                        let code = match keys {
                            "enter" => KeyCode::Enter,
                            "esc" => KeyCode::Esc,
                            "tab" => KeyCode::Tab,
                            _ => KeyCode::Down,
                        };
                        (keys, action, key(code))
                    })
                    .collect(),
                (None, true) => [
                    ("n", "name", key(KeyCode::Char('n'))),
                    ("m", "model", key(KeyCode::Char('m'))),
                    ("e", "effort", key(KeyCode::Char('e'))),
                    ("d", "folder", key(KeyCode::Char('d'))),
                    ("h", "host", key(KeyCode::Char('h'))),
                    ("w", "worktree", key(KeyCode::Char('w'))),
                    ("a", "agent", key(KeyCode::Char('a'))),
                    ("esc", "back", key(KeyCode::Esc)),
                ]
                .into_iter()
                .filter(|(letter, _, _)| *letter != "w" || crate::pending::offers_worktree())
                .collect(),
                (None, false) => vec![
                    ("enter", "start", key(KeyCode::Enter)),
                    (
                        "ctrl+s",
                        "settings",
                        Hit::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                    ),
                    ("shift+tab", "mode", key(KeyCode::BackTab)),
                    ("esc", "back", key(KeyCode::Esc)),
                ],
            },
            None if self.filtering => vec![
                ("enter", "keep", key(KeyCode::Enter)),
                ("esc", "clear", key(KeyCode::Esc)),
                ("↑↓", "move", key(KeyCode::Down)),
            ],
            None => {
                let mut hints = match &self.selected {
                    Some(Target::New) => vec![("enter", "start", key(KeyCode::Enter))],
                    Some(Target::Section(section)) if self.folded(*section) => {
                        vec![("enter", "show", key(KeyCode::Enter))]
                    }
                    Some(Target::Section(_)) => vec![("enter", "hide", key(KeyCode::Enter))],
                    _ => vec![("enter", "open", key(KeyCode::Enter))],
                };
                if attach && matches!(self.selected, Some(Target::Agent(_))) {
                    hints.push(("a", "attach", key(KeyCode::Char('a'))));
                }
                if self.needle().is_some() {
                    hints.push(("esc", "clear filter", key(KeyCode::Esc)));
                } else {
                    hints.push(("/", "filter", key(KeyCode::Char('/'))));
                }
                if !matches!(self.selected, Some(Target::New)) {
                    hints.push(("n", "new", key(KeyCode::Char('n'))));
                }
                hints.push(("?", "keys", key(KeyCode::Char('?'))));
                hints
            }
        };
        let mut spots = Vec::new();
        let start = text::line_width(&line);
        append_hints(&mut line, &mut spots, start, &hints, width, theme);
        (Laid { line, spots }, None)
    }
}

/// Home's part of the key help.
pub fn help_rows() -> Vec<(&'static str, String)> {
    vec![
        ("Home", String::new()),
        (
            "j / k, ↑ / ↓",
            "move; the mouse highlights what it is over".into(),
        ),
        ("enter, click", "open the chat".into()),
        (
            "a, ctrl+enter",
            "attach to the agent's own terminal (this machine)".into(),
        ),
        ("/", "filter by name, project or host; esc clears".into()),
        (
            "n",
            "new agent: its first prompt, then enter (or its form, for its own terminal)".into(),
        ),
        (
            "ctrl+s then a letter",
            "a new agent's name, model, effort, folder, host, agent".into(),
        ),
        ("→ / ←, space", "show or hide a family's agents".into()),
        ("r", "rename, in place".into()),
        ("s", "stop; it can be resumed".into()),
        ("x", "delete; it asks first".into()),
        ("p", "hosts".into()),
        (
            "d, q",
            "detach: leave to the shell; agents keep running".into(),
        ),
    ]
}

/// What home needs from the app that is not the fleet's.
pub struct Place<'a> {
    pub local_host: &'a [u8],
    /// This build's version, compared with the daemon's.
    pub version: &'a str,
    /// Where a new agent works, as the person would write it.
    pub working_dir: &'a str,
    pub attach: bool,
    /// Where the person chats, which decides how a new agent starts.
    pub chat_in: ChatIn,
    /// What each agent starts with.
    pub defaults: &'a crate::setup::Defaults,
}

/// `key word` pairs, three blanks apart, as many as fit; each is a click
/// target for its key. `? keys` is last and the first to go.
fn append_hints(
    line: &mut Line<'static>,
    spots: &mut Vec<(Option<(usize, usize)>, Hit)>,
    start: usize,
    hints: &[(&str, &str, Hit)],
    width: usize,
    theme: Theme,
) {
    let mut at = start;
    for (i, (key, word, hit)) in hints.iter().enumerate() {
        let gap = if i == 0 { 0 } else { 3 };
        let need = gap + text::str_width(key) + 1 + text::str_width(word);
        if at + need > width - MARGIN {
            break;
        }
        line.spans.push(Span::raw(" ".repeat(gap)));
        let from = at + gap;
        // The key is what you press, so it reads first; its action recedes.
        line.spans
            .push(Span::styled(key.to_string(), theme.emphasis()));
        line.spans
            .push(Span::styled(format!(" {word}"), theme.faint()));
        at += need;
        spots.push((Some((from, at)), hit.clone()));
    }
}

/// Half a line of the highlight's tint above and below it, drawn with half
/// blocks into the blank lines around the row, so the highlighted row reads
/// as a card with room around its words. Where rows are packed with no
/// blank line between them, the highlight stays flat.
fn pad_highlight(laid: &mut [Laid], (start, end): (usize, usize), width: usize, theme: Theme) {
    let Some(surface) = theme.row_surface().and_then(|style| style.bg) else {
        return;
    };
    if end <= start {
        return;
    }
    let is_blank = |laid: &Laid| laid.spots.is_empty() && laid.line.spans.is_empty();
    // The range may take in the blank line laid after a row.
    let mut end = end;
    while end > start && laid.get(end - 1).is_some_and(is_blank) {
        end -= 1;
    }
    // The tint's own extent: the outer margin on each side.
    let edge = |glyph: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(MARGIN)),
            Span::styled(
                glyph.repeat(width.saturating_sub(2 * MARGIN)),
                Style::default().fg(surface),
            ),
        ])
    };
    if let Some(above) = start.checked_sub(1).and_then(|at| laid.get_mut(at))
        && is_blank(above)
    {
        above.line = edge("▄");
    }
    if let Some(below) = laid.get_mut(end)
        && is_blank(below)
    {
        below.line = edge("▀");
    }
}

/// One blank line, unless the list already ends in one.
fn blank(laid: &mut Vec<Laid>) {
    let ends_blank = laid
        .last()
        .is_some_and(|last| last.spots.is_empty() && last.line.spans.is_empty());
    if !ends_blank {
        laid.push(Laid::default());
    }
}

/// Places `right` against the right margin, or drops it when it would
/// touch what is already on the line.
fn place_right(line: &mut Line<'static>, right: Line<'static>, width: usize) {
    let used = text::line_width(line);
    let need = text::line_width(&right);
    if need == 0 || used + 2 + need + MARGIN > width {
        return;
    }
    pad_to(line, width - MARGIN - need);
    line.spans.extend(right.spans);
}

/// A highlightable line's first cells: blank, or `›` at the heading column
/// when the terminal gave no ground to tint.
fn lead(chosen: bool, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    if chosen && theme.row_surface().is_none() {
        pad_to(&mut line, HEAD_COL);
        line.spans.push(Span::styled("›", theme.text()));
    }
    line
}

fn name_style(chosen: bool, dim: bool, theme: Theme) -> Style {
    if chosen && theme.row_surface().is_none() {
        theme.emphasis()
    } else if dim {
        theme.muted()
    } else {
        theme.bright()
    }
}

/// The highlighted row's tint, from the outer margin on one side to the
/// outer margin on the other. Every highlightable line starts with at least
/// a margin of blank padding, so the tint's start is taken from that.
fn tint(line: Line<'static>, chosen: bool, width: usize, theme: Theme) -> Line<'static> {
    let Some(surface) = theme.row_surface().filter(|_| chosen) else {
        return line;
    };
    let mut spans = line.spans.into_iter();
    let mut out = Line::from(Span::raw(" ".repeat(MARGIN)));
    if let Some(first) = spans.next() {
        let rest: String = first.content.chars().skip(MARGIN).collect();
        out.spans
            .push(Span::styled(rest, first.style.patch(surface)));
    }
    for span in spans {
        out.spans
            .push(Span::styled(span.content, span.style.patch(surface)));
    }
    text::fill(&mut out, surface, width - MARGIN);
    out
}

/// An agent's mark: empty when nothing is happening, full while it works,
/// and full in the accent when it needs you. Every other state is said in
/// words on the second line, so there are only three marks to learn.
fn mark(entry: &Entry, quiet: bool, theme: Theme) -> (&'static str, Style) {
    if quiet {
        return ("○", theme.faint());
    }
    if entry.loud.is_some() {
        return ("●", theme.accent());
    }
    match ui_state::attention(&entry.agent) {
        Attention::NeedsYou => ("●", theme.accent()),
        Attention::Working => ("●", theme.text()),
        Attention::Starting | Attention::Idle => ("○", theme.muted()),
        Attention::Exited => ("○", theme.faint()),
    }
}

/// An agent's lines. The first: its mark, its name, and faint where it is
/// (project, and host when not this machine), with its age at the right
/// or, on the highlighted row, `[x]`. A second only when there is something
/// known to say: its host away, why it ended, a folded member that needs
/// you, or what it asks or is doing where that is known (see the pending
/// seam); never its state again in words. Ink follows importance: the name
/// and an ask read brightest, the second line grey, where and when faint.
#[allow(clippy::too_many_arguments)]
fn agent_lines(
    fleet: &FleetState,
    entry: &Entry,
    chosen: bool,
    renaming: Option<&Editor>,
    now_ms: i64,
    width: usize,
    theme: Theme,
    place: &Place<'_>,
) -> Vec<Laid> {
    let agent = &entry.agent;
    let target = Target::Agent(entry.key.clone());
    let host = fleet.host(&agent.host_id);
    let attention = ui_state::attention(agent);
    let exited = attention == Attention::Exited;
    // A live agent on a host out of reach is only as it last said.
    let unreached = !exited
        && host
            .is_some_and(|host| host.presence() != Presence::Online || host.revoked == Some(true));
    let quiet = exited || unreached;
    let indent = 2 * entry.depth;
    // Text sits a margin inside the card's edge, on the right as on the left.
    let end = width - 2 * MARGIN;

    let mut first = lead(chosen, theme);
    let mut spots = vec![(None, Hit::Row(target.clone()))];
    pad_to(&mut first, MARK_COL + indent);
    let (glyph, glyph_style) = mark(entry, unreached, theme);
    first.spans.push(Span::styled(glyph, glyph_style));
    pad_to(&mut first, NAME_COL + indent);
    // The right edge: the age, or on the highlighted row its `[x]`.
    let right = if chosen {
        CLOSE.to_owned()
    } else {
        text::age(now_ms, agent.last_activity_ms)
    };
    let right_at = end.saturating_sub(text::str_width(&right));
    let fold = (entry.children > 0).then(|| {
        format!(
            " {} {}",
            if entry.expanded { "▾" } else { "▸" },
            entry.children
        )
    });
    let meta = {
        let mut parts = vec![project(&agent.cwd).to_owned()];
        if let Some(host) = host.filter(|host| host.host_id != place.local_host) {
            parts.push(host.name.clone());
        }
        parts.join(" · ")
    };
    let room = right_at.saturating_sub(NAME_COL + indent + 2);
    let fold_width = fold.as_deref().map_or(0, text::str_width);
    // Renaming, the name is a field in its own place, underlined.
    match renaming {
        Some(editor) => first.spans.push(Span::styled(
            text::ellipsize(editor.text(), room.max(1)),
            theme.bright().add_modifier(Modifier::UNDERLINED),
        )),
        None => {
            let name = text::ellipsize(name_of(agent), room.saturating_sub(fold_width).max(1));
            first
                .spans
                .push(Span::styled(name, name_style(chosen, quiet, theme)));
        }
    }
    if let Some(fold) = fold {
        let from = text::line_width(&first);
        let style = if entry.loud.is_some() {
            theme.accent()
        } else {
            theme.faint()
        };
        first.spans.push(Span::styled(fold, style));
        spots.push((
            Some((from, text::line_width(&first))),
            Hit::Fold(entry.key.clone()),
        ));
    }
    let used = text::line_width(&first) + 2;
    if right_at > used + 1 {
        pad_to(&mut first, used);
        let meta = text::ellipsize(&meta, right_at - used - 1);
        first.spans.push(Span::styled(meta, theme.faint()));
    }
    pad_to(&mut first, right_at);
    if chosen {
        first.spans.push(Span::styled(right, theme.text()));
        spots.push((Some((right_at, end)), Hit::Close(entry.key.clone())));
    } else {
        first.spans.push(Span::styled(right, theme.faint()));
    }

    let first = Laid {
        line: tint(first, chosen, width, theme),
        spots,
    };
    let mut second = Line::default();
    pad_to(&mut second, NAME_COL + indent);
    let room = end.saturating_sub(NAME_COL + indent);
    let said = crate::pending::home_summary(agent)
        .map(|summary| text::first_line(&summary).to_owned())
        .filter(|text| !text.is_empty());
    if let Some((name, waiting)) = &entry.loud {
        let name = text::ellipsize(name, room / 2);
        push(&mut second, "↳ ", theme.faint(), end);
        push(&mut second, name, theme.text(), end);
        push(
            &mut second,
            format!(" · {}", text::first_line(waiting)),
            theme.text(),
            end,
        );
    } else {
        let (detail, style) = if unreached {
            let host = host.map_or("its host", |host| host.name.as_str());
            (format!("{host} is away"), theme.faint())
        } else if exited {
            match agent
                .exit_cause
                .as_deref()
                .filter(|cause| !cause.is_empty())
            {
                Some(FINISHED) => ("finished".to_owned(), theme.faint()),
                Some(cause) if CLEAN_EXITS.contains(&cause) => ("exited".to_owned(), theme.faint()),
                None => ("exited".to_owned(), theme.faint()),
                Some(cause) => (format!("exited · {cause}"), theme.error()),
            }
        } else if let Some(said) = said {
            // What it asks is the most important line on home.
            let style = if attention == Attention::NeedsYou {
                theme.text()
            } else {
                theme.muted()
            };
            (said, style)
        } else {
            return vec![first];
        };
        push(&mut second, text::ellipsize(&detail, room), style, end);
    }
    vec![first, Laid::row(tint(second, chosen, width, theme), target)]
}
