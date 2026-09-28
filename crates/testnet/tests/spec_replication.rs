//! Peer sources and the replica block, across real daemons on loopback
//! links with real agents on the fakes.
//!
//! A replica's rows for another host's agent are empty or one contiguous
//! block ending at the origin's newest row. These cases hold that under a
//! first tail, live records, a delta after a break, a Reset beyond the cap,
//! a stream that dies right after its Snapshot while a page lands a newer
//! revision of an old row, a link back before the follower looks, an exit
//! whose records arrive after its Exited row, pages from the origin,
//! trimming after a Reset, the source policy and a rewound origin; and they
//! hold the markers in sequence with the rows they cover.

#![cfg(unix)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use node::{Launch, SourcePolicy, SourceVerdict};
use provider_fakes::script::Step;
use store::{AgentKey, CommitClock, Marker, PageEnd, Store as _};
use testnet::observe::{self, Mark, holds_for, marks};
use testnet::{AgentDecl, JournalCut, Net, NetOptions, PATIENCE, Topology};
use wire::{
    FetchRequest, InventoryEvent, Item, SessionEvent, StopMode, inventory_event, session_event,
};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// `turns` turns of `per_turn` messages each, "t<turn>-<n>".
fn turns(turns: usize, per_turn: usize) -> Vec<Step> {
    let mut steps = Vec::new();
    for turn in 0..turns {
        for n in 0..per_turn {
            steps.push(text(&format!("t{turn}-{n}")));
        }
        steps.push(Step::TurnEnd);
    }
    steps
}

/// One turn of `n` messages, "<label>-<n>".
fn turn(label: &str, n: usize) -> Vec<Step> {
    let mut steps: Vec<Step> = (0..n).map(|at| text(&format!("{label}-{at}"))).collect();
    steps.push(Step::TurnEnd);
    steps
}

/// A net whose hosts keep a tail of `k`, with `more` for any other
/// parameter by host.
fn options(k: u32, more: impl Fn(&str, &mut Launch) + Send + Sync + 'static) -> NetOptions {
    NetOptions {
        launch: Some(Arc::new(move |host: &str, launch: &mut Launch| {
            launch.tail_rows = k;
            more(host, launch);
        })),
        ..NetOptions::default()
    }
}

fn desk_and_laptop() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

/// Every row the origin holds for `agent`, oldest first.
async fn origin_rows(net: &Net, agent: &str) -> Vec<Item> {
    let agent = net.agent(agent).unwrap().clone();
    let runtime = net.runtime(&agent.host).unwrap();
    let store = runtime.store().await;
    let mut rows = store.cut(&agent.key(), u32::MAX).unwrap().held;
    rows.sort_by_key(|item| item.order);
    rows
}

/// The origin's newest revision for `agent`: what a CaughtUp carries.
async fn origin_revision(net: &Net, agent: &str) -> u64 {
    let agent = net.agent(agent).unwrap().clone();
    let runtime = net.runtime(&agent.host).unwrap();
    let row = runtime.store().await.agent(&agent.key()).unwrap().unwrap();
    row.next_revision - 1
}

/// The replica's source cursor and marker at `host`.
async fn replica_state(net: &Net, host: &str, agent: &str) -> Option<(u64, Option<Marker>)> {
    let key = net.agent(agent).unwrap().key();
    let runtime = net.runtime(host).unwrap();
    let store = runtime.store().await;
    let row = store.agent(&key).unwrap()?;
    Some((row.source_cursor, store.cut(&key, 0).unwrap().marker))
}

/// Waits until `host` holds what the origin holds for `agent`: caught up
/// through the origin's newest revision, with the block intact.
async fn wait_current(net: &Net, host: &str, agent: &str) {
    let what = format!("{agent}'s replica at {host} to be current");
    let waited = observe::eventually(&what, PATIENCE, || async {
        let newest = origin_revision(net, agent).await;
        replica_state(net, host, agent).await == Some((newest, Some(Marker::CaughtUp)))
            && net.assert_block_invariant(host, agent).await.is_ok()
    })
    .await;
    if let Err(stuck) = waited {
        let state = replica_state(net, host, agent).await;
        let newest = origin_revision(net, agent).await;
        let block = net.assert_block_invariant(host, agent).await;
        panic!(
            "{stuck}\n  replica (cursor, marker): {state:?}\n  origin newest: {newest}\n  block: {block:?}"
        );
    }
}

/// Waits until the origin's journal for `agent` holds a message saying
/// `wanted`.
async fn wait_origin_says(net: &Net, agent: &str, wanted: &str) {
    observe::eventually(&format!("{agent} to say {wanted}"), PATIENCE, || async {
        origin_rows(net, agent)
            .await
            .iter()
            .any(|item| item.text.contains(wanted))
    })
    .await
    .unwrap();
}

/// The marks after the last `Detached`.
fn since_last_detached(events: &[SessionEvent]) -> Vec<Mark> {
    let all = marks(events);
    let from = all
        .iter()
        .rposition(|mark| *mark == Mark::Detached)
        .map_or(0, |at| at + 1);
    all[from..].to_vec()
}

fn count_caught_up(events: &[SessionEvent]) -> usize {
    marks(events)
        .iter()
        .filter(|mark| matches!(mark, Mark::CaughtUp(_)))
        .count()
}

async fn fetch(
    net: &Net,
    host: &str,
    agent: &str,
    before_order: Option<u64>,
    limit: u32,
) -> Result<wire::FetchResponse, node::ServeError> {
    let id = net.agent(agent).unwrap().id;
    net.runtime(host)
        .unwrap()
        .fetch(&FetchRequest {
            agent_id: id.as_bytes().to_vec(),
            before_order,
            limit,
        })
        .await
}

async fn sever(net: &mut Net) {
    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
}

async fn restore(net: &mut Net) {
    net.restore_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", true).await.unwrap();
}

/// A replica starts from a tail of K, follows live records, takes a delta
/// after a break that fits the cap and a Reset with a fresh tail after one
/// that does not; its markers arrive in sequence with the rows they cover,
/// again after every reconnect, and never while the host is away. A client
/// asking for more than K rows gets K.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_takes_a_tail_live_records_a_delta_and_a_reset_with_markers_in_sequence() {
    const K: u32 = 6;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(8, 2))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "t0-1").await;
    wait_current(&net, "laptop", "worker").await;

    let mut chat = net.observe("laptop", "worker", 50).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    // Live: each record reaches the chat, and the cursor follows it.
    net.send("worker", "two").await.unwrap();
    wait_origin_says(&net, "worker", "t1-1").await;
    chat.observe_until(
        |events| {
            events.iter().any(|event| {
                matches!(&event.of, Some(session_event::Of::Item(item)) if item.text.contains("t1-1"))
            })
        },
        PATIENCE,
    )
    .await
    .unwrap();
    wait_current(&net, "laptop", "worker").await;

    // N is clamped to K, at the replica and at the origin alike.
    for host in ["laptop", "desk"] {
        let mut wide = net.observe(host, "worker", 50).await.unwrap();
        let opening = wide
            .observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
        let held = marks(opening)
            .iter()
            .filter(|mark| matches!(mark, Mark::Item(_)))
            .count();
        assert_eq!(held, K as usize, "{host}: {}", wide.transcript());
        assert!(origin_rows(&net, "worker").await.len() > K as usize);
        assert_eq!(marks(opening)[0], Mark::Snapshot);
    }

    // A break that fits the cap: Detached, then the delta and CaughtUp,
    // with no Reset and nothing marked current while the host is away.
    sever(&mut net).await;
    chat.observe_until(
        |events| marks(events).last() == Some(&Mark::Detached),
        PATIENCE,
    )
    .await
    .unwrap();
    let caught_up_before = count_caught_up(chat.events());
    net.send("worker", "three").await.unwrap();
    wait_origin_says(&net, "worker", "t2-1").await;
    holds_for(
        "no CaughtUp while the host is away",
        Duration::from_millis(300),
        || {
            let seen = count_caught_up(chat.events());
            async move { seen == caught_up_before }
        },
    )
    .await
    .unwrap();
    restore(&mut net).await;
    let events = chat
        .observe_until(
            |events| {
                since_last_detached(events)
                    .iter()
                    .any(|m| matches!(m, Mark::CaughtUp(_)))
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let delta = since_last_detached(events);
    assert!(
        !delta.contains(&Mark::Reset),
        "a delta, not a Reset: {delta:?}"
    );
    assert!(
        delta.iter().any(|mark| matches!(mark, Mark::Item(_))),
        "the missed rows arrive before CaughtUp: {delta:?}"
    );
    assert!(matches!(delta.last(), Some(Mark::CaughtUp(_))), "{delta:?}");
    wait_current(&net, "laptop", "worker").await;

    // A break beyond the cap: Reset, the Snapshot, a fresh tail of K, then
    // CaughtUp.
    sever(&mut net).await;
    chat.observe_until(
        |events| marks(events).last() == Some(&Mark::Detached),
        PATIENCE,
    )
    .await
    .unwrap();
    for turn in 3..7 {
        net.send("worker", &format!("turn {turn}")).await.unwrap();
        wait_origin_says(&net, "worker", &format!("t{turn}-1")).await;
    }
    restore(&mut net).await;
    let events = chat
        .observe_until(
            |events| {
                since_last_detached(events)
                    .iter()
                    .any(|m| matches!(m, Mark::CaughtUp(_)))
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let reset = since_last_detached(events);
    assert_eq!(reset.first(), Some(&Mark::Reset), "{reset:?}");
    assert_eq!(reset.get(1), Some(&Mark::Snapshot), "{reset:?}");
    let tail = reset
        .iter()
        .filter(|mark| matches!(mark, Mark::Item(_)))
        .count();
    assert_eq!(tail, K as usize, "{reset:?}");
    assert!(matches!(reset.last(), Some(Mark::CaughtUp(_))), "{reset:?}");
    wait_current(&net, "laptop", "worker").await;
    println!(
        "the laptop's chat on the desk's worker, K = {K}:\n{}",
        chat.transcript()
    );
    net.shutdown().await.unwrap();
}

/// A source's stream dies right after the origin's Snapshot, whose revision
/// is ahead of the rows behind it, while a page from the origin lands a
/// newer revision of an old row and a newer row is still missing. The
/// cursor did not move, so the reconnect asks after the old cursor and
/// nothing is lost; the source reconnects on the policy clock with
/// Detached between.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_dying_after_its_snapshot_replays_after_the_cursor_while_a_page_lands_newer() {
    const K: u32 = 6;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps([turn("first", 10), turn("second", 1), turn("third", 1)].concat()),
    );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    let worker = net.agent("worker").unwrap().clone();
    let key = worker.key();
    wait_current(&net, "laptop", "worker").await;
    // The first turn happens while the laptop is away, so it comes back to
    // a Reset: its block is the newest K rows and the oldest lie below it.
    sever(&mut net).await;
    net.send("worker", "go").await.unwrap();
    wait_origin_says(&net, "worker", "first-9").await;
    restore(&mut net).await;
    wait_current(&net, "laptop", "worker").await;
    let (cursor, _) = replica_state(&net, "laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 50).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    // The next stream to reach the laptop's source drops right after its
    // Snapshot, once: the Snapshot is taken, the event after it is not.
    let dropped = Arc::new(AtomicBool::new(false));
    {
        let dropped = dropped.clone();
        let snapshot_seen = AtomicBool::new(false);
        let watched = key.clone();
        net.runtime("laptop")
            .unwrap()
            .set_source_hook(Some(Arc::new(
                move |agent: &AgentKey, event: &SessionEvent| {
                    if *agent != watched || dropped.load(Ordering::SeqCst) {
                        SourceVerdict::Keep
                    } else if matches!(event.of, Some(session_event::Of::Snapshot(_))) {
                        snapshot_seen.store(true, Ordering::SeqCst);
                        SourceVerdict::Keep
                    } else if snapshot_seen.load(Ordering::SeqCst) {
                        dropped.store(true, Ordering::SeqCst);
                        SourceVerdict::Drop
                    } else {
                        SourceVerdict::Keep
                    }
                },
            )));
    }

    // While the laptop is cut off the origin writes a new turn, then
    // revises its oldest row: the revision a page will bring is newer than
    // the rows the laptop is still missing.
    sever(&mut net).await;
    net.send("worker", "two").await.unwrap();
    wait_origin_says(&net, "worker", "second-0").await;
    let oldest = origin_rows(&net, "worker").await[0].clone();
    {
        let runtime = net.runtime("desk").unwrap();
        let mut store = runtime.store().await;
        let at = store.cursor(&key).unwrap();
        let revised = Item {
            text: format!("{} (revised)", oldest.text),
            ..oldest.clone()
        };
        store
            .commit(
                &key,
                &[(
                    at,
                    wire::Step {
                        items: vec![revised],
                        ..wire::Step::default()
                    },
                )],
                CommitClock {
                    now_ms: net.now_ms(),
                    notify_delay_ms: 0,
                },
            )
            .unwrap();
    }
    let revised = origin_rows(&net, "worker").await[0].clone();
    assert!(revised.revision > cursor);

    restore(&mut net).await;
    observe::eventually("the stream to drop after its Snapshot", PATIENCE, || {
        let dropped = dropped.load(Ordering::SeqCst);
        async move { dropped }
    })
    .await
    .unwrap();
    // The source is waiting out its backoff on the policy clock; the
    // Snapshot it dropped after did not move its cursor.
    assert_eq!(
        replica_state(&net, "laptop", "worker").await,
        Some((cursor, Some(Marker::Detached)))
    );

    // A page down from the laptop reaches below the block to the origin,
    // which answers with the revised row; it joins the block and is kept.
    let floor = {
        let runtime = net.runtime("laptop").unwrap();
        let row = runtime.store().await.agent(&key).unwrap().unwrap();
        row.complete_from_order.unwrap()
    };
    let page = fetch(&net, "laptop", "worker", Some(floor), 100)
        .await
        .unwrap();
    assert!(page.exhausted, "the origin holds every row");
    assert!(
        page.items
            .iter()
            .any(|item| item.key == revised.key && item.revision == revised.revision),
        "{:#?}",
        page.items
    );
    {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        let held = store.get(&key, &revised.key).unwrap().unwrap();
        assert_eq!(
            held.revision, revised.revision,
            "the page's revision is kept"
        );
        assert_eq!(
            store.cursor(&key).unwrap(),
            cursor,
            "a page never moves the cursor"
        );
        assert!(
            !store
                .cut(&key, u32::MAX)
                .unwrap()
                .held
                .iter()
                .any(|item| item.text.contains("second-0")),
            "the new turn is still missing"
        );
    }

    // Nothing reconnects before the backoff is up; then the source asks
    // after its old cursor and the delta carries the missing rows.
    holds_for(
        "the source waits out its backoff",
        Duration::from_millis(200),
        || {
            let caught_up = count_caught_up(chat.events());
            async move { caught_up == 1 }
        },
    )
    .await
    .unwrap();
    net.advance(Duration::from_secs(1)).unwrap();
    let events = chat
        .observe_until(
            |events| {
                since_last_detached(events)
                    .iter()
                    .any(|m| matches!(m, Mark::CaughtUp(_)))
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let replay = since_last_detached(events);
    assert!(!replay.contains(&Mark::Reset), "{replay:?}");
    wait_current(&net, "laptop", "worker").await;
    println!(
        "the laptop's chat through a stream dropped after its Snapshot:\n{}",
        chat.transcript()
    );
    let laptop_rows = {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        store.cut(&key, u32::MAX).unwrap().held
    };
    assert_eq!(
        laptop_rows.len(),
        origin_rows(&net, "worker").await.len(),
        "the page and the delta together hold the origin's whole history"
    );
    net.shutdown().await.unwrap();
}

/// A source whose stream is lost while the link stays up detaches its
/// chat itself: the host was never lost, yet the chat sees Detached and
/// the replica's marker reads Detached for as long as the source waits out
/// its backoff; then it catches up again.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_lost_with_the_link_up_detaches_through_the_backoff() {
    const K: u32 = 6;
    let mut net = Net::start_with(desk_and_laptop(), options(K, |_, _| {}))
        .await
        .unwrap();

    // Once armed, the laptop's source drops the next record it is sent. A
    // source reads the hook when its stream opens, so it is set before the
    // agent exists.
    let armed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let laptop = net.runtime("laptop").unwrap();
    {
        let armed = armed.clone();
        let dropped = dropped.clone();
        laptop.set_source_hook(Some(Arc::new(move |_: &AgentKey, _: &SessionEvent| {
            if armed.swap(false, Ordering::SeqCst) {
                dropped.store(true, Ordering::SeqCst);
                SourceVerdict::Drop
            } else {
                SourceVerdict::Keep
            }
        })));
    }
    net.spawn(
        AgentDecl::new("worker", "desk")
            .steps(turns(2, 3))
            .prompt("go"),
    )
    .await
    .unwrap();
    wait_origin_says(&net, "worker", "t0-2").await;
    wait_current(&net, "laptop", "worker").await;
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    let seen = chat.events().len();

    armed.store(true, Ordering::SeqCst);
    net.send("worker", "two").await.unwrap();
    chat.observe_until(
        |events| marks(&events[seen..]).contains(&Mark::Detached),
        PATIENCE,
    )
    .await
    .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    let desk = net.host("desk").unwrap().host_id;
    assert!(laptop.host_ready(desk), "the host was never lost");
    holds_for(
        "the marker to read Detached through the backoff",
        Duration::from_millis(200),
        || async {
            replica_state(&net, "laptop", "worker").await.unwrap().1 == Some(Marker::Detached)
        },
    )
    .await
    .unwrap();

    net.advance(Duration::from_secs(1)).unwrap();
    chat.observe_until(
        |events| {
            since_last_detached(&events[seen..])
                .iter()
                .any(|m| matches!(m, Mark::CaughtUp(_)))
        },
        PATIENCE,
    )
    .await
    .unwrap();
    wait_current(&net, "laptop", "worker").await;
    println!(
        "the laptop's chat through a stream lost with the link up:\n{}",
        chat.transcript()
    );
    drop(laptop);
    net.shutdown().await.unwrap();
}

/// A host that goes away and comes back between two of the follower's
/// looks at its route is followed again at once, and so is one whose new
/// link is up before the follower notices that its stream on the old one
/// ended: the backoff runs on the policy clock, which nothing here
/// advances, so a follower that missed either would wait on it forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_back_before_the_follower_looks_is_followed_without_its_backoff() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(3, 2))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(6, |_, _| {}))
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "t0-1").await;
    wait_current(&net, "laptop", "worker").await;
    for _ in 0..3 {
        sever(&mut net).await;
        restore(&mut net).await;
        wait_current(&net, "laptop", "worker").await;
    }

    // The follower is busy with a change to the worker while the link is
    // cut and a new one comes up, and reads its stream's end only after.
    let (release, released) = tokio::sync::watch::channel(false);
    let (heard, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    let held = Arc::new(AtomicBool::new(false));
    net.runtime("laptop")
        .unwrap()
        .set_inventory_hook(Some(Arc::new(move |_, event: &InventoryEvent| {
            let about_an_agent = matches!(&event.of, Some(inventory_event::Of::Agent(_)));
            if !about_an_agent || held.swap(true, Ordering::SeqCst) {
                return SourceVerdict::Keep;
            }
            let _ = heard.send(());
            let mut released = released.clone();
            SourceVerdict::Hold(Box::pin(async move {
                let _ = released.wait_for(|released| *released).await;
            }))
        })));
    net.send("worker", "two").await.unwrap();
    hearing
        .recv()
        .await
        .expect("the follower hears the worker change");
    sever(&mut net).await;
    restore(&mut net).await;
    release.send(true).unwrap();
    wait_current(&net, "laptop", "worker").await;
    net.shutdown().await.unwrap();
}

/// Fetch serves the block from the replica's own rows; below it the origin
/// answers and the page extends the block; with the origin away it is an
/// error, never an empty page.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_serves_the_block_locally_extends_it_from_the_origin_and_errors_when_away() {
    const K: u32 = 4;
    let topology = desk_and_laptop()
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(turns(4, 2))
                .prompt("go"),
        )
        .agent(
            AgentDecl::new("other", "desk")
                .steps(turns(4, 2))
                .prompt("go"),
        );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    for agent in ["worker", "other"] {
        wait_origin_says(&net, agent, "t0-1").await;
        wait_current(&net, "laptop", agent).await;
    }
    // A break past the cap leaves each replica a block of the newest K.
    sever(&mut net).await;
    for agent in ["worker", "other"] {
        for turn in 1..3 {
            net.send(agent, &format!("turn {turn}")).await.unwrap();
            wait_origin_says(&net, agent, &format!("t{turn}-1")).await;
        }
    }
    restore(&mut net).await;
    for agent in ["worker", "other"] {
        wait_current(&net, "laptop", agent).await;
    }

    // Inside the block: the replica's own rows.
    let top = fetch(&net, "laptop", "worker", None, 2).await.unwrap();
    assert_eq!(top.items.len(), 2);
    assert!(!top.exhausted);

    // Below it: the origin, and the block grows down to the first row.
    let origin = origin_rows(&net, "worker").await;
    assert!(origin.len() > K as usize);
    let all = fetch(&net, "laptop", "worker", None, 100).await.unwrap();
    assert_eq!(all.items.len(), origin.len());
    assert!(all.exhausted);
    {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        let page = store
            .page(&net.agent("worker").unwrap().key(), None, 100)
            .unwrap();
        assert_eq!(page.items.len(), origin.len());
        assert_eq!(
            page.end,
            PageEnd::Exhausted,
            "the origin said there is no more"
        );
    }
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();

    // With the origin away, what the block holds is still served, and
    // asking below it is an error.
    sever(&mut net).await;
    let held = fetch(&net, "laptop", "other", None, 100).await.unwrap();
    assert_eq!(
        held.items.len(),
        K as usize,
        "the block, and no claim of more"
    );
    assert!(!held.exhausted);
    let floor = held.items.last().unwrap().order;
    let below = fetch(&net, "laptop", "other", Some(floor), 10).await;
    assert!(
        matches!(below, Err(node::ServeError::OriginUnreachable)),
        "{below:?}"
    );
    net.shutdown().await.unwrap();
}

/// After a Reset the old block's rows lie below the new one; the replica
/// retention sweep takes them first and then trims the block to K, and
/// paging down from the top never skips an order. The sweep runs on the
/// retention interval of the policy clock.
#[tokio::test(flavor = "multi_thread")]
async fn trimming_after_a_reset_keeps_the_block_whole_and_runs_on_the_retention_interval() {
    const K: u32 = 4;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(6, 2))
            .prompt("go"),
    );
    let mut net = Net::start_with(
        topology,
        options(K, |host, launch| {
            if host == "laptop" {
                launch.replica_budget_bytes = 1;
            }
        }),
    )
    .await
    .unwrap();
    let key = net.agent("worker").unwrap().key();
    wait_origin_says(&net, "worker", "t0-1").await;
    wait_current(&net, "laptop", "worker").await;
    net.send("worker", "two").await.unwrap();
    wait_origin_says(&net, "worker", "t1-1").await;
    wait_current(&net, "laptop", "worker").await;
    // The whole history, paged down from the origin into the block.
    let all = fetch(&net, "laptop", "worker", None, 100).await.unwrap();
    assert!(all.exhausted);

    // Beyond the cap while away: the reconnect is a Reset, and the old
    // rows stay below the fresh tail.
    sever(&mut net).await;
    for turn in 2..5 {
        net.send("worker", &format!("turn {turn}")).await.unwrap();
        wait_origin_says(&net, "worker", &format!("t{turn}-1")).await;
    }
    restore(&mut net).await;
    wait_current(&net, "laptop", "worker").await;
    let (floor, stored) = {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        let row = store.agent(&key).unwrap().unwrap();
        (
            row.complete_from_order.unwrap(),
            store.get(&key, &all.items.last().unwrap().key).unwrap(),
        )
    };
    assert!(
        stored.is_some(),
        "the old rows are still stored below the block"
    );
    assert!(stored.unwrap().order < floor);

    let (sweep, _) = net
        .runtime("laptop")
        .unwrap()
        .sweep_replica_retention()
        .await
        .unwrap();
    assert!(!sweep.steps.is_empty(), "{sweep:?}");
    {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        let mut orders = Vec::new();
        let mut before = None;
        loop {
            let page = store.page(&key, before, 2).unwrap();
            orders.extend(page.items.iter().map(|item| item.order));
            before = page.items.last().map(|item| item.order);
            if page.end != PageEnd::More {
                assert_eq!(page.end, PageEnd::Boundary);
                break;
            }
        }
        assert_eq!(orders.len(), K as usize, "{orders:?}");
        assert!(
            orders.windows(2).all(|pair| pair[0] == pair[1] + 1),
            "paging down yields no gap: {orders:?}"
        );
        assert!(
            store
                .get(&key, &all.items.last().unwrap().key)
                .unwrap()
                .is_none(),
            "the stale rows went first"
        );
    }
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();
    // Below the trimmed block the origin still answers.
    let older = fetch(&net, "laptop", "worker", None, 100).await.unwrap();
    assert!(older.exhausted);
    assert_eq!(older.items.len(), origin_rows(&net, "worker").await.len());

    // The sweep is scheduled: one retention interval of policy time runs it.
    let runtime = net.runtime("laptop").unwrap();
    let mut retention = runtime.retention();
    let runs = retention.borrow().runs;
    net.advance(Duration::from_millis(
        (Launch::default().retention_interval_ms + 1) as u64,
    ))
    .unwrap();
    tokio::time::timeout(
        PATIENCE,
        retention.wait_for(|seen| seen.runs > runs && seen.replicas.is_some()),
    )
    .await
    .expect("the scheduled sweep runs")
    .unwrap();
    drop(retention);
    drop(runtime);
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// Under OnDemand no agent gets a source until a client subscribes, and
/// then only that one: a background wake warms one chat. Switching to
/// Listed sweeps every other agent current. A source for an agent that
/// exits closes once its catch-up has landed, and opens again when its
/// origin resumes it.
#[tokio::test(flavor = "multi_thread")]
async fn on_demand_warms_one_chat_listed_sweeps_the_rest_and_exited_agents_close() {
    const K: u32 = 6;
    let topology = desk_and_laptop()
        .agent(
            AgentDecl::new("named", "desk")
                .steps(turns(4, 1))
                .prompt("go"),
        )
        .agent(
            AgentDecl::new("quiet", "desk")
                .steps(turns(4, 1))
                .prompt("go"),
        );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    let named = net.agent("named").unwrap().key();
    let quiet = net.agent("quiet").unwrap().key();
    for agent in ["named", "quiet"] {
        wait_origin_says(&net, agent, "t0-0").await;
        wait_current(&net, "laptop", agent).await;
    }
    let laptop = net.runtime("laptop").unwrap();
    assert_eq!(laptop.open_sources(), {
        let mut both = vec![named.clone(), quiet.clone()];
        both.sort();
        both
    });

    // The phone asleep: the link goes, work happens, the app wakes OnDemand.
    laptop.set_source_policy(SourcePolicy::OnDemand);
    drop(laptop);
    sever(&mut net).await;
    for agent in ["named", "quiet"] {
        net.send(agent, "two").await.unwrap();
        wait_origin_says(&net, agent, "t1-0").await;
    }
    restore(&mut net).await;
    let laptop = net.runtime("laptop").unwrap();
    observe::eventually("the laptop's inventory to catch up", PATIENCE, || async {
        laptop.host_ready(net.host("desk").unwrap().host_id)
    })
    .await
    .unwrap();
    holds_for(
        "no source opens by itself",
        Duration::from_millis(300),
        || {
            let open = laptop.open_sources();
            async move { open.is_empty() }
        },
    )
    .await
    .unwrap();

    // The notification names one chat; opening it brings that one current.
    let mut chat = net.observe("laptop", "named", 10).await.unwrap();
    chat.observe_until(
        |events| {
            since_last_detached(events)
                .iter()
                .any(|m| matches!(m, Mark::CaughtUp(_)))
        },
        PATIENCE,
    )
    .await
    .unwrap();
    wait_current(&net, "laptop", "named").await;
    assert_eq!(laptop.open_sources(), vec![named.clone()]);
    let stale = replica_state(&net, "laptop", "quiet").await.unwrap();
    assert!(
        stale.0 < origin_revision(&net, "quiet").await,
        "quiet was left alone"
    );

    // Foreground: every listed agent gets a source.
    laptop.set_source_policy(SourcePolicy::Listed);
    wait_current(&net, "laptop", "quiet").await;

    // An agent that exits: its source closes once its catch-up has landed.
    let quiet_id = net.agent("quiet").unwrap().id;
    net.runtime("desk")
        .unwrap()
        .stop(quiet_id, StopMode::Graceful)
        .await
        .unwrap();
    observe::eventually("quiet's source to close", PATIENCE, || async {
        !laptop.open_sources().contains(&quiet)
    })
    .await
    .unwrap();
    wait_current(&net, "laptop", "quiet").await;
    let row = laptop.store().await.agent(&quiet).unwrap().unwrap();
    assert_eq!(row.lifecycle, wire::Lifecycle::Exited as i32);
    assert_eq!(laptop.open_sources(), vec![named]);

    // Resumed by its origin, it is followed again.
    net.resume("quiet", Some("again")).await.unwrap();
    observe::eventually("quiet's source to reopen", PATIENCE, || async {
        laptop.open_sources().contains(&quiet)
    })
    .await
    .unwrap();
    wait_origin_says(&net, "quiet", "t0-0").await;
    wait_current(&net, "laptop", "quiet").await;
    let row = laptop.store().await.agent(&quiet).unwrap().unwrap();
    assert_eq!(
        (row.lifecycle, row.incarnation),
        (wire::Lifecycle::Live as i32, 2)
    );
    drop(laptop);
    net.shutdown().await.unwrap();
}

/// A runtime switched to OnDemand with its link still up, as a phone put
/// away while it still runs is, closes every source no client is
/// subscribed to at once and keeps the watched one: work at the origin
/// reaches only the watched chat until the runtime is Listed again. The
/// watched chat's source opens again when its host comes back, without the
/// client subscribing again.
#[tokio::test(flavor = "multi_thread")]
async fn switching_to_on_demand_closes_every_source_no_client_watches() {
    const K: u32 = 6;
    let topology = desk_and_laptop()
        .agent(
            AgentDecl::new("named", "desk")
                .steps(turns(4, 1))
                .prompt("go"),
        )
        .agent(
            AgentDecl::new("quiet", "desk")
                .steps(turns(4, 1))
                .prompt("go"),
        );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    let named = net.agent("named").unwrap().key();
    for agent in ["named", "quiet"] {
        wait_origin_says(&net, agent, "t0-0").await;
        wait_current(&net, "laptop", agent).await;
    }
    let laptop = net.runtime("laptop").unwrap();
    assert_eq!(laptop.open_sources().len(), 2);

    let chat = net.observe("laptop", "named", 10).await.unwrap();
    laptop.set_source_policy(SourcePolicy::OnDemand);
    observe::eventually("the unwatched source to close", PATIENCE, || async {
        laptop.open_sources() == vec![named.clone()]
    })
    .await
    .unwrap();
    for agent in ["named", "quiet"] {
        net.send(agent, "two").await.unwrap();
        wait_origin_says(&net, agent, "t1-0").await;
    }
    wait_current(&net, "laptop", "named").await;
    let stale = replica_state(&net, "laptop", "quiet").await.unwrap();
    assert!(
        stale.0 < origin_revision(&net, "quiet").await,
        "quiet was left alone"
    );

    sever(&mut net).await;
    net.send("named", "three").await.unwrap();
    wait_origin_says(&net, "named", "t2-0").await;
    restore(&mut net).await;
    wait_current(&net, "laptop", "named").await;
    assert_eq!(laptop.open_sources(), vec![named.clone()]);

    laptop.set_source_policy(SourcePolicy::Listed);
    wait_current(&net, "laptop", "quiet").await;
    drop(chat);
    drop(laptop);
    net.shutdown().await.unwrap();
}

/// A replica of a host that lost power holds rows the host no longer has.
/// No source of that host opens before its generation is compared, not
/// even for a chat a client opens while the host is away, so the laptop
/// drops them and takes a fresh tail: every chat sees a Reset and never a
/// row the origin lost.
#[tokio::test(flavor = "multi_thread")]
async fn a_rewound_origin_is_compared_before_any_source_opens_and_its_lost_rows_never_return() {
    const K: u32 = 6;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(4, 1))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    let key = net.agent("worker").unwrap().key();
    wait_origin_says(&net, "worker", "t0-0").await;
    wait_current(&net, "laptop", "worker").await;
    observe::eventually("the journal read to its end", PATIENCE, || async {
        let end = net.journal_end("worker").unwrap();
        net.runtime("desk")
            .unwrap()
            .store()
            .await
            .cursor(&key)
            .unwrap()
            == end
    })
    .await
    .unwrap();
    let durable = net.journal_end("worker").unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.send("worker", "lost").await.unwrap();
    wait_origin_says(&net, "worker", "t1-0").await;
    wait_current(&net, "laptop", "worker").await;
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    // The laptop loses desk before desk comes back rewound.
    sever(&mut net).await;
    let laptop = net.runtime("laptop").unwrap();
    let desk = net.host("desk").unwrap().host_id;
    observe::eventually("the laptop to detach desk's agents", PATIENCE, || async {
        replica_state(&net, "laptop", "worker").await.unwrap().1 == Some(Marker::Detached)
    })
    .await
    .unwrap();
    assert!(!laptop.host_ready(desk), "an away host is not ready");
    let generation = net.generation("desk").unwrap();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: durable,
        }],
    )
    .await
    .unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);

    // A client opening the chat while desk is away opens no source: none
    // may open before desk's new inventory is in and compared.
    let mut away = net.observe("laptop", "worker", 10).await.unwrap();
    holds_for(
        "no source opens while desk is away",
        Duration::from_millis(300),
        || {
            let open = laptop.open_sources();
            let ready = laptop.host_ready(desk);
            async move { open.is_empty() && !ready }
        },
    )
    .await
    .unwrap();
    drop(laptop);
    restore(&mut net).await;
    for chat in [&mut away, &mut chat] {
        let events = chat
            .observe_until(
                |events| {
                    since_last_detached(events)
                        .iter()
                        .any(|m| matches!(m, Mark::CaughtUp(_)))
                },
                PATIENCE,
            )
            .await
            .unwrap();
        assert!(
            since_last_detached(events).contains(&Mark::Reset),
            "{}",
            chat.transcript()
        );
        let from = events
            .iter()
            .rposition(|event| matches!(event.of, Some(session_event::Of::Detached(_))))
            .unwrap_or(0);
        assert!(
            !events[from..].iter().any(|event| matches!(
                &event.of,
                Some(session_event::Of::Item(item)) if item.text.contains("t1-0")
            )),
            "the chat never sees a row the origin lost again: {}",
            chat.transcript()
        );
    }
    wait_current(&net, "laptop", "worker").await;
    let held = net
        .runtime("laptop")
        .unwrap()
        .store()
        .await
        .cut(&key, u32::MAX)
        .unwrap()
        .held;
    assert!(
        !held.iter().any(|item| item.text.contains("t1-0")),
        "a row the origin lost never returns: {held:#?}"
    );
    assert_eq!(
        net.runtime("laptop")
            .unwrap()
            .store()
            .await
            .host_generation(net.host("desk").unwrap().host_id.as_bytes())
            .unwrap(),
        Some(generation + 1)
    );
    net.shutdown().await.unwrap();
}

/// An exited agent's replica settles only once its own stream has passed
/// the exit. The origin publishes the final records and then the Exited
/// row, on streams that travel independently; here the session stream is
/// held back until the Exited row has been applied, and the replica still
/// ends with the origin's newest row.
#[tokio::test(flavor = "multi_thread")]
async fn an_exited_agent_settles_only_once_its_own_stream_has_passed_the_exit() {
    let mut net = Net::start_with(desk_and_laptop(), options(6, |_, _| {}))
        .await
        .unwrap();

    // Once armed, the laptop's sources hold the first record they are sent
    // until the gate opens; markers pass. A source reads the hook when its
    // stream opens, so it is set before the agent exists.
    let armed = Arc::new(AtomicBool::new(false));
    let holding = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(tokio::sync::Notify::new());
    let laptop = net.runtime("laptop").unwrap();
    {
        let armed = armed.clone();
        let holding = holding.clone();
        let gate = gate.clone();
        laptop.set_source_hook(Some(Arc::new(move |_: &AgentKey, event: &SessionEvent| {
            let record = matches!(
                event.of,
                Some(
                    session_event::Of::Snapshot(_)
                        | session_event::Of::Item(_)
                        | session_event::Of::Append(_)
                )
            );
            if record && armed.swap(false, Ordering::SeqCst) {
                holding.store(true, Ordering::SeqCst);
                let gate = gate.clone();
                SourceVerdict::Hold(Box::pin(async move { gate.notified().await }))
            } else {
                SourceVerdict::Keep
            }
        })));
    }
    net.spawn(
        AgentDecl::new("brief", "desk")
            .steps(turns(1, 2))
            .prompt("go"),
    )
    .await
    .unwrap();
    let brief = net.agent("brief").unwrap().clone();
    let key = brief.key();
    wait_origin_says(&net, "brief", "t0-1").await;
    wait_current(&net, "laptop", "brief").await;
    armed.store(true, Ordering::SeqCst);
    net.runtime("desk")
        .unwrap()
        .stop(brief.id, StopMode::Graceful)
        .await
        .unwrap();
    observe::eventually(
        "the laptop to list brief exited while holding an exit record",
        PATIENCE,
        || async {
            holding.load(Ordering::SeqCst)
                && laptop
                    .store()
                    .await
                    .agent(&key)
                    .unwrap()
                    .is_some_and(|row| row.lifecycle == wire::Lifecycle::Exited as i32)
        },
    )
    .await
    .unwrap();
    let (cursor, _) = replica_state(&net, "laptop", "brief").await.unwrap();
    assert!(
        cursor < origin_revision(&net, "brief").await,
        "the exit's records have not landed when the Exited row has"
    );
    gate.notify_one();

    wait_current(&net, "laptop", "brief").await;
    let origin = origin_rows(&net, "brief").await;
    let mut replica = laptop.store().await.cut(&key, u32::MAX).unwrap().held;
    replica.sort_by_key(|item| item.order);
    assert_eq!(
        replica.last().map(|item| (&item.key, item.revision)),
        origin.last().map(|item| (&item.key, item.revision)),
        "the replica ends with the origin's newest row"
    );
    observe::eventually("brief's source to close", PATIENCE, || async {
        !laptop.open_sources().contains(&key)
    })
    .await
    .unwrap();
    drop(laptop);
    net.shutdown().await.unwrap();
}

/// A person on the laptop works with the blobs of the desk's agent as with
/// its own: an attachment stored from the laptop is written into the
/// agent's directory on the desk, a diff of its working tree is made on the
/// desk, and the laptop reads each blob from the desk once and keeps the
/// bytes under the replica, so the next read needs no link.
#[tokio::test(flavor = "multi_thread")]
async fn blobs_and_diffs_of_a_peers_agent_are_made_at_its_origin_and_kept_by_the_reader() {
    use wire::client_service_server::ClientService as _;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(1, 1))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(8, |_, _| {}))
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "t0-0").await;
    wait_current(&net, "laptop", "worker").await;
    let worker = net.agent("worker").unwrap().clone();
    let agent_id = worker.id.as_bytes().to_vec();
    let own = net.host("desk").unwrap().data_dir.clone();

    let work = net.host("desk").unwrap().work.clone();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(work.join("deploy.sh"), "rsync build/ prod:/srv\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "first"]);
    std::fs::write(work.join("deploy.sh"), "rsync --delete build/ prod:/srv\n").unwrap();

    let laptop = net.client("laptop").unwrap();
    let stored = laptop
        .put_blob(tonic::Request::new(wire::PutBlobRequest {
            agent_id: agent_id.clone(),
            name: "notes.txt".to_owned(),
            mime: "text/plain".to_owned(),
            bytes: b"from the laptop".to_vec(),
        }))
        .await
        .expect("the desk stores the laptop's attachment")
        .into_inner();
    let hex = |hash: &[u8]| hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let on_desk = |hash: &[u8]| {
        walk(&own).into_iter().find(|path| {
            path.file_name()
                .is_some_and(|name| name == hex(hash).as_str())
        })
    };
    let written = on_desk(&stored.hash).expect("the attachment is in the desk's data");
    assert_eq!(std::fs::read(written).unwrap(), b"from the laptop");

    let diff = laptop
        .diff(tonic::Request::new(wire::DiffRequest {
            agent_id: agent_id.clone(),
            base: Some(wire::DiffBase {
                base: Some(wire::diff_base::Base::WorkingTree(wire::Empty {})),
            }),
        }))
        .await
        .expect("the desk diffs its agent's working tree")
        .into_inner();
    let patch = diff.patch.unwrap();
    assert!(
        on_desk(&patch.hash).is_some(),
        "the patch is the desk's blob"
    );

    let read = |hash: Vec<u8>| {
        let laptop = laptop.clone();
        let agent_id = agent_id.clone();
        async move {
            laptop
                .get_blob(tonic::Request::new(wire::GetBlobRequest { agent_id, hash }))
                .await
                .map(|response| response.into_inner().bytes)
        }
    };
    let bytes = read(patch.hash.clone())
        .await
        .expect("the laptop reads the patch");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("+rsync --delete build/ prod:/srv"), "{text}");

    // Kept: with the desk away the laptop still has the patch it read, and
    // nothing it never read.
    sever(&mut net).await;
    assert_eq!(
        read(patch.hash.clone())
            .await
            .expect("kept under the replica"),
        text.as_bytes()
    );
    let unread = read(stored.hash.clone()).await;
    assert!(unread.is_err(), "{unread:?}");
    net.shutdown().await.unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A dump taken where another host's agent is only a replica asks that
/// host for its side: the agent's journal tail, facts ring, checkpoint and
/// specs land beside the replica's row and slice, with that host's own row,
/// slice and manifest (never its daemon log, which covers every profile its
/// installation serves); planted secrets stay out of every file. A host
/// that cannot be reached is named in the manifest's errors.
#[tokio::test(flavor = "multi_thread")]
async fn a_dump_gathers_the_host_side_of_a_peers_agents() {
    const TOKEN: &str = "ghp_PLANTEDgathertoken0001abcdef";
    const KEY: &str = "sk-ant-api03-PLANTEDgatherkey0002";
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![text(&format!("pushed with {TOKEN}")), Step::TurnEnd])
            .prompt(&format!("deploy using {KEY}")),
    );
    let mut net = Net::start_with(topology, NetOptions::default())
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "pushed with").await;
    wait_current(&net, "laptop", "worker").await;
    let worker = net.agent("worker").unwrap().clone();
    let request = wire::DumpRequest {
        agent_ids: Vec::new(),
        reason: "gathered".to_owned(),
        automatic: false,
    };

    let bundle = net
        .runtime("laptop")
        .unwrap()
        .dump(request.clone())
        .await
        .unwrap();
    let agent = bundle.join("agents").join(worker.id.to_string());
    let files: Vec<String> = walk(&bundle)
        .iter()
        .map(|path| {
            path.strip_prefix(&bundle)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let has = |prefix: &str| files.iter().any(|file| file.starts_with(prefix));
    let at = format!("agents/{}", worker.id);
    for (what, prefix) in [
        ("the replica's own row", format!("{at}/row.pb")),
        ("the replica's own slice", format!("{at}/store.pb")),
        ("the host's journal tail", format!("{at}/journal/")),
        ("the host's facts ring", format!("{at}/part/facts/")),
        ("the spec", format!("{at}/part/spec.")),
        ("the host's row", format!("{at}/host/row.pb")),
        ("the host's slice", format!("{at}/host/store.pb")),
        (
            "the host's manifest",
            format!("hosts/{}/manifest.json", worker.host_id),
        ),
    ] {
        assert!(
            has(&prefix),
            "{what} ({prefix}) is in the bundle: {files:#?}"
        );
    }
    assert!(
        files
            .iter()
            .any(|file| file.starts_with(&format!("{at}/part/facts/"))
                && file.ends_with(".checkpoint")),
        "the host's facts checkpoint is in the bundle: {files:#?}"
    );
    for path in walk(&bundle) {
        let bytes = std::fs::read(&path).unwrap();
        for secret in [TOKEN, KEY] {
            let hex = interpret::to_hex(secret.as_bytes());
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_bytes())
                    && !bytes.windows(hex.len()).any(|w| w == hex.as_bytes()),
                "{secret} is in {}",
                path.display()
            );
        }
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    let entry = &manifest["agents"][0];
    assert_eq!(entry["gathered_from"], worker.host_id.to_string());
    assert!(
        entry["part"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part.as_str().unwrap().starts_with("facts/")),
        "{manifest:#}"
    );
    assert!(agent.join("journal").is_dir());
    println!(
        "the laptop's dump holds the desk's side of its worker: {} files",
        files.len()
    );

    // The desk away: its side is missing, and the manifest says why.
    sever(&mut net).await;
    let bundle = net.runtime("laptop").unwrap().dump(request).await.unwrap();
    assert!(!bundle.join(&at).join("journal").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    let errors = manifest["errors"].to_string();
    assert!(errors.contains("desk cannot be reached"), "{manifest:#}");
    println!("with the desk away the manifest says: {errors}");

    net.shutdown().await.unwrap();
}
