//! The terminal's one painter for neutral unified-diff rows.

use amux_ui::claude::DiffDocument;
use amux_ui::diff::{RowFact, RowKind};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::render::{Theme, clip_to_width, line_len, push_span};

/// The two number columns keep two digits of shape even for a tiny patch.
const MIN_GUTTER_DIGITS: usize = 2;
/// One blank cell separates the dual gutter from the sign column.
const GUTTER_GAP: usize = 1;
const NEW_FILE_LEFT: usize = 3;
const NEW_FILE_GUTTER_GAP: usize = 2;
const NEW_FILE_SIGN_GAP: usize = 1;

#[derive(Clone, Copy)]
enum GutterLayout {
    Dual,
    NewOnly,
}

/// Screen lines grouped by the source diff row that produced them.
pub(crate) struct PaintedDiff {
    rows: Vec<Vec<Line<'static>>>,
}

impl PaintedDiff {
    pub(crate) fn screen_len(&self) -> usize {
        self.rows.iter().map(Vec::len).sum()
    }

    pub(crate) fn into_lines(self) -> Vec<Line<'static>> {
        self.rows.into_iter().flatten().collect()
    }

    /// Screen lines still grouped by source row, for surfaces that must
    /// address a diff row rather than a screen line — the review page maps
    /// its cursor and its scroll onto these groups.
    pub(crate) fn into_row_groups(self) -> Vec<Vec<Line<'static>>> {
        self.rows
    }

    /// A screen-row head for feed patches that have no remainder row of their
    /// own. The boolean says whether any painted screen row was omitted.
    pub(crate) fn into_screen_head(self, budget: usize) -> (Vec<Line<'static>>, bool) {
        let total = self.screen_len();
        let lines: Vec<_> = self.into_lines().into_iter().take(budget).collect();
        let cut = lines.len() < total;
        (lines, cut)
    }

    /// A docked preview reserves its final screen row for the caller's words.
    /// Whole diff rows are kept together so the remainder count can state the
    /// number of source rows that are absent rather than guessing from wraps.
    pub(crate) fn into_preview(self, budget: usize) -> DiffPreview {
        let budget = budget.max(1);
        if self.screen_len() <= budget {
            return DiffPreview {
                lines: self.into_lines(),
                hidden: 0,
            };
        }

        let available = budget - 1;
        let total_rows = self.rows.len();
        let mut lines = Vec::new();
        let mut shown = 0usize;
        for row in self.rows {
            if lines.len() + row.len() > available {
                if lines.is_empty() {
                    lines.extend(row.into_iter().take(available));
                }
                break;
            }
            lines.extend(row);
            shown += 1;
        }
        DiffPreview {
            lines,
            hidden: total_rows - shown,
        }
    }
}

pub(crate) struct DiffPreview {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) hidden: usize,
}

/// Paint typed rows with dual old/new gutters and wrapped continuations.
///
/// `left` positions the shared layout within its containing surface. `fill`
/// preserves a panel row's semantic tint to its edge; fullscreen rows stay
/// open for the frame assembler, as they did before the painter was shared.
pub(crate) fn paint_rows(
    rows: &[RowFact],
    theme: Theme,
    width: usize,
    left: usize,
    fill: bool,
) -> PaintedDiff {
    paint_rows_with_layout(rows, theme, width, left, fill, GutterLayout::Dual)
}

/// Write documents keep their established single-new-number gutter while
/// sharing the same wrapping and continuation painter as unified diffs.
pub(crate) fn paint_added_rows(
    rows: &[RowFact],
    theme: Theme,
    width: usize,
    fill: bool,
) -> PaintedDiff {
    paint_rows_with_layout(
        rows,
        theme,
        width,
        NEW_FILE_LEFT,
        fill,
        GutterLayout::NewOnly,
    )
}

fn paint_rows_with_layout(
    rows: &[RowFact],
    theme: Theme,
    width: usize,
    left: usize,
    fill: bool,
    layout: GutterLayout,
) -> PaintedDiff {
    let digits = match layout {
        GutterLayout::Dual => rows
            .iter()
            .flat_map(|row| [row.old, row.new])
            .flatten()
            .map(|number| number.to_string().len())
            .max()
            .map(|digits| digits.max(MIN_GUTTER_DIGITS)),
        GutterLayout::NewOnly => rows
            .iter()
            .filter_map(|row| row.new)
            .map(|number| number.to_string().len())
            .max(),
    };
    let gutter_width = match (layout, digits) {
        (GutterLayout::Dual, Some(digits)) => digits * 2 + 1,
        (GutterLayout::NewOnly, Some(digits)) => digits,
        (_, None) => 0,
    };
    let sign_col = left
        + gutter_width
        + match layout {
            GutterLayout::Dual => GUTTER_GAP,
            GutterLayout::NewOnly => NEW_FILE_GUTTER_GAP,
        };

    PaintedDiff {
        rows: rows
            .iter()
            .map(|row| paint_row(row, digits, sign_col, layout, theme, width, fill))
            .collect(),
    }
}

fn paint_row(
    row: &RowFact,
    digits: Option<usize>,
    sign_col: usize,
    layout: GutterLayout,
    theme: Theme,
    width: usize,
    fill_row: bool,
) -> Vec<Line<'static>> {
    let style = match row.kind {
        RowKind::Meta => theme.diff_meta(),
        RowKind::Context => theme.diff_context(),
        RowKind::Added => theme.diff_added(),
        RowKind::Removed => theme.diff_removed(),
    };
    let sign_gap = match layout {
        GutterLayout::Dual => 0,
        GutterLayout::NewOnly => NEW_FILE_SIGN_GAP,
    };
    let (sign, content, content_col) = match row.kind {
        RowKind::Added => ('+', strip_prefix(&row.text, '+'), sign_col + 1 + sign_gap),
        RowKind::Removed => ('-', strip_prefix(&row.text, '-'), sign_col + 1 + sign_gap),
        RowKind::Context => (' ', strip_prefix(&row.text, ' '), sign_col + 1 + sign_gap),
        RowKind::Meta => (' ', row.text.as_str(), sign_col),
    };
    let content = content.replace('\t', "    ");
    let content_width = width.saturating_sub(content_col).max(1);
    let mut rest = content.as_str();
    let mut first = true;
    let mut lines = Vec::new();

    loop {
        let head = clip_to_width(rest, content_width);
        let (chunk, remainder) = if head.is_empty() && !rest.is_empty() {
            let end = rest
                .grapheme_indices(true)
                .next()
                .map(|(_, grapheme)| grapheme.len())
                .unwrap_or(0);
            rest.split_at(end)
        } else {
            rest.split_at(head.len())
        };
        let mut line = Line::default();
        if first {
            if let Some(digits) = digits {
                let (col, numbers) = match layout {
                    GutterLayout::Dual => (
                        sign_col - (digits * 2 + 1 + GUTTER_GAP),
                        format!(
                            "{:>digits$} {:>digits$}",
                            row.old.map(|number| number.to_string()).unwrap_or_default(),
                            row.new.map(|number| number.to_string()).unwrap_or_default(),
                        ),
                    ),
                    GutterLayout::NewOnly => (
                        sign_col - (digits + NEW_FILE_GUTTER_GAP),
                        format!(
                            "{:>digits$}",
                            row.new.map(|number| number.to_string()).unwrap_or_default()
                        ),
                    ),
                };
                push_span(&mut line, col, numbers, theme.gutter());
            }
            if row.kind != RowKind::Meta {
                push_span(&mut line, sign_col, sign.to_string(), style);
            }
        }
        push_span(&mut line, content_col, chunk.to_string(), style);
        if fill_row {
            fill(&mut line, width, style);
        }
        lines.push(line);
        first = false;
        rest = remainder;
        if rest.is_empty() {
            break;
        }
    }

    lines
}

fn strip_prefix(text: &str, prefix: char) -> &str {
    text.strip_prefix(prefix).unwrap_or(text)
}

fn fill(line: &mut Line<'static>, width: usize, style: ratatui::style::Style) {
    let used = line_len(line);
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), style));
    }
}

/// The full diff body for the reader. Scrolling, not this painter, is its
/// size policy.
pub fn reader_rows(document: &DiffDocument, width: usize, theme: Theme) -> Vec<Line<'static>> {
    paint_rows(&document.document.rows(), theme, width, 4, false).into_lines()
}

#[cfg(test)]
mod tests {
    use amux_ui::diff::{Document, Hunk, Numbering};

    use super::*;

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn rows(numbering: Numbering, hunks: Vec<Hunk>) -> Vec<RowFact> {
        Document {
            numbering,
            hunks,
            truncated: false,
        }
        .rows()
    }

    fn hunk(old_start: u32, new_start: u32, lines: &[&str]) -> Hunk {
        Hunk {
            old_start,
            new_start,
            header: None,
            lines: lines.iter().map(|line| line.to_string()).collect(),
        }
    }

    #[test]
    fn old_and_new_gutters_advance_independently() {
        let rows = rows(
            Numbering::Absolute,
            vec![hunk(14, 20, &[" ctx", "-old", "+new", "+extra"])],
        );
        let lines = paint_rows(&rows, Theme::default(), 60, 0, false).into_lines();
        assert_eq!(
            text_of(&lines),
            vec![
                "      @@ -14,2 +20,3 @@",
                "14 20  ctx",
                "15    -old",
                "   21 +new",
                "   22 +extra",
            ]
        );
    }

    #[test]
    fn numberless_rows_never_pay_for_empty_gutters() {
        let rows = rows(Numbering::None, vec![hunk(1, 1, &[" ctx", "-old", "+new"])]);
        let lines = paint_rows(&rows, Theme::default(), 40, 0, false).into_lines();
        assert_eq!(text_of(&lines), vec!["  ctx", " -old", " +new"]);
    }

    #[test]
    fn wrapped_continuations_have_blank_gutters_and_count_as_screen_rows() {
        let rows = vec![RowFact {
            old: None,
            new: Some(9),
            kind: RowKind::Added,
            text: "+abcdefghijklmnopqrstuvwxyz".into(),
        }];
        let painted = paint_rows(&rows, Theme::default(), 20, 0, false);
        assert_eq!(painted.screen_len(), 2);
        assert_eq!(
            text_of(&painted.into_lines()),
            vec!["    9 +abcdefghijklm", "       nopqrstuvwxyz",]
        );
    }

    #[test]
    fn preview_budget_counts_wraps_and_reserves_the_remainder_row() {
        let rows = vec![
            RowFact {
                old: None,
                new: None,
                kind: RowKind::Added,
                text: "+abcdefghijkl".into(),
            },
            RowFact {
                old: None,
                new: None,
                kind: RowKind::Added,
                text: "+tail".into(),
            },
            RowFact {
                old: None,
                new: None,
                kind: RowKind::Added,
                text: "+last".into(),
            },
        ];
        let preview = paint_rows(&rows, Theme::default(), 10, 0, false).into_preview(3);
        assert_eq!(
            preview.lines.len(),
            2,
            "the wrapped first row consumes two rows"
        );
        assert_eq!(preview.hidden, 2);
    }

    #[test]
    fn preview_keeps_screen_lines_when_the_first_source_row_exceeds_the_budget() {
        let rows = vec![RowFact {
            old: None,
            new: Some(9),
            kind: RowKind::Added,
            text: format!("+{}", "x".repeat(1_000)),
        }];
        let preview = paint_rows(&rows, Theme::default(), 120, 0, false).into_preview(8);

        assert_eq!(preview.lines.len(), 7);
        assert_eq!(preview.hidden, 1);
        assert!(text_of(&preview.lines)[0].starts_with("    9 +"));
    }
}
