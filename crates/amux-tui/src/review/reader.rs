//! A review someone already sent, read rather than written.
//!
//! The message carried the comments; the diff they hang on is an artifact
//! that has to be fetched. Until it arrives — or if it is gone from the
//! agent's store altogether — the comments still say what was said and
//! quote the rows they were written on, so a review never reads as empty
//! just because its patch is out of reach.

use amux_ui::BaseIdentity;
use amux_ui::review::{Review, ReviewComment, ReviewHeader, Side, parse_base, parse_stored_patch};
use ratatui::text::Line;

use super::view::file_line;
use crate::chat::diff::paint_rows;
use crate::render::{Theme, push_span};

/// The body indents past the column the page's cursor bar lives in, so a
/// read review lines up with the page that wrote it.
const BODY_LEFT: usize = 2;
/// Quoted rows and comment text under a heading, in the missing-diff body.
const QUOTE_LEFT: usize = 4;

/// The reader's body for one sent review.
///
/// With the patch in hand this is the review page's own body: every file,
/// every row, each comment under the row it was written on. Without it,
/// the comments carry themselves.
pub fn review_reader_rows(
    header: &ReviewHeader,
    comments: &[ReviewComment],
    diff: Option<&str>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    match diff.and_then(|patch| over_diff(header, comments, patch, width, theme)) {
        Some(lines) => lines,
        None => without_diff(comments, diff.is_some(), width, theme),
    }
}

/// Rows and threads over the fetched patch, or `None` when the patch does
/// not parse against the identity the review states.
fn over_diff(
    header: &ReviewHeader,
    comments: &[ReviewComment],
    patch: &str,
    width: usize,
    theme: Theme,
) -> Option<Vec<Line<'static>>> {
    let identity = BaseIdentity {
        base: parse_base(&header.base),
        head: header.head.clone(),
        merge_base: header.merge_base.clone(),
        blobs: header.blobs.clone(),
    };
    let document = parse_stored_patch(patch, identity).ok()?;
    let core = Review::with_comments(document, header.diff.clone(), comments.to_vec());
    let rows = core.comment_rows();

    let mut lines = Vec::new();
    for (index, file) in core.document().files.iter().enumerate() {
        lines.push(file_line(
            file,
            false,
            core.comments_in(index),
            width,
            theme,
            false,
        ));
        let groups = paint_rows(&file.rows, theme, width, BODY_LEFT, false).into_row_groups();
        for (row, group) in groups.into_iter().enumerate() {
            lines.extend(group);
            lines.extend(super::comments::thread_lines(
                core.comments(),
                &rows,
                amux_ui::review::RowRef { file: index, row },
                width,
                theme,
            ));
        }
    }
    Some(lines)
}

/// The comments alone, each under the rows it quotes, with one line
/// saying why the diff is not on screen.
fn without_diff(
    comments: &[ReviewComment],
    fetched: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut notice = Line::default();
    push_span(
        &mut notice,
        BODY_LEFT,
        if fetched {
            "This review's diff no longer matches what it was taken against; the comments are below."
        } else {
            "This review's diff is not available on this host; the comments are below."
        }
        .to_string(),
        theme.muted(),
    );
    lines.push(notice);
    lines.push(Line::default());

    for comment in comments {
        let mut heading = Line::default();
        push_span(&mut heading, BODY_LEFT, heading_text(comment), theme.text());
        lines.push(heading);
        for quoted in &comment.quoted {
            let mut line = Line::default();
            push_span(&mut line, QUOTE_LEFT, quoted.clone(), theme.muted());
            lines.push(line);
        }
        for chunk in
            super::comments::wrap(&comment.text, width.saturating_sub(QUOTE_LEFT + 2).max(8))
        {
            let mut line = Line::default();
            push_span(
                &mut line,
                QUOTE_LEFT,
                format!("\u{270e} {chunk}"),
                theme.text(),
            );
            lines.push(line);
        }
        lines.push(Line::default());
    }
    lines
}

/// Where a comment was written, the way the element itself spells it.
fn heading_text(comment: &ReviewComment) -> String {
    let side = |side: Side| match side {
        Side::Old => "old",
        Side::New => "new",
    };
    format!(
        "{} @@ {}:{}..{}:{}",
        comment.path,
        side(comment.start_side),
        comment.start_line,
        side(comment.side),
        comment.line,
    )
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{sample_patch, sample_review_with_comments};
    use super::*;

    fn sent() -> (ReviewHeader, Vec<ReviewComment>) {
        let view = sample_review_with_comments();
        (view.review().header(), view.review().comments().to_vec())
    }

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A patch that does not parse against what the review says it was
    /// taken against is no better than a missing one: showing rows from
    /// the wrong tree would put every comment on the wrong line.
    #[test]
    fn review_reader_falls_back_when_the_fetched_patch_does_not_parse() {
        let (header, comments) = sent();
        let body = text(&review_reader_rows(
            &header,
            &comments,
            Some("this is not a patch"),
            120,
            Theme::default(),
        ));
        assert!(
            body.contains("no longer matches"),
            "the notice says the patch did not fit: {body:?}"
        );
        assert!(body.contains("Say why the store had to go."));
    }

    /// The comments hold their own quoted rows, so a host that cannot get
    /// the patch still shows what each one was written on.
    #[test]
    fn review_reader_without_a_diff_quotes_the_rows_each_comment_names() {
        let (header, comments) = sent();
        let body = text(&review_reader_rows(
            &header,
            &comments,
            None,
            120,
            Theme::default(),
        ));
        assert!(body.contains("not available on this host"));
        assert!(body.contains("src/lib.rs @@ new:2..new:2"));
        assert!(body.contains("+pub mod attachments;"));
    }

    /// With the patch in hand the review reads as it was written: every
    /// file, and each comment under its own row.
    #[test]
    fn review_reader_with_the_diff_paints_every_file_and_hangs_the_threads() {
        let (header, comments) = sent();
        let body = text(&review_reader_rows(
            &header,
            &comments,
            Some(sample_patch()),
            120,
            Theme::default(),
        ));
        for path in ["src/lib.rs", "notes/old-plan.md", "src/attachments.rs"] {
            assert!(body.contains(path), "{path} is on screen: {body:?}");
        }
        let rows: Vec<&str> = body.lines().collect();
        let anchored = rows
            .iter()
            .position(|row| row.contains("+pub mod attachments;"))
            .expect("the commented row is painted");
        assert!(
            rows[anchored + 1].contains("Say why the store had to go."),
            "the thread hangs under its row: {:?}",
            rows[anchored + 1]
        );
    }
}
