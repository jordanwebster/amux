//! Chapter 1 — Connection: epochs, snapshots, and auth expiry.
//!
//! Reconnect replaces state by snapshot under a new epoch, with an explicit
//! synchronized marker separating catch-up from live. Cloud-auth expiry is a
//! degraded state, never a dead app.

use amux_ui::{Connection, DisconnectReason, Msg};

use crate::harness::*;

fn degraded_auth_sequence() -> Vec<Msg> {
    seq([
        vec![connected("nova"), host_up(&a_host("nova"))],
        synced(),
        vec![
            agent_up(&an_agent("fix-auth-bug", "nova")),
            command(op(1), rename_cmd("fix-auth-bug", "auth-fix")),
            op_failed_auth(op(1)),
        ],
    ])
}

fn auth_disconnect_sequence() -> Vec<Msg> {
    seq([
        degraded_auth_sequence(),
        vec![disconnected(DisconnectReason::AuthenticationRequired)],
    ])
}

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

/// An RPC failing with invalid credentials degrades the fleet to the
/// cloud-auth banner while the daemon link stays connected and local agents
/// keep working; a connection-level credential failure names itself.
#[test]
fn auth_expiry_surfaces_authentication_required() {
    let model = fold(degraded_auth_sequence());
    assert!(model.cloud_auth_required());
    assert!(model.is_connected(), "auth expiry must not kill the app");
    assert!(model.agent(agent_id("fix-auth-bug")).is_some());

    let model = fold(auth_disconnect_sequence());
    assert_eq!(
        model.connection(),
        &Connection::Disconnected {
            reason: DisconnectReason::AuthenticationRequired
        }
    );
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

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("connection::degraded_auth", degraded_auth_sequence()),
        ("connection::auth_disconnect", auth_disconnect_sequence()),
        ("connection::reconnect", reconnect_sequence()),
    ]
}
