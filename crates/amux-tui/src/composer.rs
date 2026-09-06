//! The composer: a multiline readline editor over the draft (`docs/CHAT.md`
//! §Composer and control and §Keybindings).
//!
//! The draft is renderer ViewState (D1): it survives ask takeovers,
//! scrolling, phase changes, and send gating — nothing here reads the
//! Model. Every clearing operation is a kill into a single-slot kill
//! buffer (keybindings §5.3), so no clearing key is destructive: Ctrl+Y
//! restores the last kill. Cursor arithmetic is char-based throughout —
//! byte offsets never leave this module.
//!
//! [`readline_key`] is the one definition of the readline editing set:
//! the main composer and the ask panels' one-line fields both dispatch
//! through it (P6 applies to every text field), layering their own
//! bindings around it.

use std::collections::BTreeMap;

use amux_ui::attachments::{ArtifactKind, DraftAttachment, Mention, MentionKind, format_mention};
use amux_ui::review::Review;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

/// The Unicode private-use range the composer draws token slots from.
///
/// A token is exactly one char in the draft, so every existing cursor,
/// deletion and kill rule applies to it unchanged: one Left steps over
/// it, one Backspace removes all of it, and a kill carries it whole.
/// Private-use code points carry no textual meaning, so they cannot
/// collide with anything a person types or pastes.
const SLOT_FIRST: char = '\u{e000}';
const SLOT_LAST: char = '\u{f8ff}';

/// A bracketed paste this many lines long, or this many chars long, is
/// long enough to bury the sentence around it, so it becomes one atomic
/// token instead of filling the draft.
pub const PASTE_TOKEN_LINES: usize = 8;
pub const PASTE_TOKEN_CHARS: usize = 1000;

/// The `name` a pasted-text attachment carries into the feed. Pasted text
/// has no source filename, and the mention format requires a name.
const PASTED_NAME: &str = "pasted text";

fn is_slot(c: char) -> bool {
    (SLOT_FIRST..=SLOT_LAST).contains(&c)
}

/// One atomic mention occupying a single slot char in the draft.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Token {
    pub slot: char,
    /// Display-only: what the composer paints in place of the slot.
    pub label: String,
    pub attachment: TokenAttachment,
}

/// What a token will become when the draft is exported.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "token", rename_all = "snake_case")]
pub enum TokenAttachment {
    Command {
        name: String,
    },
    Artifact(DraftAttachment),
    Text {
        body: String,
        lines: u32,
    },
    Review,
    FrozenReview {
        mention: Box<Mention>,
        diff: Option<DraftAttachment>,
    },
}

/// The label a token of this kind carries at this per-kind ordinal.
///
/// Numbering is per kind and in draft order, so the labels renumber when a
/// token is deleted: what the person sees always counts what is there.
pub fn token_label(
    kind: &TokenAttachment,
    ordinal: usize,
    name: &str,
    detail: Option<&str>,
) -> String {
    match kind {
        TokenAttachment::Command { name } => format!("[/{name}]"),
        TokenAttachment::Artifact(attachment) => match attachment.kind {
            ArtifactKind::Image => match detail {
                Some(detail) => format!("[Image #{ordinal} \u{b7} {detail}]"),
                None => format!("[Image #{ordinal}]"),
            },
            _ => format!("[File #{ordinal} {name}]"),
        },
        TokenAttachment::Text { lines, .. } => {
            let detail = detail.map(str::to_owned).unwrap_or_else(|| {
                let unit = if *lines == 1 { "line" } else { "lines" };
                format!("{lines} {unit}")
            });
            format!("[Pasted #{ordinal} \u{b7} {detail}]")
        }
        TokenAttachment::FrozenReview { .. } => "[Review]".to_string(),
        TokenAttachment::Review => match detail {
            Some(detail) => format!("[Review \u{b7} {detail}]"),
            None => "[Review]".to_string(),
        },
    }
}

/// The editable draft with a char-indexed cursor and the single-slot kill
/// buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Composer {
    chars: Vec<char>,
    /// Char index in `0..=chars.len()`.
    cursor: usize,
    /// Last kill (single slot, last kill wins). Empty kills never clobber
    /// it — a Ctrl+U at line start must not eat the yankable text.
    kill: Option<String>,
    /// Boxed so a token-free draft stays small: the ask panels embed several
    /// composers and the chat view enums are size-linted.
    tokens: Box<TokenState>,
}

/// The tokens a draft owns, live and set aside.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TokenState {
    /// The last send-cleared draft, verbatim — slot chars and all, which
    /// is what a failed send has to put back. The command carries the
    /// EXPORTED text, and canonical elements are not a draft.
    text: String,
    /// Every live token by slot. Text order is read off `chars`, so the
    /// draft's attachment list is derived and never stored twice.
    live: BTreeMap<char, Token>,
    /// Tokens of the last send-clear. A send that fails restores its text,
    /// and the tokens have to come back with it, so `clear_for_send` sets
    /// them aside exactly as a kill sets text aside.
    sent: BTreeMap<char, Token>,
}

impl Composer {
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The draft with the `▌` cursor glyph inserted at the cursor position
    /// — the renderer's display form (the wireframes' visual language).
    pub fn display_with_cursor(&self) -> String {
        let (display, cursor) = self.display_form();
        let mut out: String = display.chars().take(cursor).collect();
        out.push('▌');
        out.extend(display.chars().skip(cursor));
        out
    }

    /// The painted draft — every token slot replaced by its label — and the
    /// cursor's char index within it. Labels are wider than their one slot
    /// char, so display positions and draft positions are different spaces
    /// and only this function converts between them.
    fn display_form(&self) -> (String, usize) {
        let mut out = String::new();
        let mut cursor = 0usize;
        for (index, c) in self.chars.iter().enumerate() {
            if index == self.cursor {
                cursor = out.chars().count();
            }
            match self.tokens.live.get(c) {
                Some(token) => out.push_str(&token.label),
                None => out.push(*c),
            }
        }
        if self.cursor >= self.chars.len() {
            cursor = out.chars().count();
        }
        (out, cursor)
    }

    /// Hard-wrapped display rows plus the row holding the cursor. This is
    /// terminal layout shared by both native chat composers; editing remains
    /// renderer-local and protocol-independent.
    pub(crate) fn display_rows(&self, width: usize) -> (Vec<String>, usize) {
        use unicode_segmentation::UnicodeSegmentation;

        let width = width.max(1);
        let display = self.display_with_cursor();
        let cursor_pos = self.display_form().1;
        let mut rows = Vec::new();
        let mut cursor_row = 0usize;
        let mut chars_seen = 0usize;
        for logical in display.split('\n') {
            let mut row = String::new();
            let mut row_cells = 0usize;
            for grapheme in logical.graphemes(true) {
                let cells = crate::render::str_width(grapheme);
                if row_cells + cells > width && !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                    row_cells = 0;
                }
                if cursor_pos >= chars_seen && cursor_pos < chars_seen + grapheme.chars().count() {
                    cursor_row = rows.len();
                }
                row.push_str(grapheme);
                row_cells += cells;
                chars_seen += grapheme.chars().count();
            }
            rows.push(row);
            chars_seen += 1;
        }
        (rows, cursor_row)
    }

    /// Clear for a dispatched send. Not a kill: the sent text lives on as
    /// the optimistic echo (and is restored from the finished-op failure
    /// fact if the send fails), so it must not clobber the kill slot.
    pub fn clear_for_send(&mut self) {
        self.cursor = 0;
        self.tokens.text = std::mem::take(&mut self.chars).iter().collect();
        self.tokens.sent = std::mem::take(&mut self.tokens.live);
        // The tokens the kill slot was keeping alive belong to the sent
        // draft now. Their slot chars would yank back as invisible text
        // with no attachment behind them, so the kill keeps only the
        // words.
        self.kill = self.kill.take().and_then(|kill| {
            let text: String = kill.chars().filter(|c| !is_slot(*c)).collect();
            (!text.is_empty()).then_some(text)
        });
        self.prune();
    }

    /// Restore a failed send's text (C5: the draft resurfaces with the
    /// failure stated). Cursor lands at the end.
    pub fn restore(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
        for c in &self.chars {
            if let Some(token) = self.tokens.sent.get(c).cloned() {
                self.tokens.live.insert(*c, token);
            }
        }
        self.prune();
    }

    /// Put back the exact draft the last send cleared, tokens included.
    ///
    /// Returns false when there is nothing set aside, so the caller can
    /// fall back to the text the failed command carried.
    pub fn restore_sent(&mut self) -> bool {
        if self.tokens.text.is_empty() {
            return false;
        }
        let text = self.tokens.text.clone();
        self.restore(&text);
        true
    }

    // --- insertion ---------------------------------------------------------

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Ctrl+J — the guaranteed newline in any terminal.
    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    /// Yank and programmatic insertion of whole strings.
    pub fn insert_str(&mut self, text: &str) {
        for c in text.chars() {
            self.insert(c);
        }
    }

    /// Bracketed paste: literal text insertion — newlines and tabs land in
    /// the draft, never as bindings (see [`sanitize_paste`]).
    pub fn paste(&mut self, text: &str) {
        self.insert_str(&sanitize_paste(text));
    }

    /// A bracketed paste routed by size: text long enough to bury the
    /// sentence around it becomes one atomic token, shorter text lands as
    /// characters. Returns the token's slot when one was made.
    pub fn paste_or_attach(&mut self, text: &str) -> Option<char> {
        let body = sanitize_paste(text);
        let lines = body.lines().count().max(1);
        if lines < PASTE_TOKEN_LINES && body.chars().count() < PASTE_TOKEN_CHARS {
            self.insert_str(&body);
            return None;
        }
        Some(self.insert_token(
            String::new(),
            TokenAttachment::Text {
                body,
                lines: lines as u32,
            },
        ))
    }

    /// Inserts an artifact token at the cursor; `renumber` gives its label.
    pub fn attach(&mut self, attachment: DraftAttachment) -> char {
        self.insert_token(String::new(), TokenAttachment::Artifact(attachment))
    }

    // --- deletion ----------------------------------------------------------

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
            self.prune();
        }
    }

    /// Ctrl+D — delete forward; never EOF, never quit (P6/P4).
    pub fn delete_forward(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
            self.prune();
        }
    }

    // --- motion ------------------------------------------------------------

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    /// Start of the current logical line (between newlines — ^U/^K scope,
    /// keybindings §5.6).
    fn line_start(&self) -> usize {
        self.chars[..self.cursor]
            .iter()
            .rposition(|c| *c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// End of the current logical line.
    fn line_end(&self) -> usize {
        self.chars[self.cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.chars.len())
    }

    /// Home / Ctrl+A-less line start (C-a is the chrome leader; Home
    /// serves, keybindings P6).
    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// End / Ctrl+E.
    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// ↑ / Ctrl+P — previous logical line, same column (clamped).
    pub fn up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let col = self.cursor - start;
        let prev_start = self.chars[..start - 1]
            .iter()
            .rposition(|c| *c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_len = start - 1 - prev_start;
        self.cursor = prev_start + col.min(prev_len);
    }

    /// ↓ / Ctrl+N — next logical line, same column (clamped).
    pub fn down(&mut self) {
        let end = self.line_end();
        if end == self.chars.len() {
            return;
        }
        let col = self.cursor - self.line_start();
        let next_start = end + 1;
        let next_len = self.chars[next_start..]
            .iter()
            .position(|c| *c == '\n')
            .unwrap_or(self.chars.len() - next_start);
        self.cursor = next_start + col.min(next_len);
    }

    /// Ctrl+← — word left (ext tier; convenience, never the only path).
    pub fn word_left(&mut self) {
        let mut i = self.cursor;
        while i > 0 && !self.chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && self.chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        self.cursor = i;
    }

    /// Ctrl+→ — word right.
    pub fn word_right(&mut self) {
        let len = self.chars.len();
        let mut i = self.cursor;
        while i < len && !self.chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < len && self.chars[i].is_alphanumeric() {
            i += 1;
        }
        self.cursor = i;
    }

    // --- kills (single slot; every clearing key is reversible, P4) ---------

    fn kill_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let killed: String = self.chars[start..end].iter().collect();
        self.chars.drain(start..end);
        self.cursor = start;
        self.kill = Some(killed);
        self.prune();
    }

    /// Ctrl+U — kill to line start (strict readline; ^C owns clear-all,
    /// keybindings §2.2).
    pub fn kill_to_line_start(&mut self) {
        self.kill_range(self.line_start(), self.cursor);
    }

    /// Ctrl+K — kill to line end.
    pub fn kill_to_line_end(&mut self) {
        self.kill_range(self.cursor, self.line_end());
    }

    /// Ctrl+W — delete word backward (readline unix-word-rubout:
    /// whitespace, then the word).
    pub fn kill_word_back(&mut self) {
        let mut start = self.cursor;
        while start > 0 && self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.kill_range(start, self.cursor);
    }

    /// Ctrl+C on a non-empty draft — abandon the whole draft as a kill
    /// (keybindings §2.1: the clearing press; yankable, so never
    /// destructive).
    pub fn kill_all(&mut self) {
        self.kill_range(0, self.chars.len());
    }

    /// Ctrl+Y — yank the last kill at the cursor.
    ///
    /// The kill slot keeps its text so it can be yanked repeatedly, but an
    /// attachment is not text: a token already back in the draft must not
    /// come back a second time and send the same file twice.
    pub fn yank(&mut self) {
        let Some(kill) = self.kill.clone() else {
            return;
        };
        let text: String = kill
            .chars()
            .filter(|c| !is_slot(*c) || !self.chars.contains(c))
            .collect();
        if text.is_empty() {
            return;
        }
        self.insert_str(&text);
        self.renumber();
    }

    // --- tokens (atomic mentions) -----------------------------------------

    /// Inserts an atomic token at the cursor and returns its slot char.
    ///
    /// The supplied label is what a Review token keeps: only the chat knows
    /// its comment count. Artifact and text labels are renumbered here and
    /// after every deletion.
    pub fn insert_token(&mut self, label: String, attachment: TokenAttachment) -> char {
        let slot = self.free_slot();
        self.tokens.live.insert(
            slot,
            Token {
                slot,
                label,
                attachment,
            },
        );
        self.insert(slot);
        self.renumber();
        slot
    }

    /// Every live token in draft order.
    pub fn tokens(&self) -> Vec<&Token> {
        self.chars
            .iter()
            .filter_map(|c| self.tokens.live.get(c))
            .collect()
    }

    /// The token occupying a slot, if it is still in the draft.
    pub fn token(&self, slot: char) -> Option<&Token> {
        self.tokens.live.get(&slot)
    }

    /// Replaces a token's label — the review's label counts its comments,
    /// which change while the draft stands.
    pub fn set_token_label(&mut self, slot: char, label: String) {
        if let Some(token) = self.tokens.live.get_mut(&slot) {
            token.label = label;
        }
    }

    /// Takes a token out of the draft, as though it had been deleted.
    ///
    /// Unlike a kill this leaves nothing yankable: the caller is discarding
    /// what the token stood for, so bringing the token back would leave a
    /// mention with nothing behind it.
    pub fn remove_token(&mut self, slot: char) {
        let Some(index) = self.chars.iter().position(|c| *c == slot) else {
            return;
        };
        self.chars.remove(index);
        if self.cursor > index {
            self.cursor -= 1;
        }
        self.tokens.live.remove(&slot);
        if let Some(kill) = self.kill.take() {
            let text: String = kill.chars().filter(|c| *c != slot).collect();
            self.kill = (!text.is_empty()).then_some(text);
        }
        self.prune();
    }

    /// Puts the cursor immediately after a token, where Enter sends.
    ///
    /// Leaving the review page lands here rather than on the slot: on it,
    /// Enter would resume the page the person just left.
    pub fn cursor_after_token(&mut self, slot: char) -> bool {
        match self.chars.iter().position(|c| *c == slot) {
            Some(index) => {
                self.cursor = index + 1;
                true
            }
            None => false,
        }
    }

    /// The review token's slot when the cursor sits ON it.
    ///
    /// Enter there resumes the review; after `q` the cursor sits just after
    /// the token, where Enter sends, so a cursor immediately after the slot
    /// is deliberately not a match.
    pub fn review_token_at_cursor(&self) -> Option<char> {
        let slot = *self.chars.get(self.cursor)?;
        match self.tokens.live.get(&slot)?.attachment {
            TokenAttachment::Review => Some(slot),
            _ => None,
        }
    }

    /// Restore live binary resources after the pure draft restoration step.
    /// Resource bytes do not affect rendering and never enter the UI trace.
    pub(crate) fn hydrate_queued_attachments(
        &mut self,
        get: impl Fn(&amux_ui::ArtifactId) -> Option<std::sync::Arc<[u8]>>,
    ) {
        for token in self.tokens.live.values_mut() {
            let attachment = match &mut token.attachment {
                TokenAttachment::Artifact(attachment) => Some(attachment),
                TokenAttachment::FrozenReview { diff, .. } => diff.as_mut(),
                _ => None,
            };
            if let Some(attachment) = attachment
                && attachment.bytes.is_none()
            {
                attachment.bytes = get(&attachment.id);
            }
        }
    }

    pub fn restore_queued(&mut self, draft: &amux_ui::Draft) {
        for segment in &draft.segments {
            match segment {
                amux_ui::DraftSegment::CommandToken { name } => {
                    self.insert_token(
                        format!("[/{name}]"),
                        TokenAttachment::Command { name: name.clone() },
                    );
                }
                amux_ui::DraftSegment::Text { text } => {
                    self.restore_queued_text(text, &draft.attachments)
                }
            }
        }
    }

    fn restore_queued_text(&mut self, text: &str, attachments: &[DraftAttachment]) {
        for segment in amux_ui::split_mentions(text) {
            match segment {
                amux_ui::Segment::Prose(text) => self.insert_str(&text),
                amux_ui::Segment::Mention(mention) => match &mention.kind {
                    MentionKind::Image { id } | MentionKind::File { id } => {
                        if let Some(attachment) = attachments.iter().find(|a| &a.id == id) {
                            self.attach(attachment.clone());
                        } else {
                            self.insert_str(&format_mention(&mention));
                        }
                    }
                    MentionKind::Text { body, lines } => {
                        self.insert_token(
                            String::new(),
                            TokenAttachment::Text {
                                body: body.clone(),
                                lines: *lines,
                            },
                        );
                    }
                    MentionKind::Review { header, .. } => {
                        let diff = attachments.iter().find(|a| a.id == header.diff).cloned();
                        self.insert_token(
                            "[Review]".into(),
                            TokenAttachment::FrozenReview {
                                mention: Box::new(mention),
                                diff,
                            },
                        );
                    }
                },
            }
        }
    }

    /// The sendable draft: canonical elements in place of the tokens, plus
    /// the artifacts to store and pin, in draft order.
    ///
    /// The review element is rendered from the live draft review, so a
    /// review token with no review behind it exports nothing.
    pub fn export(&self, review: Option<&Review>) -> (String, Vec<DraftAttachment>) {
        let draft = self.export_draft(review);
        (draft.text(), draft.attachments)
    }

    pub fn export_draft(&self, review: Option<&Review>) -> amux_ui::Draft {
        let mut segments = Vec::new();
        let mut text = String::new();
        let mut attachments = Vec::new();
        for c in &self.chars {
            let Some(token) = self.tokens.live.get(c) else {
                text.push(*c);
                continue;
            };
            match &token.attachment {
                TokenAttachment::Command { name } => {
                    if !text.is_empty() {
                        segments.push(amux_ui::DraftSegment::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    segments.push(amux_ui::DraftSegment::CommandToken { name: name.clone() });
                }
                TokenAttachment::Artifact(attachment) => {
                    let id = attachment.id.clone();
                    let kind = match attachment.kind {
                        ArtifactKind::Image => MentionKind::Image { id },
                        _ => MentionKind::File { id },
                    };
                    text.push_str(&format_mention(&Mention {
                        kind,
                        name: attachment.name.clone(),
                        size: Some(attachment.size),
                        path: None,
                    }));
                    attachments.push(attachment.clone());
                }
                TokenAttachment::Text { body, lines } => {
                    text.push_str(&format_mention(&Mention {
                        kind: MentionKind::Text {
                            body: body.clone(),
                            lines: *lines,
                        },
                        name: PASTED_NAME.to_string(),
                        size: None,
                        path: None,
                    }));
                }
                TokenAttachment::FrozenReview { mention, diff } => {
                    text.push_str(&format_mention(mention));
                    attachments.extend(diff.clone());
                }
                TokenAttachment::Review => {
                    let Some(review) = review else {
                        continue;
                    };
                    let (mention, attachment) = amux_ui::review_mention(review);
                    text.push_str(&format_mention(&mention));
                    attachments.push(attachment);
                }
            }
        }
        if !text.is_empty() {
            segments.push(amux_ui::DraftSegment::Text { text });
        }
        amux_ui::Draft {
            segments,
            attachments,
        }
    }

    /// The lowest slot not spoken for by the draft or the send stash.
    fn free_slot(&self) -> char {
        (SLOT_FIRST..=SLOT_LAST)
            .find(|slot| {
                !self.tokens.live.contains_key(slot) && !self.tokens.sent.contains_key(slot)
            })
            .unwrap_or(SLOT_LAST)
    }

    /// Drops tokens no longer reachable, then renumbers what is left.
    ///
    /// A token killed out of the draft stays alive while the kill slot holds
    /// its char, so Ctrl+Y brings back text and tokens together.
    fn prune(&mut self) {
        let kill = self.kill.clone().unwrap_or_default();
        let dropped: Vec<char> = self
            .tokens
            .live
            .keys()
            .copied()
            .filter(|slot| !self.chars.contains(slot) && !kill.contains(*slot))
            .collect();
        for slot in dropped {
            self.tokens.live.remove(&slot);
        }
        self.renumber();
    }

    /// Recomputes the per-kind ordinal in every artifact and text label.
    fn renumber(&mut self) {
        let order: Vec<char> = self
            .chars
            .iter()
            .copied()
            .filter(|c| self.tokens.live.contains_key(c))
            .collect();
        let mut images = 0usize;
        let mut files = 0usize;
        let mut pastes = 0usize;
        for slot in order {
            let Some(token) = self.tokens.live.get(&slot) else {
                continue;
            };
            let (ordinal, name) = match &token.attachment {
                TokenAttachment::Artifact(attachment) => match attachment.kind {
                    ArtifactKind::Image => {
                        images += 1;
                        (images, attachment.name.clone())
                    }
                    _ => {
                        files += 1;
                        (files, attachment.name.clone())
                    }
                },
                TokenAttachment::Text { .. } => {
                    pastes += 1;
                    (pastes, PASTED_NAME.to_string())
                }
                // The review label counts comments, which only the chat knows.
                TokenAttachment::Command { .. }
                | TokenAttachment::Review
                | TokenAttachment::FrozenReview { .. } => continue,
            };
            let label = token_label(&token.attachment, ordinal, &name, None);
            if let Some(token) = self.tokens.live.get_mut(&slot) {
                token.label = label;
            }
        }
    }
}

/// Pasted text made safe for the draft and for the wire.
///
/// CRLF/CR normalize to LF (matching the send path's `normalize_prompt`)
/// and tabs expand to spaces at insertion (mirroring the reader's
/// tabs-expand-before-width-math policy), so the draft stays sendable —
/// the C6 encoder refuses control bytes other than `\n`. Any other control
/// character is stripped: it would be invisible in the composer AND
/// unsendable, a trap in both directions. Private-use chars go too: they
/// are the token slots, and pasted text must never forge one.
fn sanitize_paste(text: &str) -> String {
    const TAB: &str = "    ";
    let mut out = String::with_capacity(text.len());
    for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str(TAB),
            c if c.is_control() || is_slot(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// The shared readline editing set (P6): cursor and word motion, kills,
/// yank, printable insertion. Returns `true` when the key was consumed
/// as editing; keys outside the set — submit, stage navigation,
/// multiline row motion, scroll — return `false` for the caller's own
/// bindings.
pub(crate) fn readline_key(field: &mut Composer, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl => {
            match c {
                'b' => field.left(),
                'f' => field.right(),
                'e' => field.end(),
                'w' => field.kill_word_back(),
                'u' => field.kill_to_line_start(),
                'k' => field.kill_to_line_end(),
                'd' => field.delete_forward(),
                'y' => field.yank(),
                _ => return false,
            }
            true
        }
        KeyCode::Left if ctrl => {
            field.word_left();
            true
        }
        KeyCode::Right if ctrl => {
            field.word_right();
            true
        }
        KeyCode::Left => {
            field.left();
            true
        }
        KeyCode::Right => {
            field.right();
            true
        }
        KeyCode::Home => {
            field.home();
            true
        }
        KeyCode::End => {
            field.end();
            true
        }
        KeyCode::Backspace => {
            field.backspace();
            true
        }
        KeyCode::Delete => {
            field.delete_forward();
            true
        }
        // Printables belong to the draft (P2) — `q`, `f`, digits, `?`
        // all type.
        KeyCode::Char(c) => {
            field.insert(c);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composer(text: &str, cursor: usize) -> Composer {
        let mut c = Composer::default();
        c.insert_str(text);
        c.cursor = cursor;
        c
    }

    #[test]
    fn insertion_and_newline_build_a_multiline_draft() {
        let mut c = Composer::default();
        c.insert_str("fix the");
        c.insert_newline();
        c.insert_str("tests");
        assert_eq!(c.text(), "fix the\ntests");
        assert_eq!(c.display_with_cursor(), "fix the\ntests▌");
    }

    #[test]
    fn cursor_motion_moves_by_chars_and_lines() {
        let mut c = composer("ab\ncdef", 7);
        c.home();
        assert_eq!(c.cursor(), 3);
        c.end();
        assert_eq!(c.cursor(), 7);
        c.up();
        assert_eq!(c.cursor(), 2, "column clamps to the shorter line");
        c.down();
        assert_eq!(c.cursor(), 5, "column carries into the longer line");
        assert_eq!(c.display_with_cursor(), "ab\ncd▌ef");
    }

    #[test]
    fn word_motion_jumps_word_boundaries() {
        let mut c = composer("send the prompt", 15);
        c.word_left();
        assert_eq!(c.cursor(), 9);
        c.word_left();
        assert_eq!(c.cursor(), 5);
        c.word_right();
        assert_eq!(c.cursor(), 8);
    }

    #[test]
    fn kills_land_in_the_single_slot_and_yank_restores() {
        let mut c = composer("hello world", 11);
        c.kill_word_back();
        assert_eq!(c.text(), "hello ");
        c.yank();
        assert_eq!(c.text(), "hello world");
        // The slot holds the LAST kill only.
        c.kill_to_line_start();
        assert_eq!(c.text(), "");
        c.yank();
        assert_eq!(c.text(), "hello world");
    }

    #[test]
    fn ctrl_u_and_ctrl_k_scope_to_the_current_logical_line() {
        let mut c = composer("keep\nkill this line\nkeep", 10);
        c.kill_to_line_end();
        assert_eq!(c.text(), "keep\nkill \nkeep");
        c.kill_to_line_start();
        assert_eq!(c.text(), "keep\n\nkeep");
        c.yank();
        assert_eq!(c.text(), "keep\nkill \nkeep", "yank restores the last kill");
    }

    #[test]
    fn clear_all_is_a_kill_and_never_destructive() {
        let mut c = composer("draft in progress", 5);
        c.kill_all();
        assert!(c.is_empty());
        c.yank();
        assert_eq!(c.text(), "draft in progress");
        assert_eq!(c.cursor(), 17);
    }

    #[test]
    fn empty_kills_never_clobber_the_slot() {
        let mut c = composer("precious", 8);
        c.kill_all(); // slot = "precious", draft empty
        c.kill_to_line_start(); // nothing to kill — must not clobber
        c.kill_word_back(); // nothing to kill — must not clobber
        c.kill_to_line_end(); // nothing to kill — must not clobber
        c.yank();
        assert_eq!(c.text(), "precious", "the slot survived the empty kills");
    }

    #[test]
    fn delete_forward_and_backspace_edit_around_the_cursor() {
        let mut c = composer("abcd", 2);
        c.delete_forward();
        assert_eq!(c.text(), "abd");
        c.backspace();
        assert_eq!(c.text(), "ad");
        assert_eq!(c.cursor(), 1);
    }

    #[test]
    fn clear_for_send_keeps_the_kill_slot() {
        let mut c = composer("kill me", 7);
        c.kill_all();
        c.yank();
        c.clear_for_send();
        assert!(c.is_empty());
        c.yank();
        assert_eq!(c.text(), "kill me", "send-clear is not a kill");
    }

    #[test]
    fn unicode_drafts_edit_by_chars_not_bytes() {
        let mut c = Composer::default();
        c.insert_str("héllo ▲");
        assert_eq!(c.cursor(), 7);
        c.backspace();
        assert_eq!(c.text(), "héllo ");
        c.word_left();
        assert_eq!(c.cursor(), 0);
        c.delete_forward();
        assert_eq!(c.text(), "éllo ");
    }

    #[test]
    fn restore_places_the_cursor_at_the_end() {
        let mut c = Composer::default();
        c.restore("failed send text");
        assert_eq!(c.display_with_cursor(), "failed send text▌");
    }

    #[test]
    fn paste_with_newlines_grows_the_draft_without_sending() {
        let mut c = Composer::default();
        c.paste("line one\nline two\nline three");
        assert_eq!(c.text(), "line one\nline two\nline three");
        // Sendable as one prompt: the validator accepts printable + \n.
        assert!(amux_ui::claude::answer::check_prompt(&c.text()).is_ok());
    }

    #[test]
    fn paste_normalizes_crlf_and_lone_cr_to_newlines() {
        let mut c = Composer::default();
        c.paste("a\r\nb\rc");
        assert_eq!(c.text(), "a\nb\nc");
    }

    #[test]
    fn paste_expands_tabs_and_the_draft_stays_sendable() {
        let mut c = Composer::default();
        c.paste("indent:\n\tcode line");
        assert_eq!(c.text(), "indent:\n    code line");
        assert!(
            amux_ui::claude::answer::check_prompt(&c.text()).is_ok(),
            "expanded tabs pass the C6 control-byte validator"
        );
    }

    #[test]
    fn paste_strips_other_control_bytes() {
        let mut c = Composer::default();
        // An ESC mid-paste could otherwise form the bracketed-paste
        // terminator inside the injected program (Phase 3's P2 finding).
        c.paste("safe\x1b[201~text\x07");
        assert_eq!(c.text(), "safe[201~text");
        assert!(amux_ui::claude::answer::check_prompt(&c.text()).is_ok());
    }

    #[test]
    fn paste_lands_at_the_cursor() {
        let mut c = composer("ab", 1);
        c.paste("XY");
        assert_eq!(c.display_with_cursor(), "aXY▌b");
    }
}

#[cfg(test)]
mod tokens {
    use amux_ui::review::{Review, anchor, parse_patch};
    use amux_ui::{BaseIdentity, DiffBase, DiffFile};

    use super::*;

    const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
 one
-two
+three";

    fn image(name: &str, bytes: &[u8]) -> TokenAttachment {
        TokenAttachment::Artifact(DraftAttachment::from_bytes(
            ArtifactKind::Image,
            name,
            "image/png",
            bytes.to_vec(),
        ))
    }

    fn file(name: &str, bytes: &[u8]) -> TokenAttachment {
        TokenAttachment::Artifact(DraftAttachment::from_bytes(
            ArtifactKind::File,
            name,
            "text/plain",
            bytes.to_vec(),
        ))
    }

    fn pasted(body: &str) -> TokenAttachment {
        TokenAttachment::Text {
            body: body.to_string(),
            lines: body.lines().count() as u32,
        }
    }

    fn review() -> Review {
        let identity = BaseIdentity {
            base: DiffBase::WorkingTree,
            head: "4f2a9c1".into(),
            merge_base: None,
            blobs: vec![("src/lib.rs".into(), "2222222".into())],
        };
        let files = vec![DiffFile {
            path: "src/lib.rs".into(),
            added: 1,
            removed: 1,
        }];
        let doc = parse_patch(PATCH, identity, &files).unwrap();
        let row = amux_ui::review::RowRef { file: 0, row: 2 };
        let mut review = Review::new(
            doc,
            "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
        );
        review.add(anchor(review.document(), row, row).unwrap(), "why?".into());
        review
    }

    #[test]
    fn a_token_is_one_char_so_cursor_motion_steps_over_it() {
        let mut c = Composer::default();
        c.insert_str("look ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.insert_str(" here");
        assert_eq!(c.cursor(), 11);

        c.home();
        for _ in 0..5 {
            c.right();
        }
        assert_eq!(c.cursor(), 5, "the cursor stops before the token");
        c.right();
        assert_eq!(c.cursor(), 6, "one press clears the whole token");
        c.left();
        assert_eq!(c.cursor(), 5, "and one press back sits on it again");
        assert_eq!(c.display_with_cursor(), "look ▌[Image #1] here");
    }

    #[test]
    fn one_backspace_removes_a_whole_token_and_its_attachment() {
        let mut c = Composer::default();
        c.insert_str("a ");
        c.insert_token(String::new(), file("notes.md", b"notes"));
        assert_eq!(c.tokens().len(), 1);

        c.backspace();
        assert!(c.tokens().is_empty(), "the token left with its one char");
        assert_eq!(c.text(), "a ");
        let (text, attachments) = c.export(None);
        assert_eq!(text, "a ");
        assert!(attachments.is_empty(), "its attachment left with it");
    }

    #[test]
    fn delete_forward_removes_the_token_under_the_cursor() {
        let mut c = Composer::default();
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.insert_str("tail");
        c.home();
        c.delete_forward();
        assert_eq!(c.text(), "tail");
        assert!(c.tokens().is_empty());
    }

    #[test]
    fn export_renders_the_elements_and_lists_attachments_in_text_order() {
        let mut c = Composer::default();
        c.insert_str("see ");
        c.insert_token(String::new(), image("shot.png", b"png bytes"));
        c.insert_str(" and ");
        c.insert_token(String::new(), file("notes.md", b"notes"));
        c.insert_str(" please");

        let (text, attachments) = c.export(None);
        assert!(text.starts_with("see <amux-attachment "), "{text}");
        assert!(text.contains("kind=\"image\" name=\"shot.png\""), "{text}");
        assert!(text.contains("/> and <amux-attachment "), "{text}");
        assert!(text.contains("kind=\"file\" name=\"notes.md\""), "{text}");
        assert!(text.ends_with("/> please"), "{text}");
        assert!(
            !text.chars().any(is_slot),
            "no slot char survives the export"
        );
        let names: Vec<&str> = attachments
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect();
        assert_eq!(names, vec!["shot.png", "notes.md"]);
        assert!(
            attachments
                .iter()
                .all(|attachment| attachment.bytes.is_some()),
            "the live bytes ride along to be stored"
        );

        // Every element the export writes parses back as one mention.
        let mentions = amux_ui::split_mentions(&text)
            .into_iter()
            .filter(|segment| matches!(segment, amux_ui::Segment::Mention(_)))
            .count();
        assert_eq!(mentions, 2);
    }

    #[test]
    fn a_pasted_text_token_exports_its_body_without_an_artifact() {
        let mut c = Composer::default();
        c.insert_token(String::new(), pasted("one\ntwo\nthree"));
        let (text, attachments) = c.export(None);
        assert!(text.contains("kind=\"text\""), "{text}");
        assert!(text.contains("lines=\"3\""), "{text}");
        assert!(text.contains("one\ntwo\nthree"), "{text}");
        assert!(attachments.is_empty(), "pasted text is not an artifact");
    }

    #[test]
    fn labels_renumber_per_kind_after_a_deletion() {
        let mut c = Composer::default();
        c.insert_token(String::new(), image("one.png", b"1"));
        c.insert_token(String::new(), file("notes.md", b"n"));
        c.insert_token(String::new(), image("two.png", b"2"));
        c.insert_token(String::new(), pasted("a\nb"));
        let labels: Vec<&str> = c.tokens().iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "[Image #1]",
                "[File #1 notes.md]",
                "[Image #2]",
                "[Pasted #1 · 2 lines]"
            ]
        );

        // Remove the first image; the second becomes #1.
        c.home();
        c.delete_forward();
        let labels: Vec<&str> = c.tokens().iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["[File #1 notes.md]", "[Image #1]", "[Pasted #1 · 2 lines]"]
        );
    }

    #[test]
    fn the_review_token_answers_only_with_the_cursor_on_its_slot() {
        let mut c = Composer::default();
        c.insert_str("ship it ");
        let slot = c.insert_token("[Review · 1 comment]".into(), TokenAttachment::Review);
        assert_eq!(
            c.review_token_at_cursor(),
            None,
            "after `q` the cursor sits past the token, where Enter sends"
        );
        c.left();
        assert_eq!(
            c.review_token_at_cursor(),
            Some(slot),
            "on the slot Enter resumes"
        );
        c.left();
        assert_eq!(c.review_token_at_cursor(), None);
    }

    #[test]
    fn an_image_token_is_never_mistaken_for_the_review_token() {
        let mut c = Composer::default();
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.home();
        assert_eq!(c.review_token_at_cursor(), None);
    }

    #[test]
    fn exporting_a_review_writes_its_element_and_pins_its_diff() {
        let review = review();
        let mut c = Composer::default();
        c.insert_str("notes ");
        c.insert_token("[Review · 1 comment]".into(), TokenAttachment::Review);

        let (text, attachments) = c.export(Some(&review));
        assert!(
            text.starts_with("notes <amux-attachment kind=\"review\""),
            "{text}"
        );
        assert!(text.contains("why?"), "the comment travels in the body");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, review.header().diff);
        assert_eq!(attachments[0].kind, ArtifactKind::Diff);
        assert!(
            attachments[0].bytes.is_none(),
            "the diff is already stored; it only needs pinning"
        );

        // With no review behind it the token exports nothing at all.
        let (text, attachments) = c.export(None);
        assert_eq!(text, "notes ");
        assert!(attachments.is_empty());
    }

    #[test]
    fn kill_all_and_yank_round_trip_text_and_tokens() {
        let mut c = Composer::default();
        c.insert_str("look at ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.insert_str(" and ");
        c.insert_token(String::new(), file("notes.md", b"notes"));
        let before = c.export(None);

        c.kill_all();
        assert!(c.is_empty());
        assert!(
            c.tokens().is_empty(),
            "a cleared draft holds no attachments"
        );

        c.yank();
        assert_eq!(c.export(None), before, "text and tokens came back together");
        let labels: Vec<&str> = c.tokens().iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["[Image #1]", "[File #1 notes.md]"]);
    }

    #[test]
    fn a_send_leaves_no_yankable_slot_char_behind() {
        let mut c = Composer::default();
        c.insert_str("see ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.kill_all();

        c.insert_str("a later draft");
        c.clear_for_send();
        c.insert_str("next ");
        c.yank();

        assert_eq!(c.text(), "next see ", "only the words came back");
        assert!(
            !c.text().chars().any(is_slot),
            "no slot char without a live token"
        );
        assert!(c.tokens().is_empty());
        let (text, attachments) = c.export(None);
        assert!(
            !text.chars().any(is_slot),
            "export carries no private-use char: {text:?}"
        );
        assert!(attachments.is_empty());
    }

    #[test]
    fn yanking_a_killed_token_twice_attaches_it_once() {
        let mut c = Composer::default();
        c.insert_str("see ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        c.kill_all();

        c.yank();
        c.yank();

        assert_eq!(
            c.text(),
            "see \u{e000}see ",
            "the second yank repeated the words, not the token"
        );
        assert_eq!(c.tokens().len(), 1);
        let (text, attachments) = c.export(None);
        assert_eq!(attachments.len(), 1, "one attachment, not two");
        assert_eq!(
            text.matches("shot.png").count(),
            1,
            "the mention appears once: {text:?}"
        );
    }

    #[test]
    fn a_failed_send_restores_the_draft_with_its_tokens() {
        let mut c = Composer::default();
        c.insert_str("see ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        let text = c.text();
        let before = c.export(None);

        c.clear_for_send();
        assert!(c.tokens().is_empty());
        c.restore(&text);
        assert_eq!(c.export(None), before);
    }

    #[test]
    fn a_paste_can_never_forge_a_token_slot() {
        let mut c = Composer::default();
        c.insert_token(String::new(), image("shot.png", b"png"));
        let slot = c.tokens()[0].slot;
        c.paste(&format!("x{slot}y"));
        assert_eq!(
            c.text().matches(slot).count(),
            1,
            "the pasted slot was stripped"
        );
        assert_eq!(c.tokens().len(), 1);
    }

    #[test]
    fn display_paints_labels_and_wraps_on_them() {
        let mut c = Composer::default();
        c.insert_str("look ");
        c.insert_token(String::new(), image("shot.png", b"png"));
        let (rows, cursor_row) = c.display_rows(9);
        assert_eq!(rows, vec!["look [Ima", "ge #1]▌"]);
        assert_eq!(cursor_row, 1, "the cursor row follows the painted label");
    }
}

#[cfg(test)]
#[test]
fn provider_commands_composer_preserves_and_atomically_removes_restored_token() {
    let draft = amux_ui::Draft {
        segments: vec![
            amux_ui::DraftSegment::CommandToken {
                name: "review".into(),
            },
            amux_ui::DraftSegment::Text {
                text: " changes".into(),
            },
        ],
        attachments: vec![],
    };
    let mut composer = Composer::default();
    composer.restore_queued(&draft);
    assert_eq!(composer.export_draft(None), draft);
    let token = composer.tokens()[0];
    assert_eq!(token.label, "[/review]");
    for _ in 0.." changes".len() {
        composer.left();
    }
    composer.backspace();
    assert_eq!(
        composer.export_draft(None),
        amux_ui::Draft::plain(" changes", vec![])
    );
    assert!(composer.tokens().is_empty());
}
