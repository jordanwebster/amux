//! Selecting rows, writing a comment on them, and painting the threads
//! that result. Every anchor and every stored comment comes from the
//! review core; this module decides only what is on screen and which key
//! reaches which core call.

use amux_ui::review::{RowRef, anchor};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use serde::{Deserialize, Serialize};

use super::view::{ReviewOutcome, ReviewView};
use crate::composer::{Composer, readline_key};
use crate::render::{Theme, clip_to_width, push_span, str_width};

/// A thread hangs under its row, past the gutter.
const THREAD_LEFT: usize = 8;

/// An inclusive run of rows in one file. `anchor` is where `v` was pressed
/// and `head` is where the cursor has since walked to, so a selection can
/// run in either direction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub anchor: RowRef,
    pub head: RowRef,
}

impl Selection {
    pub fn one(row: RowRef) -> Self {
        Self {
            anchor: row,
            head: row,
        }
    }

    pub fn first(&self) -> RowRef {
        self.anchor.min(self.head)
    }

    pub fn last(&self) -> RowRef {
        self.anchor.max(self.head)
    }

    pub fn contains(&self, row: RowRef) -> bool {
        row >= self.first() && row <= self.last()
    }
}

/// The inline comment box: the composer's rules over the rows a comment
/// will be anchored to. `editing` names the existing comment being
/// rewritten, and is absent for a new one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommentEditor {
    pub target: Selection,
    pub field: Composer,
    pub editing: Option<usize>,
}

/// The index of the first comment the core anchors to `row`, if any.
pub fn comment_at(view: &ReviewView, row: RowRef) -> Option<usize> {
    comment_rows(view)
        .into_iter()
        .position(|at| at == Some(row))
}

/// Each comment's row, as the core places it.
pub fn comment_rows(view: &ReviewView) -> Vec<Option<RowRef>> {
    view.review().comment_rows()
}

/// Open the box over the current selection, or over the cursor row alone.
/// `editing` carries the comment being rewritten, whose text the box opens
/// with.
pub fn open_editor(view: &mut ReviewView, editing: Option<usize>) -> ReviewOutcome {
    let target = match (editing, view.selection) {
        (Some(index), _) => match comment_rows(view).get(index).copied().flatten() {
            Some(row) => Selection::one(row),
            None => return ReviewOutcome::Handled,
        },
        (None, Some(selection)) => selection,
        (None, None) => Selection::one(view.cursor),
    };
    // A comment needs an anchor, and only rows with a line number have one.
    if anchor(view.review().document(), target.first(), target.last()).is_err() {
        return ReviewOutcome::Handled;
    }
    let mut field = Composer::default();
    if let Some(index) = editing
        && let Some(comment) = view.review().comments().get(index)
    {
        field.restore(&comment.text);
    }
    view.editor = Some(CommentEditor {
        target,
        field,
        editing,
    });
    view.selection = None;
    view.cursor = target.last();
    view.follow_cursor();
    ReviewOutcome::Handled
}

/// Delete the comment the cursor stands on, through the core.
pub fn delete_at_cursor(view: &mut ReviewView) -> ReviewOutcome {
    let Some(index) = comment_at(view, view.cursor) else {
        return ReviewOutcome::Handled;
    };
    match view.review_mut().delete(index) {
        Ok(_) => {
            view.follow_cursor();
            ReviewOutcome::CommentsChanged
        }
        Err(_) => ReviewOutcome::Handled,
    }
}

/// The box owns the keyboard while it is open: the composer's rules, with
/// Enter saving rather than inserting.
pub fn editor_key(view: &mut ReviewView, key: &KeyEvent) -> ReviewOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            view.editor = None;
            view.follow_cursor();
            ReviewOutcome::Handled
        }
        KeyCode::Char('j') if ctrl => {
            if let Some(editor) = view.editor.as_mut() {
                editor.field.insert_newline();
            }
            ReviewOutcome::Handled
        }
        KeyCode::Enter => save(view),
        _ => {
            if let Some(editor) = view.editor.as_mut() {
                readline_key(&mut editor.field, key);
            }
            ReviewOutcome::Handled
        }
    }
}

fn save(view: &mut ReviewView) -> ReviewOutcome {
    let Some(editor) = view.editor.take() else {
        return ReviewOutcome::Handled;
    };
    let text = editor.field.text().trim().to_string();
    if text.is_empty() {
        // An empty box saves nothing and leaves no trace, whether it was
        // opened on a new row or on an existing comment.
        view.follow_cursor();
        return ReviewOutcome::Handled;
    }
    let changed = match editor.editing {
        Some(index) => view.review_mut().edit(index, text).is_ok(),
        None => {
            match anchor(
                view.review().document(),
                editor.target.first(),
                editor.target.last(),
            ) {
                Ok(anchor) => {
                    view.review_mut().add(anchor, text);
                    true
                }
                Err(_) => false,
            }
        }
    };
    view.follow_cursor();
    if changed {
        ReviewOutcome::CommentsChanged
    } else {
        ReviewOutcome::Handled
    }
}

/// The threads the core anchors to `row`, painted under it.
pub fn thread_lines(
    view: &ReviewView,
    rows: &[Option<RowRef>],
    row: RowRef,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, comment) in view.review().comments().iter().enumerate() {
        if rows.get(index).copied().flatten() != Some(row) {
            continue;
        }
        let text_width = width.saturating_sub(THREAD_LEFT + 4).max(8);
        for (offset, chunk) in wrap(&comment.text, text_width).into_iter().enumerate() {
            let mut line = Line::default();
            push_span(&mut line, THREAD_LEFT, "\u{2503}", theme.accent_bar());
            push_span(
                &mut line,
                THREAD_LEFT + 2,
                if offset == 0 {
                    format!("\u{270e} {chunk}")
                } else {
                    format!("  {chunk}")
                },
                theme.text(),
            );
            lines.push(line);
        }
    }
    lines
}

/// The bordered box, drawn under the last row of its target.
pub fn editor_lines(editor: &CommentEditor, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(THREAD_LEFT + 4).max(8);
    let (rows, _) = editor.field.display_rows(inner);
    let title = if editor.editing.is_some() {
        " edit comment "
    } else {
        " comment "
    };
    let mut lines = vec![box_rule(inner, title, theme)];
    for row in &rows {
        let mut line = Line::default();
        push_span(&mut line, THREAD_LEFT, "\u{2502}", theme.muted());
        push_span(
            &mut line,
            THREAD_LEFT + 2,
            format!("{row:<inner$}"),
            theme.text(),
        );
        push_span(
            &mut line,
            THREAD_LEFT + 3 + inner,
            "\u{2502}",
            theme.muted(),
        );
        lines.push(line);
    }
    lines.push(box_rule(
        inner,
        " enter save \u{b7} ctrl-j newline \u{b7} esc cancel ",
        theme,
    ));
    lines
}

/// One horizontal edge of the box, carrying its label.
fn box_rule(inner: usize, label: &str, theme: Theme) -> Line<'static> {
    let label = clip_to_width(label, inner);
    // The rule spans from the box's left edge to the right border the text
    // rows close on.
    let dashes = (inner + 4).saturating_sub(str_width(label) + 1);
    let mut line = Line::default();
    push_span(
        &mut line,
        THREAD_LEFT,
        format!("\u{2500}{label}{}", "\u{2500}".repeat(dashes)),
        theme.muted(),
    );
    line
}

/// Wrap comment text to `width` cells, keeping the writer's own newlines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut rest = paragraph;
        loop {
            let head = clip_to_width(rest, width);
            if head.is_empty() {
                out.push(rest.to_string());
                break;
            }
            out.push(head.to_string());
            rest = &rest[head.len()..];
            if rest.is_empty() {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use amux_ui::review::Side;

    use super::super::fixture::sample_review;
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(view: &mut ReviewView, code: char) -> ReviewOutcome {
        view.handle_key(&key(KeyCode::Char(code)))
    }

    fn type_text(view: &mut ReviewView, text: &str) {
        for character in text.chars() {
            view.handle_key(&key(KeyCode::Char(character)));
        }
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

    /// Walk the cursor to the removed row of the first hunk.
    fn to_removed_row(view: &mut ReviewView) {
        for _ in 0..2 {
            press(view, 'j');
        }
        assert_eq!(view.cursor(), RowRef { file: 0, row: 2 });
    }

    #[test]
    fn a_selection_across_a_removed_and_an_added_row_saves_one_anchored_comment() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'v');
        press(&mut view, 'j');
        assert_eq!(
            view.selection()
                .map(|selection| (selection.first(), selection.last())),
            Some((RowRef { file: 0, row: 2 }, RowRef { file: 0, row: 3 }))
        );

        press(&mut view, 'c');
        assert!(
            view.editor().is_some(),
            "c opens the box over the selection"
        );
        assert!(view.selection().is_none());
        type_text(&mut view, "keep the old name in a re-export");
        assert_eq!(
            view.handle_key(&key(KeyCode::Enter)),
            ReviewOutcome::CommentsChanged
        );
        assert!(view.editor().is_none());

        let comments = view.review().comments();
        assert_eq!(comments.len(), 1);
        let comment = &comments[0];
        assert_eq!(comment.path, "src/lib.rs");
        assert_eq!(
            (comment.start_side, comment.start_line),
            (Side::Old, 2),
            "the range starts on the removed row's old line"
        );
        assert_eq!(
            (comment.side, comment.line),
            (Side::New, 2),
            "and ends on the added row's new line"
        );
        assert_eq!(
            comment.quoted,
            vec!["-pub mod legacy_store;", "+pub mod attachments;"],
            "both rows are quoted, from the core"
        );
        assert_eq!(comment.text, "keep the old name in a re-export");
    }

    #[test]
    fn a_selection_cannot_leave_its_file_and_esc_cancels_it() {
        let mut view = open();
        press(&mut view, 'G');
        press(&mut view, 'v');
        let last = view.cursor();
        press(&mut view, 'j');
        assert_eq!(view.cursor(), last, "the last row is the end of the range");

        view.handle_key(&key(KeyCode::Esc));
        press(&mut view, 'g');
        press(&mut view, 'v');
        for _ in 0..20 {
            press(&mut view, 'j');
        }
        let selection = view.selection().expect("still selecting");
        assert_eq!(selection.last().file, 0, "a selection stays in one file");
        assert_eq!(view.cursor().file, 0);

        view.handle_key(&key(KeyCode::Esc));
        assert!(view.selection().is_none());
        assert_eq!(press(&mut view, 'q'), ReviewOutcome::Close);
    }

    #[test]
    fn the_box_takes_a_newline_from_ctrl_j_and_throws_the_text_away_on_esc() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'c');
        type_text(&mut view, "first");
        view.handle_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        type_text(&mut view, "second");
        let editor = view.editor().expect("the box is open");
        assert_eq!(editor.field.text(), "first\nsecond");

        view.handle_key(&key(KeyCode::Esc));
        assert!(view.editor().is_none());
        assert!(view.review().comments().is_empty(), "esc saves nothing");
    }

    #[test]
    fn an_empty_box_saves_nothing() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'c');
        assert_eq!(
            view.handle_key(&key(KeyCode::Enter)),
            ReviewOutcome::Handled
        );
        assert!(view.review().comments().is_empty());
    }

    #[test]
    fn enter_edits_the_comment_under_the_cursor_and_d_deletes_it_through_the_core() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'c');
        type_text(&mut view, "first take");
        view.handle_key(&key(KeyCode::Enter));
        assert_eq!(view.review().comment_count(), 1);

        assert_eq!(view.cursor(), RowRef { file: 0, row: 2 });
        view.handle_key(&key(KeyCode::Enter));
        let editor = view.editor().expect("enter on a commented row edits it");
        assert_eq!(editor.editing, Some(0));
        assert_eq!(editor.field.text(), "first take");
        type_text(&mut view, " — and a second");
        assert_eq!(
            view.handle_key(&key(KeyCode::Enter)),
            ReviewOutcome::CommentsChanged
        );
        assert_eq!(
            view.review().comments()[0].text,
            "first take — and a second"
        );
        assert_eq!(
            view.review().comment_count(),
            1,
            "editing rewrites rather than adds"
        );

        assert_eq!(press(&mut view, 'd'), ReviewOutcome::CommentsChanged);
        assert!(view.review().comments().is_empty());
        assert_eq!(
            press(&mut view, 'd'),
            ReviewOutcome::Handled,
            "there is nothing left to delete"
        );
    }

    #[test]
    fn a_row_without_a_line_number_cannot_be_commented_on() {
        let mut view = open();
        assert_eq!(view.cursor(), RowRef { file: 0, row: 0 });
        press(&mut view, 'c');
        assert!(
            view.editor().is_none(),
            "a hunk header has no side and no line, so it has no anchor"
        );
    }

    #[test]
    fn a_saved_comment_paints_a_thread_under_its_row_and_the_box_paints_a_border() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'c');
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        let border = lines
            .iter()
            .position(|line| line.contains("\u{2500} comment "))
            .expect("the box has a titled edge");
        assert!(
            lines[border].trim_start().starts_with('\u{2500}'),
            "{}",
            lines[border]
        );
        assert!(
            lines[border + 2].contains("enter save"),
            "the box states its keys: {}",
            lines[border + 2]
        );

        type_text(&mut view, "keep the old name in a re-export");
        view.handle_key(&key(KeyCode::Enter));
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        let row = lines
            .iter()
            .position(|line| line.contains("-pub mod legacy_store;"))
            .expect("the commented row is painted");
        let thread = &lines[row + 1];
        assert!(
            thread.contains('\u{2503}') && thread.contains("\u{270e} keep the old name"),
            "the thread hangs under its row with an accent bar and a marker: {thread:?}"
        );
    }

    #[test]
    fn a_long_comment_wraps_under_the_same_bar() {
        let mut view = open();
        to_removed_row(&mut view);
        press(&mut view, 'c');
        type_text(
            &mut view,
            "This rename drops a public module, so anything downstream that named it stops compiling; keep a re-export for one release.",
        );
        view.handle_key(&key(KeyCode::Enter));
        let lines = text_of(&view.frame(Theme::default(), 120, 40));
        let first = lines
            .iter()
            .position(|line| line.contains("\u{270e} This rename drops"))
            .expect("the thread is painted");
        assert!(
            lines[first + 1].contains('\u{2503}') && lines[first + 1].contains("release."),
            "the wrapped remainder keeps the bar: {:?}",
            lines[first + 1]
        );
    }
}
