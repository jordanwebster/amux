//! The transcript window: append above the head, upsert inside if newer,
//! drop below the oldest held row, pages extend the low edge only.

use ui_state::{BlobStatus, Msg, SessionState};
use wire::{Kind, Phase};

use crate::harness::*;

fn open_with(kind: Kind, orders: std::ops::RangeInclusive<u64>) -> SessionState {
    let mut state = SessionState::new(agent(kind), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Idle, &[], &[])),
    );
    for order in orders {
        apply_checked(&mut state, ev_item(text(kind, order, 10, "held")));
    }
    apply_checked(&mut state, caught_up(10));
    state
}

#[test]
fn a_row_above_the_head_appends() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        let outcome = apply_checked(&mut state, ev_item(text(kind, 8, 11, "new")));
        assert_eq!(outcome.changed, vec!["k8"]);
        assert_eq!(state.transcript().head(), Some(8));
        assert_eq!(state.oldest_order(), Some(5));
    }
}

#[test]
fn a_row_inside_upserts_by_key_only_if_newer() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        let outcome = apply_checked(&mut state, ev_item(text(kind, 6, 12, "revised")));
        assert_eq!(outcome.changed, vec!["k6"]);
        let outcome = apply_checked(&mut state, ev_item(text(kind, 6, 11, "older")));
        assert!(
            outcome.changed.is_empty(),
            "an older revision never overwrites a newer one"
        );
        let outcome = apply_checked(&mut state, ev_item(text(kind, 6, 12, "same revision")));
        assert!(outcome.changed.is_empty());
        assert_eq!(state.transcript().get("k6").unwrap().item.text, "revised");
    }
}

#[test]
fn a_live_row_below_the_window_is_dropped() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        let outcome = apply_checked(&mut state, ev_item(text(kind, 3, 20, "old row revised")));
        assert!(outcome.changed.is_empty());
        assert_eq!(state.oldest_order(), Some(5));
        assert!(state.transcript().get("k3").is_none());
    }
}

#[test]
fn pages_extend_the_low_edge_and_never_the_top() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        assert!(state.transcript().has_older());
        let page_items = vec![
            text(kind, 4, 3, "p4"),
            text(kind, 3, 3, "p3"),
            text(kind, 9, 3, "future"),
        ];
        let outcome = apply_page(&mut state, page_items, false);
        assert_eq!(outcome.changed, vec!["k4", "k3"]);
        assert_eq!(state.oldest_order(), Some(3));
        assert_eq!(
            state.transcript().head(),
            Some(7),
            "a page never lands above the head"
        );
        assert!(state.transcript().has_older());
        apply_page(&mut state, vec![text(kind, 2, 3, "p2")], true);
        assert!(
            !state.transcript().has_older(),
            "an exhausted page ends older history"
        );
        let mut from_one = open_with(kind, 1..=2);
        assert!(
            !from_one.transcript().has_older(),
            "a window reaching order one holds everything"
        );
        apply_checked(&mut from_one, ev_item(text(kind, 3, 11, "x")));
    }
}

#[test]
fn page_and_live_overlap_dedupe_by_key() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        // A page that overlaps the window brings a newer revision of a held
        // row and an older one of another.
        let items = vec![
            text(kind, 6, 15, "newer from page"),
            text(kind, 5, 2, "older from page"),
            text(kind, 4, 1, "p4"),
        ];
        let outcome = apply_page(&mut state, items, false);
        assert_eq!(outcome.changed, vec!["k6", "k4"]);
        assert_eq!(
            state.transcript().get("k6").unwrap().item.text,
            "newer from page"
        );
        assert_eq!(state.transcript().get("k5").unwrap().item.text, "held");
    }
}

#[test]
fn the_store_read_and_broadcast_overlap_is_deduped_by_key() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
        );
        // Held rows read from the store, already at revision 12...
        apply_checked(&mut state, ev_item(streaming(kind, 1, 12, "Hello world")));
        // ...then the broadcast buffer replays what the read already covered.
        for msg in [
            ev_item(streaming(kind, 1, 10, "Hello")),
            ev_append("k1", 10, 11, " wor"),
            ev_append("k1", 11, 12, "ld"),
        ] {
            let outcome = apply_checked(&mut state, msg);
            assert!(outcome.changed.is_empty());
            assert!(outcome.need_get.is_none());
        }
        assert_eq!(
            state.transcript().get("k1").unwrap().item.text,
            "Hello world"
        );
    }
}

#[test]
fn a_live_append_extends_the_held_text() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=1);
        apply_checked(&mut state, ev_item(streaming(kind, 2, 200, "")));
        for (base, text) in [(200, "Hel"), (201, "lo w"), (202, "orld")] {
            let outcome = apply_checked(&mut state, ev_append("k2", base, base + 1, text));
            assert_eq!(outcome.changed, vec!["k2"]);
        }
        let held = state.transcript().get("k2").unwrap();
        assert_eq!(
            (held.item.text.as_str(), held.item.revision),
            ("Hello world", 203)
        );
        apply_checked(&mut state, ev_item(text(kind, 2, 204, "Hello world")));
        assert_eq!(state.transcript().get("k2").unwrap().item.revision, 204);
    }
}

#[test]
fn a_wrong_base_becomes_a_get() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=2);
        let outcome = apply_checked(&mut state, ev_append("k2", 9, 12, "x"));
        assert_eq!(outcome.need_get.as_deref(), Some("k2"));
        assert!(outcome.changed.is_empty());
        let outcome = apply_checked(&mut state, ev_append("k9", 3, 4, "x"));
        assert_eq!(
            outcome.need_get.as_deref(),
            Some("k9"),
            "an append for a row not held is a Get"
        );
        // The driver's Get answer lands as a full item.
        let outcome = apply_checked(&mut state, ev_item(text(kind, 2, 12, "held.x")));
        assert_eq!(outcome.changed, vec!["k2"]);
    }
}

#[test]
fn an_attachment_is_a_placeholder_until_its_bytes_arrive() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=1);
        let blob = wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 2048,
        };
        let mut with_image = prompt(kind, 2, 11, "see \u{FFFC}", b"p1");
        with_image.attachments = vec![wire::Attachment {
            of: Some(wire::attachment::Of::Image(blob.clone())),
        }];
        apply_checked(&mut state, ev_item(with_image));
        assert_eq!(
            state.blob(&blob.hash),
            BlobStatus::Missing,
            "the row draws a placeholder from name, type and size"
        );
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Fetching,
        });
        assert_eq!(outcome.changed, vec!["k2"]);
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Ready,
        });
        assert_eq!(
            outcome.changed,
            vec!["k2"],
            "the row updates when the bytes arrive"
        );
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Ready,
        });
        assert!(outcome.changed.is_empty());
        let outcome = state.update(Msg::Blob {
            hash: vec![9; 32],
            status: BlobStatus::Ready,
        });
        assert!(
            outcome.changed.is_empty(),
            "only rows that reference the blob change"
        );
    }
}

#[test]
fn a_task_item_redraws_the_call_it_names() {
    use prost::Message;
    let kind = Kind::ClaudeSdk;
    let task = |order: u64, revision: u64, state: wire::TaskState| {
        let mut item = text(kind, order, revision, "");
        item.body = wire::ClaudeSdkItem {
            kind: Some(wire::claude_sdk_item::Kind::Task(wire::Task {
                task_id: "t".into(),
                description: "Count".into(),
                state: state as i32,
                tool_key: "k2".into(),
                ..wire::Task::default()
            })),
        }
        .encode_to_vec();
        item
    };
    let mut state = open_with(kind, 1..=1);
    apply_checked(
        &mut state,
        ev_item(command(kind, 2, 11, wire::ToolState::Running)),
    );
    let outcome = apply_checked(&mut state, ev_item(task(3, 12, wire::TaskState::Running)));
    assert_eq!(outcome.changed, vec!["k3", "k2"]);
    let outcome = apply_checked(&mut state, ev_item(task(3, 13, wire::TaskState::Completed)));
    assert_eq!(
        outcome.changed,
        vec!["k3", "k2"],
        "progress on the task redraws the call's row"
    );
    assert_eq!(state.transcript().referrer("k2").unwrap().item.key, "k3");
}
