//! One open chat: the client's view state over a session, the keys that
//! act on it, and the frame it draws.
//!
//! Everything here is ephemeral and the client's own: the anchor, the
//! expansion set keyed by item keys, the focused row, the draft, the ask
//! card's picks. Keys turn into [`ChatEffect`]s the event loop carries out
//! against the session; nothing in this module does I/O.

pub mod ask;
pub mod composer;
pub mod feed;
pub mod layout;
pub mod pane;
pub mod review;
pub mod rows;

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{ActivityKind, Composer, Key, PhaseView, SessionState, Waiting};
use ui_view::{
    AskCard, Away, CardState, ChatOptions, FamilyHeader, OutboxState, RowKind, ToolRows, ask_card,
    chat_rows_for, composer, outbox_rows, queue_rows, session_strip,
};
use wire::{Attachment, attachment};

use self::ask::{AskAction, AskUi};
use self::composer::{
    COMPOSER_LINES, TrayRow, activity_line, edge_row, editor_lines, foot_cards, placeholder,
    strip_line,
};
use self::feed::FeedHit;
use self::layout::{Anchor, Frame, Laid, StretchCache, Toggle};
use self::review::{ReviewAction, ReviewPage};
use crate::clipboard::ClipboardContent;
use crate::editor::{Edit, Editor};
use crate::text::{self, push, push_right};
use crate::theme::Theme;
use crate::wheel::Direction;

/// The in-flight pane beside the chat: about a third of the terminal,
/// within these widths.
const PANE_MIN: usize = 36;
const PANE_MAX: usize = 56;
/// The narrowest the chat gets beside the pane; narrower, the pane lies
/// over the feed instead.
const CHAT_MIN: usize = 60;

/// The lines one wheel event scrolls, signed for the feed's arithmetic.
fn wheel_lines(direction: Direction) -> isize {
    crate::wheel::lines(direction) as isize
}

/// How long an empty chat waits before saying it is loading.
pub const LOADING_HINT_MS: i64 = 300;

/// What a key asks the event loop to do with the session.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEffect {
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// The exited composer's one act: the draft is the new incarnation's
    /// first prompt.
    Resume {
        text: String,
        attachments: Vec<Attachment>,
    },
    Answer(wire::Input),
    /// Stop: ends the turn; the agent stays live.
    Interrupt,
    /// Takes a queued prompt back; its words return to the composer.
    Withdraw {
        id: Vec<u8>,
        text: String,
        attachments: Vec<Attachment>,
    },
    SendNow {
        id: Vec<u8>,
    },
    /// Sends an unconfirmed input again under a new id.
    Resend {
        id: Vec<u8>,
    },
    Discard {
        id: Vec<u8>,
    },
    Page(u32),
    /// Ctrl+V: read the clipboard and attach what it holds.
    Paste,
    /// Store these bytes and attach them at the cursor.
    Attach {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    Copy(String),
    /// Hand the terminal to the agent's own interface.
    RawAttach,
    /// Open the working tree's diff on the review page.
    Review,
    /// The review page, at this changed file.
    ReviewAt(String),
    /// Back to home: the header's [Home].
    Home,
    /// A key a click stands for: a hint, the composer's mode. The app
    /// handles it as if pressed, so the leader and its panel work too.
    Press(KeyEvent),
    /// A leader chord picked from the which-key panel.
    Chord(char),
}

/// Where the composer's words were drawn, for a click to place the cursor:
/// the screen cell of the first wrapped line's first column, how many
/// wrapped lines show, how many are scrolled off above, and the width the
/// draft wraps at.
#[derive(Clone, Copy, Debug, Default)]
struct ComposerSpot {
    x: u16,
    y: u16,
    rows: u16,
    skip: usize,
    wrap: usize,
}

/// A line of the which-key panel: the key, what it does, and the chord's
/// letter.
pub type PanelEntry = (String, String, char);

/// A control in the chat's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderControl {
    Diff,
    Home,
}

/// Where each header control was drawn: its columns, and what it does.
type HeaderSpots = Vec<((usize, usize), HeaderControl)>;

/// A full-screen reader over a diff, a plan or arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Reader {
    pub title: String,
    pub text: String,
    pub scroll: usize,
}

/// The client's state for one open chat.
#[derive(Debug)]
pub struct ChatView {
    pub agent_id: Vec<u8>,
    pub anchor: Anchor,
    pub expanded: HashSet<Key>,
    pub focus: Option<Key>,
    pub editor: Editor,
    pub ask: AskUi,
    /// The selected tray row while the tray has the keys.
    pub tray: Option<usize>,
    pub reader: Option<Reader>,
    /// The review page, kept behind its draft token while the chat shows.
    pub review: Option<ReviewPage>,
    pub review_open: bool,
    /// Whether the agent's own interface can be attached from here.
    pub attach: bool,
    /// Why the agent's host is away when it is, kept current by the app
    /// from the fleet.
    pub away: Away,
    /// The leader key, for the keys the chat names.
    pub leader: char,
    /// Stretches of steps the reader opened, by their oldest step.
    pub stretches: HashSet<Key>,
    /// Draw the old chat regardless of the design variant: the tests that
    /// still describe it set this, since the variant is process-wide.
    pub legacy: bool,
    /// Whether the agent runs on this machine; the header names its host
    /// only when it does not.
    pub local: bool,
    /// The working tree's lines added and removed, as last read: after the
    /// chat opens and after each turn ends.
    pub diff_stat: Option<(u32, u32)>,
    /// Whether the agent was working when the diff stat was last asked
    /// for; `None` until the first ask.
    stat_seen: Option<bool>,
    /// The header's controls on the last frame: row, columns, control.
    header_spots: Vec<(u16, (u16, u16), HeaderControl)>,
    /// The header control under the mouse.
    hover: Option<HeaderControl>,
    /// The which-key panel's entries while it shows; the app sets them
    /// before each frame.
    pub panel: Option<Vec<PanelEntry>>,
    /// Clickable places drawn on the last frame, as (row, columns, what):
    /// hints and panel lines stand for keys.
    hint_spots: Vec<(u16, (u16, u16), KeyEvent)>,
    panel_spots: Vec<(u16, (u16, u16), char)>,
    /// The pinned prompt's rows and its key.
    pin_spot: Option<((u16, u16), Key)>,
    /// The overview pane: the row above the composer, unfolded. Whether it
    /// shows is the client's layout, kept across chats by the app.
    pub pane_open: bool,
    /// Whether the pane has the keys rather than the composer.
    pub pane_keys: bool,
    /// The item the pane's keys are on, indexing [`pane::items`].
    pane_focus: Option<usize>,
    /// The item under the mouse; hover and focus are one highlight.
    pane_hover: Option<pane::PaneItem>,
    /// The working tree's changed files, as last read with the diff stat.
    pub diff_files: Option<Vec<pane::FileLine>>,
    /// Where the pane was drawn: a click inside gives it the keys.
    pane_rect: Option<Rect>,
    /// A step was just scrolled to from the pane: if it sits within the
    /// last screenful, the next frame follows the newest row instead.
    revealed: bool,
    pane_spots: Vec<(u16, (u16, u16), pane::PaneHit)>,
    /// The row above the composer, which opens the pane.
    row_spot: Option<(u16, (u16, u16))>,
    /// The jump-to-bottom control.
    jump_spot: Option<(u16, (u16, u16))>,
    /// The mode on the composer's edge.
    mode_spot: Option<(u16, (u16, u16))>,
    composer_spot: Option<ComposerSpot>,
    /// Tray rows by screen row.
    tray_spots: Vec<(u16, usize)>,
    stretch_cache: StretchCache,
    /// Where the feed's first line was drawn, for clicks.
    feed_origin: (u16, u16),
    epoch: u64,
    opened_at_ms: i64,
    /// The last frame's layout, for scrolling and focus.
    laid: Laid,
    feed: (usize, usize),
    /// Whether the session was last told the reader follows the newest row.
    told_following: bool,
    page_asked: Option<u64>,
}

impl ChatView {
    pub fn new(agent_id: Vec<u8>, now_ms: i64, attach: bool) -> ChatView {
        ChatView {
            agent_id,
            anchor: Anchor::Bottom,
            expanded: HashSet::new(),
            focus: None,
            editor: Editor::default(),
            ask: AskUi::default(),
            tray: None,
            reader: None,
            review: None,
            review_open: false,
            attach,
            away: Away::Plain,
            leader: 'a',
            stretches: HashSet::new(),
            legacy: false,
            local: true,
            diff_stat: None,
            stat_seen: None,
            header_spots: Vec::new(),
            hover: None,
            panel: None,
            hint_spots: Vec::new(),
            panel_spots: Vec::new(),
            pin_spot: None,
            pane_open: false,
            pane_keys: false,
            pane_focus: None,
            pane_hover: None,
            diff_files: None,
            pane_rect: None,
            revealed: false,
            pane_spots: Vec::new(),
            row_spot: None,
            jump_spot: None,
            mode_spot: None,
            composer_spot: None,
            tray_spots: Vec::new(),
            stretch_cache: StretchCache::default(),
            feed_origin: (0, 0),
            epoch: 0,
            opened_at_ms: now_ms,
            laid: Laid::default(),
            feed: (0, 0),
            told_following: true,
            page_asked: None,
        }
    }

    fn frame<'a>(&'a self, theme: Theme) -> Frame<'a> {
        Frame {
            anchor: &self.anchor,
            expanded: &self.expanded,
            focus: self.focus.as_ref(),
            width: self.feed.0,
            height: self.feed.1,
            theme,
            leader: self.leader,
            redesigned: self.redesigned(),
            stretches: &self.stretches,
            cache: &self.stretch_cache,
        }
    }

    /// Whether the chat draws as redesigned.
    pub fn redesigned(&self) -> bool {
        !self.legacy && crate::variant::get() == 0
    }

    /// The card to draw, synced with this client's picks. With no ask
    /// open, the last one's picks and fields go.
    fn card(&mut self, state: &SessionState) -> Option<AskCard> {
        let Some(card) = ask_card(state) else {
            self.ask = AskUi::default();
            return None;
        };
        self.ask.sync(&card);
        Some(card)
    }

    fn tray_rows(state: &SessionState) -> Vec<TrayRow> {
        let mut rows: Vec<TrayRow> = queue_rows(state).into_iter().map(TrayRow::Queued).collect();
        rows.extend(outbox_rows(state).into_iter().map(TrayRow::Outbox));
        rows
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self, state: &SessionState) -> bool {
        if self.review_open {
            return self.review.as_ref().is_some_and(ReviewPage::editing);
        }
        if self.redesigned() && self.pane_open && self.pane_keys {
            return false;
        }
        if let Some(card) = self.card_takes_keys(state) {
            return self.ask.field_text(&card);
        }
        self.tray.is_none() && !self.editor.is_empty()
    }

    /// Whether `key` opens the key help: '?' while the empty composer has
    /// the keys. Every other field that is open, a review comment or an
    /// answer among them, types it.
    pub fn opens_help(&self, state: &SessionState, key: KeyEvent) -> bool {
        key.code == KeyCode::Char('?')
            && !self.review_open
            && self.reader.is_none()
            && ask_card(state).is_none()
            && self.tray.is_none()
            && self.editor.is_empty()
    }

    /// Ctrl+C on a field with text: clears it as a kill. Only the field
    /// with the keys: a draft hidden behind a card stays.
    pub fn kill_field(&mut self, state: &SessionState) -> bool {
        if self.review_open {
            return self.review.as_mut().is_some_and(ReviewPage::kill_field);
        }
        if let Some(card) = self.card_takes_keys(state) {
            return self.ask.editing_on(&card) && self.ask.kill_field();
        }
        self.tray.is_none() && self.editor.kill_all()
    }

    /// A bracketed paste goes to the field with the keys, and nowhere when
    /// none has them.
    pub fn paste_text(&mut self, state: &SessionState, text: &str) {
        if self.review_open {
            if let Some(page) = &mut self.review {
                page.paste(text);
            }
            return;
        }
        if self.reader.is_some() {
            return;
        }
        if let Some(card) = self.card_takes_keys(state) {
            self.ask.sync(&card);
            self.ask.paste(&card, text);
            return;
        }
        if self.tray.is_none() {
            self.editor.paste(text);
            self.focus = None;
        }
    }

    fn card_takes_keys(&self, state: &SessionState) -> Option<AskCard> {
        let card = ask_card(state)?;
        (card.state != CardState::Dismissed).then_some(card)
    }

    /// One key. Ctrl+C and the leader are the app's and never reach here.
    pub fn key(&mut self, state: &SessionState, key: KeyEvent, theme: Theme) -> Vec<ChatEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.review_open
            && let Some(page) = &mut self.review
        {
            match page.key(key) {
                ReviewAction::None => {}
                ReviewAction::Close => self.review_open = false,
                ReviewAction::Comments => self.sync_review_token(),
            }
            return vec![];
        }
        if let Some(reader) = &mut self.reader {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.reader = None,
                KeyCode::Up | KeyCode::Char('k') => reader.scroll = reader.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => reader.scroll += 1,
                KeyCode::PageUp => reader.scroll = reader.scroll.saturating_sub(self.feed.1.max(1)),
                KeyCode::PageDown | KeyCode::Char(' ') => reader.scroll += self.feed.1.max(1),
                KeyCode::Home | KeyCode::Char('g') => reader.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => reader.scroll = usize::MAX / 2,
                _ => {}
            }
            return vec![];
        }
        if self.redesigned() && key.code == KeyCode::Char('t') && ctrl {
            self.pane_toggle_key();
            return vec![];
        }
        match key.code {
            KeyCode::Char('x') if ctrl => {
                return if state.phase() == PhaseView::Working || !state.open_asks().is_empty() {
                    vec![ChatEffect::Interrupt]
                } else {
                    vec![]
                };
            }
            KeyCode::PageUp => {
                self.scroll(
                    state,
                    -(self.feed.1.saturating_sub(2).max(1) as isize),
                    theme,
                );
                return vec![];
            }
            KeyCode::PageDown => {
                self.scroll(state, self.feed.1.saturating_sub(2).max(1) as isize, theme);
                return vec![];
            }
            KeyCode::End if ctrl => {
                self.follow();
                return vec![];
            }
            KeyCode::Home if ctrl => {
                if let Some(oldest) = state.transcript().iter().next() {
                    self.anchor = Anchor::Top {
                        key: oldest.item.key.clone(),
                        offset: 0,
                    };
                }
                return vec![];
            }
            _ => {}
        }
        // The pane has the keys: it takes the ones it uses and ignores the
        // rest; Esc hands the keys back to the composer.
        if self.redesigned() && self.pane_open && self.pane_keys {
            return self.pane_key(state, key);
        }
        if let Some(card) = self.card_takes_keys(state) {
            self.ask.sync(&card);
            if key.code == KeyCode::Esc && !self.ask.takes_escape(&card) {
                self.escape();
                return vec![];
            }
            return match self.ask.key(&card, key, self.attach) {
                AskAction::None => vec![],
                AskAction::Attach => vec![ChatEffect::RawAttach],
                AskAction::Answer(input) => vec![ChatEffect::Answer(*input)],
                AskAction::Interrupt => vec![ChatEffect::Interrupt],
                AskAction::Resend => state
                    .answering(&card.key)
                    .map(|sent| {
                        vec![ChatEffect::Resend {
                            id: sent.id.clone(),
                        }]
                    })
                    .unwrap_or_default(),
                AskAction::Discard => state
                    .answering(&card.key)
                    .map(|sent| {
                        vec![ChatEffect::Discard {
                            id: sent.id.clone(),
                        }]
                    })
                    .unwrap_or_default(),
                AskAction::Read { title, text } => {
                    self.reader = Some(Reader {
                        title,
                        text,
                        scroll: 0,
                    });
                    vec![]
                }
            };
        }
        if let Some(selected) = self.tray {
            return self.tray_key(state, selected, key);
        }
        match key.code {
            KeyCode::Esc => {
                self.escape();
                vec![]
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => self.submit(state),
            KeyCode::Char('v') if ctrl => vec![ChatEffect::Paste],
            KeyCode::BackTab if state.composer() == Composer::Send => next_mode(state)
                .and_then(|change| ui_view::setting_input(state.kind(), &change))
                .map(ChatEffect::Answer)
                .into_iter()
                .collect(),
            KeyCode::Up if self.editor.on_first_line() && !Self::tray_rows(state).is_empty() => {
                self.tray = Some(Self::tray_rows(state).len() - 1);
                vec![]
            }
            _ => {
                if self.editor.key(key) == Edit::Changed {
                    self.focus = None;
                }
                vec![]
            }
        }
    }

    fn tray_key(
        &mut self,
        state: &SessionState,
        selected: usize,
        key: KeyEvent,
    ) -> Vec<ChatEffect> {
        let rows = Self::tray_rows(state);
        let Some(row) = rows.get(selected.min(rows.len().saturating_sub(1))) else {
            self.tray = None;
            return vec![];
        };
        match key.code {
            KeyCode::Esc => self.tray = None,
            KeyCode::Up => self.tray = Some(selected.saturating_sub(1)),
            KeyCode::Down if selected + 1 >= rows.len() => self.tray = None,
            KeyCode::Down => self.tray = Some(selected + 1),
            _ => {
                let effect = match (row, key.code) {
                    (TrayRow::Queued(queued), KeyCode::Enter | KeyCode::Char('s'))
                        if queued.can_send_now =>
                    {
                        Some(ChatEffect::SendNow {
                            id: queued.input_id.clone(),
                        })
                    }
                    (
                        TrayRow::Queued(queued),
                        KeyCode::Char('w') | KeyCode::Delete | KeyCode::Backspace,
                    ) if queued.can_withdraw => {
                        let entry = state
                            .queue()
                            .into_iter()
                            .find(|row| row.entry.input_id == queued.input_id)
                            .map(|row| (row.entry.text.clone(), row.entry.attachments.clone()));
                        entry.map(|(text, attachments)| ChatEffect::Withdraw {
                            id: queued.input_id.clone(),
                            text,
                            attachments,
                        })
                    }
                    (TrayRow::Outbox(out), KeyCode::Char('r'))
                        if out.state == OutboxState::NotConfirmed =>
                    {
                        Some(ChatEffect::Resend {
                            id: out.input_id.clone(),
                        })
                    }
                    (TrayRow::Outbox(out), KeyCode::Char('d'))
                        if out.state != OutboxState::Sending =>
                    {
                        Some(ChatEffect::Discard {
                            id: out.input_id.clone(),
                        })
                    }
                    (TrayRow::Outbox(out), KeyCode::Char('e'))
                        if matches!(out.state, OutboxState::Rejected(_)) =>
                    {
                        if let Some(sent) = state.inputs().get(&out.input_id)
                            && let ui_state::InputWhat::Prompt { text, attachments } = &sent.what
                        {
                            self.editor.restore(text, attachments.clone());
                        }
                        Some(ChatEffect::Discard {
                            id: out.input_id.clone(),
                        })
                    }
                    _ => None,
                };
                if let Some(effect) = effect {
                    if rows.len() <= 1 {
                        self.tray = None;
                    }
                    return vec![effect];
                }
            }
        }
        vec![]
    }

    /// Enter in the composer: send when caught up and live, resume an
    /// exited agent with the draft, and otherwise keep the draft.
    fn submit(&mut self, state: &SessionState) -> Vec<ChatEffect> {
        if self.editor.is_empty() {
            return vec![];
        }
        match state.composer() {
            Composer::Send if state.can_send() => {
                let (text, attachments) = self.editor.take();
                self.follow();
                vec![ChatEffect::Prompt { text, attachments }]
            }
            Composer::Resume => {
                let (text, attachments) = self.editor.take();
                self.follow();
                vec![ChatEffect::Resume { text, attachments }]
            }
            _ => vec![],
        }
    }

    /// Esc, view-only: close the reader, clear focus, then follow the
    /// newest row. It never answers and never interrupts.
    fn escape(&mut self) {
        if self.reader.take().is_some() {
            return;
        }
        if self.focus.take().is_some() {
            return;
        }
        self.follow();
    }

    pub fn follow(&mut self) {
        self.anchor = Anchor::Bottom;
    }

    /// Ctrl+T: closed, the pane opens with the keys; open with the
    /// composer holding the keys, the pane takes them; holding them, it
    /// closes.
    pub fn pane_toggle_key(&mut self) {
        if !self.pane_open {
            self.open_pane();
        } else if !self.pane_keys {
            self.focus_pane();
        } else {
            self.close_pane();
        }
    }

    /// Opens the pane with the keys.
    pub fn open_pane(&mut self) {
        self.pane_open = true;
        self.focus_pane();
    }

    pub fn close_pane(&mut self) {
        self.pane_open = false;
        self.pane_keys = false;
        self.pane_hover = None;
    }

    /// Gives the pane the keys, on its first item when none has them yet.
    fn focus_pane(&mut self) {
        self.pane_keys = true;
        self.pane_focus.get_or_insert(0);
    }

    /// A key while the pane has the keys: j/k and the arrows move across
    /// its jobs and changed files as one list; Enter opens a job's step in
    /// the chat or a file on the review page; Esc hands the keys back to
    /// the composer, the pane staying open. It ignores the rest.
    fn pane_key(&mut self, state: &SessionState, key: KeyEvent) -> Vec<ChatEffect> {
        let jobs = ui_view::background_jobs(state);
        let items = pane::items(&jobs, self.diff_files.as_deref());
        let last = items.len().checked_sub(1);
        match key.code {
            KeyCode::Esc => self.pane_keys = false,
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                if let Some(last) = last {
                    self.pane_focus = Some(self.pane_focus.map_or(0, |at| (at + 1).min(last)));
                }
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                if last.is_some() {
                    self.pane_focus = Some(self.pane_focus.map_or(0, |at| at.saturating_sub(1)));
                }
            }
            KeyCode::Enter => {
                if let Some(item) = self.pane_focus.and_then(|at| items.get(at)).cloned() {
                    return self.open_item(state, item);
                }
            }
            _ => {}
        }
        vec![]
    }

    /// A job opens its step in the chat; a file, the review page at it.
    fn open_item(&mut self, state: &SessionState, item: pane::PaneItem) -> Vec<ChatEffect> {
        match item {
            pane::PaneItem::Job(key) => {
                self.reveal_step(state, &key);
                vec![]
            }
            pane::PaneItem::File(path) => vec![ChatEffect::ReviewAt(path)],
        }
    }

    /// Scrolls the chat to a step and opens it, and the stretch it sits in.
    fn reveal_step(&mut self, state: &SessionState, key: &Key) {
        let Some(order) = state.transcript().get(key).map(|held| held.item.order) else {
            return;
        };
        if let Some(stretch) = ui_view::stretch_at(state, order) {
            self.stretches.insert(stretch.oldest);
        }
        self.expanded.insert(key.clone());
        self.anchor = Anchor::Top {
            key: key.clone(),
            offset: 0,
        };
        self.revealed = true;
    }

    /// Whether the reader follows the newest row, when that changed since
    /// the session was last told: the one thing about the view the session
    /// needs, to trim its window while following and hold arrivals while
    /// the reader is in history.
    pub fn following_moved(&mut self) -> Option<bool> {
        let following = self.anchor == Anchor::Bottom;
        (following != self.told_following).then(|| {
            self.told_following = following;
            following
        })
    }

    fn scroll(&mut self, state: &SessionState, delta: isize, theme: Theme) {
        self.anchor = self.frame(theme).scrolled(state, &self.laid, delta);
    }

    pub fn mouse(
        &mut self,
        state: &SessionState,
        event: MouseEvent,
        theme: Theme,
    ) -> Vec<ChatEffect> {
        if self.review_open
            && let Some(page) = &mut self.review
        {
            match event.kind {
                MouseEventKind::ScrollUp => page.scroll_by(-wheel_lines(Direction::Up)),
                MouseEventKind::ScrollDown => page.scroll_by(wheel_lines(Direction::Down)),
                _ => {}
            }
            return vec![];
        }
        let control = self
            .header_spots
            .iter()
            .find(|(y, (from, to), _)| *y == event.row && (*from..*to).contains(&event.column))
            .map(|(_, _, control)| *control);
        match event.kind {
            MouseEventKind::Moved => {
                self.hover = control;
                self.pane_hover = self
                    .pane_spots
                    .iter()
                    .find(|(row, (from, to), _)| {
                        *row == event.row && (*from..*to).contains(&event.column)
                    })
                    .and_then(|(_, _, hit)| match hit {
                        pane::PaneHit::Item(item) => Some(item.clone()),
                        pane::PaneHit::Close => None,
                    });
            }
            MouseEventKind::ScrollUp => self.scroll(state, -wheel_lines(Direction::Up), theme),
            MouseEventKind::ScrollDown => self.scroll(state, wheel_lines(Direction::Down), theme),
            MouseEventKind::Down(MouseButton::Left) if self.reader.is_none() => {
                match control {
                    Some(HeaderControl::Diff) => return vec![ChatEffect::Review],
                    Some(HeaderControl::Home) => return vec![ChatEffect::Home],
                    None => {}
                }
                let (x, y) = (event.column, event.row);
                let on = |row: u16, (from, to): (u16, u16)| row == y && (from..to).contains(&x);
                if let Some((_, _, hit)) = self
                    .pane_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                    .cloned()
                {
                    match hit {
                        pane::PaneHit::Close => self.close_pane(),
                        pane::PaneHit::Item(item) => {
                            self.pane_keys = true;
                            let jobs = ui_view::background_jobs(state);
                            self.pane_focus = pane::items(&jobs, self.diff_files.as_deref())
                                .iter()
                                .position(|each| *each == item);
                            return self.open_item(state, item);
                        }
                    }
                    return vec![];
                }
                // A click anywhere else in the pane gives it the keys.
                if self
                    .pane_rect
                    .is_some_and(|rect| rect.contains(Position::new(x, y)))
                {
                    self.focus_pane();
                    return vec![];
                }
                if self.row_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.open_pane();
                    return vec![];
                }
                if let Some((_, _, chord)) = self
                    .panel_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Chord(*chord)];
                }
                if let Some((_, _, key)) = self
                    .hint_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Press(*key)];
                }
                if self.jump_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.follow();
                    return vec![];
                }
                if let Some(((from, to), key)) = &self.pin_spot
                    && (*from..*to).contains(&y)
                {
                    self.anchor = Anchor::Top {
                        key: key.clone(),
                        offset: 0,
                    };
                    return vec![];
                }
                if self.mode_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    return vec![ChatEffect::Press(KeyEvent::new(
                        KeyCode::BackTab,
                        KeyModifiers::SHIFT,
                    ))];
                }
                if let Some((_, i)) = self.tray_spots.iter().find(|(row, _)| *row == y) {
                    self.tray = Some(*i);
                    return vec![];
                }
                if let Some(spot) = self.composer_spot
                    && self.tray.is_none()
                    && (spot.y..spot.y + spot.rows).contains(&y)
                    && x >= spot.x.saturating_sub(2)
                {
                    self.pane_keys = false;
                    let row = usize::from(y - spot.y) + spot.skip;
                    let col = usize::from(x.saturating_sub(spot.x));
                    let at = composer::cursor_at(&self.editor, spot.wrap, row, col);
                    self.editor.set_cursor_chars(at);
                    return vec![];
                }
                if let Some(hit) = self.hit_at(event.column, event.row) {
                    return self.feed_hit(state, hit);
                }
            }
            _ => {}
        }
        vec![]
    }

    /// Whether the working tree's totals should be read now: once the chat
    /// is current after opening, and again each time a turn ends, since
    /// that is when the agent's changes land. Never on every frame.
    pub fn wants_diff_stat(&mut self, state: &SessionState) -> bool {
        if !self.redesigned() || !matches!(state.composer(), Composer::Send | Composer::Resume) {
            return false;
        }
        let working = state.phase() == PhaseView::Working;
        let wanted = match self.stat_seen {
            None => true,
            Some(was_working) => was_working && !working,
        };
        self.stat_seen = Some(working);
        wanted
    }

    /// What the feed line under a click does there.
    fn hit_at(&self, column: u16, row: u16) -> Option<FeedHit> {
        let (x, y) = self.feed_origin;
        let line = usize::from(row.checked_sub(y)?);
        let column = usize::from(column.checked_sub(x)?);
        self.laid
            .hits
            .get(line)?
            .iter()
            .find(|(span, _)| span.is_none_or(|(from, to)| (from..to).contains(&column)))
            .map(|(_, hit)| hit.clone())
    }

    fn feed_hit(&mut self, state: &SessionState, hit: FeedHit) -> Vec<ChatEffect> {
        match hit {
            FeedHit::Stretch(oldest) => {
                if !self.stretches.remove(&oldest) {
                    self.stretches.insert(oldest);
                }
            }
            FeedHit::Step(key) => {
                // A run opens from any member and closes as a whole.
                let transcript = state.transcript();
                let run = transcript
                    .get(&key)
                    .and_then(|held| transcript.run_at(held.item.order));
                let members: Vec<Key> = match run {
                    Some(run) => transcript
                        .range(run.oldest..=run.newest)
                        .map(|held| held.item.key.clone())
                        .collect(),
                    None => vec![key.clone()],
                };
                if members.iter().any(|member| self.expanded.contains(member)) {
                    for member in &members {
                        self.expanded.remove(member);
                    }
                } else {
                    self.expanded.insert(key);
                }
            }
        }
        vec![]
    }

    /// `<leader> k` / `<leader> j`: focus the older or newer drawn row and
    /// keep it on screen.
    pub fn move_focus(&mut self, state: &SessionState, older: bool, theme: Theme) {
        let count = self.laid.blocks.len();
        if count == 0 {
            return;
        }
        let at = self
            .focus
            .as_ref()
            .and_then(|key| self.laid.blocks.iter().position(|b| &b.key == key));
        if at == Some(0) && older {
            // The focused row is the top one: bring the row above it in
            // and focus that.
            let above =
                self.frame(theme)
                    .scrolled(state, &self.laid, -(self.laid.top_offset as isize) - 1);
            if let Anchor::Top { key, .. } = &above {
                self.focus = Some(key.clone());
                self.anchor = Anchor::Top {
                    key: key.clone(),
                    offset: 0,
                };
            }
            return;
        }
        let next = match (at, older) {
            (None, _) => count - 1,
            (Some(i), true) => i - 1,
            (Some(i), false) => (i + 1).min(count - 1),
        };
        let key = self.laid.blocks[next].key.clone();
        if next == 0 && self.laid.top_offset > 0 {
            self.anchor = Anchor::Top {
                key: key.clone(),
                offset: 0,
            };
        }
        self.focus = Some(key);
    }

    /// `<leader> o`: expand or collapse the focused row, or its run; in
    /// the redesigned feed, a folded stretch opens, an open stretch's first
    /// step folds it again, and any other step opens its detail.
    pub fn toggle_expanded(&mut self, state: &SessionState) {
        let Some(key) = self.focus.clone() else {
            return;
        };
        if let Some(block) = self.laid.blocks.iter().find(|block| block.key == key) {
            match block.toggle.clone() {
                Toggle::Row => {}
                Toggle::Stretch(oldest) => {
                    self.feed_hit(state, FeedHit::Stretch(oldest));
                    return;
                }
                Toggle::Step(step) => {
                    let header =
                        block
                            .hits
                            .first()
                            .and_then(|hits| hits.first())
                            .and_then(|(_, hit)| match hit {
                                FeedHit::Stretch(oldest) => Some(oldest.clone()),
                                _ => None,
                            });
                    match header {
                        Some(oldest) => self.feed_hit(state, FeedHit::Stretch(oldest)),
                        None => self.feed_hit(state, FeedHit::Step(step)),
                    };
                    return;
                }
            }
        }
        let transcript = state.transcript();
        let run = transcript
            .get(&key)
            .and_then(|held| transcript.run_at(held.item.order));
        match run {
            Some(run) => {
                let members: Vec<Key> = transcript
                    .range(run.oldest..=run.newest)
                    .map(|held| held.item.key.clone())
                    .collect();
                let open = members.iter().any(|member| self.expanded.contains(member));
                if open {
                    for member in &members {
                        self.expanded.remove(member);
                    }
                    self.focus = Some(run.newest_key);
                } else {
                    self.expanded.insert(key);
                }
            }
            None => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
        }
    }

    /// `<leader> y`: the focused row's words, or the newest row's.
    pub fn copy_text(&self) -> Option<String> {
        let block = match &self.focus {
            Some(key) => self.laid.blocks.iter().find(|b| &b.key == key),
            None => self.laid.blocks.last(),
        }?;
        let text: Vec<String> = block
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_start_matches(['▌', '▎'])
                    .trim()
                    .to_owned()
            })
            .collect();
        Some(text.join("\n").trim().to_owned())
    }

    /// What Ctrl+V found: text follows the paste rules, an image or a file
    /// is stored and attached.
    pub fn paste(&mut self, content: ClipboardContent) -> Result<Option<ChatEffect>, String> {
        match content {
            ClipboardContent::Image { mime, bytes } => Ok(Some(ChatEffect::Attach {
                name: "clipboard.png".into(),
                mime,
                bytes,
            })),
            ClipboardContent::Path(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let bytes = std::fs::read(&path)
                    .map_err(|error| format!("{name} could not be read: {error}"))?;
                Ok(Some(ChatEffect::Attach {
                    mime: mime_of(&name).to_owned(),
                    name,
                    bytes,
                }))
            }
            ClipboardContent::Text(text) => {
                self.editor.paste(&text);
                Ok(None)
            }
            ClipboardContent::Empty => Ok(None),
        }
    }

    /// A stored blob, attached at the cursor as an image or a file.
    pub fn attach_blob(&mut self, blob: wire::BlobRef) {
        let of = if blob.mime.starts_with("image/") {
            attachment::Of::Image(blob)
        } else {
            attachment::Of::File(blob)
        };
        self.editor.insert_attachment(Attachment { of: Some(of) });
    }

    /// `<leader> r` with a review already in the draft: back to its page.
    /// False when there is none, and a fresh diff is wanted.
    pub fn resume_review(&mut self) -> bool {
        let held = self
            .review
            .as_ref()
            .is_some_and(|page| self.editor.find_attachment(|a| page.owns(a)).is_some());
        self.review_open = held;
        held
    }

    /// A frozen diff and its patch: the review page opens over the chat.
    pub fn open_review(&mut self, diff: wire::Diff, patch: String) {
        self.open_review_at(diff, patch, None);
    }

    /// The review page, at the file `at` when one is named.
    pub fn open_review_at(&mut self, diff: wire::Diff, patch: String, at: Option<&str>) {
        let mut page = ReviewPage::new(diff, patch);
        if let Some(path) = at {
            page.show_file(path);
        }
        self.review = Some(page);
        self.review_open = true;
    }

    /// The review page already held, turned to the file `at`.
    pub fn review_file(&mut self, at: &str) {
        if let Some(page) = &mut self.review {
            page.show_file(at);
        }
    }

    /// Keeps the draft's Review token in step with the page: the first
    /// comment inserts it at the cursor, later ones update it where it
    /// sits, and the last deletion takes it out.
    fn sync_review_token(&mut self) {
        let Some(page) = &self.review else {
            return;
        };
        let held = self.editor.find_attachment(|a| page.owns(a));
        match (held, page.comments().is_empty()) {
            (Some(index), false) => self.editor.replace_attachment(index, page.attachment()),
            (Some(index), true) => self.editor.remove_attachment(index),
            (None, false) => self.editor.insert_attachment(page.attachment()),
            (None, true) => {}
        }
    }

    /// A page was asked for; another waits until the window grows.
    pub fn page_sent(&mut self, state: &SessionState) {
        self.page_asked = state.oldest_order();
    }

    /// Lays the chat out in `area` and paints it. Returns the page to ask
    /// for, if the reader is within a page of the oldest held row.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        if state.epoch() != self.epoch {
            // A Reset swapped the transcript in: scroll to the newest, the
            // one whole-list reload.
            self.epoch = state.epoch();
            self.follow();
            self.focus = None;
            self.page_asked = None;
        }
        let width = usize::from(area.width);
        if self.review_open
            && let Some(page) = &mut self.review
        {
            page.draw(paint, area, footer, theme);
            return None;
        }
        if let Some(reader) = &mut self.reader {
            draw_reader(paint, area, reader, theme);
            return None;
        }
        if self.redesigned() {
            return self.draw_turns(paint, area, state, family, footer, now_ms, theme);
        }
        let mut top: Vec<Line<'static>> = vec![header(state, self.away, width, theme)];
        if let Some(family) = family {
            top.push(family_line(family, width, theme));
        }
        top.push(Line::default());

        let mut bottom: Vec<Line<'static>> = Vec::new();
        if self.anchor != Anchor::Bottom {
            let words = if state.arrivals_held() {
                "↓ new activity below · pgdn or ctrl+end for the newest"
            } else {
                "↓ scrolled back · pgdn or ctrl+end for the newest"
            };
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, words, theme.muted(), width);
            bottom.push(line);
        }
        let view = composer(state, now_ms);
        if let Some(activity) = &view.activity {
            let running = match &activity.kind {
                ActivityKind::Running { key } => running_subject(state, key),
                _ => None,
            };
            bottom.push(activity_line(activity, running.as_deref(), width, theme));
        }
        let strip = session_strip(state);
        if let Some(line) = strip_line(&strip, width, theme) {
            bottom.push(line);
        }
        bottom.extend(foot_cards(&strip, width, theme));
        let tray = Self::tray_rows(state);
        if let Some(selected) = self.tray {
            if tray.is_empty() {
                self.tray = None;
            } else if selected >= tray.len() {
                self.tray = Some(tray.len() - 1);
            }
        }
        for (i, row) in tray.iter().enumerate() {
            bottom.push(row.line(self.tray == Some(i), width, theme));
        }

        let name = state
            .agent()
            .name
            .clone()
            .unwrap_or_else(|| "the agent".into());
        let host = state
            .host()
            .map(|host| host.name.clone())
            .unwrap_or_else(|| "its host".into());
        let mut cursor = None;
        let card = self.card(state);
        let mut hint = match &footer {
            Some(footer) => footer.clone(),
            None => Line::default(),
        };
        match &card {
            Some(card) => {
                let lines = self.ask.render(card, &name, self.attach, width, theme);
                let cap = (usize::from(area.height) / 2).max(4);
                let start = bottom.len() + 1;
                let skip = lines.lines.len().saturating_sub(cap);
                if let Some((row, col)) = lines.cursor.filter(|(row, _)| *row >= skip) {
                    cursor = Some((start + row - skip, col));
                }
                bottom.push(Line::default());
                bottom.extend(on_panel(
                    lines.lines.into_iter().skip(skip).collect(),
                    width,
                    theme,
                ));
                if footer.is_none() && card.state == CardState::Dismissed {
                    let (lines, at) = editor_lines(
                        &self.editor,
                        &placeholder(&state.composer(), &name, &host, self.away),
                        width,
                        theme,
                    );
                    cursor = Some((bottom.len() + at.0, at.1));
                    bottom.extend(lines);
                    hint = composer_hint(state, &self.editor, self.away, self.leader, width, theme);
                }
            }
            None => {
                bottom.push(Line::default());
                let (mut lines, at) = editor_lines(
                    &self.editor,
                    &placeholder(&state.composer(), &name, &host, self.away),
                    width,
                    theme,
                );
                let skip = (at.0 + 1).saturating_sub(COMPOSER_LINES);
                lines = lines.into_iter().skip(skip).take(COMPOSER_LINES).collect();
                if self.tray.is_none() {
                    cursor = Some((bottom.len() + at.0 - skip, at.1));
                }
                bottom.extend(lines);
                if footer.is_none() {
                    hint = match self.tray.and_then(|i| tray.get(i)) {
                        Some(row) => {
                            let mut line = Line::from(Span::raw("  "));
                            push(&mut line, row.hint(), theme.muted(), width);
                            line
                        }
                        None => {
                            composer_hint(state, &self.editor, self.away, self.leader, width, theme)
                        }
                    };
                }
            }
        }
        bottom.push(hint);

        let height = usize::from(area.height);
        let feed_height = height.saturating_sub(top.len() + bottom.len());
        self.feed = (width, feed_height);
        let mut lines = top;
        let top_len = lines.len();
        if state.transcript().is_empty() {
            lines.extend(empty_feed(
                state,
                feed_height,
                now_ms - self.opened_at_ms,
                width,
                theme,
            ));
            self.laid = Laid::default();
        } else {
            let laid = self.frame(theme).layout(state);
            lines.extend(laid.lines.iter().cloned());
            self.laid = laid;
        }
        let bottom_start = lines.len();
        lines.extend(bottom);
        paint.render_widget(Paragraph::new(lines), area);
        if let Some((row, col)) = cursor {
            let y = area.y as usize + bottom_start + row;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            if y < (area.y + area.height) as usize {
                paint.set_cursor_position(Position::new(x as u16, y as u16));
            }
        }
        let _ = top_len;
        let page = if state.transcript().is_empty() {
            state.transcript().has_older().then_some(layout::PAGE)
        } else {
            self.laid.page
        };
        match page {
            Some(n) if self.page_asked != state.oldest_order() || self.page_asked.is_none() => {
                Some(n)
            }
            _ => None,
        }
    }
}

impl ChatView {
    /// The redesigned chat: a top line naming the agent and where it
    /// stands, the feed as turns, and the composer boxed with the model and
    /// mode on its bottom edge. Returns the page to ask for, as `draw` does.
    #[allow(clippy::too_many_arguments)]
    fn draw_turns(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        // The in-flight pane beside the chat narrows everything under the
        // header, unless the chat would be left too narrow: then it lies
        // over the feed's right side instead.
        let full = usize::from(area.width);
        let side = self.pane_open;
        let pane_width = (full / 3).clamp(PANE_MIN, PANE_MAX).min(full);
        let beside = side && full.saturating_sub(pane_width) >= CHAT_MIN;
        let width = if beside { full - pane_width } else { full };
        let name = state
            .agent()
            .name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "the agent".into());
        let host = state
            .host()
            .map(|host| host.name.clone())
            .unwrap_or_else(|| "its host".into());

        let strip = session_strip(state);
        let jobs = if self.pane_open {
            ui_view::background_jobs(state)
        } else {
            Vec::new()
        };
        let items = pane::items(&jobs, self.diff_files.as_deref());
        // The item drawn as a card, in the pane and, for a job, at its step
        // in the feed: the one under the mouse, else the one the pane's keys
        // are on.
        let pane_keys = self.pane_open && self.pane_keys;
        let lit = self
            .pane_hover
            .as_ref()
            .and_then(|hovered| items.iter().position(|item| item == hovered))
            .or(self
                .pane_focus
                .filter(|_| pane_keys)
                .map(|at| at.min(items.len().saturating_sub(1)))
                .filter(|_| !items.is_empty()));
        let (header, controls) = self.top_line(state, &name, &strip, full, theme);
        // A blank line above the header keeps it off the terminal's edge.
        self.header_spots = controls
            .into_iter()
            .map(|((from, to), control)| {
                (
                    area.y + 1,
                    (area.x + from as u16, area.x + to as u16),
                    control,
                )
            })
            .collect();
        let mut top: Vec<Line<'static>> = vec![Line::default(), header];
        if let Some(family) = family {
            let mut line = family_line(family, width, theme);
            for span in &mut line.spans {
                if span.style == theme.muted() {
                    span.style = theme.faint();
                }
            }
            top.push(line);
        }
        top.push(Line::default());

        let mut bottom: Vec<Line<'static>> = Vec::new();
        let view = composer(state, now_ms);
        if let Some(activity) = &view.activity {
            bottom.push(quiet_activity(activity, width, theme));
        }
        // The row rests on the composer's box; a blank line sets it apart
        // from whatever is above, as the feed's last row already ends in
        // one. It is the pane folded: while the pane is open it takes the
        // row's place.
        let mut row_at = None;
        if !side && let Some(line) = edge_row(&strip, !self.editor.is_empty(), now_ms, width, theme)
        {
            if !bottom.is_empty() {
                bottom.push(Line::default());
            }
            row_at = Some((bottom.len(), text::line_width(&line)));
            bottom.push(line);
        }
        bottom.extend(foot_cards(&strip, width, theme));
        let tray = Self::tray_rows(state);
        if let Some(selected) = self.tray {
            if tray.is_empty() {
                self.tray = None;
            } else if selected >= tray.len() {
                self.tray = Some(tray.len() - 1);
            }
        }
        let mut tray_at = Vec::new();
        for (i, row) in tray.iter().enumerate() {
            tray_at.push((bottom.len(), i));
            bottom.push(row.line(self.tray == Some(i), width, theme));
        }

        let mut cursor = None;
        let card = self.card(state);
        // Where the composer's box sits among the bottom lines, once drawn.
        let mut boxed_at: Option<(usize, Boxed)> = None;
        let mut composer_box = |bottom: &mut Vec<Line<'static>>,
                                cursor: &mut Option<(usize, usize)>,
                                editor: &Editor,
                                takes_keys: bool| {
            let boxed = boxed_composer(
                editor,
                &placeholder(&state.composer(), &name, &host, self.away),
                &EdgeWords {
                    model: ui_view::model_words(state),
                    effort: strip.effort.clone(),
                    mode: ui_view::mode_words(state),
                },
                !pane_keys,
                width,
                theme,
            );
            if takes_keys {
                *cursor = Some((bottom.len() + boxed.cursor.0, boxed.cursor.1));
            }
            let at = bottom.len();
            bottom.extend(boxed.lines.iter().cloned());
            boxed_at = Some((at, boxed));
        };
        let legend = || {
            if pane_keys {
                "j/k move · enter open · esc back · ctrl+t close".to_owned()
            } else {
                turn_hint_words(state, &self.editor, self.away, self.leader)
            }
        };
        let mut hint: Result<Line<'static>, String> = match &footer {
            Some(footer) => Ok(footer.clone()),
            None => Ok(Line::default()),
        };
        let mut boxed = false;
        match &card {
            Some(card) => {
                let lines = self.ask.render(card, &name, self.attach, width, theme);
                let cap = (usize::from(area.height) / 2).max(4);
                // The feed's last row already ends in a blank line.
                let start = bottom.len();
                let skip = lines.lines.len().saturating_sub(cap);
                if let Some((row, col)) = lines.cursor.filter(|(row, _)| *row >= skip) {
                    cursor = Some((start + row - skip, col));
                }
                bottom.extend(on_panel(
                    lines.lines.into_iter().skip(skip).collect(),
                    width,
                    theme,
                ));
                if footer.is_none() && card.state == CardState::Dismissed {
                    composer_box(&mut bottom, &mut cursor, &self.editor, true);
                    boxed = true;
                    hint = Err(legend());
                }
            }
            None => {
                composer_box(&mut bottom, &mut cursor, &self.editor, self.tray.is_none());
                boxed = true;
                if footer.is_none() {
                    hint = Err(match self.tray.and_then(|i| tray.get(i)) {
                        Some(row) => row.hint().to_owned(),
                        None => legend(),
                    });
                }
            }
        }
        // A blank line lets the composer's box breathe above the keys.
        if boxed {
            bottom.push(Line::default());
        }
        let (hint, hint_keys) = match hint {
            Ok(line) => (line, Vec::new()),
            Err(words) => keys_line(&words, width, theme),
        };
        let hint_row = bottom.len();
        bottom.push(hint);

        let height = usize::from(area.height);
        let feed_height = height.saturating_sub(top.len() + bottom.len());
        self.feed = (width, feed_height);
        self.feed_origin = (area.x, area.y + top.len() as u16);
        let feed_top = top.len();
        let mut lines = top;
        self.pin_spot = None;
        self.jump_spot = None;
        self.panel_spots.clear();
        if state.transcript().is_empty() {
            lines.extend(empty_feed(
                state,
                feed_height,
                now_ms - self.opened_at_ms,
                width,
                theme,
            ));
            self.laid = Laid::default();
        } else {
            let laid = self.frame(theme).layout(state);
            // A step revealed within the last screenful cannot reach the
            // top; the feed simply shows the newest row.
            if std::mem::take(&mut self.revealed) && laid.at_bottom {
                self.anchor = Anchor::Bottom;
            }
            let mut feed: Vec<Line<'static>> = laid.lines.clone();
            text::drop_cut_padding(&mut feed);
            if let Some((pinned, key)) = pinned(state, &laid, feed_height, width, theme) {
                let rows = pinned.len();
                // The blank line under the block is not the pin's to click.
                let block_rows = rows.saturating_sub(1);
                for (at, line) in pinned.into_iter().enumerate() {
                    if let Some(slot) = feed.get_mut(at) {
                        *slot = line;
                    }
                }
                let y = area.y + feed_top as u16;
                self.pin_spot = Some(((y, y + block_rows as u16), key));
                // The line under the pin is the feed's new edge: a cut
                // block's padding there would read as a stray band.
                if let Some(rest) = feed.get_mut(rows..) {
                    text::drop_cut_padding(rest);
                }
            }
            // The lit job's step, when the feed shows it, carries the same
            // card, so the pane's line and the feed's line read as one.
            if let Some(pane::PaneItem::Job(job)) = lit.and_then(|at| items.get(at))
                && let Some(block) = laid.blocks.iter().position(|block| &block.key == job)
            {
                let owners = line_owners(&laid, feed.len());
                // Exactly the step's own lines, flat: a block may end in
                // the blank line that sets it apart, which is not the step's.
                let rows: Vec<usize> = owners
                    .iter()
                    .enumerate()
                    .filter(|(at, owner)| {
                        **owner == Some(block) && text::line_width(&feed[*at]) > 0
                    })
                    .map(|(at, _)| at)
                    .collect();
                if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
                    pane::card_highlight(
                        &mut feed,
                        *first..*last + 1,
                        2,
                        width.saturating_sub(2),
                        false,
                        theme,
                    );
                }
            }
            if self.anchor != Anchor::Bottom
                && let Some(last) = feed.len().checked_sub(1)
            {
                // The control sits in the blank line that sets the feed
                // apart from what is below it, never over history.
                let (control, from) = jump_control(state, width, theme);
                let control_width = text::line_width(&control);
                feed[last] = text::overlay(&Line::default(), from, control, width);
                text::drop_cut_padding(&mut feed[..last]);
                self.jump_spot = Some((
                    area.y + (feed_top + last) as u16,
                    (area.x + from as u16, area.x + (from + control_width) as u16),
                ));
            }
            if let Some(entries) = self.panel.clone() {
                let panel = which_key_panel(&entries, self.leader, width, theme);
                let first = feed.len().saturating_sub(panel.len());
                for (i, (line, chord)) in panel.into_iter().enumerate() {
                    let at = first + i;
                    if let Some(slot) = feed.get_mut(at) {
                        let line_width = text::line_width(&line);
                        *slot = text::overlay(slot, 0, line, width);
                        if let Some(chord) = chord {
                            self.panel_spots.push((
                                area.y + (feed_top + at) as u16,
                                (area.x, area.x + line_width as u16),
                                chord,
                            ));
                        }
                    }
                }
            }
            lines.extend(feed);
            self.laid = laid;
        }
        let bottom_start = lines.len();
        let bottom_y = |row: usize| area.y + (bottom_start + row) as u16;
        self.hint_spots = hint_keys
            .into_iter()
            .map(|((from, to), key)| {
                (
                    bottom_y(hint_row),
                    (area.x + from as u16, area.x + to as u16),
                    key,
                )
            })
            .collect();
        self.tray_spots = tray_at
            .into_iter()
            .map(|(row, i)| (bottom_y(row), i))
            .collect();
        self.row_spot = match row_at {
            Some((at, row_width)) if !self.pane_open => {
                Some((bottom_y(at), (area.x + 2, area.x + row_width as u16)))
            }
            _ => None,
        };
        self.pane_spots.clear();
        self.pane_rect = None;
        self.mode_spot = None;
        self.composer_spot = None;
        if let Some((at, boxed)) = &boxed_at {
            if let Some((from, to)) = boxed.mode {
                self.mode_spot = Some((
                    bottom_y(at + boxed.lines.len() - 1),
                    (area.x + from as u16, area.x + to as u16),
                ));
            }
            self.composer_spot = Some(ComposerSpot {
                x: area.x + boxed.text_x as u16,
                y: bottom_y(at + 1),
                rows: boxed.lines.len().saturating_sub(2) as u16,
                skip: boxed.skip,
                wrap: boxed.wrap,
            });
        }
        lines.extend(bottom);
        paint.render_widget(Paragraph::new(lines), area);
        if side {
            // Beside the chat from the feed's first line down to the line
            // above the keys; over the feed alone when it lies on top.
            let x = area.x + (full - pane_width) as u16;
            let y = area.y + feed_top as u16;
            let bottom_edge = if beside {
                area.y + area.height.saturating_sub(1)
            } else {
                y + feed_height as u16
            };
            let rect = Rect {
                x,
                y,
                width: pane_width as u16,
                height: bottom_edge.saturating_sub(y),
            };
            if !beside {
                // Lying over the feed, it clears a column more than it
                // draws, so the feed's cut text never touches the rule.
                let cleared = Rect {
                    x: rect.x.saturating_sub(1),
                    width: rect.width + 1,
                    ..rect
                };
                paint.render_widget(ratatui::widgets::Clear, cleared);
            }
            self.pane_rect = Some(rect);
            self.draw_side_pane(paint, rect, &strip, &jobs, lit, now_ms, theme);
        }
        // While the pane has the keys, the composer shows no cursor.
        if let Some((row, col)) = cursor.filter(|_| !pane_keys) {
            let y = area.y as usize + bottom_start + row;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            if y < (area.y + area.height) as usize {
                paint.set_cursor_position(Position::new(x as u16, y as u16));
            }
        }
        let page = if state.transcript().is_empty() {
            state.transcript().has_older().then_some(layout::PAGE)
        } else {
            self.laid.page
        };
        match page {
            Some(n) if self.page_asked != state.oldest_order() || self.page_asked.is_none() => {
                Some(n)
            }
            _ => None,
        }
    }

    /// The pane beside the chat: a hairline rule down its left edge, then
    /// its lines past the margin, cut at the rect's foot.
    #[allow(clippy::too_many_arguments)]
    fn draw_side_pane(
        &mut self,
        paint: &mut Paint<'_>,
        rect: Rect,
        strip: &ui_view::Strip,
        jobs: &[ui_view::JobView],
        lit: Option<usize>,
        now_ms: i64,
        theme: Theme,
    ) {
        // The rule, a blank column, then the pane's lines out to the outer
        // margin: a highlighted job's tint spans them, its words sit two
        // columns further in, as on home.
        const LEFT: u16 = 2;
        const RIGHT: u16 = 2;
        let inner = rect.width.saturating_sub(LEFT + RIGHT);
        let focused = self.pane_keys;
        let files = self.diff_files.as_deref();
        let draw = |roomy| {
            pane::pane_lines(
                strip,
                jobs,
                files,
                lit,
                focused,
                roomy,
                now_ms,
                usize::from(inner),
                theme,
            )
        };
        let mut content = draw(true);
        let height = usize::from(rect.height);
        if content.lines.len() > height {
            content = draw(false);
        }
        // Still too tall: its last line says so rather than stopping short.
        if content.lines.len() > height && height > 0 {
            content.lines.truncate(height - 1);
            while content
                .lines
                .last()
                .is_some_and(|line| text::line_width(line) <= 2)
            {
                content.lines.pop();
            }
            let mut more = Line::from(Span::raw("  "));
            push(&mut more, "… more below", theme.faint(), usize::from(inner));
            content.lines.push(more);
            let shown = content.lines.len() - 1;
            content.hits.retain(|(row, _, _)| *row < shown);
        }
        // Which area has the keys shows on the frame, never the content:
        // the rule turns grey while the pane has them.
        let rule_ink = if focused {
            theme.muted()
        } else {
            theme.hairline()
        };
        let rule: Vec<Line<'static>> = (0..rect.height)
            .map(|_| Line::from(Span::styled("│", rule_ink)))
            .collect();
        paint.render_widget(
            Paragraph::new(rule),
            Rect {
                width: 1.min(rect.width),
                ..rect
            },
        );
        let body = Rect {
            x: rect.x + LEFT,
            width: inner,
            ..rect
        };
        for (row, (from, to), hit) in content.hits {
            if row < usize::from(rect.height) {
                self.pane_spots.push((
                    rect.y + row as u16,
                    (body.x + from as u16, body.x + to as u16),
                    hit,
                ));
            }
        }
        paint.render_widget(Paragraph::new(content.lines), body);
    }

    /// The header. At the left the agent's name, then faint where it runs:
    /// its project, and its host when that is not this machine. At the
    /// right how much of its context is used, the working tree's `[Diff]`
    /// and `[Home]`. Where the chat stands is not repeated here, since the
    /// feed shows work and asks; only a problem is (host away,
    /// reconnecting, catching up, exited). Returns the line and the
    /// controls' column ranges.
    fn top_line(
        &self,
        state: &SessionState,
        name: &str,
        strip: &ui_view::Strip,
        width: usize,
        theme: Theme,
    ) -> (Line<'static>, HeaderSpots) {
        let host = state
            .host()
            .map(|host| host.name.clone())
            .unwrap_or_default();
        // The right side, built first so the left knows its room.
        let mut right: Vec<Span<'static>> = Vec::new();
        // Groups on both sides are split by the same faint bar.
        let bar = || Span::styled(" │ ", theme.faint());
        let gap = |right: &mut Vec<Span<'static>>| {
            if !right.is_empty() {
                right.push(bar());
            }
        };
        if let Some((words, style)) = problem_words(state, self.away, &host, theme) {
            right.push(Span::styled(words, style));
        }
        if let Some(context) = &strip.context {
            gap(&mut right);
            let used = tokens_short(context.used_tokens);
            let words = match context.window_tokens {
                Some(window) => format!("{used} / {}", tokens_short(window)),
                None => used,
            };
            let style = if context.in_strip {
                theme.warn()
            } else {
                theme.faint()
            };
            right.push(Span::styled(words, style));
        }
        let diff = match self.diff_stat {
            Some((added, removed)) if added + removed > 0 => format!("[Diff +{added} −{removed}]"),
            _ => "[Diff]".to_owned(),
        };
        let mut controls = Vec::new();
        for (words, control) in [
            (diff, HeaderControl::Diff),
            ("[Home]".to_owned(), HeaderControl::Home),
        ] {
            gap(&mut right);
            let style = if self.hover == Some(control) {
                theme.emphasis()
            } else {
                theme.muted()
            };
            controls.push((right.len(), text::str_width(&words), control));
            right.push(Span::styled(words, style));
        }
        let right_width: usize = right
            .iter()
            .map(|span| text::str_width(&span.content))
            .sum();
        let end = width.saturating_sub(2);
        let right_at = end.saturating_sub(right_width);

        let mut line = Line::from(Span::raw("  "));
        let room = right_at.saturating_sub(2);
        push(&mut line, name, theme.bright(), room);
        // Where it works: its path as the person would write it, and its
        // host when that is not this machine. The branch goes before the
        // path, as `main ~/source/amux`, once the inventory carries one.
        let cwd = &state.agent().cwd;
        let mut place = if self.local {
            text::tilde(cwd)
        } else {
            cwd.clone()
        };
        if !self.local && !host.is_empty() {
            place = format!("{place} · {host}");
        }
        if !place.is_empty() && text::line_width(&line) + 6 < room {
            line.spans.push(bar());
            push(&mut line, place, theme.faint(), room);
        }
        let mut spots = Vec::new();
        if text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            let mut at = right_at;
            for (i, span) in right.into_iter().enumerate() {
                if let Some((_, span_width, control)) =
                    controls.iter().find(|(index, _, _)| *index == i)
                {
                    spots.push(((at, at + span_width), *control));
                }
                at += text::str_width(&span.content);
                line.spans.push(span);
            }
        }
        (line, spots)
    }
}

/// "41K", "1.2M": a token count at a glance.
fn tokens_short(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}K", (count + 500) / 1_000),
        _ => {
            let tenths = (count + 50_000) / 100_000;
            if tenths.is_multiple_of(10) {
                format!("{}M", tenths / 10)
            } else {
                format!("{}.{}M", tenths / 10, tenths % 10)
            }
        }
    }
}

/// Where the chat stands, only when that is a problem the person should
/// see: the host away, reconnecting, catching up, a rebuilt history on its
/// way, or the agent exited. Working, idle and needs-you show in the feed.
fn problem_words(
    state: &SessionState,
    away: Away,
    host: &str,
    theme: Theme,
) -> Option<(String, ratatui::style::Style)> {
    let (words, style) = state_words(state, away, host, theme);
    match (state.composer(), state.phase()) {
        (_, PhaseView::Exited { .. }) | (Composer::Disabled(_), _) => Some((
            words,
            if style == theme.muted() {
                theme.faint()
            } else {
                style
            },
        )),
        _ if state.reset_pending() => Some((words, theme.faint())),
        _ => None,
    }
}

/// While the agent works, one quiet line that it is, and for how long.
/// What it is doing shows as live steps in the feed.
fn quiet_activity(activity: &ui_state::Activity, width: usize, theme: Theme) -> Line<'static> {
    let elapsed = text::duration(activity.elapsed_ms - activity.elapsed_ms % 1_000);
    let words = match &activity.kind {
        ActivityKind::Thinking => "Thinking".to_owned(),
        ActivityKind::Subagents { count } => format!(
            "{count} subagent{} working",
            if *count == 1 { "" } else { "s" }
        ),
        ActivityKind::Compacting => "Compacting".to_owned(),
        ActivityKind::Retrying { attempt, .. } => format!("Retrying · attempt {attempt}"),
        ActivityKind::Working | ActivityKind::Running { .. } => "Working".to_owned(),
    };
    let mut line = Line::from(Span::raw("    "));
    push(&mut line, words, theme.muted(), width);
    push(&mut line, format!(" · {elapsed}"), theme.faint(), width);
    line
}

/// What the composer's bottom edge says, in the words a person reads: the
/// model by its name, its effort, and the mode.
struct EdgeWords {
    model: Option<String>,
    effort: Option<String>,
    mode: Option<String>,
}

/// The composer in its box, as drawn: its lines, the cursor's (line,
/// column), the mode's columns on the bottom edge, where the draft's words
/// start, how many wrapped lines are scrolled off above, and the width the
/// draft wraps at.
struct Boxed {
    lines: Vec<Line<'static>>,
    cursor: (usize, usize),
    mode: Option<(usize, usize)>,
    text_x: usize,
    skip: usize,
    wrap: usize,
}

/// The composer in its box, the model, effort and mode on the bottom edge.
fn boxed_composer(
    editor: &Editor,
    placeholder: &str,
    edge_words: &EdgeWords,
    focused: bool,
    width: usize,
    theme: Theme,
) -> Boxed {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let (mut body, (row, col)) = editor_lines(editor, placeholder, inner, theme);
    // The box already says where to type, so the edge mark becomes a prompt.
    for (i, line) in body.iter_mut().enumerate() {
        if let Some(first) = line.spans.first_mut() {
            *first = Span::styled(if i == 0 { "› " } else { "  " }, theme.muted());
        }
    }
    let skip = (row + 1).saturating_sub(COMPOSER_LINES);
    // Holding the keys, the box's edge is grey; otherwise it recedes to a
    // hairline. The words on its edge keep their own inks.
    let edge = if focused {
        theme.muted()
    } else {
        theme.hairline()
    };
    let span = width.saturating_sub(2 * MARGIN + 2);
    let mut out = Vec::new();
    let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut top, "╭", edge, width);
    push(&mut top, "─".repeat(span), edge, width);
    push(&mut top, "╮", edge, width);
    out.push(top);
    for line in body.into_iter().skip(skip).take(COMPOSER_LINES) {
        let mut boxed = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut boxed, "│ ", edge, width);
        boxed.spans.extend(line.spans);
        text::pad_to(&mut boxed, width - MARGIN - 1);
        push(&mut boxed, "│", edge, width);
        out.push(boxed);
    }
    // "Opus (high) · accept edits": the model faint by its name, its
    // effort beside it, and the mode, which Shift+Tab (or a click) changes,
    // in the reading ink. The edge is a status line, so the mode is
    // lowercase.
    let known = |fact: &Option<String>| fact.clone().filter(|fact| !fact.is_empty());
    let mut label: Vec<Span<'static>> = Vec::new();
    match (known(&edge_words.model), known(&edge_words.effort)) {
        (Some(model), Some(effort)) => {
            label.push(Span::styled(format!("{model} ({effort})"), theme.faint()))
        }
        (Some(model), None) => label.push(Span::styled(model, theme.faint())),
        (None, Some(effort)) => label.push(Span::styled(format!("({effort})"), theme.faint())),
        (None, None) => {}
    }
    let mode_words = known(&edge_words.mode).map(|mode| mode.to_lowercase());
    if let Some(mode) = &mode_words {
        if !label.is_empty() {
            label.push(Span::styled(" · ", theme.faint()));
        }
        label.push(Span::styled(mode.clone(), theme.text()));
    }
    let label_width: usize = label
        .iter()
        .map(|part| text::str_width(&part.content))
        .sum();
    let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut bottom, "╰", edge, width);
    let mut mode = None;
    if label.is_empty() || label_width + 6 > span {
        push(&mut bottom, "─".repeat(span), edge, width);
    } else {
        let rule = span.saturating_sub(label_width + 3).max(1);
        push(&mut bottom, "─".repeat(rule), edge, width);
        push(&mut bottom, " ", edge, width);
        bottom.spans.extend(label);
        if let Some(words) = &mode_words {
            let to = text::line_width(&bottom);
            mode = Some((to - text::str_width(words), to));
        }
        push(&mut bottom, " ─", edge, width);
    }
    text::pad_to(&mut bottom, width - MARGIN - 1);
    push(&mut bottom, "╯", edge, width);
    out.push(bottom);
    Boxed {
        lines: out,
        cursor: (1 + row - skip, MARGIN + 2 + col),
        mode,
        text_x: MARGIN + 4,
        skip,
        wrap: inner,
    }
}

/// The keys under the composer, by what is happening, few enough to read
/// at a glance and always ending with the way to more (the leader's
/// panel). What everyone knows (Enter sends, pasting attaches) is not
/// said; Enter is named only when it does something else. Words that are
/// not a legend start with a capital and read as a sentence.
fn turn_hint_words(state: &SessionState, editor: &Editor, away: Away, leader: char) -> String {
    let working = state.phase() == PhaseView::Working;
    let mode = next_mode(state).is_some();
    let more = format!("ctrl+{leader} more");
    let mut pairs: Vec<String> = Vec::new();
    match state.composer() {
        Composer::Send => {
            if working {
                pairs.push("enter queue".into());
                pairs.push("ctrl+x stop".into());
            } else if !editor.is_empty() && !crate::terminal::shift_enter_reported() {
                // Shift+Enter is the newline everyone expects; where the
                // terminal cannot tell it from Enter, say the key that works.
                pairs.push("ctrl+j newline".into());
            }
            if mode {
                pairs.push("shift+tab mode".into());
            }
        }
        Composer::Resume => pairs.push("enter resume".into()),
        Composer::Disabled(Waiting::Detached) if away == Away::SignedOut => {
            return "Draft kept · sending waits until this machine signs in".to_owned();
        }
        Composer::Disabled(Waiting::Detached) if away == Away::Revoked => {
            return "Draft kept · sending waits until you pair again".to_owned();
        }
        Composer::Disabled(_) => return "Draft kept · sending waits".to_owned(),
    }
    pairs.push(more);
    pairs.join(" · ")
}

/// Where each drawn key pair sits on its line, and the key it presses.
type KeySpots = Vec<((usize, usize), KeyEvent)>;

/// A key legend in home's style: each key bright and bold, its action
/// faint, three blanks apart, whole pairs or none. Words that are not a
/// legend (they start with a capital) read as one faint sentence. Returns
/// the line and, for each pair drawn, its columns and the key it stands
/// for, so a click can press it.
fn keys_line(words: &str, width: usize, theme: Theme) -> (Line<'static>, KeySpots) {
    let mut line = Line::from(Span::raw("  "));
    let mut spots = Vec::new();
    if words.chars().next().is_some_and(char::is_uppercase) {
        push(&mut line, words, theme.faint(), width);
        return (line, spots);
    }
    let room = width.saturating_sub(2);
    for (i, pair) in words.split(" · ").enumerate() {
        let gap = if i > 0 { 3 } else { 0 };
        if text::line_width(&line) + gap + text::str_width(pair) > room {
            break;
        }
        if i > 0 {
            push(&mut line, "   ", theme.faint(), room);
        }
        let from = text::line_width(&line);
        let (key, action) = split_key(pair);
        push(&mut line, key.clone(), theme.emphasis(), room);
        if !action.is_empty() {
            push(&mut line, format!(" {action}"), theme.faint(), room);
        }
        if let Some(event) = key_event(&key) {
            spots.push(((from, text::line_width(&line)), event));
        }
    }
    (line, spots)
}

/// The key a legend names, as the event pressing it sends: `enter`,
/// `shift+tab`, `ctrl+x`, a letter.
fn key_event(key: &str) -> Option<KeyEvent> {
    let (modifiers, name) = match key.split_once('+') {
        Some(("ctrl", name)) => (KeyModifiers::CONTROL, name),
        Some(("shift", "tab")) => {
            return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        }
        Some(_) => return None,
        None => (KeyModifiers::NONE, key),
    };
    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "end" => KeyCode::End,
        "↑" => KeyCode::Up,
        name if name.chars().count() == 1 => KeyCode::Char(name.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

/// "enter send now" → ("enter", "send now"); "ctrl+x stop" → ("ctrl+x", "stop").
fn split_key(pair: &str) -> (String, String) {
    match pair.split_once(' ') {
        Some((key, action)) => (key.to_owned(), action.to_owned()),
        None => (pair.to_owned(), String::new()),
    }
}

/// The prompt to pin under the header, and its key: the prompt of the turn
/// that owns the feed's first line under the pin. Best effort: nothing when
/// that turn's prompt is not held (older than the window; a page will bring
/// it), when the line belongs to no turn, or when the feed is too short to
/// spare the rows. Faint while the next turn's prompt is about to take the
/// pin.
fn pinned(
    state: &SessionState,
    laid: &Laid,
    height: usize,
    width: usize,
    theme: Theme,
) -> Option<(Vec<Line<'static>>, Key)> {
    // The block's three lines and the blank line a message keeps under it.
    const ROWS: usize = 4;
    // The next prompt within this many lines of the pin fades it.
    const FADE: usize = 3;
    if height < ROWS + 6 {
        return None;
    }
    let owners = line_owners(laid, height);
    let owner = (*owners.get(ROWS)?)?;
    let order = laid.blocks.get(owner)?.order;
    let transcript = state.transcript();
    let oldest = transcript.oldest_held()?;
    let mut prompt = None;
    for held in transcript.range(oldest..=order).rev() {
        match held.class {
            ui_state::ItemClass::Prompt => {
                prompt = Some(held.item.key.clone());
                break;
            }
            // Another turn's end: the line belongs to no turn.
            ui_state::ItemClass::Turn if held.item.order != order => return None,
            _ => {}
        }
    }
    let key = prompt?;
    // A prompt whose own block starts on screen below the top is its own
    // landmark: pinning it too would draw it twice.
    if let Some(first) = owners
        .iter()
        .position(|block| block.is_some_and(|block| laid.blocks[block].key == key))
        && first > 0
    {
        return None;
    }
    let row = chat_rows_for(
        state,
        std::slice::from_ref(&key),
        &ChatOptions {
            tools: ToolRows::ShowAll,
        },
    )
    .pop()?;
    let next = owners
        .iter()
        .enumerate()
        .skip(ROWS + 1)
        .find(|(_, block)| {
            block.is_some_and(|block| {
                block != owner && matches!(laid.blocks[block].row.kind, RowKind::Prompt { .. })
            })
        })
        .map(|(at, _)| at);
    let faint = next.is_some_and(|at| at - ROWS <= FADE);
    let mut lines = feed::pinned_prompt(&row, faint, width, theme);
    if lines.is_empty() {
        return None;
    }
    lines.push(Line::default());
    Some((lines, key))
}

/// Which drawn block each of the feed's `height` lines belongs to, top
/// first; `None` for the blank lines that pad a short chat.
fn line_owners(laid: &Laid, height: usize) -> Vec<Option<usize>> {
    let mut owners: Vec<Option<usize>> = laid
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(i, block)| std::iter::repeat_n(Some(i), block.lines.len()))
        .skip(laid.top_offset)
        .take(height)
        .collect();
    if owners.len() < height {
        let mut padded = vec![None; height - owners.len()];
        padded.append(&mut owners);
        owners = padded;
    }
    owners
}

/// The control that returns to the newest row while the reader is in
/// history, centred: "↓ Jump to Bottom  ctrl+end", or how many rows arrived
/// meanwhile. Returns it and its first column.
fn jump_control(state: &SessionState, width: usize, theme: Theme) -> (Line<'static>, usize) {
    let surface = ratatui::style::Style {
        bg: theme.user_surface().bg,
        ..Default::default()
    };
    let arrived = state.held_arrivals();
    let words = if arrived > 0 {
        format!("↓ {arrived} new")
    } else if state.arrivals_held() {
        "↓ New Activity".to_owned()
    } else {
        "↓ Jump to Bottom".to_owned()
    };
    let line = Line::from(vec![
        Span::styled(format!(" {words}  "), theme.text().patch(surface)),
        Span::styled("ctrl+end ", theme.faint().patch(surface)),
    ]);
    let from = width.saturating_sub(text::line_width(&line)) / 2;
    (line, from)
}

/// The leader's panel: every next key and what it does, on the tinted
/// surface at the left margin, with a line of padding above and below.
/// Each line comes with the chord it picks, for clicks.
fn which_key_panel(
    entries: &[PanelEntry],
    leader: char,
    width: usize,
    theme: Theme,
) -> Vec<(Line<'static>, Option<char>)> {
    const MARGIN: usize = 2;
    const INSET: usize = 2;
    let surface = ratatui::style::Style {
        bg: theme.user_surface().bg,
        ..Default::default()
    };
    let key_width = entries
        .iter()
        .map(|(key, _, _)| text::str_width(key))
        .max()
        .unwrap_or(1);
    let title = format!("ctrl+{leader}");
    let inner = entries
        .iter()
        .map(|(_, action, _)| key_width + 3 + text::str_width(action))
        .max()
        .unwrap_or(0)
        .max(text::str_width(&title));
    let panel_width = (inner + 2 * INSET).min(width.saturating_sub(2 * MARGIN));
    let row = |spans: Vec<Span<'static>>| {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        line.spans.push(Span::styled(" ".repeat(INSET), surface));
        for span in spans {
            line.spans
                .push(Span::styled(span.content, span.style.patch(surface)));
        }
        text::fill(&mut line, surface, MARGIN + panel_width);
        line
    };
    let mut out = vec![
        (row(Vec::new()), None),
        (row(vec![Span::styled(title, theme.faint())]), None),
    ];
    for (key, action, chord) in entries {
        let pad = key_width.saturating_sub(text::str_width(key));
        out.push((
            row(vec![
                Span::styled(key.clone(), theme.emphasis()),
                Span::raw(" ".repeat(pad + 3)),
                Span::styled(action.clone(), theme.faint()),
            ]),
            Some(*chord),
        ));
    }
    out.push((row(Vec::new()), None));
    out
}

fn mime_of(name: &str) -> &'static str {
    let extension = name.rsplit('.').next().unwrap_or_default().to_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// What the activity line names while a tool runs: the call's subject.
fn running_subject(state: &SessionState, key: &Key) -> Option<String> {
    let opts = ChatOptions {
        tools: ToolRows::ShowAll,
    };
    let row = chat_rows_for(state, std::slice::from_ref(key), &opts).pop()?;
    Some(match row.kind {
        RowKind::Command { command, .. } => text::first_line(&command).to_owned(),
        RowKind::Explore { subject, .. } => subject,
        RowKind::ToolCall { server, tool, .. } if server.is_empty() => tool,
        RowKind::ToolCall { server, tool, .. } => format!("{server} · {tool}"),
        RowKind::FileChange { files, .. } => {
            files.first().map(|f| f.path.clone()).unwrap_or_default()
        }
        _ => return None,
    })
}

fn kind_word(kind: wire::Kind) -> &'static str {
    match kind {
        wire::Kind::ClaudePty => "claude",
        wire::Kind::ClaudeSdk => "claude sdk",
        wire::Kind::Codex => "codex",
        wire::Kind::Unspecified => "agent",
    }
}

/// "fix-auth · claude @ mbp          opus · high · default · working"
fn header(state: &SessionState, away: Away, width: usize, theme: Theme) -> Line<'static> {
    let agent = state.agent();
    let name = agent.name.clone().unwrap_or_else(|| "unnamed".into());
    let host = state
        .host()
        .map(|host| host.name.clone())
        .unwrap_or_default();
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, name, theme.emphasis(), width);
    let mut about = format!(" · {}", kind_word(state.kind()));
    if !host.is_empty() {
        about.push_str(&format!(" @ {host}"));
    }
    push(&mut line, about, theme.muted(), width);
    let (words, style) = state_words(state, away, &host, theme);
    let strip = session_strip(state);
    let facts: Vec<String> = [strip.model, strip.effort, strip.mode]
        .into_iter()
        .flatten()
        .filter(|fact| !fact.is_empty())
        .collect();
    let mut right = facts.join(" · ");
    let room = width.saturating_sub(text::line_width(&line) + 4);
    if right.is_empty() || text::str_width(&right) + text::str_width(&words) + 3 > room {
        right = words.clone();
    } else {
        right = format!("{right} · {words}");
    }
    let styled_words = right.ends_with(&words) && style != theme.muted();
    if styled_words {
        let prefix = right[..right.len() - words.len()].to_owned();
        let total = text::str_width(&right);
        let used = text::line_width(&line);
        if used + 2 + total <= width {
            line.spans.push(Span::raw(" ".repeat(width - used - total)));
            line.spans.push(Span::styled(prefix, theme.muted()));
            line.spans.push(Span::styled(words, style));
        }
    } else {
        push_right(&mut line, &right, theme.muted(), width);
    }
    line
}

/// The one word, or few, for where the chat stands: needs you, working,
/// idle, exited with its cause, or why it cannot be current.
fn state_words(
    state: &SessionState,
    away: Away,
    host: &str,
    theme: Theme,
) -> (String, ratatui::style::Style) {
    let away_words = || {
        let host = if host.is_empty() { "host" } else { host };
        match away {
            Away::Plain => format!("{host} away · not current"),
            Away::SignedOut => format!("{host} away · this machine is signed out"),
            Away::Revoked => format!("{host} no longer trusts this machine"),
        }
    };
    // The session's host entry follows the fleet; this machine's own is
    // always online.
    let host_away = state
        .host()
        .is_some_and(|entry| entry.presence != wire::Presence::Online as i32);
    match (state.composer(), state.phase()) {
        // Resuming asks the host, so why it is away matters more than how
        // the agent ended.
        (_, PhaseView::Exited { .. }) if host_away => {
            (format!("exited · {}", away_words()), theme.warn())
        }
        (_, PhaseView::Exited { cause }) => (
            match cause {
                Some(cause) if !cause.is_empty() && cause != "exited" => {
                    format!("exited · {cause}")
                }
                _ => "exited".to_owned(),
            },
            theme.muted(),
        ),
        (Composer::Disabled(Waiting::Detached), _) => (away_words(), theme.warn()),
        (Composer::Disabled(Waiting::Reconnecting), _) => ("reconnecting".to_owned(), theme.warn()),
        (Composer::Disabled(Waiting::CatchingUp), _) => ("catching up".to_owned(), theme.muted()),
        (_, _) if state.reset_pending() => ("refreshing".to_owned(), theme.muted()),
        (_, PhaseView::NeedsYou) => ("needs you".to_owned(), theme.accent()),
        (_, PhaseView::Working) => ("working".to_owned(), theme.muted()),
        (_, PhaseView::Idle) => ("idle".to_owned(), theme.muted()),
        (_, PhaseView::Starting) => ("starting".to_owned(), theme.muted()),
    }
}

/// "↑ planner · 3 subagents · 1 needs you"
fn family_line(family: &FamilyHeader, width: usize, theme: Theme) -> Line<'static> {
    let mut parts = Vec::new();
    if let Some(parent) = &family.parent {
        parts.push(format!(
            "↑ {}",
            if parent.name.is_empty() {
                "parent"
            } else {
                &parent.name
            }
        ));
    }
    let count = family.children.len();
    if count > 0 {
        parts.push(format!(
            "{count} subagent{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    let waiting = family
        .children
        .iter()
        .filter(|child| child.attention == ui_state::Attention::NeedsYou)
        .count();
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, parts.join(" · "), theme.muted(), width);
    if waiting > 0 {
        push(
            &mut line,
            format!(
                " · {waiting} need{} you",
                if waiting == 1 { "s" } else { "" }
            ),
            theme.accent(),
            width,
        );
    }
    line
}

fn composer_hint(
    state: &SessionState,
    editor: &Editor,
    away: Away,
    leader: char,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let keys = HintKeys {
        working: state.phase() == PhaseView::Working,
        leader,
        mode: next_mode(state).is_some(),
    };
    hint_line(&state.composer(), away, keys, editor, width, theme)
}

/// What the keys under the composer depend on beyond its mode and draft.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HintKeys {
    pub working: bool,
    pub leader: char,
    /// Shift+Tab changes the agent's mode.
    pub mode: bool,
}

/// The keys under the composer, for its mode and draft, with the mode key
/// at the right while the composer sends.
pub(crate) fn hint_line(
    composer: &Composer,
    away: Away,
    keys: HintKeys,
    editor: &Editor,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let review = format!("ctrl+{} r review", keys.leader);
    let words = match composer {
        Composer::Send if keys.working => {
            "enter queue · ctrl+j newline · ↑ queued · ctrl+x stop".to_owned()
        }
        Composer::Send if editor.is_empty() => {
            format!("enter send · ctrl+j newline · ctrl+v attach · {review} · ? help")
        }
        Composer::Send => format!("enter send · ctrl+j newline · ctrl+v attach · {review}"),
        Composer::Resume => "enter resume with this message · ctrl+j newline".to_owned(),
        Composer::Disabled(Waiting::Detached) if away == Away::SignedOut => {
            "draft kept · sending waits until this machine signs in".to_owned()
        }
        Composer::Disabled(Waiting::Detached) if away == Away::Revoked => {
            "draft kept · sending waits until you pair again".to_owned()
        }
        Composer::Disabled(_) => "draft kept · sending waits".to_owned(),
    };
    let mut line = Line::from(Span::raw("  "));
    let mode = if keys.mode && *composer == Composer::Send {
        "shift+tab mode"
    } else {
        ""
    };
    let room = width.saturating_sub(text::str_width(mode) + 3);
    push(&mut line, words, theme.muted(), room);
    push_right(&mut line, mode, theme.muted(), width.saturating_sub(1));
    line
}

/// The mode Shift+Tab moves the agent to: its own next mode where the
/// agent only cycles, else the next offered mode that still asks before
/// acting. None where the mode cannot change from here.
pub(crate) fn next_mode(state: &SessionState) -> Option<ui_view::SettingChange> {
    let view = ui_view::settings(state);
    if view.cycle_mode {
        return Some(ui_view::SettingChange::CycleMode);
    }
    if view.mode_refusal.is_some() {
        return None;
    }
    let offered: Vec<&ui_view::ModeChoice> = view
        .modes
        .iter()
        .filter(|mode| !mode.reported && (!mode.stops_asking || mode.current))
        .collect();
    if offered.len() < 2 {
        return None;
    }
    let next = offered
        .iter()
        .position(|mode| mode.current)
        .map_or(0, |at| (at + 1) % offered.len());
    Some(ui_view::SettingChange::Mode(offered[next].value.clone()))
}

/// The ask card's lines on the panel surface, edge to edge.
pub(crate) fn on_panel(
    lines: Vec<Line<'static>>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let panel = theme.panel();
    lines
        .into_iter()
        .map(|line| {
            let mut line = Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, panel.patch(span.style)))
                    .collect::<Vec<_>>(),
            );
            text::fill(&mut line, panel, width);
            line
        })
        .collect()
}

fn empty_feed(
    state: &SessionState,
    height: usize,
    waited_ms: i64,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(); height];
    let away = matches!(state.composer(), Composer::Disabled(Waiting::Detached))
        || state.host().is_some_and(|host| {
            matches!(
                host.presence(),
                wire::Presence::Offline | wire::Presence::Away
            )
        });
    let words = if away {
        Some("Its host is away, and nothing of this chat is held here yet.")
    } else if state.has_snapshot() && state.caught_up() {
        Some("Nothing here yet.")
    } else if waited_ms >= LOADING_HINT_MS {
        Some("Loading…")
    } else {
        None
    };
    if let (Some(words), Some(line)) = (words, lines.get_mut(height / 2)) {
        let mut centred = Line::from(Span::raw(" ".repeat(width.saturating_sub(words.len()) / 2)));
        push(&mut centred, words, theme.muted(), width);
        *line = centred;
    }
    lines
}

fn draw_reader(paint: &mut Paint<'_>, area: Rect, reader: &mut Reader, theme: Theme) {
    let width = usize::from(area.width);
    let height = usize::from(area.height).saturating_sub(3);
    let body: Vec<String> = text::wrap(&reader.text, width.saturating_sub(4));
    let max = body.len().saturating_sub(height);
    reader.scroll = reader.scroll.min(max);
    let mut lines = Vec::new();
    let mut head = Line::from(Span::raw("  "));
    push(&mut head, reader.title.clone(), theme.emphasis(), width);
    let shown = format!(
        "lines {}-{}/{}",
        (reader.scroll + 1).min(body.len()),
        (reader.scroll + height).min(body.len()),
        body.len()
    );
    push_right(&mut head, &shown, theme.muted(), width);
    lines.push(head);
    lines.push(Line::from(Span::styled("─".repeat(width), theme.muted())));
    for part in body.iter().skip(reader.scroll).take(height) {
        let style = if part.starts_with("@@") {
            theme.diff_meta()
        } else if part.starts_with('+') {
            theme.diff_added()
        } else if part.starts_with('-') {
            theme.diff_removed()
        } else {
            theme.text()
        };
        let mut line = Line::from(Span::raw("  "));
        push(&mut line, part.clone(), style, width);
        lines.push(line);
    }
    while lines.len() < height + 2 {
        lines.push(Line::default());
    }
    let mut foot = Line::from(Span::raw("  "));
    push(
        &mut foot,
        "↑↓/pgup/pgdn scroll · g/G top/bottom · esc close",
        theme.muted(),
        width,
    );
    lines.push(foot);
    paint.render_widget(Paragraph::new(lines), area);
}
