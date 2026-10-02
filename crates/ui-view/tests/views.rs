//! View invariants over small authored session states, and the authored
//! goldens for what no interpreter fixture reaches: an input not confirmed,
//! the exited composer, steering a queued prompt, an unanswerable or
//! dismissed ask, and the strip's facts.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use prost::Message;
use ui_state::{AgentKey, Attention, FleetMsg, FleetState, InputOutcome, Key, Msg, SessionState};
use ui_view::*;
use wire::{Item, Kind, Phase, SessionEvent, ToolClass, ToolState, session_event};

const KINDS: [Kind; 3] = [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex];

/// The window's cap: more rows than any case here delivers.
const CAP: usize = 200;

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: Some("worker".into()),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: Phase::Working as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn body(kind: Kind, tool: Option<(&str, bool)>, text: bool) -> Vec<u8> {
    use wire::{claude_pty_item as pty, claude_sdk_item as sdk, codex_item as codex};
    let class = |explore: bool| if explore { ToolClass::Read } else { ToolClass::Consequential } as i32;
    match (kind, tool) {
        (Kind::Codex, Some((name, explore))) => wire::CodexItem {
            kind: Some(codex::Kind::Work(wire::Work {
                of: Some(wire::work::Of::Command(wire::CommandWork {
                    command: name.into(),
                    action: if name == "Read" {
                        "read".into()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                })),
                state: ToolState::Succeeded as i32,
                class: class(explore),
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        (Kind::Codex, None) if text => wire::CodexItem {
            kind: Some(codex::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (Kind::Codex, None) => wire::CodexItem {
            kind: Some(codex::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
        (_, Some((name, explore))) => {
            let tool = wire::ToolCall {
                name: name.into(),
                state: ToolState::Succeeded as i32,
                class: class(explore),
                input_json: br#"{"file_path":"src/lib.rs"}"#.to_vec(),
                ..Default::default()
            };
            if kind == Kind::ClaudePty {
                wire::ClaudePtyItem {
                    kind: Some(pty::Kind::Tool(tool)),
                }
                .encode_to_vec()
            } else {
                wire::ClaudeSdkItem {
                    kind: Some(sdk::Kind::Tool(tool)),
                }
                .encode_to_vec()
            }
        }
        (Kind::ClaudePty, None) if text => wire::ClaudePtyItem {
            kind: Some(pty::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (Kind::ClaudePty, None) => wire::ClaudePtyItem {
            kind: Some(pty::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
        (_, None) if text => wire::ClaudeSdkItem {
            kind: Some(sdk::Kind::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        (_, None) => wire::ClaudeSdkItem {
            kind: Some(sdk::Kind::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
    }
}

fn item(kind: Kind, order: u64, revision: u64, tool: Option<(&str, bool)>) -> Item {
    Item {
        key: format!("k{order}"),
        order,
        revision,
        text: if tool.is_none() {
            format!("text {order}")
        } else {
            String::new()
        },
        kind: wire::kind_tag(kind).into(),
        body: body(kind, tool, true),
        at_ms: order as i64 * 1000,
        ..Item::default()
    }
}

fn read(kind: Kind, order: u64, revision: u64) -> Item {
    item(kind, order, revision, Some(("Read", true)))
}

fn snapshot(kind: Kind, phase: Phase, body: Vec<u8>, queue: Vec<wire::QueuedInput>) -> Msg {
    event(session_event::Of::Snapshot(wire::Snapshot {
        kind: wire::kind_tag(kind).into(),
        body,
        phase: phase as i32,
        queue,
        ..wire::Snapshot::default()
    }))
}

fn caught_up() -> Msg {
    event(session_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }))
}

fn page(state: &mut SessionState, items: Vec<Item>, exhausted: bool) {
    let epoch = state.epoch();
    state.update(Msg::Page {
        items,
        exhausted,
        epoch,
    });
}

fn all_rows(state: &SessionState, opts: &ChatOptions) -> Vec<Row> {
    chat_rows(state, 0..=u64::MAX, opts)
}

/// A random walk of live items, revisions and pages; after every step the
/// row invariants hold.
#[test]
fn row_ids_never_move_and_rows_by_keys_equal_rows_by_range() {
    for kind in KINDS {
        for seed in 1..25u64 {
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            let mut state = SessionState::new(agent(kind), CAP);
            state.update(snapshot(kind, Phase::Working, vec![], vec![]));
            let start = 40;
            let mut head = start;
            state.update(event(session_event::Of::Item(read(kind, head, 1))));
            state.update(caught_up());
            let expanded = HashSet::new();
            let opts = ChatOptions {
                tools: ToolRows::CollapseRuns {
                    expanded: &expanded,
                },
            };
            for revision in 2..60u64 {
                let before: Vec<Key> = all_rows(&state, &opts)
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                let choice = next() % 4;
                if choice == 3 {
                    let oldest = state.oldest_order().unwrap();
                    if oldest > 1 {
                        let items = (oldest.saturating_sub(3).max(1)..oldest)
                            .rev()
                            .map(|o| read(kind, o, revision))
                            .collect();
                        page(&mut state, items, next() % 7 == 0);
                    }
                } else {
                    let order = if choice < 2 {
                        head += 1;
                        head
                    } else {
                        start + next() % (head - start + 1)
                    };
                    let tool = match next() % 3 {
                        0 => Some(("Read", true)),
                        1 => Some(("Bash", false)),
                        _ => None,
                    };
                    state.update(event(session_event::Of::Item(item(
                        kind, order, revision, tool,
                    ))));
                }
                let rows = all_rows(&state, &opts);
                let after: Vec<Key> = rows.iter().map(|row| row.id.clone()).collect();
                // Ids keep their relative order; new ids arrive only at the edges.
                let kept: Vec<&Key> = after.iter().filter(|id| before.contains(id)).collect();
                assert_eq!(
                    kept,
                    before.iter().collect::<Vec<_>>(),
                    "{kind:?} seed {seed}"
                );
                if let (Some(first), Some(last)) = (before.first(), before.last()) {
                    let lo = after.iter().position(|id| id == first).unwrap();
                    let hi = after.iter().position(|id| id == last).unwrap();
                    assert_eq!(
                        hi - lo + 1,
                        before.len(),
                        "nothing inserted between held rows"
                    );
                }
                // Rows for keys are the same rows as by range.
                let keys: Vec<Key> = after.iter().filter(|_| next() % 2 == 0).cloned().collect();
                let by_keys = chat_rows_for(&state, &keys, &opts);
                let by_range: Vec<Row> = rows
                    .iter()
                    .filter(|row| keys.contains(&row.id))
                    .cloned()
                    .collect();
                assert_eq!(by_keys, by_range);
                // open_below only where a run starts at the oldest held row
                // and older history exists.
                let transcript = state.transcript();
                for row in &rows {
                    if let Some(run) = &row.run
                        && run.open_below
                    {
                        assert_eq!(
                            transcript.get(&run.oldest).unwrap().item.order,
                            transcript.oldest_held().unwrap()
                        );
                        assert!(transcript.has_older());
                    }
                }
            }
        }
    }
}

#[test]
fn a_range_extends_across_a_run_at_either_edge() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Working, vec![], vec![]));
        for order in 1..=3 {
            state.update(event(session_event::Of::Item(item(
                kind, order, order, None,
            ))));
        }
        for order in 4..=8 {
            state.update(event(session_event::Of::Item(read(kind, order, order))));
        }
        state.update(event(session_event::Of::Item(item(kind, 9, 9, None))));
        let expanded = HashSet::new();
        let opts = ChatOptions {
            tools: ToolRows::CollapseRuns {
                expanded: &expanded,
            },
        };
        let rows = chat_rows(&state, 2..=5, &opts);
        let orders: Vec<u64> = rows.iter().map(|row| row.order).collect();
        assert_eq!(
            orders,
            vec![2, 3, 4, 5, 6, 7, 8],
            "the summary at 8 is always included"
        );
        let summary = rows.last().unwrap();
        let run = summary.run.as_ref().unwrap();
        assert!(run.is_summary && !summary.collapsed);
        assert_eq!((run.len, run.reads), (5, 5));
        assert!(rows[2..6].iter().all(|row| row.collapsed));
        // Expanded by any member key.
        let expanded: HashSet<Key> = ["k5".to_owned()].into();
        let opts = ChatOptions {
            tools: ToolRows::CollapseRuns {
                expanded: &expanded,
            },
        };
        assert!(
            chat_rows(&state, 4..=8, &opts)
                .iter()
                .all(|row| !row.collapsed)
        );
        let hidden = chat_rows(
            &state,
            1..=9,
            &ChatOptions {
                tools: ToolRows::Hide,
            },
        );
        assert!(hidden.iter().filter(|row| row.collapsed).count() == 5);
        let shown = chat_rows(
            &state,
            1..=9,
            &ChatOptions {
                tools: ToolRows::ShowAll,
            },
        );
        assert!(shown.iter().all(|row| !row.collapsed));
    }
}

#[test]
fn attachments_keep_their_positions() {
    let image = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![1; 32],
            name: "a.png".into(),
            mime: "image/png".into(),
            size: 10,
        })),
    };
    let text = wire::Attachment {
        of: Some(wire::attachment::Of::Text(wire::InlineText {
            name: "pasted".into(),
            text: "x\ny".into(),
        })),
    };
    let tokens = composer_tokens("see \u{FFFC} and \u{FFFC}.", &[image.clone(), text.clone()]);
    assert_eq!(
        tokens,
        vec![
            Segment::Text("see ".into()),
            Segment::Attachment(AttachmentView::of(&image)),
            Segment::Text(" and ".into()),
            Segment::Attachment(AttachmentView::of(&text)),
            Segment::Text(".".into()),
        ]
    );
    assert!(matches!(
        tokens[3],
        Segment::Attachment(AttachmentView::Text { lines: 2, .. })
    ));
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        let mut prompt = item(kind, 1, 1, None);
        prompt.body = body(kind, None, false);
        prompt.text = "\u{FFFC}first".into();
        prompt.attachments = vec![image.clone()];
        state.update(event(session_event::Of::Item(prompt)));
        let rows = all_rows(&state, &ChatOptions::default());
        assert_eq!(
            rows[0].kind,
            RowKind::Prompt {
                text: vec![
                    Segment::Attachment(AttachmentView::of(&image)),
                    Segment::Text("first".into())
                ],
                steered: false
            }
        );
    }
}

fn fleet_row(host: &str, id: &str, phase: Phase, parent: Option<&str>, last: i64) -> FleetMsg {
    FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Agent(wire::Agent {
            agent_id: id.as_bytes().to_vec(),
            host_id: host.as_bytes().to_vec(),
            name: Some(id.into()),
            lifecycle: wire::Lifecycle::Live as i32,
            phase: phase as i32,
            parent: parent.map(|parent| wire::AgentParent {
                host_id: host.as_bytes().to_vec(),
                agent_id: parent.as_bytes().to_vec(),
            }),
            last_activity_ms: last,
            ..wire::Agent::default()
        })),
    }))
}

#[test]
fn the_fleet_ranks_families_by_their_loudest_member() {
    let mut fleet = FleetState::new();
    for msg in [
        fleet_row("a", "quiet", Phase::Idle, None, 900),
        fleet_row("a", "parent", Phase::Idle, None, 100),
        fleet_row("a", "child", Phase::NeedsYou, Some("parent"), 50),
        fleet_row("a", "busy", Phase::Working, None, 10),
    ] {
        fleet.update(msg);
    }
    let names = |rows: Vec<FleetRow>| {
        rows.into_iter()
            .map(|row| (row.card.name, row.depth))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(fleet_list(&fleet, &HashSet::new())),
        vec![
            ("parent".into(), 0),
            ("busy".into(), 0),
            ("quiet".into(), 0)
        ]
    );
    let expand: HashSet<Vec<u8>> = [b"parent".to_vec()].into();
    assert_eq!(
        names(fleet_list(&fleet, &expand)),
        vec![
            ("parent".into(), 0),
            ("child".into(), 1),
            ("busy".into(), 0),
            ("quiet".into(), 0)
        ]
    );
    let card = fleet_card(&fleet, b"parent").unwrap();
    assert_eq!(
        (card.attention, card.family_attention, card.children),
        (Attention::Idle, Attention::NeedsYou, 1)
    );
    // Folded, the family still counts: two members, one needing the person.
    assert_eq!((card.members, card.members_need_you), (2, 1));
    let alone = fleet_card(&fleet, b"busy").unwrap();
    assert_eq!((alone.members, alone.members_need_you), (1, 0));
    // The child's ask is not hosted in the parent's chat: the family header
    // carries its attention and nothing more.
    let header = family_header(&fleet, b"parent").unwrap();
    assert_eq!(header.attention, Attention::NeedsYou);
    assert_eq!(
        header.children[0].agent,
        AgentKey {
            host: b"a".to_vec(),
            agent: b"child".to_vec()
        }
    );
    assert!(family_header(&fleet, b"quiet").is_none());
    assert_eq!(
        family_header(&fleet, b"child")
            .unwrap()
            .parent
            .unwrap()
            .name,
        "parent"
    );
}

#[test]
fn the_review_document_parses_files_hunks_and_places_comments() {
    let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +10,4 @@ fn a()\n keep\n-old\n+new\n+more\n keep2\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\ndiff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
    let comments = vec![
        wire::ReviewComment {
            path: "src/a.rs".into(),
            line: 11,
            old_line: 0,
            text: "why new?".into(),
        },
        wire::ReviewComment {
            path: "src/a.rs".into(),
            line: 0,
            old_line: 11,
            text: "keep old".into(),
        },
        wire::ReviewComment {
            path: "new.txt".into(),
            line: 0,
            old_line: 0,
            text: "whole file".into(),
        },
    ];
    let doc = review_doc(
        &wire::Diff {
            head: "abc".into(),
            ..Default::default()
        },
        patch,
        &comments,
    );
    assert_eq!(doc.files.len(), 3);
    assert_eq!((doc.added, doc.removed), (3, 1));
    let a = &doc.files[0];
    assert_eq!(
        (a.path.as_str(), a.status, a.added, a.removed),
        ("src/a.rs", FileStatus::Modified, 2, 1)
    );
    let lines = &a.hunks[0].lines;
    assert_eq!(lines[0].old_line, Some(10));
    assert_eq!(
        (lines[2].kind, lines[2].new_line, lines[2].comments.clone()),
        (LineKind::Added, Some(11), vec!["why new?".to_owned()])
    );
    assert_eq!(lines[1].comments, vec!["keep old".to_owned()]);
    assert_eq!(doc.files[1].status, FileStatus::Added);
    assert_eq!(doc.files[1].comments, vec!["whole file".to_owned()]);
    assert_eq!(
        (
            doc.files[2].status,
            doc.files[2].old_path.as_deref(),
            doc.files[2].path.as_str()
        ),
        (FileStatus::Renamed, Some("x"), "y")
    );
}

fn full_snapshot(kind: Kind, asks: Vec<wire::Ask>, codex_asks: Vec<wire::CodexAsk>) -> Vec<u8> {
    let usage = wire::UsageLimits {
        state: wire::UsageState::NearLimit as i32,
        windows: vec![wire::UsageWindow {
            name: "5h".into(),
            used_percent: 91.0,
            resets_at_ms: Some(5),
        }],
        credits: None,
    };
    let servers = wire::ToolServerHealth {
        state: wire::HealthState::Degraded as i32,
        servers: vec![
            wire::ToolServer {
                name: "ok".into(),
                status: wire::ToolServerStatus::Ready as i32,
                error: String::new(),
            },
            wire::ToolServer {
                name: "github".into(),
                status: wire::ToolServerStatus::Failed as i32,
                error: "401".into(),
            },
        ],
    };
    let sign_in = wire::SignIn {
        state: wire::SignInState::Expired as i32,
        account: "me@example.com".into(),
        message: "Log in again".into(),
    };
    let background = wire::BackgroundProcesses {
        known: true,
        running: 2,
    };
    let tasks = wire::TaskList {
        known: true,
        entries: vec![
            wire::TaskListEntry {
                id: "1".into(),
                subject: "Read".into(),
                status: wire::TaskListStatus::Completed as i32,
                active_form: String::new(),
            },
            wire::TaskListEntry {
                id: "2".into(),
                subject: "Update copy".into(),
                status: wire::TaskListStatus::InProgress as i32,
                active_form: "Updating the copy".into(),
            },
            wire::TaskListEntry {
                id: "3".into(),
                subject: "Test".into(),
                status: wire::TaskListStatus::Pending as i32,
                active_form: String::new(),
            },
        ],
    };
    let context = wire::ContextMeter {
        known: true,
        used_tokens: 171_000,
        window_tokens: Some(200_000),
        breakdown: vec![],
    };
    match kind {
        Kind::ClaudePty => wire::ClaudePtySnapshot {
            asks,
            tasks: Some(tasks),
            context: Some(context),
            model: Some("opus".into()),
            permission_mode: Some("default".into()),
            usage: Some(usage),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_processes: Some(background),
            ..Default::default()
        }
        .encode_to_vec(),
        Kind::ClaudeSdk => wire::ClaudeSdkSnapshot {
            asks,
            tasks: Some(tasks),
            context: Some(context),
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission_mode: Some("default".into()),
            usage: Some(usage),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_processes: Some(background),
            ..Default::default()
        }
        .encode_to_vec(),
        _ => wire::CodexSnapshot {
            asks: codex_asks,
            plan: Some(tasks),
            context: Some(context),
            model: Some("gpt".into()),
            effort: Some("high".into()),
            approval_policy: Some("on-request".into()),
            sandbox: Some("workspace-write".into()),
            usage: Some(usage),
            servers: Some(servers),
            sign_in: Some(sign_in),
            background_processes: Some(background),
            ..Default::default()
        }
        .encode_to_vec(),
    }
}

fn prompt_input(kind: Kind, id: &[u8], text: &str) -> wire::Input {
    use wire::input::Of;
    let prompt = wire::PromptInput {
        text: text.into(),
        attachments: vec![],
    };
    let of = match kind {
        Kind::ClaudePty => Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Prompt(prompt)),
        }),
        Kind::ClaudeSdk => Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(prompt)),
        }),
        _ => Of::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(prompt)),
        }),
    };
    wire::Input {
        input_id: id.to_vec(),
        of: Some(of),
    }
}

fn queued(id: &[u8], steer: bool, from_agent: Option<&str>) -> wire::QueuedInput {
    wire::QueuedInput {
        input_id: id.to_vec(),
        text: format!("queued {}", String::from_utf8_lossy(id)),
        steer,
        sender: from_agent.map(|name| wire::Sender {
            value: Some(wire::sender::Value::Agent(wire::AgentSender {
                name: name.into(),
                ..Default::default()
            })),
        }),
        ..Default::default()
    }
}

/// The authored story each kind's golden tells.
fn authored(kind: Kind) -> String {
    let mut out = String::new();
    let mut state = SessionState::new(agent(kind), CAP);
    let unanswerable = wire::Ask {
        key: "menu-1".into(),
        body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
            reason: "a menu this build can't read".into(),
        })),
        ..Default::default()
    };
    let codex_question = wire::CodexAsk {
        key: "q-1".into(),
        body: Some(wire::codex_ask::Body::Question(wire::QuestionAsk {
            questions: vec![wire::Question {
                header: "Env".into(),
                question: "Which environment?".into(),
                options: vec![
                    wire::QuestionOption {
                        label: "staging (Recommended)".into(),
                        ..Default::default()
                    },
                    wire::QuestionOption {
                        label: "production".into(),
                        ..Default::default()
                    },
                ],
                allow_other: true,
                ..Default::default()
            }],
        })),
        ..Default::default()
    };
    let body = full_snapshot(kind, vec![unanswerable], vec![codex_question]);
    let mut show = |label: &str, state: &SessionState| {
        let _ = writeln!(out, "## {label}");
        let _ = writeln!(out, "  composer {:?}", composer(state, 10_000));
        let _ = writeln!(out, "  queue {:?}", queue_rows(state));
        let _ = writeln!(out, "  outbox {:?}", outbox_rows(state));
        match ask_card(state) {
            Some(card) => {
                let _ = writeln!(
                    out,
                    "  ask {} {:?} body {:?}",
                    card.key, card.state, card.body
                );
                let choices: Vec<String> = card
                    .choices
                    .iter()
                    .map(|choice| {
                        format!(
                            "{:?}{}",
                            choice.outcome,
                            if choice.primary { "*" } else { "" }
                        )
                    })
                    .collect();
                let _ = writeln!(out, "    choices [{}]", choices.join(", "));
            }
            None => {
                let _ = writeln!(out, "  ask none");
            }
        }
        let _ = writeln!(out, "  strip {:?}", session_strip(state));
    };
    state.update(snapshot(
        kind,
        Phase::NeedsYou,
        body.clone(),
        vec![
            queued(b"q1", false, None),
            queued(b"m1", false, Some("reviewer")),
        ],
    ));
    state.update(Msg::Entry(wire::Agent {
        phase: Phase::NeedsYou as i32,
        ..agent(kind)
    }));
    state.update(caught_up());
    show(
        "an ask that can't be answered here, two queued prompts, every strip fact",
        &state,
    );

    state.update(Msg::Send(prompt_input(kind, b"q2", "mine")));
    state.update(Msg::Sent(
        b"q2".to_vec(),
        ui_state::InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                queued: true,
            })),
        }),
    ));
    let queue = vec![
        queued(b"q1", true, None),
        queued(b"m1", false, Some("reviewer")),
        queued(b"q2", false, None),
    ];
    state.update(snapshot(kind, Phase::NeedsYou, body.clone(), queue));
    show(
        "q1 sent now: it reads steered; my own prompt queued behind",
        &state,
    );

    state.update(Msg::Send(prompt_input(kind, b"lost", "did this land?")));
    state.update(Msg::Sent(b"lost".to_vec(), InputOutcome::Lost));
    state.update(Msg::Send(prompt_input(kind, b"nope", "rejected one")));
    state.update(Msg::Sent(
        b"nope".to_vec(),
        ui_state::InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                reason: "draining".into(),
            })),
        }),
    ));
    state.update(Msg::Send(prompt_input(kind, b"flying", "in flight")));
    show(
        "not confirmed (resend or discard), rejected with its reason, and one in flight",
        &state,
    );

    state.update(Msg::Entry(wire::Agent {
        lifecycle: wire::Lifecycle::Exited as i32,
        exit_cause: Some("killed".into()),
        ..agent(kind)
    }));
    show(
        "exited: the ask is dismissed and the composer offers Resume",
        &state,
    );

    state.update(event(session_event::Of::Detached(wire::Detached {})));
    state.update(Msg::Entry(wire::Agent {
        incarnation: 2,
        ..agent(kind)
    }));
    show(
        "resumed but detached: drafting continues, sending waits",
        &state,
    );
    out
}

#[test]
fn authored_view_goldens() {
    let mut failures = Vec::new();
    for kind in KINDS {
        let rendered = authored(kind);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens/authored")
            .join(format!("{}.golden", wire::kind_tag(kind)));
        if std::env::var("UI_VIEW_UPDATE_GOLDENS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &rendered).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(golden) if golden == rendered => {}
            Ok(_) => failures.push(format!(
                "{} differs; rerun with UI_VIEW_UPDATE_GOLDENS=1 and review",
                path.display()
            )),
            Err(_) => failures.push(format!("{} missing", path.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A resume's first prompt is sent with the resume, so it reads sending;
/// when the new incarnation holds it in its queue, the queue row alone
/// draws it.
#[test]
fn a_sent_prompt_the_queue_lists_is_drawn_once() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Starting, vec![], vec![]));
        state.update(caught_up());
        state.update(Msg::Send(prompt_input(kind, b"r1", "resumed")));
        assert_eq!(outbox_rows(&state).len(), 1, "sending until it lands");
        state.update(snapshot(
            kind,
            Phase::NeedsYou,
            vec![],
            vec![queued(b"r1", false, None)],
        ));
        assert!(outbox_rows(&state).is_empty());
        assert_eq!(queue_rows(&state).len(), 1);
    }
}

/// A refused Send now (terminal Claude too old to steer) leaves the prompt
/// queued to run at turn end: its row reads queued again with Send now still
/// offered, and nothing lands in the outbox. The reason is the press's reply.
#[test]
fn a_refused_send_now_leaves_the_prompt_queued() {
    let kind = Kind::ClaudePty;
    let mut state = SessionState::new(agent(kind), CAP);
    state.update(snapshot(
        kind,
        Phase::Working,
        vec![],
        vec![queued(b"q1", false, None)],
    ));
    state.update(caught_up());
    state.update(Msg::Send(wire::Input {
        input_id: b"now".to_vec(),
        of: Some(wire::input::Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::SendNow(wire::SendQueuedNow {
                queued_input_id: b"q1".to_vec(),
            })),
        })),
    }));
    assert!(
        queue_rows(&state)[0].steered,
        "steered while the press is in flight"
    );
    state.update(Msg::Sent(
        b"now".to_vec(),
        InputOutcome::Reply(wire::SendInputResponse {
            of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                reason: "Send now needs Claude 2.1.283 or later".into(),
            })),
        }),
    ));
    let rows = queue_rows(&state);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].steered);
    assert!(rows[0].can_send_now);
    assert!(rows[0].can_withdraw);
    assert!(outbox_rows(&state).is_empty());
}

#[test]
fn unknown_renders_one_way() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        state.update(snapshot(kind, Phase::Starting, vec![], vec![]));
        assert_eq!(session_strip(&state), Strip::default());
    }
}

fn host_entry(host: &str, signed_in: Option<bool>) -> FleetMsg {
    FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(wire::HostEntry {
            host_id: host.as_bytes().to_vec(),
            name: host.into(),
            signed_in,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            ..wire::HostEntry::default()
        })),
    }))
}

#[test]
fn a_host_is_away_because_this_machine_signed_out_only_when_it_did() {
    let mut fleet = FleetState::new();
    fleet.update(host_entry("desk", Some(true)));
    // A profile never bound to an account is not signed out.
    fleet.update(host_entry("laptop", None));
    assert!(!signed_out(&fleet, b"laptop"));
    assert_eq!(away(&fleet, b"laptop", b"desk"), Away::Plain);

    fleet.update(host_entry("laptop", Some(false)));
    assert!(signed_out(&fleet, b"laptop"));
    assert_eq!(away(&fleet, b"laptop", b"desk"), Away::SignedOut);
    // The machine's own agents are never away for it.
    assert_eq!(away(&fleet, b"laptop", b"laptop"), Away::Plain);
    // The desk's own sign-in says nothing about this machine.
    assert_eq!(away(&fleet, b"desk", b"laptop"), Away::Plain);

    fleet.update(host_entry("laptop", Some(true)));
    assert_eq!(away(&fleet, b"laptop", b"desk"), Away::Plain);
}

#[test]
fn a_host_that_revoked_trust_is_away_for_that_reason_first() {
    let mut fleet = FleetState::new();
    fleet.update(host_entry("laptop", Some(false)));
    let FleetMsg::Event(mut desk) = host_entry("desk", Some(false)) else {
        unreachable!()
    };
    if let Some(wire::inventory_event::Of::Host(entry)) = &mut desk.of {
        entry.revoked = Some(true);
    }
    fleet.update(FleetMsg::Event(desk));
    assert_eq!(away(&fleet, b"laptop", b"desk"), Away::Revoked);
    assert_eq!(away(&fleet, b"laptop", b"laptop"), Away::Plain);
}

fn settings_of(kind: Kind, body: Vec<u8>) -> SettingsView {
    let mut state = SessionState::new(agent(kind), CAP);
    state.update(snapshot(kind, Phase::Idle, body, Vec::new()));
    settings(&state)
}

fn offered(value: &str, efforts: &[&str], default: Option<&str>) -> wire::OfferedModel {
    wire::OfferedModel {
        value: value.into(),
        display_name: value.to_uppercase(),
        description: String::new(),
        efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
        default_effort: default.map(Into::into),
        resolved_model: format!("id-{value}"),
    }
}

fn command(name: &str) -> wire::OfferedCommand {
    wire::OfferedCommand {
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn settings_mark_the_current_model_effort_and_mode() {
    let view = settings_of(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
            models: vec![
                offered("default", &["low", "high"], None),
                offered("sonnet", &["low", "medium", "high"], None),
            ],
            commands: vec![
                command("compact"),
                command("config"),
                command("stripe:test-cards"),
            ],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let current: Vec<&str> = view
        .models
        .iter()
        .filter(|model| model.current)
        .map(|model| model.value.as_str())
        .collect();
    assert_eq!(current, ["sonnet"]);
    assert_eq!(
        view.efforts
            .iter()
            .map(|effort| (effort.value.as_str(), effort.current))
            .collect::<Vec<_>>(),
        [("low", false), ("medium", false), ("high", true)],
        "the current model's efforts"
    );
    let modes: Vec<(ModeValue, bool, bool)> = view
        .modes
        .iter()
        .map(|mode| (mode.value.clone(), mode.current, mode.stops_asking))
        .collect();
    assert_eq!(
        modes,
        [
            (ModeValue::Claude("default".into()), false, false),
            (ModeValue::Claude("acceptEdits".into()), false, false),
            (ModeValue::Claude("plan".into()), true, false),
            (ModeValue::Claude("auto".into()), false, false),
            (ModeValue::Claude("bypassPermissions".into()), false, true),
        ]
    );
    assert_eq!(
        view.commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["compact", "stripe:test-cards"],
        "a terminal-only command is dropped"
    );
    assert_eq!(view.model_refusal, None);
    assert!(
        view.effort_refusal.is_some(),
        "headless Claude refuses effort"
    );
    assert_eq!(view.mode_refusal, None);
}

#[test]
fn a_reported_value_outside_the_offer_is_shown_as_current() {
    let view = settings_of(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("claude-haiku-4-5-20251001".into()),
            effort: Some("max".into()),
            permission_mode: Some("dontAsk".into()),
            models: vec![offered("haiku", &[], None)],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let last = view.models.last().unwrap();
    assert!(last.current && last.reported);
    assert_eq!(last.value, "claude-haiku-4-5-20251001");
    assert!(!view.models[0].current);
    assert_eq!(view.efforts.len(), 1);
    assert!(view.efforts[0].current && view.efforts[0].reported);
    let mode = view.modes.last().unwrap();
    assert_eq!(mode.value, ModeValue::Claude("dontAsk".into()));
    assert!(mode.current && mode.reported);
    assert_eq!(view.modes.iter().filter(|mode| mode.current).count(), 1);

    let nothing = settings_of(Kind::ClaudeSdk, Vec::new());
    assert!(nothing.models.is_empty() && nothing.efforts.is_empty());
    assert!(nothing.modes.iter().all(|mode| !mode.current));
}

#[test]
fn codex_modes_are_presets_and_a_pair_outside_them_is_reported() {
    let codex = |approval: &str, sandbox: &str| {
        settings_of(
            Kind::Codex,
            wire::CodexSnapshot {
                model: Some("gpt-a".into()),
                approval_policy: Some(approval.into()),
                sandbox: Some(sandbox.into()),
                models: vec![offered("gpt-a", &["low", "medium"], Some("medium"))],
                commands: vec![command("config")],
                ..Default::default()
            }
            .encode_to_vec(),
        )
    };
    let view = codex("never", "danger-full-access");
    let presets: Vec<(Option<String>, bool, bool)> = view
        .modes
        .iter()
        .map(|mode| match &mode.value {
            ModeValue::Codex { preset, .. } => (preset.clone(), mode.current, mode.stops_asking),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        presets,
        [
            (Some("read-only".into()), false, false),
            (Some("auto".into()), false, false),
            (Some("full-access".into()), true, true),
        ]
    );
    assert_eq!(
        view.efforts
            .iter()
            .map(|effort| (effort.value.as_str(), effort.default, effort.current))
            .collect::<Vec<_>>(),
        [("low", false, false), ("medium", true, false)],
        "no effort reported: the default is marked, none is current"
    );
    assert_eq!(view.commands.len(), 1, "a Codex skill is never filtered");
    assert_eq!(
        (
            &view.model_refusal,
            &view.effort_refusal,
            &view.mode_refusal
        ),
        (&None, &None, &None)
    );

    let view = codex("untrusted", "workspace-write");
    let mode = view.modes.last().unwrap();
    assert_eq!(
        mode.value,
        ModeValue::Codex {
            preset: None,
            approval_policy: "untrusted".into(),
            sandbox: "workspace-write".into(),
        }
    );
    assert!(mode.current && mode.reported);
    assert_eq!(view.modes.len(), 4);
}

#[test]
fn an_offered_alias_is_marked_for_the_model_id_it_resolves_to() {
    let view = settings_of(
        Kind::ClaudeSdk,
        wire::ClaudeSdkSnapshot {
            model: Some("id-sonnet".into()),
            models: vec![
                offered("default", &["low", "high"], None),
                offered("sonnet", &["low", "medium", "high"], None),
            ],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let current: Vec<(&str, bool)> = view
        .models
        .iter()
        .filter(|model| model.current)
        .map(|model| (model.value.as_str(), model.reported))
        .collect();
    assert_eq!(
        current,
        [("sonnet", false)],
        "the reported id marks the alias that resolves to it"
    );
}

#[test]
fn terminal_claude_shows_what_it_reports_and_says_how_to_type_a_change() {
    let view = settings_of(
        Kind::ClaudePty,
        wire::ClaudePtySnapshot {
            model: Some("claude-sonnet-5".into()),
            permission_mode: Some("acceptEdits".into()),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    assert_eq!(
        view.models
            .iter()
            .map(|model| (model.value.as_str(), model.current, model.reported))
            .collect::<Vec<_>>(),
        [("claude-sonnet-5", true, true)],
        "the reported model alone: nothing is offered"
    );
    assert!(view.efforts.is_empty(), "nothing reports the effort");
    assert!(view.commands.is_empty());
    assert_eq!(
        view.modes
            .iter()
            .map(|mode| (mode.value.clone(), mode.current))
            .collect::<Vec<_>>(),
        [(ModeValue::Claude("acceptEdits".into()), true)],
        "the current mode alone: there is no pick"
    );
    assert!(view.cycle_mode);
    assert_eq!((&view.model_refusal, &view.effort_refusal), (&None, &None));
    assert!(view.mode_refusal.is_some());
    let typing = view.change_by_typing.expect("the typing sentence");
    assert!(typing.contains("/model <name>") && typing.contains("/effort <level>"));

    let sdk = settings_of(Kind::ClaudeSdk, Vec::new());
    assert!(!sdk.cycle_mode, "headless Claude picks its mode");
    assert_eq!(sdk.change_by_typing, None);
}

fn tool_item(
    order: u64,
    name: &str,
    input: serde_json::Value,
    outcome: serde_json::Value,
    state: ToolState,
) -> Item {
    let class = if name == "Read" {
        ToolClass::Read
    } else {
        ToolClass::Consequential
    };
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body: wire::ClaudeSdkItem {
            kind: Some(wire::claude_sdk_item::Kind::Tool(wire::ToolCall {
                name: name.into(),
                input_json: input.to_string().into_bytes(),
                outcome_json: outcome.to_string().into_bytes(),
                state: state as i32,
                class: class as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1000,
        ..Item::default()
    }
}

/// A landed edit's patch head is its first lines, numbered on the side each
/// belongs to, with the count of the rest; an edit still running has none.
/// A collapsed run names its newest distinct subjects.
#[test]
fn a_landed_edit_shows_its_numbered_patch_head_and_a_run_its_newest_subjects() {
    use serde_json::json;
    let mut state = SessionState::new(agent(Kind::ClaudeSdk), CAP);
    let edit = json!({"file_path": "src/retry.rs", "old_string": "x", "new_string": "y"});
    let patch = json!({"structuredPatch": [{
        "oldStart": 20, "newStart": 20,
        "lines": [" fn retry() {", "-    sleep(1);", "+    sleep(delay);", "+    attempts += 1;", " }"],
    }]});
    page(
        &mut state,
        vec![
            tool_item(
                1,
                "Read",
                json!({"file_path": "src/a.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(
                2,
                "Read",
                json!({"file_path": "src/b.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(
                3,
                "Read",
                json!({"file_path": "src/c.rs"}),
                json!({}),
                ToolState::Succeeded,
            ),
            tool_item(4, "Edit", edit.clone(), patch, ToolState::Succeeded),
            tool_item(5, "Edit", edit, json!({}), ToolState::Running),
        ],
        true,
    );
    let head = patch_head(&state, &"k4".to_owned(), 3).expect("the landed edit has a head");
    let seen: Vec<(Option<u32>, LineKind, &str)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(20), LineKind::Context, "fn retry() {"),
            (Some(21), LineKind::Removed, "    sleep(1);"),
            (Some(21), LineKind::Added, "    sleep(delay);"),
        ]
    );
    assert_eq!(head.more, 2);
    assert_eq!(
        patch_head(&state, &"k5".to_owned(), 3),
        None,
        "still running"
    );
    assert_eq!(patch_head(&state, &"k1".to_owned(), 3), None, "not an edit");
    assert_eq!(run_subjects(&state, 3, 2), vec!["src/c.rs", "src/b.rs"]);

    // Codex's patch is a unified diff, numbered from its hunk header.
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    let change = |state: ToolState| {
        Item {
        key: "change".into(),
        order: 1,
        revision: 1,
        kind: wire::kind_tag(Kind::Codex).into(),
        body: wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Work(wire::Work {
                of: Some(wire::work::Of::FileChange(wire::FileChangeWork {
                    changes: vec![wire::FileChange {
                        path: "lexer.rs".into(),
                        patch: "diff --git a/lexer.rs b/lexer.rs\n--- a/lexer.rs\n+++ b/lexer.rs\n@@ -7,2 +7,2 @@\n-a\n+b\n c\n".into(),
                        ..Default::default()
                    }],
                })),
                state: state as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        ..Item::default()
    }
    };
    page(&mut codex, vec![change(ToolState::Succeeded)], true);
    let head = patch_head(&codex, &"change".to_owned(), 10).unwrap();
    let seen: Vec<(Option<u32>, LineKind)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(7), LineKind::Removed),
            (Some(7), LineKind::Added),
            (Some(8), LineKind::Context),
        ]
    );
    assert_eq!(head.more, 0);
}

/// Inside a hunk a removed "-- comment" reads "--- comment" and an added
/// "++ x" reads "+++ x"; they are lines of the patch, not file headers,
/// and the lines after them keep their numbers.
#[test]
fn a_patch_head_keeps_hunk_lines_that_look_like_file_headers() {
    let mut codex = SessionState::new(agent(Kind::Codex), CAP);
    let patch = "diff --git a/q.sql b/q.sql\n--- a/q.sql\n+++ b/q.sql\n@@ -3,2 +3,2 @@\n--- old note\n+++ new note\n select 1;\ndiff --git a/r.sql b/r.sql\n--- a/r.sql\n+++ b/r.sql\n@@ -10 +10 @@\n-a\n+b\n";
    let change = Item {
        key: "change".into(),
        order: 1,
        revision: 1,
        kind: wire::kind_tag(Kind::Codex).into(),
        body: wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Work(wire::Work {
                of: Some(wire::work::Of::FileChange(wire::FileChangeWork {
                    changes: vec![wire::FileChange {
                        path: "q.sql".into(),
                        patch: patch.into(),
                        ..Default::default()
                    }],
                })),
                state: ToolState::Succeeded as i32,
                ..Default::default()
            })),
        }
        .encode_to_vec(),
        ..Item::default()
    };
    page(&mut codex, vec![change], true);
    let head = patch_head(&codex, &"change".to_owned(), 10).unwrap();
    let seen: Vec<(Option<u32>, LineKind, &str)> = head
        .lines
        .iter()
        .map(|line| (line.number, line.kind, line.text.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some(3), LineKind::Removed, "-- old note"),
            (Some(3), LineKind::Added, "++ new note"),
            (Some(4), LineKind::Context, "select 1;"),
            (Some(10), LineKind::Removed, "a"),
            (Some(10), LineKind::Added, "b"),
        ]
    );
}
