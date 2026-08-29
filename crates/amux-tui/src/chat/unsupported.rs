//! The chat of an agent whose protocol this build carries no fold for.
//!
//! An agent kind can exist before its reader does. When it does, the honest
//! screen is one that names the protocol it cannot read and still lets a
//! person leave — not a missing chat, and not a frame invented out of rows
//! nobody parsed. Nothing here reads a layer, because there is none.

use amux_ui::{AgentId, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::composer::Composer;
use crate::render::{
    FrameContext, Theme, blank_line, bottom_border, clip_to_width, finish_line, line_len, new_line,
    push_span, str_width,
};
use crate::view::{QuitGuard, UiAction};

const GLYPH_COL: usize = 2;
const MIN_WIDTH: usize = 24;
const MIN_HEIGHT: usize = 10;

#[derive(Clone, Debug)]
pub(crate) struct View {
    pub agent: AgentId,
    /// The protocol this chat is declining to render, named on screen so a
    /// person can say what is missing without reading the source.
    pub protocol: &'static str,
    /// Owned so the outer chat has one uniform shape; no key ever reaches
    /// it, because there is nothing here to send to.
    pub composer: Composer,
    pub quit_guard: QuitGuard,
    pub leader: char,
    pub pending_leader: bool,
    pub kitty: bool,
    pub help: bool,
}

impl View {
    pub(crate) fn open(agent: AgentId, protocol: &'static str, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            protocol,
            composer: Composer::default(),
            quit_guard: QuitGuard::default(),
            leader,
            pending_leader: false,
            kitty,
            help: false,
        }
    }
}

/// Only the chords that leave: closing the chat, quitting, and stepping to
/// the next agent in the family. Everything else is inert on purpose — a
/// key that looks like it typed somewhere would be a lie.
pub(crate) fn handle_chat_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    now: DateTime<Utc>,
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if chat.pending_leader {
        chat.pending_leader = false;
        chat.quit_guard.disarm();
        return match key.code {
            KeyCode::Char('s') => Some(UiAction::CloseChat),
            KeyCode::Char('d') => Some(UiAction::Quit),
            KeyCode::Char('n') => {
                crate::chat::next_in_family(model, chat.agent).map(UiAction::OpenChat)
            }
            _ => None,
        };
    }
    if let KeyCode::Char(c) = key.code
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && c == chat.leader
    {
        chat.pending_leader = true;
        chat.quit_guard.disarm();
        return None;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        // No composer to clear, so every press is a quit press.
        if chat.quit_guard.press(now) {
            return Some(UiAction::Quit);
        }
        return None;
    }
    chat.quit_guard.disarm();
    None
}

pub(crate) fn build_chat_lines(
    model: &Model,
    chat: &View,
    ctx: &FrameContext,
) -> Vec<Line<'static>> {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return vec![Line::from("amux: terminal too small")];
    }
    let theme = ctx.theme;
    let banner = crate::chat::family_banner(model, chat.agent);

    let mut lines = Vec::with_capacity(height);
    lines.push(top_border(width, theme));
    lines.push(header_line(model, chat, width, theme));
    // A child's ask still reaches its parent here: this chat cannot read
    // its own agent's rows, which says nothing about the family's.
    if let Some(banner) = &banner {
        lines.push(banner_line(&banner.row(false, chat.leader), width, theme));
    }
    lines.push(rule(width, theme));

    lines.push(blank_line(width));
    lines.push(text_line(
        "this chat is unsupported in this build",
        width,
        theme.warn(),
    ));
    lines.push(blank_line(width));
    lines.push(text_line(
        &format!("{} carries rows nothing here folds.", chat.protocol),
        width,
        theme.text(),
    ));
    lines.push(text_line(
        "The agent keeps running; only its chat is missing.",
        width,
        theme.muted(),
    ));

    // The frame is fixed-height like every other chat, and the row above
    // the border is where every chat states its keys.
    let hint = if chat.quit_guard.is_armed() {
        text_line(QuitGuard::HINT, width, theme.warn())
    } else {
        text_line(
            &format!(
                "C-{leader} s close · C-{leader} n next agent · C-{leader} d quit",
                leader = chat.leader
            ),
            width,
            theme.muted(),
        )
    };
    while lines.len() + 2 < height {
        lines.push(blank_line(width));
    }
    lines.push(hint);
    lines.push(bottom_border(width));
    lines.truncate(height);
    lines
}

fn top_border(width: usize, theme: Theme) -> Line<'static> {
    let mut text = String::from("┌");
    while text.chars().count() < width - 1 {
        text.push('─');
    }
    text.push('┐');
    Line::from(Span::styled(text, theme.muted()))
}

fn rule(width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    let mut text = String::new();
    while str_width(&text) < width.saturating_sub(2) {
        text.push('─');
    }
    line.spans.push(Span::styled(text, theme.muted()));
    finish_line(&mut line, width);
    line
}

fn header_line(model: &Model, chat: &View, width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    if let Some(card) = model.agent(chat.agent) {
        let host = model.host_name(card.agent.host_id).unwrap_or("?");
        push_span(&mut line, GLYPH_COL, card.display_name(), theme.text());
        line.spans.push(Span::styled(
            format!(" · {} @ {host}", card.agent.kind),
            theme.muted(),
        ));
        line.spans.push(Span::styled(
            crate::chat::subagent_marker(model, chat.agent),
            theme.muted(),
        ));
    }
    let left = "chat · ";
    let word = "unsupported";
    let col = width
        .saturating_sub(2 + str_width(left) + str_width(word))
        .max(line_len(&line) + 1);
    push_span(&mut line, col, left, theme.muted());
    line.spans.push(Span::styled(word, theme.warn()));
    finish_line(&mut line, width);
    line
}

fn banner_line(text: &str, width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, "!", theme.warn());
    push_span(
        &mut line,
        GLYPH_COL + 2,
        clip_to_width(text, width.saturating_sub(GLYPH_COL + 3)),
        theme.warn(),
    );
    finish_line(&mut line, width);
    line
}

fn text_line(text: &str, width: usize, style: ratatui::style::Style) -> Line<'static> {
    let mut line = new_line();
    push_span(
        &mut line,
        GLYPH_COL,
        clip_to_width(text, width.saturating_sub(GLYPH_COL + 1)),
        style,
    );
    finish_line(&mut line, width);
    line
}
