//! Executable examples from the attachment guide.

use std::path::Path;

use amux_ui::attachments::{Mention, MentionKind, Segment, format_mention, split_mentions};
use amux_ui::review::{ReviewComment, ReviewHeader, Side};

#[test]
fn documented_review_is_the_formatter_output_and_name_is_optional() {
    let guide = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ATTACHMENTS.md"),
    )
    .expect("read the attachment guide");
    let documented = guide
        .split_once("```text\n<amux-attachment kind=\"review\"")
        .expect("attachment guide has a canonical review example")
        .1
        .split_once("\n```")
        .expect("canonical review example closes its code fence")
        .0;
    let documented = format!("<amux-attachment kind=\"review\"{documented}");
    let named = Mention {
        kind: MentionKind::Review {
            header: ReviewHeader {
                diff: "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
                    .parse()
                    .expect("documented diff id"),
                base: "working-tree".into(),
                head: "4f2a9c1".into(),
                merge_base: None,
                blobs: vec![("src/lib.rs".into(), "a1b2c3".into())],
            },
            comments: vec![ReviewComment {
                path: "src/lib.rs".into(),
                start_side: Side::Old,
                start_line: 12,
                side: Side::New,
                line: 13,
                quoted: vec!["-old call".into(), "+new call".into()],
                text: "Use helper.".into(),
            }],
        },
        name: "review".into(),
        size: None,
        path: None,
    };
    assert_eq!(documented, format_mention(&named));

    let mut unnamed = named;
    unnamed.name.clear();
    let formatted = format_mention(&unnamed);
    assert!(!formatted.contains(" name="));
    assert_eq!(
        split_mentions(&formatted),
        vec![Segment::Mention(unnamed)],
        "an omitted review name round-trips as the empty optional name"
    );
}
