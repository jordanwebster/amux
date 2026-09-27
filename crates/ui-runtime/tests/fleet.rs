//! The fleet driver against an in-memory runtime.

mod support;

use client::ManualClock;
use support::*;
use ui_runtime::Fleet;
use ui_state::{AgentKey, Connection};
use wire::Kind;

fn named(id: &[u8]) -> wire::Agent {
    wire::Agent {
        agent_id: id.to_vec(),
        ..agent(Kind::Codex)
    }
}

#[tokio::test]
async fn the_fleet_opens_at_caught_up_and_relists_after_a_reconnect() {
    let clock = ManualClock::new(0);
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::open(client, clock.clone()));
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_agent(named(b"b")));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    assert!(fleet.state().caught_up());
    assert_eq!(fleet.state().agents().count(), 2);
    let key = |id: &[u8]| AgentKey {
        host: b"host-a".to_vec(),
        agent: id.to_vec(),
    };
    assert_eq!(fleet.take_changed(), [key(b"a"), key(b"b")]);

    // The daemon restarts: the fleet waits its backoff, re-lists, and drops
    // the agent the new stream no longer names.
    drop(feed);
    clock.armed(250).await;
    assert_eq!(fleet.state().connection(), Connection::Reconnecting);
    assert!(!fleet.state().caught_up());
    calls.none().await;
    clock.advance(250);
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_caught_up());
    until(fleet.changed(), || fleet.state().caught_up().then_some(())).await;
    let ids: Vec<Vec<u8>> = fleet
        .state()
        .agents()
        .map(|agent| agent.agent_id.clone())
        .collect();
    assert_eq!(ids, [b"a".to_vec()]);
    assert!(fleet.take_changed().contains(&key(b"b")));
    assert_eq!(fleet.trace().replay(), *fleet.state());
    let names: Vec<String> = fleet
        .dump_part()
        .files
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(names, ["client/fleet/state.txt", "client/fleet/trace.txt"]);
}
