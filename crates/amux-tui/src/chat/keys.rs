//! Chat key handling: keys mutate ChatView and produce `UiAction`s; all
//! domain writes leave as Commands through the runtime (never bytes —
//! encodings live in amux-ui's C6 module).
//!
//! The binding set is `docs/CHAT.md` §Keybindings' plain tier, derived in
//! `notes/chat-v1/keybindings.md`: readline is law inside the composer
//! (P6), reflex keys stay harmless (P4), interrupt shares a key with
//! nothing (P5). Kitty-tier sugar (Shift+Enter newline) is absent until
//! the chrome feature-detects kitty — Phase 6; hints never advertise it.

use amux_ui::claude::AskState;
use amux_ui::claude::encoding::{self, AskAnswer};
use amux_ui::{Command, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::chat::ask_ui::{self, AskKeyOutcome, AskStage, AskUi};
use crate::chat::composer::Composer;
use crate::chat::reader::{self, ReaderSource, ReaderView};
use crate::chat::{ChatView, FeedScroll, composer, entry_watermark, render};
use crate::view::UiAction;

pub fn handle_chat_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
    now: DateTime<Utc>,
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    // Any keypress dismisses a stated failure (dismissal is view state;
    // the Model keeps the outcome).
    chat.send_failure = None;
    chat.ask_failure = None;
    // Defensive sync: keys may arrive before the first reconcile.
    chat.sync_ask(model);

    // The chrome leader chords work from chat exactly as from raw attach
    // (`docs/CHAT.md` §State transitions): `<leader> s` back to the
    // fleet, `<leader> d` detach to the shell. Leaving is a chrome
    // affair, never an Esc stage — a pending ask survives it (the Model
    // keeps the obligation; reopening re-docks the panel). Chat never
    // shadows the leader, so this composes BEFORE every other binding —
    // Ctrl+C included, matching raw attach where the leader is the one
    // byte passthrough does not forward. An unrecognized chord key is
    // consumed: the leader must never leak a keystroke into panels or
    // the draft.
    if chat.pending_leader {
        chat.pending_leader = false;
        chat.quit_guard.disarm();
        return match key.code {
            KeyCode::Char('s') => Some(UiAction::CloseChat),
            KeyCode::Char('d') => Some(UiAction::Quit),
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

    // Ctrl+C is the chrome-wide guarded abandon key — ONE rule
    // (`docs/CHAT.md` §Keybindings), intercepted before any panel,
    // reader, read-only surface, or the help overlay sees the key,
    // because it must never answer, deny, or interrupt anything (P5): a
    // focused non-empty text field is cleared as a kill (yankable; the
    // clearing press never arms); otherwise — ask menu stages, PENDING,
    // readers, read-only chats, the open overlay, the empty composer —
    // the press arms the quit guard, and a fresh second press quits.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        match focused_field(chat, model).filter(|field| !field.is_empty()) {
            Some(field) => {
                field.kill_all();
                chat.quit_guard.note_clear();
            }
            None => {
                if chat.quit_guard.press(now) {
                    return Some(UiAction::Quit);
                }
            }
        }
        return None;
    }
    // Any other key disarms the quit guard.
    chat.quit_guard.disarm();

    // The `?` overlay: any other key closes it and is consumed (the
    // fleet's Help mode idiom) — the leader and the guard above still
    // compose over it, like everywhere else in the chrome.
    if chat.help {
        chat.help = false;
        return None;
    }

    // Read-only chats have a single viewing focus: scroll keys, `f`, and
    // `q` only (F1) — write affordances are absent, not disabled, so the
    // interrupt and composer branches below simply do not exist here.
    if chat.read_only(model) {
        return readonly_key(chat, model, key, viewport);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // D3: interrupt is the one deliberate binding that works in every
        // focus state — open ask panels and readers included, even while
        // send is gated. Never on Esc, never on Ctrl+C. The reducer
        // dispatches it ungated.
        KeyCode::Char('x') if ctrl => {
            return Some(UiAction::Dispatch(Command::Interrupt { agent: chat.agent }));
        }
        // The settled view-only Esc chain: never answers, never
        // interrupts.
        KeyCode::Esc => {
            esc_chain(chat);
            return None;
        }
        _ => {}
    }

    // The fullscreen reader owns keys while open.
    if chat.reader.is_some() {
        return reader_key(chat, model, key, viewport);
    }

    // A docked ask owns the composer area and its keys (C1).
    if let Some(head) = chat.ask_head(model) {
        if matches!(head.state, AskState::AnsweredOptimistic { .. }) {
            // PENDING: the collapsed marker holds the panel; only the feed
            // scrolls (the answer is in flight — nothing to select).
            scroll_keys(chat, model, &key, viewport);
            return None;
        }
        return panel_key(chat, model, key, viewport);
    }

    composer_key(chat, model, key, viewport)
}

/// The focused text field, if any — the surface the guarded Ctrl+C
/// clears: a read-only chat has none (F1); an interactive ask head
/// (Pending or SendFailed) owns its open text stage, and its menu stages
/// have no field; the optimistic-pending marker and the accepted-plans
/// reader have none; otherwise the composer is the focused field. This
/// mirrors the focus derivation keys and paste routing use — the
/// invisible composer behind a docked panel is never "focused".
fn focused_field<'c>(chat: &'c mut ChatView, model: &Model) -> Option<&'c mut Composer> {
    if chat.read_only(model) || chat.help {
        return None;
    }
    let ask_state = chat.ask_head(model).map(|ask| match ask.state {
        AskState::Pending | AskState::SendFailed { .. } => true,
        AskState::AnsweredOptimistic { .. } => false,
    });
    match ask_state {
        Some(true) => chat.ask_ui.as_mut().and_then(AskUi::active_field),
        Some(false) => None,
        None if chat.reader.is_some() => None,
        None => Some(&mut chat.composer),
    }
}

/// The composer focus: the chat-level bindings (Phase 4's key set, plus
/// Ctrl+T for the plan reader) layered over the shared readline set
/// ([`composer::readline_key`] — the same machinery the panel text
/// fields dispatch through).
fn composer_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Ctrl+C never reaches here — the chrome-wide guard intercepts it
        // in `handle_chat_key` (clear-as-kill on a non-empty draft;
        // arm-then-quit otherwise).
        // Ctrl+T: the reader on the newest accepted plan (B6); ←/→ steps
        // between plans once open. Only bound while a plan exists — the
        // feed's `ctrl+t to read` affordance is the hint.
        KeyCode::Char('t') if ctrl => {
            let plans = model
                .claude(chat.agent)
                .map(|layer| layer.accepted_plans().len())
                .unwrap_or(0);
            if plans > 0 {
                chat.reader = Some(ReaderView {
                    source: ReaderSource::Plans { index: plans - 1 },
                    scroll: 0,
                });
            }
        }
        // Shift+Enter: kitty-tier newline sugar. Dispatch trusts the
        // delivered event — a plain terminal cannot produce it (Enter and
        // Shift+Enter are byte-identical without the kitty protocol);
        // the tier gate lives in hints and the `?` overlay.
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            chat.composer.insert_newline()
        }
        KeyCode::Enter => return send(chat, model),
        // Ctrl+J: the guaranteed newline in any terminal (Shift+Enter
        // above is the kitty sugar). Ctrl+P/N and the arrows are
        // multiline row motion — above the one-line readline set.
        KeyCode::Char('j') if ctrl => chat.composer.insert_newline(),
        KeyCode::Char('p') if ctrl => chat.composer.up(),
        KeyCode::Char('n') if ctrl => chat.composer.down(),
        KeyCode::Up => chat.composer.up(),
        KeyCode::Down => chat.composer.down(),
        // D4: Shift+Tab cycles the permission mode; the current mode
        // renders in the footer from hook facts. Gated exactly where the
        // injected CSI Z would not reach claude's composer.
        KeyCode::BackTab => {
            if model.claude_mode_cycle_gate(chat.agent).is_none() {
                return Some(UiAction::Dispatch(Command::CyclePermissionMode {
                    agent: chat.agent,
                }));
            }
        }
        // Tab is reserved for the future queueing door (D2) — a no-op
        // until that lands deliberately.
        KeyCode::Tab => {}
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        // Ctrl+Home / Ctrl+End: feed oldest / newest (ext tier —
        // convenience, never the sole path; PgUp/PgDn are guaranteed).
        KeyCode::Home if ctrl => jump_top(chat, model, viewport),
        KeyCode::End if ctrl => chat.scroll = FeedScroll::Following,
        // The `?` help overlay — on an EMPTY draft only; with anything
        // typed, `?` is a printable and types (P2). The footer hint
        // advertises it exactly when this branch is live.
        KeyCode::Char('?') if !ctrl && chat.composer.is_empty() => {
            chat.help = true;
        }
        // The shared readline set (P6): motion, kills, yank, printables —
        // `?` included on a non-empty draft. What it leaves stays
        // unbound, each an act of restraint: Ctrl+A (chrome leader),
        // Ctrl+G (emacs abort reflex — must never fire agent actions),
        // Ctrl+R (reserved: history search), Ctrl+L (shell redraw
        // reflex), Ctrl+V (bracketed paste owns pasting), and the
        // byte-aliases Ctrl+H/I/M.
        _ => {
            composer::readline_key(&mut chat.composer, &key);
        }
    }
    None
}

/// Bracketed paste into the chat: literal insertion into the focused text
/// surface — tabs and newlines land as text, never as bindings (a pasted
/// CR must never submit a partial prompt). Routing follows the same
/// model/focus derivation as keys: a read-only chat has NO composer
/// (F1 — the paste is dropped, never retained invisibly); an open panel
/// text stage takes it (newlines flattened to spaces — panel fields are
/// one-line); a docked panel without a field, or an open reader, drops
/// it rather than typing into the invisible composer; only the composer
/// focus inserts (see [`crate::chat::composer::Composer::paste`]).
pub fn handle_chat_paste(chat: &mut ChatView, model: &Model, text: &str) {
    chat.send_failure = None;
    chat.ask_failure = None;
    // Paste is input like any other key: it disarms the quit guard.
    chat.quit_guard.disarm();
    // The same defensive sync keys run: a pending ask that has not been
    // reconciled yet still owns the surface — the paste must not slip
    // into the composer through that window.
    chat.sync_ask(model);
    if chat.read_only(model) {
        return;
    }
    if let Some(ui) = chat.ask_ui.as_mut() {
        if let Some(field) = ui.active_field() {
            let one_line = text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', " ");
            field.paste(&one_line);
        }
        return;
    }
    if chat.reader.is_some() {
        return;
    }
    chat.composer.paste(text);
}

/// Enter: send, gated on phase by the same derivation the footer states
/// (D2) — while gated, Enter is a no-op and the draft is kept.
fn send(chat: &mut ChatView, model: &Model) -> Option<UiAction> {
    if chat.composer.is_empty() {
        return None;
    }
    if model.claude_send_gate(chat.agent).refusal().is_some() {
        return None;
    }
    let text = chat.composer.text();
    chat.composer.clear_for_send();
    Some(UiAction::Dispatch(Command::SendPrompt {
        agent: chat.agent,
        text,
    }))
}

/// The deterministic view-only Esc chain (`docs/CHAT.md` §State
/// transitions), checked in order — first hit wins. Esc never answers an
/// ask and never interrupts.
fn esc_chain(chat: &mut ChatView) {
    // Stage 1: close the reader — an open text field inside it closes
    // first (the request-changes stage steps back to the action row with
    // its text kept), then the reader itself; a plan-review reader drops
    // to its docked panel form.
    if chat.reader.is_some() {
        if chat.ask_reader_open()
            && let Some(ui) = chat.ask_ui.as_mut()
            && ui.step_back()
        {
            return;
        }
        chat.reader = None;
        return;
    }
    // Stage 2: step back ask stages, flooring at the menu stage — the
    // panel is never dismissed while its ask pends.
    if let Some(ui) = chat.ask_ui.as_mut()
        && ui.step_back()
    {
        return;
    }
    // Stage 3: reset feed scroll to following — empty draft only.
    if matches!(chat.scroll, FeedScroll::Paused { .. }) && chat.composer.is_empty() {
        chat.scroll = FeedScroll::Following;
    }
    // Stage 4: nothing.
}

// --- panel and reader routing ------------------------------------------------

/// Keys while an ask panel is docked and interactive (Pending or
/// SendFailed): the stage machine first, the feed's scroll keys as the
/// fallback — the feed stays scrollable behind the docked panel.
fn panel_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let head = chat.ask_head(model)?;
    // Unverified menu shapes render read-only-style (C2): no actions to
    // route — only the read affordance and the feed scroll live.
    if encoding::menu_shape_refusal(&head.kind).is_some() {
        if key.code == KeyCode::Char('f') && ask_ui::has_readable(head) {
            chat.reader = Some(ReaderView::ask());
            return None;
        }
        scroll_keys(chat, model, &key, viewport);
        return None;
    }
    let ask_id = head.id;
    let outcome = chat
        .ask_ui
        .as_mut()
        .map(|ui| ui.handle_key(head, &key, true))
        .unwrap_or(AskKeyOutcome::NotHandled);
    match outcome {
        AskKeyOutcome::Answer(answer) => Some(dispatch_answer(chat, ask_id, answer)),
        AskKeyOutcome::OpenReader => {
            chat.reader = Some(ReaderView::ask());
            None
        }
        AskKeyOutcome::Handled => None,
        AskKeyOutcome::NotHandled => {
            scroll_keys(chat, model, &key, viewport);
            None
        }
    }
}

fn dispatch_answer(chat: &ChatView, ask: u64, answer: AskAnswer) -> UiAction {
    UiAction::Dispatch(Command::AnswerAsk {
        agent: chat.agent,
        ask,
        answer,
    })
}

/// Keys while the fullscreen reader is open: the writable ask's action
/// row / feedback stage first, then pager motion (P7 — no text field
/// means bare letters are safe; in the request-changes stage they type).
fn reader_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    if chat.ask_reader_open()
        && let Some(head) = chat.ask_head(model)
        && matches!(head.state, AskState::Pending)
        && encoding::menu_shape_refusal(&head.kind).is_none()
    {
        let ask_id = head.id;
        let outcome = chat
            .ask_ui
            .as_mut()
            .map(|ui| ui.handle_key(head, &key, false))
            .unwrap_or(AskKeyOutcome::NotHandled);
        match outcome {
            AskKeyOutcome::Answer(answer) => return Some(dispatch_answer(chat, ask_id, answer)),
            AskKeyOutcome::Handled | AskKeyOutcome::OpenReader => {
                // Enter on Deny opens the one-line feedback stage, which
                // is docked (C2): the reader closes to the panel.
                if chat
                    .ask_ui
                    .as_ref()
                    .is_some_and(|ui| ui.stage == AskStage::DenyFeedback)
                {
                    chat.reader = None;
                }
                return None;
            }
            AskKeyOutcome::NotHandled => {}
        }
    }
    match key.code {
        // q leaves a read surface (P7); in the writable review reader it
        // is the Esc-stage alias — the plan stays, the docked panel
        // remains (text stages consumed q above as a printable).
        KeyCode::Char('q') => {
            chat.reader = None;
            None
        }
        // ←/→ step between accepted plans (resolved reader only).
        KeyCode::Left => {
            if let Some(index) = reader::plans_step(model, chat, -1)
                && let Some(view) = chat.reader.as_mut()
            {
                view.source = ReaderSource::Plans { index };
                view.scroll = 0;
            }
            None
        }
        KeyCode::Right => {
            if let Some(index) = reader::plans_step(model, chat, 1)
                && let Some(view) = chat.reader.as_mut()
            {
                view.source = ReaderSource::Plans { index };
                view.scroll = 0;
            }
            None
        }
        _ => {
            reader_scroll(chat, model, &key, viewport);
            None
        }
    }
}

/// Pager motion over the reader body: ↑↓ j/k, PgUp/PgDn, Home/End g/G.
fn reader_scroll(chat: &mut ChatView, model: &Model, key: &KeyEvent, viewport: (u16, u16)) -> bool {
    let Some((page, max_top)) = reader::scroll_metrics(model, chat, viewport) else {
        return false;
    };
    let Some(view) = chat.reader.as_mut() else {
        return false;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => view.scroll = (view.scroll + 1).min(max_top),
        KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(page),
        KeyCode::PageDown => view.scroll = (view.scroll + page).min(max_top),
        KeyCode::Home | KeyCode::Char('g') => view.scroll = 0,
        KeyCode::End | KeyCode::Char('G') => view.scroll = max_top,
        _ => return false,
    }
    true
}

/// The feed's scroll keys, shared by every docked focus state.
fn scroll_keys(chat: &mut ChatView, model: &Model, key: &KeyEvent, viewport: (u16, u16)) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        KeyCode::Home if ctrl => jump_top(chat, model, viewport),
        KeyCode::End if ctrl => chat.scroll = FeedScroll::Following,
        _ => return false,
    }
    true
}

/// The read-only chat (F1): a pager over the live feed. Bare letters are
/// safe — no text field exists here — and every write affordance is
/// absent, not disabled: no interrupt, no answers, no composer.
fn readonly_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    if chat.reader.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => chat.reader = None,
            _ => {
                reader_scroll(chat, model, &key, viewport);
            }
        }
        return None;
    }
    match key.code {
        // q leaves the read surface — back to the fleet.
        KeyCode::Char('q') => return Some(UiAction::CloseChat),
        // f opens the pending ask's diff or plan in the reader — the fact
        // panel's one read affordance.
        KeyCode::Char('f') => {
            if chat.ask_head(model).is_some_and(ask_ui::has_readable) {
                chat.reader = Some(ReaderView::ask());
            }
        }
        KeyCode::Esc => {
            if matches!(chat.scroll, FeedScroll::Paused { .. }) {
                chat.scroll = FeedScroll::Following;
            }
        }
        KeyCode::PageUp => page_up(chat, model, viewport),
        KeyCode::PageDown => page_down(chat, model, viewport),
        KeyCode::Up | KeyCode::Char('k') => line_up(chat, model, viewport),
        KeyCode::Down | KeyCode::Char('j') => line_down(chat, model, viewport),
        KeyCode::Home | KeyCode::Char('g') => jump_top(chat, model, viewport),
        KeyCode::End | KeyCode::Char('G') => chat.scroll = FeedScroll::Following,
        _ => {}
    }
    None
}

/// Scroll bounds under the paused layout (the paused rule takes a row, so
/// paging targets that geometry).
fn scroll_metrics(chat: &ChatView, model: &Model, viewport: (u16, u16)) -> (usize, usize) {
    let layout = render::layout(model, chat, viewport);
    let feed_h = layout.feed_height_when_paused().max(1);
    let page = feed_h.saturating_sub(1).max(1);
    (page, feed_h)
}

fn pause_at(chat: &mut ChatView, model: &Model, top_line: usize) {
    let entry_watermark = match chat.scroll {
        // Re-anchoring while already paused keeps the original watermark:
        // "new entries" counts from when following stopped.
        FeedScroll::Paused {
            entry_watermark, ..
        } => entry_watermark,
        FeedScroll::Following => entry_watermark(model, chat.agent),
    };
    chat.scroll = FeedScroll::Paused {
        top_line,
        entry_watermark,
    };
}

fn page_up(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let (page, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    match chat.scroll {
        FeedScroll::Following => {
            let max_top = total.saturating_sub(feed_h);
            if max_top == 0 {
                // Everything already fits: nothing to scroll back to.
                return;
            }
            pause_at(chat, model, max_top.saturating_sub(page));
        }
        FeedScroll::Paused { top_line, .. } => {
            pause_at(chat, model, top_line.saturating_sub(page));
        }
    }
}

fn page_down(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let FeedScroll::Paused { top_line, .. } = chat.scroll else {
        return;
    };
    let (page, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    let max_top = total.saturating_sub(feed_h);
    let next = top_line + page;
    if next >= max_top {
        // Reaching the bottom resumes following — sticky-bottom re-pins,
        // no dedicated resume key (keybindings §2.4).
        chat.scroll = FeedScroll::Following;
    } else {
        pause_at(chat, model, next);
    }
}

/// Jump to the feed's oldest retained line (Ctrl+Home; `g`/Home in the
/// read-only pager).
fn jump_top(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let (_, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    if total > feed_h {
        pause_at(chat, model, 0);
    }
}

/// One-line pager motion (read-only chats: ↑/k, ↓/j).
fn line_up(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let (_, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    let max_top = total.saturating_sub(feed_h);
    match chat.scroll {
        FeedScroll::Following => {
            if max_top > 0 {
                pause_at(chat, model, max_top - 1);
            }
        }
        FeedScroll::Paused { top_line, .. } => pause_at(chat, model, top_line.saturating_sub(1)),
    }
}

fn line_down(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let FeedScroll::Paused { top_line, .. } = chat.scroll else {
        return;
    };
    let (_, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    let max_top = total.saturating_sub(feed_h);
    if top_line + 1 >= max_top {
        chat.scroll = FeedScroll::Following;
    } else {
        pause_at(chat, model, top_line + 1);
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::{Model, Msg, OpId, ServerMsg, StreamEntry, StreamMsg, update};
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn agent_id() -> amux_ui::AgentId {
        Uuid::from_u128(7)
    }

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("epoch")
    }

    fn fold(model: &mut Model, msgs: Vec<Msg>) {
        for msg in msgs {
            update(model, msg);
        }
    }

    fn base_msgs() -> Vec<Msg> {
        let agent = amux_ui::Agent {
            id: agent_id(),
            host_id: Uuid::from_u128(1),
            name: Some("fix-auth".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                "claude_raw_v1".to_string(),
                "claude_pty_transcript_v1".to_string(),
            ],
            readonly: false,
            args: Vec::new(),
            created_at: t(0),
        };
        let host = amux_ui::HostEntry {
            id: Uuid::from_u128(1),
            name: "mbp".to_string(),
            online: true,
            version: None,
            capabilities: None,
            trust_status: amux_ui::HostTrustStatus::Trusted,
            last_dial_error: None,
        };
        vec![
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(1)),
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::ReplayComplete,
            },
        ]
    }

    fn rows(at: i64, first_seq: u64, payloads: Vec<serde_json::Value>) -> Msg {
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::Batch {
                at: t(at),
                entries: payloads
                    .into_iter()
                    .enumerate()
                    .map(|(offset, payload)| StreamEntry {
                        seq: first_seq + offset as u64,
                        payload,
                    })
                    .collect(),
            },
        }
    }

    fn ready_row() -> serde_json::Value {
        json!({"type": "amux.transcript_ready"})
    }

    fn prompt_row(n: u8) -> serde_json::Value {
        json!({
            "type": "user",
            "uuid": format!("dddddddd-0000-4000-8000-0000000000{n:02}"),
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-12T09:00:00.000Z",
            "message": {"role": "user", "content": format!("prompt {n}")},
            "origin": {"kind": "human"},
            "promptSource": "typed",
        })
    }

    fn idle_model() -> Model {
        let mut model = Model::default();
        fold(&mut model, base_msgs());
        fold(&mut model, vec![rows(1, 1, vec![ready_row()])]);
        model
    }

    fn working_model() -> Model {
        let mut model = idle_model();
        fold(&mut model, vec![rows(2, 2, vec![prompt_row(1)])]);
        model
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn chat_with_draft(text: &str) -> ChatView {
        let mut chat = ChatView::open(agent_id(), 'a', false);
        chat.composer.insert_str(text);
        chat
    }

    const VIEWPORT: (u16, u16) = (80, 20);

    #[test]
    fn enter_sends_when_ready_and_clears_the_draft() {
        let model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::SendPrompt {
                agent: agent_id(),
                text: "add retry".to_string(),
            }))
        );
        assert!(chat.composer.is_empty(), "the draft moved into the send");
    }

    #[test]
    fn enter_is_a_noop_while_gated_and_keeps_the_draft() {
        let model = working_model();
        let mut chat = chat_with_draft("and document it");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(action, None, "send is gated while working (D2)");
        assert_eq!(chat.composer.text(), "and document it", "draft kept");
    }

    #[test]
    fn enter_on_an_empty_draft_is_a_noop() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None
        );
    }

    #[test]
    fn ctrl_x_interrupts_in_every_state_and_never_touches_the_draft() {
        for model in [idle_model(), working_model()] {
            let mut chat = chat_with_draft("precious draft");
            let action = handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0));
            assert_eq!(
                action,
                Some(UiAction::Dispatch(Command::Interrupt { agent: agent_id() }))
            );
            assert_eq!(chat.composer.text(), "precious draft");
        }
    }

    #[test]
    fn ctrl_c_clears_a_nonempty_draft_as_a_yankable_kill_and_never_arms() {
        let model = idle_model();
        let mut chat = chat_with_draft("half a thought");
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None
        );
        assert!(chat.composer.is_empty());
        assert!(
            !chat.quit_guard.is_armed(),
            "the clearing press never arms (2.1)"
        );
        chat.composer.yank();
        assert_eq!(chat.composer.text(), "half a thought", "clear is a kill");
    }

    /// The chrome-wide guard in the chat: empty draft → arm (rendered),
    /// fresh second press → quit; any other key disarms; a stale arm
    /// re-arms. Quit-from-a-full-composer is three deliberate presses.
    #[test]
    fn ctrl_c_on_an_empty_draft_arms_then_a_second_press_quits() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None,
            "a single Ctrl+C never quits"
        );
        assert!(chat.quit_guard.is_armed());
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(2)),
            Some(UiAction::Quit)
        );
    }

    #[test]
    fn any_other_key_disarms_and_a_stale_arm_rearms() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT, t(1));
        assert!(!chat.quit_guard.is_armed(), "another key disarms");
        handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(2));
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(10)),
            None,
            "a stale arm re-arms instead of quitting"
        );
        assert!(chat.quit_guard.is_armed());
    }

    #[test]
    fn shift_tab_cycles_the_mode_only_when_the_injection_would_reach_claude() {
        let mut chat = ChatView::open(agent_id(), 'a', false);
        let action = handle_chat_key(
            &mut chat,
            &idle_model(),
            press(KeyCode::BackTab),
            VIEWPORT,
            t(0),
        );
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::CyclePermissionMode {
                agent: agent_id()
            }))
        );

        // A pending ask owns the keystroke channel: CSI Z would navigate
        // the form — refused.
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![json!({
                    "type": "hook.permission_request",
                    "tool_name": "Bash",
                    "tool_input": {"command": "echo hi"},
                })],
            )],
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::BackTab), VIEWPORT, t(0)),
            None
        );
    }

    #[test]
    fn tab_is_reserved_and_question_mark_opens_help_or_types() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Tab), VIEWPORT, t(0)),
            None
        );
        assert!(
            chat.composer.is_empty(),
            "tab stays a no-op (queueing door)"
        );
        // Empty draft: `?` opens the overlay; the next key closes it and
        // is consumed — nothing types through the overlay.
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT, t(0));
        assert!(chat.help, "`?` on an empty draft opens the help overlay");
        assert!(chat.composer.is_empty());
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('x')), VIEWPORT, t(0));
        assert!(!chat.help, "any key closes");
        assert!(chat.composer.is_empty(), "the closing key is consumed");
        // Non-empty draft: `?` is a printable and types (P2).
        chat.composer.insert_str("what");
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT, t(0));
        assert!(!chat.help);
        assert_eq!(chat.composer.text(), "what?", "`?` types into a draft");
    }

    /// The overlay composes with the chrome: the leader chords still
    /// work over it, and Ctrl+C runs the quit guard (no field is focused
    /// while the overlay covers the screen).
    #[test]
    fn the_help_overlay_yields_to_leader_and_quit_guard() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('s')), VIEWPORT, t(0)),
            Some(UiAction::CloseChat),
            "leader chords work over the overlay"
        );
        let mut chat = ChatView::open(agent_id(), 'a', false);
        chat.composer.insert_str("draft");
        handle_chat_key(&mut chat, &model, ctrl('u'), VIEWPORT, t(0)); // empty the draft
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT, t(0));
        assert!(chat.help);
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None
        );
        assert!(chat.quit_guard.is_armed(), "^C over the overlay arms");
        assert!(chat.help, "the overlay stays while the guard arms");
    }

    /// A feed tall enough to scroll at the test viewport.
    fn long_feed_model() -> Model {
        let mut model = idle_model();
        for n in 0..20u8 {
            fold(
                &mut model,
                vec![rows(2 + n as i64, 2 + n as u64, vec![prompt_row(n)])],
            );
        }
        model
    }

    #[test]
    fn pgup_pauses_with_a_watermark_and_pgdn_at_the_bottom_resumes() {
        let model = long_feed_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT, t(0));
        let FeedScroll::Paused {
            entry_watermark, ..
        } = chat.scroll
        else {
            panic!("PgUp pauses following");
        };
        assert_eq!(entry_watermark, 20, "watermark is the entry count at pause");

        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT, t(0));
        assert!(
            matches!(chat.scroll, FeedScroll::Paused { .. }),
            "mid-feed PgDn stays paused"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT, t(0));
        assert_eq!(
            chat.scroll,
            FeedScroll::Following,
            "reaching the bottom resumes following"
        );
    }

    #[test]
    fn pgup_with_a_short_feed_stays_following() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT, t(0));
        assert_eq!(chat.scroll, FeedScroll::Following);
    }

    #[test]
    fn esc_resets_scroll_only_on_an_empty_draft() {
        let model = long_feed_model();
        let mut chat = chat_with_draft("reading notes");
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT, t(0));
        assert!(
            matches!(chat.scroll, FeedScroll::Paused { .. }),
            "a non-empty draft keeps Esc away from the scroll (stage 3 gate)"
        );
        chat.composer.kill_all();
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT, t(0));
        assert_eq!(chat.scroll, FeedScroll::Following);
    }

    #[test]
    fn readline_chords_edit_the_draft() {
        let model = idle_model();
        let mut chat = chat_with_draft("fix the tests");
        handle_chat_key(&mut chat, &model, ctrl('w'), VIEWPORT, t(0));
        assert_eq!(chat.composer.text(), "fix the ");
        handle_chat_key(&mut chat, &model, ctrl('u'), VIEWPORT, t(0));
        assert!(chat.composer.is_empty());
        handle_chat_key(&mut chat, &model, ctrl('y'), VIEWPORT, t(0));
        assert_eq!(chat.composer.text(), "fix the ");
        handle_chat_key(&mut chat, &model, ctrl('b'), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, ctrl('d'), VIEWPORT, t(0));
        assert_eq!(chat.composer.text(), "fix the");
    }

    #[test]
    fn ctrl_j_inserts_a_newline_instead_of_sending() {
        let model = idle_model();
        let mut chat = chat_with_draft("first");
        let action = handle_chat_key(&mut chat, &model, ctrl('j'), VIEWPORT, t(0));
        assert_eq!(action, None);
        assert_eq!(chat.composer.text(), "first\n");
    }

    /// Shift+Enter is the kitty-tier newline sugar: when the terminal
    /// delivers it (only the kitty protocol can), it is a newline, never
    /// a send.
    #[test]
    fn shift_enter_inserts_a_newline_instead_of_sending() {
        let model = idle_model();
        let mut chat = chat_with_draft("first");
        let action = handle_chat_key(
            &mut chat,
            &model,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            VIEWPORT,
            t(0),
        );
        assert_eq!(action, None);
        assert_eq!(chat.composer.text(), "first\n");
    }

    #[test]
    fn a_failed_send_restores_the_draft_and_states_the_failure() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        let Some(UiAction::Dispatch(command)) = action else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::Error {
                    error: amux_ui::OpError {
                        message: "input raced the session".to_string(),
                        auth_required: false,
                    },
                },
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.composer.text(), "add retry", "draft resurfaced (C5)");
        assert_eq!(chat.send_failure(), Some("input raced the session"));

        // The next keypress dismisses the stated failure.
        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT, t(0));
        assert_eq!(chat.send_failure(), None);
    }

    #[test]
    fn a_failed_send_never_clobbers_text_typed_in_the_meantime() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        chat.composer.insert_str("newer thought");
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::Error {
                    error: amux_ui::OpError {
                        message: "transport lost".to_string(),
                        auth_required: false,
                    },
                },
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.composer.text(), "newer thought");
        assert_eq!(chat.send_failure(), Some("transport lost"));
    }

    /// The run loop reconciles immediately after dispatch: a command the
    /// reducer refuses SYNCHRONOUSLY (here: a whitespace-only prompt —
    /// the encoder's EmptyText refusal; the disconnected fail-fast is the
    /// same shape) must resurface the draft and state the failure without
    /// any further runtime message.
    #[test]
    fn a_synchronous_refusal_resurfaces_the_draft_without_further_messages() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("   ");
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("enter dispatches (the draft is non-empty, the gate is Ready)");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        // `Runtime::dispatch` folds the Command synchronously; the refusal
        // is finished state before dispatch even returns.
        fold(&mut model, vec![Msg::Command { op, command }]);
        chat.reconcile(&model); // what run.rs does right after dispatch
        assert_eq!(chat.composer.text(), "   ", "draft resurfaced");
        assert_eq!(chat.send_failure(), Some("prompt must not be empty"));
    }

    #[test]
    fn paste_inserts_literally_and_dismisses_a_stated_failure() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        chat.send_failure = Some("older failure".to_string());
        handle_chat_paste(&mut chat, &model, "one\n\ttwo");
        assert_eq!(chat.composer.text(), "one\n    two");
        assert_eq!(chat.send_failure(), None, "paste is input; it dismisses");
    }

    /// A read-only chat has NO composer (F1 — absent, not disabled): a
    /// paste must not be retained invisibly, exposable later if the
    /// agent ever became writable.
    #[test]
    fn paste_in_a_readonly_chat_retains_nothing() {
        let model = readonly_ask_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_paste(&mut chat, &model, "secret scratch text");
        assert!(chat.composer.is_empty(), "no composer surface exists");
        assert!(
            chat.ask_ui
                .as_ref()
                .is_none_or(|ui| ui.deny_feedback.is_empty()),
            "no panel field took it either"
        );
    }

    /// A pending ask owns the surface even before the first reconcile:
    /// the paste routes through the same sync the keys use and is
    /// dropped at the menu stage, never slipped into the hidden
    /// composer.
    #[test]
    fn paste_with_a_pending_ask_before_reconcile_retains_nothing() {
        let model = edit_ask_model();
        let mut chat = ChatView::open(agent_id(), 'a', false); // deliberately not reconciled
        handle_chat_paste(&mut chat, &model, "stray paste");
        assert!(chat.composer.is_empty(), "menu stage has no text field");
        let ui = chat.ask_ui.as_ref().expect("the paste synced the panel");
        assert!(ui.deny_feedback.is_empty());
    }

    // --- ask panels, reader, read-only (Phase 5) ----------------------------

    use amux_ui::claude::encoding::{PermissionAnswer, PlanAnswer, QuestionResponse};

    fn hook_row(tool: &str, input: serde_json::Value, suggestions: usize) -> serde_json::Value {
        let mut row = json!({
            "type": "hook.permission_request",
            "tool_name": tool,
            "tool_input": input,
            "permission_mode": "default",
        });
        if suggestions > 0 {
            let entries: Vec<serde_json::Value> = (0..suggestions)
                .map(|_| {
                    json!({"type": "addDirectories", "destination": "session",
                                "directories": ["/work"]})
                })
                .collect();
            row["permission_suggestions"] = serde_json::Value::Array(entries);
        }
        row
    }

    fn edit_ask_model() -> Model {
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![hook_row(
                    "Edit",
                    json!({"file_path": "sync/config.rs", "old_string": "a\n", "new_string": "b\n"}),
                    1,
                )],
            )],
        );
        model
    }

    fn plan_ask_model() -> Model {
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![hook_row(
                    "ExitPlanMode",
                    json!({"plan": "# plan\n\n- step"}),
                    0,
                )],
            )],
        );
        model
    }

    fn question_model(multi: bool) -> Model {
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![hook_row(
                    "AskUserQuestion",
                    json!({"questions": [
                        {"header": "Color", "question": "Which?", "multiSelect": multi,
                         "options": [{"label": "Red"}, {"label": "Blue"}]}
                    ]}),
                    1,
                )],
            )],
        );
        model
    }

    fn open_chat(model: &Model) -> ChatView {
        let mut chat = ChatView::open(agent_id(), 'a', false);
        chat.reconcile(model);
        chat
    }

    fn answer_of(action: Option<UiAction>) -> amux_ui::claude::encoding::AskAnswer {
        match action {
            Some(UiAction::Dispatch(Command::AnswerAsk { agent, ask, answer })) => {
                assert_eq!(agent, agent_id());
                assert_eq!(ask, 0, "the head ask");
                answer
            }
            other => panic!("expected an AnswerAsk dispatch, got {other:?}"),
        }
    }

    #[test]
    fn digits_select_and_enter_dispatches_the_answer() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('2')), VIEWPORT, t(0)),
            None,
            "digits select, never submit (P8)"
        );
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Permission(PermissionAnswer::AllowScoped)
        );
    }

    #[test]
    fn esc_floors_at_the_menu_and_never_dismisses_the_panel() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        for _ in 0..3 {
            assert_eq!(
                handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT, t(0)),
                None
            );
        }
        assert!(
            chat.ask_ui.is_some(),
            "the panel is not dismissible while its ask pends"
        );
    }

    #[test]
    fn deny_stage_preserves_text_and_enter_carries_the_feedback() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        for c in "why".chars() {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(c)), VIEWPORT, t(0));
        }
        // Esc steps back to the menu; the typed text survives (P8).
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT, t(0));
        assert!(matches!(
            chat.ask_ui.as_ref().expect("panel").stage,
            crate::chat::ask_ui::AskStage::Menu { cursor: 2 }
        ));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some("why".to_string())
            })
        );
    }

    #[test]
    fn deny_with_empty_text_is_a_plain_deny() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: None
            })
        );
    }

    #[test]
    fn ctrl_x_interrupts_from_the_panel_and_its_text_stage() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0)),
            Some(UiAction::Dispatch(Command::Interrupt { agent: agent_id() }))
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0)),
            Some(UiAction::Dispatch(Command::Interrupt { agent: agent_id() })),
            "interrupt works in every focus state (D3)"
        );
    }

    #[test]
    fn ctrl_c_clears_the_panel_field_never_the_draft() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        chat.composer.insert_str("precious draft");
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        for c in "oops".chars() {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(c)), VIEWPORT, t(0));
        }
        handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0));
        assert!(
            chat.ask_ui
                .as_ref()
                .expect("panel")
                .deny_feedback
                .is_empty(),
            "^C cleared the focused field"
        );
        assert!(!chat.quit_guard.is_armed(), "the clearing press never arms");
        assert_eq!(
            chat.composer.text(),
            "precious draft",
            "the draft survives (D1)"
        );
    }

    /// In chat with no text field focused (ask menu stage) the buffer in
    /// scope is empty by definition: ^C arms the quit guard — and still
    /// never answers, denies, or interrupts anything (P5). The invisible
    /// composer behind the panel is never cleared.
    #[test]
    fn ctrl_c_in_the_ask_menu_arms_and_never_answers() {
        let model = edit_ask_model();
        let mut chat = open_chat(&model);
        chat.composer.insert_str("precious draft");
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None
        );
        assert!(chat.quit_guard.is_armed());
        assert!(chat.ask_ui.is_some(), "the panel stays; nothing answered");
        assert_eq!(
            chat.composer.text(),
            "precious draft",
            "the invisible draft is not the focused field"
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(1)),
            Some(UiAction::Quit),
            "the guard is the same chrome-wide rule here"
        );
    }

    // --- leader chords (chrome navigation from chat) -----------------------

    /// `<leader> s` returns to the fleet and `<leader> d` detaches to
    /// the shell, from every focus — composer, docked ask, read-only —
    /// exactly as from raw attach. A pending ask survives leaving.
    #[test]
    fn leader_chords_navigate_from_every_focus() {
        for model in [idle_model(), edit_ask_model(), readonly_ask_model()] {
            let mut chat = open_chat(&model);
            assert_eq!(
                handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0)),
                None,
                "the leader press itself is pending, not an action"
            );
            assert_eq!(
                handle_chat_key(&mut chat, &model, press(KeyCode::Char('s')), VIEWPORT, t(0)),
                Some(UiAction::CloseChat)
            );
            let mut chat = open_chat(&model);
            handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0));
            assert_eq!(
                handle_chat_key(&mut chat, &model, press(KeyCode::Char('d')), VIEWPORT, t(0)),
                Some(UiAction::Quit),
                "detach means the shell"
            );
        }
    }

    /// An unrecognized chord key is consumed — the leader never leaks a
    /// keystroke into the draft, and `<leader> x` must not interrupt.
    #[test]
    fn an_unrecognized_leader_chord_consumes_the_key() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0)),
            None,
            "no interrupt fires through a broken chord"
        );
        handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('z')), VIEWPORT, t(0));
        assert!(chat.composer.is_empty(), "the chord key never types");
        // The chord state is one-shot: the next `s` is a plain printable.
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('s')), VIEWPORT, t(0));
        assert_eq!(chat.composer.text(), "s");
    }

    /// The chords compose with the CONFIGURED leader (ctrl+b here):
    /// ctrl+a is then unbound chrome-side and consumed by readline as a
    /// no-op (C-a stays the leader's sacrifice, never line-start).
    #[test]
    fn a_configured_leader_moves_the_chord() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id(), 'b', false);
        handle_chat_key(&mut chat, &model, ctrl('b'), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('s')), VIEWPORT, t(0)),
            Some(UiAction::CloseChat)
        );
    }

    /// Read-only chats have no text field anywhere: ^C is always the
    /// arm/quit branch (F1 keeps every write affordance absent).
    #[test]
    fn ctrl_c_in_a_readonly_chat_arms_then_quits() {
        let model = readonly_ask_model();
        let mut chat = open_chat(&model);
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None
        );
        assert!(chat.quit_guard.is_armed());
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(1)),
            Some(UiAction::Quit)
        );
    }

    #[test]
    fn a_single_select_question_submits_on_the_confirmed_selection() {
        let model = question_model(false);
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('2')), VIEWPORT, t(0));
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![1],
                    other: None
                }]
            }
        );
    }

    #[test]
    fn the_other_field_types_and_submits_its_text() {
        let model = question_model(false);
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        for c in "ochre".chars() {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(c)), VIEWPORT, t(0));
        }
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![],
                    other: Some("ochre".to_string())
                }]
            }
        );
    }

    #[test]
    fn multi_select_space_toggles_and_the_review_gates_submission() {
        let model = question_model(true);
        let mut chat = open_chat(&model);
        // Tab straight to the submit tab with nothing answered: Enter is a
        // no-op — the review states the unanswered item instead.
        handle_chat_key(&mut chat, &model, press(KeyCode::Tab), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None,
            "unanswered forms do not submit"
        );
        // Back to the question, toggle two options, advance, submit.
        handle_chat_key(&mut chat, &model, press(KeyCode::Tab), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Char(' ')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Down), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Char(' ')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![0, 1],
                    other: None
                }]
            }
        );
    }

    #[test]
    fn plan_review_opens_the_reader_first_and_esc_docks_it() {
        let model = plan_ask_model();
        let mut chat = open_chat(&model);
        assert!(chat.reader.is_some(), "plan review opens the reader (C3)");
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT, t(0));
        assert!(chat.reader.is_none(), "Esc drops to the docked panel");
        assert!(chat.ask_ui.is_some(), "the docked form remains");
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('f')), VIEWPORT, t(0));
        assert!(chat.reader.is_some(), "`f` returns to the full reader");
    }

    #[test]
    fn request_changes_requires_feedback_and_q_types_there() {
        let model = plan_ask_model();
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None,
            "request-changes will not submit empty (C3)"
        );
        // `q` is a printable while the feedback field is focused (P2).
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('q')), VIEWPORT, t(0));
        assert!(chat.reader.is_some(), "q typed instead of closing");
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: "q".to_string()
            })
        );
    }

    #[test]
    fn plan_approve_dispatches_from_the_reader() {
        let model = plan_ask_model();
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('2')), VIEWPORT, t(0));
        let answer = answer_of(handle_chat_key(
            &mut chat,
            &model,
            press(KeyCode::Enter),
            VIEWPORT,
            t(0),
        ));
        assert_eq!(
            answer,
            amux_ui::claude::encoding::AskAnswer::Plan(PlanAnswer::ApproveManual)
        );
    }

    /// Accepted plans reopen with Ctrl+T; ←/→ steps between them; q
    /// closes (B6).
    #[test]
    fn ctrl_t_opens_the_newest_plan_and_arrows_step() {
        let mut model = idle_model();
        for (n, id) in [(2u8, "toolu_a"), (5u8, "toolu_b")] {
            fold(
                &mut model,
                vec![rows(
                    n as i64,
                    n as u64 * 10,
                    vec![
                        json!({
                            "type": "assistant",
                            "uuid": format!("dddddddd-0000-4000-8000-00000000aa{n:02}"),
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-12T09:00:01.000Z",
                            "message": {"id": format!("msg_{id}"), "role": "assistant",
                                        "stop_reason": "tool_use",
                                        "content": [{"type": "tool_use", "id": id,
                                                     "name": "ExitPlanMode",
                                                     "input": {"plan": format!("# plan {id}")}}]},
                        }),
                        json!({
                            "type": "user",
                            "uuid": format!("dddddddd-0000-4000-8000-00000000bb{n:02}"),
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-12T09:00:02.000Z",
                            "message": {"role": "user", "content": [
                                {"type": "tool_result", "tool_use_id": id,
                                 "content": "User has approved your plan."}
                            ]},
                            "toolUseResult": {"plan": format!("# plan {id}")},
                        }),
                    ],
                )],
            );
        }
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, ctrl('t'), VIEWPORT, t(0));
        assert!(
            matches!(
                &chat.reader,
                Some(super::ReaderView {
                    source: super::ReaderSource::Plans { index: 1 },
                    ..
                })
            ),
            "Ctrl+T opens the newest accepted plan"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Left), VIEWPORT, t(0));
        assert!(matches!(
            &chat.reader,
            Some(super::ReaderView {
                source: super::ReaderSource::Plans { index: 0 },
                ..
            })
        ));
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('q')), VIEWPORT, t(0));
        assert!(chat.reader.is_none(), "q closes the reader");
    }

    fn readonly_ask_model() -> Model {
        let mut model = Model::default();
        let mut msgs = base_msgs();
        if let Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent }) = &mut msgs[2] {
            agent.readonly = true;
        } else {
            panic!("fixture shape");
        }
        fold(&mut model, msgs);
        fold(&mut model, vec![rows(1, 1, vec![ready_row()])]);
        fold(&mut model, vec![rows(2, 2, vec![prompt_row(1)])]);
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![hook_row(
                    "Edit",
                    json!({"file_path": "sync/config.rs", "old_string": "a\n", "new_string": "b\n"}),
                    1,
                )],
            )],
        );
        model
    }

    #[test]
    fn readonly_chats_read_and_leave_but_never_write() {
        let model = readonly_ask_model();
        let mut chat = open_chat(&model);
        assert!(
            chat.reader.is_none(),
            "read-only plan/ask never auto-opens a reader — the fact panel renders"
        );
        // Write affordances are absent, not disabled: no interrupt, no
        // answers, no typing.
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0)),
            None
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('1')), VIEWPORT, t(0)),
            None
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None
        );
        assert!(chat.composer.is_empty(), "nothing typed anywhere");
        // The one read affordance: f opens the diff reader.
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('f')), VIEWPORT, t(0));
        assert!(chat.reader.is_some());
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('q')), VIEWPORT, t(0));
        assert!(chat.reader.is_none(), "q closes the reader first");
        // q with no reader leaves the chat — back to the fleet (F1).
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('q')), VIEWPORT, t(0)),
            Some(UiAction::CloseChat)
        );
    }

    /// End then one Up must move immediately in a resolved-plan reader:
    /// the scroll metrics use the SAME tail derivation the frame renders
    /// (a one-row hint tail here, not the writable action-row tail), so
    /// the stored offset never lands past the render clamp.
    #[test]
    fn end_then_up_moves_immediately_in_a_resolved_reader() {
        let plan: String = (1..=30)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut model = idle_model();
        fold(
            &mut model,
            vec![rows(
                2,
                10,
                vec![
                    json!({
                        "type": "assistant",
                        "uuid": "dddddddd-0000-4000-8000-00000000cc01",
                        "sessionId": "22222222-2222-4222-8222-222222222222",
                        "timestamp": "2026-08-12T09:00:01.000Z",
                        "message": {"id": "msg_long", "role": "assistant",
                                    "stop_reason": "tool_use",
                                    "content": [{"type": "tool_use", "id": "toolu_long",
                                                 "name": "ExitPlanMode",
                                                 "input": {"plan": plan}}]},
                    }),
                    json!({
                        "type": "user",
                        "uuid": "dddddddd-0000-4000-8000-00000000cc02",
                        "sessionId": "22222222-2222-4222-8222-222222222222",
                        "timestamp": "2026-08-12T09:00:02.000Z",
                        "message": {"role": "user", "content": [
                            {"type": "tool_result", "tool_use_id": "toolu_long",
                             "content": "User has approved your plan."}
                        ]},
                        "toolUseResult": {"plan": plan},
                    }),
                ],
            )],
        );
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, ctrl('t'), VIEWPORT, t(0));
        // Resolved reader: tail is the one hint row, so at 80x20 the body
        // shows 14 rows of the 30-line plan — max top is 16.
        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT, t(0));
        assert_eq!(
            chat.reader.as_ref().expect("reader open").scroll,
            16,
            "End lands exactly on the render clamp"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Up), VIEWPORT, t(0));
        assert_eq!(
            chat.reader.as_ref().expect("reader open").scroll,
            15,
            "one Up moves immediately — no dead presses"
        );
    }

    /// An answer submitted FROM the reader that the reducer refuses
    /// synchronously must state its failure visibly: the reader closes
    /// to the docked panel, which renders the refusal on the next frame
    /// (the same drop an async SendFailed takes).
    #[test]
    fn a_refused_answer_from_the_reader_surfaces_in_the_docked_panel() {
        let mut model = plan_ask_model();
        let mut chat = open_chat(&model);
        assert!(chat.reader.is_some(), "plan review opens the reader");
        // The daemon link drops; the next dispatch refuses synchronously.
        fold(
            &mut model,
            vec![Msg::Server(ServerMsg::Disconnected {
                reason: amux_ui::DisconnectReason::ApplicationShutdown,
            })],
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('1')), VIEWPORT, t(0));
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("the confirm dispatches");
        };
        let op = OpId(Uuid::from_u128(43));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        chat.reconcile(&model);
        assert!(
            chat.reader.is_none(),
            "the reader dropped to the docked panel"
        );
        assert_eq!(
            chat.ask_failure.as_deref(),
            Some(amux_ui::NOT_CONNECTED_ERROR)
        );
        // The next frame states it: the docked panel renders the failure.
        let ctx = crate::render::FrameContext {
            viewport: VIEWPORT,
            theme: crate::render::Theme::Dark,
            now: t(60),
        };
        let frame = crate::chat::build_chat_lines(&model, &chat, &ctx);
        let text: String = frame
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(
            text.contains("not connected"),
            "the refusal is visible on the next frame"
        );
    }

    /// Whitespace-only Other text is no answer (the encoder's trimmed
    /// emptiness rule, applied at the form): it neither submits a
    /// single-select question nor marks the review tab answered.
    #[test]
    fn whitespace_only_other_stays_unanswered() {
        // Single-select: committing spaces chooses nothing and Enter
        // cannot submit the form.
        let model = question_model(false);
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        for _ in 0..3 {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(' ')), VIEWPORT, t(0));
        }
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None,
            "committing whitespace chooses nothing"
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None,
            "the form stays unanswered — no dispatch, no encoder refusal"
        );

        // Multi-select: the review tab must not lie about answered state.
        let model = question_model(true);
        let mut chat = open_chat(&model);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        for _ in 0..2 {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(' ')), VIEWPORT, t(0));
        }
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)); // commit: not chosen
        handle_chat_key(&mut chat, &model, press(KeyCode::Tab), VIEWPORT, t(0)); // to submit
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None,
            "the unanswered review refuses to submit"
        );
    }

    #[test]
    fn a_pending_answer_leaves_the_panel_inert() {
        let mut model = edit_ask_model();
        let mut chat = open_chat(&model);
        let action = {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('1')), VIEWPORT, t(0));
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        };
        let Some(UiAction::Dispatch(command)) = action else {
            panic!("the confirm dispatches");
        };
        let op = OpId(Uuid::from_u128(41));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        chat.reconcile(&model);
        // The answer is optimistically in flight: no second dispatch, no
        // stage changes.
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            None
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('1')), VIEWPORT, t(0)),
            None
        );
    }

    #[test]
    fn remote_resolution_dismisses_the_panel() {
        let mut model = edit_ask_model();
        let mut chat = open_chat(&model);
        assert!(chat.ask_ui.is_some());
        // The turn-end authority closes the ask (remote resolution's
        // catch-up path); the panel dismisses on reconcile.
        fold(
            &mut model,
            vec![rows(
                4,
                4,
                vec![json!({
                    "type": "system",
                    "subtype": "turn_duration",
                    "uuid": "dddddddd-0000-4000-8000-0000000000ff",
                    "sessionId": "22222222-2222-4222-8222-222222222222",
                    "timestamp": "2026-08-12T09:00:30.000Z",
                    "durationMs": 30000,
                })],
            )],
        );
        chat.reconcile(&model);
        assert!(
            chat.ask_ui.is_none(),
            "the panel dismissed; the fact renders"
        );
    }

    #[test]
    fn a_new_head_gets_a_fresh_panel() {
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![
                    hook_row("Bash", json!({"command": "echo one"}), 1),
                    hook_row("Bash", json!({"command": "echo two"}), 1),
                ],
            )],
        );
        let mut chat = open_chat(&model);
        // Open the deny stage on the first ask.
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        // The first ask resolves (its tool_use correlates, then denies).
        fold(
            &mut model,
            vec![rows(
                4,
                4,
                vec![
                    json!({
                        "type": "assistant",
                        "uuid": "dddddddd-0000-4000-8000-0000000000e1",
                        "sessionId": "22222222-2222-4222-8222-222222222222",
                        "timestamp": "2026-08-12T09:00:10.000Z",
                        "message": {"id": "msg_e1", "role": "assistant", "stop_reason": "tool_use",
                                    "content": [{"type": "tool_use", "id": "toolu_e1",
                                                 "name": "Bash", "input": {"command": "echo one"}}]},
                    }),
                    json!({
                        "type": "user",
                        "uuid": "dddddddd-0000-4000-8000-0000000000e2",
                        "sessionId": "22222222-2222-4222-8222-222222222222",
                        "timestamp": "2026-08-12T09:00:11.000Z",
                        "toolDenialKind": "user-rejected",
                        "message": {"role": "user", "content": [
                            {"type": "tool_result", "tool_use_id": "toolu_e1", "is_error": true,
                             "content": "denied"}
                        ]},
                    }),
                ],
            )],
        );
        chat.reconcile(&model);
        let ui = chat.ask_ui.as_ref().expect("second ask heads the queue");
        assert_eq!(ui.ask_id, 1, "fresh panel for the new head");
        assert!(
            matches!(ui.stage, crate::chat::ask_ui::AskStage::Menu { cursor: 0 }),
            "the old ask's deny stage died with it"
        );
    }

    #[test]
    fn a_successful_send_leaves_no_failure_and_no_pending_watch() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::InputSent,
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.send_failure(), None);
        assert!(chat.composer.is_empty());
    }
}
