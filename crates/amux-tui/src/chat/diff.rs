//! Shared numbered-row representation for Claude and Codex diffs.

use amux_ui::claude::{DiffArtifact, DiffNumbering};

pub use super::claude::diff::reader_rows;

/// Presentation class for one row of a unified diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DiffRowKind {
    Meta,
    Context,
    Added,
    Removed,
}

/// One pre-painted unified-diff row with independent old and new gutters.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct DiffRow {
    pub(crate) old: Option<u32>,
    pub(crate) new: Option<u32>,
    pub(crate) kind: DiffRowKind,
    pub(crate) text: String,
}

/// Convert Claude's typed hunks without interpreting their content.
#[allow(dead_code)]
pub(crate) fn diff_rows_from_claude(artifact: &DiffArtifact) -> Vec<DiffRow> {
    let numbered = artifact.numbering == DiffNumbering::Absolute;
    let mut rows = Vec::new();
    for hunk in &artifact.hunks {
        let old_count = hunk
            .lines
            .iter()
            .filter(|line| matches!(line.as_bytes().first(), Some(b' ' | b'-')))
            .count();
        let new_count = hunk
            .lines
            .iter()
            .filter(|line| matches!(line.as_bytes().first(), Some(b' ' | b'+')))
            .count();
        rows.push(DiffRow {
            old: None,
            new: None,
            kind: DiffRowKind::Meta,
            text: format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, old_count, hunk.new_start, new_count
            ),
        });

        let mut old = hunk.old_start;
        let mut new = hunk.new_start;
        for text in &hunk.lines {
            let (kind, old_row, new_row) = match text.as_bytes().first() {
                Some(b' ') => {
                    let row = (DiffRowKind::Context, Some(old), Some(new));
                    old = old.saturating_add(1);
                    new = new.saturating_add(1);
                    row
                }
                Some(b'+') => {
                    let row = (DiffRowKind::Added, None, Some(new));
                    new = new.saturating_add(1);
                    row
                }
                Some(b'-') => {
                    let row = (DiffRowKind::Removed, Some(old), None);
                    old = old.saturating_add(1);
                    row
                }
                _ => (DiffRowKind::Meta, None, None),
            };
            rows.push(DiffRow {
                old: old_row.filter(|_| numbered),
                new: new_row.filter(|_| numbered),
                kind,
                text: text.clone(),
            });
        }
    }
    rows
}

/// Parse the hunks in a Codex unified patch, ignoring header-only input.
#[allow(dead_code)]
pub(crate) fn diff_rows_from_patch(patch: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut counters = None;
    let mut last_was_body = false;
    for text in patch.lines() {
        if let Some(((old, old_left), (new, new_left))) = hunk_ranges(text) {
            rows.push(DiffRow {
                old: None,
                new: None,
                kind: DiffRowKind::Meta,
                text: text.to_string(),
            });
            counters = Some((old, new, old_left, new_left));
            last_was_body = false;
            continue;
        }

        let Some((old, new, old_left, new_left)) = counters.as_mut() else {
            if last_was_body && text.starts_with('\\') {
                rows.push(DiffRow {
                    old: None,
                    new: None,
                    kind: DiffRowKind::Meta,
                    text: text.to_string(),
                });
            }
            last_was_body = false;
            continue;
        };
        let (kind, old_row, new_row) = match text.as_bytes().first() {
            Some(b' ') if *old_left > 0 && *new_left > 0 => {
                let row = (DiffRowKind::Context, Some(*old), Some(*new));
                *old = old.saturating_add(1);
                *new = new.saturating_add(1);
                *old_left -= 1;
                *new_left -= 1;
                row
            }
            Some(b'+') if *new_left > 0 => {
                let row = (DiffRowKind::Added, None, Some(*new));
                *new = new.saturating_add(1);
                *new_left -= 1;
                row
            }
            Some(b'-') if *old_left > 0 => {
                let row = (DiffRowKind::Removed, Some(*old), None);
                *old = old.saturating_add(1);
                *old_left -= 1;
                row
            }
            Some(b'\\') => (DiffRowKind::Meta, None, None),
            _ => {
                counters = None;
                last_was_body = false;
                continue;
            }
        };
        rows.push(DiffRow {
            old: old_row,
            new: new_row,
            kind,
            text: text.to_string(),
        });
        last_was_body = kind != DiffRowKind::Meta;
        if *old_left == 0 && *new_left == 0 {
            counters = None;
        }
    }
    rows
}

#[allow(dead_code)]
fn hunk_ranges(header: &str) -> Option<((u32, u32), (u32, u32))> {
    let ranges = header.strip_prefix("@@ -")?;
    let (old, ranges) = ranges.split_once(" +")?;
    let (new, _) = ranges.split_once(" @@")?;
    Some((parse_range(old)?, parse_range(new)?))
}

#[allow(dead_code)]
fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::claude::{DiffHunk, DiffMagnitude};

    use super::*;

    fn artifact(numbering: DiffNumbering, hunks: Vec<DiffHunk>) -> DiffArtifact {
        DiffArtifact {
            numbering,
            magnitude: DiffMagnitude::Fact {
                added: 0,
                removed: 0,
            },
            hunks,
        }
    }

    #[test]
    fn claude_rows_classify_and_advance_gutters_across_hunks() {
        let rows = diff_rows_from_claude(&artifact(
            DiffNumbering::Absolute,
            vec![
                DiffHunk {
                    old_start: 10,
                    new_start: 20,
                    lines: vec![
                        " shared\ttext".into(),
                        "-old".into(),
                        "+new".into(),
                        " tail".into(),
                    ],
                },
                DiffHunk {
                    old_start: 40,
                    new_start: 70,
                    lines: vec!["-gone".into(), "+arrived".into()],
                },
            ],
        ));

        assert_eq!(rows[0].kind, DiffRowKind::Meta);
        assert_eq!(rows[0].text, "@@ -10,3 +20,3 @@");
        assert_eq!(
            rows[1],
            row(Some(10), Some(20), DiffRowKind::Context, " shared\ttext")
        );
        assert_eq!(rows[2], row(Some(11), None, DiffRowKind::Removed, "-old"));
        assert_eq!(rows[3], row(None, Some(21), DiffRowKind::Added, "+new"));
        assert_eq!(
            rows[4],
            row(Some(12), Some(22), DiffRowKind::Context, " tail")
        );
        assert_eq!(rows[5].kind, DiffRowKind::Meta);
        assert_eq!(rows[6], row(Some(40), None, DiffRowKind::Removed, "-gone"));
        assert_eq!(rows[7], row(None, Some(70), DiffRowKind::Added, "+arrived"));
    }

    #[test]
    fn claude_numberless_artifact_keeps_relative_numbers_out_of_the_gutter() {
        let rows = diff_rows_from_claude(&artifact(
            DiffNumbering::None,
            vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![" old".into(), "+new".into()],
            }],
        ));
        assert_eq!(rows[1].old, None);
        assert_eq!(rows[1].new, None);
        assert_eq!(rows[2].old, None);
        assert_eq!(rows[2].new, None);
    }

    #[test]
    fn codex_rows_parse_several_hunks_and_preserve_text_verbatim() {
        let rows = diff_rows_from_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -3,2 +8,3 @@ section\n same\n-old\n+new\tvalue\n+extra\n@@ -20 +30 @@\n-last\n+next",
        );

        assert_eq!(
            rows[0],
            row(None, None, DiffRowKind::Meta, "@@ -3,2 +8,3 @@ section")
        );
        assert_eq!(
            rows[1],
            row(Some(3), Some(8), DiffRowKind::Context, " same")
        );
        assert_eq!(rows[2], row(Some(4), None, DiffRowKind::Removed, "-old"));
        assert_eq!(
            rows[3],
            row(None, Some(9), DiffRowKind::Added, "+new\tvalue")
        );
        assert_eq!(rows[4], row(None, Some(10), DiffRowKind::Added, "+extra"));
        assert_eq!(rows[5], row(None, None, DiffRowKind::Meta, "@@ -20 +30 @@"));
        assert_eq!(rows[6], row(Some(20), None, DiffRowKind::Removed, "-last"));
        assert_eq!(rows[7], row(None, Some(30), DiffRowKind::Added, "+next"));
    }

    #[test]
    fn empty_or_headerless_patch_has_no_rows() {
        assert!(diff_rows_from_patch("").is_empty());
        assert!(diff_rows_from_patch("--- a/file\n+++ b/file\n-old\n+new").is_empty());
        assert!(diff_rows_from_patch("@@ -x,2 +1,2 @@\n-old\n+new").is_empty());
    }

    fn row(old: Option<u32>, new: Option<u32>, kind: DiffRowKind, text: &str) -> DiffRow {
        DiffRow {
            old,
            new,
            kind,
            text: text.to_string(),
        }
    }
}
