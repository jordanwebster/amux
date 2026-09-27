//! Whole-frame goldens reached through served hosts: a testnet desk and
//! laptop running real daemons and agent processes on the fake providers,
//! observed from the laptop's client service the way a terminal there
//! subscribes, and drawn by the fleet and chat views into the test
//! backend. The fleet at two widths, a chat with its strip, and a replica
//! chat across its origin's rewind: rows kept on screen until the Reset's
//! CaughtUp, then the rebuilt transcript.
//!
//! Rewrite with `UPDATE_GOLDENS=1 just test-tui`; CI refuses to rewrite.

#![cfg(feature = "fixtures")]

mod common;

use std::time::Duration;

use common::{assert_golden, capture};
use provider_fakes::script::{Ask, Step};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use testnet::observe::{caught_up, inventory_caught_up};
use testnet::{AgentDecl, FakeKind, JournalCut, Net, PATIENCE, Topology};
use tui::chat::ChatView;
use tui::fleet::FleetView;
use tui::{ColorMode, Theme};
use ui_state::{Connection, FleetMsg, FleetState, Msg, SessionState};
use wire::{InventoryEvent, SessionEvent, session_event};

fn theme() -> Theme {
    Theme::dark(ColorMode::TrueColor)
}

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

fn topology() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
        .agent(
            AgentDecl::new("scout", "desk")
                .kind(FakeKind::ClaudePty)
                .steps(vec![
                    text("pty.sock is named in crates/agent-dir/src/lib.rs."),
                    Step::TurnEnd,
                ])
                .prompt("Find where the attach socket is named."),
        )
        .agent(
            AgentDecl::new("coder", "desk")
                .kind(FakeKind::Codex)
                .steps(vec![text("Hello from Codex."), Step::TurnEnd])
                .prompt("Say hello."),
        )
        .agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![
                    text("Ready when you are."),
                    Step::TurnEnd,
                    text("Working on the relay tests."),
                    Step::WaitFor {
                        path: "hold".into(),
                    },
                    Step::TurnEnd,
                ])
                .prompt("Get ready."),
        )
}

/// The fleet as `host`'s inventory says it once every agent is idle.
async fn fleet_at(net: &Net, host: &str) -> FleetState {
    let mut inventory = net.observe_inventory(host).await.unwrap();
    let events: Vec<InventoryEvent> = inventory
        .observe_until(
            |events| {
                // Every agent through its first turn, so no row is caught
                // mid-start.
                let agents = testnet::observe::inventory_agents(events);
                inventory_caught_up(events)
                    && agents.len() == 3
                    && agents.iter().all(|agent| {
                        agent.lifecycle() == wire::Lifecycle::Live
                            && agent.phase() == wire::Phase::Idle
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let mut fleet = FleetState::new();
    fleet.update(FleetMsg::Connection(Connection::Live));
    for event in events {
        fleet.update(FleetMsg::Event(Box::new(event)));
    }
    fleet
}

/// The fleet as `host`'s inventory says it once `agent` is listed.
async fn fleet_listing(net: &Net, host: &str, agent: &[u8]) -> FleetState {
    let mut inventory = net.observe_inventory(host).await.unwrap();
    let events: Vec<InventoryEvent> = inventory
        .observe_until(
            |events| {
                inventory_caught_up(events)
                    && testnet::observe::inventory_agents(events)
                        .iter()
                        .any(|listed| listed.agent_id == agent)
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let mut fleet = FleetState::new();
    fleet.update(FleetMsg::Connection(Connection::Live));
    for event in events {
        fleet.update(FleetMsg::Event(Box::new(event)));
    }
    fleet
}

/// A chat's state after `events`, with its fleet entry and host.
fn chat_state(fleet: &FleetState, agent: &[u8], events: &[SessionEvent]) -> SessionState {
    let entry = fleet.find(agent).expect("the agent is listed").clone();
    let host = fleet.host(&entry.host_id).cloned();
    let mut state = SessionState::new(entry);
    state.update(Msg::Connection(Connection::Live));
    if let Some(host) = host {
        state.update(Msg::Host(host));
    }
    for event in events {
        state.update(Msg::Event(event.clone()));
    }
    state
}

/// The newest item's time: "now" for ages, so none of them moves.
fn now_of(events: &[SessionEvent]) -> i64 {
    events
        .iter()
        .filter_map(|event| match &event.of {
            Some(session_event::Of::Item(item)) => Some(item.at_ms),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn draw_chat(state: &SessionState, width: u16, height: u16) -> Buffer {
    let mut view = ChatView::new(state.agent().agent_id.clone(), 0, false);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, state, None, None, 0, theme());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn draw_fleet(fleet: &FleetState, now_ms: i64, width: u16, height: u16) -> Buffer {
    let mut view = FleetView::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, fleet, None, now_ms, theme());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn frame(name: &str, buffer: &Buffer) {
    assert!(
        tui::vocabulary::FRAMES
            .iter()
            .any(|(known, _)| *known == name),
        "{name} is not described in tui::vocabulary::FRAMES"
    );
    assert_golden(&format!("frame_{name}"), &capture(buffer, theme()));
}

#[tokio::test(flavor = "multi_thread")]
async fn served_frames_match_their_goldens() {
    let net = Net::start(topology()).await.unwrap();
    let worker = net.agent("worker").unwrap().id;
    let fleet = fleet_at(&net, "laptop").await;
    let mut chat = net.observe("laptop", "worker", 20).await.unwrap();
    let events = chat
        .observe_until(
            |events| {
                caught_up(events)
                    && events.iter().any(|event| {
                        matches!(&event.of, Some(session_event::Of::Item(item))
                            if item.text.contains("Ready when you are"))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let now = now_of(&events) + 1_000;
    frame("fleet", &draw_fleet(&fleet, now, 110, 16));
    frame("fleet_60col", &draw_fleet(&fleet, now, 60, 16));
    let state = chat_state(&fleet, worker.as_bytes(), &events);
    frame("chat_strip", &draw_chat(&state, 110, 24));

    // A terminal Claude on the desk shows a tool server's dialog in its own
    // terminal: the laptop's chat docks the escape where the composer was,
    // with the conversation still above it.
    let mut net = net;
    let gatekeeper = net
        .spawn(
            AgentDecl::new("gatekeeper", "desk")
                .kind(FakeKind::ClaudePty)
                .steps(vec![
                    Step::Ask(Ask::ToolServerDialog {
                        server: "tracker".to_owned(),
                        tool: "sign_in".to_owned(),
                        link: false,
                        wait_for: None,
                        output: String::new(),
                    }),
                    Step::TurnEnd,
                ])
                .prompt("File the flaky relay test."),
        )
        .await
        .unwrap();
    let listed = fleet_listing(&net, "laptop", &gatekeeper.agent_id).await;
    let mut asking = net.observe("laptop", "gatekeeper", 20).await.unwrap();
    let events = asking
        .observe_until(
            // Terminal Claude's calls and the ask's own row are held until
            // the prompt's row lands, so the card can arrive first; the
            // frame waits for the rows it shows above the card. The turn
            // says nothing before the dialog: text from Claude's transcript
            // can land on either side of rows its hooks deliver.
            |events| {
                caught_up(events)
                    && ui_view::ask_card(&chat_state(&listed, &gatekeeper.agent_id, events))
                        .is_some_and(|card| !card.item_key.is_empty())
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let state = chat_state(&listed, &gatekeeper.agent_id, &events);
    assert!(
        matches!(
            ui_view::ask_card(&state).map(|card| card.body),
            Some(ui_view::AskBody::Unanswerable { .. })
        ),
        "the dialog is an ask the chat cannot answer"
    );
    frame("ask_escape", &draw_chat(&state, 110, 24));

    // The origin rewinds to a checkpoint taken before the second turn: the
    // laptop's replica is Reset while its rows stay on screen, and swaps to
    // the rebuilt transcript at CaughtUp.
    let cut = net.journal_end("worker").unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.send("worker", "Run the relay tests.").await.unwrap();
    let before = chat
        .observe_until(
            |events| {
                events.iter().any(|event| {
                    matches!(&event.of, Some(session_event::Of::Item(item))
                        if item.text.contains("Working on the relay tests"))
                })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .len();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: cut,
        }],
    )
    .await
    .unwrap();
    let events = chat
        .observe_until(
            |events| {
                let after = &events[before..];
                after
                    .iter()
                    .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
                    .is_some_and(|reset| caught_up(&after[reset..]))
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let reset = before
        + events[before..]
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
            .unwrap();
    let swap = reset
        + events[reset..]
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
            .unwrap();
    let pending = chat_state(&fleet, worker.as_bytes(), &events[..swap]);
    assert!(pending.reset_pending(), "the Reset waits for its CaughtUp");
    frame("rewind_before_swap", &draw_chat(&pending, 110, 24));
    let swapped = chat_state(&fleet, worker.as_bytes(), &events[..=swap]);
    assert!(!swapped.reset_pending());
    frame("rewind_after_swap", &draw_chat(&swapped, 110, 24));

    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}
