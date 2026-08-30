//! The diff body renderer (`docs/CHAT.md` §Diffs and the reader's
//! artifacts; `notes/chat-v1/diff-rendering.md` §4 is the layout spec).
//!
//! One pure renderer over the layer's hunk model, serving both producers
//! (the ask-time computed preview today, `structuredPatch` restatements
//! when the post-hoc reading door opens) and both surfaces (the docked
//! panel's budgeted preview and the fullscreen reader). Layout rules:
//! sign column then content; in numbered form the number column
//! right-aligns to the widest number and a replaced pair repeats its
//! number (`15 -` / `15 +` describe one position in two file versions);
//! long lines wrap — never horizontal scroll — with blank-gutter
//! continuation rows so the gutter never lies; tabs expand before width
//! math; `⋮` rows sit between hunks; the preview cut always states the
//! arithmetic (`⋮ +K more lines · f full diff`).
//!
//! Rows come back "open" (no padding, no right border): the frame
//! assembler finishes every line once, like the feed.

use amux_ui::claude::{DiffArtifact, DiffMagnitude};
use amux_ui::diff::{Hunk, Numbering};
use ratatui::style::Style;
use ratatui::text::Line;

use crate::render::{Theme, clip_to_width, push_span};

/// Left margin before the gutter (inside the border cell).
const MARGIN: usize = 2;
/// Gap between the number column and the sign column.
const NUMBER_GAP: usize = 2;

/// The docked panel's preview budget: at most this many screen rows
/// (wrapped rows count — the budget is screen rows, not diff rows),
/// remainder line included.
pub(crate) const PREVIEW_BUDGET: usize = 8;

/// `(+9 -2)` / `(replaces every occurrence)` — magnitude as the header
/// states it. Estimated counts render in the same form as facts; the
/// epistemic tag is Model state, not V1 chrome.
pub(crate) fn magnitude_text(magnitude: &DiffMagnitude) -> String {
    match magnitude {
        DiffMagnitude::Fact { added, removed } | DiffMagnitude::Estimated { added, removed } => {
            match (added, removed) {
                (0, 0) => "(±0)".to_string(),
                (added, 0) => format!("(+{added})"),
                (0, removed) => format!("(-{removed})"),
                (added, removed) => format!("(+{added} -{removed})"),
            }
        }
        DiffMagnitude::ReplacesEveryOccurrence => "(replaces every occurrence)".to_string(),
    }
}

/// One classified diff row, pre-wrap.
struct Row {
    /// Display number (old for `-`, new for `+`/context), when numbering
    /// is absolute.
    number: Option<u64>,
    sign: char,
    content: String,
    style_kind: RowKind,
}

enum RowKind {
    Added,
    Removed,
    Context,
    /// A prefix this build does not know: rendered dim verbatim, never a
    /// crash (tolerate-unknown, diff-rendering §1.1).
    Unknown,
}

impl Row {
    fn style(&self, theme: Theme) -> Style {
        match self.style_kind {
            RowKind::Added => theme.diff_added(),
            RowKind::Removed => theme.diff_removed(),
            RowKind::Context => theme.diff_context(),
            RowKind::Unknown => theme.diff_meta(),
        }
    }
}

/// Tabs expand before any width math (both subjects agree; the gutter
/// arithmetic would otherwise lie about cells).
fn expand_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

/// Classify one hunk, walking the line numbers from its starts: context
/// and `+` number by the NEW file, `-` by the OLD; a replaced pair repeats
/// its number.
fn hunk_rows(hunk: &Hunk, numbered: bool) -> Vec<Row> {
    let mut old = u64::from(hunk.old_start);
    let mut new = u64::from(hunk.new_start);
    hunk.lines
        .iter()
        .map(|line| {
            let (sign, content) = match line.chars().next() {
                Some(sign @ (' ' | '-' | '+')) => (sign, expand_tabs(&line[sign.len_utf8()..])),
                _ => ('\0', expand_tabs(line)),
            };
            let (number, style_kind) = match sign {
                ' ' => {
                    let n = new;
                    old += 1;
                    new += 1;
                    (Some(n), RowKind::Context)
                }
                '-' => {
                    let n = old;
                    old += 1;
                    (Some(n), RowKind::Removed)
                }
                '+' => {
                    let n = new;
                    new += 1;
                    (Some(n), RowKind::Added)
                }
                _ => (None, RowKind::Unknown),
            };
            Row {
                number: number.filter(|_| numbered),
                sign: if sign == '\0' { ' ' } else { sign },
                content,
                style_kind,
            }
        })
        .collect()
}

/// The number-gutter width: widest number the artifact will display
/// (zero when numberless).
fn gutter_width(artifact: &DiffArtifact) -> usize {
    if artifact.document.numbering != Numbering::Absolute {
        return 0;
    }
    artifact
        .document
        .hunks
        .iter()
        .flat_map(|hunk| hunk_rows(hunk, true))
        .filter_map(|row| row.number)
        .max()
        .map(|n| n.to_string().len())
        .unwrap_or(0)
}

/// Render one classified row as wrapped screen lines: gutter + sign +
/// content, continuations with a blank gutter indented to the content
/// column.
fn row_lines(row: &Row, gutter: usize, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let sign_col = 1 + MARGIN + gutter + NUMBER_GAP;
    let content_col = sign_col + 2;
    let content_width = width.saturating_sub(content_col + 1).max(1);
    let style = row.style(theme);

    let mut lines = Vec::new();
    let mut rest = row.content.as_str();
    let mut first = true;
    loop {
        let head = clip_to_width(rest, content_width);
        let (chunk, remainder) = if head.is_empty() && !rest.is_empty() {
            // A single grapheme wider than the row: emit it whole rather
            // than loop (finish_line clips defensively).
            let end = rest.chars().next().map(char::len_utf8).unwrap_or(0);
            rest.split_at(end)
        } else {
            rest.split_at(head.len())
        };
        let mut line = Line::default();
        if first {
            if let Some(number) = row.number {
                push_span(
                    &mut line,
                    1 + MARGIN + gutter - number.to_string().len(),
                    number.to_string(),
                    theme.diff_meta(),
                );
            }
            push_span(&mut line, sign_col, row.sign.to_string(), style);
            push_span(&mut line, content_col, chunk.to_string(), style);
        } else {
            // Blank-gutter continuation: the number column never lies
            // about rows.
            push_span(&mut line, content_col, chunk.to_string(), style);
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

/// The `⋮` gap row between hunks, indented into the gutter.
fn gap_line(gutter: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    let col = if gutter > 0 {
        // Indented into the number gutter, right-aligned.
        1 + MARGIN + gutter - 1
    } else {
        // Numberless: sits in the sign column.
        1 + MARGIN + NUMBER_GAP
    };
    push_span(&mut line, col, "⋮", theme.diff_meta());
    line
}

/// The remainder line: `⋮ +K more lines · <affordance>` — always states
/// the arithmetic (K = diff rows not shown).
fn remainder_line(hidden: usize, affordance: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(
        &mut line,
        1 + MARGIN + 1,
        format!("⋮  +{hidden} more lines · {affordance}"),
        theme.diff_meta(),
    );
    line
}

/// The full diff body for the reader: every hunk, `⋮` gaps, wrapped rows.
/// No internal truncation — scroll is the size policy.
pub fn reader_rows(artifact: &DiffArtifact, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let numbered = artifact.document.numbering == Numbering::Absolute;
    let gutter = gutter_width(artifact);
    let mut lines = Vec::new();
    for (index, hunk) in artifact.document.hunks.iter().enumerate() {
        if index > 0 {
            lines.push(gap_line(gutter, theme));
        }
        for row in hunk_rows(hunk, numbered) {
            lines.extend(row_lines(&row, gutter, width, theme));
        }
    }
    lines
}

/// A new file's content as a `+` block (`docs/CHAT.md`: Write asks and
/// Diff's create case share it): numbered `1..=N` in the reader,
/// numberless in the panel preview.
pub(crate) fn new_file_rows(
    content: &str,
    width: usize,
    theme: Theme,
    numbered: bool,
) -> Vec<Line<'static>> {
    let hunk = new_file_hunk(content);
    let gutter = if numbered {
        content.lines().count().to_string().len()
    } else {
        0
    };
    hunk_rows(&hunk, numbered)
        .iter()
        .flat_map(|row| row_lines(row, gutter, width, theme))
        .collect()
}

/// The panel's new-file preview: numberless `+` rows under the same
/// budget-and-remainder rule as the diff preview.
pub(crate) fn new_file_preview(
    content: &str,
    width: usize,
    theme: Theme,
    budget: usize,
) -> Vec<Line<'static>> {
    let budget = budget.max(1);
    let total = content.lines().count();
    let rows: Vec<Row> = hunk_rows(&new_file_hunk(content), false);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut shown = 0usize;
    let mut cut = false;
    for row in &rows {
        let wrapped = row_lines(row, 0, width, theme);
        let reserve = usize::from(shown + 1 < rows.len());
        if lines.len() + wrapped.len() > budget.saturating_sub(reserve) {
            cut = true;
            break;
        }
        lines.extend(wrapped);
        shown += 1;
    }
    if cut {
        lines.push(remainder_line(total - shown, "f full view", theme));
    }
    lines
}

fn new_file_hunk(content: &str) -> Hunk {
    Hunk {
        old_start: 1,
        new_start: 1,
        header: None,
        lines: content.lines().map(|line| format!("+{line}")).collect(),
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::diff::Document;

    use super::*;

    fn artifact(hunks: Vec<Hunk>, numbering: Numbering) -> DiffArtifact {
        DiffArtifact {
            document: Document {
                numbering,
                hunks,
                truncated: false,
            },
            magnitude: DiffMagnitude::Fact {
                added: 0,
                removed: 0,
            },
        }
    }

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

    fn hunk(old_start: u32, new_start: u32, lines: &[&str]) -> Hunk {
        Hunk {
            old_start,
            new_start,
            header: None,
            lines: lines.iter().map(|line| line.to_string()).collect(),
        }
    }

    /// The number walk: context and `+` by the new file, `-` by the old —
    /// so a replaced pair repeats its number (two file versions, one
    /// position).
    #[test]
    fn replaced_pairs_repeat_their_number() {
        let a = artifact(
            vec![hunk(
                14,
                14,
                &[" ctx", "-old line", "+new line", "+extra", " tail"],
            )],
            Numbering::Absolute,
        );
        let rows = text_of(&reader_rows(&a, 60, Theme::default()));
        assert_eq!(
            rows,
            vec![
                "   14    ctx",
                "   15  - old line",
                "   15  + new line",
                "   16  + extra",
                "   17    tail",
            ],
            "15 - / 15 + describe one position in two file versions"
        );
    }

    /// Numberless (ask-time) form: sign column only — numbers are not a
    /// fact before the tool runs.
    #[test]
    fn numberless_rows_carry_the_sign_column_only() {
        let a = artifact(vec![hunk(1, 1, &[" ctx", "-old", "+new"])], Numbering::None);
        let rows = text_of(&reader_rows(&a, 40, Theme::default()));
        assert_eq!(rows, vec!["       ctx", "     - old", "     + new"]);
    }

    /// Long lines wrap with a blank gutter — the number column never lies
    /// about rows — and wraps count against the preview budget.
    #[test]
    fn long_lines_wrap_with_a_blank_gutter() {
        let a = artifact(
            vec![hunk(9, 9, &["+abcdefghijklmnopqrstuvwxyz"])],
            Numbering::Absolute,
        );
        let rows = text_of(&reader_rows(&a, 20, Theme::default()));
        assert_eq!(rows[0], "   9  + abcdefghijk");
        assert_eq!(rows[1], "        lmnopqrstuv");
        assert_eq!(rows[2], "        wxyz");
    }

    /// A `⋮` gap separates hunks in the reader; the gutter width comes
    /// from the widest number in the whole patch.
    #[test]
    fn hunk_gaps_render_the_ellipsis_row() {
        let a = artifact(
            vec![
                hunk(2, 2, &[" a", "-b", "+B"]),
                hunk(140, 140, &[" y", "+z"]),
            ],
            Numbering::Absolute,
        );
        let rows = text_of(&reader_rows(&a, 40, Theme::default()));
        assert_eq!(
            rows,
            vec![
                "     2    a",
                "     3  - b",
                "     3  + B",
                "     ⋮",
                "   140    y",
                "   141  + z",
            ]
        );
    }

    /// A single hunk renders every row it has, context included.
    #[test]
    fn a_single_hunk_renders_every_row() {
        let a = artifact(
            vec![hunk(1, 1, &[" c1", "-old", "+new", " c2"])],
            Numbering::None,
        );
        let lines = reader_rows(&a, 60, Theme::default());
        assert_eq!(lines.len(), 4, "every row of the hunk, nothing hidden");
    }

    /// New-file blocks: numberless `+` rows in the panel, numbered in the
    /// reader; the preview budget cuts with the arithmetic stated.
    #[test]
    fn new_file_blocks_share_the_plus_gutter() {
        let content = "use std::time::Duration;\n\npub struct RetryPolicy;";
        let panel = text_of(&new_file_preview(content, 60, Theme::default(), 2));
        assert_eq!(
            panel,
            vec![
                "     + use std::time::Duration;",
                "    ⋮  +2 more lines · f full view",
            ]
        );
        let reader = text_of(&new_file_rows(content, 60, Theme::default(), true));
        assert_eq!(
            reader,
            vec![
                "   1  + use std::time::Duration;",
                "   2  + ",
                "   3  + pub struct RetryPolicy;",
            ]
        );
    }

    /// Unknown prefixes render dim verbatim, never a crash
    /// (tolerate-unknown — jsdiff can emit `\ No newline at end of file`).
    #[test]
    fn unknown_prefixes_render_verbatim() {
        let a = artifact(
            vec![hunk(1, 1, &["\\ No newline at end of file"])],
            Numbering::Absolute,
        );
        let rows = text_of(&reader_rows(&a, 60, Theme::default()));
        assert_eq!(rows, vec!["       \\ No newline at end of file"]);
    }

    /// Magnitude headers: counts for facts and estimates alike, semantics
    /// for replace-all.
    #[test]
    fn magnitude_text_states_counts_or_semantics() {
        assert_eq!(
            magnitude_text(&DiffMagnitude::Fact {
                added: 9,
                removed: 2
            }),
            "(+9 -2)"
        );
        assert_eq!(
            magnitude_text(&DiffMagnitude::Estimated {
                added: 2,
                removed: 0
            }),
            "(+2)"
        );
        assert_eq!(
            magnitude_text(&DiffMagnitude::ReplacesEveryOccurrence),
            "(replaces every occurrence)"
        );
    }
}
