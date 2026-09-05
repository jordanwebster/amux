//! The chat of an agent whose protocol this build carries no fold for.
//!
//! An agent kind can exist before its reader does. When it does, the honest
//! screen is one that names the protocol it cannot read and still lets a
//! person leave — not a missing chat, and not a frame invented out of rows
//! nobody parsed. Nothing here reads a layer, because there is none.

use amux_ui::{AgentId, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::Line;
use serde::{Deserialize, Serialize};

use super::blocks::{GLYPH_COL, TEXT_COL, paint_header};
use super::frame::{BlockKey, BlockKind, ChatFrameParts, FeedBlocks, PaintedBlock};
use crate::composer::Composer;
use crate::render::{FrameContext, Theme, push_span};
use crate::view::{QuitGuard, UiAction};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct View {
    pub agent: AgentId,
    /// The protocol this chat is declining to render, named on screen so a
    /// person can say what is missing without reading the source. Owned
    /// rather than borrowed from the protocol table: the name has to
    /// survive a round trip through bytes, and a build that reads a view
    /// recorded by another one cannot promise the same statics.
    pub protocol: String,
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
            protocol: protocol.to_string(),
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

/// The placeholder as shared-frame parts: a real header, the family banner
/// a child can still raise here, one feed block that states what is
/// missing, and a bottom row naming the keys that leave.
pub(crate) fn frame_parts(model: &Model, chat: &View, ctx: &FrameContext) -> ChatFrameParts {
    let width = ctx.viewport.0 as usize;
    let theme = ctx.theme;
    // A child's ask still reaches its parent here: this chat cannot read
    // its own agent's rows, which says nothing about the family's.
    let banner = super::family_banner(model, chat.agent).map(|banner| {
        let mut line = Line::default();
        push_span(&mut line, GLYPH_COL, "⚠".to_string(), theme.warn());
        push_span(
            &mut line,
            TEXT_COL,
            banner.row(false, chat.leader),
            theme.warn(),
        );
        line
    });
    ChatFrameParts {
        header: header_row(model, chat, theme, width),
        banner,
        feed: FeedBlocks {
            blocks: vec![placeholder_block(chat, theme)],
            history_truncated: false,
            loading: false,
        },
        activity: None,
        bottom: vec![hint_row(chat, theme)],
        overlay: None,
    }
}

fn header_row(model: &Model, chat: &View, theme: Theme, width: usize) -> Line<'static> {
    let name = match model.agent(chat.agent) {
        Some(card) => format!(
            "{} · {} @ {}{}",
            card.display_name(),
            card.agent.kind.provider(),
            model.host_name(card.agent.host_id).unwrap_or("?"),
            super::subagent_marker(model, chat.agent),
        ),
        None => String::new(),
    };
    paint_header(
        &name,
        ("unsupported", theme.warn()),
        "chat · ",
        theme,
        width,
    )
}

fn placeholder_block(chat: &View, theme: Theme) -> PaintedBlock {
    let row = |text: &str, style| {
        let mut line = Line::default();
        push_span(&mut line, GLYPH_COL, text.to_string(), style);
        line
    };
    let missing = format!("{} carries rows nothing here folds.", chat.protocol);
    let running = "The agent keeps running; only its chat is missing.";
    let headline = "this chat is unsupported in this build";
    PaintedBlock {
        key: BlockKey(0),
        kind: BlockKind::Activity,
        copy_text: format!("{headline}\n{missing}\n{running}"),
        lines: vec![
            row(headline, theme.warn()),
            Line::default(),
            row(&missing, theme.text()),
            row(running, theme.muted()),
        ],
        run: None,
    }
}

fn hint_row(chat: &View, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    if chat.quit_guard.is_armed() {
        push_span(
            &mut line,
            GLYPH_COL,
            QuitGuard::HINT.to_string(),
            theme.warn(),
        );
    } else {
        push_span(
            &mut line,
            GLYPH_COL,
            format!(
                "C-{leader} s close · C-{leader} n next agent · C-{leader} d quit",
                leader = chat.leader
            ),
            theme.muted(),
        );
    }
    line
}
