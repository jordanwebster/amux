//! Claude-specific diff chrome: magnitude words and Write-content blocks
//! (`docs/CHAT.md` §Unified diffs and the reader's documents).
//!
//! Unified-diff row geometry is owned by `chat::diff`; this module only adapts
//! Claude documents whose presentation differs from a landed patch.

use amux_ui::claude::DiffMagnitude;
use amux_ui::diff::{RowFact, RowKind};
use ratatui::text::Line;

use crate::chat::diff;
use crate::render::{Theme, push_span};

/// The docked panel's preview budget: at most this many screen rows,
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

/// A new file's content as a `+` block: numbered in the reader, numberless in
/// the panel. This is presentation of a typed content document, not patch
/// parsing; unified diff rows still go through the shared painter.
pub(crate) fn new_file_rows(
    content: &str,
    width: usize,
    theme: Theme,
    numbered: bool,
) -> Vec<Line<'static>> {
    diff::paint_added_rows(&new_file_facts(content, numbered), theme, width, false).into_lines()
}

/// The panel's new-file preview under the same screen-row budget and
/// remainder rule as a diff preview.
pub(crate) fn new_file_preview(
    content: &str,
    width: usize,
    theme: Theme,
    budget: usize,
) -> Vec<Line<'static>> {
    let preview = diff::paint_added_rows(&new_file_facts(content, false), theme, width, false)
        .into_preview(budget);
    let mut lines = preview.lines;
    if preview.hidden > 0 {
        lines.push(remainder_line(preview.hidden, "f full document", theme));
    }
    lines
}

fn new_file_facts(content: &str, numbered: bool) -> Vec<RowFact> {
    content
        .lines()
        .enumerate()
        .map(|(index, content)| RowFact {
            old: None,
            new: numbered.then(|| u32::try_from(index + 1).unwrap_or(u32::MAX)),
            kind: RowKind::Added,
            text: format!("+{content}"),
        })
        .collect()
}

fn remainder_line(hidden: usize, affordance: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(
        &mut line,
        4,
        format!("⋮  +{hidden} more lines · {affordance}"),
        theme.diff_meta(),
    );
    line
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn new_file_blocks_share_the_diff_painter_and_screen_budget() {
        let content = "use std::time::Duration;\n\npub struct RetryPolicy;";
        let panel: Vec<String> = text_of(&new_file_preview(content, 60, Theme::default(), 2))
            .into_iter()
            .map(|row| row.trim_end().to_string())
            .collect();
        assert_eq!(
            panel,
            vec![
                "     + use std::time::Duration;",
                "    ⋮  +2 more lines · f full document",
            ]
        );
        let reader: Vec<String> = text_of(&new_file_rows(content, 60, Theme::default(), true))
            .into_iter()
            .map(|row| row.trim_end().to_string())
            .collect();
        assert_eq!(
            reader,
            vec![
                "   1  + use std::time::Duration;",
                "   2  +",
                "   3  + pub struct RetryPolicy;",
            ]
        );
    }

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
