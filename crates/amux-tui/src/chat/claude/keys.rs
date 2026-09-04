//! Chat key handling: keys mutate View and produce `UiAction`s; all
//! domain writes leave as Commands through the runtime (never bytes —
//! answers live in amux-ui's C6 module).
//!
//! The binding set is `docs/CHAT.md` §Keybindings' plain tier, derived in
//! `docs/CHAT.md` §Keybindings: readline is law inside the composer
//! (P6), reflex keys stay harmless (P4), interrupt shares a key with
//! nothing (P5). Kitty-tier sugar (Shift+Enter newline) is absent until
//! the chrome feature-detects kitty — Phase 6; hints never advertise it.

use amux_ui::claude::AskState;
use amux_ui::claude::answer::{self, AskAnswer};
use amux_ui::{ClaudeCommand, Command, DiffBase, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::chat::claude::{View, reader_actionable, reader_context, shared_ask};
use crate::chat::claude_shared::ask_ui::{AskKeyOutcome, AskStage, AskUi};
use crate::chat::claude_shared::reader::{self, ReaderSource, ReaderView};
use crate::chat::inline::{InlineAsk, InlineOutcome};
use crate::chat::viewport::ScrollIntent;
use crate::clipboard::ClipboardContent;
use crate::composer;
use crate::composer::Composer;
use crate::review::ReviewOutcome;
use crate::view::UiAction;

pub fn handle_chat_key(
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
            // `<leader> r`: review the agent's diff. A leader chord for the
            // same reason the others are: it opens a whole screen, it must
            // work from under a panel, and it must never reach a draft. A
            // read-only chat has no draft to put a review in, so it has no
            // review either (F1).
            KeyCode::Char('r') if !chat.read_only(model) => open_review(chat),
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

    // The review page replaces the whole frame while it is open, so it owns
    // every key the chrome did not already claim — including Esc, which
    // steps back inside the page, and the letters the composer would
    // otherwise type.
    if chat.review_open() {
        return review_key(chat, key, viewport);
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
        KeyCode::Char('x') if ctrl && chat.inline_ask.is_none() => {
            return Some(UiAction::Dispatch(Command::Claude(
                ClaudeCommand::Interrupt { agent: chat.agent },
            )));
        }
        // The settled view-only Esc chain: never answers, never
        // interrupts. A docked guest panel owns Esc first — it is the
        // way back out of somebody else's ask.
        KeyCode::Esc if chat.inline_ask.is_none() => {
            esc_chain(chat);
            return None;
        }
        _ => {}
    }

    // A docked child ask owns the composer area and its keys, exactly as
    // this chat's own ask would (C1) — including Ctrl+X, which
    // interrupts the agent whose ask is on screen.
    if chat.inline_ask.is_some() {
        return inline_key(chat, model, key, viewport);
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
/// clears. This mirrors the focus derivation keys and paste routing use —
/// the invisible composer behind a docked panel is never "focused".
fn focused_field<'c>(chat: &'c mut View, model: &Model) -> Option<&'c mut Composer> {
    match focus(chat, model) {
        Focus::Nothing => None,
        Focus::Ask => chat.ask_ui.as_mut().and_then(AskUi::active_field),
        Focus::Inline => chat.inline_ask.as_mut().and_then(InlineAsk::active_field),
        Focus::Review => chat
            .review
            .as_mut()
            .and_then(|draft| draft.view.editor_field_mut()),
        Focus::Composer => Some(&mut chat.composer),
    }
}

/// Which surface owns the keyboard right now. Derived from the Model and
/// the view together, and separately from taking the field, so the two
/// callers that need the answer — the guarded Ctrl+C and paste routing —
/// cannot drift apart.
enum Focus {
    Nothing,
    /// This agent's own ask, docked or under the reader.
    Ask,
    /// A child's ask, docked here.
    Inline,
    /// The open review page — its comment box when one is open.
    Review,
    Composer,
}

fn focus(chat: &View, model: &Model) -> Focus {
    // A read-only chat has no field anywhere (F1); the open help overlay
    // covers whatever field there was.
    if chat.read_only(model) || chat.help {
        return Focus::Nothing;
    }
    // The review page covers the composer; only its comment box is a field.
    if chat.review_open() {
        return Focus::Review;
    }
    if chat.reader.is_some() {
        return match reader_actionable(model, chat) {
            true => Focus::Ask,
            false => Focus::Nothing,
        };
    }
    // A docked child ask covers the composer, so the field it has open
    // is the focused one — the guarded Ctrl+C clears a half-typed denial
    // to a child the same way it clears one to this agent.
    if chat.inline_ask.is_some() {
        return Focus::Inline;
    }
    match chat.ask_head(model).map(|ask| &ask.state) {
        // An interactive ask head owns the surface: its open text stage
        // is the field; its menu stages have none.
        Some(AskState::Pending | AskState::SendFailed { .. }) => Focus::Ask,
        // The optimistic-pending marker has no field; otherwise the
        // composer is focused.
        Some(AskState::AnsweredOptimistic { .. }) => Focus::Nothing,
        None => Focus::Composer,
    }
}

/// The composer focus: the chat-level bindings (Phase 4's key set, plus
/// Ctrl+T for the plan reader) layered over the shared readline set
/// ([`composer::readline_key`] — the same machinery the panel text
/// fields dispatch through).
fn composer_key(
    chat: &mut View,
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
        // Enter ON the review token resumes the page; anywhere else it
        // sends. The token is one char wide, so the two are never the same
        // position, and leaving the page puts the cursor past it.
        KeyCode::Enter => {
            if let Some(slot) = chat.composer.review_token_at_cursor()
                && let Some(draft) = chat.review.as_mut()
                && draft.slot == Some(slot)
            {
                draft.open = true;
                return None;
            }
            return send(chat, model);
        }
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
            if amux_ui::claude::mode_cycle_gate(model, chat.agent).is_none() {
                return Some(UiAction::Dispatch(Command::Claude(
                    ClaudeCommand::CyclePermissionMode { agent: chat.agent },
                )));
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
        KeyCode::End if ctrl => follow(chat),
        // The `?` help overlay — on an EMPTY draft only; with anything
        // typed, `?` is a printable and types (P2). The footer hint
        // advertises it exactly when this branch is live.
        KeyCode::Char('?') if !ctrl && chat.composer.is_empty() => {
            chat.help = true;
        }
        // Ctrl+V: attach what the clipboard holds. A terminal cannot
        // deliver image bytes through a bracketed paste, so this is the
        // one path an image reaches a draft by.
        KeyCode::Char('v') if ctrl => {
            attach_clipboard(chat, model, crate::clipboard::read_clipboard());
        }
        // The shared readline set (P6): motion, kills, yank, printables —
        // `?` included on a non-empty draft. What it leaves stays
        // unbound, each an act of restraint: Ctrl+A (chrome leader),
        // Ctrl+G (emacs abort reflex — must never fire agent actions),
        // Ctrl+R (reserved: history search), Ctrl+L (shell redraw
        // reflex), and the byte-aliases Ctrl+H/I/M.
        _ => {
            composer::readline_key(&mut chat.composer, &key);
        }
    }
    discard_deleted_review(chat);
    None
}

/// Deleting the review token discards the review behind it.
///
/// The token is the review's only place in the draft, so removing it is
/// how a person throws one away — there is no other finish step to undo. A
/// kill is not a deletion: the token stays alive in the kill buffer, so
/// Ctrl+U then Ctrl+Y brings the review back with the words it sat among.
fn discard_deleted_review(chat: &mut View) {
    let Some(slot) = chat.review.as_ref().and_then(|draft| draft.slot) else {
        return;
    };
    if chat.composer.token(slot).is_none() {
        chat.review = None;
    }
}

/// Bracketed paste into the chat: literal insertion into the focused text
/// surface — tabs and newlines land as text, never as bindings (a pasted
/// CR must never submit a partial prompt). Routing follows the same
/// model/focus derivation as keys: a read-only chat has NO composer
/// (F1 — the paste is dropped, never retained invisibly); an open panel
/// text stage takes it (newlines flattened to spaces — panel fields are
/// one-line); a docked panel without a field, or an open reader, drops
/// it rather than typing into the invisible composer; only the composer
/// focus inserts (see [`crate::composer::Composer::paste`]).
pub fn handle_chat_paste(chat: &mut View, model: &Model, text: &str) {
    chat.send_failure = None;
    chat.ask_failure = None;
    // Paste is input like any other key: it disarms the quit guard.
    chat.quit_guard.disarm();
    // The fullscreen help overlay owns focus. Keep it open and drop the
    // paste, just as a read-only chat drops input instead of mutating the
    // hidden composer behind its visible surface.
    if chat.help {
        return;
    }
    // The same defensive sync keys run: a pending ask that has not been
    // reconciled yet still owns the surface — the paste must not slip
    // into the composer through that window.
    chat.sync_ask(model);
    if chat.read_only(model) {
        return;
    }
    if chat.reader.is_some() {
        if reader_actionable(model, chat)
            && let Some(field) = chat.ask_ui.as_mut().and_then(AskUi::active_field)
        {
            let one_line = text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', " ");
            field.paste(&one_line);
        }
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
    chat.composer.paste_or_attach(text);
}

/// Ctrl+V: attach what the clipboard holds.
///
/// The content is a parameter, not read here, so the binding is testable
/// without a host clipboard. Text is a paste like any other and follows
/// the same focus routing; an image or a file has no home in a one-line
/// answer field, so an open panel, reader or docked ask drops it rather
/// than attaching it to the draft hidden behind them.
pub(crate) fn attach_clipboard(chat: &mut View, model: &Model, content: ClipboardContent) {
    if let ClipboardContent::Text(text) = content {
        handle_chat_paste(chat, model, &text);
        return;
    }
    chat.send_failure = None;
    chat.ask_failure = None;
    chat.quit_guard.disarm();
    if chat.help {
        return;
    }
    chat.sync_ask(model);
    if chat.read_only(model)
        || chat.reader.is_some()
        || chat.ask_ui.is_some()
        || chat.inline_ask.is_some()
    {
        return;
    }
    chat.send_failure = crate::chat::attach::attach_clipboard(&mut chat.composer, content);
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

/// `<leader> r`: open the review page, or come back to the one this draft
/// already has.
///
/// The first press asks the daemon to freeze the repository's diff; the
/// page opens over the result when it arrives. Every later press resumes
/// the same frozen diff — a review that refetched would move the rows its
/// comments are anchored to out from under them. Nothing here is gated on
/// what the agent is doing: reviewing what it has written so far is most
/// wanted exactly while it is still writing.
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

/// Keys while the review page is open.
///
/// The page decides everything about itself; this handles only what
/// leaving it and writing on it mean to the chat around it.
fn review_key(chat: &mut View, key: KeyEvent, viewport: (u16, u16)) -> Option<UiAction> {
    // D3: interrupt reaches the agent from every focus state, the open
    // comment box included — it is a control chord, so it can never be
    // something the person meant to type.
    if key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(UiAction::Dispatch(Command::Claude(
            ClaudeCommand::Interrupt { agent: chat.agent },
        )));
    }
    let draft = chat.review.as_mut()?;
    // Scroll follows the cursor as the key moves it, so the page has to
    // know the screen it is on before the key arrives, not when it draws.
    draft.view.set_viewport(viewport.0, viewport.1);
    match draft.view.handle_key(&key) {
        ReviewOutcome::CommentsChanged => {
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

/// Enter: send, gated on phase by the same derivation the footer states
/// (D2) — while gated, Enter is a no-op and the draft is kept.
fn send(chat: &mut View, model: &Model) -> Option<UiAction> {
    if chat.composer.is_empty() {
        return None;
    }
    if amux_ui::claude::send_gate(model, chat.agent)
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
        Command::Claude(ClaudeCommand::SendPrompt {
            agent: chat.agent,
            text,
        })
    }))
}

/// The deterministic view-only Esc chain (`docs/CHAT.md` §State
/// transitions), checked in order — first hit wins. Esc never answers an
/// ask and never interrupts.
fn esc_chain(chat: &mut View) {
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
    if chat.composer.is_empty() {
        follow(chat);
    }
    // Stage 4: nothing.
}

// --- panel and reader routing ------------------------------------------------

/// Keys while an ask panel is docked and interactive (Pending or
/// SendFailed): the stage machine first, the feed's scroll keys as the
/// fallback — the feed stays scrollable behind the docked panel.
fn panel_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    let head = chat.ask_head(model)?;
    // Unverified menu shapes render read-only-style (C2): no actions to
    // route — only the read affordance and the feed scroll live.
    if answer::menu_shape_refusal(&head.kind).is_some() {
        if key.code == KeyCode::Char('f') && shared_ask(head).has_readable() {
            chat.reader = Some(ReaderView::ask());
            return None;
        }
        scroll_keys(chat, model, &key, viewport);
        return None;
    }
    let ask_id = head.id;
    let shared = shared_ask(head);
    let outcome = chat
        .ask_ui
        .as_mut()
        .map(|ui| ui.handle_key(&shared, &key, true))
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

fn dispatch_answer(chat: &View, ask: u64, answer: AskAnswer) -> UiAction {
    UiAction::Dispatch(Command::Claude(ClaudeCommand::AnswerAsk {
        agent: chat.agent,
        ask,
        answer,
    }))
}

/// Keys while the fullscreen reader is open: the writable ask's action
/// row / feedback stage first, then pager motion (P7 — no text field
/// means bare letters are safe; in the request-changes stage they type).
fn reader_key(
    chat: &mut View,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
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
            if let Some(index) =
                reader_context(model, chat).and_then(|ctx| reader::plans_step(&ctx, -1))
                && let Some(view) = chat.reader.as_mut()
            {
                view.source = ReaderSource::Plans { index };
                view.scroll = 0;
            }
            None
        }
        KeyCode::Right => {
            if let Some(index) =
                reader_context(model, chat).and_then(|ctx| reader::plans_step(&ctx, 1))
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
fn reader_scroll(chat: &mut View, model: &Model, key: &KeyEvent, viewport: (u16, u16)) -> bool {
    let Some((page, max_top)) =
        reader_context(model, chat).and_then(|ctx| reader::scroll_metrics(&ctx, viewport))
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

/// The feed's scroll keys, shared by every docked focus state.
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

/// The read-only chat (F1): a pager over the live feed. Bare letters are
/// safe — no text field exists here — and every write affordance is
/// absent, not disabled: no interrupt, no answers, no composer.
fn readonly_key(
    chat: &mut View,
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
            if chat
                .ask_head(model)
                .is_some_and(|ask| shared_ask(ask).has_readable())
            {
                chat.reader = Some(ReaderView::ask());
            }
        }
        KeyCode::Esc => {
            follow(chat);
        }
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

/// Jump to the feed's oldest retained line (Ctrl+Home; `g`/Home in the
/// read-only pager).
fn jump_top(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Oldest);
}

/// One-line pager motion (read-only chats: ↑/k, ↓/j).
fn line_up(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Rows(-1));
}

fn line_down(chat: &mut View, _model: &Model, _viewport: (u16, u16)) {
    request_scroll(chat, ScrollIntent::Rows(1));
}

#[cfg(test)]
mod tests {
    use amux_ui::{Model, Msg, OpId, ServerMsg, StreamEntry, StreamMsg, update};
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::chat::FeedScroll;
    use crate::chat::claude::render;
    use crate::chat::frame::{FrameSpacing, PaintCache, compose_chat_frame, feed_metrics};
    use crate::chat::viewport::{FeedViewport, apply_scroll};

    pub(super) fn agent_id() -> amux_ui::AgentId {
        Uuid::from_u128(7)
    }

    pub(super) fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("epoch")
    }

    pub(super) fn fold(model: &mut Model, msgs: Vec<Msg>) {
        for msg in msgs {
            update(model, msg);
        }
    }

    pub(super) fn base_msgs() -> Vec<Msg> {
        let agent = amux_ui::Agent {
            id: agent_id(),
            host_id: Uuid::from_u128(1),
            name: Some("fix-auth".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            kind: amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: t(0),
            parent: None,
            working_on: None,
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

    pub(super) fn rows(at: i64, first_seq: u64, payloads: Vec<serde_json::Value>) -> Msg {
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

    pub(super) fn ready_row() -> serde_json::Value {
        json!({"type": "amux.transcript_ready"})
    }

    pub(super) fn prompt_row(n: u8) -> serde_json::Value {
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

    pub(super) fn idle_model() -> Model {
        let mut model = Model::default();
        fold(&mut model, base_msgs());
        fold(&mut model, vec![rows(1, 1, vec![ready_row()])]);
        model
    }

    pub(super) fn working_model() -> Model {
        let mut model = idle_model();
        fold(&mut model, vec![rows(2, 2, vec![prompt_row(1)])]);
        model
    }

    pub(super) fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub(super) fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub(super) fn chat_with_draft(text: &str) -> View {
        let mut chat = View::open(agent_id(), 'a', false);
        chat.composer.insert_str(text);
        chat
    }

    pub(super) const VIEWPORT: (u16, u16) = (80, 20);

    #[test]
    fn enter_sends_when_ready_and_clears_the_draft() {
        let model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::Claude(
                ClaudeCommand::SendPrompt {
                    agent: agent_id(),
                    text: "add retry".to_string(),
                }
            )))
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
        let mut chat = View::open(agent_id(), 'a', false);
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
                Some(UiAction::Dispatch(Command::Claude(
                    ClaudeCommand::Interrupt { agent: agent_id() },
                )))
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
        let mut chat = View::open(agent_id(), 'a', false);
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
        let mut chat = View::open(agent_id(), 'a', false);
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
        let mut chat = View::open(agent_id(), 'a', false);
        let action = handle_chat_key(
            &mut chat,
            &idle_model(),
            press(KeyCode::BackTab),
            VIEWPORT,
            t(0),
        );
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::Claude(
                ClaudeCommand::CyclePermissionMode { agent: agent_id() },
            )))
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
        let mut chat = View::open(agent_id(), 'a', false);
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
        let mut chat = View::open(agent_id(), 'a', false);
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, ctrl('a'), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Char('s')), VIEWPORT, t(0)),
            Some(UiAction::CloseChat),
            "leader chords work over the overlay"
        );
        let mut chat = View::open(agent_id(), 'a', false);
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

    fn drive_scroll(chat: &mut View, feed: &mut FeedViewport, model: &Model, key: KeyEvent) {
        super::handle_chat_key(chat, model, key, VIEWPORT, t(0));
        let Some(intent) = chat.scroll_intent.take() else {
            return;
        };
        let ctx = crate::render::FrameContext {
            viewport: VIEWPORT,
            theme: crate::render::Theme::default(),
            now: t(0),
        };
        let mut cache = PaintCache::default();
        let parts = render::claude_frame_parts(model, chat, feed, &mut cache, &ctx);
        let geometry = parts.geometry(VIEWPORT, true);
        let metrics = feed_metrics(&parts.feed, FrameSpacing::DEFAULT, &geometry);
        apply_scroll(
            feed,
            &metrics,
            intent,
            crate::chat::entry_watermark(model, chat.agent),
        );
    }

    #[test]
    fn pgup_pauses_with_a_watermark_and_pgdn_at_the_bottom_resumes() {
        let model = long_feed_model();
        let mut chat = View::open(agent_id(), 'a', false);
        let mut feed = FeedViewport::following();
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageUp));
        let FeedScroll::Paused {
            entry_watermark, ..
        } = feed.scroll
        else {
            panic!("PgUp pauses following");
        };
        assert_eq!(entry_watermark, 20, "watermark is the entry count at pause");

        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageUp));
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageDown));
        assert!(
            matches!(feed.scroll, FeedScroll::Paused { .. }),
            "mid-feed PgDn stays paused"
        );
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageDown));
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageDown));
        assert_eq!(
            feed.scroll,
            FeedScroll::Following,
            "reaching the bottom resumes following"
        );
    }

    #[test]
    fn pgup_with_a_short_feed_stays_following() {
        let model = idle_model();
        let mut chat = View::open(agent_id(), 'a', false);
        let mut feed = FeedViewport::following();
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageUp));
        assert_eq!(feed.scroll, FeedScroll::Following);
    }

    #[test]
    fn esc_resets_scroll_only_on_an_empty_draft() {
        let model = long_feed_model();
        let mut chat = chat_with_draft("reading notes");
        let mut feed = FeedViewport::following();
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::PageUp));
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::Esc));
        assert!(
            matches!(feed.scroll, FeedScroll::Paused { .. }),
            "a non-empty draft keeps Esc away from the scroll (stage 3 gate)"
        );
        chat.composer.kill_all();
        drive_scroll(&mut chat, &mut feed, &model, press(KeyCode::Esc));
        assert_eq!(feed.scroll, FeedScroll::Following);
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
                    error: amux_ui::OpError::general("input raced the session"),
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
                    error: amux_ui::OpError::general("transport lost"),
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
        let mut chat = View::open(agent_id(), 'a', false);
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
        let mut chat = View::open(agent_id(), 'a', false);
        handle_chat_paste(&mut chat, &model, "secret scratch text");
        assert!(chat.composer.is_empty(), "no composer surface exists");
        assert!(
            chat.ask_ui
                .as_ref()
                .is_none_or(|ui| ui.deny_feedback.is_empty()),
            "no panel field took it either"
        );
    }

    /// The help overlay owns the visible focus. A paste disarms the
    /// chrome guard like any input, but cannot close the overlay or mutate
    /// the composer hidden behind it.
    #[test]
    fn paste_with_help_open_retains_nothing_and_leaves_help_open() {
        let model = idle_model();
        let mut chat = View::open(agent_id(), 'a', false);
        chat.help = true;
        chat.quit_guard.press(t(0));

        handle_chat_paste(&mut chat, &model, "invisible draft");

        assert!(chat.help, "paste does not close the help overlay");
        assert!(chat.composer.is_empty(), "the hidden draft is untouched");
        assert!(!chat.quit_guard.is_armed(), "paste disarms the quit guard");
    }

    /// A pending ask owns the surface even before the first reconcile:
    /// the paste routes through the same sync the keys use and is
    /// dropped at the menu stage, never slipped into the hidden
    /// composer.
    #[test]
    fn paste_with_a_pending_ask_before_reconcile_retains_nothing() {
        let model = edit_ask_model();
        let mut chat = View::open(agent_id(), 'a', false); // deliberately not reconciled
        handle_chat_paste(&mut chat, &model, "stray paste");
        assert!(chat.composer.is_empty(), "menu stage has no text field");
        let ui = chat.ask_ui.as_ref().expect("the paste synced the panel");
        assert!(ui.deny_feedback.is_empty());
    }

    // --- ask panels, reader, read-only (Phase 5) ----------------------------

    use amux_ui::claude::answer::{PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse};

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

    pub(super) fn edit_ask_model() -> Model {
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

    fn open_chat(model: &Model) -> View {
        let mut chat = View::open(agent_id(), 'a', false);
        chat.reconcile(model);
        chat
    }

    fn answer_of(action: Option<UiAction>) -> amux_ui::claude::answer::AskAnswer {
        match action {
            Some(UiAction::Dispatch(Command::Claude(ClaudeCommand::AnswerAsk {
                agent,
                ask,
                answer,
            }))) => {
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
            amux_ui::claude::answer::AskAnswer::Permission(PermissionAnswer::AllowScoped {
                suggestion: 0
            })
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
            crate::chat::claude_shared::ask_ui::AskStage::Menu { cursor: 2 }
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
            amux_ui::claude::answer::AskAnswer::Permission(PermissionAnswer::Deny {
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
            amux_ui::claude::answer::AskAnswer::Permission(PermissionAnswer::Deny {
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
            Some(UiAction::Dispatch(Command::Claude(
                ClaudeCommand::Interrupt { agent: agent_id() },
            )))
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT, t(0)),
            Some(UiAction::Dispatch(Command::Claude(
                ClaudeCommand::Interrupt { agent: agent_id() },
            ))),
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
        let mut chat = View::open(agent_id(), 'a', false);
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
        let mut chat = View::open(agent_id(), 'b', false);
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
            amux_ui::claude::answer::AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![1],
                    other: None
                }]
            })
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
            amux_ui::claude::answer::AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![],
                    other: Some("ochre".to_string())
                }]
            })
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
            amux_ui::claude::answer::AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0, 1],
                    other: None
                }]
            })
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
            amux_ui::claude::answer::AskAnswer::Plan(PlanAnswer::RequestChanges {
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
            amux_ui::claude::answer::AskAnswer::Plan(PlanAnswer::ApproveManual)
        );
    }

    #[test]
    fn retained_ask_reader_gates_actions_but_keeps_navigation_and_close() {
        let plan = (1..=40)
            .map(|line| format!("plan line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![hook_row("ExitPlanMode", json!({"plan": plan}), 0)],
            )],
        );
        let mut chat = open_chat(&model);
        assert!(
            chat.reader.is_some(),
            "the actionable plan opens in the reader"
        );
        assert!(amux_ui::claude::allows_answer(&model, agent_id()));

        // Enter request-changes and seed its focused field while the ask is
        // actionable. Every focus path must freeze this state after the
        // stream gate closes.
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('3')), VIEWPORT, t(0));
        handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0));
        for c in "seed feedback".chars() {
            handle_chat_key(&mut chat, &model, press(KeyCode::Char(c)), VIEWPORT, t(0));
        }
        let before = {
            let ui = chat.ask_ui.as_ref().expect("reader answer state");
            assert_eq!(ui.stage, AskStage::PlanFeedback);
            (
                ui.stage.clone(),
                ui.deny_feedback.text(),
                ui.plan_feedback.text(),
            )
        };

        fold(
            &mut model,
            vec![Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Closed {
                    reason: amux_ui::StreamCloseReason::HostUnreachable,
                },
            }],
        );
        assert!(!amux_ui::claude::allows_answer(&model, agent_id()));

        handle_chat_paste(&mut chat, &model, " pasted mutation");
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT, t(0)),
            None,
            "Ctrl+C must not clear a hidden reader field"
        );
        for code in [KeyCode::Char('x'), KeyCode::Char('3'), KeyCode::Enter] {
            assert_eq!(
                handle_chat_key(&mut chat, &model, press(code), VIEWPORT, t(0)),
                None,
                "a hidden reader action must not dispatch"
            );
            let ui = chat.ask_ui.as_ref().expect("retained answer state");
            assert_eq!(
                (
                    ui.stage.clone(),
                    ui.deny_feedback.text(),
                    ui.plan_feedback.text(),
                ),
                before,
                "cursor, stage, and feedback stay unchanged while answering is gated"
            );
        }

        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT, t(0));
        assert!(
            chat.reader.as_ref().expect("reader remains open").scroll > 0,
            "the retained document remains navigable"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('q')), VIEWPORT, t(0));
        assert!(chat.reader.is_none(), "q still closes the read surface");
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
        // shows 15 rows of the 30-line plan — max top is 15.
        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT, t(0));
        assert_eq!(
            chat.reader.as_ref().expect("reader open").scroll,
            15,
            "End lands exactly on the render clamp"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Up), VIEWPORT, t(0));
        assert_eq!(
            chat.reader.as_ref().expect("reader open").scroll,
            14,
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
            theme: crate::render::Theme::default(),
            now: t(60),
        };
        let viewport = FeedViewport::following();
        let mut cache = PaintCache::default();
        let parts = render::claude_frame_parts(&model, &chat, &viewport, &mut cache, &ctx);
        let frame = compose_chat_frame(parts, &viewport, ctx.theme, ctx.viewport);
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
            matches!(
                ui.stage,
                crate::chat::claude_shared::ask_ui::AskStage::Menu { cursor: 0 }
            ),
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

/// Attachment routing: what a paste and Ctrl+V put in the draft, and what
/// the draft survives. Sibling of `tests` so the check filter names it.
#[cfg(test)]
mod attachments {
    use super::tests::{VIEWPORT, chat_with_draft, ctrl, edit_ask_model, idle_model, press, t};
    use super::*;
    use crate::composer::TokenAttachment;

    fn long_paste(lines: usize) -> String {
        (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn labels(chat: &View) -> Vec<String> {
        chat.composer
            .tokens()
            .iter()
            .map(|token| token.label.clone())
            .collect()
    }

    #[test]
    fn a_long_paste_becomes_one_token_and_a_short_one_is_text() {
        let model = idle_model();
        let mut chat = View::open(agent_id_local(), 'a', false);
        handle_chat_paste(&mut chat, &model, &long_paste(9));
        assert_eq!(labels(&chat), vec!["[Pasted #1 · 9 lines]"]);
        assert_eq!(chat.composer.text().chars().count(), 1, "one slot char");

        handle_chat_paste(&mut chat, &model, "one\ntwo\nthree");
        assert_eq!(labels(&chat), vec!["[Pasted #1 · 9 lines]"], "no new token");
        assert!(
            chat.composer.text().ends_with("one\ntwo\nthree"),
            "short text lands as characters"
        );
    }

    /// The char threshold catches a single enormous line, which the line
    /// count alone would let through.
    #[test]
    fn a_one_line_paste_over_the_char_threshold_becomes_a_token() {
        let model = idle_model();
        let mut chat = View::open(agent_id_local(), 'a', false);
        handle_chat_paste(&mut chat, &model, &"x".repeat(1000));
        assert_eq!(labels(&chat), vec!["[Pasted #1 · 1 line]"]);
    }

    #[test]
    fn a_clipboard_image_becomes_an_image_token() {
        let model = idle_model();
        let mut chat = chat_with_draft("what is wrong here");
        attach_clipboard(
            &mut chat,
            &model,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        assert_eq!(labels(&chat), vec!["[Image #1]"]);
        assert_eq!(chat.send_failure(), None);
        let (text, attachments) = chat.composer.export(None);
        assert!(
            text.starts_with("what is wrong here<amux-attachment "),
            "{text}"
        );
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime, "image/png");
    }

    #[test]
    fn a_clipboard_file_path_becomes_a_file_token() {
        let model = idle_model();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, b"notes").unwrap();
        let mut chat = View::open(agent_id_local(), 'a', false);
        attach_clipboard(&mut chat, &model, ClipboardContent::Path(file));
        assert_eq!(labels(&chat), vec!["[File #1 notes.md]"]);
    }

    /// A file that vanished between copy and paste states why instead of
    /// attaching a token with nothing behind it.
    #[test]
    fn an_unreadable_clipboard_path_states_the_refusal() {
        let model = idle_model();
        let mut chat = View::open(agent_id_local(), 'a', false);
        attach_clipboard(
            &mut chat,
            &model,
            ClipboardContent::Path("/no/such/file.md".into()),
        );
        assert!(chat.composer.tokens().is_empty());
        assert!(
            chat.send_failure()
                .is_some_and(|stated| stated.starts_with("file.md could not be read")),
            "{:?}",
            chat.send_failure()
        );
    }

    /// Ctrl+V on plain text is a paste, so its size decides the same way.
    #[test]
    fn clipboard_text_follows_the_paste_rules() {
        let model = idle_model();
        let mut chat = View::open(agent_id_local(), 'a', false);
        attach_clipboard(&mut chat, &model, ClipboardContent::Text("short".into()));
        assert_eq!(chat.composer.text(), "short");
        attach_clipboard(&mut chat, &model, ClipboardContent::Text(long_paste(12)));
        assert_eq!(labels(&chat), vec!["[Pasted #1 · 12 lines]"]);
    }

    /// An ask panel owns the surface: an attachment has no home in a
    /// one-line answer field, and must not land in the hidden draft.
    #[test]
    fn an_ask_takeover_and_return_keep_the_text_and_both_tokens() {
        let idle = idle_model();
        let mut chat = chat_with_draft("look at ");
        attach_clipboard(
            &mut chat,
            &idle,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        handle_chat_paste(&mut chat, &idle, &long_paste(9));
        let before = chat.composer.export(None);
        assert_eq!(labels(&chat), vec!["[Image #1]", "[Pasted #1 · 9 lines]"]);

        // The ask takes over.
        let asking = edit_ask_model();
        chat.sync_ask(&asking);
        assert!(chat.ask_ui.is_some(), "the panel owns the surface");
        attach_clipboard(
            &mut chat,
            &asking,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"other".to_vec(),
            },
        );
        handle_chat_key(&mut chat, &asking, press(KeyCode::Down), VIEWPORT, t(0));

        // …and hands it back.
        chat.sync_ask(&idle);
        assert!(chat.ask_ui.is_none(), "the ask resolved");
        assert_eq!(
            chat.composer.export(None),
            before,
            "the draft came back whole: text and both tokens"
        );
    }

    /// Scrolling and a phase change are view state; the draft is ViewState
    /// too and neither touches it (D1).
    #[test]
    fn scrolling_and_a_phase_change_leave_the_draft_alone() {
        let idle = idle_model();
        let mut chat = chat_with_draft("see ");
        attach_clipboard(
            &mut chat,
            &idle,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        let before = chat.composer.export(None);

        handle_chat_key(&mut chat, &idle, press(KeyCode::PageUp), VIEWPORT, t(0));
        let working = super::tests::working_model();
        handle_chat_key(&mut chat, &working, ctrl('e'), VIEWPORT, t(0));
        chat.reconcile(&working);
        assert_eq!(chat.composer.export(None), before);
    }

    /// Enter while the gate refuses keeps the whole draft — the tokens
    /// most of all: re-attaching a screenshot is not a keystroke away.
    #[test]
    fn a_gated_enter_leaves_the_tokens_untouched() {
        let working = super::tests::working_model();
        let mut chat = chat_with_draft("look at ");
        attach_clipboard(
            &mut chat,
            &working,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        let before = chat.composer.export(None);
        assert!(
            handle_chat_key(&mut chat, &working, press(KeyCode::Enter), VIEWPORT, t(0)).is_none(),
            "the gate refuses"
        );
        assert_eq!(chat.composer.export(None), before);
        assert!(matches!(
            chat.composer.tokens()[0].attachment,
            TokenAttachment::Artifact(_)
        ));
    }

    /// A send that carries attachments dispatches the attachment command
    /// with the artifacts in draft order, not the plain prompt.
    #[test]
    fn enter_sends_the_exported_draft_with_its_attachments() {
        let model = idle_model();
        let mut chat = chat_with_draft("what is wrong here ");
        attach_clipboard(
            &mut chat,
            &model,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        let Some(UiAction::Dispatch(Command::SendPromptWithAttachments {
            text, attachments, ..
        })) = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("a draft with a token sends as an attachment prompt");
        };
        assert!(
            text.starts_with("what is wrong here <amux-attachment "),
            "{text}"
        );
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name, "clipboard.png");
        assert!(chat.composer.is_empty(), "the draft cleared for the send");
    }

    /// A draft with nothing attached still sends as the plain prompt: the
    /// attachment command exists for drafts that carry one.
    #[test]
    fn a_plain_draft_still_sends_as_a_plain_prompt() {
        let model = idle_model();
        let mut chat = chat_with_draft("no attachments here");
        assert!(matches!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0)),
            Some(UiAction::Dispatch(Command::Claude(
                amux_ui::ClaudeCommand::SendPrompt { .. }
            )))
        ));
    }

    /// A typed failure names the attachment and puts the draft back as it
    /// was — canonical elements are not a draft, so the token returns as a
    /// token, ready to send again.
    #[test]
    fn failed_send_restores_the_draft_with_its_token_and_states_the_failure() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("look at ");
        attach_clipboard(
            &mut chat,
            &model,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: b"png bytes".to_vec(),
            },
        );
        let before = chat.composer.export(None);
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT, t(0))
        else {
            panic!("enter dispatches");
        };
        let Command::SendPromptWithAttachments { attachments, .. } = &command else {
            panic!("an attachment send");
        };
        let missing = attachments[0].id.clone();
        let op = amux_ui::OpId(uuid::Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        super::tests::fold(&mut model, vec![amux_ui::Msg::Command { op, command }]);
        super::tests::fold(
            &mut model,
            vec![amux_ui::Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::Error {
                    error: amux_ui::OpError::AttachmentMissing {
                        id: missing,
                        name: "clipboard.png".into(),
                    },
                },
            }],
        );
        chat.reconcile(&model);

        assert_eq!(
            chat.composer.export(None),
            before,
            "text and token came back exactly as sent"
        );
        assert_eq!(labels(&chat), vec!["[Image #1]"]);
        assert!(
            chat.send_failure()
                .is_some_and(|stated| stated.contains("clipboard.png")),
            "the footer names the attachment: {:?}",
            chat.send_failure()
        );
    }

    fn agent_id_local() -> amux_ui::AgentId {
        super::tests::agent_id()
    }
}

/// The review page inside the chat that hosts it: opening over a frozen
/// diff, the token it leaves in the draft, and what leaving, resuming,
/// discarding and sending it mean. Sibling of `tests` so the check filter
/// names it.
#[cfg(test)]
mod review {
    use amux_ui::{Msg, OpId, OpOutcome};
    use uuid::Uuid;

    use super::tests::{VIEWPORT, ctrl, fold, idle_model, press, t, working_model};
    use super::*;
    use crate::review::fixture::sample_diff_response;

    fn agent() -> amux_ui::AgentId {
        super::tests::agent_id()
    }

    fn chat() -> View {
        View::open(agent(), 'a', false)
    }

    fn key(chat: &mut View, model: &Model, key: KeyEvent) -> Option<UiAction> {
        handle_chat_key(chat, model, key, VIEWPORT, t(0))
    }

    fn type_text(chat: &mut View, model: &Model, text: &str) {
        for character in text.chars() {
            key(chat, model, press(KeyCode::Char(character)));
        }
    }

    /// `<leader> r`, and whatever it dispatched.
    fn leader_r(chat: &mut View, model: &Model) -> Option<Command> {
        key(chat, model, ctrl('a'));
        match key(chat, model, press(KeyCode::Char('r'))) {
            Some(UiAction::Dispatch(command)) => Some(command),
            other => {
                assert!(other.is_none(), "the chord dispatches or does nothing");
                None
            }
        }
    }

    /// The daemon's answer to a diff request, folded in and reconciled.
    fn deliver(chat: &mut View, model: &mut Model, command: Command, nth: u128) {
        let op = OpId(Uuid::from_u128(nth));
        chat.note_dispatched(op, &command);
        let base = match &command {
            Command::RequestDiff { base, .. } => base.clone(),
            other => panic!("expected a diff request, got {other:?}"),
        };
        fold(model, vec![Msg::Command { op, command }]);
        fold(
            model,
            vec![Msg::OpResult {
                op,
                outcome: OpOutcome::DiffReady {
                    response: sample_diff_response(base),
                },
            }],
        );
        chat.reconcile(model);
    }

    /// The chord, the frozen diff, and the page on screen.
    fn opened(model: &mut Model) -> View {
        let mut chat = chat();
        let command = leader_r(&mut chat, model).expect("the chord requests a diff");
        deliver(&mut chat, model, command, 1);
        assert!(chat.review_open(), "the page opens over the frozen diff");
        chat
    }

    /// Move to a row, open the comment box, type, and save.
    fn comment(chat: &mut View, model: &Model, text: &str) {
        key(chat, model, press(KeyCode::Char('j')));
        key(chat, model, press(KeyCode::Char('c')));
        type_text(chat, model, text);
        key(chat, model, press(KeyCode::Enter));
    }

    fn labels(chat: &View) -> Vec<String> {
        chat.composer
            .tokens()
            .iter()
            .map(|token| token.label.clone())
            .collect()
    }

    #[test]
    fn the_chord_requests_the_working_tree_diff_and_opens_the_page_over_it() {
        let mut model = idle_model();
        let mut chat = chat();
        let command = leader_r(&mut chat, &model).expect("the chord requests a diff");
        assert_eq!(
            command,
            Command::RequestDiff {
                agent: agent(),
                base: DiffBase::WorkingTree,
            }
        );
        assert!(!chat.review_open(), "nothing opens before the diff arrives");
        deliver(&mut chat, &mut model, command, 1);
        assert!(chat.review_open());
        assert_eq!(
            chat.review
                .as_ref()
                .expect("a review")
                .view
                .review()
                .document()
                .files
                .len(),
            3,
            "the page holds the whole frozen patch"
        );
        assert!(
            chat.composer.is_empty(),
            "an unwritten review leaves no token"
        );
    }

    /// The token appears the moment there is something to send, and its
    /// label counts what is behind it.
    #[test]
    fn the_first_saved_comment_inserts_the_token_at_the_cursor() {
        let mut model = idle_model();
        let mut chat = chat();
        chat.composer.insert_str("look at ");
        let command = leader_r(&mut chat, &model).expect("the chord requests a diff");
        deliver(&mut chat, &mut model, command, 1);
        comment(&mut chat, &model, "say why");
        assert_eq!(labels(&chat), vec!["[Review · 1 comment]"]);
        assert_eq!(
            chat.composer.text().chars().count(),
            "look at ".len() + 1,
            "the token is one char, at the cursor"
        );

        key(&mut chat, &model, press(KeyCode::Char('j')));
        key(&mut chat, &model, press(KeyCode::Char('c')));
        type_text(&mut chat, &model, "and here");
        key(&mut chat, &model, press(KeyCode::Enter));
        assert_eq!(
            labels(&chat),
            vec!["[Review · 2 comments]"],
            "the label counts the comments behind it"
        );
    }

    /// `q` puts the person back in the draft with the cursor past the
    /// token, so the next Enter sends rather than reopening the page.
    #[test]
    fn q_returns_to_the_draft_with_the_cursor_after_the_token() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        key(&mut chat, &model, press(KeyCode::Char('q')));
        assert!(!chat.review_open(), "the page closes");
        assert!(
            chat.review.is_some(),
            "the review itself stays in the draft"
        );
        assert_eq!(chat.composer.cursor(), 1, "the cursor sits past the token");
        assert_eq!(chat.composer.review_token_at_cursor(), None);
    }

    #[test]
    fn enter_after_the_token_sends_the_review() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        key(&mut chat, &model, press(KeyCode::Char('q')));
        let Some(UiAction::Dispatch(Command::SendPromptWithAttachments {
            text, attachments, ..
        })) = key(&mut chat, &model, press(KeyCode::Enter))
        else {
            panic!("enter past the token sends");
        };
        assert!(
            text.contains("kind=\"review\"") && text.contains("say why"),
            "the review element rides the prompt: {text}"
        );
        let diff = &chat_diff_id();
        assert!(
            text.contains(diff.as_str()),
            "the element cites its diff: {text}"
        );
        assert_eq!(
            attachments
                .iter()
                .map(|attachment| attachment.id.to_string())
                .collect::<Vec<_>>(),
            vec![diff.clone()],
            "the diff is listed to pin, with no bytes to store"
        );
        assert!(attachments[0].bytes.is_none());
    }

    /// The artifact the fixture diff was stored as.
    fn chat_diff_id() -> String {
        crate::review::fixture::sample_diff_response(DiffBase::WorkingTree)
            .artifact
            .id
            .to_string()
    }

    #[test]
    fn enter_on_the_token_resumes_the_page_instead_of_sending() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        key(&mut chat, &model, press(KeyCode::Char('q')));
        chat.composer.left();
        assert!(chat.composer.review_token_at_cursor().is_some());
        assert_eq!(
            key(&mut chat, &model, press(KeyCode::Enter)),
            None,
            "enter on the token sends nothing"
        );
        assert!(chat.review_open(), "it resumes the page");
    }

    #[test]
    fn the_chord_resumes_the_frozen_review_without_asking_again() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        key(&mut chat, &model, press(KeyCode::Char('q')));
        assert_eq!(
            leader_r(&mut chat, &model),
            None,
            "the second chord asks for no new diff"
        );
        assert!(chat.review_open());
        assert_eq!(
            chat.review
                .as_ref()
                .expect("a review")
                .view
                .review()
                .comment_count(),
            1,
            "the comments survive leaving and coming back"
        );
    }

    /// The token is the review's only place in the draft, so deleting it is
    /// how a review is thrown away.
    #[test]
    fn backspacing_the_token_discards_the_review() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        key(&mut chat, &model, press(KeyCode::Char('q')));
        key(&mut chat, &model, press(KeyCode::Backspace));
        assert!(chat.composer.is_empty(), "the token is gone from the draft");
        assert!(chat.review.is_none(), "and so is the review behind it");
        assert!(!chat.review_open());
    }

    /// Reviewing what the agent has written so far is most wanted while it
    /// is still writing: the request is not gated, the page stands over the
    /// diff it froze, and send stays refused until the turn ends.
    #[test]
    fn a_review_opened_while_the_agent_works_is_frozen_and_send_stays_gated() {
        let mut model = working_model();
        let mut chat = chat();
        let command = leader_r(&mut chat, &model).expect("a working agent still yields a diff");
        deliver(&mut chat, &mut model, command, 1);
        assert!(chat.review_open());
        comment(&mut chat, &model, "say why");
        let before = chat
            .review
            .as_ref()
            .expect("a review")
            .view
            .review()
            .document()
            .clone();
        key(&mut chat, &model, press(KeyCode::Char('q')));
        assert_eq!(
            key(&mut chat, &model, press(KeyCode::Enter)),
            None,
            "send is still refused while the agent works"
        );
        assert_eq!(
            labels(&chat),
            vec!["[Review · 1 comment]"],
            "the draft is kept"
        );
        assert_eq!(
            &before,
            chat.review
                .as_ref()
                .expect("a review")
                .view
                .review()
                .document(),
            "the frozen diff never refetched"
        );
    }

    /// `b` asks for the same work against the branch base. The comments
    /// stay with the patch they were anchored into; the token keeps its
    /// place in the draft.
    #[test]
    fn b_re_requests_against_the_branch_base() {
        let mut model = idle_model();
        let mut chat = opened(&mut model);
        comment(&mut chat, &model, "say why");
        let Some(UiAction::Dispatch(command)) = key(&mut chat, &model, press(KeyCode::Char('b')))
        else {
            panic!("b re-requests the diff");
        };
        assert_eq!(
            command,
            Command::RequestDiff {
                agent: agent(),
                base: DiffBase::Branch {
                    base: "main".to_string()
                },
            }
        );
        deliver(&mut chat, &mut model, command, 2);
        assert!(chat.review_open(), "the page reopens over the new base");
        assert_eq!(
            chat.review
                .as_ref()
                .expect("a review")
                .view
                .review()
                .comment_count(),
            0,
            "a comment written on one patch does not follow to another"
        );
        assert!(
            labels(&chat).is_empty(),
            "and the token it put in the draft goes with it"
        );
    }

    /// A diff the repository cannot produce says why in the footer instead
    /// of opening an empty page.
    #[test]
    fn a_refused_diff_states_why_and_opens_nothing() {
        let mut model = idle_model();
        let mut chat = chat();
        let command = leader_r(&mut chat, &model).expect("the chord requests a diff");
        let op = OpId(Uuid::from_u128(1));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: OpOutcome::Error {
                    error: amux_ui::OpError::DiffUnavailable {
                        message: "not a git checkout".to_string(),
                    },
                },
            }],
        );
        chat.reconcile(&model);
        assert!(!chat.review_open());
        assert_eq!(chat.send_failure(), Some("not a git checkout"));
    }

    /// A read-only chat has no draft to put a review in, so it has no
    /// review chord either.
    #[test]
    fn a_read_only_chat_has_no_review_chord() {
        let mut model = idle_model();
        let readonly = readonly_model(&mut model);
        let mut chat = chat();
        assert_eq!(leader_r(&mut chat, &readonly), None);
        assert!(chat.review.is_none());
    }

    fn readonly_model(model: &mut Model) -> Model {
        let mut agent = model.agent(agent()).expect("the agent").agent.clone();
        agent.readonly = true;
        let mut readonly = model.clone();
        fold(
            &mut readonly,
            vec![Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent })],
        );
        readonly
    }
}
