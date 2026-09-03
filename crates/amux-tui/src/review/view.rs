//! Navigation and frame for one open review.

use std::collections::BTreeSet;

use amux_ui::DiffBase;
use amux_ui::review::{Review, RowRef};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use super::comments::{CommentEditor, Selection};
use crate::chat::diff::paint_rows;
use crate::render::{Theme, push_right, push_span};

/// Rows the frame spends on chrome: the two header lines, the rule under
/// them, the rule above the footer, and the footer itself.
const CHROME_ROWS: usize = 5;
/// The body indents past the cursor bar in column 0.
const BODY_LEFT: usize = 2;

/// What the host must do after a key reached the review page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewOutcome {
    /// The page consumed the key; nothing outside it changed.
    Handled,
    /// The key did not belong to the review page.
    Ignored,
    /// Leave the page, keeping the review and its comments.
    Close,
    /// Re-request the diff against another base and reopen over the result.
    SwitchBase(DiffBase),
    /// A comment was saved, edited or deleted; the draft's review token has
    /// to catch up.
    CommentsChanged,
}

/// The open file list, with the entry the keyboard is pointing at.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileOverlay {
    pub selected: usize,
}

/// One review as a screen: the frozen core plus where the reader is
/// looking at it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewView {
    pub(super) core: Review,
    pub(super) cursor: RowRef,
    pub(super) selection: Option<Selection>,
    pub(super) editor: Option<CommentEditor>,
    pub(super) folded: BTreeSet<usize>,
    pub(super) overlay: Option<FileOverlay>,
    pub(super) scroll: usize,
    /// The base `b` offers when the review is against the working tree.
    /// The page cannot know a repository's trunk, so its host names it.
    pub(super) branch: String,
    pub(super) width: u16,
    pub(super) height: u16,
}

impl ReviewView {
    /// Open at the first row of the first file.
    pub fn new(core: Review, branch: impl Into<String>) -> Self {
        Self {
            core,
            cursor: RowRef { file: 0, row: 0 },
            selection: None,
            editor: None,
            folded: BTreeSet::new(),
            overlay: None,
            scroll: 0,
            branch: branch.into(),
            width: 80,
            height: 24,
        }
    }

    /// The core, for export, the token label, and comment editing.
    pub fn review(&self) -> &Review {
        &self.core
    }

    pub fn review_mut(&mut self) -> &mut Review {
        &mut self.core
    }

    pub fn cursor(&self) -> RowRef {
        self.cursor
    }

    pub fn is_folded(&self, file: usize) -> bool {
        self.folded.contains(&file)
    }

    pub fn overlay(&self) -> Option<&FileOverlay> {
        self.overlay.as_ref()
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn editor(&self) -> Option<&CommentEditor> {
        self.editor.as_ref()
    }

    /// The open comment box's text field. The chat's guarded Ctrl+C clears
    /// whatever field is focused, and while the page is up that is this one.
    pub fn editor_field_mut(&mut self) -> Option<&mut crate::composer::Composer> {
        self.editor.as_mut().map(|editor| &mut editor.field)
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// The viewport the page lays itself out for. Scroll follows the cursor
    /// as it moves, so the page has to know the height before the key
    /// arrives, not only when it draws.
    pub fn set_viewport(&mut self, width: u16, height: u16) {
        self.resize(width, height);
        self.follow_cursor();
    }

    /// Learn the screen without pulling the body back to the cursor.
    ///
    /// The wheel deliberately scrolls away from the cursor, so a wheel
    /// notch cannot go through `set_viewport`: following the cursor there
    /// would undo the previous notch before the next one applied.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    pub(super) fn files(&self) -> &[amux_ui::review::ReviewFile] {
        &self.core.document().files
    }

    pub(super) fn body_height(&self) -> usize {
        (self.height as usize).saturating_sub(CHROME_ROWS).max(1)
    }

    // --- positions ----------------------------------------------------

    /// The rows a file offers the cursor. A folded file offers exactly one:
    /// its header row stands for the whole file.
    fn rows_in(&self, file: usize) -> usize {
        if self.folded.contains(&file) {
            1
        } else {
            self.files()
                .get(file)
                .map_or(0, |file| file.rows.len())
                .max(1)
        }
    }

    fn first_position(&self) -> RowRef {
        RowRef { file: 0, row: 0 }
    }

    fn last_position(&self) -> RowRef {
        let file = self.files().len().saturating_sub(1);
        RowRef {
            file,
            row: self.rows_in(file).saturating_sub(1),
        }
    }

    pub(super) fn next_position(&self, at: RowRef) -> Option<RowRef> {
        if at.row + 1 < self.rows_in(at.file) {
            return Some(RowRef {
                file: at.file,
                row: at.row + 1,
            });
        }
        (at.file + 1 < self.files().len()).then_some(RowRef {
            file: at.file + 1,
            row: 0,
        })
    }

    pub(super) fn prev_position(&self, at: RowRef) -> Option<RowRef> {
        if at.row > 0 {
            return Some(RowRef {
                file: at.file,
                row: at.row - 1,
            });
        }
        let file = at.file.checked_sub(1)?;
        Some(RowRef {
            file,
            row: self.rows_in(file).saturating_sub(1),
        })
    }

    /// Where `J` and `K` may land: every hunk start of an open file, and the
    /// single row a folded file offers.
    fn hunk_positions(&self) -> Vec<RowRef> {
        let mut positions = Vec::new();
        for (index, file) in self.files().iter().enumerate() {
            if self.folded.contains(&index) {
                positions.push(RowRef {
                    file: index,
                    row: 0,
                });
                continue;
            }
            positions.extend(file.hunk_starts.iter().map(|row| RowRef {
                file: index,
                row: *row,
            }));
        }
        positions
    }

    fn clamp_cursor(&mut self) {
        let files = self.files().len();
        if files == 0 {
            self.cursor = RowRef { file: 0, row: 0 };
            return;
        }
        self.cursor.file = self.cursor.file.min(files - 1);
        self.cursor.row = self.cursor.row.min(self.rows_in(self.cursor.file) - 1);
    }

    // --- keys ---------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> ReviewOutcome {
        if self.editor.is_some() {
            return super::comments::editor_key(self, key);
        }
        if self.overlay.is_some() {
            return self.handle_overlay_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return ReviewOutcome::Ignored;
        }
        match key.code {
            KeyCode::Char('v') => {
                self.selection = match self.selection {
                    Some(_) => None,
                    None => Some(Selection {
                        anchor: self.cursor,
                        head: self.cursor,
                    }),
                };
                ReviewOutcome::Handled
            }
            KeyCode::Char('c') => super::comments::open_editor(self, None),
            KeyCode::Enter => {
                let existing = super::comments::comment_at(self, self.cursor);
                super::comments::open_editor(self, existing)
            }
            KeyCode::Char('d') => super::comments::delete_at_cursor(self),
            KeyCode::Esc => {
                // Esc steps back out of the selection without leaving the
                // page; there is nothing else to step out of here.
                self.selection = None;
                ReviewOutcome::Handled
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_to(self.next_position(self.cursor)),
            KeyCode::Char('k') | KeyCode::Up => self.move_to(self.prev_position(self.cursor)),
            KeyCode::Char('J') => {
                let next = self
                    .hunk_positions()
                    .into_iter()
                    .find(|position| *position > self.cursor);
                self.move_to(next)
            }
            KeyCode::Char('K') => {
                let previous = self
                    .hunk_positions()
                    .into_iter()
                    .rfind(|position| *position < self.cursor);
                self.move_to(previous)
            }
            KeyCode::Char(']') => {
                let file = self.cursor.file + 1;
                self.move_to((file < self.files().len()).then_some(RowRef { file, row: 0 }))
            }
            KeyCode::Char('[') => {
                let file = self.cursor.file.checked_sub(1);
                self.move_to(file.map(|file| RowRef { file, row: 0 }))
            }
            KeyCode::Char('g') => self.move_to(Some(self.first_position())),
            KeyCode::Char('G') => self.move_to(Some(self.last_position())),
            KeyCode::Char('n') => {
                let next = self
                    .core
                    .rows_with_comments()
                    .into_iter()
                    .find(|row| *row > self.cursor);
                self.jump_to_comment(next)
            }
            KeyCode::Char('N') => {
                let previous = self
                    .core
                    .rows_with_comments()
                    .into_iter()
                    .rfind(|row| *row < self.cursor);
                self.jump_to_comment(previous)
            }
            KeyCode::Char('z') => {
                let file = self.cursor.file;
                if !self.folded.remove(&file) {
                    self.folded.insert(file);
                    self.cursor = RowRef { file, row: 0 };
                }
                self.follow_cursor();
                ReviewOutcome::Handled
            }
            KeyCode::Char('f') => {
                self.overlay = Some(FileOverlay {
                    selected: self.cursor.file,
                });
                ReviewOutcome::Handled
            }
            KeyCode::Char('b') => ReviewOutcome::SwitchBase(self.other_base()),
            KeyCode::Char('q') => ReviewOutcome::Close,
            _ => ReviewOutcome::Ignored,
        }
    }

    fn handle_overlay_key(&mut self, key: &KeyEvent) -> ReviewOutcome {
        let Some(overlay) = self.overlay.as_mut() else {
            return ReviewOutcome::Ignored;
        };
        let files = self.core.document().files.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                overlay.selected = (overlay.selected + 1).min(files.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                let file = overlay.selected;
                self.overlay = None;
                self.folded.remove(&file);
                self.cursor = RowRef { file, row: 0 };
                self.follow_cursor();
            }
            // The overlay is a step inside the page, so both the step-back
            // key and the close key leave it without leaving the review.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => self.overlay = None,
            _ => return ReviewOutcome::Ignored,
        }
        ReviewOutcome::Handled
    }

    /// A wheel notch scrolls the body without moving the cursor.
    pub fn handle_wheel(&mut self, delta: i32) {
        let max = self.max_scroll();
        let scroll = self.scroll as i64 + i64::from(delta);
        self.scroll = scroll.clamp(0, max as i64) as usize;
    }

    pub(super) fn move_to(&mut self, position: Option<RowRef>) -> ReviewOutcome {
        if let Some(position) = position {
            // A selection lives inside one file, because an anchor does:
            // a range crossing files has no path to name.
            if let Some(selection) = self.selection.as_mut() {
                if position.file != selection.anchor.file {
                    return ReviewOutcome::Handled;
                }
                selection.head = position;
            }
            self.cursor = position;
            self.follow_cursor();
        }
        ReviewOutcome::Handled
    }

    /// A comment can sit in a file the reader folded. Jumping to it opens
    /// that file again, so `n` always lands on the row the comment is on.
    fn jump_to_comment(&mut self, row: Option<RowRef>) -> ReviewOutcome {
        if let Some(row) = row {
            self.folded.remove(&row.file);
            self.cursor = row;
            self.follow_cursor();
        }
        ReviewOutcome::Handled
    }

    fn other_base(&self) -> DiffBase {
        match &self.core.document().identity.base {
            DiffBase::WorkingTree => DiffBase::Branch {
                base: self.branch.clone(),
            },
            DiffBase::Branch { .. } => DiffBase::WorkingTree,
        }
    }

    // --- layout and frame ---------------------------------------------

    fn body(&self, theme: Theme) -> Body {
        let width = self.width as usize;
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        let comment_rows = super::comments::comment_rows(self);
        for (index, file) in self.files().iter().enumerate() {
            let folded = self.folded.contains(&index);
            let comments = self.core.comments_in(index);
            let start = lines.len();
            lines.push(file_line(
                file,
                folded,
                comments,
                width,
                theme,
                // An open file's rows carry the cursor themselves; a folded
                // one has no rows, so its header carries it.
                folded && index == self.cursor.file,
            ));
            if folded {
                spans.push((
                    RowRef {
                        file: index,
                        row: 0,
                    },
                    start,
                    lines.len() - start,
                ));
                continue;
            }
            let groups = paint_rows(&file.rows, theme, width, BODY_LEFT, false).into_row_groups();
            for (row, group) in groups.into_iter().enumerate() {
                let here = RowRef { file: index, row };
                // The first row of a file carries its header line, so
                // stepping into a file always brings the file's name on
                // screen with it.
                let at = if row == 0 { start } else { lines.len() };
                let marked = self.marks(here);
                for (offset, mut line) in group.into_iter().enumerate() {
                    if marked && offset == 0 {
                        mark_cursor(&mut line, theme);
                    }
                    lines.push(line);
                }
                let mut height = lines.len() - at;
                lines.extend(super::comments::thread_lines(
                    self.core.comments(),
                    &comment_rows,
                    here,
                    width,
                    theme,
                ));
                if let Some(editor) = self
                    .editor
                    .as_ref()
                    .filter(|editor| editor.target.last() == here)
                {
                    // The box must stay on screen while it is typed in, so
                    // the row it hangs from claims its height.
                    let box_lines = super::comments::editor_lines(editor, width, theme);
                    height = lines.len() - at + box_lines.len();
                    lines.extend(box_lines);
                }
                spans.push((here, at, height));
            }
        }
        Body { lines, spans }
    }

    /// Whether a row wears the cursor bar: the cursor itself, or any row
    /// inside an open selection.
    fn marks(&self, row: RowRef) -> bool {
        match self.selection.as_ref() {
            Some(selection) => selection.contains(row),
            None => self.cursor == row,
        }
    }

    fn max_scroll(&self) -> usize {
        let total = self.body(Theme::default()).lines.len();
        total.saturating_sub(self.body_height())
    }

    /// Keep the cursor's row inside the body window after every motion.
    pub(super) fn follow_cursor(&mut self) {
        self.clamp_cursor();
        let body = self.body(Theme::default());
        let height = self.body_height();
        let Some((_, start, span)) = body
            .spans
            .iter()
            .copied()
            .find(|(row, _, _)| *row == self.cursor)
        else {
            return;
        };
        if start < self.scroll || span >= height {
            self.scroll = start;
        } else if start + span > self.scroll + height {
            self.scroll = start + span - height;
        }
        self.scroll = self.scroll.min(body.lines.len().saturating_sub(height));
    }

    /// The whole page, ready for the frame assembler.
    pub fn frame(&self, theme: Theme, width: u16, height: u16) -> Vec<Line<'static>> {
        let mut page = self.clone();
        page.width = width;
        page.height = height;
        page.follow_cursor();
        page.render(theme)
    }

    fn render(&self, theme: Theme) -> Vec<Line<'static>> {
        let width = self.width as usize;
        let height = self.height as usize;
        let body_height = self.body_height();
        let body = self.body(theme);
        let total = body.lines.len();
        let start = self.scroll.min(total.saturating_sub(body_height));

        let mut window: Vec<Line<'static>> = if self.overlay.is_some() {
            self.overlay_lines(theme)
        } else {
            body.lines
                .into_iter()
                .skip(start)
                .take(body_height)
                .collect()
        };
        window.truncate(body_height);
        while window.len() < body_height {
            window.push(Line::default());
        }

        let shown = total.saturating_sub(start).min(body_height);
        let mut lines = Vec::with_capacity(height);
        lines.push(self.title_line(width, theme));
        lines.push(self.file_status_line(start, shown, total, width, theme));
        lines.push(rule_line(width, theme));
        lines.extend(window);
        lines.push(rule_line(width, theme));
        lines.push(self.footer_line(width, theme));
        lines.truncate(height);
        lines
    }

    fn title_line(&self, width: usize, theme: Theme) -> Line<'static> {
        let header = self.core.header();
        let mut line = Line::default();
        push_span(
            &mut line,
            BODY_LEFT,
            format!(
                "review \u{b7} {} @ {}",
                base_words(&header.base),
                header.head
            ),
            theme.text(),
        );
        let files = self.files().len();
        let added: u32 = self.files().iter().map(|file| file.added).sum();
        let removed: u32 = self.files().iter().map(|file| file.removed).sum();
        let comments = self.core.comment_count();
        push_right(
            &mut line,
            format!(
                "{files} {} \u{b7} +{added} \u{2212}{removed} \u{b7} {comments} {}",
                plural(files, "file"),
                plural(comments, "comment"),
            ),
            width,
            theme.muted(),
        );
        line
    }

    fn file_status_line(
        &self,
        start: usize,
        shown: usize,
        total: usize,
        width: usize,
        theme: Theme,
    ) -> Line<'static> {
        let mut line = Line::default();
        // While the file list is open it, not the body, is what the reader
        // is pointing at, so the line under the header follows it.
        let at = self
            .overlay
            .as_ref()
            .map_or(self.cursor.file, |overlay| overlay.selected);
        if let Some(file) = self.files().get(at) {
            push_span(
                &mut line,
                BODY_LEFT,
                format!("{}  +{} \u{2212}{}", file.path, file.added, file.removed),
                theme.text(),
            );
        }
        if self.overlay.is_none() {
            let position = if total == 0 {
                "lines 0-0/0".to_string()
            } else {
                format!("lines {}-{}/{}", start + 1, start + shown, total)
            };
            push_right(&mut line, position, width, theme.muted());
        }
        line
    }

    fn overlay_lines(&self, theme: Theme) -> Vec<Line<'static>> {
        let selected = self.overlay.as_ref().map(|overlay| overlay.selected);
        let mut lines = vec![{
            let mut line = Line::default();
            push_span(&mut line, BODY_LEFT, "files", theme.muted());
            line
        }];
        for (index, file) in self.files().iter().enumerate() {
            let mut line = Line::default();
            if selected == Some(index) {
                push_span(&mut line, 0, "\u{258c}", theme.focus_bar());
            }
            push_span(
                &mut line,
                BODY_LEFT + 2,
                format!("{}  +{} \u{2212}{}", file.path, file.added, file.removed),
                theme.text(),
            );
            let comments = self.core.comments_in(index);
            if comments > 0 {
                push_span(
                    &mut line,
                    0,
                    format!("  {comments} {}", plural(comments, "comment")),
                    theme.muted(),
                );
            }
            if self.folded.contains(&index) {
                push_span(&mut line, 0, "  folded", theme.muted());
            }
            lines.push(line);
        }
        lines
    }

    fn footer_line(&self, width: usize, theme: Theme) -> Line<'static> {
        let text = if self.editor.is_some() {
            "enter save \u{b7} ctrl-j newline \u{b7} esc cancel"
        } else if self.selection.is_some() {
            "j/k extend \u{b7} c comment \u{b7} esc cancel"
        } else if self.overlay.is_some() {
            "j/k file \u{b7} enter go \u{b7} esc back"
        } else {
            "j/k rows \u{b7} v select \u{b7} c comment \u{b7} J/K hunks \u{b7} [/] files \u{b7} f files \u{b7} z fold \u{b7} n/N comments \u{b7} b base \u{b7} q close"
        };
        let mut line = Line::default();
        push_span(&mut line, 4, text.to_string(), theme.muted());
        let _ = width;
        line
    }
}

/// The body's screen lines and where each addressable row sits in them.
struct Body {
    lines: Vec<Line<'static>>,
    spans: Vec<(RowRef, usize, usize)>,
}

pub(super) fn file_line(
    file: &amux_ui::review::ReviewFile,
    folded: bool,
    comments: usize,
    width: usize,
    theme: Theme,
    cursor: bool,
) -> Line<'static> {
    let mut line = Line::default();
    if cursor {
        push_span(&mut line, 0, "\u{258c}", theme.focus_bar());
    }
    push_span(
        &mut line,
        BODY_LEFT,
        format!(
            "{} {}  +{} \u{2212}{}",
            if folded { "\u{25b8}" } else { "\u{25be}" },
            file.path,
            file.added,
            file.removed
        ),
        theme.diff_meta(),
    );
    let mut trailer = String::new();
    if folded {
        trailer.push_str(&format!(
            "  {} {} folded",
            file.rows.len(),
            plural(file.rows.len(), "row")
        ));
    }
    if comments > 0 {
        trailer.push_str(&format!("  {comments} {}", plural(comments, "comment")));
    }
    if !trailer.is_empty() {
        push_span(&mut line, 0, trailer, theme.diff_meta());
    }
    let _ = width;
    line
}

/// Put the cursor bar in column 0 without moving the row it marks: the
/// body is painted with a two-cell indent, so the bar takes the first of
/// those cells rather than pushing the row right.
fn mark_cursor(line: &mut Line<'static>, theme: Theme) {
    if let Some(first) = line.spans.first_mut()
        && let Some(rest) = first.content.strip_prefix(' ')
    {
        first.content = rest.to_string().into();
    }
    line.spans
        .insert(0, Span::styled("\u{258c}", theme.focus_bar()));
}

fn rule_line(width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    line.spans
        .push(Span::styled("\u{2500}".repeat(width), theme.muted()));
    line
}

/// The header's base in words rather than in the body's wire spelling.
fn base_words(base: &str) -> String {
    match base.strip_prefix("branch:") {
        Some(branch) => format!("branch {branch}"),
        None => "working tree".to_string(),
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::review::{Side, anchor};

    use super::super::fixture::{sample_review, sample_review_against};
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(view: &mut ReviewView, code: char) -> ReviewOutcome {
        view.handle_key(&key(KeyCode::Char(code)))
    }

    fn open() -> ReviewView {
        let mut view = sample_review();
        view.set_viewport(120, 40);
        view
    }

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn comment_on(view: &mut ReviewView, file: usize, row: usize, text: &str) {
        let at = RowRef { file, row };
        let anchor = anchor(view.review().document(), at, at).expect("anchorable fixture row");
        view.review_mut().add(anchor, text.to_string());
    }

    #[test]
    fn fixture_parses_into_three_files_with_two_hunks_in_the_first() {
        let view = open();
        let files = &view.review().document().files;
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].hunk_starts, vec![0, 5]);
        assert_eq!((files[0].added, files[0].removed), (4, 2));
        assert_eq!(files[0].rows.len(), 12);
    }

    #[test]
    fn j_and_k_walk_rows_and_stop_at_the_document_edges() {
        let mut view = open();
        assert_eq!(press(&mut view, 'k'), ReviewOutcome::Handled);
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });

        for _ in 0..11 {
            press(&mut view, 'j');
        }
        assert_eq!(view.cursor(), RowRef { file: 0, row: 11 });
        press(&mut view, 'j');
        assert_eq!(
            view.cursor(),
            RowRef { file: 1, row: 0 },
            "the last row of a file steps into the next file"
        );
        press(&mut view, 'k');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 11 }, "and back again");

        press(&mut view, 'G');
        let last = view.review().document().files[2].rows.len() - 1;
        assert_eq!(view.cursor(), RowRef { file: 2, row: last });
        press(&mut view, 'j');
        assert_eq!(view.cursor(), RowRef { file: 2, row: last });
    }

    #[test]
    fn capital_j_and_k_step_between_hunks_across_files() {
        let mut view = open();
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 5 });
        press(&mut view, 'J');
        assert_eq!(
            view.cursor(),
            RowRef { file: 1, row: 0 },
            "the last hunk of a file steps into the next file's first"
        );
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 });
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 }, "and stops");

        press(&mut view, 'K');
        assert_eq!(view.cursor(), RowRef { file: 1, row: 0 });
        press(&mut view, 'K');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 5 });
        press(&mut view, 'K');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        press(&mut view, 'K');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
    }

    #[test]
    fn bracket_keys_step_between_files_and_g_returns_to_the_top() {
        let mut view = open();
        press(&mut view, ']');
        assert_eq!(view.cursor(), RowRef { file: 1, row: 0 });
        press(&mut view, ']');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 });
        press(&mut view, ']');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 });
        press(&mut view, '[');
        assert_eq!(view.cursor(), RowRef { file: 1, row: 0 });
        press(&mut view, '[');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        press(&mut view, '[');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });

        press(&mut view, 'G');
        press(&mut view, 'g');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        assert_eq!(view.scroll(), 0);
    }

    #[test]
    fn folding_collapses_a_file_to_one_position_and_unfolding_restores_it() {
        let mut view = open();
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 5 });
        press(&mut view, 'z');
        assert!(view.is_folded(0));
        assert_eq!(
            view.cursor(),
            RowRef { file: 0, row: 0 },
            "folding puts the cursor on the row that stands for the file"
        );
        press(&mut view, 'j');
        assert_eq!(
            view.cursor(),
            RowRef { file: 1, row: 0 },
            "a folded file offers exactly one position"
        );
        press(&mut view, 'k');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        press(&mut view, 'z');
        assert!(!view.is_folded(0));
        press(&mut view, 'G');
        press(&mut view, 'g');
        for _ in 0..12 {
            press(&mut view, 'j');
        }
        assert_eq!(view.cursor(), RowRef { file: 1, row: 0 });
    }

    #[test]
    fn a_folded_file_is_one_hunk_stop() {
        let mut view = open();
        press(&mut view, ']');
        press(&mut view, 'z');
        press(&mut view, 'g');
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 5 });
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 1, row: 0 });
        press(&mut view, 'J');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 });
    }

    #[test]
    fn n_and_capital_n_walk_the_cores_rows_with_comments_and_unfold_to_reach_them() {
        let mut view = open();
        comment_on(&mut view, 0, 3, "this rename needs a note");
        comment_on(&mut view, 2, 1, "state the crate this belongs to");
        assert_eq!(
            view.review().rows_with_comments(),
            vec![RowRef { file: 0, row: 3 }, RowRef { file: 2, row: 1 }]
        );

        press(&mut view, 'z');
        assert!(view.is_folded(0));
        press(&mut view, 'n');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 3 });
        assert!(!view.is_folded(0), "jumping to a comment opens its file");
        press(&mut view, 'n');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 1 });
        press(&mut view, 'n');
        assert_eq!(view.cursor(), RowRef { file: 2, row: 1 }, "and stops");
        press(&mut view, 'N');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 3 });
        press(&mut view, 'N');
        assert_eq!(view.cursor(), RowRef { file: 0, row: 3 });
    }

    #[test]
    fn the_file_overlay_states_the_cores_magnitudes_and_comment_counts() {
        let mut view = open();
        comment_on(&mut view, 2, 1, "state the crate this belongs to");
        comment_on(&mut view, 2, 2, "and the invariant");
        press(&mut view, 'f');
        assert_eq!(view.overlay().map(|overlay| overlay.selected), Some(0));

        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        assert!(lines.iter().any(|line| line.contains("files")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("src/lib.rs  +4 \u{2212}2")),
            "the overlay quotes the core's magnitudes: {lines:?}"
        );
        let attachments = lines
            .iter()
            .find(|line| line.contains("src/attachments.rs"))
            .expect("the new file is listed");
        assert!(
            attachments.contains("+3 \u{2212}0") && attachments.contains("2 comments"),
            "{attachments}"
        );
        assert_eq!(
            view.review().comments_in(2),
            2,
            "the count the overlay printed is the core's"
        );

        press(&mut view, 'j');
        press(&mut view, 'j');
        assert_eq!(view.overlay().map(|overlay| overlay.selected), Some(2));
        view.handle_key(&key(KeyCode::Enter));
        assert!(view.overlay().is_none());
        assert_eq!(view.cursor(), RowRef { file: 2, row: 0 });
    }

    #[test]
    fn the_overlay_closes_without_leaving_the_review() {
        let mut view = open();
        press(&mut view, 'f');
        assert_eq!(
            view.handle_key(&key(KeyCode::Esc)),
            ReviewOutcome::Handled,
            "esc steps back out of the overlay"
        );
        assert!(view.overlay().is_none());
        press(&mut view, 'f');
        assert_eq!(press(&mut view, 'q'), ReviewOutcome::Handled);
        assert!(view.overlay().is_none());
        assert_eq!(press(&mut view, 'q'), ReviewOutcome::Close);
    }

    #[test]
    fn b_offers_the_other_base_and_q_closes() {
        let mut view = open();
        assert_eq!(
            press(&mut view, 'b'),
            ReviewOutcome::SwitchBase(DiffBase::Branch {
                base: "main".into()
            })
        );
        let mut branch = sample_review_against(DiffBase::Branch {
            base: "main".into(),
        });
        branch.set_viewport(120, 40);
        assert_eq!(
            press(&mut branch, 'b'),
            ReviewOutcome::SwitchBase(DiffBase::WorkingTree)
        );
        assert_eq!(press(&mut view, 'q'), ReviewOutcome::Close);
    }

    #[test]
    fn the_wheel_scrolls_the_body_without_moving_the_cursor() {
        let mut view = sample_review();
        view.set_viewport(120, 12);
        view.handle_wheel(3);
        assert_eq!(view.scroll(), 3);
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        view.handle_wheel(-10);
        assert_eq!(view.scroll(), 0);
        view.handle_wheel(1_000);
        assert!(view.scroll() > 0);
        let ceiling = view.scroll();
        view.handle_wheel(1);
        assert_eq!(view.scroll(), ceiling, "the wheel stops at the last screen");
    }

    #[test]
    fn the_scroll_follows_the_cursor_through_a_short_viewport() {
        let mut view = sample_review();
        view.set_viewport(120, 12);
        assert_eq!(view.scroll(), 0);
        press(&mut view, 'G');
        assert!(view.scroll() > 0, "the end of the diff is on screen");
        let bottom = view.scroll();
        press(&mut view, 'g');
        assert_eq!(view.scroll(), 0);
        assert!(bottom > 0);
    }

    #[test]
    fn the_frame_states_the_base_the_totals_and_the_bindings() {
        let mut view = open();
        comment_on(&mut view, 0, 3, "this rename needs a note");
        let lines = text_of(&view.frame(Theme::default(), 120, 40));

        assert!(
            lines[0].contains("review \u{b7} working tree @ 4f2a9c1"),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("3 files \u{b7} +7 \u{2212}4 \u{b7} 1 comment"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("src/lib.rs  +4 \u{2212}2"),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains("lines 1-"), "{}", lines[1]);
        let footer = lines.last().expect("a footer");
        assert!(
            footer.contains("q close") && footer.contains("z fold"),
            "{footer}"
        );
    }

    #[test]
    fn a_branch_review_names_its_branch_in_the_header() {
        let mut view = sample_review_against(DiffBase::Branch {
            base: "main".into(),
        });
        view.set_viewport(120, 40);
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        assert!(
            lines[0].contains("review \u{b7} branch main @ 4f2a9c1"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn the_body_paints_file_headers_hunk_meta_and_blank_continuation_gutters() {
        let view = open();
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("\u{25be} src/lib.rs  +4 \u{2212}2")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("@@ -10,3 +10,5 @@")),
            "hunk headers stay in the body: {lines:?}"
        );
        let wrapped = lines
            .iter()
            .position(|line| line.contains("A review holds its diff open"))
            .expect("the long added line is painted");
        let continuation = &lines[wrapped + 1];
        assert!(
            continuation[..9].chars().all(char::is_whitespace)
                && continuation.contains("refetches."),
            "a wrapped row continues under a blank gutter: {continuation:?}"
        );
        assert!(
            lines[wrapped].contains("11 +"),
            "and the first screen row of that source row carries its number: {:?}",
            lines[wrapped]
        );
    }

    #[test]
    fn a_folded_file_paints_one_dim_meta_row_that_states_what_is_hidden() {
        let mut view = open();
        press(&mut view, 'z');
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        let header = lines
            .iter()
            .find(|line| line.contains("rows folded"))
            .expect("the folded file keeps its header");
        assert!(
            header.contains("\u{25b8} src/lib.rs") && header.contains("12 rows folded"),
            "{header}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("pub mod legacy_store")),
            "a folded file paints none of its rows"
        );
    }

    #[test]
    fn a_side_is_never_derived_outside_the_core() {
        let view = open();
        let at = RowRef { file: 0, row: 2 };
        let anchor = anchor(view.review().document(), at, at).expect("removed row anchors");
        assert_eq!(anchor.side, Side::Old);
        assert_eq!(anchor.path, "src/lib.rs");
    }
}
