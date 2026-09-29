//! The fleet: every family head, loudest first, with expanded families'
//! members beneath their parents; create, rename, stop and delete; the
//! hosts overlay. Home: a chat always returns here.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{AgentKey, Attention, Connection, FleetState};
use ui_view::{FleetCard, FleetRow, fleet_list};
use wire::{Attachment, Kind, Presence, Trust};

use crate::editor::Editor;
use crate::home::{self, Home};
use crate::hosts;
use crate::text::{self, pad_to, push, push_right};
use crate::theme::Theme;

/// What a fleet key asks the event loop to do.
#[derive(Clone, Debug, PartialEq)]
pub enum FleetEffect {
    Open(AgentKey),
    /// The agent's own interface, raw.
    Attach(AgentKey),
    Create {
        kind: Kind,
    },
    /// Create an agent with its first prompt, and open its chat or stay.
    Start {
        kind: Kind,
        text: String,
        attachments: Vec<Attachment>,
        open: bool,
    },
    Rename {
        agent: AgentKey,
        name: String,
    },
    Stop(AgentKey),
    Delete(AgentKey),
    /// Leave amux.
    Quit,
    /// Show the key help.
    Help,
}

#[derive(Clone, Debug, PartialEq)]
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
    New {
        selected: usize,
    },
}

pub(crate) const KINDS: [(Kind, &str); 3] = [
    (Kind::ClaudePty, "Claude (terminal)"),
    (Kind::ClaudeSdk, "Claude (SDK)"),
    (Kind::Codex, "Codex"),
];

#[derive(Debug, Default)]
pub struct FleetView {
    selected: Option<AgentKey>,
    pub expanded: HashSet<Vec<u8>>,
    overlay: Option<Overlay>,
    /// Scroll: the first row drawn.
    top: usize,
    /// Whether raw attach is offered.
    pub attach: bool,
    /// This build's version, and the host whose daemon it talks to.
    pub version: String,
    pub local_host: Vec<u8>,
    /// Where a new agent works, as the person would write it.
    pub working_dir: String,
    /// Home as redesigned; drawn instead of the framed grid while the
    /// design variant says so.
    pub home: Home,
}

/// Whether home is the redesigned one.
fn redesigned() -> bool {
    crate::variant::get() == 1
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudePty => "claude",
        Kind::ClaudeSdk => "claude sdk",
        Kind::Codex => "codex",
        Kind::Unspecified => "agent",
    }
}

impl FleetView {
    pub fn rows(&self, fleet: &FleetState) -> Vec<FleetRow> {
        fleet_list(fleet, &self.expanded)
    }

    fn index(&self, rows: &[FleetRow]) -> usize {
        self.selected
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.card.agent == key))
            .unwrap_or(0)
    }

    pub fn selected(&self) -> Option<&AgentKey> {
        if redesigned() {
            return self.home.selected_agent();
        }
        self.selected.as_ref()
    }

    pub fn select(&mut self, agent: AgentKey) {
        self.home.select(agent.clone());
        self.selected = Some(agent);
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self) -> bool {
        if redesigned() {
            return self.home.field_text();
        }
        matches!(&self.overlay, Some(Overlay::Rename { editor, .. }) if !editor.is_empty())
    }

    pub fn kill_field(&mut self) -> bool {
        if redesigned() {
            return self.home.kill_field();
        }
        match &mut self.overlay {
            Some(Overlay::Rename { editor, .. }) => editor.kill_all(),
            _ => false,
        }
    }

    /// A bracketed paste types into the rename field; nothing else in the
    /// fleet takes text. A name is one line.
    pub fn paste(&mut self, text: &str) {
        if redesigned() {
            self.home.paste(text);
            return;
        }
        if let Some(Overlay::Rename { editor, .. }) = &mut self.overlay {
            let parts: Vec<&str> = text
                .split(['\r', '\n'])
                .filter(|part| !part.is_empty())
                .collect();
            editor.insert_str(&parts.join(" "));
        }
    }

    /// One key: an open overlay's first, so `q` and `?` quit and help only
    /// from the list.
    pub fn key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        if redesigned() {
            return self.home.key(fleet, key, self.attach);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
        }
        match key.code {
            KeyCode::Char('q') => return vec![FleetEffect::Quit],
            KeyCode::Char('?') => return vec![FleetEffect::Help],
            _ => {}
        }
        let rows = self.rows(fleet);
        let at = self.index(&rows);
        let current = rows.get(at).map(|row| row.card.agent.clone());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(row) = rows.get(at.saturating_sub(1)) {
                    self.selected = Some(row.card.agent.clone());
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(row) = rows.get((at + 1).min(rows.len().saturating_sub(1))) {
                    self.selected = Some(row.card.agent.clone());
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = rows.first().map(|row| row.card.agent.clone());
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = rows.last().map(|row| row.card.agent.clone());
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) && self.attach => {
                return current.map(FleetEffect::Attach).into_iter().collect();
            }
            KeyCode::Enter => return current.map(FleetEffect::Open).into_iter().collect(),
            KeyCode::Char('o') if self.attach => {
                return current.map(FleetEffect::Attach).into_iter().collect();
            }
            KeyCode::Char('z') | KeyCode::Char(' ') | KeyCode::Tab => {
                if let Some(row) = rows
                    .get(at)
                    .filter(|row| row.card.children > 0 || row.expanded)
                {
                    let id = row.card.agent.agent.clone();
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                }
            }
            KeyCode::Char('n') => self.overlay = Some(Overlay::New { selected: 0 }),
            KeyCode::Char('h') => self.overlay = Some(Overlay::Hosts),
            KeyCode::Char('r') => {
                if let Some(row) = rows.get(at) {
                    let mut editor = Editor::default();
                    editor.set(&row.card.name, vec![]);
                    self.overlay = Some(Overlay::Rename {
                        agent: row.card.agent.clone(),
                        editor,
                    });
                }
            }
            KeyCode::Char('s') | KeyCode::Char('d') => {
                if let Some(row) = rows.get(at) {
                    let delete = key.code == KeyCode::Char('d');
                    if delete || row.card.attention != Attention::Exited {
                        self.overlay = Some(Overlay::Confirm {
                            agent: row.card.agent.clone(),
                            name: row.card.name.clone(),
                            delete,
                        });
                    }
                }
            }
            _ => {}
        }
        vec![]
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
            Overlay::New { selected } => match key.code {
                KeyCode::Esc => vec![],
                KeyCode::Up => {
                    self.overlay = Some(Overlay::New {
                        selected: selected.saturating_sub(1),
                    });
                    vec![]
                }
                KeyCode::Down => {
                    self.overlay = Some(Overlay::New {
                        selected: (selected + 1).min(KINDS.len() - 1),
                    });
                    vec![]
                }
                KeyCode::Char(c @ '1'..='3') => vec![FleetEffect::Create {
                    kind: KINDS[c as usize - '1' as usize].0,
                }],
                KeyCode::Enter => vec![FleetEffect::Create {
                    kind: KINDS[selected].0,
                }],
                _ => {
                    self.overlay = Some(Overlay::New { selected });
                    vec![]
                }
            },
        }
    }

    /// The framed grid answers only the wheel, which it ignores.
    pub fn mouse(&mut self, fleet: &FleetState, event: MouseEvent) -> Vec<FleetEffect> {
        if redesigned() {
            return self.home.mouse(fleet, event, self.attach);
        }
        vec![]
    }

    /// Whether the screen moves with time while nothing else changes.
    pub fn animating(&self, fleet: &FleetState) -> bool {
        redesigned() && self.home.animating(fleet)
    }

    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        fleet: &FleetState,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) {
        if redesigned() {
            let place = home::Place {
                local_host: &self.local_host,
                working_dir: &self.working_dir,
                attach: self.attach,
            };
            self.home
                .draw(paint, area, fleet, footer, now_ms, theme, &place);
            return;
        }
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        // Inside the frame's two borders.
        let inner = width.saturating_sub(2);
        let capacity = height.saturating_sub(CHROME_ROWS);
        let rows = self.rows(fleet);
        if self.selected.is_none()
            || !rows
                .iter()
                .any(|row| Some(&row.card.agent) == self.selected.as_ref())
        {
            let at = self.index(&rows);
            self.selected = rows.get(at).map(|row| row.card.agent.clone());
        }
        let hosts_open = matches!(self.overlay, Some(Overlay::Hosts));
        let mut body = Vec::new();
        if hosts_open {
            body = hosts::overlay_lines(fleet, &self.local_host, inner, theme);
        } else {
            let at = self.index(&rows);
            if at < self.top {
                self.top = at;
            } else if capacity > 0 && at >= self.top + capacity {
                self.top = at + 1 - capacity;
            }
            if rows.is_empty() {
                let words = if fleet.caught_up() {
                    "No agents yet · n starts one"
                } else {
                    "Loading the fleet…"
                };
                let mut line = Line::default();
                pad_to(&mut line, NAME_COL);
                push(&mut line, words, theme.muted(), inner);
                body.push(line);
            }
            let grid = Grid::new(width);
            for (i, row) in rows.iter().enumerate().skip(self.top).take(capacity) {
                body.push(self.row_line(fleet, row, i == at, &grid, now_ms, inner, theme));
            }
        }
        body.truncate(capacity);
        body.resize(capacity, Line::default());

        let mut lines = vec![title(width, theme), self.header(fleet, &rows, inner, theme)];
        lines.push(Line::default());
        lines.extend(body);
        let mut banner = Line::default();
        if let Some((words, style)) = hosts::banner(fleet, &self.local_host, &self.version, theme) {
            pad_to(&mut banner, MARK_COL);
            push(&mut banner, words, style, inner);
        }
        lines.push(banner);
        let (status, cursor) = self.status_bar(fleet, &rows, footer, hosts_open, inner, theme);
        lines.push(status);
        let status_row = lines.len() - 1;
        let mut framed: Vec<Line<'static>> = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 {
                    line
                } else {
                    boxed(line, width, theme)
                }
            })
            .collect();
        framed.push(bottom(width, theme));
        framed.truncate(height);
        paint.render_widget(Paragraph::new(framed), area);
        if let Some(col) = cursor {
            paint.set_cursor_position(Position::new(
                area.x + (col + 1).min(width.saturating_sub(2)) as u16,
                area.y + status_row as u16,
            ));
        }
    }

    /// The agent count at the right, and how many need the person at the
    /// left when any do.
    fn header(
        &self,
        fleet: &FleetState,
        rows: &[FleetRow],
        inner: usize,
        theme: Theme,
    ) -> Line<'static> {
        let mut line = Line::default();
        let waiting = rows
            .iter()
            .filter(|row| row.depth == 0 && row.card.family_attention == Attention::NeedsYou)
            .count();
        if waiting > 0 {
            pad_to(&mut line, MARK_COL);
            push(
                &mut line,
                format!("{waiting} need{} you", if waiting == 1 { "s" } else { "" }),
                theme.accent(),
                inner,
            );
        }
        let agents = fleet.agents().count();
        let count = format!("{agents} agent{}", if agents == 1 { "" } else { "s" });
        push_right(&mut line, &count, theme.muted(), inner.saturating_sub(1));
        line
    }

    /// The bottom line: an open prompt or a notice when there is one, else
    /// the connection with the host count and the keys that fit.
    fn status_bar(
        &self,
        fleet: &FleetState,
        rows: &[FleetRow],
        footer: Option<Line<'static>>,
        hosts_open: bool,
        inner: usize,
        theme: Theme,
    ) -> (Line<'static>, Option<usize>) {
        let mut line = Line::default();
        pad_to(&mut line, MARK_COL);
        match (&self.overlay, footer) {
            (Some(Overlay::Rename { editor, .. }), _) => {
                push(&mut line, "Rename to: ", theme.muted(), inner);
                let cursor = text::line_width(&line) + editor.cursor_chars();
                push(&mut line, editor.text(), theme.text(), inner);
                push_right(
                    &mut line,
                    "enter apply  esc cancel",
                    theme.muted(),
                    inner.saturating_sub(1),
                );
                return (line, Some(cursor));
            }
            (Some(Overlay::Confirm { name, delete, .. }), _) => {
                let words = if *delete {
                    format!("Delete {name} and its history? y delete · n keep")
                } else {
                    format!("Stop {name}? It can be resumed later. y stop · n keep")
                };
                push(&mut line, words, theme.warn(), inner);
                return (line, None);
            }
            (Some(Overlay::New { selected }), _) => {
                push(&mut line, "New: ", theme.muted(), inner);
                for (i, (_, label)) in KINDS.iter().enumerate() {
                    let style = if i == *selected {
                        theme.emphasis()
                    } else {
                        theme.muted()
                    };
                    push(&mut line, format!("{}. {label}  ", i + 1), style, inner);
                }
                push(&mut line, "enter start · esc cancel", theme.muted(), inner);
                return (line, None);
            }
            (_, Some(footer)) => {
                // The app's lines start with their own margin.
                let mut spans = footer.spans.into_iter().peekable();
                while spans
                    .peek()
                    .is_some_and(|span| span.content.trim().is_empty())
                {
                    spans.next();
                }
                line.spans.extend(spans);
                return (line, None);
            }
            _ => {}
        }
        let trusted: Vec<_> = fleet
            .hosts()
            .filter(|host| host.trust() == Trust::Trusted)
            .collect();
        let out = trusted
            .iter()
            .filter(|host| host.presence() != Presence::Online || host.revoked == Some(true))
            .count();
        let (dot, dot_style, summary) = match fleet.connection() {
            Connection::Live => {
                let mut words = format!(
                    "connected · {} host{}",
                    trusted.len(),
                    if trusted.len() == 1 { "" } else { "s" }
                );
                if out > 0 {
                    words.push_str(&format!(" · {out} offline"));
                }
                ("●", theme.ok(), words)
            }
            Connection::Connecting => ("◌", theme.muted(), "connecting".to_owned()),
            Connection::Reconnecting => ("◌", theme.warn(), "reconnecting to amux".to_owned()),
        };
        push(&mut line, dot, dot_style, inner);
        pad_to(&mut line, BADGE_COL);
        push(&mut line, summary, theme.text(), inner);
        let hints_at = HINTS_COL.max(text::line_width(&line) + 2);
        let fits = |hints: &str| hints_at + text::str_width(hints) < inner;
        let hints = if hosts_open {
            Some("esc close".to_owned())
        } else {
            self.hints(rows).into_iter().find(|hints| fits(hints))
        };
        if let Some(hints) = hints.filter(|hints| fits(hints)) {
            pad_to(&mut line, hints_at);
            line.spans.push(Span::styled(hints, theme.muted()));
        }
        (line, None)
    }

    /// The key hints, longest first: the ways into the selected row lead,
    /// then `z` where something folds; each is dropped in turn until the
    /// block fits, and a hint that would name a dead key is never offered.
    /// Over an attached terminal `o terminal` is the only way back to it, so
    /// it outlasts every other hint.
    fn hints(&self, rows: &[FleetRow]) -> Vec<String> {
        let entry = if self.attach {
            "enter open  o terminal"
        } else {
            "enter open"
        };
        let folds = rows
            .iter()
            .any(|row| row.card.children > 0 || row.depth > 0);
        let rest = if folds {
            "n new  r rename  s stop  d delete  z fold  h hosts  q quit  ? help"
        } else {
            "n new  r rename  s stop  d delete  h hosts  q quit  ? help"
        };
        let short = "n new  h hosts  q quit  ? help";
        if self.attach {
            vec![
                format!("{entry}  {rest}"),
                format!("o terminal  {rest}"),
                format!("o terminal  {short}"),
                "o terminal".to_owned(),
            ]
        } else {
            vec![
                format!("{entry}  {rest}"),
                rest.to_owned(),
                short.to_owned(),
            ]
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row_line(
        &self,
        fleet: &FleetState,
        row: &FleetRow,
        selected: bool,
        grid: &Grid,
        now_ms: i64,
        inner: usize,
        theme: Theme,
    ) -> Line<'static> {
        let card = &row.card;
        let host = fleet.host(&card.agent.host);
        // A live agent on a host out of reach is as it last said: nothing on
        // its row is current, so none of it claims the body text.
        let unreached = card.attention != Attention::Exited
            && host.is_some_and(|host| {
                host.presence() == Presence::Offline || host.revoked == Some(true)
            });
        let mut line = Line::default();
        if selected {
            pad_to(&mut line, MARK_COL);
            line.spans.push(Span::styled("▎", theme.focus_bar()));
        }
        let folded = card.children > 0 && !row.expanded;
        let (glyph, glyph_style, word, word_style) = if unreached {
            ("–", theme.muted(), "–".to_owned(), theme.muted())
        } else if folded && card.family_attention == Attention::NeedsYou {
            // A folded family wears its loudest member's mark: the row
            // stands in for everyone behind it.
            let (word, style) = status_word(card, theme);
            ("!", theme.accent(), word, style)
        } else {
            let (glyph, style) = glyph(card, theme);
            let (word, word_style) = status_word(card, theme);
            (glyph, style, word, word_style)
        };
        pad_to(&mut line, BADGE_COL);
        if glyph != " " {
            line.spans.push(Span::styled(glyph, glyph_style));
        }
        // Members indent one step per generation, and a folded family's
        // count eats into the same name cell, so a family never pushes the
        // grid out of line.
        let indent = 2 * row.depth as usize;
        let marker = if card.children > 0 {
            format!(" {}{}", if row.expanded { "▾" } else { "▸" }, card.children)
        } else {
            String::new()
        };
        let name = if card.name.is_empty() {
            "unnamed"
        } else {
            &card.name
        };
        let name_style = if selected {
            theme.emphasis()
        } else if unreached {
            theme.muted()
        } else {
            theme.text()
        };
        pad_to(&mut line, NAME_COL + indent);
        let room = NAME_WIDTH.saturating_sub(indent + text::str_width(&marker));
        line.spans
            .push(Span::styled(text::ellipsize(name, room), name_style));
        if !marker.is_empty() {
            let style = if folded && card.family_attention == Attention::NeedsYou {
                theme.accent()
            } else {
                theme.muted()
            };
            line.spans.push(Span::styled(marker, style));
        }
        pad_to(&mut line, KIND_COL);
        push(
            &mut line,
            text::ellipsize(kind_word(card.kind), KIND_WIDTH),
            theme.muted(),
            inner,
        );
        let cell = match host {
            Some(host) => hosts::host_cell(host, &self.local_host),
            None if !card.host.is_empty() => card.host.clone(),
            None => "?".to_owned(),
        };
        pad_to(&mut line, HOST_COL);
        push(
            &mut line,
            text::ellipsize(&cell, grid.host_width),
            if unreached {
                theme.muted()
            } else {
                theme.text()
            },
            inner,
        );
        pad_to(&mut line, grid.age_col);
        push(
            &mut line,
            text::age(now_ms, card.last_activity_ms),
            theme.muted(),
            inner,
        );
        if inner >= grid.status_col + STATUS_WIDTH {
            pad_to(&mut line, grid.status_col);
            push(&mut line, word, word_style, inner);
        }
        let about = match card.attention {
            Attention::Exited => card
                .exit_cause
                .clone()
                .filter(|cause| !cause.is_empty() && cause != "exited" && cause != FINISHED),
            _ => card
                .working_on
                .clone()
                .filter(|working| !working.is_empty()),
        };
        if let Some(about) = about.filter(|_| inner >= grid.about_col + ABOUT_MIN) {
            pad_to(&mut line, grid.about_col);
            push(&mut line, text::first_line(&about), theme.muted(), inner);
        }
        line
    }
}

/// Rows the frame, header, banner and status bar take.
const CHROME_ROWS: usize = 6;
/// Columns inside the frame's left border.
const MARK_COL: usize = 1;
const BADGE_COL: usize = 3;
const NAME_COL: usize = 5;
const NAME_WIDTH: usize = 21;
const KIND_COL: usize = 27;
const KIND_WIDTH: usize = 10;
const HOST_COL: usize = 39;
const HOST_WIDTH: usize = 10;
const AGE_COL: usize = 50;
const STATUS_COL: usize = 55;
const STATUS_WIDTH: usize = 10;
const ABOUT_COL: usize = 66;
/// The least room the working-on summary is drawn in: the most expendable
/// cell, so a cramped screen drops it first, then the status word.
const ABOUT_MIN: usize = 10;
/// From this frame width the host cell widens, and every cell after it
/// moves right with it.
const WIDE_GRID: usize = 96;
const WIDE_EXTRA: usize = 8;
/// Where the key hints start on the status bar, past the connection.
const HINTS_COL: usize = 24;
/// The exit cause of a one-shot child that finished its work.
const FINISHED: &str = "finished";

struct Grid {
    host_width: usize,
    age_col: usize,
    status_col: usize,
    about_col: usize,
}

impl Grid {
    fn new(width: usize) -> Self {
        let extra = if width >= WIDE_GRID { WIDE_EXTRA } else { 0 };
        Grid {
            host_width: HOST_WIDTH + extra,
            age_col: AGE_COL + extra,
            status_col: STATUS_COL + extra,
            about_col: ABOUT_COL + extra,
        }
    }
}

/// The attention column: needs you, working, starting, finished, exited
/// and idle each wear their own mark; only needs-you is loud.
fn glyph(card: &FleetCard, theme: Theme) -> (&'static str, Style) {
    match card.attention {
        Attention::NeedsYou => ("!", theme.accent()),
        Attention::Working => ("⋯", theme.muted()),
        Attention::Starting => ("◌", theme.muted()),
        Attention::Idle => (" ", theme.muted()),
        Attention::Exited if card.exit_cause.as_deref() == Some(FINISHED) => ("✓", theme.ok()),
        Attention::Exited => ("×", theme.muted()),
    }
}

fn status_word(card: &FleetCard, theme: Theme) -> (String, Style) {
    let word = match card.attention {
        Attention::NeedsYou => return ("needs you".to_owned(), theme.accent()),
        Attention::Working => "working",
        Attention::Starting => "starting",
        Attention::Idle => "idle",
        Attention::Exited if card.exit_cause.as_deref() == Some(FINISHED) => "finished",
        Attention::Exited => "exited",
    };
    (word.to_owned(), theme.muted())
}

/// The top border with the product name as the screen's title.
fn title(width: usize, theme: Theme) -> Line<'static> {
    let rule = "─".repeat(width.saturating_sub(8));
    Line::from(vec![
        Span::styled("┌ ", theme.muted()),
        Span::styled("amux", theme.emphasis()),
        Span::styled(format!(" {rule}┐"), theme.muted()),
    ])
}

fn bottom(width: usize, theme: Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("└{}┘", "─".repeat(width.saturating_sub(2))),
        theme.muted(),
    ))
}

/// `line` between the frame's borders: clipped or padded to the inside.
fn boxed(line: Line<'static>, width: usize, theme: Theme) -> Line<'static> {
    let inner = width.saturating_sub(2);
    let mut framed = Line::from(Span::styled("│", theme.muted()));
    let mut used = 0;
    for span in line.spans {
        let room = inner.saturating_sub(used);
        if room == 0 {
            break;
        }
        let content = text::clip_to_width(&span.content, room).to_owned();
        used += text::str_width(&content);
        framed.spans.push(Span::styled(content, span.style));
    }
    let mut body = Line::from(framed.spans.split_off(1));
    pad_to(&mut body, inner);
    framed.spans.extend(body.spans);
    framed.spans.push(Span::styled("│", theme.muted()));
    framed
}
