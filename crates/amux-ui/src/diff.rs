//! Unified-diff facts shared by clients.
//!
//! This module interprets the patch format and walks its independent old and
//! new coordinates. It deliberately has no rendering vocabulary: gutters,
//! wrapping, clipping, colours, and preview budgets belong to each client.

use serde::{Deserialize, Serialize};

/// Whether a document states file positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Numbering {
    /// Hunk starts are absolute positions in the old and new files.
    Absolute,
    /// Hunk starts are relative to an unlocated snippet and must not surface.
    None,
}

/// One unified-diff hunk in prefix-embedded form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    /// The exact `@@` header when the source supplied one. Documents built
    /// from structured hunks synthesize the equivalent header during the row
    /// walk, while numberless documents never invent coordinates.
    pub header: Option<String>,
    /// Rows retain their leading ` `, `-`, or `+`; unknown prefixes remain
    /// verbatim metadata.
    pub lines: Vec<String>,
}

/// A neutral unified-diff document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub numbering: Numbering,
    pub hunks: Vec<Hunk>,
    /// The supplied source was a bounded head rather than the complete patch.
    pub truncated: bool,
}

impl Document {
    /// Derive every numbered row in one walk over the retained hunks.
    pub fn rows(&self) -> Vec<RowFact> {
        let numbered = self.numbering == Numbering::Absolute;
        let mut rows = Vec::new();

        for (index, hunk) in self.hunks.iter().enumerate() {
            // Only *between* hunks. A boundary before the first one
            // separates a hunk from the file header above it, which is not
            // a discontinuity — and it is the row a reader's cursor lands
            // on when they open a file.
            if index > 0 {
                rows.push(RowFact::boundary());
            }

            let mut old = hunk.old_start;
            let mut new = hunk.new_start;
            for text in &hunk.lines {
                let (kind, old_row, new_row) = match text.as_bytes().first() {
                    Some(b' ') => {
                        let row = (RowKind::Context, Some(old), Some(new));
                        old = old.saturating_add(1);
                        new = new.saturating_add(1);
                        row
                    }
                    Some(b'+') => {
                        let row = (RowKind::Added, None, Some(new));
                        new = new.saturating_add(1);
                        row
                    }
                    Some(b'-') => {
                        let row = (RowKind::Removed, Some(old), None);
                        old = old.saturating_add(1);
                        row
                    }
                    _ => (RowKind::Note, None, None),
                };
                rows.push(RowFact {
                    old: old_row.filter(|_| numbered),
                    new: new_row.filter(|_| numbered),
                    kind,
                    text: text.clone(),
                });
            }
        }

        rows
    }

    /// Body rows exclude the synthetic or retained hunk headers.
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }
}

/// Format-level meaning of a derived row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// The boundary between two hunks. It carries no content of its own —
    /// what it says, that the next line is not the one after the last, the
    /// row numbers on either side of it already say.
    Boundary,
    /// A line inside a hunk that is not content: `\ No newline at end of
    /// file`. It is a fact about the patch and has to be shown.
    Note,
    Context,
    Added,
    Removed,
}

/// One row with independent old-file and new-file coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowFact {
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub kind: RowKind,
    pub text: String,
}

impl RowFact {
    /// The break between two hunks. It carries no text: what a reader needs
    /// from it is that there is a gap, and the row numbers on either side
    /// say how big.
    fn boundary() -> Self {
        Self {
            old: None,
            new: None,
            kind: RowKind::Boundary,
            text: String::new(),
        }
    }
}

/// Parse a bounded unified-patch head into typed hunks.
///
/// File headers and other preamble are ignored. A hunk is retained only when
/// it contains at least one valid body row. When its declared ranges are not
/// consumed, the observed prefix remains useful but the document is marked
/// truncated so no consumer can mistake it for the complete change.
pub fn parse_unified_patch(head: &str, truncated: bool) -> Document {
    let mut hunks = Vec::new();
    let mut pending: Option<PendingHunk> = None;
    let mut incomplete = false;

    for text in head.lines() {
        if let Some(ranges) = hunk_ranges(text) {
            if let Some(previous) = pending.take() {
                retain_observed_hunk(previous, &mut hunks, &mut incomplete);
            }
            pending = Some(PendingHunk::new(text, ranges));
            continue;
        }

        let Some(hunk) = pending.as_mut() else {
            continue;
        };
        if hunk.complete() && is_file_header(text) {
            hunks.push(pending.take().expect("the completed hunk exists").finish());
            continue;
        }
        if !hunk.push(text) {
            let previous = pending.take().expect("the pending hunk exists");
            retain_observed_hunk(previous, &mut hunks, &mut incomplete);
        }
    }

    if let Some(last) = pending {
        retain_observed_hunk(last, &mut hunks, &mut incomplete);
    }

    Document {
        numbering: Numbering::Absolute,
        hunks,
        truncated: truncated || incomplete,
    }
}

fn retain_observed_hunk(hunk: PendingHunk, hunks: &mut Vec<Hunk>, incomplete: &mut bool) {
    if !hunk.has_valid_prefix() {
        return;
    }
    *incomplete |= !hunk.complete();
    hunks.push(hunk.finish());
}

#[derive(Clone, Copy)]
struct Ranges {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

struct PendingHunk {
    header: String,
    ranges: Ranges,
    old_left: u32,
    new_left: u32,
    lines: Vec<String>,
    body_rows: usize,
}

impl PendingHunk {
    fn new(header: &str, ranges: Ranges) -> Self {
        Self {
            header: header.to_string(),
            ranges,
            old_left: ranges.old_count,
            new_left: ranges.new_count,
            lines: Vec::new(),
            body_rows: 0,
        }
    }

    fn push(&mut self, text: &str) -> bool {
        match text.as_bytes().first() {
            Some(b' ') if self.old_left > 0 && self.new_left > 0 => {
                self.old_left -= 1;
                self.new_left -= 1;
                self.body_rows += 1;
            }
            Some(b'+') if self.new_left > 0 => {
                self.new_left -= 1;
                self.body_rows += 1;
            }
            Some(b'-') if self.old_left > 0 => {
                self.old_left -= 1;
                self.body_rows += 1;
            }
            Some(b'\\') if self.body_rows > 0 => {}
            _ => return false,
        }
        self.lines.push(text.to_string());
        true
    }

    fn complete(&self) -> bool {
        self.body_rows > 0 && self.old_left == 0 && self.new_left == 0
    }

    fn has_valid_prefix(&self) -> bool {
        self.body_rows > 0
    }

    fn finish(self) -> Hunk {
        Hunk {
            old_start: self.ranges.old_start,
            new_start: self.ranges.new_start,
            header: Some(self.header),
            lines: self.lines,
        }
    }
}

fn hunk_ranges(header: &str) -> Option<Ranges> {
    let ranges = header.strip_prefix("@@ -")?;
    let (old, ranges) = ranges.split_once(" +")?;
    let (new, _) = ranges.split_once(" @@")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some(Ranges {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

fn is_file_header(text: &str) -> bool {
    text.starts_with("diff --git ") || text.starts_with("--- ") || text.starts_with("+++ ")
}
