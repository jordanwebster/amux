//! Neutral unified-patch parsing and row-coordinate facts.

use amux_ui::diff::{Document, Hunk, Numbering, RowFact, RowKind, parse_unified_patch};

fn row(old: Option<u32>, new: Option<u32>, kind: RowKind, text: &str) -> RowFact {
    RowFact {
        old,
        new,
        kind,
        text: text.to_string(),
    }
}

#[test]
fn structured_hunks_classify_and_advance_independent_coordinates() {
    let document = Document {
        numbering: Numbering::Absolute,
        hunks: vec![
            Hunk {
                old_start: 10,
                new_start: 20,
                header: None,
                lines: vec![
                    " shared\ttext".into(),
                    "-old".into(),
                    "+new".into(),
                    " tail".into(),
                ],
            },
            Hunk {
                old_start: 40,
                new_start: 70,
                header: None,
                lines: vec!["-gone".into(), "+arrived".into()],
            },
        ],
        truncated: false,
    };

    assert_eq!(
        document.rows(),
        vec![
            row(None, None, RowKind::Meta, "@@ -10,3 +20,3 @@"),
            row(Some(10), Some(20), RowKind::Context, " shared\ttext"),
            row(Some(11), None, RowKind::Removed, "-old"),
            row(None, Some(21), RowKind::Added, "+new"),
            row(Some(12), Some(22), RowKind::Context, " tail"),
            row(None, None, RowKind::Meta, "@@ -40,1 +70,1 @@"),
            row(Some(40), None, RowKind::Removed, "-gone"),
            row(None, Some(70), RowKind::Added, "+arrived"),
        ]
    );
}

#[test]
fn a_numberless_document_states_no_position_anywhere() {
    let document = Document {
        numbering: Numbering::None,
        hunks: vec![
            Hunk {
                old_start: 1,
                new_start: 1,
                header: None,
                lines: vec![" old".into(), "+new".into()],
            },
            Hunk {
                old_start: 9,
                new_start: 12,
                header: None,
                lines: vec!["-gone".into()],
            },
        ],
        truncated: false,
    };
    let rows = document.rows();

    assert!(
        rows.iter()
            .all(|row| row.old.is_none() && row.new.is_none())
    );
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        vec![" old", "+new", "@@", "-gone"],
        "only the real boundary between hunks receives a bare marker"
    );
}

#[test]
fn a_patch_preserves_headers_and_parses_several_hunks() {
    let document = parse_unified_patch(
        "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -3,2 +8,3 @@ section\n same\n-old\n+new\tvalue\n+extra\n@@ -20 +30 @@\n-last\n+next",
        false,
    );

    assert_eq!(
        document.rows(),
        vec![
            row(None, None, RowKind::Meta, "@@ -3,2 +8,3 @@ section"),
            row(Some(3), Some(8), RowKind::Context, " same"),
            row(Some(4), None, RowKind::Removed, "-old"),
            row(None, Some(9), RowKind::Added, "+new\tvalue"),
            row(None, Some(10), RowKind::Added, "+extra"),
            row(None, None, RowKind::Meta, "@@ -20 +30 @@"),
            row(Some(20), None, RowKind::Removed, "-last"),
            row(None, Some(30), RowKind::Added, "+next"),
        ]
    );
}

#[test]
fn complete_hunks_survive_the_next_files_preamble() {
    let document = parse_unified_patch(
        "diff --git a/a b/a\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -4 +7 @@\n-old b\n+new b",
        false,
    );

    assert_eq!(document.hunks.len(), 2);
    assert_eq!(document.hunks[0].old_start, 1);
    assert_eq!(document.hunks[1].new_start, 7);
}

#[test]
fn a_complete_hunk_survives_an_unknown_trailer() {
    let document = parse_unified_patch(
        "@@ -1 +1 @@\n-old\n+new\nprovider diagnostic outside the patch",
        false,
    );

    assert_eq!(
        document.rows(),
        vec![
            row(None, None, RowKind::Meta, "@@ -1 +1 @@"),
            row(Some(1), None, RowKind::Removed, "-old"),
            row(None, Some(1), RowKind::Added, "+new"),
        ]
    );
}

#[test]
fn empty_headerless_and_bodyless_malformed_patches_have_no_rows() {
    for patch in [
        "",
        "diff --git a/a b/a\n--- a/file\n+++ b/file",
        "--- a/file\n+++ b/file\n-old\n+new",
        "@@ -x,2 +1,2 @@\n-old\n+new",
        "@@ -1,2 +1,2 @@",
    ] {
        assert!(
            parse_unified_patch(patch, false).rows().is_empty(),
            "malformed patch produced rows: {patch:?}"
        );
    }
}

#[test]
fn an_incomplete_hunk_retains_its_observed_prefix_and_states_the_cut() {
    let document = parse_unified_patch("@@ -1,2 +1,2 @@\n-old\n+new", false);

    assert!(document.truncated);
    assert_eq!(
        document.rows(),
        vec![
            row(None, None, RowKind::Meta, "@@ -1,2 +1,2 @@"),
            row(Some(1), None, RowKind::Removed, "-old"),
            row(None, Some(1), RowKind::Added, "+new"),
        ]
    );
}

#[test]
fn a_truncated_tail_hunk_parses_as_far_as_the_head_reaches() {
    let patch = "@@ -10,3 +20,4 @@\n kept\n-old\n+new";
    for source_was_truncated in [false, true] {
        let document = parse_unified_patch(patch, source_was_truncated);
        assert!(document.truncated);
        assert_eq!(
            document.rows(),
            vec![
                row(None, None, RowKind::Meta, "@@ -10,3 +20,4 @@"),
                row(Some(10), Some(20), RowKind::Context, " kept"),
                row(Some(11), None, RowKind::Removed, "-old"),
                row(None, Some(21), RowKind::Added, "+new"),
            ]
        );
    }
}
