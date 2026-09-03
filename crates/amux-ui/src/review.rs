//! Review facts and comments shared by every client renderer.

use amux::{ArtifactId, BaseIdentity, DiffBase, DiffFile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::diff::{RowFact, RowKind, parse_unified_patch};

const COMMENT_TEXT_PREFIX: &str = "text-bytes: ";

/// Where a comment sorts when the document no longer holds its rows:
/// after every row that is still there.
const LAST_ROW: RowRef = RowRef {
    file: usize::MAX,
    row: usize::MAX,
};

/// A multi-file patch frozen with the repository identity it was derived from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewDocument {
    pub files: Vec<ReviewFile>,
    pub identity: BaseIdentity,
}

/// One file's magnitudes, numbered rows, and hunk-header positions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub rows: Vec<RowFact>,
    pub hunk_starts: Vec<usize>,
}

/// A stable address into a frozen review document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RowRef {
    pub file: usize,
    pub row: usize,
}

/// The side of a unified diff to which a review endpoint refers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "old" => Some(Self::Old),
            "new" => Some(Self::New),
            _ => None,
        }
    }
}

/// A comment with stable old/new-side endpoints and the exact reviewed rows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewComment {
    pub path: String,
    pub start_side: Side,
    pub start_line: u32,
    pub side: Side,
    pub line: u32,
    pub quoted: Vec<String>,
    pub text: String,
}

/// The resolved location and quoted rows for a new comment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Anchor {
    pub path: String,
    pub start_side: Side,
    pub start_line: u32,
    pub side: Side,
    pub line: u32,
    pub quoted: Vec<String>,
}

/// Repository identity and artifact reference carried by a review mention.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewHeader {
    pub diff: ArtifactId,
    pub base: String,
    pub head: String,
    pub merge_base: Option<String>,
    pub blobs: Vec<(String, String)>,
}

/// A frozen document and its editable, document-ordered comment set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Review {
    doc: ReviewDocument,
    base: DiffBase,
    diff: ArtifactId,
    comments: Vec<ReviewComment>,
}

impl Review {
    pub fn new(doc: ReviewDocument, diff: ArtifactId) -> Self {
        let base = doc.identity.base.clone();
        Self {
            doc,
            base,
            diff,
            comments: Vec::new(),
        }
    }

    /// A frozen document with a comment set already written elsewhere.
    ///
    /// A viewer reading a review someone else sent has the comments as
    /// text and the diff as an artifact; it never authored an anchor, so
    /// it needs a way in that does not go through `add`.
    pub fn with_comments(
        doc: ReviewDocument,
        diff: ArtifactId,
        comments: Vec<ReviewComment>,
    ) -> Self {
        let mut review = Self::new(doc, diff);
        for comment in comments {
            let key = comment_row(&review.doc, &comment).unwrap_or(LAST_ROW);
            let index = review.comments.partition_point(|existing| {
                comment_row(&review.doc, existing).unwrap_or(LAST_ROW) <= key
            });
            review.comments.insert(index, comment);
        }
        review
    }

    pub fn document(&self) -> &ReviewDocument {
        &self.doc
    }

    /// Adds a comment at the position implied by its first endpoint.
    pub fn add(&mut self, anchor: Anchor, text: String) -> usize {
        let comment = ReviewComment {
            path: anchor.path,
            start_side: anchor.start_side,
            start_line: anchor.start_line,
            side: anchor.side,
            line: anchor.line,
            quoted: anchor.quoted,
            text,
        };
        let key = comment_row(&self.doc, &comment).unwrap_or(LAST_ROW);
        let index = self.comments.partition_point(|existing| {
            comment_row(&self.doc, existing).unwrap_or(LAST_ROW) <= key
        });
        self.comments.insert(index, comment);
        index
    }

    pub fn edit(&mut self, index: usize, text: String) -> Result<(), ReviewError> {
        let comment = self
            .comments
            .get_mut(index)
            .ok_or(ReviewError::NoSuchComment { index })?;
        comment.text = text;
        Ok(())
    }

    pub fn delete(&mut self, index: usize) -> Result<ReviewComment, ReviewError> {
        if index >= self.comments.len() {
            return Err(ReviewError::NoSuchComment { index });
        }
        Ok(self.comments.remove(index))
    }

    pub fn comments(&self) -> &[ReviewComment] {
        &self.comments
    }

    pub fn comment_count(&self) -> usize {
        self.comments.len()
    }

    pub fn comments_in(&self, file: usize) -> usize {
        let Some(file) = self.doc.files.get(file) else {
            return 0;
        };
        self.comments
            .iter()
            .filter(|comment| comment.path == file.path)
            .count()
    }

    /// The row each comment is rendered under, in comment order. A comment
    /// whose rows the document no longer holds has none.
    pub fn comment_rows(&self) -> Vec<Option<RowRef>> {
        self.comments
            .iter()
            .map(|comment| comment_row(&self.doc, comment))
            .collect()
    }

    /// Returns the row under which each comment thread is rendered.
    pub fn rows_with_comments(&self) -> Vec<RowRef> {
        let mut rows = self
            .comments
            .iter()
            .filter_map(|comment| comment_row(&self.doc, comment))
            .collect::<Vec<_>>();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    pub fn body(&self) -> String {
        format_body(&self.header(), &self.comments)
    }

    pub fn header(&self) -> ReviewHeader {
        ReviewHeader {
            diff: self.diff.clone(),
            base: format_base(&self.base),
            head: self.doc.identity.head.clone(),
            merge_base: self.doc.identity.merge_base.clone(),
            blobs: self.doc.identity.blobs.clone(),
        }
    }
}

/// Parses a complete git patch into file-local rows using the shared diff walker.
pub fn parse_patch(
    patch: &str,
    identity: BaseIdentity,
    files: &[DiffFile],
) -> Result<ReviewDocument, ReviewError> {
    let sections = patch_sections(patch)?;
    if sections.len() != files.len() {
        return Err(ReviewError::MalformedPatch {
            line: patch.lines().count().max(1),
        });
    }

    let mut review_files = Vec::with_capacity(files.len());
    for (section, file) in sections.into_iter().zip(files) {
        let document = parse_unified_patch(&section.text, false);
        if document.truncated {
            return Err(ReviewError::MalformedPatch {
                line: section.start_line,
            });
        }
        let mut hunk_starts = Vec::with_capacity(document.hunks.len());
        let mut row = 0;
        for hunk in &document.hunks {
            hunk_starts.push(row);
            row += 1 + hunk.lines.len();
        }
        let rows = document.rows();
        let added = rows.iter().filter(|row| row.kind == RowKind::Added).count() as u32;
        let removed = rows
            .iter()
            .filter(|row| row.kind == RowKind::Removed)
            .count() as u32;
        if added != file.added || removed != file.removed {
            return Err(ReviewError::MalformedPatch {
                line: section.start_line,
            });
        }
        review_files.push(ReviewFile {
            path: file.path.clone(),
            added: file.added,
            removed: file.removed,
            rows,
            hunk_starts,
        });
    }

    Ok(ReviewDocument {
        files: review_files,
        identity,
    })
}

/// Parses a patch that arrived on its own, taking the file list from the
/// patch itself.
///
/// A viewer that fetched a stored review diff has no separate list of
/// changed files to check it against: the artifact is the whole record of
/// what was reviewed, and its section headers are the only statement of
/// which paths it covers.
pub fn parse_stored_patch(
    patch: &str,
    identity: BaseIdentity,
) -> Result<ReviewDocument, ReviewError> {
    let files = stated_files(patch)?;
    parse_patch(patch, identity, &files)
}

/// The base a review header spells out, read back.
///
/// Anything that is not a branch spelling is the working tree, because
/// that is what a review with no branch named was taken against.
pub fn parse_base(spelling: &str) -> DiffBase {
    match spelling.strip_prefix("branch:") {
        Some(base) => DiffBase::Branch {
            base: base.to_string(),
        },
        None => DiffBase::WorkingTree,
    }
}

/// The path and magnitude each section of a patch states about itself.
fn stated_files(patch: &str) -> Result<Vec<DiffFile>, ReviewError> {
    let mut files = Vec::new();
    for section in patch_sections(patch)? {
        let path = section_path(&section.text).ok_or(ReviewError::MalformedPatch {
            line: section.start_line,
        })?;
        let rows = parse_unified_patch(&section.text, false).rows();
        files.push(DiffFile {
            path,
            added: rows.iter().filter(|row| row.kind == RowKind::Added).count() as u32,
            removed: rows
                .iter()
                .filter(|row| row.kind == RowKind::Removed)
                .count() as u32,
        });
    }
    Ok(files)
}

/// The path a section is about: the new side, or the old side for a file
/// the patch deletes.
fn section_path(section: &str) -> Option<String> {
    let mut old = None;
    for line in section.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            return Some(path.to_string());
        }
        if let Some(path) = line.strip_prefix("--- a/") {
            old = Some(path.to_string());
        }
        if line.starts_with("@@") {
            break;
        }
    }
    old
}

/// Resolves an inclusive, file-local row range into review coordinates.
pub fn anchor(doc: &ReviewDocument, from: RowRef, to: RowRef) -> Result<Anchor, ReviewError> {
    if from.file != to.file {
        return Err(ReviewError::NotAnchorable { row: to });
    }
    let file = doc
        .files
        .get(from.file)
        .ok_or(ReviewError::NotAnchorable { row: from })?;
    let (start, end) = if from.row <= to.row {
        (from, to)
    } else {
        (to, from)
    };
    let start_row = file
        .rows
        .get(start.row)
        .ok_or(ReviewError::NotAnchorable { row: start })?;
    let end_row = file
        .rows
        .get(end.row)
        .ok_or(ReviewError::NotAnchorable { row: end })?;
    let (start_side, start_line) = endpoint(start_row, start)?;
    let (side, line) = endpoint(end_row, end)?;
    let quoted = file.rows[start.row..=end.row]
        .iter()
        .map(|row| row.text.clone())
        .collect();
    Ok(Anchor {
        path: file.path.clone(),
        start_side,
        start_line,
        side,
        line,
        quoted,
    })
}

/// Parses comments from a review body and verifies its blob identity line.
pub fn parse_body(header: &ReviewHeader, body: &str) -> Result<Vec<ReviewComment>, ReviewError> {
    let parsed = parse_body_parts(body)?;
    if parsed.blobs != header.blobs {
        return Err(ReviewError::MalformedBody { line: 1 });
    }
    Ok(parsed.comments)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ReviewError {
    #[error("review row {row:?} cannot be anchored")]
    NotAnchorable { row: RowRef },
    #[error("review has no comment at index {index}")]
    NoSuchComment { index: usize },
    #[error("malformed patch at line {line}")]
    MalformedPatch { line: usize },
    #[error("malformed review body at line {line}")]
    MalformedBody { line: usize },
}

pub(crate) fn format_body(header: &ReviewHeader, comments: &[ReviewComment]) -> String {
    let blobs = serde_json::to_string(&header.blobs).expect("review blob pairs serialize");
    let mut body = format!("blobs: {blobs}");
    for comment in comments {
        body.push_str("\n## ");
        body.push_str(&comment.path);
        body.push_str(" @@ ");
        body.push_str(comment.start_side.as_str());
        body.push(':');
        body.push_str(&comment.start_line.to_string());
        body.push_str("..");
        body.push_str(comment.side.as_str());
        body.push(':');
        body.push_str(&comment.line.to_string());
        for quoted in &comment.quoted {
            body.push_str("\n> ");
            body.push_str(quoted);
        }
        body.push('\n');
        body.push_str(COMMENT_TEXT_PREFIX);
        body.push_str(&comment.text.len().to_string());
        body.push('\n');
        body.push_str(&comment.text);
    }
    body
}

pub(crate) struct ParsedBody {
    pub(crate) blobs: Vec<(String, String)>,
    pub(crate) comments: Vec<ReviewComment>,
}

pub(crate) fn parse_body_parts(body: &str) -> Result<ParsedBody, ReviewError> {
    let (blobs_line, mut cursor, has_comments) = match body.find('\n') {
        Some(end) => (&body[..end], end + 1, true),
        None => (body, body.len(), false),
    };
    let blobs = blobs_line
        .strip_prefix("blobs: ")
        .ok_or(ReviewError::MalformedBody { line: 1 })?;
    let blobs = serde_json::from_str(blobs).map_err(|_| ReviewError::MalformedBody { line: 1 })?;
    let mut comments = Vec::new();
    let mut line_number = 2;

    if has_comments && cursor == body.len() {
        return Err(ReviewError::MalformedBody { line: 2 });
    }

    while cursor < body.len() {
        let heading_line = line_number;
        let (heading, next, heading_has_newline) = body_line_at(body, cursor);
        let (path, start_side, start_line, side, line) =
            parse_heading(heading).ok_or(ReviewError::MalformedBody { line: heading_line })?;
        if !heading_has_newline {
            return Err(ReviewError::MalformedBody { line: heading_line });
        }
        cursor = next;
        line_number += 1;

        let mut quoted = Vec::new();
        let text_len = loop {
            let part_line = line_number;
            let (part, next, has_newline) = body_line_at(body, cursor);
            if let Some(row) = part.strip_prefix("> ") {
                quoted.push(row.to_string());
                if !has_newline {
                    return Err(ReviewError::MalformedBody { line: part_line });
                }
                cursor = next;
                line_number += 1;
                continue;
            }

            if quoted.is_empty() {
                return Err(ReviewError::MalformedBody { line: heading_line });
            }
            let length = part
                .strip_prefix(COMMENT_TEXT_PREFIX)
                .filter(|length| {
                    !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|length| length.parse::<usize>().ok())
                .ok_or(ReviewError::MalformedBody { line: part_line })?;
            if !has_newline {
                return Err(ReviewError::MalformedBody { line: part_line });
            }
            cursor = next;
            line_number += 1;
            break length;
        };

        let text_start = cursor;
        let text_end = text_start
            .checked_add(text_len)
            .filter(|end| *end <= body.len() && body.is_char_boundary(*end))
            .ok_or(ReviewError::MalformedBody { line: line_number })?;
        let text = body[text_start..text_end].to_string();
        line_number += text.bytes().filter(|byte| *byte == b'\n').count();
        cursor = text_end;

        if cursor < body.len() {
            if body.as_bytes()[cursor] != b'\n' {
                return Err(ReviewError::MalformedBody { line: line_number });
            }
            cursor += 1;
            line_number += 1;
            if cursor == body.len() {
                return Err(ReviewError::MalformedBody { line: line_number });
            }
        }

        comments.push(ReviewComment {
            path,
            start_side,
            start_line,
            side,
            line,
            quoted,
            text,
        });
    }

    Ok(ParsedBody { blobs, comments })
}

fn body_line_at(body: &str, start: usize) -> (&str, usize, bool) {
    match body[start..].find('\n') {
        Some(relative) => {
            let end = start + relative;
            (&body[start..end], end + 1, true)
        }
        None => (&body[start..], body.len(), false),
    }
}

struct PatchSection {
    start_line: usize,
    text: String,
}

fn patch_sections(patch: &str) -> Result<Vec<PatchSection>, ReviewError> {
    let mut sections = Vec::new();
    let mut current_start = None;
    let mut current = Vec::new();
    for (index, line) in patch.lines().enumerate() {
        let line_number = index + 1;
        if line.starts_with("diff --git ") {
            if let Some(start_line) = current_start.replace(line_number) {
                sections.push(PatchSection {
                    start_line,
                    text: current.join("\n"),
                });
                current.clear();
            }
        } else if current_start.is_none() {
            if line.trim().is_empty() {
                continue;
            }
            return Err(ReviewError::MalformedPatch { line: line_number });
        }
        if current_start.is_some() {
            current.push(line);
        }
    }
    if let Some(start_line) = current_start {
        sections.push(PatchSection {
            start_line,
            text: current.join("\n"),
        });
    }
    Ok(sections)
}

fn endpoint(row: &RowFact, row_ref: RowRef) -> Result<(Side, u32), ReviewError> {
    match row.kind {
        RowKind::Removed => row
            .old
            .map(|line| (Side::Old, line))
            .ok_or(ReviewError::NotAnchorable { row: row_ref }),
        RowKind::Added | RowKind::Context => row
            .new
            .map(|line| (Side::New, line))
            .ok_or(ReviewError::NotAnchorable { row: row_ref }),
        RowKind::Meta => Err(ReviewError::NotAnchorable { row: row_ref }),
    }
}

fn comment_row(doc: &ReviewDocument, comment: &ReviewComment) -> Option<RowRef> {
    let file = doc
        .files
        .iter()
        .position(|file| file.path == comment.path)?;
    let row = doc.files[file]
        .rows
        .iter()
        .position(|row| match comment.side {
            Side::Old => row.old == Some(comment.line),
            Side::New => row.new == Some(comment.line),
        })?;
    Some(RowRef { file, row })
}

fn format_base(base: &DiffBase) -> String {
    match base {
        DiffBase::WorkingTree => "working-tree".to_string(),
        DiffBase::Branch { base } => format!("branch:{base}"),
    }
}

fn parse_heading(heading: &str) -> Option<(String, Side, u32, Side, u32)> {
    let heading = heading.strip_prefix("## ")?;
    let (path, range) = heading.rsplit_once(" @@ ")?;
    let (start, end) = range.split_once("..")?;
    let (start_side, start_line) = parse_endpoint(start)?;
    let (side, line) = parse_endpoint(end)?;
    Some((path.to_string(), start_side, start_line, side, line))
}

fn parse_endpoint(endpoint: &str) -> Option<(Side, u32)> {
    let (side, line) = endpoint.split_once(':')?;
    Some((Side::parse(side)?, line.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use amux_artifacts::id_of;

    use super::*;

    const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 keep
-old
+new
 tail
@@ -10 +10 @@
-old ten
+new ten
diff --git a/deleted.txt b/deleted.txt
deleted file mode 100644
index 3333333..0000000
--- a/deleted.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..4444444
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+alpha
+beta";

    fn identity() -> BaseIdentity {
        BaseIdentity {
            base: DiffBase::WorkingTree,
            head: "4f2a9c1".into(),
            merge_base: None,
            blobs: vec![
                ("src/lib.rs".into(), "2222222".into()),
                ("new.txt".into(), "4444444".into()),
            ],
        }
    }

    fn files() -> Vec<DiffFile> {
        vec![
            DiffFile {
                path: "src/lib.rs".into(),
                added: 2,
                removed: 2,
            },
            DiffFile {
                path: "deleted.txt".into(),
                added: 0,
                removed: 2,
            },
            DiffFile {
                path: "new.txt".into(),
                added: 2,
                removed: 0,
            },
        ]
    }

    fn document() -> ReviewDocument {
        parse_patch(PATCH, identity(), &files()).unwrap()
    }

    /// A viewer that fetched only the stored patch has to reach the same
    /// document the daemon's file list produced, or the comments it hangs
    /// on the rows would land somewhere else.
    #[test]
    fn review_parse_stored_patch_reads_the_file_list_out_of_the_patch() {
        let stored = parse_stored_patch(PATCH, identity()).unwrap();
        assert_eq!(stored, document());
        assert_eq!(
            stored
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/lib.rs", "deleted.txt", "new.txt"],
        );
    }

    /// A review arriving as text carries its comments in element order;
    /// the reader shows them under the rows, so they have to sort into
    /// document order on the way in.
    #[test]
    fn review_with_comments_sorts_a_sent_comment_set_into_document_order() {
        let document = document();
        let late = anchor(
            &document,
            RowRef { file: 2, row: 2 },
            RowRef { file: 2, row: 2 },
        )
        .unwrap();
        let early = anchor(
            &document,
            RowRef { file: 0, row: 3 },
            RowRef { file: 0, row: 3 },
        )
        .unwrap();
        let comment = |anchor: Anchor, text: &str| ReviewComment {
            path: anchor.path,
            start_side: anchor.start_side,
            start_line: anchor.start_line,
            side: anchor.side,
            line: anchor.line,
            quoted: anchor.quoted,
            text: text.to_string(),
        };
        let review = Review::with_comments(
            document,
            id_of(b"patch"),
            vec![comment(late, "second"), comment(early, "first")],
        );
        assert_eq!(
            review
                .comments()
                .iter()
                .map(|comment| comment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"],
        );
        assert_eq!(review.comments_in(0), 1);
        assert_eq!(review.comments_in(2), 1);
    }

    /// The base a header spells out has to come back as the base a review
    /// was taken against, or a reader would attribute every branch review
    /// to the working tree.
    #[test]
    fn review_parse_base_round_trips_both_spellings() {
        for base in [
            DiffBase::WorkingTree,
            DiffBase::Branch {
                base: "main".into(),
            },
        ] {
            assert_eq!(parse_base(&format_base(&base)), base);
        }
    }

    #[test]
    fn review_parse_patch_keeps_files_hunks_and_both_coordinates() {
        let document = document();
        assert_eq!(document.files.len(), 3);
        assert_eq!(document.files[0].hunk_starts, vec![0, 5]);
        assert_eq!(document.files[0].rows[1].old, Some(1));
        assert_eq!(document.files[0].rows[1].new, Some(1));
        assert_eq!(document.files[0].rows[2].old, Some(2));
        assert_eq!(document.files[0].rows[2].new, None);
        assert_eq!(document.files[0].rows[3].old, None);
        assert_eq!(document.files[0].rows[3].new, Some(2));
        assert_eq!(document.files[1].rows[1].old, Some(1));
        assert_eq!(document.files[1].rows[1].new, None);
        assert_eq!(document.files[2].rows[1].old, None);
        assert_eq!(document.files[2].rows[1].new, Some(1));

        for (file_index, file) in document.files.iter().enumerate() {
            for (row_index, row) in file.rows.iter().enumerate() {
                let row_ref = RowRef {
                    file: file_index,
                    row: row_index,
                };
                if row.kind == RowKind::Meta {
                    assert_eq!(
                        anchor(&document, row_ref, row_ref),
                        Err(ReviewError::NotAnchorable { row: row_ref })
                    );
                } else {
                    let resolved = anchor(&document, row_ref, row_ref).unwrap();
                    assert_eq!(resolved.quoted, vec![row.text.clone()]);
                }
            }
        }
    }

    #[test]
    fn review_anchor_spans_old_and_new_rows_and_rejects_meta_endpoints() {
        let document = document();
        let selection = anchor(
            &document,
            RowRef { file: 0, row: 2 },
            RowRef { file: 0, row: 3 },
        )
        .unwrap();
        assert_eq!(selection.start_side, Side::Old);
        assert_eq!(selection.start_line, 2);
        assert_eq!(selection.side, Side::New);
        assert_eq!(selection.line, 2);
        assert_eq!(selection.quoted, vec!["-old", "+new"]);

        let meta = RowRef { file: 0, row: 0 };
        assert_eq!(
            anchor(&document, meta, RowRef { file: 0, row: 1 }),
            Err(ReviewError::NotAnchorable { row: meta })
        );
        assert_eq!(
            anchor(&document, RowRef { file: 0, row: 1 }, meta),
            Err(ReviewError::NotAnchorable { row: meta })
        );
    }

    #[test]
    fn review_comment_set_is_ordered_editable_and_body_round_trips() {
        let document = document();
        let diff = id_of(PATCH.as_bytes());
        let mut review = Review::new(document.clone(), diff.clone());
        let new_file = anchor(
            &document,
            RowRef { file: 2, row: 1 },
            RowRef { file: 2, row: 2 },
        )
        .unwrap();
        let changed = anchor(
            &document,
            RowRef { file: 0, row: 2 },
            RowRef { file: 0, row: 3 },
        )
        .unwrap();
        review.add(new_file, "Explain the new file.".into());
        assert_eq!(review.add(changed, "Use the existing helper.".into()), 0);

        assert_eq!(review.comment_count(), 2);
        assert_eq!(review.comments_in(0), 1);
        assert_eq!(review.comments_in(1), 0);
        assert_eq!(review.comments_in(2), 1);
        assert_eq!(
            review.rows_with_comments(),
            vec![RowRef { file: 0, row: 3 }, RowRef { file: 2, row: 2 }]
        );
        let header = review.header();
        assert_eq!(header.diff, diff);
        assert_eq!(header.base, "working-tree");
        assert_eq!(
            parse_body(&header, &review.body()).unwrap(),
            review.comments()
        );

        let old_body = review.body();
        review.edit(0, "Use a smaller helper.".into()).unwrap();
        assert_ne!(review.body(), old_body);
        assert!(review.body().contains("Use a smaller helper."));
        let removed = review.delete(1).unwrap();
        assert_eq!(removed.path, "new.txt");
        assert_eq!(review.comment_count(), 1);
        assert!(!review.body().contains("Explain the new file."));
        assert_eq!(
            review.edit(9, "missing".into()),
            Err(ReviewError::NoSuchComment { index: 9 })
        );
    }

    #[test]
    fn review_body_with_two_files_parses_back_equal() {
        let header = ReviewHeader {
            diff: id_of(PATCH.as_bytes()),
            base: "branch:main".into(),
            head: "2222222".into(),
            merge_base: Some("1111111".into()),
            blobs: identity().blobs,
        };
        let comments = vec![
            ReviewComment {
                path: "src/lib.rs".into(),
                start_side: Side::Old,
                start_line: 2,
                side: Side::New,
                line: 2,
                quoted: vec!["-old".into(), "+new".into()],
                text: "First line\nsecond line".into(),
            },
            ReviewComment {
                path: "new.txt".into(),
                start_side: Side::New,
                start_line: 1,
                side: Side::New,
                line: 2,
                quoted: vec!["+alpha".into(), "+beta".into()],
                text: "Why both?".into(),
            },
        ];
        let body = format_body(&header, &comments);
        assert_eq!(parse_body(&header, &body).unwrap(), comments);
    }

    #[test]
    fn review_body_round_trips_arbitrary_comment_text() {
        let header = ReviewHeader {
            diff: id_of(PATCH.as_bytes()),
            base: "working-tree".into(),
            head: "2222222".into(),
            merge_base: None,
            blobs: identity().blobs,
        };
        let texts = [
            "> this belongs to the comment",
            "before\n## src/looks-like-a-heading.rs @@ old:7..new:8\nafter",
            "",
            "first\n\nthird\n",
            "Unicode is counted in bytes: åæø",
        ];
        let comments = texts
            .into_iter()
            .enumerate()
            .map(|(index, text)| ReviewComment {
                path: format!("src/file-{index}.rs"),
                start_side: Side::New,
                start_line: index as u32 + 1,
                side: Side::New,
                line: index as u32 + 1,
                quoted: vec![format!("+row {index}")],
                text: text.into(),
            })
            .collect::<Vec<_>>();

        let body = format_body(&header, &comments);
        assert_eq!(parse_body(&header, &body).unwrap(), comments);
    }

    #[test]
    fn review_body_rejects_ambiguous_unframed_comment_text() {
        let header = ReviewHeader {
            diff: id_of(PATCH.as_bytes()),
            base: "working-tree".into(),
            head: "2222222".into(),
            merge_base: None,
            blobs: identity().blobs,
        };
        let blobs = serde_json::to_string(&header.blobs).unwrap();
        let body = format!(
            "blobs: {blobs}\n## src/lib.rs @@ old:2..new:2\n> -old\n> this could be quoted or comment text"
        );

        assert_eq!(
            parse_body(&header, &body),
            Err(ReviewError::MalformedBody { line: 4 })
        );
    }
}
