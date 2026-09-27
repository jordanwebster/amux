//! The fleet: every family head, loudest first, with expanded families'
//! members beneath their parents; create, rename, stop and delete; the
//! hosts overlay. Home: a chat always returns here.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{AgentKey, Attention, Connection, FleetState};
use ui_view::{FleetRow, fleet_list};
use wire::{Kind, Presence, Trust};

use crate::editor::Editor;
use crate::hosts;
use crate::text::{self, push, push_right};
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
    Rename {
        agent: AgentKey,
        name: String,
    },
    Stop(AgentKey),
    Delete(AgentKey),
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

const KINDS: [(Kind, &str); 3] = [
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
}

fn glyph(attention: Attention, theme: Theme) -> (&'static str, ratatui::style::Style) {
    match attention {
        Attention::NeedsYou => ("●", theme.accent()),
        Attention::Working => ("◐", theme.ok()),
        Attention::Starting => ("◌", theme.muted()),
        Attention::Idle => ("○", theme.muted()),
        Attention::Exited => ("·", theme.muted()),
    }
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
        self.selected.as_ref()
    }

    pub fn select(&mut self, agent: AgentKey) {
        self.selected = Some(agent);
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self) -> bool {
        matches!(&self.overlay, Some(Overlay::Rename { editor, .. }) if !editor.is_empty())
    }

    pub fn kill_field(&mut self) -> bool {
        match &mut self.overlay {
            Some(Overlay::Rename { editor, .. }) => editor.kill_all(),
            _ => false,
        }
    }

    pub fn key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
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

    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        fleet: &FleetState,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        if matches!(self.overlay, Some(Overlay::Hosts)) {
            let mut lines = hosts::overlay_lines(fleet, width, theme);
            lines.resize(height, Line::default());
            paint.render_widget(Paragraph::new(lines), area);
            return;
        }
        let rows = self.rows(fleet);
        if self.selected.is_none()
            || !rows
                .iter()
                .any(|row| Some(&row.card.agent) == self.selected.as_ref())
        {
            let at = self.index(&rows);
            self.selected = rows.get(at).map(|row| row.card.agent.clone());
        }
        let mut lines = vec![self.header(fleet, &rows, width, theme), Line::default()];
        let body = height.saturating_sub(4);
        let at = self.index(&rows);
        if at < self.top {
            self.top = at;
        } else if body > 0 && at >= self.top + body {
            self.top = at + 1 - body;
        }
        if rows.is_empty() {
            let words = if fleet.caught_up() {
                "No agents yet · n starts one"
            } else {
                "Loading the fleet…"
            };
            let mut line = Line::from(Span::raw("    "));
            push(&mut line, words, theme.muted(), width);
            lines.push(line);
        }
        for (i, row) in rows.iter().enumerate().skip(self.top).take(body) {
            lines.push(self.row_line(row, i == at, now_ms, width, theme));
        }
        let mut cursor = None;
        let used = lines.len();
        lines.resize(height.saturating_sub(1).max(used), Line::default());
        let foot = match (&self.overlay, footer) {
            (Some(Overlay::Rename { editor, .. }), _) => {
                let mut line = Line::from(Span::raw("  "));
                push(&mut line, "Rename to: ", theme.muted(), width);
                cursor = Some(text::line_width(&line) + editor.cursor_chars());
                push(&mut line, editor.text(), theme.text(), width);
                line
            }
            (Some(Overlay::Confirm { name, delete, .. }), _) => {
                let mut line = Line::from(Span::raw("  "));
                let words = if *delete {
                    format!("Delete {name} and its history? y delete · n keep")
                } else {
                    format!("Stop {name}? It can be resumed later. y stop · n keep")
                };
                push(&mut line, words, theme.warn(), width);
                line
            }
            (Some(Overlay::New { selected }), _) => {
                let mut line = Line::from(Span::raw("  New: "));
                for (i, (_, label)) in KINDS.iter().enumerate() {
                    let style = if i == *selected {
                        theme.emphasis()
                    } else {
                        theme.muted()
                    };
                    push(&mut line, format!("{}. {label}  ", i + 1), style, width);
                }
                push(&mut line, "enter start · esc cancel", theme.muted(), width);
                line
            }
            (_, Some(footer)) => footer,
            _ => {
                let mut words = String::from("enter open");
                if self.attach {
                    words.push_str(" · o terminal");
                }
                words.push_str(
                    " · n new · r rename · s stop · d delete · z family · h hosts · ? help",
                );
                let mut line = Line::from(Span::raw("  "));
                push(&mut line, words, theme.muted(), width);
                line
            }
        };
        let foot_row = lines.len();
        lines.push(foot);
        paint.render_widget(Paragraph::new(lines), area);
        if let Some(col) = cursor {
            paint.set_cursor_position(Position::new(
                area.x + col.min(width.saturating_sub(1)) as u16,
                area.y + foot_row as u16,
            ));
        }
    }

    fn header(
        &self,
        fleet: &FleetState,
        rows: &[FleetRow],
        width: usize,
        theme: Theme,
    ) -> Line<'static> {
        let mut line = Line::from(Span::raw("  "));
        push(&mut line, "amux", theme.emphasis(), width);
        let agents = fleet.agents().count();
        push(
            &mut line,
            format!(" · {agents} agent{}", if agents == 1 { "" } else { "s" }),
            theme.muted(),
            width,
        );
        let waiting = rows
            .iter()
            .filter(|row| row.depth == 0 && row.card.family_attention == Attention::NeedsYou)
            .count();
        if waiting > 0 {
            push(
                &mut line,
                format!(" · {waiting} need you"),
                theme.accent(),
                width,
            );
        }
        let trusted: Vec<_> = fleet
            .hosts()
            .filter(|h| h.trust() == Trust::Trusted)
            .collect();
        let away = trusted
            .iter()
            .filter(|h| h.presence() != Presence::Online)
            .count();
        let right = match fleet.connection() {
            Connection::Connecting => "connecting".to_owned(),
            Connection::Reconnecting => "reconnecting to amux".to_owned(),
            Connection::Live if trusted.len() > 1 && away > 0 => {
                format!("{} hosts · {away} away", trusted.len())
            }
            Connection::Live if trusted.len() > 1 => format!("{} hosts", trusted.len()),
            Connection::Live => String::new(),
        };
        push_right(&mut line, &right, theme.muted(), width);
        line
    }

    fn row_line(
        &self,
        row: &FleetRow,
        selected: bool,
        now_ms: i64,
        width: usize,
        theme: Theme,
    ) -> Line<'static> {
        let card = &row.card;
        let mut line = Line::from(Span::styled(
            if selected { "▌ " } else { "  " },
            theme.focus_bar(),
        ));
        line.spans.push(Span::raw("  ".repeat(row.depth as usize)));
        let (mark, style) = glyph(card.attention, theme);
        push(&mut line, format!("{mark} "), style, width);
        let name = if card.name.is_empty() {
            "unnamed"
        } else {
            &card.name
        };
        let name_style = if selected {
            theme.emphasis()
        } else {
            theme.text()
        };
        push(&mut line, text::ellipsize(name, 24), name_style, width);
        if card.children > 0 {
            let fold = if row.expanded { "▾" } else { "▸" };
            let family = if card.family_attention == Attention::NeedsYou && !row.expanded {
                theme.accent()
            } else {
                theme.muted()
            };
            push(
                &mut line,
                format!(" {fold}{}", card.children),
                family,
                width,
            );
        }
        let pad = 34usize.saturating_sub(text::line_width(&line));
        line.spans.push(Span::raw(" ".repeat(pad)));
        let mut about = kind_word(card.kind).to_owned();
        if !card.host.is_empty() {
            about.push_str(&format!(" @ {}", card.host));
            if card.host_presence != Presence::Online && card.host_presence != Presence::Unspecified
            {
                about.push_str(" (away)");
            }
        }
        push(&mut line, format!("{about:<24}  "), theme.muted(), width);
        let status = match card.attention {
            Attention::NeedsYou => ("needs you".to_owned(), theme.accent()),
            Attention::Exited => (
                card.exit_cause
                    .as_ref()
                    .filter(|cause| !cause.is_empty())
                    .map(|cause| format!("exited · {cause}"))
                    .unwrap_or_else(|| "exited".into()),
                theme.muted(),
            ),
            Attention::Starting => ("starting".to_owned(), theme.muted()),
            _ => match &card.working_on {
                Some(working) if !working.is_empty() => (working.clone(), theme.text()),
                _ if card.attention == Attention::Working => ("working".to_owned(), theme.muted()),
                _ => ("idle".to_owned(), theme.muted()),
            },
        };
        let age = text::age(now_ms, card.last_activity_ms);
        let room = width.saturating_sub(text::str_width(&age) + 2);
        push(&mut line, status.0, status.1, room);
        push_right(&mut line, &age, theme.muted(), width);
        line
    }
}
