use amux_ui::codex::{CodexCommand, CodexPhase};
use amux_ui::{Command, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::View;
use crate::chat::inline::{InlineAsk, InlineOutcome};
use crate::chat::viewport::ScrollIntent;
use crate::composer;
use crate::view::UiAction;

pub(crate) fn handle_chat_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
    now: DateTime<Utc>,
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    chat.scroll_intent = None;
    chat.send_failure = None;
    chat.answer_failure = None;
    chat.reconcile(model);

    if chat.pending_leader {
        chat.pending_leader = false;
        chat.quit_guard.disarm();
        return match key.code {
            KeyCode::Char('s') => Some(UiAction::CloseChat),
            KeyCode::Char('d') => Some(UiAction::Quit),
            // `<leader> n`: the next agent in this family (U3). Family
            // navigation is chrome navigation, so it lives with the other
            // two — which also means it works from a read-only chat, from
            // under an open panel, and never leaks into a draft.
            KeyCode::Char('n') => {
                crate::chat::next_in_family(model, chat.agent).map(UiAction::OpenChat)
            }
            // `<leader> m`: open or close the completion bodies (U4).
            // A display toggle rather than a per-row affordance: the feed
            // has no cursor to point at one row with, and a chord that
            // does the same thing everywhere is teachable in one line.
            KeyCode::Char('m') => {
                chat.reports_open = !chat.reports_open;
                None
            }
            // `<leader> a`: dock the ask the banner names, or send it
            // back (U2). A leader chord because it is the same act in
            // both chats and must never reach a draft or a panel; the
            // banner names it, and only while it would open something.
            KeyCode::Char('a') => {
                toggle_inline_ask(chat, model);
                None
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
        let composer_focused = !chat.help
            && !chat.read_only(model)
            && chat.inline_ask.is_none()
            && model
                .codex(chat.agent)
                .is_none_or(|layer| layer.ask_head().is_none())
            && !matches!(
                amux_ui::codex::phase(model, chat.agent),
                CodexPhase::BlockedUnsupported { .. }
            );
        if composer_focused && !chat.composer.is_empty() {
            chat.composer.kill_all();
            chat.quit_guard.note_clear();
        } else if chat.quit_guard.press(now) {
            return Some(UiAction::Quit);
        }
        return None;
    }
    chat.quit_guard.disarm();

    if chat.help {
        chat.help = false;
        return None;
    }

    if chat.read_only(model) {
        return readonly_key(chat, model, key, viewport);
    }

    // A docked child ask owns the composer area and its keys, exactly as
    // this chat's own ask would — including Ctrl+X, which interrupts the
    // agent whose ask is on screen.
    if chat.inline_ask.is_some() {
        return inline_key(chat, model, key, viewport);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Char('x') && ctrl {
        return amux_ui::codex::allows_interrupt(model, chat.agent).then_some({
            UiAction::Dispatch(Command::Codex(CodexCommand::Interrupt {
                agent: chat.agent,
            }))
        });
    }

    if model
        .codex(chat.agent)
        .and_then(|layer| layer.ask_head())
        .is_some()
    {
        return approval_key(chat, model, key, viewport);
    }
    if matches!(
        amux_ui::codex::phase(model, chat.agent),
        CodexPhase::BlockedUnsupported { .. }
    ) {
        scroll_keys(chat, model, &key, viewport);
        return None;
    }

    match key.code {
        KeyCode::Esc => {
            if chat.composer.is_empty() {
                follow(chat);
            }
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            chat.composer.insert_newline();
        }
        KeyCode::Enter => return send(chat, model),
        KeyCode::Char('j') if ctrl => chat.composer.insert_newline(),
        KeyCode::Char('p') if ctrl => chat.composer.up(),
        KeyCode::Char('n') if ctrl => chat.composer.down(),
        KeyCode::Up => chat.composer.up(),
        KeyCode::Down => chat.composer.down(),
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        KeyCode::Home if ctrl => jump_top(chat, model, viewport),
        KeyCode::End if ctrl => follow(chat),
        KeyCode::Char('?') if !ctrl && chat.composer.is_empty() => chat.help = true,
        KeyCode::Tab | KeyCode::BackTab => {}
        _ => {
            composer::readline_key(&mut chat.composer, &key);
        }
    }
    None
}

pub(crate) fn handle_chat_paste(chat: &mut View, model: &Model, text: &str) {
    chat.send_failure = None;
    chat.answer_failure = None;
    chat.quit_guard.disarm();
    chat.reconcile(model);
    // A docked child ask covers the composer: its open field takes the
    // paste, and it drops when there is none. Nothing typed behind a
    // guest panel reaches this agent's draft.
    if let Some(inline) = chat.inline_ask.as_mut() {
        let one_line = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', " ");
        crate::chat::inline::handle_paste(inline, &one_line);
        return;
    }
    if chat.help
        || chat.read_only(model)
        || model
            .codex(chat.agent)
            .and_then(|layer| layer.ask_head())
            .is_some()
        || matches!(
            amux_ui::codex::phase(model, chat.agent),
            CodexPhase::BlockedUnsupported { .. }
        )
    {
        return;
    }
    chat.composer.paste(text);
}

/// `<leader> a`: dock the ask the banner names, or send it back (U2).
/// Nothing happens when the banner names a child with no panel to dock —
/// a finished child needs a person, not an answer — which is exactly the
/// condition the banner withholds the chord under.
fn toggle_inline_ask(chat: &mut View, model: &Model) {
    if chat.inline_ask.take().is_some() {
        return;
    }
    let Some(banner) = crate::chat::family_banner(model, chat.agent) else {
        return;
    };
    if !crate::chat::inline::can_open(model, chat.agent, banner.child) {
        return;
    }
    chat.inline_ask = InlineAsk::open(model, banner.child);
}

/// Keys while a child's ask is docked here: the child's layer's own
/// panel first, this chat's feed scrolling as the fallback — the
/// conversation stays readable behind a guest exactly as behind an ask
/// of this agent's own.
fn inline_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let inline = chat.inline_ask.as_mut()?;
    match crate::chat::inline::handle_key(model, inline, &key) {
        InlineOutcome::Dispatch(command) => Some(UiAction::Dispatch(command)),
        InlineOutcome::Close => {
            chat.inline_ask = None;
            None
        }
        InlineOutcome::Handled => None,
        InlineOutcome::NotHandled => {
            scroll_keys(chat, model, &key, viewport);
            None
        }
    }
}

fn send(chat: &mut View, model: &Model) -> Option<UiAction> {
    if chat.composer.is_empty() {
        return None;
    }
    let text = chat.composer.text();
    let native = if amux_ui::codex::allows_steer(model, chat.agent) {
        CodexCommand::Steer {
            agent: chat.agent,
            text,
        }
    } else if amux_ui::codex::allows_prompt(model, chat.agent) {
        CodexCommand::Prompt {
            agent: chat.agent,
            text,
        }
    } else {
        return None;
    };
    chat.composer.clear_for_send();
    Some(UiAction::Dispatch(Command::Codex(native)))
}

fn approval_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let ask = model.codex(chat.agent)?.ask_head()?;
    let count = ask.actions.len();
    let allows_answer = amux_ui::codex::allows_answer(model, chat.agent);
    match key.code {
        KeyCode::Char(digit @ '1'..='9') if allows_answer => {
            let index = digit as usize - '1' as usize;
            if index < count {
                chat.approval_cursor = index;
            }
        }
        KeyCode::Up if allows_answer => {
            chat.approval_cursor = chat.approval_cursor.saturating_sub(1);
        }
        KeyCode::Down if allows_answer => {
            chat.approval_cursor = (chat.approval_cursor + 1).min(count.saturating_sub(1));
        }
        KeyCode::Enter => {
            if !allows_answer {
                return None;
            }
            let action = ask.actions.get(chat.approval_cursor)?;
            let decision = action.decision()?;
            return Some(UiAction::Dispatch(Command::Codex(CodexCommand::Answer {
                agent: chat.agent,
                request_id: ask.request_id.clone(),
                decision,
            })));
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            scroll_keys(chat, model, &key, viewport);
        }
        KeyCode::Esc => {}
        _ => {
            scroll_keys(chat, model, &key, viewport);
        }
    }
    None
}

fn readonly_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    match key.code {
        KeyCode::Char('q') => return Some(UiAction::CloseChat),
        KeyCode::Esc => follow(chat),
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        KeyCode::Up | KeyCode::Char('k') => line_up(chat, model, viewport),
        KeyCode::Down | KeyCode::Char('j') => line_down(chat, model, viewport),
        KeyCode::Home | KeyCode::Char('g') => jump_top(chat, model, viewport),
        KeyCode::End | KeyCode::Char('G') => follow(chat),
        _ => {}
    }
    None
}

fn scroll_keys(chat: &mut View, model: &Model, key: &KeyEvent, viewport: (u16, u16)) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        KeyCode::Home if ctrl => jump_top(chat, model, viewport),
        KeyCode::End if ctrl => follow(chat),
        _ => return false,
    }
    true
}

fn request_scroll(chat: &mut View, intent: ScrollIntent) {
    chat.scroll_intent = Some(intent);
}

fn follow(chat: &mut View) {
    request_scroll(chat, ScrollIntent::Follow);
}

fn page_up(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Page(-1));
}

fn page_down(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Page(1));
}

fn jump_top(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Oldest);
}

fn line_up(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Rows(-1));
}

fn line_down(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Rows(1));
}
