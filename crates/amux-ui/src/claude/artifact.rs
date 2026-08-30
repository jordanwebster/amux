//! Ask-time artifacts (`docs/CHAT.md` §Diffs and the reader's artifacts).
//!
//! Two diff producers exist in the design, both Claude-layer folds: the
//! post-hoc restatement of `toolUseResult.structuredPatch` (FACT — the
//! transcript states every landed edit, and V1 consumes it only as the
//! feed line's magnitude), and the ask-time preview computed here — the
//! ONE place a diff is ever computed, because at ask time the transcript
//! states no diff at all: the hook carries only `old_string`/`new_string`
//! (`docs/CHAT.md` §Diffs). Absolute line numbers are
//! unavailable at ask time (locating the snippet would require reading the
//! file, and a chat client can be relay-remote), so ask-time artifacts are
//! numberless and their magnitude is an ESTIMATE — exact for single-site
//! edits, wrong under `replace_all`, which therefore states
//! "replaces every occurrence" instead of counts.
//!
//! The hunk model is jsdiff's `structuredPatch` shape (prefix char
//! embedded in each line), so one renderer serves both producers when the
//! post-hoc reading door opens.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// How many unchanged lines surround a change in the computed preview —
/// jsdiff's own default, and what every observed `structuredPatch` uses.
const CONTEXT_LINES: usize = 3;

/// One hunk in jsdiff's `structuredPatch` shape: `lines` carry the prefix
/// character embedded in the string — `' '` context, `'-'` removed, `'+'`
/// added (any other prefix renders dim verbatim, tolerate-unknown).
/// `old_start`/`new_start` are 1-based line numbers of the hunk's first
/// row; for [`DiffNumbering::None`] artifacts they are snippet-relative
/// and never rendered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<String>,
}

/// Whether the hunks carry file-absolute line numbers (`structuredPatch`,
/// FACT) or none (ask-time — numbers are not a fact before the tool runs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffNumbering {
    Absolute,
    None,
}

/// The `(+a -r)` counts with their epistemic status (the fact-vs-inferred
/// discipline applies to diffs too, diff-rendering §1.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "magnitude", rename_all = "snake_case")]
pub enum DiffMagnitude {
    /// Prefix counts over `structuredPatch` (FACT).
    Fact { added: u64, removed: u64 },
    /// Counts over the computed snippet diff (INFERRED — exact for
    /// single-site edits).
    Estimated { added: u64, removed: u64 },
    /// `replace_all`: N sites change and the snippet counts would lie —
    /// state the semantics instead.
    ReplacesEveryOccurrence,
}

/// A renderable diff: hunks plus how to number and how to state magnitude.
/// The renderer is a pure function over this — one renderer, both
/// producers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffArtifact {
    pub numbering: DiffNumbering,
    pub magnitude: DiffMagnitude,
    pub hunks: Vec<DiffHunk>,
}

impl DiffArtifact {
    /// Total diff body rows across all hunks — the remainder-line
    /// arithmetic's base (`⋮ +K more lines`).
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }
}

/// The typed body a permission ask carries (C2): computed once at ask
/// creation in the fold and retained WITH the ask — ask-time artifacts
/// live with their ask (evict bytes, never obligations), and nothing else
/// retains them in V1. Plan asks carry no entry here: the plan markdown
/// already rides `ToolInvocation::Plan`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "artifact", rename_all = "snake_case")]
pub enum AskArtifact {
    /// An Edit ask's mini-diff of `old_string` → `new_string`.
    Diff(DiffArtifact),
    /// A Write ask's proposed content. Create-vs-overwrite is unknowable
    /// before the tool runs; the header claims neither.
    NewFile { content: String },
}

/// The ask-time Edit preview: a line diff of the two snippets, grouped
/// with jsdiff-conventional context, numberless. Edit's uniqueness
/// requirement means `old_string` naturally carries its own context
/// lines, so the small snippet diff reads like a real hunk.
pub(crate) fn ask_time_diff(old: &str, new: &str, replace_all: bool) -> DiffArtifact {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();
    let mut added = 0u64;
    let mut removed = 0u64;
    for group in diff.grouped_ops(CONTEXT_LINES) {
        let Some(first) = group.first() else { continue };
        let old_start = first.old_range().start as u32 + 1;
        let new_start = first.new_range().start as u32 + 1;
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => {
                        removed += 1;
                        '-'
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        '+'
                    }
                };
                let text = change.value();
                let text = text.strip_suffix('\n').unwrap_or(text);
                let text = text.strip_suffix('\r').unwrap_or(text);
                lines.push(format!("{sign}{text}"));
                // A ± row that does not end in a newline is a real
                // difference the stripped text can no longer show: an
                // edit adding only a final newline would otherwise render
                // as visually identical -/+ rows. State it the way jsdiff
                // and git do — a marker row the renderer shows dim
                // verbatim (its `\` prefix is outside the sign
                // vocabulary, so it takes no line number and the meta
                // tone). Context rows are exempt: an unchanged EOF
                // missing its newline on both sides states no difference
                // the approval needs.
                if change.missing_newline() && change.tag() != ChangeTag::Equal {
                    lines.push("\\ No newline at end of file".to_string());
                }
            }
        }
        hunks.push(DiffHunk {
            old_start,
            new_start,
            lines,
        });
    }
    DiffArtifact {
        numbering: DiffNumbering::None,
        magnitude: if replace_all {
            DiffMagnitude::ReplacesEveryOccurrence
        } else {
            DiffMagnitude::Estimated { added, removed }
        },
        hunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_site_edit_computes_one_context_grouped_hunk() {
        let old = "pub struct RetryConfig {\n    pub max_attempts: u8,\n    pub base_delay: Duration,\n}\n";
        let new = "pub struct RetryConfig {\n    pub max_attempts: u8,        // capped at 6\n    pub jitter_ms: u16,\n    pub base_delay: Duration,\n}\n";
        let artifact = ask_time_diff(old, new, false);
        assert_eq!(artifact.numbering, DiffNumbering::None);
        assert_eq!(
            artifact.magnitude,
            DiffMagnitude::Estimated {
                added: 2,
                removed: 1
            }
        );
        assert_eq!(artifact.hunks.len(), 1);
        assert_eq!(
            artifact.hunks[0].lines,
            vec![
                " pub struct RetryConfig {",
                "-    pub max_attempts: u8,",
                "+    pub max_attempts: u8,        // capped at 6",
                "+    pub jitter_ms: u16,",
                "     pub base_delay: Duration,",
                " }",
            ],
            "unified rows with the prefix embedded, trailing newlines stripped"
        );
        assert_eq!(artifact.line_count(), 6);
    }

    #[test]
    fn distant_changes_group_into_separate_hunks() {
        let old: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 1\n", "line 1 changed\n")
            .replace("line 18\n", "line 18 changed\n");
        let artifact = ask_time_diff(&old, &new, false);
        assert_eq!(
            artifact.hunks.len(),
            2,
            "changes further apart than the context width form two hunks"
        );
        assert_eq!(artifact.hunks[0].old_start, 1);
        assert_eq!(
            artifact.hunks[1].old_start, 16,
            "the second hunk opens at its own context start"
        );
        assert_eq!(
            artifact.magnitude,
            DiffMagnitude::Estimated {
                added: 2,
                removed: 2
            }
        );
    }

    /// An edit differing only by a final newline must not render as
    /// visually identical -/+ rows: the missing-newline fact becomes the
    /// jsdiff/git marker row, so the user can tell what they are
    /// approving.
    #[test]
    fn a_newline_only_edit_states_the_missing_newline() {
        let artifact = ask_time_diff("VALUE=1", "VALUE=1\n", false);
        assert_eq!(artifact.hunks.len(), 1);
        assert_eq!(
            artifact.hunks[0].lines,
            vec!["-VALUE=1", "\\ No newline at end of file", "+VALUE=1",]
        );
        assert_eq!(
            artifact.magnitude,
            DiffMagnitude::Estimated {
                added: 1,
                removed: 1
            },
            "the marker is a statement, not a change row"
        );

        // The other direction: the NEW text is the one missing its
        // newline — the marker follows the added row.
        let artifact = ask_time_diff("VALUE=1\n", "VALUE=2", false);
        assert_eq!(
            artifact.hunks[0].lines,
            vec!["-VALUE=1", "+VALUE=2", "\\ No newline at end of file",]
        );
    }

    #[test]
    fn replace_all_states_semantics_instead_of_counts() {
        let artifact = ask_time_diff("a\n", "b\n", true);
        assert_eq!(artifact.magnitude, DiffMagnitude::ReplacesEveryOccurrence);
        assert_eq!(artifact.hunks.len(), 1, "the snippet diff still renders");
    }
}
