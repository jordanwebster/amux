//! Chapter 6 — The chat feed: replay, epochs, and retention.
//!
//! `docs/CHAT.md` B9/B10: everything before the `amux.transcript_ready`
//! marker is replay; a fresh session has no transcript file until its
//! first turn, so an empty feed is an empty chat, not a loading one.
//! Retention is bounded and honest — a window that does not start at the
//! beginning of history says so, evicting content never evicts the
//! pairing index, and a re-replay after source-shrink recovery folds
//! idempotently by row uuid.

use amux_ui::claude::{FeedEntryKind, ToolOutcome};
use amux_ui::{Msg, StreamMsg};
use serde_json::json;

use crate::harness::*;

/// A fresh session renders the empty-chat state, not the loading band:
/// transcript files are created lazily on the first turn (Phase 0,
/// observed), so no rows and no ready marker mean "empty", never
/// "loading forever".
#[test]
fn a_fresh_session_is_empty_chat_not_loading() {
    let model = fold(chat_base("fix-auth-bug"));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert_eq!(layer.entry_count(), 0);
    assert!(
        !layer.transcript_ready(),
        "no transcript_ready before the first turn"
    );
    assert!(!layer.history_truncated());
}

/// The `amux.transcript_ready` marker separates replay from live; the pong
/// fixture carries it before its prompt, and the fold records it as the
/// layer's replay/live fact.
#[test]
fn transcript_ready_marks_the_replay_live_boundary() {
    let model = fold(chat_feed("fix-auth-bug", "pong"));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(layer.transcript_ready());
}

/// Replay that begins past the start of the source buffer is the honest
/// truncation fact: the feed's first state is the boundary, stated (B9).
#[test]
fn a_truncated_window_states_the_boundary() {
    let model = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("fix-auth-bug", "nova")),
        ],
        synced(),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
            batch("fix-auth-bug", 5, chat_rows("pong")),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(layer.history_truncated());
    assert_eq!(layer.evicted_entries(), 0, "truncation fact, not eviction");
}

/// Source-shrink recovery re-replays the file from the start; the fold
/// treats the repeated prefix as re-replay, idempotent by row uuid (B10) —
/// folding the same rows twice yields the identical layer.
#[test]
fn re_replay_is_idempotent_by_row_uuid() {
    let once = fold(chat_feed("fix-auth-bug", "tools"));
    let twice = fold(seq([
        chat_feed("fix-auth-bug", "tools"),
        vec![batch("fix-auth-bug", 20, chat_rows("tools"))],
    ]));
    assert_eq!(
        claude_layer(&once, "fix-auth-bug"),
        claude_layer(&twice, "fix-auth-bug"),
        "a repeated prefix is re-replay, not new content"
    );
}

/// A row from a different `sessionId` IS the relink (`/clear`, resume,
/// fork): the buffer was cleared and a new file replays. The layer opens a
/// fresh epoch — feed, session facts, and the ready marker all belong to
/// the new file (§16: the relink is the only reliable `/clear` signal).
#[test]
fn a_different_session_id_opens_a_fresh_epoch() {
    let relinked = fold(seq([
        chat_feed("fix-auth-bug", "pong"),
        vec![batch("fix-auth-bug", 20, chat_rows("tools"))],
    ]));
    // The same tools batch folded alone, at the same stream position, so
    // even the provenance seqs match.
    let direct = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 20, chat_rows("tools"))],
    ]));
    assert_eq!(
        claude_layer(&relinked, "fix-auth-bug"),
        claude_layer(&direct, "fix-auth-bug"),
        "the old epoch's state is gone; only the new file's fold remains"
    );
}

/// A re-subscription replays the source tail from scratch, and the layer
/// folds from scratch with it: rows without uuids (hooks, markers,
/// session-state) carry no dedupe key, so the window reset is what keeps
/// them exactly-once.
#[test]
fn reopening_the_stream_refolds_the_window() {
    let reopened = fold(seq([
        chat_feed("fix-auth-bug", "pong"),
        vec![
            stream("fix-auth-bug", StreamMsg::Opened { truncated: false }),
            batch("fix-auth-bug", 30, chat_rows("pong")),
            stream("fix-auth-bug", StreamMsg::ReplayComplete),
        ],
    ]));
    let direct = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 30, chat_rows("pong"))],
    ]));
    assert_eq!(
        claude_layer(&reopened, "fix-auth-bug"),
        claude_layer(&direct, "fix-auth-bug")
    );
}

/// An authored prompt row with a distinct uuid, timestamp and text.
fn a_prompt_row(n: u64) -> serde_json::Value {
    json!({
        "type": "user",
        "uuid": uuid::Uuid::from_u128(0x1000 + u128::from(n)).to_string(),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": format!("prompt {n}")},
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "promptId": uuid::Uuid::from_u128(0x9000 + u128::from(n)).to_string(),
    })
}

/// The feed is bounded: eviction is from the front, counted, and honest —
/// the layer reports a truncated history once anything was evicted.
#[test]
fn eviction_is_bounded_counted_and_honest() {
    let over = 1005u64;
    let rows: Vec<serde_json::Value> = (0..over).map(a_prompt_row).collect();
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, rows)],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    assert_eq!(layer.entry_count(), 1000);
    assert_eq!(layer.evicted_entries(), 5);
    assert!(layer.history_truncated());
    let first = layer.entries().next().expect("front entry");
    assert!(
        matches!(&first.kind, FeedEntryKind::Prompt(prompt) if prompt.text == "prompt 5"),
        "the oldest retained entry follows the evicted ones: {:?}",
        first.kind
    );
}

/// Evicting content never evicts the pairing index (B9's
/// obligations-outlive-eviction structure): a tool whose entry was evicted
/// still pairs its late result — the obligation resolves without the
/// bytes.
#[test]
fn the_pairing_index_outlives_feed_eviction() {
    let tool_use = json!({
        "type": "assistant",
        "uuid": uuid::Uuid::from_u128(0x2000).to_string(),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T21:59:00.000Z",
        "message": {
            "id": "msg_evicted",
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "toolu_outlives",
                "name": "Bash",
                "input": {"command": "sleep 3600"}
            }]
        }
    });
    let late_result = json!({
        "type": "user",
        "uuid": uuid::Uuid::from_u128(0x2001).to_string(),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T23:00:00.000Z",
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": "toolu_outlives",
            "content": "done"
        }]}
    });
    let flood: Vec<serde_json::Value> = (0..1001).map(a_prompt_row).collect();

    let mut msgs = chat_base("fix-auth-bug");
    msgs.push(batch("fix-auth-bug", 10, vec![tool_use]));
    msgs.push(batch("fix-auth-bug", 20, flood));
    let model = fold(msgs.clone());
    let layer = claude_layer(&model, "fix-auth-bug");
    assert!(
        !layer
            .entries()
            .any(|entry| matches!(entry.kind, FeedEntryKind::Tool(_))),
        "the tool entry itself was evicted"
    );
    assert_eq!(layer.open_tool_count(), 1, "the obligation survived");

    msgs.push(batch("fix-auth-bug", 30, vec![late_result]));
    let model = fold(msgs);
    let layer = claude_layer(&model, "fix-auth-bug");
    assert_eq!(layer.open_tool_count(), 0, "the late result resolved it");
}

/// A result whose tool_use fell outside the window entirely renders as an
/// honest result-only line, never a crash and never a silent drop.
#[test]
fn an_orphan_result_renders_result_only() {
    let orphan = json!({
        "type": "user",
        "uuid": uuid::Uuid::from_u128(0x3000).to_string(),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": "toolu_before_the_window",
            "content": "output from before history began"
        }]}
    });
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch("fix-auth-bug", 10, vec![orphan])],
    ]));
    let layer = claude_layer(&model, "fix-auth-bug");
    let tool = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("an orphan tool line");
    assert_eq!(tool.name, None);
    assert!(matches!(tool.outcome, ToolOutcome::Success { .. }));
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("feed_replay::fresh", chat_base("fix-auth-bug")),
        ("feed_replay::pong", chat_feed("fix-auth-bug", "pong")),
        (
            "feed_replay::re_replay",
            seq([
                chat_feed("fix-auth-bug", "tools"),
                vec![batch("fix-auth-bug", 20, chat_rows("tools"))],
            ]),
        ),
        (
            "feed_replay::relink",
            seq([
                chat_feed("fix-auth-bug", "pong"),
                vec![batch("fix-auth-bug", 20, chat_rows("tools"))],
            ]),
        ),
        (
            "feed_replay::eviction",
            seq([
                chat_base("fix-auth-bug"),
                vec![batch(
                    "fix-auth-bug",
                    10,
                    (0..1005).map(a_prompt_row).collect(),
                )],
            ]),
        ),
    ]
}
