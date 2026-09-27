//! One open chat: the client's view state over a session, the keys that
//! act on it, and the frame it draws.
//!
//! Everything here is ephemeral and the client's own: the anchor, the
//! expansion set keyed by item keys, the focused row, the draft, the ask
//! card's picks. Keys turn into [`ChatEffect`]s the event loop carries out
//! against the session; nothing in this module does I/O.

pub mod ask;
pub mod composer;
pub mod layout;
pub mod review;
pub mod rows;

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
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
    COMPOSER_LINES, TrayRow, activity_line, editor_lines, foot_cards, placeholder, strip_line,
};
use self::layout::{Anchor, Frame, Laid};
use self::review::{ReviewAction, ReviewPage};
use crate::clipboard::ClipboardContent;
use crate::editor::{Edit, Editor};
use crate::text::{self, push, push_right};
use crate::theme::Theme;

/// How long an empty chat waits before saying it is loading.
pub const LOADING_HINT_MS: i64 = 300;
/// Feed lines one wheel notch scrolls.
const WHEEL_LINES: isize = 3;

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
}

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
    epoch: u64,
    opened_at_ms: i64,
    /// The last frame's layout, for scrolling and focus.
    laid: Laid,
    feed: (usize, usize),
    /// Rows arrived below while the reader was scrolled back.
    new_below: bool,
    seen_head: Option<u64>,
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
            epoch: 0,
            opened_at_ms: now_ms,
            laid: Laid::default(),
            feed: (0, 0),
            new_below: false,
            seen_head: None,
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
        }
    }

    /// The card to draw, synced with this client's picks.
    fn card(&mut self, state: &SessionState) -> Option<AskCard> {
        let card = ask_card(state)?;
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
        if self.ask.editing() {
            return true;
        }
        self.card_takes_keys(state).is_none() && self.tray.is_none() && !self.editor.is_empty()
    }

    /// Ctrl+C on a field with text: clears it as a kill.
    pub fn kill_field(&mut self) -> bool {
        if self.review_open {
            return self.review.as_mut().is_some_and(ReviewPage::kill_field);
        }
        self.ask.kill_field() || self.editor.kill_all()
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
        if let Some(card) = self.card_takes_keys(state) {
            self.ask.sync(&card);
            if key.code == KeyCode::Esc && !self.ask.editing() {
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
        self.new_below = false;
    }

    fn scroll(&mut self, state: &SessionState, delta: isize, theme: Theme) {
        let anchor = self.frame(theme).scrolled(state, &self.laid, delta);
        if anchor == Anchor::Bottom {
            self.new_below = false;
        }
        self.anchor = anchor;
    }

    pub fn mouse(&mut self, state: &SessionState, event: MouseEvent, theme: Theme) {
        if self.review_open
            && let Some(page) = &mut self.review
        {
            match event.kind {
                MouseEventKind::ScrollUp => page.scroll_by(-WHEEL_LINES),
                MouseEventKind::ScrollDown => page.scroll_by(WHEEL_LINES),
                _ => {}
            }
            return;
        }
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll(state, -WHEEL_LINES, theme),
            MouseEventKind::ScrollDown => self.scroll(state, WHEEL_LINES, theme),
            _ => {}
        }
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

    /// `<leader> o`: expand or collapse the focused row, or its run.
    pub fn toggle_expanded(&mut self, state: &SessionState) {
        let Some(key) = self.focus.clone() else {
            return;
        };
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
        self.review = Some(ReviewPage::new(diff, patch));
        self.review_open = true;
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
        let mut top: Vec<Line<'static>> = vec![header(state, self.away, width, theme)];
        if let Some(family) = family {
            top.push(family_line(family, width, theme));
        }
        top.push(Line::default());

        let mut bottom: Vec<Line<'static>> = Vec::new();
        let head = state.transcript().head();
        if head != self.seen_head {
            if self.anchor != Anchor::Bottom && self.seen_head.is_some() {
                self.new_below = true;
            }
            self.seen_head = head;
        }
        if self.anchor != Anchor::Bottom {
            let words = if self.new_below {
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
                    hint = composer_hint(state, &self.editor, self.away, width, theme);
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
                        None => composer_hint(state, &self.editor, self.away, width, theme),
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
    let (words, style) = match (state.composer(), state.phase()) {
        (_, PhaseView::Exited { cause }) => (
            match cause {
                Some(cause) if !cause.is_empty() && cause != "exited" => {
                    format!("exited · {cause}")
                }
                _ => "exited".to_owned(),
            },
            theme.muted(),
        ),
        (Composer::Disabled(Waiting::Detached), _) => (
            format!(
                "{} away · {}",
                if host.is_empty() { "host" } else { &host },
                match away {
                    Away::Plain => "not current",
                    Away::SignedOut => "this machine is signed out",
                }
            ),
            theme.warn(),
        ),
        (Composer::Disabled(Waiting::Reconnecting), _) => ("reconnecting".to_owned(), theme.warn()),
        (Composer::Disabled(Waiting::CatchingUp), _) => ("catching up".to_owned(), theme.muted()),
        (_, _) if state.reset_pending() => ("refreshing".to_owned(), theme.muted()),
        (_, PhaseView::NeedsYou) => ("needs you".to_owned(), theme.accent()),
        (_, PhaseView::Working) => ("working".to_owned(), theme.muted()),
        (_, PhaseView::Idle) => ("idle".to_owned(), theme.muted()),
        (_, PhaseView::Starting) => ("starting".to_owned(), theme.muted()),
    };
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
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let working = state.phase() == PhaseView::Working;
    hint_line(&state.composer(), away, working, editor, width, theme)
}

/// The keys under the composer, for its mode and draft.
pub(crate) fn hint_line(
    composer: &Composer,
    away: Away,
    working: bool,
    editor: &Editor,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let words = match composer {
        Composer::Send if working => "enter queue · ctrl+j newline · ↑ queued · ctrl+x stop",
        Composer::Send if editor.is_empty() => {
            "enter send · ctrl+j newline · ctrl+v attach · ? help"
        }
        Composer::Send => "enter send · ctrl+j newline · ctrl+v attach",
        Composer::Resume => "enter resume with this message · ctrl+j newline",
        Composer::Disabled(Waiting::Detached) if away == Away::SignedOut => {
            "draft kept · sending waits until this machine signs in"
        }
        Composer::Disabled(_) => "draft kept · sending waits",
    };
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, words, theme.muted(), width);
    line
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
