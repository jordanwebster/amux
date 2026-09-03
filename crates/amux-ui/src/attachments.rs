//! Canonical attachment mentions embedded in chat text.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

pub use amux::{ArtifactId, ArtifactKind, ArtifactRef};
use amux_artifacts::id_of;
use serde::{Deserialize, Serialize};

use crate::review::{ReviewComment, ReviewHeader};

const OPEN: &str = "<amux-attachment";
const CLOSE: &str = "</amux-attachment>";

/// One parsed attachment element in prompt or reply text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mention {
    pub kind: MentionKind,
    pub name: String,
    pub size: Option<u64>,
    pub path: Option<String>,
}

/// The closed set of message-level attachment kinds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MentionKind {
    Image {
        id: ArtifactId,
    },
    File {
        id: ArtifactId,
    },
    Text {
        body: String,
        lines: u32,
    },
    Review {
        header: ReviewHeader,
        comments: Vec<ReviewComment>,
    },
}

/// A run of ordinary text or one well-formed attachment mention.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "segment", content = "value", rename_all = "snake_case")]
pub enum Segment {
    Prose(String),
    Mention(Mention),
}

/// Artifact data retained by a composer until its message is sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftAttachment {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub bytes: Option<Arc<[u8]>>,
}

impl DraftAttachment {
    /// Builds an artifact-backed draft and computes its content identity locally.
    pub fn from_bytes(
        kind: ArtifactKind,
        name: impl Into<String>,
        mime: impl Into<String>,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Self {
        let bytes = bytes.into();
        Self {
            id: id_of(&bytes),
            kind,
            name: name.into(),
            mime: mime.into(),
            size: bytes.len() as u64,
            bytes: Some(bytes),
        }
    }
}

/// Splits text into prose and valid attachment elements.
///
/// Invalid candidates remain byte-for-byte in prose. Scanning resumes after
/// the invalid opening marker so a later valid mention can still be found.
pub fn split_mentions(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut prose_start = 0;
    let mut search_start = 0;

    while let Some(relative) = text[search_start..].find(OPEN) {
        let start = search_start + relative;
        if let Some((mention, end)) = parse_element(text, start) {
            push_prose(&mut segments, &text[prose_start..start]);
            segments.push(Segment::Mention(mention));
            prose_start = end;
            search_start = end;
        } else {
            search_start = start + OPEN.len();
        }
    }
    push_prose(&mut segments, &text[prose_start..]);
    segments
}

/// Formats one mention into its canonical element representation.
pub fn format_mention(mention: &Mention) -> String {
    match &mention.kind {
        MentionKind::Image { id } => format_artifact("image", id, mention),
        MentionKind::File { id } => format_artifact("file", id, mention),
        MentionKind::Text { body, lines } => format!(
            "<amux-attachment kind=\"text\" name=\"{}\" lines=\"{lines}\">{}</amux-attachment>",
            escape(&mention.name, true),
            escape(body, false),
        ),
        MentionKind::Review { header, comments } => {
            let mut opening = format!(
                "<amux-attachment kind=\"review\" diff=\"{}\" base=\"{}\" head=\"{}\"",
                header.diff,
                escape(&header.base, true),
                escape(&header.head, true),
            );
            if let Some(merge_base) = &header.merge_base {
                opening.push_str(" merge-base=\"");
                opening.push_str(&escape(merge_base, true));
                opening.push('"');
            }
            if !mention.name.is_empty() {
                opening.push_str(" name=\"");
                opening.push_str(&escape(&mention.name, true));
                opening.push('"');
            }
            opening.push_str(&format!(" comments=\"{}\">", comments.len()));
            opening.push_str(&escape(
                &crate::review::format_body(header, comments),
                false,
            ));
            opening.push_str(CLOSE);
            opening
        }
    }
}

fn format_artifact(kind: &str, id: &ArtifactId, mention: &Mention) -> String {
    let mut element = format!(
        "<amux-attachment id=\"{id}\" kind=\"{kind}\" name=\"{}\"",
        escape(&mention.name, true)
    );
    if let Some(size) = mention.size {
        element.push_str(&format!(" size=\"{size}\""));
    }
    if let Some(path) = &mention.path {
        element.push_str(" path=\"");
        element.push_str(&escape(path, true));
        element.push('"');
    }
    element.push_str("/>");
    element
}

fn parse_element(text: &str, start: usize) -> Option<(Mention, usize)> {
    let opening_end = tag_end(text, start)?;
    let opening = &text[start..opening_end];
    let (self_closing, mut attributes) = parse_opening(opening)?;
    let kind = attributes.remove("kind")?;

    let (body, end) = if self_closing {
        (None, opening_end)
    } else {
        let close_start = text[opening_end..].find(CLOSE)? + opening_end;
        (
            Some(decode(&text[opening_end..close_start])?),
            close_start + CLOSE.len(),
        )
    };

    let mention = match kind.as_str() {
        "image" | "file" if self_closing => {
            let id = ArtifactId::from_str(&attributes.remove("id")?).ok()?;
            let name = attributes.remove("name")?;
            let size = attributes
                .remove("size")
                .map(|value| value.parse())
                .transpose()
                .ok()?;
            let path = attributes.remove("path");
            let kind = if kind == "image" {
                MentionKind::Image { id }
            } else {
                MentionKind::File { id }
            };
            Mention {
                kind,
                name,
                size,
                path,
            }
        }
        "text" if !self_closing => Mention {
            kind: MentionKind::Text {
                body: body?,
                lines: attributes.remove("lines")?.parse().ok()?,
            },
            name: attributes.remove("name")?,
            size: None,
            path: None,
        },
        "review" if !self_closing => {
            let diff = ArtifactId::from_str(&attributes.remove("diff")?).ok()?;
            let base = attributes.remove("base")?;
            let head = attributes.remove("head")?;
            let merge_base = attributes.remove("merge-base");
            let name = attributes.remove("name").unwrap_or_default();
            let expected_comments: usize = attributes.remove("comments")?.parse().ok()?;
            let body = crate::review::parse_body(&body?)?;
            if body.comments.len() != expected_comments {
                return None;
            }
            Mention {
                kind: MentionKind::Review {
                    header: ReviewHeader {
                        diff,
                        base,
                        head,
                        merge_base,
                        blobs: body.blobs,
                    },
                    comments: body.comments,
                },
                name,
                size: None,
                path: None,
            }
        }
        _ => return None,
    };
    if !attributes.is_empty() {
        return None;
    }
    Some((mention, end))
}

fn tag_end(text: &str, start: usize) -> Option<usize> {
    let mut quoted = false;
    for (relative, character) in text[start..].char_indices() {
        match character {
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(start + relative + character.len_utf8()),
            _ => {}
        }
    }
    None
}

fn parse_opening(opening: &str) -> Option<(bool, BTreeMap<String, String>)> {
    let inner = opening.strip_prefix(OPEN)?.strip_suffix('>')?;
    if inner
        .chars()
        .next()
        .is_some_and(|character| !character.is_ascii_whitespace() && character != '/')
    {
        return None;
    }
    let trimmed = inner.trim_end();
    let (self_closing, mut rest) = match trimmed.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let mut attributes = BTreeMap::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let name_len = rest
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
            })
            .unwrap_or(rest.len());
        if name_len == 0 {
            return None;
        }
        let name = &rest[..name_len];
        rest = rest[name_len..].trim_start();
        rest = rest.strip_prefix('=')?.trim_start();
        rest = rest.strip_prefix('"')?;
        let value_end = rest.find('"')?;
        let value = decode(&rest[..value_end])?;
        rest = &rest[value_end + 1..];
        if attributes.insert(name.to_string(), value).is_some() {
            return None;
        }
    }
    Some((self_closing, attributes))
}

fn escape(value: &str, attribute: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            '\'' if attribute => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn decode(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(offset) = rest.find('&') {
        decoded.push_str(&rest[..offset]);
        rest = &rest[offset..];
        let (character, consumed) = if rest.starts_with("&amp;") {
            ('&', 5)
        } else if rest.starts_with("&lt;") {
            ('<', 4)
        } else if rest.starts_with("&gt;") {
            ('>', 4)
        } else if rest.starts_with("&quot;") {
            ('"', 6)
        } else if rest.starts_with("&apos;") {
            ('\'', 6)
        } else {
            return None;
        };
        decoded.push(character);
        rest = &rest[consumed..];
    }
    decoded.push_str(rest);
    Some(decoded)
}

fn push_prose(segments: &mut Vec<Segment>, prose: &str) {
    if prose.is_empty() {
        return;
    }
    if let Some(Segment::Prose(previous)) = segments.last_mut() {
        previous.push_str(prose);
    } else {
        segments.push(Segment::Prose(prose.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::Side;

    fn round_trip(mention: Mention) {
        let formatted = format_mention(&mention);
        assert_eq!(split_mentions(&formatted), vec![Segment::Mention(mention)]);
    }

    #[test]
    fn attachments_round_trip_every_kind() {
        round_trip(Mention {
            kind: MentionKind::Image {
                id: id_of(b"image"),
            },
            name: "screen & \"notes\".png".into(),
            size: Some(120_433),
            path: None,
        });
        round_trip(Mention {
            kind: MentionKind::File { id: id_of(b"file") },
            name: "facts.txt".into(),
            size: None,
            path: None,
        });
        round_trip(Mention {
            kind: MentionKind::Text {
                body: "one < two & three\nsecond line".into(),
                lines: 2,
            },
            name: "pasted-1".into(),
            size: None,
            path: None,
        });
        round_trip(Mention {
            kind: MentionKind::Review {
                header: ReviewHeader {
                    diff: id_of(b"patch"),
                    base: "branch: topic & follow-up".into(),
                    head: "4f2a9c1".into(),
                    merge_base: Some("91da2ef".into()),
                    blobs: vec![("src/a file.rs".into(), "abc123".into())],
                },
                comments: vec![ReviewComment {
                    path: "src/a file.rs".into(),
                    start_side: Side::Old,
                    start_line: 7,
                    side: Side::New,
                    line: 8,
                    quoted: vec!["-old < value".into(), "+new & value".into()],
                    text: "Keep this & explain why.\nSecond line.".into(),
                }],
            },
            name: String::new(),
            size: None,
            path: None,
        });
    }

    #[test]
    fn attachments_round_trip_materialised_path() {
        round_trip(Mention {
            kind: MentionKind::Image {
                id: id_of(b"image"),
            },
            name: "shot.png".into(),
            size: Some(5),
            path: Some("/tmp/a & b/\"shot\".png".into()),
        });
    }

    #[test]
    fn attachments_malformed_elements_stay_prose_and_do_not_hide_later_mentions() {
        let valid = format_mention(&Mention {
            kind: MentionKind::File {
                id: id_of(b"valid"),
            },
            name: "valid.txt".into(),
            size: Some(5),
            path: None,
        });
        let malformed = "<amux-attachment kind=\"image\" name=\"missing-id\"/>";
        assert_eq!(
            split_mentions(&format!("before {malformed} then {valid} after")),
            vec![
                Segment::Prose(format!("before {malformed} then ")),
                Segment::Mention(Mention {
                    kind: MentionKind::File {
                        id: id_of(b"valid")
                    },
                    name: "valid.txt".into(),
                    size: Some(5),
                    path: None,
                }),
                Segment::Prose(" after".into()),
            ]
        );
    }

    #[test]
    fn attachments_draft_id_is_computed_from_bytes() {
        let bytes = b"draft bytes".to_vec();
        let draft = DraftAttachment::from_bytes(
            ArtifactKind::File,
            "draft.txt",
            "text/plain",
            bytes.clone(),
        );
        assert_eq!(draft.id, id_of(&bytes));
        assert_eq!(draft.size, bytes.len() as u64);
        assert_eq!(draft.bytes.as_deref(), Some(bytes.as_slice()));
    }

    #[test]
    fn attachments_reexports_artifact_ref() {
        let artifact = ArtifactRef {
            id: id_of(b"ref"),
            kind: ArtifactKind::File,
            name: "ref.txt".into(),
            mime: "text/plain".into(),
            size: 3,
        };
        assert_eq!(artifact.size, 3);
    }
}
