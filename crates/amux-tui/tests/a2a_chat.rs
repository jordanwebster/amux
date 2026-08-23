//! The chat surfaces a family adds: the header's subagent marker and the
//! key that cycles through the family, the banner a child raises in its
//! parent's chat, and the rows an agent message makes in both native
//! chats.
//!
//! One fixture family serves all of them — a Claude lead with a Codex
//! child that is blocked on a command, a Claude child that finished, and a
//! grandchild — because these surfaces exist to be read together.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test a2a_chat`
//! and review the diff like code.

use amux_tui::view::{UiAction, ViewState};
use amux_tui::{ChatView, FrameContext, Theme, render};
use amux_ui::{
    Agent, AgentId, AgentParent, HostEntry, HostId, Model, Msg, ServerMsg, StreamEntry, StreamMsg,
    update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use uuid::Uuid;

const WIDTH: u16 = 88;
const HEIGHT: u16 = 26;

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_754_697_600, 0).expect("fixture epoch")
}

fn at(seconds: i64) -> DateTime<Utc> {
    t0() + chrono::TimeDelta::seconds(seconds)
}

/// The frame's "now".
const NOW: i64 = 4000;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn agent_id(name: &str) -> AgentId {
    Uuid::from_u128((2u128 << 64) | u128::from(fnv1a(name.as_bytes())))
}

fn host_id() -> HostId {
    Uuid::from_u128(1)
}

const LEAD: &str = "refactor-tunnels";
const RUNNER: &str = "test-runner";
const SCRIBE: &str = "write-the-docs";
const HUNTER: &str = "flake-hunter";

const CLAUDE_PROTOCOL: &str = "claude_pty_transcript_v1";
const CODEX_PROTOCOL: &str = "codex_sdk_v1";

fn an_agent(name: &str, kind: &str, protocol: &str, parent: Option<&str>) -> Agent {
    Agent {
        id: agent_id(name),
        host_id: host_id(),
        name: Some(name.to_string()),
        command: kind.to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        agent_type: kind.to_string(),
        io_protocols: vec!["terminal_v1".to_string(), protocol.to_string()],
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
        parent: parent.map(|name| AgentParent {
            agent_id: agent_id(name),
            host_id: host_id(),
        }),
        working_on: None,
    }
}

fn a_host() -> HostEntry {
    HostEntry {
        id: host_id(),
        name: "mbp".to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(amux_ui::Capabilities::default()),
        trust_status: amux_ui::HostTrustStatus::Trusted,
        last_dial_error: None,
    }
}

fn agent_up(agent: &Agent) -> Msg {
    Msg::Server(ServerMsg::AgentUpserted {
        agent: agent.clone(),
    })
}

fn opened(name: &str) -> Vec<Msg> {
    vec![
        Msg::Stream {
            agent: agent_id(name),
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: agent_id(name),
            event: StreamMsg::ReplayComplete,
        },
    ]
}

fn batch(name: &str, arrived: i64, rows: Vec<Value>) -> Msg {
    Msg::Stream {
        agent: agent_id(name),
        event: StreamMsg::Batch {
            at: at(arrived),
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(offset, payload)| StreamEntry {
                    seq: 10 + offset as u64,
                    payload,
                })
                .collect(),
        },
    }
}

// --- transcript rows ------------------------------------------------------

fn claude_ready() -> Value {
    json!({"type": "amux.transcript_ready"})
}

fn codex_ready() -> Value {
    json!({"type": "amux.codex_ready"})
}

fn prompt_row(n: u32, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": format!("dddddddd-0000-4000-8000-0000{n:08}"),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": text},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn assistant_row(n: u32, text: &str) -> Value {
    json!({
        "type": "assistant",
        "uuid": format!("dddddddd-0000-4000-8000-0000{n:08}"),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:04.000Z",
        "message": {
            "id": format!("msg_{n}"),
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
        },
    })
}

fn stop_row() -> Value {
    json!({"type": "hook.stop"})
}

// --- the fixture family ---------------------------------------------------

/// A Claude lead with three descendants: a Codex child blocked on a
/// command it wants to run, a Claude child that finished its turn, and a
/// grandchild under the Codex child.
fn family_msgs() -> Vec<Msg> {
    let mut msgs = vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host: a_host() }),
        agent_up(&an_agent(LEAD, "claude", CLAUDE_PROTOCOL, None)),
        agent_up(&an_agent(RUNNER, "codex", CODEX_PROTOCOL, Some(LEAD))),
        agent_up(&an_agent(SCRIBE, "claude", CLAUDE_PROTOCOL, Some(LEAD))),
        agent_up(&an_agent(HUNTER, "codex", CODEX_PROTOCOL, Some(RUNNER))),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ];

    msgs.extend(opened(LEAD));
    msgs.push(batch(
        LEAD,
        NOW - 60,
        vec![
            claude_ready(),
            prompt_row(1, "split the tunnel supervisor"),
            assistant_row(2, "Starting with the reconnect path."),
            stop_row(),
        ],
    ));

    msgs.extend(opened(RUNNER));
    msgs.push(batch(
        RUNNER,
        NOW - 30,
        vec![
            codex_ready(),
            json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
            json!({"type":"item/commandExecution/requestApproval","itemId":"exec-ask","command":"cargo test --workspace","cwd":"/work","reason":"Run the repository test suite?","proposedNetworkPolicyAmendments":[]}),
            json!({"type":"amux.codex_approval_required","request_id":"approval-1","availableDecisions":["accept","decline"]}),
        ],
    ));

    msgs.extend(opened(SCRIBE));
    msgs.push(batch(
        SCRIBE,
        NOW - 300,
        vec![
            claude_ready(),
            prompt_row(3, "document the new handshake"),
            stop_row(),
        ],
    ));

    msgs.extend(opened(HUNTER));
    msgs.push(batch(HUNTER, NOW - 20, vec![codex_ready()]));
    msgs
}

fn fold(msgs: Vec<Msg>) -> Model {
    let mut model = Model::default();
    for msg in msgs {
        update(&mut model, msg);
    }
    let violations = model.check_invariants();
    assert!(violations.is_empty(), "fixture coherent: {violations:?}");
    model
}

fn family_model() -> Model {
    fold(family_msgs())
}

fn chat_on(model: &Model, name: &str) -> ViewState {
    chat_on_id(model, agent_id(name))
}

fn chat_on_id(model: &Model, agent: AgentId) -> ViewState {
    let mut chat = ChatView::open(model, agent, 'a', false).expect("a known protocol");
    chat.reconcile(model);
    ViewState {
        chat: Some(chat),
        ..ViewState::default()
    }
}

// --- rendering ------------------------------------------------------------

fn render_buffer(
    model: &Model,
    view: &ViewState,
    theme: Theme,
    height: u16,
) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (WIDTH, height),
        theme,
        now: at(NOW),
    };
    terminal
        .draw(|frame| render(model, view, &ctx, frame))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        out.push('\n');
    }
    out
}

fn buffer_styles(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell").style();
            out.push(match style.fg {
                Some(Color::Red) => 'r',
                Some(Color::Yellow) => 'y',
                Some(Color::Green) => 'g',
                Some(Color::Cyan) => 'c',
                Some(Color::Blue) => 'B',
                Some(Color::DarkGray) => 'a',
                _ if style.add_modifier.contains(Modifier::BOLD) => 'b',
                _ if style.add_modifier.contains(Modifier::ITALIC) => 'i',
                _ if style.add_modifier.contains(Modifier::DIM) => 'd',
                _ => '.',
            });
        }
        out.push('\n');
    }
    out
}

/// Every surface is locked in both themes, text and styles together — the
/// pairing the Codex goldens established.
fn assert_surface(name: &str, model: &Model, view: &ViewState, height: u16) {
    for (theme_name, theme) in [("dark", Theme::Dark), ("light", Theme::Light)] {
        let buffer = render_buffer(model, view, theme, height);
        let rendered = format!(
            "--- text ---\n{}--- styles ---\n{}",
            buffer_text(&buffer),
            buffer_styles(&buffer)
        );
        let golden = format!("{name}_{theme_name}");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("{golden}.txt"));
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::write(&path, rendered).expect("write golden");
        } else {
            let expected = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("missing golden {golden} — run with UPDATE_GOLDENS=1"));
            assert_eq!(rendered, expected, "frame {golden} diverged");
        }
    }
}

fn header_of(model: &Model, view: &ViewState) -> String {
    buffer_text(&render_buffer(model, view, Theme::Dark, HEIGHT))
        .lines()
        .nth(1)
        .expect("the header row")
        .to_string()
}

fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

/// The leader chord, as two presses.
fn leader(view: &mut ViewState, model: &Model, key: char) -> Option<UiAction> {
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        press(KeyCode::Char('a'), KeyModifiers::CONTROL),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        press(KeyCode::Char(key), KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    )
}

// --- the header marker (U3) ----------------------------------------------

/// A parent's chat says how many agents it started, at any depth — the
/// count is the whole subtree, because that is what is out of sight.
#[test]
fn a2a_header_marker_counts_the_whole_subtree() {
    let model = family_model();
    let header = header_of(&model, &chat_on(&model, LEAD));
    assert!(
        header.contains("⋯ 3 subagents"),
        "the lead's two children and one grandchild: {header}"
    );

    let header = header_of(&model, &chat_on(&model, RUNNER));
    assert!(
        header.contains("⋯ 1 subagent"),
        "one subagent is not `1 subagents`: {header}"
    );
}

/// An agent that started nobody says nothing: the marker is a fact about
/// this agent, not a slot the header always reserves.
#[test]
fn a2a_header_marker_is_absent_without_a_family() {
    let model = family_model();
    let header = header_of(&model, &chat_on(&model, SCRIBE));
    assert!(!header.contains("subagent"), "{header}");
}

/// Both native chats state it, in the same words: the marker is a fact
/// about the fleet, not a Claude idiom a Codex chat imitates.
#[test]
fn a2a_header_marker_reads_the_same_in_both_chats() {
    let model = family_model();
    assert!(header_of(&model, &chat_on(&model, LEAD)).contains("⋯ 3 subagents"));
    assert!(header_of(&model, &chat_on(&model, RUNNER)).contains("⋯ 1 subagent"));
}

#[test]
fn a2a_header_marker_claude_parent() {
    let model = family_model();
    assert_surface(
        "a2a_header_marker_claude",
        &model,
        &chat_on(&model, LEAD),
        HEIGHT,
    );
}

#[test]
fn a2a_header_marker_codex_parent() {
    let model = family_model();
    assert_surface(
        "a2a_header_marker_codex",
        &model,
        &chat_on(&model, RUNNER),
        HEIGHT,
    );
}

/// `<leader> n` walks the family in the order the fleet ranks it and wraps
/// back to the top — one repeated key goes in and comes back out, from a
/// child as readily as from the parent.
#[test]
fn a2a_header_marker_key_cycles_into_the_family_and_back() {
    let model = family_model();
    let order: Vec<AgentId> = std::iter::once(agent_id(LEAD))
        .chain(
            model
                .family_of(agent_id(LEAD))
                .into_iter()
                .map(|member| member.card.agent.id),
        )
        .collect();
    assert_eq!(order.len(), 4, "the lead and its three descendants");

    let mut view = chat_on(&model, LEAD);
    let mut walked = Vec::new();
    for _ in 0..order.len() {
        let Some(UiAction::OpenChat(next)) = leader(&mut view, &model, 'n') else {
            panic!("the chord names the next agent in the family");
        };
        walked.push(next);
        view = chat_on_id(&model, next);
    }
    assert_eq!(
        walked,
        order
            .iter()
            .skip(1)
            .chain(std::iter::once(&order[0]))
            .copied()
            .collect::<Vec<_>>(),
        "into the children in family order, then back to the top"
    );
}

/// An agent in no family has nowhere to cycle to, and the chord says so by
/// doing nothing — it never dumps the human back onto the fleet.
#[test]
fn a2a_header_marker_key_is_inert_outside_a_family() {
    let mut msgs = vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host: a_host() }),
        agent_up(&an_agent(LEAD, "claude", CLAUDE_PROTOCOL, None)),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ];
    msgs.extend(opened(LEAD));
    msgs.push(batch(LEAD, NOW - 60, vec![claude_ready()]));
    let model = fold(msgs);

    let mut view = chat_on(&model, LEAD);
    assert!(leader(&mut view, &model, 'n').is_none());
    assert!(view.chat.is_some(), "the chat stays open");
}

// --- the child-ask banner (U1) -------------------------------------------

/// The banner's row is the one under the header — read it there rather
/// than by searching for the glyph, which an ask panel also wears.
fn banner_of(model: &Model, view: &ViewState) -> Option<String> {
    let frame = buffer_text(&render_buffer(model, view, Theme::Dark, HEIGHT));
    let row = frame.lines().nth(2)?.trim_matches(['│', ' ']).to_string();
    row.starts_with('⚠').then_some(row)
}

/// The parent's chat says which child is waiting and for what — the act
/// itself, in the child's own layer's words, not a generic "needs you".
#[test]
fn a2a_banner_names_the_child_and_the_act_it_is_blocked_on() {
    let model = family_model();
    let banner = banner_of(&model, &chat_on(&model, LEAD)).expect("a child is asking");
    assert!(
        banner.contains("test-runner needs permission: cargo test --workspace"),
        "{banner}"
    );
}

/// A grandchild's ask reaches the top of the family: the banner is about
/// everyone below this agent, not just the agents it started itself.
#[test]
fn a2a_banner_carries_a_grandchild_up_the_family() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        HUNTER,
        NOW - 10,
        vec![
            json!({"type":"turn/started","turn":{"id":"turn-h","status":"inProgress"}}),
            json!({"type":"item/commandExecution/requestApproval","itemId":"exec-h","command":"rm -rf target","cwd":"/work","proposedNetworkPolicyAmendments":[]}),
            json!({"type":"amux.codex_approval_required","request_id":"approval-h","availableDecisions":["accept","decline"]}),
        ],
    ));
    let model = fold(msgs);

    let from_the_middle = banner_of(&model, &chat_on(&model, RUNNER)).expect("its own child asks");
    assert!(
        from_the_middle.contains("flake-hunter"),
        "{from_the_middle}"
    );

    let from_the_top = banner_of(&model, &chat_on(&model, LEAD)).expect("three are asking");
    assert!(
        from_the_top.starts_with("⚠ test-runner needs permission")
            && from_the_top.ends_with("· +2 more"),
        "the loudest is named and the rest are counted: {from_the_top}"
    );
}

/// The banner is a derivation, not a record: answering the ask in the
/// child's own chat empties it on the next frame, with nothing to clear
/// in the parent.
#[test]
fn a2a_banner_clears_by_re_derivation_when_the_ask_is_answered() {
    let mut msgs = family_msgs();
    let model = fold(msgs.clone());
    assert!(banner_of(&model, &chat_on(&model, LEAD)).is_some());

    msgs.push(batch(
        RUNNER,
        NOW - 5,
        vec![json!({
            "type": "amux.codex_approval_resolved",
            "request_id": "approval-1",
            "reason": "answered",
        })],
    ));
    let answered = fold(msgs);
    let after = banner_of(&answered, &chat_on(&answered, LEAD)).expect("a finished child remains");
    assert!(
        !after.contains("test-runner"),
        "the answered ask left the parent's chat with nothing to clear: {after}"
    );
    assert!(
        after.contains("write-the-docs finished") && !after.contains("more"),
        "what is left is exactly what is still true: {after}"
    );
}

/// With nobody below waiting, there is no row at all — the banner is not
/// a slot the chat reserves.
#[test]
fn a2a_banner_is_absent_when_no_child_is_waiting() {
    let mut msgs = vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host: a_host() }),
        agent_up(&an_agent(LEAD, "claude", CLAUDE_PROTOCOL, None)),
        agent_up(&an_agent(HUNTER, "codex", CODEX_PROTOCOL, Some(LEAD))),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ];
    msgs.extend(opened(LEAD));
    msgs.push(batch(LEAD, NOW - 60, vec![claude_ready()]));
    msgs.extend(opened(HUNTER));
    msgs.push(batch(HUNTER, NOW - 20, vec![codex_ready()]));
    let model = fold(msgs);
    assert_eq!(banner_of(&model, &chat_on(&model, LEAD)), None);
}

/// A parent's own ask is its own business: the banner reports the family
/// below, and the chat is already showing what this agent is waiting on.
#[test]
fn a2a_banner_is_silent_about_the_agent_whose_chat_it_is() {
    let model = family_model();
    assert_eq!(
        banner_of(&model, &chat_on(&model, RUNNER)),
        None,
        "the runner's own approval panel is the surface for its own ask"
    );
}

/// The banner costs the feed a row rather than floating over it: the
/// frame is the same height, and nothing it was showing is covered.
#[test]
fn a2a_banner_takes_a_row_from_the_feed_and_covers_nothing() {
    let model = family_model();
    let view = chat_on(&model, LEAD);
    let frame = buffer_text(&render_buffer(&model, &view, Theme::Dark, HEIGHT));
    assert_eq!(frame.lines().count(), HEIGHT as usize);
    assert!(
        frame.contains("split the tunnel supervisor"),
        "the conversation below it is intact:\n{frame}"
    );
}

#[test]
fn a2a_banner_in_a_claude_parents_chat() {
    let model = family_model();
    assert_surface("a2a_banner_claude", &model, &chat_on(&model, LEAD), HEIGHT);
}

/// The same banner in a Codex parent's chat: the words come from the
/// child's layer, the row from the parent's.
#[test]
fn a2a_banner_in_a_codex_parents_chat() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        SCRIBE,
        NOW - 15,
        vec![json!({
            "type": "hook.permission_request",
            "tool_name": "Bash",
            "tool_input": {"command": "git push --force origin a2a"},
        })],
    ));
    // Re-parent the scribe under the Codex runner so a Codex chat is the
    // one reporting a Claude child's ask.
    msgs.push(agent_up(&an_agent(
        SCRIBE,
        "claude",
        CLAUDE_PROTOCOL,
        Some(RUNNER),
    )));
    let model = fold(msgs);
    let banner = banner_of(&model, &chat_on(&model, RUNNER)).expect("its child is asking");
    assert!(
        banner.contains("write-the-docs needs permission: git push --force origin a2a"),
        "{banner}"
    );
    assert_surface("a2a_banner_codex", &model, &chat_on(&model, RUNNER), HEIGHT);
}
