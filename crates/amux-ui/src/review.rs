//! Review data that is independent of any particular renderer.

use amux::ArtifactId;
use serde::{Deserialize, Serialize};

/// The side of a unified diff to which a review endpoint refers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
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

/// Repository identity and artifact reference carried by a review mention.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewHeader {
    pub diff: ArtifactId,
    pub base: String,
    pub head: String,
    pub merge_base: Option<String>,
    pub blobs: Vec<(String, String)>,
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
        body.push_str(&comment.text);
    }
    body
}

pub(crate) struct ParsedBody {
    pub(crate) blobs: Vec<(String, String)>,
    pub(crate) comments: Vec<ReviewComment>,
}

pub(crate) fn parse_body(body: &str) -> Option<ParsedBody> {
    let mut lines = body.split('\n').peekable();
    let blobs = lines.next()?.strip_prefix("blobs: ")?;
    let blobs = serde_json::from_str(blobs).ok()?;
    let mut comments = Vec::new();

    while lines.peek().is_some() {
        let heading = lines.next()?;
        let (path, start_side, start_line, side, line) = parse_heading(heading)?;
        let mut quoted = Vec::new();
        while let Some(line) = lines.peek() {
            let Some(row) = line.strip_prefix("> ") else {
                break;
            };
            quoted.push(row.to_string());
            lines.next();
        }
        if quoted.is_empty() {
            return None;
        }

        let mut text = String::new();
        while let Some(line) = lines.peek() {
            if parse_heading(line).is_some() {
                break;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(lines.next()?);
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

    Some(ParsedBody { blobs, comments })
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
