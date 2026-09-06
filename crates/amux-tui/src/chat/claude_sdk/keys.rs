//! Every key this chat takes, and what it means.
//!
//! The shape is the chat shape: the leader chords compose over
//! everything, Ctrl+C is the guarded abandon key nothing else may see
//! first, and the composer takes what is left. What is particular to a
//! session driven over stream-JSON is that the permission mode is a
//! session fact this screen can change directly — Shift+Tab cycles it —
//! rather than a menu somewhere inside the agent's own terminal.

use amux_ui::claude_sdk::{AskState, ClaudeSdkCommand, SdkPhase, SendGate};
use amux_ui::{AgentId, Command, DiffBase, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{View, reader_actionable, shared_ask};
use crate::chat::claude_shared::ask_ui::{AskKeyOutcome, AskStage, AskUi, PanelAnswer};
use crate::chat::claude_shared::reader::{self, ReaderSource, ReaderView};
use crate::chat::inline::{InlineAsk, InlineOutcome};
use crate::chat::viewport::ScrollIntent;
use crate::clipboard::ClipboardContent;
use crate::composer;
use crate::review::ReviewOutcome;
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
    chat.reconcile(model);

    // The leader chords are chrome navigation: they compose over the
    // reader, the review page and the help overlay alike.
    if chat.pending_leader {
        chat.pending_leader = false;
        chat.quit_guard.disarm();
        return match key.code {
            KeyCode::Char('s') => Some(UiAction::CloseChat),
            KeyCode::Char('d') => Some(UiAction::Quit),
            KeyCode::Char('n') => {
                crate::chat::next_in_family(model, chat.agent).map(UiAction::OpenChat)
            }
            KeyCode::Char('m') => {
                chat.reports_open = !chat.reports_open;
                None
            }
            KeyCode::Char('a') => {
                toggle_inline_ask(chat, model);
                None
            }
            KeyCode::Char('r') if !chat.read_only(model) => open_review(chat),
            // `<leader> c`: what the context is spent on. Fetching costs
            // the session a round trip, so it happens when a person asks
            // and again only when they ask again.
            KeyCode::Char('c') => request_context_breakdown(chat, model),
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

    // Ctrl+C is the chrome-wide guarded abandon key, intercepted before
    // any panel, reader, read-only surface or overlay sees it: a focused
    // non-empty text field is cleared as a kill; otherwise the press arms
    // the guard, and a fresh second press quits.
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
    chat.quit_guard.disarm();
    // Any keypress dismisses a stated failure; the Model keeps the
    // outcome either way.
    chat.send_failure = None;
    chat.ask_failure = None;

    // The `?` overlay: any other key closes it and is consumed.
    if chat.help {
        chat.help = false;
        return None;
    }

    if chat.review_open() {
        return review_key(chat, model, key, viewport);
    }

    // The breakdown overlay covers the frame: esc closes it, the leader
    // chords above still compose over it, and nothing else leaks into the
    // draft behind it.
    if chat.context_open {
        if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
            chat.context_open = false;
        }
        return None;
    }

    // A pending ask owns the surface, so the panel has to exist before a
    // key is routed: a chat may take a keystroke before its first
    // reconcile.
    chat.sync_ask(model);

    if chat.reader.is_some() {
        return reader_key(chat, model, key, viewport);
    }

    if chat.read_only(model) {
        return readonly_key(chat, model, key, viewport);
    }

    // A docked child ask owns the composer area and its keys, including
    // Ctrl+X, which interrupts the agent whose ask is on screen.
    if chat.inline_ask.is_some() {
        return inline_key(chat, model, key, viewport);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Char('x') && ctrl {
        return interrupt(chat, model);
    }

    // The view-only Esc chain: it steps back through what is open and
    // never answers an ask.
    if key.code == KeyCode::Esc {
        esc_chain(chat);
        return None;
    }

    // This session's own ask owns the composer area and its keys.
    if let Some(head) = chat.ask_head(model) {
        if matches!(head.state, AskState::AnsweredOptimistic { .. }) {
            // The collapsed marker holds the panel while the answer is in
            // flight: there is nothing to select, so only the feed moves.
            scroll_keys(chat, &key);
            return None;
        }
        return panel_key(chat, model, key);
    }

    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            chat.composer.insert_newline();
        }
        KeyCode::Enter => return send(chat, model),
        // Shift+Tab cycles the permission mode the header states. The
        // session owns the order and refuses a mode it was not launched
        // with, so nothing is decided here but the request.
        KeyCode::BackTab => {
            return allows_mode_cycle(model, chat.agent).then_some(UiAction::Dispatch(
                Command::ClaudeSdk(ClaudeSdkCommand::CyclePermissionMode { agent: chat.agent }),
            ));
        }
        KeyCode::Char('j') if ctrl => chat.composer.insert_newline(),
        KeyCode::Char('p') if ctrl => chat.composer.up(),
        KeyCode::Char('n') if ctrl => chat.composer.down(),
        // Ctrl+T reopens the plans this session already got through,
        // newest first; ←/→ steps back through the older ones.
        KeyCode::Char('t') if ctrl => {
            let plans = super::accepted_plans(model, chat.agent).len();
            if plans > 0 {
                chat.reader = Some(ReaderView {
                    source: ReaderSource::Plans { index: plans - 1 },
                    scroll: 0,
                });
            }
        }
        // Ctrl+V: attach what the clipboard holds. A terminal cannot
        // deliver image bytes through a bracketed paste, so this is the
        // one path an image reaches a draft by.
        KeyCode::Char('v') if ctrl => {
            attach_clipboard(chat, model, crate::clipboard::read_clipboard());
        }
        KeyCode::Up => chat.composer.up(),
        KeyCode::Down => chat.composer.down(),
        KeyCode::PageUp => page_up(chat),
        KeyCode::PageDown => page_down(chat),
        KeyCode::Home if ctrl => jump_top(chat),
        KeyCode::End if ctrl => follow(chat),
        KeyCode::Char('?') if !ctrl && chat.composer.is_empty() => chat.help = true,
        KeyCode::Tab => {
            return crate::chat::queue::key(
                model,
                chat.agent,
                &mut chat.composer,
                chat.review.as_ref().map(|draft| draft.view.review()),
            );
        }
        _ => {
            composer::readline_key(&mut chat.composer, &key);
        }
    }
    None
}

pub(crate) fn handle_chat_paste(chat: &mut View, model: &Model, text: &str) {
    chat.send_failure = None;
    chat.ask_failure = None;
    chat.quit_guard.disarm();
    chat.reconcile(model);
    // Every surface that covers the composer drops the paste rather than
    // writing into a draft the person cannot see.
    if chat.help || chat.context_open || chat.review_open() || chat.read_only(model) {
        return;
    }
    // An open answer field takes the paste as one line; the draft behind
    // a docked panel is not what the person is typing into.
    if chat.reader.is_some() {
        if reader_actionable(model, chat)
            && let Some(field) = chat.ask_ui.as_mut().and_then(AskUi::active_field)
        {
            field.paste(&one_line(text));
        }
        return;
    }
    if let Some(ui) = chat.ask_ui.as_mut() {
        if let Some(field) = ui.active_field() {
            field.paste(&one_line(text));
        }
        return;
    }
    if let Some(inline) = chat.inline_ask.as_mut() {
        crate::chat::inline::handle_paste(inline, &one_line(text));
        return;
    }
    // Every surface that covers the composer drops the paste rather than
    // writing into a draft the person cannot see.
    if chat.help || chat.reader.is_some() || chat.review_open() || chat.read_only(model) {
        return;
    }
    chat.composer.paste_or_attach(text);
}

/// Pasted text as a one-line answer field takes it.
fn one_line(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', " ")
}

/// Ctrl+V: attach what the clipboard holds.
///
/// The content is a parameter, not read here, so the binding is testable
/// without a host clipboard. Text is a paste like any other and follows
/// the same focus routing; an image or a file has no home in a docked
/// ask's one-line field, so it is dropped rather than attached to the
/// draft hidden behind it.
pub(crate) fn attach_clipboard(chat: &mut View, model: &Model, content: ClipboardContent) {
    if let ClipboardContent::Text(text) = content {
        handle_chat_paste(chat, model, &text);
        return;
    }
    chat.send_failure = None;
    chat.quit_guard.disarm();
    chat.reconcile(model);
    if chat.help
        || chat.reader.is_some()
        || chat.review_open()
        || chat.context_open
        || chat.read_only(model)
        || chat.ask_ui.is_some()
        || chat.inline_ask.is_some()
    {
        return;
    }
    chat.send_failure = crate::chat::attach::attach_clipboard(&mut chat.composer, content);
}

/// `<leader> c`: ask the session where its context went and put the
/// answer on screen. Pressing it again while the overlay is open is a
/// refresh, which is another round trip and so another request.
fn request_context_breakdown(chat: &mut View, model: &Model) -> Option<UiAction> {
    if !allows_context_breakdown(model, chat.agent) {
        return None;
    }
    chat.context_open = true;
    Some(UiAction::Dispatch(Command::ClaudeSdk(
        ClaudeSdkCommand::RequestContextBreakdown { agent: chat.agent },
    )))
}

/// Whether the breakdown request would reach the session at all — a
/// session that cannot take a control call has nothing to answer with,
/// and a hint must not name a dead key.
pub(crate) fn allows_context_breakdown(model: &Model, agent: AgentId) -> bool {
    matches!(
        amux_ui::claude_sdk::send_gate(model, agent),
        SendGate::Ready | SendGate::Working | SendGate::NeedsYou
    )
}

/// Whether the mode-cycle chord would reach the session at all. The
/// session refuses it while it cannot take input and while it has never
/// reported a mode to cycle from, and a hint must not name a dead key.
pub(crate) fn allows_mode_cycle(model: &Model, agent: AgentId) -> bool {
    matches!(
        amux_ui::claude_sdk::send_gate(model, agent),
        SendGate::Ready | SendGate::Working | SendGate::NeedsYou
    ) && model
        .claude_sdk(agent)
        .is_some_and(|layer| layer.session().permission_mode.is_some())
}

/// Interrupt reaches the session whenever there is something to stop.
fn interrupt(chat: &View, model: &Model) -> Option<UiAction> {
    matches!(
        amux_ui::claude_sdk::phase(model, chat.agent),
        SdkPhase::Working | SdkPhase::NeedsYou { .. }
    )
    .then(|| {
        UiAction::Dispatch(Command::ClaudeSdk(ClaudeSdkCommand::Interrupt {
            agent: chat.agent,
        }))
    })
}

fn send(chat: &mut View, model: &Model) -> Option<UiAction> {
    if chat.composer.is_empty() {
        return None;
    }
    if amux_ui::claude_sdk::send_gate(model, chat.agent)
        .refusal()
        .is_some()
    {
        return None;
    }
    // A draft with no tokens sends exactly as it always did: the
    // attachment command exists for drafts that actually carry one.
    let attached = !chat.composer.tokens().is_empty();
    let (text, attachments) = chat
        .composer
        .export(chat.review.as_ref().map(|draft| draft.view.review()));
    chat.composer.clear_for_send();
    Some(UiAction::Dispatch(if attached {
        Command::SendPromptWithAttachments {
            agent: chat.agent,
            text,
            attachments,
        }
    } else {
        Command::ClaudeSdk(ClaudeSdkCommand::SendPrompt {
            agent: chat.agent,
            text,
        })
    }))
}

/// `<leader> r`: resume the review already frozen, or ask for the diff a
/// fresh one is frozen against.
fn open_review(chat: &mut View) -> Option<UiAction> {
    if let Some(draft) = chat.review.as_mut() {
        draft.open = true;
        return None;
    }
    if chat.diff_pending() {
        return None;
    }
    Some(UiAction::Dispatch(Command::RequestDiff {
        agent: chat.agent,
        base: DiffBase::WorkingTree,
    }))
}

/// Keys while the review page is open. The page decides everything about
/// itself; this handles only what leaving it and writing on it mean to
/// the chat around it.
fn review_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    // Interrupt reaches the agent from every focus state, the open
    // comment box included — it is a control chord, so it can never be
    // something the person meant to type.
    if key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return interrupt(chat, model);
    }
    let draft = chat.review.as_mut()?;
    draft.view.set_viewport(viewport.0, viewport.1);
    match draft.view.handle_key(&key) {
        ReviewOutcome::CommentsChanged => {
            // The token's label counts the comments behind it, so it has
            // to catch up with whatever was just written.
            let mut draft = chat.review.take()?;
            draft.sync_token(&mut chat.composer);
            chat.review = Some(draft);
            None
        }
        ReviewOutcome::Close => {
            draft.open = false;
            // Back in the draft the cursor sits just PAST the token, where
            // Enter sends. On it, Enter would reopen the page just left.
            if let Some(slot) = draft.slot {
                chat.composer.cursor_after_token(slot);
            }
            None
        }
        ReviewOutcome::SwitchBase(base) => Some(UiAction::Dispatch(Command::RequestDiff {
            agent: chat.agent,
            base,
        })),
        ReviewOutcome::Handled | ReviewOutcome::Ignored => None,
    }
}

/// Keys while the fullscreen reader is open: the reader's own scrolling
/// and plan stepping, and Esc to leave it.
fn reader_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    if key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return interrupt(chat, model);
    }
    // Esc steps back through the ask's own stages before it closes the
    // reader, so a half-typed request for changes survives one press.
    if key.code == KeyCode::Esc {
        esc_chain(chat);
        return None;
    }
    // The plan's action row lives in the reader while a writable ask is
    // open: ↑↓ scroll the plan there, so the panel does not take them.
    if reader_actionable(model, chat)
        && let Some(head) = chat.ask_head(model)
    {
        let ask_id = head.id;
        let shared = shared_ask(head);
        let outcome = chat
            .ask_ui
            .as_mut()
            .map(|ui| ui.handle_key(&shared, &key, false))
            .unwrap_or(AskKeyOutcome::NotHandled);
        match outcome {
            AskKeyOutcome::Answer(answer) => return dispatch_answer(chat, ask_id, answer),
            AskKeyOutcome::Handled | AskKeyOutcome::OpenReader => {
                // The deny stage is docked, not read: the reader closes to
                // the panel that owns the field.
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
        // q leaves a read surface; nothing here is a text field, so a bare
        // letter is safe.
        KeyCode::Char('q') => chat.reader = None,
        // ←/→ step between accepted plans.
        KeyCode::Left | KeyCode::Right => {
            let delta = if key.code == KeyCode::Left { -1 } else { 1 };
            if let Some(index) =
                super::reader_context(model, chat).and_then(|ctx| reader::plans_step(&ctx, delta))
                && let Some(view) = chat.reader.as_mut()
            {
                view.source = ReaderSource::Plans { index };
                view.scroll = 0;
            }
        }
        _ => {
            reader_scroll(chat, model, &key, viewport);
        }
    }
    None
}

/// Pager motion over the reader body: ↑↓ j/k, PgUp/PgDn, Home/End g/G.
fn reader_scroll(chat: &mut View, model: &Model, key: &KeyEvent, viewport: (u16, u16)) -> bool {
    let Some((page, max_top)) =
        super::reader_context(model, chat).and_then(|ctx| reader::scroll_metrics(&ctx, viewport))
    else {
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

/// The view-only Esc chain, checked in order — first hit wins. Esc never
/// answers an ask and never interrupts.
fn esc_chain(chat: &mut View) {
    // An open text field inside the reader closes first, then the reader
    // itself; a plan reader drops to its docked panel form.
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
    // Ask stages step back toward their menu; the panel itself stays
    // while its ask pends.
    if let Some(ui) = chat.ask_ui.as_mut()
        && ui.step_back()
    {
        return;
    }
    if chat.composer.is_empty() {
        follow(chat);
    }
}

/// Keys while this session's ask is docked and interactive: the stage
/// machine first, the feed's scroll keys as the fallback — the feed stays
/// readable behind the panel a decision is made from.
fn panel_key(chat: &mut View, model: &Model, key: KeyEvent) -> Option<UiAction> {
    let head = chat.ask_head(model)?;
    let ask_id = head.id;
    let shared = shared_ask(head);
    let outcome = chat
        .ask_ui
        .as_mut()
        .map(|ui| ui.handle_key(&shared, &key, true))
        .unwrap_or(AskKeyOutcome::NotHandled);
    match outcome {
        AskKeyOutcome::Answer(answer) => dispatch_answer(chat, ask_id, answer),
        AskKeyOutcome::OpenReader => {
            chat.reader = Some(ReaderView::ask());
            None
        }
        AskKeyOutcome::Handled => None,
        AskKeyOutcome::NotHandled => {
            scroll_keys(chat, &key);
            None
        }
    }
}

/// Send a collected answer to the session. Every kind this panel can
/// collect is one this transport carries, so nothing is dropped here.
fn dispatch_answer(chat: &View, ask: u64, answer: PanelAnswer) -> Option<UiAction> {
    Some(UiAction::Dispatch(super::answer_command(
        chat.agent, ask, answer,
    )))
}

/// `<leader> a`: dock the ask the banner names, or send it back. Nothing
/// happens when the banner names a child with no panel to dock.
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

/// Keys while a child's ask is docked here: the child's layer's own panel
/// first, this chat's feed scrolling as the fallback.
fn inline_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let _ = viewport;
    let inline = chat.inline_ask.as_mut()?;
    match crate::chat::inline::handle_key(model, inline, &key) {
        InlineOutcome::Dispatch(command) => Some(UiAction::Dispatch(command)),
        InlineOutcome::Close => {
            chat.inline_ask = None;
            None
        }
        InlineOutcome::Handled => None,
        InlineOutcome::NotHandled => {
            scroll_keys(chat, &key);
            None
        }
    }
}

fn readonly_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let _ = viewport;
    match key.code {
        KeyCode::Char('q') => return Some(UiAction::CloseChat),
        // An observer reads the document an ask is about; the fact panel
        // offers `f` only while there is one.
        KeyCode::Char('f') => {
            if chat
                .ask_head(model)
                .is_some_and(|ask| shared_ask(ask).has_readable())
            {
                chat.reader = Some(ReaderView::ask());
            }
        }
        KeyCode::Esc => follow(chat),
        KeyCode::PageUp => page_up(chat),
        KeyCode::PageDown => page_down(chat),
        KeyCode::Up | KeyCode::Char('k') => request_scroll(chat, ScrollIntent::Rows(-1)),
        KeyCode::Down | KeyCode::Char('j') => request_scroll(chat, ScrollIntent::Rows(1)),
        KeyCode::Home | KeyCode::Char('g') => jump_top(chat),
        KeyCode::End | KeyCode::Char('G') => follow(chat),
        KeyCode::Char('?') => chat.help = true,
        _ => {}
    }
    None
}

/// The one text field a key could be typing into right now — what Ctrl+C
/// clears instead of arming the quit guard.
fn focused_field<'v>(
    chat: &'v mut View,
    model: &Model,
) -> Option<&'v mut crate::composer::Composer> {
    if chat.help || chat.review_open() || chat.context_open || chat.read_only(model) {
        return None;
    }
    if chat.reader.is_some() {
        return reader_actionable(model, chat)
            .then(|| chat.ask_ui.as_mut().and_then(AskUi::active_field))
            .flatten();
    }
    if chat.ask_ui.is_some() {
        // A docked panel covers the composer: its open text stage is the
        // field, and its menu stages have none.
        return chat.ask_ui.as_mut().and_then(AskUi::active_field);
    }
    if chat.inline_ask.is_some() {
        return None;
    }
    Some(&mut chat.composer)
}

fn scroll_keys(chat: &mut View, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => page_up(chat),
        KeyCode::PageDown => page_down(chat),
        KeyCode::Home if ctrl => jump_top(chat),
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

fn page_up(chat: &mut View) {
    request_scroll(chat, ScrollIntent::Page(-1));
}

fn page_down(chat: &mut View) {
    request_scroll(chat, ScrollIntent::Page(1));
}

fn jump_top(chat: &mut View) {
    request_scroll(chat, ScrollIntent::Oldest);
}
