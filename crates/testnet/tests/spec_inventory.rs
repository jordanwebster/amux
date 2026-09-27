//! The inventory between hosts: the one host set, reconciliation and
//! generations, across real daemons on loopback links with real agents on
//! the fakes.
//!
//! Every trusted host is in every inventory snapshot whatever its
//! presence, and discovery adds candidates only from this host's scope. A
//! replica whose origin no longer lists the agent is dropped and its chats
//! close; an origin whose generation changed has every replica dropped and
//! rebuilt from a fresh tail, and nothing of another host's is touched.

#![cfg(unix)]

use std::time::Duration;

use provider_fakes::script::Step;
use store::Store as _;
use testnet::observe::{self, Mark, holds_for, inventory_agents, inventory_hosts, marks};
use testnet::{AgentDecl, HostDecl, JournalCut, Net, PATIENCE, Topology, withdraw};
use uuid::Uuid;
use wire::{
    HostEntry, InventoryEvent, Presence, SessionEvent, Trust, inventory_event, session_event,
};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// One turn saying `label`.
fn says(label: &str) -> Vec<Step> {
    vec![text(label), Step::TurnEnd]
}

fn desk_and_laptop() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

fn host_named<'a>(net: &Net, hosts: &'a [HostEntry], name: &str) -> Option<&'a HostEntry> {
    let id = net.host(name).unwrap().host_id;
    hosts.iter().find(|host| host.host_id == id.as_bytes())
}

/// Whether the inventory seen so far lists `name` with this trust and
/// presence.
fn lists_host(
    net: &Net,
    events: &[InventoryEvent],
    name: &str,
    trust: Trust,
    presence: Presence,
) -> bool {
    host_named(net, &inventory_hosts(events), name)
        .is_some_and(|host| host.trust == trust as i32 && host.presence == presence as i32)
}

fn lists_agent(net: &Net, events: &[InventoryEvent], agent: &str) -> bool {
    let id = net.agent(agent).unwrap().id;
    inventory_agents(events)
        .iter()
        .any(|row| row.agent_id == id.as_bytes())
}

/// Whether the stream removed the agent with this id.
fn removed(events: &[InventoryEvent], id: Uuid, reason: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.of, Some(inventory_event::Of::AgentRemoved(gone))
            if gone.agent_id == id.as_bytes() && gone.reason.as_deref() == Some(reason))
    })
}

/// Waits until `host` holds a caught-up replica of `agent`.
async fn wait_replica_current(net: &Net, host: &str, agent: &str) {
    let key = net.agent(agent).unwrap().key();
    let origin = net.agent(agent).unwrap().host.clone();
    observe::eventually(
        &format!("{agent}'s replica at {host} to be current"),
        PATIENCE,
        || async {
            let newest = {
                let runtime = net.runtime(&origin).unwrap();
                let store = runtime.store().await;
                store.agent(&key).unwrap().map(|row| row.next_revision - 1)
            };
            let runtime = net.runtime(host).unwrap();
            let store = runtime.store().await;
            let Some(row) = store.agent(&key).unwrap() else {
                return false;
            };
            Some(row.source_cursor) == newest
                && store.cut(&key, 0).unwrap().marker == Some(store::Marker::CaughtUp)
        },
    )
    .await
    .unwrap();
}

/// A trusted host that cannot be reached stays in the host set, and its
/// agents stay listed: a reconnect snapshot never forgets them. Only its
/// presence says it is away.
#[tokio::test(flavor = "multi_thread")]
async fn a_trusted_host_stays_in_every_snapshot_while_unreachable() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(says("hello"))
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    wait_replica_current(&net, "laptop", "worker").await;
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(
            |events| {
                observe::inventory_caught_up(events)
                    && lists_host(&net, events, "desk", Trust::Trusted, Presence::Online)
                    && lists_host(&net, events, "laptop", Trust::Trusted, Presence::Online)
                    && lists_agent(&net, events, "worker")
            },
            PATIENCE,
        )
        .await
        .unwrap();

    net.sever_link("desk", "laptop").unwrap();
    fleet
        .observe_until(
            |events| lists_host(&net, events, "desk", Trust::Trusted, Presence::Offline),
            PATIENCE,
        )
        .await
        .unwrap();
    let mut reopened = net.observe_inventory("laptop").await.unwrap();
    let opening = reopened
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert!(
        lists_host(&net, opening, "desk", Trust::Trusted, Presence::Offline)
            && lists_agent(&net, opening, "worker"),
        "an unreachable trusted host and its agents are in the snapshot:\n{}",
        reopened.transcript()
    );
    assert!(
        !opening
            .iter()
            .any(|event| matches!(event.of, Some(inventory_event::Of::HostRemoved(_)))),
        "{}",
        reopened.transcript()
    );

    net.restore_link("desk", "laptop").unwrap();
    fleet
        .observe_until(
            |events| {
                let hosts = inventory_hosts(events);
                host_named(&net, &hosts, "desk")
                    .is_some_and(|desk| desk.presence == Presence::Online as i32)
                    && lists_agent(&net, events, "worker")
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// Discovery lists the machines it finds in this host's scope, matched
/// exactly, as candidates; a trusted host found by discovery stays
/// trusted, a host in another scope never appears, and a candidate that
/// goes away leaves the set.
#[tokio::test(flavor = "multi_thread")]
async fn discovery_candidates_appear_only_from_the_same_scope() {
    let found = |name: &str| HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        ..HostDecl::default()
    };
    let topology = Topology::new()
        .scope("run-a")
        .host_decl(found("phone"))
        .host_decl(found("desk"))
        .host_decl(found("near"))
        .host_decl(HostDecl {
            scope: Some("run-a-other".to_owned()),
            ..found("stranger")
        })
        .host_decl(HostDecl {
            scope: Some(String::new()),
            ..found("everyday")
        })
        .link("phone", "desk");
    let mut net = Net::start(topology).await.unwrap();
    let mut fleet = net.observe_inventory("phone").await.unwrap();
    fleet
        .observe_until(
            |events| {
                lists_host(&net, events, "near", Trust::Candidate, Presence::Online)
                    && lists_host(&net, events, "desk", Trust::Trusted, Presence::Online)
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let outsiders = [
        net.host("stranger").unwrap().host_id,
        net.host("everyday").unwrap().host_id,
    ];
    holds_for("no host from another scope", Duration::from_secs(1), || {
        let hosts = inventory_hosts(fleet.events());
        let seen = outsiders
            .iter()
            .any(|id| hosts.iter().any(|host| host.host_id == id.as_bytes()));
        async move { !seen }
    })
    .await
    .unwrap();
    let desk = host_named(&net, &inventory_hosts(fleet.events()), "desk")
        .unwrap()
        .clone();
    assert_eq!(
        desk.trust,
        Trust::Trusted as i32,
        "found and trusted is trusted"
    );

    let near = net.host("near").unwrap().host_id;
    net.stop_daemon("near").await.unwrap();
    fleet
        .observe_until(
            |events| {
                events.iter().any(|event| {
                    matches!(&event.of, Some(inventory_event::Of::HostRemoved(gone))
                        if gone.host_id == near.as_bytes())
                })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// An agent its host stops listing is dropped from the replica and every
/// chat on it closes, whether the host says so live or the replica learns
/// it at the next inventory catch-up after a break.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_its_origin_no_longer_lists_is_dropped_and_its_chats_close() {
    let topology = desk_and_laptop()
        .agent(AgentDecl::new("live", "desk").steps(says("a")).prompt("go"))
        .agent(
            AgentDecl::new("quiet", "desk")
                .steps(says("b"))
                .prompt("go"),
        )
        .agent(AgentDecl::new("kept", "desk").steps(says("c")).prompt("go"));
    let mut net = Net::start(topology).await.unwrap();
    for agent in ["live", "quiet", "kept"] {
        wait_replica_current(&net, "laptop", agent).await;
    }
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut live_chat = net.observe("laptop", "live", 10).await.unwrap();
    let mut quiet_chat = net.observe("laptop", "quiet", 10).await.unwrap();
    let mut kept_chat = net.observe("laptop", "kept", 10).await.unwrap();
    for chat in [&mut live_chat, &mut quiet_chat, &mut kept_chat] {
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
    }
    let live = net.agent("live").unwrap().clone();
    let quiet = net.agent("quiet").unwrap().clone();

    // Said live: the delta on the host's inventory stream.
    net.delete("live").await.unwrap();
    live_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| removed(events, live.id, node::NO_LONGER_LISTED),
            PATIENCE,
        )
        .await
        .unwrap();

    // Learned after a break: the reconciliation at the inventory's CaughtUp.
    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    net.delete("quiet").await.unwrap();
    holds_for(
        "the replica kept while its host is away",
        Duration::from_millis(300),
        || {
            let runtime = net.runtime("laptop").unwrap();
            let key = quiet.key();
            async move { runtime.store().await.agent(&key).unwrap().is_some() }
        },
    )
    .await
    .unwrap();
    net.restore_link("desk", "laptop").unwrap();
    quiet_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| removed(events, quiet.id, node::NO_LONGER_LISTED),
            PATIENCE,
        )
        .await
        .unwrap();
    let dir = net.host("laptop").unwrap().data_dir.clone();
    let replica_dirs: Vec<_> = walk(&dir)
        .into_iter()
        .filter(|path| path.to_string_lossy().contains(&quiet.id.to_string()))
        .collect();
    assert!(
        replica_dirs.is_empty(),
        "the dropped replica's files go: {replica_dirs:?}"
    );

    // The agent it still lists is untouched: no Reset, still current.
    wait_replica_current(&net, "laptop", "kept").await;
    assert!(
        !marks(kept_chat.events()).contains(&Mark::Reset),
        "{}",
        kept_chat.transcript()
    );
    assert!(!kept_chat.is_closed());
    net.shutdown().await.unwrap();
}

/// Every path under `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// A generation change drops that host's replicas and records the new
/// generation, and touches nothing of another host's: the other host's
/// chat sees no Reset and its rows stay. An agent the rewound host no
/// longer has is dropped and its chat closes.
#[tokio::test(flavor = "multi_thread")]
async fn a_generation_change_drops_one_hosts_replicas_and_nothing_else() {
    let topology = Topology::new()
        .host("desk")
        .host("lab")
        .host("laptop")
        .link("desk", "laptop")
        .link("lab", "laptop")
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(says("w"))
                .prompt("go"),
        )
        .agent(AgentDecl::new("other", "lab").steps(says("o")).prompt("go"));
    let mut net = Net::start(topology).await.unwrap();
    wait_replica_current(&net, "laptop", "worker").await;
    wait_replica_current(&net, "laptop", "other").await;
    net.checkpoint_host("desk").await.unwrap();
    net.spawn(AgentDecl::new("late", "desk").steps(says("l")).prompt("go"))
        .await
        .unwrap();
    wait_replica_current(&net, "laptop", "late").await;

    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut worker_chat = net.observe("laptop", "worker", 10).await.unwrap();
    let mut other_chat = net.observe("laptop", "other", 10).await.unwrap();
    let mut late_chat = net.observe("laptop", "late", 10).await.unwrap();
    for chat in [&mut worker_chat, &mut other_chat, &mut late_chat] {
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
    }
    let other_rows = {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        store
            .cut(&net.agent("other").unwrap().key(), u32::MAX)
            .unwrap()
            .held
    };
    let generation = net.generation("desk").unwrap();
    let late = net.agent("late").unwrap().clone();

    net.rewind_host("desk", &[]).await.unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);
    worker_chat
        .observe_until(
            |events| {
                let after = marks(events);
                after
                    .iter()
                    .position(|mark| *mark == Mark::Reset)
                    .is_some_and(|reset| {
                        after[reset..]
                            .iter()
                            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    late_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| {
                removed(events, late.id, node::NO_LONGER_LISTED)
                    && host_named(&net, &inventory_hosts(events), "desk")
                        .is_some_and(|desk| desk.generation == generation + 1)
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let desk = net.host("desk").unwrap().host_id;
    assert_eq!(
        net.runtime("laptop")
            .unwrap()
            .store()
            .await
            .host_generation(desk.as_bytes())
            .unwrap(),
        Some(generation + 1)
    );

    let runtime = net.runtime("laptop").unwrap();
    let store = runtime.store().await;
    assert_eq!(
        store
            .cut(&net.agent("other").unwrap().key(), u32::MAX)
            .unwrap()
            .held,
        other_rows,
        "another host's replica is untouched"
    );
    drop(store);
    drop(runtime);
    let other = marks(other_chat.events());
    assert!(
        !other
            .iter()
            .any(|mark| matches!(mark, Mark::Reset | Mark::Detached)),
        "{}",
        other_chat.transcript()
    );
    assert!(!other_chat.is_closed());
    net.shutdown().await.unwrap();
}

/// Untrusting a host removes it from the host set and drops its replicas;
/// their chats close.
#[tokio::test(flavor = "multi_thread")]
async fn an_untrusted_host_leaves_the_set_with_its_replicas() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(says("hello"))
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    wait_replica_current(&net, "laptop", "worker").await;
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    let desk = net.host("desk").unwrap().host_id;
    let worker = net.agent("worker").unwrap().clone();

    net.untrust("laptop", "desk").await.unwrap();
    chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| {
                removed(events, worker.id, node::NOT_TRUSTED)
                    && events.iter().any(|event| {
                        matches!(&event.of, Some(inventory_event::Of::HostRemoved(gone))
                            if gone.host_id == desk.as_bytes())
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    assert!(
        net.runtime("laptop")
            .unwrap()
            .store()
            .await
            .agent(&worker.key())
            .unwrap()
            .is_none()
    );
    net.shutdown().await.unwrap();
}

/// This host's own entry says whether its profile is signed in to its
/// account: unset while it was never bound, and flipping with signing out
/// and back in without any link changing, so a client can name the cause
/// when a host only the relay reaches goes away.
#[tokio::test(flavor = "multi_thread")]
async fn the_own_entry_carries_this_profiles_sign_in() {
    let net = Net::start(Topology::new().relay(&["ada"]).host("laptop"))
        .await
        .unwrap();
    let own = |events: &[InventoryEvent]| {
        host_named(&net, &inventory_hosts(events), "laptop").map(|entry| entry.signed_in)
    };
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    let opening = fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert_eq!(own(opening), Some(None), "{}", fleet.transcript());

    net.sign_in("laptop", "ada").await.unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(true)), PATIENCE)
        .await
        .unwrap();

    let profile = net.host("laptop").unwrap().profile;
    net.front_door("laptop")
        .await
        .unwrap()
        .logout_profile(wire::ProfileOperation {
            profile_id: profile.to_string(),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(false)), PATIENCE)
        .await
        .unwrap();
    let mut reopened = net.observe_inventory("laptop").await.unwrap();
    let opening = reopened
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert_eq!(own(opening), Some(Some(false)), "{}", reopened.transcript());

    net.sign_in("laptop", "ada").await.unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(true)), PATIENCE)
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

fn queued(event: &SessionEvent, id: &[u8]) -> bool {
    matches!(&event.of, Some(session_event::Of::Snapshot(snapshot))
        if snapshot.queue.iter().any(|entry| entry.input_id == id))
}

/// The origin-rewind journey. An origin checkpoints while a prompt waits
/// in its queue, the prompt is withdrawn and a paired laptop's chat sees
/// that, and then the origin loses power: its store goes back to the
/// checkpoint, where the prompt is still queued, and it reboots under a
/// new generation. The laptop's chat sees Detached, then Reset, the fresh
/// tail and CaughtUp, and no Snapshot after the Reset ever shows the
/// withdrawn prompt as queued.
#[tokio::test(flavor = "multi_thread")]
async fn origin_rewind() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![
                text("ready"),
                Step::TurnEnd,
                text("working on it"),
                Step::WaitFor {
                    path: "hold".into(),
                },
                Step::TurnEnd,
            ])
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    let kind = net.agent("worker").unwrap().kind;
    wait_replica_current(&net, "laptop", "worker").await;
    let mut chat = net.observe("laptop", "worker", 20).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    net.send("worker", "next").await.unwrap();
    chat.observe_until(
        |events| {
            events.iter().any(|event| {
                matches!(&event.of, Some(session_event::Of::Item(item))
                    if item.text.contains("working on it"))
            })
        },
        PATIENCE,
    )
    .await
    .unwrap();
    let later = Uuid::new_v4();
    let accepted = net
        .input("worker", testnet::prompt(kind, later.as_bytes(), "later"))
        .await
        .unwrap();
    assert!(
        matches!(
            accepted.of,
            Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                queued: true
            }))
        ),
        "{accepted:?}"
    );
    chat.observe_until(
        |events| events.iter().any(|event| queued(event, later.as_bytes())),
        PATIENCE,
    )
    .await
    .unwrap();
    // What reaches the drive: the store with the prompt still queued.
    net.checkpoint_host("desk").await.unwrap();

    net.input(
        "worker",
        withdraw(kind, Uuid::new_v4().as_bytes(), later.as_bytes()),
    )
    .await
    .unwrap();
    chat.observe_until(
        |events| {
            events
                .last()
                .is_some_and(|event| matches!(event.of, Some(session_event::Of::Snapshot(_))))
                && !queued(events.last().unwrap(), later.as_bytes())
        },
        PATIENCE,
    )
    .await
    .unwrap();
    let before = chat.events().len();
    let generation = net.generation("desk").unwrap();

    let journal = net.journal_end("worker").unwrap();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: journal,
        }],
    )
    .await
    .unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);
    let events = chat
        .observe_until(
            |events| {
                let after = marks(&events[before..]);
                after
                    .iter()
                    .position(|mark| *mark == Mark::Reset)
                    .is_some_and(|reset| {
                        after[reset..]
                            .iter()
                            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let after = &events[before..];
    let reset = after
        .iter()
        .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
        .unwrap();
    let rebuilt = &after[reset..];
    let shown_queued = rebuilt
        .iter()
        .filter(|event| queued(event, later.as_bytes()))
        .count();
    let saw_detached = after[..reset]
        .iter()
        .any(|event| matches!(event.of, Some(session_event::Of::Detached(_))));

    let mut transcript = String::new();
    let line = |out: &mut String, text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(
        &mut transcript,
        format!(
            "origin-rewind: desk generation {generation} -> {}; laptop chat on worker, tail 20",
            generation + 1
        ),
    );
    line(&mut transcript, "before the power cut:".to_owned());
    for event in &events[..before] {
        line(&mut transcript, format!("  {}", describe(event)));
    }
    line(&mut transcript, "after the power cut:".to_owned());
    for event in after {
        line(&mut transcript, format!("  {}", describe(event)));
    }
    line(
        &mut transcript,
        format!(
            "check: Detached before the Reset: {saw_detached}; snapshots after the Reset showing the withdrawn prompt queued: {shown_queued}"
        ),
    );
    println!("{transcript}");

    assert!(saw_detached, "{transcript}");
    assert_eq!(shown_queued, 0, "{transcript}");
    assert!(
        rebuilt
            .iter()
            .any(|event| matches!(event.of, Some(session_event::Of::Snapshot(_)))),
        "the rebuilt block leads with a Snapshot: {transcript}"
    );
    assert!(
        rebuilt.iter().any(|event| matches!(&event.of,
            Some(session_event::Of::Item(item)) if item.text.contains("working on it"))),
        "the fresh tail carries the rows the journal kept: {transcript}"
    );
    net.shutdown().await.unwrap();
}

fn describe(event: &SessionEvent) -> String {
    use testnet::observe::Describe as _;
    event.describe()
}
