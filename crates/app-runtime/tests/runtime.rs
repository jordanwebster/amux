//! The app runtime against a real node in process, running an agent on a
//! fake provider: the host is woken once per turn, takes the changed keys,
//! reads rows by key and acts on the chat.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_runtime::values::{ActOutcome, Draft, PageOutcome, RowOptions, ToolRowsOption};
use app_runtime::{AppRuntime, Chat, Wake};
use client::{Client, InProcess, SystemClock};
use model::{AgentKey, InputState};
use provider_fakes::script::{Ask, Question, Step, Tool, ToolClass};
use testnet::{AgentDecl, FakeKind, Net, Topology};
use tokio::sync::mpsc;
use ui_view::{AskBody, ChoiceOutcome, Pick, RowKind};

const PATIENCE: Duration = Duration::from_secs(20);

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn topology() -> Topology {
    let steps = vec![
        text("turn one"),
        Step::TurnEnd,
        Step::Ask(Ask::Permission(Tool {
            name: None,
            class: ToolClass::Consequential,
            input: None,
            outcome: Default::default(),
            wait_for: None,
        })),
        text("ran it"),
        Step::TurnEnd,
        Step::Ask(Ask::Question {
            questions: vec![
                Question {
                    question: "Which suite?".into(),
                    header: "Suite".into(),
                    options: vec!["unit".into(), "full".into()],
                    multi_select: false,
                },
                Question {
                    question: "Which platforms?".into(),
                    header: "Platforms".into(),
                    options: vec!["mac".into(), "linux".into(), "windows".into()],
                    multi_select: true,
                },
            ],
        }),
        text("answered"),
        Step::TurnEnd,
        text("turn four"),
        Step::TurnEnd,
    ];
    Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .steps(steps)
            .prompt("go"),
    )
}

/// The host's side: wakes land on a channel, as a main thread's queue.
struct Host {
    wakes: mpsc::UnboundedReceiver<Wake>,
    count: Arc<Mutex<Vec<Wake>>>,
}

async fn open(net: &Net) -> (AppRuntime, Host) {
    let client: Arc<dyn Client> = Arc::new(InProcess::new(net.client("desk").unwrap()));
    let (sender, wakes) = mpsc::unbounded_channel();
    let count = Arc::new(Mutex::new(Vec::new()));
    let seen = count.clone();
    let wake = Arc::new(move |wake: Wake| {
        seen.lock().unwrap().push(wake);
        let _ = sender.send(wake);
    });
    let local = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let runtime = AppRuntime::open(client, Arc::new(SystemClock), local, wake)
        .await
        .unwrap();
    (runtime, Host { wakes, count })
}

impl Host {
    /// Waits for the next wake naming `wanted`.
    async fn next(&mut self, wanted: Wake) {
        tokio::time::timeout(PATIENCE, async {
            loop {
                if self.wakes.recv().await.expect("the runtime is open") == wanted {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never woken for {wanted:?}"))
    }

    fn wakes_for(&self, wanted: Wake) -> usize {
        self.count
            .lock()
            .unwrap()
            .iter()
            .filter(|wake| **wake == wanted)
            .count()
    }
}

fn worker(net: &Net) -> AgentKey {
    let agent = net.agent("worker").unwrap();
    AgentKey {
        host: agent.host_id.as_bytes().to_vec(),
        agent: agent.id.as_bytes().to_vec(),
    }
}

/// Takes changes on every wake until `check` holds over the held rows.
async fn until(host: &mut Host, chat: &Chat, what: &str, check: impl Fn(&Chat) -> bool) {
    let wake = Wake::Chat(chat.id());
    let found = tokio::time::timeout(PATIENCE, async {
        loop {
            if check(chat) {
                return;
            }
            host.next(wake).await;
            chat.take_changes();
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("never saw {what}"));
}

fn says(chat: &Chat, wanted: &str) -> bool {
    let keys = chat.keys();
    chat.rows_for(&keys, &RowOptions::default())
        .iter()
        .any(|row| format!("{:?}", row.kind).contains(wanted))
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_wait_for_the_hosts_turn_and_rows_are_read_by_key() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let fleet = runtime.fleet_rows(&[]);
    assert_eq!(fleet.len(), 1, "{fleet:?}");
    assert_eq!(fleet[0].card.name, "worker");
    let hosts = runtime.hosts();
    assert!(
        hosts.iter().any(|host| host.local && host.name == "desk"),
        "{hosts:?}"
    );

    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    until(&mut host, &chat, "the first turn", |chat| {
        chat.frame().caught_up && says(chat, "turn one")
    })
    .await;
    let before = chat.keys();
    let newest = before.last().unwrap().clone();
    assert_eq!(chat.keys_above(&newest), Some(Vec::new()));
    assert_eq!(chat.keys_above("no such key"), None);

    // No wake is owed while nothing moves: the host took everything.
    chat.take_changes();
    let woken = host.wakes_for(Wake::Chat(chat.id()));

    let sent = chat
        .send(&Draft {
            text: "please run it".into(),
            attachments: Vec::new(),
        })
        .await;
    assert!(
        matches!(sent.state, InputState::Settled | InputState::Sent),
        "{sent:?}"
    );
    // Many updates land between the host's turns; each turn is one wake,
    // and no wake is owed until the host takes what the last one brought.
    let mut turns = 0;
    let mut session_moved = false;
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            host.next(Wake::Chat(chat.id())).await;
            turns += 1;
            assert_eq!(host.wakes_for(Wake::Chat(chat.id())) - woken, turns);
            session_moved |= chat.take_changes().session;
            if let Some(card) = chat.ask_card() {
                return card;
            }
        }
    })
    .await
    .expect("the permission ask");
    assert!(session_moved, "the ask moved the frame");

    // The ids only grew above the newest the host held.
    let above = chat.keys_above(&newest).unwrap();
    assert!(!above.is_empty());
    assert_eq!(chat.keys(), [before.clone(), above.clone()].concat());
    let rows = chat.rows_for(&above, &RowOptions::default());
    assert_eq!(
        rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        above
    );
    assert!(
        rows.iter()
            .any(|row| matches!(row.kind, RowKind::Prompt { .. }))
    );

    // Allow once, by the choice's position on the card.
    assert!(matches!(card.body, AskBody::Command { .. }), "{card:?}");
    let allow = card
        .choices
        .iter()
        .position(|choice| choice.outcome == ChoiceOutcome::AllowOnce)
        .expect("allow once is offered");
    assert_eq!(
        chat.answer_choice("some other ask", allow, "").await,
        ActOutcome::Rejected("that ask is no longer the one waiting".into())
    );
    assert_eq!(
        chat.answer_choice(&card.key, allow, "").await,
        ActOutcome::Done
    );
    until(&mut host, &chat, "the call to run", |chat| {
        chat.ask_card().is_none() && says(chat, "ran it")
    })
    .await;
    let decided = chat
        .rows_for(&chat.keys(), &RowOptions::default())
        .into_iter()
        .find(|row| row.decision.is_some())
        .expect("the decision lands as a row");
    assert_eq!(decided.id, card.item_key);

    // Questions: one pick per question, in order.
    chat.send(&Draft {
        text: "ask me".into(),
        attachments: Vec::new(),
    })
    .await;
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(card) = chat.ask_card() {
                return card;
            }
            host.next(Wake::Chat(chat.id())).await;
            chat.take_changes();
        }
    })
    .await
    .expect("the question ask");
    let AskBody::Question(questions) = &card.body else {
        panic!("{card:?}");
    };
    assert_eq!(questions.len(), 2);
    assert!(questions[1].multi_select);
    let picks = [Pick::Options(vec![1]), Pick::Options(vec![0, 2])];
    assert_eq!(
        chat.answer_questions(&card.key, &picks, "").await,
        ActOutcome::Done
    );
    until(&mut host, &chat, "the answers", |chat| {
        chat.ask_card().is_none() && says(chat, "answered")
    })
    .await;
    let rows = chat.rows_for(&chat.keys(), &RowOptions::default());
    let answered = format!("{rows:?}");
    assert!(answered.contains("full"), "{answered}");
    assert!(answered.contains("windows"), "{answered}");

    drop(chat);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn older_rows_arrive_below_the_oldest_the_host_holds() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    // The whole first turn, its end included: a row landing after the list
    // is taken would be paged in below but missing from it.
    until(&mut host, &chat, "the first turn to end", |chat| {
        chat.frame().caught_up && says(chat, "turn one") && says(chat, "TurnEnd")
    })
    .await;
    let everything = chat.keys();
    assert!(everything.len() >= 2, "{everything:?}");
    drop(chat);

    let chat = runtime.open_chat(&worker(&net), 1).await.unwrap();
    let held = chat.keys();
    assert_eq!(held.len(), 1);
    assert!(chat.frame().has_older);
    assert_eq!(chat.keys_below(&held[0]), Some(Vec::new()));
    let mut pages = 0;
    while chat.frame().has_older {
        assert!(matches!(chat.page_older(1).await, PageOutcome::Arrived(_)));
        pages += 1;
        assert!(pages < 20, "paging never reached the first row");
    }
    let below = chat.keys_below(&held[0]).unwrap();
    assert_eq!([below, held].concat(), everything);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn collapsed_runs_and_the_frame_read_as_the_views_say() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    until(&mut host, &chat, "the first turn", |chat| {
        chat.frame().caught_up && says(chat, "turn one")
    })
    .await;
    let frame = chat.frame();
    assert_eq!(frame.name, "worker");
    assert_eq!(frame.agent, worker(&net));
    assert!(frame.queue.is_empty() && frame.outbox.is_empty());
    let keys = chat.keys();
    let hidden = chat.rows_for(
        &keys,
        &RowOptions {
            tools: ToolRowsOption::Hide,
        },
    );
    assert_eq!(hidden.len(), keys.len(), "hidden rows keep their ids");
    let strip = chat.strip();
    assert!(strip.failed_servers.is_empty());
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dump_carries_the_fleet_and_every_open_chat() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    until(&mut host, &chat, "the first turn", |chat| {
        says(chat, "turn one")
    })
    .await;
    let bundle = runtime.dump("asked for in a test").await.unwrap();
    assert!(bundle.join("manifest.json").is_file());
    assert!(bundle.join("client/fleet/state.txt").is_file());
    let sessions = std::fs::read_dir(bundle.join("client/sessions"))
        .expect("the open chat's part")
        .count();
    assert_eq!(sessions, 1);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fleet_wakes_the_host_when_an_agent_moves() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    runtime.take_fleet_changes();
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    chat.send(&Draft {
        text: "please run it".into(),
        attachments: Vec::new(),
    })
    .await;
    tokio::time::timeout(PATIENCE, async {
        loop {
            host.next(Wake::Fleet).await;
            let changes = runtime.take_fleet_changes();
            if changes.agents.contains(&worker(&net))
                && runtime.fleet_card(&worker(&net)).unwrap().attention
                    == model::Attention::NeedsYou
            {
                return;
            }
        }
    })
    .await
    .expect("the card turns to needs-you");
    net.shutdown().await.unwrap();
}
