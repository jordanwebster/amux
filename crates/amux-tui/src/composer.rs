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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The editable draft with a char-indexed cursor and the single-slot kill
/// buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    chars: Vec<char>,
    /// Char index in `0..=chars.len()`.
    cursor: usize,
    /// Last kill (single slot, last kill wins). Empty kills never clobber
    /// it — a Ctrl+U at line start must not eat the yankable text.
    kill: Option<String>,
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
        let mut out: String = self.chars[..self.cursor].iter().collect();
        out.push('▌');
        out.extend(&self.chars[self.cursor..]);
        out
    }

    /// Hard-wrapped display rows plus the row holding the cursor. This is
    /// terminal layout shared by both native chat composers; editing remains
    /// renderer-local and protocol-independent.
    pub(crate) fn display_rows(&self, width: usize) -> (Vec<String>, usize) {
        use unicode_segmentation::UnicodeSegmentation;

        let width = width.max(1);
        let display = self.display_with_cursor();
        let cursor_pos = self.cursor();
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
        self.chars.clear();
        self.cursor = 0;
    }

    /// Restore a failed send's text (C5: the draft resurfaces with the
    /// failure stated). Cursor lands at the end.
    pub fn restore(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
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
    /// the draft, never as bindings. CRLF/CR normalize to LF (matching the
    /// send path's `normalize_prompt`), tabs expand to spaces at insertion
    /// (mirroring the reader's tabs-expand-before-width-math policy) so
    /// the draft stays sendable — the C6 encoder refuses control bytes
    /// other than `\n`. Any other control character is stripped: it would
    /// be invisible in the composer AND unsendable, a trap in both
    /// directions.
    pub fn paste(&mut self, text: &str) {
        const TAB: &str = "    ";
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in normalized.chars() {
            match c {
                '\n' => self.insert('\n'),
                '\t' => self.insert_str(TAB),
                c if c.is_control() => {}
                c => self.insert(c),
            }
        }
    }

    // --- deletion ----------------------------------------------------------

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Ctrl+D — delete forward; never EOF, never quit (P6/P4).
    pub fn delete_forward(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
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
    pub fn yank(&mut self) {
        if let Some(kill) = self.kill.clone() {
            self.insert_str(&kill);
        }
    }
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
