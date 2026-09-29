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

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{AgentKey, Attention, Connection, FleetState};
use wire::{Agent, Kind, Presence, Trust};

use crate::chat::composer::editor_lines;
use crate::editor::Editor;
use crate::fleet::{FleetEffect, KINDS};
use crate::hosts;
use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Past this, a family that is idle or exited folds into "Older".
const DAY_MS: i64 = 86_400_000;
/// Blank columns at each side of the screen.
const MARGIN: usize = 2;
/// Where section headings start, and the `›` of a highlight without a tint.
const HEAD_COL: usize = MARGIN;
/// Where a row's mark sits, and the text after it.
const MARK_COL: usize = MARGIN + 2;
const NAME_COL: usize = MARK_COL + 2;
/// Lines one mouse-wheel notch scrolls.
const WHEEL: usize = 3;
/// From this height on, rows keep a blank line between them.
const ROOMY: u16 = 30;
/// The composer takes at most this share of the draft screen's height.
const COMPOSER_SHARE: usize = 2;
/// Exit causes that are the person's own act or a clean end, not a failure.
const CLEAN_EXITS: [&str; 5] = ["stopped", "finished", "exited", "aborted", "killed"];
const FINISHED: &str = "finished";

/// Something the list can highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    New,
    Agent(AgentKey),
    Older,
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
    /// The draft's provider: the next one.
    Provider,
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

/// When an agent's place in the order last moved: its activity time as of
/// the last change of attention this client saw.
#[derive(Clone, Copy, Debug)]
struct Moment {
    attention: Attention,
    at_ms: i64,
}

/// A new agent before it exists: the first prompt and what will run it.
#[derive(Debug, Default)]
pub struct Draft {
    pub editor: Editor,
    /// Index into `KINDS`.
    kind: usize,
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
    older_open: bool,
    /// The filter's text while one is typed or kept.
    filter: Option<Editor>,
    /// The top line has the keys.
    filtering: bool,
    overlay: Option<Overlay>,
    pub draft: Draft,
    moments: HashMap<AgentKey, Moment>,
    /// The first list line drawn.
    top: usize,
    /// A key moved the highlight, so the list scrolls to keep it in view.
    /// Hover and the wheel leave the list where it is.
    reveal: bool,
    /// Clickable places on the last frame, the most specific first.
    spots: Vec<Spot>,
}

/// One block of the list.
enum Item {
    New,
    Heading(&'static str, usize),
    Gap,
    Agent(Box<Entry>),
    Older(usize),
    Note(&'static str),
}

struct Entry {
    agent: Agent,
    key: AgentKey,
    depth: usize,
    children: usize,
    expanded: bool,
    moment: i64,
    /// A folded family's member that needs you, standing in on its head's
    /// second line: its name and what it is waiting on.
    loud: Option<(String, String)>,
}

struct Family {
    head: AgentKey,
    members: Vec<AgentKey>,
    attention: Attention,
    moment: i64,
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

    pub fn selected_agent(&self) -> Option<&AgentKey> {
        match &self.selected {
            Some(Target::Agent(agent)) => Some(agent),
            _ => None,
        }
    }

    /// The create call came back: the draft is spent.
    pub fn started(&mut self) {
        self.draft = Draft {
            kind: self.draft.kind,
            ..Draft::default()
        };
    }

    /// The create call failed: the draft is the person's again.
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

    /// Keeps each agent's moment, which moves only when its attention does.
    fn observe(&mut self, fleet: &FleetState) {
        self.moments.retain(|key, _| fleet.agent(key).is_some());
        for agent in fleet.agents() {
            let attention = ui_state::attention(agent);
            let key = ui_state::agent_key(agent);
            if self
                .moments
                .get(&key)
                .is_none_or(|moment| moment.attention != attention)
            {
                self.moments.insert(
                    key,
                    Moment {
                        attention,
                        at_ms: agent.last_activity_ms,
                    },
                );
            }
        }
    }

    fn moment(&self, agent: &Agent) -> i64 {
        self.moments
            .get(&ui_state::agent_key(agent))
            .map_or(agent.last_activity_ms, |moment| moment.at_ms)
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
            agent
                .working_on
                .as_ref()
                .map(|working| working.text.as_str()),
            agent.exit_cause.as_deref(),
            Some(kind_word(agent.kind())),
        ]
        .into_iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(needle))
    }

    /// Every family, newest first, each with its members in family order.
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
                let moment = members
                    .iter()
                    .filter_map(|member| fleet.agent(member))
                    .map(|agent| self.moment(agent))
                    .max()
                    .unwrap_or(0);
                Family {
                    attention: fleet.family_attention(&head).unwrap_or(Attention::Exited),
                    head,
                    members,
                    moment,
                }
            })
            .collect();
        families.sort_by(|a, b| b.moment.cmp(&a.moment).then(a.head.cmp(&b.head)));
        families
    }

    fn items(&self, fleet: &FleetState, now_ms: i64) -> Vec<Item> {
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
        let (needs, rest): (Vec<&Family>, Vec<&Family>) = families
            .iter()
            .partition(|family| family.attention == Attention::NeedsYou);
        let (recent, older): (Vec<&Family>, Vec<&Family>) = rest.into_iter().partition(|family| {
            !matches!(family.attention, Attention::Idle | Attention::Exited)
                || now_ms - family.moment <= DAY_MS
        });
        if !needs.is_empty() {
            items.push(Item::Gap);
            items.push(Item::Heading("Needs you", needs.len()));
            for family in &needs {
                self.push_agent(fleet, &family.head, 0, family, &mut items);
            }
        }
        if !recent.is_empty() {
            items.push(Item::Gap);
            // Named only when "Needs you" sits above it, to mark where that
            // section ends; alone, the list needs no heading.
            if !needs.is_empty() {
                items.push(Item::Heading("Recent", recent.len()));
            }
            for family in &recent {
                self.push_agent(fleet, &family.head, 0, family, &mut items);
            }
        }
        if !older.is_empty() {
            items.push(Item::Gap);
            items.push(Item::Older(older.len()));
            if self.older_shown() {
                for family in &older {
                    self.push_agent(fleet, &family.head, 0, family, &mut items);
                }
            }
        }
        items
    }

    /// A filter looks through the older families too.
    fn older_shown(&self) -> bool {
        self.older_open || self.needle().is_some()
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
            std::cmp::Reverse(fleet.agent(child).map_or(0, |agent| self.moment(agent)))
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
                    let waiting = member
                        .working_on
                        .as_ref()
                        .map(|working| working.text.clone())
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| "needs you".into());
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
            moment: self.moment(agent),
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
                Item::Older(_) => Some(Target::Older),
                _ => None,
            })
            .collect()
    }

    /// The highlight, kept on something still listed: when its agent left,
    /// the first agent, else "+ New agent".
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
            Target::Older => self.older_open = !self.older_open,
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
        self.observe(fleet);
        if self.draft.open {
            return self.draft_key(key);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
        }
        let items = self.items(fleet, now_ms());
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
            KeyCode::Enter | KeyCode::Char('o')
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
            KeyCode::Char('h') => self.overlay = Some(Overlay::Hosts),
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
            KeyCode::Char('d') => {
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
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h')
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

    fn draft_key(&mut self, key: KeyEvent) -> Vec<FleetEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.draft.open = false,
            _ if self.draft.starting => {}
            KeyCode::Tab => self.draft.kind = (self.draft.kind + 1) % KINDS.len(),
            KeyCode::BackTab => self.draft.kind = (self.draft.kind + KINDS.len() - 1) % KINDS.len(),
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.draft.editor.text().trim().is_empty() {
                    return vec![];
                }
                self.draft.starting = true;
                return vec![FleetEffect::Start {
                    kind: KINDS[self.draft.kind].0,
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
        match event.kind {
            MouseEventKind::Moved => {
                if listing && let Some(Hit::Row(target)) = hit(true) {
                    self.selected = Some(target);
                    self.reveal = false;
                }
                vec![]
            }
            MouseEventKind::ScrollUp if listing => {
                self.top = self.top.saturating_sub(WHEEL);
                self.reveal = false;
                vec![]
            }
            MouseEventKind::ScrollDown if listing => {
                self.top += WHEEL;
                self.reveal = false;
                vec![]
            }
            MouseEventKind::Down(MouseButton::Left) => match hit(false) {
                Some(Hit::Key(key)) => self.key(fleet, key, attach),
                Some(Hit::Provider) if !self.draft.starting => {
                    self.draft.kind = (self.draft.kind + 1) % KINDS.len();
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
        self.observe(fleet);
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        if height < 3 || width < 2 * MARGIN + 8 {
            return;
        }
        let mut laid: Vec<Laid> = Vec::with_capacity(height);
        let mut cursor;
        if self.draft.open {
            laid.push(Laid::plain(self.draft_top(width, theme)));
            let (composer, at) = self.composer(fleet, place, width, height, theme);
            let body = height.saturating_sub(2 + composer.len());
            laid.resize_with(1 + body, Laid::default);
            cursor = at.map(|(col, row)| (col, laid.len() + row));
            laid.extend(composer);
        } else {
            let (top, at) = self.top_line(fleet, width, theme, place);
            laid.push(Laid::plain(top));
            cursor = at.map(|col| (col, 0));
            laid.push(Laid::default());
            let room = height - 3;
            if matches!(self.overlay, Some(Overlay::Hosts)) {
                laid.extend(
                    hosts::overlay_lines(fleet, place.local_host, width - MARGIN, theme)
                        .into_iter()
                        .take(room)
                        .map(|line| {
                            let mut indented = Line::from(Span::raw(" ".repeat(MARGIN - 1)));
                            indented.spans.extend(line.spans);
                            Laid::plain(indented)
                        }),
                );
            } else {
                laid.extend(self.list(fleet, now_ms, width, room, area.height, theme, place));
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
        push(&mut line, "amux", theme.emphasis(), width);
        // Where a new agent would start.
        push(
            &mut line,
            format!("  {}", place.working_dir),
            theme.muted(),
            width,
        );
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
        match fleet.connection() {
            Connection::Live => {}
            Connection::Connecting => parts.push(("connecting".into(), theme.muted())),
            Connection::Reconnecting => {
                parts.push(("reconnecting to amux".into(), theme.warn()));
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
        let items = self.items(fleet, now_ms);
        let targets = Self::targets(&items);
        let selected = self.settle(&targets);
        let roomy = height >= ROOMY;
        let mut laid: Vec<Laid> = Vec::new();
        let mut highlight = (0, 0);
        for item in &items {
            let start = laid.len();
            match item {
                Item::New => {
                    let chosen = selected == Target::New;
                    let mut line = lead(chosen, theme);
                    pad_to(&mut line, MARK_COL);
                    push(&mut line, "+", theme.muted(), width);
                    pad_to(&mut line, NAME_COL);
                    push(
                        &mut line,
                        "New agent",
                        name_style(chosen, false, theme),
                        width,
                    );
                    laid.push(Laid::row(tint(line, chosen, width, theme), Target::New));
                }
                Item::Heading(words, count) => {
                    // The label, its count, and a faint rule to the right
                    // edge, so a section's end is visible at a glance.
                    let mut line = Line::default();
                    pad_to(&mut line, HEAD_COL);
                    let label = if *words == "Needs you" {
                        theme.accent()
                    } else {
                        theme.muted()
                    };
                    push(&mut line, *words, label, width);
                    push(&mut line, format!(" {count} "), theme.muted(), width);
                    let end = width.saturating_sub(MARGIN);
                    let rule = end.saturating_sub(text::line_width(&line));
                    push(&mut line, "─".repeat(rule), theme.hairline(), width);
                    laid.push(Laid::plain(line));
                }
                Item::Gap => blank(&mut laid),
                Item::Note(words) => {
                    let mut line = Line::default();
                    pad_to(&mut line, NAME_COL);
                    push(&mut line, *words, theme.muted(), width);
                    laid.push(Laid::plain(line));
                }
                Item::Older(count) => {
                    let chosen = selected == Target::Older;
                    let mut line = lead(chosen, theme);
                    pad_to(&mut line, MARK_COL);
                    push(
                        &mut line,
                        if self.older_shown() { "▾" } else { "▸" },
                        theme.muted(),
                        width,
                    );
                    pad_to(&mut line, NAME_COL);
                    push(&mut line, "Older", name_style(chosen, true, theme), width);
                    push(&mut line, format!(" · {count}"), theme.muted(), width);
                    laid.push(Laid::row(tint(line, chosen, width, theme), Target::Older));
                }
                Item::Agent(entry) => {
                    let chosen = selected == Target::Agent(entry.key.clone());
                    laid.extend(agent_lines(
                        fleet, entry, chosen, now_ms, width, theme, place,
                    ));
                    if roomy {
                        blank(&mut laid);
                    }
                }
            }
            let is_selected = match item {
                Item::New => selected == Target::New,
                Item::Older(_) => selected == Target::Older,
                Item::Agent(entry) => selected == Target::Agent(entry.key.clone()),
                _ => false,
            };
            if is_selected {
                highlight = (start, laid.len());
            }
        }
        while laid
            .last()
            .is_some_and(|laid| laid.spots.is_empty() && laid.line.spans.is_empty())
        {
            laid.pop();
        }
        if self.reveal {
            if highlight.0 < self.top {
                self.top = highlight.0;
            } else if highlight.1 > self.top + room {
                self.top = highlight.1 - room;
            }
            // The first rows sit under "+ New agent" and a heading: keep
            // those in view with them.
            if targets.iter().position(|t| *t == selected).unwrap_or(0) <= 1 {
                self.top = 0;
            }
            self.reveal = false;
        }
        self.top = self.top.min(laid.len().saturating_sub(room));
        laid.into_iter().skip(self.top).take(room).collect()
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
        // What the agent will be, on the bottom edge: provider, where it
        // works, and on which machine.
        let host = fleet
            .host(place.local_host)
            .map(|host| host.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "this machine".into());
        let provider = KINDS[self.draft.kind].1;
        let rest = [place.working_dir, "no worktree", host.as_str()].join(" · ");
        let label = text::str_width(provider) + 3 + text::str_width(&rest);
        let rule = (width - 2 * MARGIN - 2).saturating_sub(label + 4);
        let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut bottom, "╰", edge, width);
        push(&mut bottom, "─".repeat(rule.max(1)), edge, width);
        push(&mut bottom, " ", edge, width);
        let from = text::line_width(&bottom);
        push(&mut bottom, provider, theme.text(), width);
        let to = text::line_width(&bottom);
        push(&mut bottom, " · ", edge, width);
        push(&mut bottom, rest, edge, width);
        push(&mut bottom, " ─", edge, width);
        pad_to(&mut bottom, width - MARGIN - 1);
        push(&mut bottom, "╯", edge, width);
        out.push(Laid {
            line: bottom,
            spots: vec![(Some((from, to)), Hit::Provider)],
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
            Some(Overlay::Rename { editor, .. }) => {
                push(&mut line, "rename to ", theme.muted(), width);
                let cursor = text::line_width(&line) + editor.cursor_chars();
                push(&mut line, editor.text(), theme.text(), width);
                let mut right = Line::default();
                append_hints(
                    &mut right,
                    &mut Vec::new(),
                    0,
                    &[
                        ("enter", "rename", key(KeyCode::Enter)),
                        ("esc", "keep", key(KeyCode::Esc)),
                    ],
                    width,
                    theme,
                );
                place_right(&mut line, right, width);
                return (Laid::plain(line), Some(cursor));
            }
            Some(Overlay::Confirm { name, delete, .. }) => {
                let words = if *delete {
                    format!("Delete {name} and its history?")
                } else {
                    format!("Stop {name}? It can be resumed later.")
                };
                push(&mut line, words, theme.warn(), width);
                push(&mut line, "   ", theme.muted(), width);
                let verb = if *delete { "delete" } else { "stop" };
                vec![
                    ("y", verb, key(KeyCode::Char('y'))),
                    ("n", "keep", key(KeyCode::Char('n'))),
                ]
            }
            Some(Overlay::Hosts) => vec![("esc", "close", key(KeyCode::Esc))],
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
            None if self.draft.open => vec![
                ("enter", "start", key(KeyCode::Enter)),
                (
                    "ctrl+enter",
                    "start, stay home",
                    Hit::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                ),
                ("tab", "provider", key(KeyCode::Tab)),
                ("esc", "back", key(KeyCode::Esc)),
            ],
            None if self.filtering => vec![
                ("enter", "keep", key(KeyCode::Enter)),
                ("esc", "clear", key(KeyCode::Esc)),
                ("↑↓", "move", key(KeyCode::Down)),
            ],
            None => {
                let mut hints = match &self.selected {
                    Some(Target::New) => vec![("enter", "start", key(KeyCode::Enter))],
                    Some(Target::Older) if self.older_open => {
                        vec![("enter", "hide older", key(KeyCode::Enter))]
                    }
                    Some(Target::Older) => vec![("enter", "show older", key(KeyCode::Enter))],
                    _ => vec![("enter", "open", key(KeyCode::Enter))],
                };
                if attach && matches!(self.selected, Some(Target::Agent(_))) {
                    hints.push(("o", "terminal", key(KeyCode::Char('o'))));
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
        ("/", "filter by name, project or host; esc clears".into()),
        ("n", "new agent: its first prompt, then enter".into()),
        ("→ / ←, space", "show or hide a family's agents".into()),
        ("r / s / d, ×", "rename · stop · delete".into()),
        ("h", "hosts: trusted and found nearby".into()),
        ("q", "quit".into()),
    ]
}

/// What home needs from the app that is not the fleet's.
pub struct Place<'a> {
    pub local_host: &'a [u8],
    /// Where a new agent works, as the person would write it.
    pub working_dir: &'a str,
    pub attach: bool,
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
        line.spans.push(Span::styled(key.to_string(), theme.text()));
        line.spans
            .push(Span::styled(format!(" {word}"), theme.muted()));
        at += need;
        spots.push((Some((from, at)), hit.clone()));
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

fn now_ms() -> i64 {
    use client::Clock as _;
    client::SystemClock.now_ms()
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
        theme.text()
    }
}

/// The highlighted row's tint, inset one column from each edge. Every
/// highlightable line starts with blank padding, so the first column is
/// taken from that.
fn tint(line: Line<'static>, chosen: bool, width: usize, theme: Theme) -> Line<'static> {
    let Some(surface) = theme.row_surface().filter(|_| chosen) else {
        return line;
    };
    let mut spans = line.spans.into_iter();
    let mut out = Line::from(Span::raw(" "));
    if let Some(first) = spans.next() {
        let rest = first
            .content
            .strip_prefix(' ')
            .unwrap_or(&first.content)
            .to_owned();
        out.spans
            .push(Span::styled(rest, first.style.patch(surface)));
    }
    for span in spans {
        out.spans
            .push(Span::styled(span.content, span.style.patch(surface)));
    }
    text::fill(&mut out, surface, width - 1);
    out
}

/// An agent's mark. The circles read as one scale, empty (idle) to half
/// (working) to full (needs you); only needs-you is loud, and nothing
/// moves, because motion on a list of many agents is noise.
fn mark(entry: &Entry, unreached: bool, theme: Theme) -> (&'static str, Style) {
    let agent = &entry.agent;
    if unreached {
        return ("–", theme.muted());
    }
    if entry.loud.is_some() {
        return ("●", theme.accent());
    }
    match ui_state::attention(agent) {
        Attention::NeedsYou => ("●", theme.accent()),
        Attention::Working => ("◐", theme.text()),
        Attention::Starting => ("◌", theme.muted()),
        Attention::Idle => ("○", theme.muted()),
        Attention::Exited => match agent.exit_cause.as_deref() {
            Some(FINISHED) => ("✓", theme.muted()),
            Some(cause) if !cause.is_empty() && !CLEAN_EXITS.contains(&cause) => {
                ("✗", theme.error())
            }
            _ => ("·", theme.muted()),
        },
    }
}

/// What an agent's second line says when it has reported nothing better.
fn state_words(agent: &Agent) -> &'static str {
    match ui_state::attention(agent) {
        Attention::NeedsYou => "waiting for you",
        Attention::Working => "working",
        Attention::Starting => "starting",
        Attention::Idle => "idle",
        Attention::Exited => "exited",
    }
}

/// An agent's lines: its mark, name, project and host, and its age or the
/// highlighted row's `×`; then what it is doing, why it stopped, or its
/// state in words. Every agent takes two lines, so rows keep one height.
fn agent_lines(
    fleet: &FleetState,
    entry: &Entry,
    chosen: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
    place: &Place<'_>,
) -> Vec<Laid> {
    let agent = &entry.agent;
    let target = Target::Agent(entry.key.clone());
    let host = fleet.host(&agent.host_id);
    let exited = ui_state::attention(agent) == Attention::Exited;
    // A live agent on a host out of reach is only as it last said.
    let unreached = !exited
        && host
            .is_some_and(|host| host.presence() != Presence::Online || host.revoked == Some(true));
    let indent = 2 * entry.depth;
    let end = width - MARGIN;

    let mut first = lead(chosen, theme);
    let mut spots = vec![(None, Hit::Row(target.clone()))];
    pad_to(&mut first, MARK_COL + indent);
    let (glyph, glyph_style) = mark(entry, unreached, theme);
    first.spans.push(Span::styled(glyph, glyph_style));
    pad_to(&mut first, NAME_COL + indent);
    // The right edge: the age, or on the highlighted row its `×`.
    let right = if chosen {
        "×".to_owned()
    } else {
        text::age(now_ms, entry.moment)
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
    let name = text::ellipsize(name_of(agent), room.saturating_sub(fold_width).max(1));
    let dim = exited || unreached;
    first
        .spans
        .push(Span::styled(name, name_style(chosen, dim, theme)));
    if let Some(fold) = fold {
        let from = text::line_width(&first);
        let style = if entry.loud.is_some() {
            theme.accent()
        } else {
            theme.muted()
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
        first.spans.push(Span::styled(meta, theme.muted()));
    }
    pad_to(&mut first, right_at);
    if chosen {
        first.spans.push(Span::styled(right, theme.text()));
        spots.push((
            Some((right_at.saturating_sub(1), end + 1)),
            Hit::Close(entry.key.clone()),
        ));
    } else {
        first.spans.push(Span::styled(right, theme.muted()));
    }

    let mut second = Line::default();
    pad_to(&mut second, NAME_COL + indent);
    let room = end.saturating_sub(NAME_COL + indent);
    if let Some((name, waiting)) = &entry.loud {
        let name = text::ellipsize(name, room / 2);
        push(&mut second, "↳ ", theme.muted(), end);
        push(&mut second, name, theme.text(), end);
        push(
            &mut second,
            format!(" · {}", text::first_line(waiting)),
            theme.muted(),
            end,
        );
    } else {
        let detail = if unreached {
            let host = host.map_or("its host", |host| host.name.as_str());
            match agent.working_on.as_ref().filter(|w| !w.text.is_empty()) {
                Some(working) => format!("{host} is away · {}", text::first_line(&working.text)),
                None => format!("{host} is away"),
            }
        } else if exited {
            match agent
                .exit_cause
                .as_deref()
                .filter(|cause| !cause.is_empty())
            {
                Some(FINISHED) => "finished".to_owned(),
                Some("exited") | None => "exited".to_owned(),
                Some(cause) => format!("exited · {cause}"),
            }
        } else {
            agent
                .working_on
                .as_ref()
                .map(|working| text::first_line(&working.text).to_owned())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| state_words(agent).to_owned())
        };
        push(
            &mut second,
            text::ellipsize(&detail, room),
            theme.muted(),
            end,
        );
    }
    vec![
        Laid {
            line: tint(first, chosen, width, theme),
            spots,
        },
        Laid::row(tint(second, chosen, width, theme), target),
    ]
}
