//! The fleet driver against an in-memory runtime.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use client::ManualClock;
use support::*;
use ui_runtime::{Fleet, HELD_ROWS, Window};
use ui_state::{AgentKey, Connection};
use wire::{FetchResponse, Kind, SessionEvent, subscribe_request};

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
    let open = tokio::spawn(Fleet::connect(client, clock.clone()));
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_agent(named(b"b")));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    // Each live agent's session subscribes; the runtime has not answered
    // yet, which holds nothing up.
    let mut streams = Streams::default();
    streams.answer(&mut calls).await;
    streams.answer(&mut calls).await;
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
    assert!(fleet.session(&key(b"b")).is_none());
    streams.until_closed(b"b").await;
    let state = fleet.state();
    assert_eq!(state.trace().replay(), *state);
    drop(state);
    let names: Vec<String> = fleet
        .dump_part()
        .files
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(names, ["client/fleet/state.txt", "client/fleet/trace.txt"]);
}

#[tokio::test]
async fn the_fleet_dump_part_writes_no_names_paths_or_status_text() {
    const NAME: &str = "PLANTEDname0001";
    const CWD: &str = "PLANTEDcwd0002";
    const WORKING_ON: &str = "PLANTEDworkingon0003";
    const EXIT: &str = "PLANTEDexit0004";
    const HOST: &str = "PLANTEDhost0005";
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, ManualClock::new(0)));
    let feed = calls.inventory().await;
    feed.send(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(wire::HostEntry {
            host_id: b"host-a".to_vec(),
            name: HOST.into(),
            last_dial_error: Some(HOST.into()),
            addrs: vec![HOST.into()],
            ..wire::HostEntry::default()
        })),
    });
    feed.send(inventory_agent(wire::Agent {
        name: NAME.into(),
        cwd: CWD.into(),
        working_on: Some(wire::WorkingOn {
            text: WORKING_ON.into(),
            updated_at_ms: 1,
        }),
        exit_cause: Some(EXIT.into()),
        ..named(b"a")
    }));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    let part = fleet.dump_part();
    for secret in [NAME, CWD, WORKING_ON, EXIT, HOST] {
        assert_eq!(part_holds(&part, secret), None, "{secret} is in the dump");
    }
    let trace = String::from_utf8(part.files[1].contents.clone()).unwrap();
    assert!(trace.contains("Agent 686f73742d61/61 Codex"), "{trace}");
    assert!(trace.contains("CaughtUp rev=0"), "{trace}");
}

fn key(id: &[u8]) -> AgentKey {
    AgentKey {
        host: b"host-a".to_vec(),
        agent: id.to_vec(),
    }
}

/// Every session stream the driver opened, by agent, so a test can count
/// the ones still open.
#[derive(Default)]
struct Streams {
    feeds: HashMap<Vec<u8>, Vec<Feed<SessionEvent>>>,
}

impl Streams {
    /// Answers the next call, a Subscribe, and checks the agent had no
    /// other stream open. Returns the agent and the tail it asked for.
    async fn answer(&mut self, calls: &mut Calls) -> (Vec<u8>, u32) {
        let (request, feed) = calls.subscribe().await;
        let Some(subscribe_request::From::Tail(tail)) = request.from else {
            panic!("a session subscribes with a tail");
        };
        let agent = request.agent_id;
        assert_eq!(
            self.open(&agent),
            0,
            "{agent:?} was subscribed to twice at once"
        );
        self.feeds.entry(agent.clone()).or_default().push(feed);
        (agent, tail)
    }

    fn open(&self, agent: &[u8]) -> usize {
        self.feeds.get(agent).map_or(0, |feeds| {
            feeds.iter().filter(|feed| !feed.is_closed()).count()
        })
    }

    /// The agent's one open stream.
    fn feed(&self, agent: &[u8]) -> &Feed<SessionEvent> {
        assert_eq!(self.open(agent), 1, "{agent:?} has one stream open");
        self.feeds[agent]
            .iter()
            .find(|feed| !feed.is_closed())
            .unwrap()
    }

    async fn until_closed(&self, agent: &[u8]) {
        tokio::time::timeout(PATIENCE, async {
            while self.open(agent) > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{agent:?}'s stream stayed open"));
    }
}

/// Serves an opening: a snapshot and the newest `tail` of `rows` rows.
fn serve(feed: &Feed<SessionEvent>, rows: u64, tail: u32) {
    feed.send(snapshot(Kind::Codex, 1, &[]));
    let first = rows.saturating_sub(u64::from(tail)) + 1;
    for order in first..=rows {
        feed.send(ev(text_item(Kind::Codex, order, order, "row")));
    }
    feed.send(caught_up(1));
}

async fn fleet_of(agents: Vec<wire::Agent>) -> (Fleet, Calls, Feed<wire::InventoryEvent>) {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, ManualClock::new(0)));
    let feed = calls.inventory().await;
    for agent in agents {
        feed.send(inventory_agent(agent));
    }
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    (fleet, calls, feed)
}

async fn session_of(fleet: &Fleet, agent: &AgentKey) -> Arc<ui_runtime::Session> {
    tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(session) = fleet.session(agent)
                && session.state().caught_up()
            {
                return session;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the agent's session opened")
}

const CHAT: Window = Window { tail: 40, cap: 200 };

#[tokio::test]
async fn home_and_the_chat_share_one_session_per_live_agent_and_never_subscribe_twice() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a"), exited(named(b"b"))]).await;
    let mut streams = Streams::default();

    // Only the live agent is subscribed to, with the fleet's few rows.
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"a"[..], HELD_ROWS));
    serve(streams.feed(b"a"), 30, tail);
    let held = session_of(&fleet, &key(b"a")).await;
    assert_eq!(held.state().transcript().len(), HELD_ROWS as usize);
    assert!(
        fleet.session(&key(b"b")).is_none(),
        "an exited agent has none"
    );
    calls.none().await;

    // A chat on it widens the same session: older rows page in from the
    // local runtime, and nothing subscribes again.
    let opening = tokio::spawn({
        let fleet = Arc::new(fleet);
        let opened = fleet.clone();
        async move { (opened.open(&key(b"a"), CHAT).await, fleet) }
    });
    let (request, reply) = calls.fetch().await;
    assert_eq!(request.before_order, Some(15));
    assert_eq!(request.limit, CHAT.tail - HELD_ROWS);
    let items = (1..=14)
        .rev()
        .map(|order| text_item(Kind::Codex, order, order, "older"))
        .collect();
    reply
        .send(Ok(FetchResponse {
            items,
            exhausted: true,
        }))
        .ok();
    let (opened, fleet) = opening.await.unwrap();
    let chat = opened.expect("the chat opens");
    assert!(Arc::ptr_eq(&chat, &held), "the chat reads home's session");
    assert_eq!(chat.state().transcript().len(), 30);
    assert_eq!(chat.state().oldest_order(), Some(1));
    calls.none().await;

    // Closing the chat narrows the session back to the fleet's rows.
    fleet.close(&key(b"a"));
    assert_eq!(held.state().transcript().len(), HELD_ROWS as usize);
    assert_eq!(held.state().oldest_order(), Some(15));
    assert_eq!(streams.open(b"a"), 1);

    // The exited agent's session exists only while a chat shows it.
    let opening = tokio::spawn({
        let fleet = fleet.clone();
        async move { fleet.open(&key(b"b"), CHAT).await.map(drop) }
    });
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"b"[..], CHAT.tail));
    serve(streams.feed(b"b"), 3, tail);
    opening
        .await
        .unwrap()
        .expect("the exited agent's chat opens");
    assert!(fleet.session(&key(b"b")).is_some());
    fleet.close(&key(b"b"));
    assert!(fleet.session(&key(b"b")).is_none());
    streams.until_closed(b"b").await;

    // Leaving the foreground closes every stream; returning reopens each
    // once, with a tail.
    fleet.set_foreground(false);
    streams.until_closed(b"a").await;
    assert!(held.suspended());
    calls.none().await;
    fleet.set_foreground(true);
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"a"[..], HELD_ROWS));
    serve(streams.feed(b"a"), 31, tail);
    until(held.changed(), || held.state().caught_up().then_some(())).await;
    assert!(!held.suspended());
    assert_eq!(held.state().transcript().head(), Some(31));
    assert!(Arc::ptr_eq(&fleet.session(&key(b"a")).unwrap(), &held));
    calls.none().await;
}

#[tokio::test]
async fn home_wakes_for_what_is_outside_the_rows_and_not_for_rows() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a")]).await;
    let mut streams = Streams::default();
    let (_, tail) = streams.answer(&mut calls).await;
    serve(streams.feed(b"a"), 3, tail);
    let held = session_of(&fleet, &key(b"a")).await;
    fleet.take_changed();

    // A streamed row moves the session and leaves home asleep.
    streams
        .feed(b"a")
        .send(ev(text_item(Kind::Codex, 4, 4, "streaming")));
    until(held.changed(), || {
        (held.state().transcript().head() == Some(4)).then_some(())
    })
    .await;
    assert!(fleet.take_changed().is_empty(), "a row does not wake home");

    // The turn ending arrives as a snapshot, which does.
    let mut idle = snapshot(Kind::Codex, 2, &[]);
    if let Some(wire::session_event::Of::Snapshot(snapshot)) = &mut idle.of {
        snapshot.phase = wire::Phase::Idle as i32;
    }
    streams.feed(b"a").send(idle);
    let woke = until(fleet.changed(), || {
        let changed = fleet.take_changed();
        (!changed.is_empty()).then_some(changed)
    })
    .await;
    assert_eq!(woke, [key(b"a")]);
}

#[tokio::test]
async fn an_agent_that_exits_loses_its_session_unless_a_chat_shows_it() {
    let (fleet, mut calls, inventory) = fleet_of(vec![named(b"a"), named(b"c")]).await;
    let mut streams = Streams::default();
    for _ in 0..2 {
        let (agent, tail) = streams.answer(&mut calls).await;
        serve(streams.feed(&agent), 3, tail);
    }
    session_of(&fleet, &key(b"a")).await;
    let shown = session_of(&fleet, &key(b"c")).await;
    let fleet = Arc::new(fleet);
    let chat = fleet.open(&key(b"c"), CHAT).await.expect("the chat opens");
    assert!(Arc::ptr_eq(&chat, &shown));

    inventory.send(inventory_agent(exited(named(b"a"))));
    inventory.send(inventory_agent(exited(named(b"c"))));
    streams.until_closed(b"a").await;
    assert!(fleet.session(&key(b"a")).is_none());
    // The chat keeps its session, and the fleet feeds it the exit.
    until(chat.changed(), || {
        (chat.state().agent().lifecycle() == wire::Lifecycle::Exited).then_some(())
    })
    .await;
    assert_eq!(streams.open(b"c"), 1);
    drop((chat, shown));
    fleet.close(&key(b"c"));
    streams.until_closed(b"c").await;
    assert!(fleet.session(&key(b"c")).is_none());
}
