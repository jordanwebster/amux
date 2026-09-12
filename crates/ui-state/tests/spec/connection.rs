//! Chapter 1 — Connection epochs and snapshots.
//!
//! Reconnect replaces state by snapshot under a new epoch, with an explicit
//! synchronized marker separating catch-up from live.

use ui_state::{DisconnectReason, Effect, Msg, StreamMsg};

use crate::harness::*;

fn reconnect_sequence() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("first-life-a", "nova")),
            agent_up(&an_agent("first-life-b", "nova")),
        ],
        synced(),
        vec![
            disconnected(DisconnectReason::TransportError {
                message: "connection reset".to_string(),
            }),
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("first-life-b", "nova")),
            agent_up(&an_agent("second-life-c", "nova")),
        ],
        synced(),
    ])
}

fn reconnect_stream_prune_sequence() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            // Local claude agent: the subscription policy opened its stream.
            agent_up(&an_agent("first-life-a", "nova")),
        ],
        synced(),
        vec![
            stream("first-life-a", StreamMsg::Opened { truncated: false }),
            stream("first-life-a", StreamMsg::ReplayComplete),
            disconnected(DisconnectReason::TransportError {
                message: "connection reset".to_string(),
            }),
            connected("nova"),
            host_up(&a_host("nova")),
            // The agent is NOT re-upserted under the new epoch.
        ],
        synced(),
    ])
}

/// After a reconnect completes its snapshot, entities that were not
/// re-upserted under the new epoch are gone — no ghosts from the old life.
#[test]
fn reconnect_replaces_inventory_via_snapshot() {
    let model = fold(reconnect_sequence());
    assert_eq!(model.epoch(), 2);
    assert!(model.agent(agent_id("first-life-a")).is_none());
    assert!(model.agent(agent_id("first-life-b")).is_some());
    assert!(model.agent(agent_id("second-life-c")).is_some());
    assert!(model.host(host_id("nova")).is_some());
}

/// Between `Connected` and the synchronized markers the Model is in
/// catch-up: old rows remain visible and renderers can tell "loading" from
/// "empty". The swap to the new snapshot happens at the marker, not before.
#[test]
fn snapshot_epoch_separates_catchup_from_live() {
    let before_sync = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("stale-agent", "nova")),
        ],
        synced(),
        vec![
            disconnected(DisconnectReason::TransportError {
                message: "connection reset".to_string(),
            }),
            connected("nova"),
        ],
    ]);
    let model = fold(before_sync.clone());
    assert!(model.is_connected());
    assert!(!model.is_synchronized(), "catch-up is not live");
    assert!(
        model.agent(agent_id("stale-agent")).is_some(),
        "old rows stay on screen during catch-up"
    );

    let model = fold(seq([before_sync, vec![host_up(&a_host("nova"))], synced()]));
    assert!(model.is_synchronized());
    assert!(
        model.agent(agent_id("stale-agent")).is_none(),
        "entities not re-upserted under the new epoch are gone at the marker"
    );
    assert!(model.host(host_id("nova")).is_some());
}

/// A stream pruned at the epoch barrier still has a shell task behind it:
/// the prune emits `CloseStream` so the task is released, not orphaned to
/// keep sending stale events across reconnects.
#[test]
fn epoch_prune_emits_close_stream_for_dropped_streams() {
    let (model, effects) = fold_with_effects(reconnect_stream_prune_sequence());
    assert!(
        model.agent(agent_id("first-life-a")).is_none(),
        "the un-re-upserted agent is pruned"
    );
    assert!(
        model.stream(agent_id("first-life-a")).is_none(),
        "its stream state is pruned with it"
    );
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::CloseStream { agent } if *agent == agent_id("first-life-a")
        )),
        "the prune must release the shell's stream task: {effects:?}"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("connection::reconnect", reconnect_sequence()),
        (
            "connection::reconnect_stream_prune",
            reconnect_stream_prune_sequence(),
        ),
    ]
}
