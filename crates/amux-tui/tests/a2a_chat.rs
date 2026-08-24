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

/// A second host, known by name, so a message from another machine can
/// be told from one nobody here can place.
fn another_host() -> HostEntry {
    HostEntry {
        id: Uuid::from_u128(2),
        name: "tessin".to_string(),
        ..a_host()
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

/// The generic `<amux …>` tag the paste carrier delivers, formatted exactly
/// as the daemon's formatter writes it. Spelled out rather than imported:
/// this crate consumes `amux-ui` only, and the agreement between the two
/// spellings is asserted in the reducer specs, not here.
fn amux_tag(kind: &str, from: &str, host: &str, text: &str) -> String {
    format!(
        "<amux id=\"00000000-0000-4000-8000-0000000000a1\" kind=\"{kind}\" \
from=\"{from}/{host}\" \
from-id=\"00000000-0000-0000-0000-0000000000b0\" from-kind=\"codex\">\n{text}\n</amux>"
    )
}

/// The host every fixture agent lives on, as the wire spells it.
const HERE: &str = "00000000-0000-0000-0000-000000000001";
/// A second host this inventory knows by name.
const THERE: &str = "00000000-0000-0000-0000-000000000002";

/// An envelope arriving in a Claude transcript: an ordinary user row whose
/// text is the tag.
fn claude_message_row(n: u32, kind: &str, from: &str, text: &str) -> Value {
    claude_message_row_from(n, kind, from, HERE, text)
}

fn claude_message_row_from(n: u32, kind: &str, from: &str, host: &str, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": format!("dddddddd-0000-4000-8000-0000{n:08}"),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:01:00.000Z",
        "isMeta": false,
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": amux_tag(kind, from, host, text)},
    })
}

/// The row the daemon writes into a Codex thread for the same envelope —
/// the native thread shows nothing for an injected item.
fn codex_message_row(kind: &str, from: &str, text: &str) -> Value {
    codex_message_row_from(kind, from, HERE, text)
}

fn codex_message_row_from(kind: &str, from: &str, host: &str, text: &str) -> Value {
    json!({
        "type": "amux.codex_message",
        "id": "00000000-0000-0000-0000-0000000000a1",
        "kind": kind,
        "from": format!("{from}/{host}"),
        "from_id": "00000000-0000-0000-0000-0000000000b0",
        "text": text,
        "delivery": "inject_queued",
    })
}

/// An outbound `send`, as Claude records the MCP tool call.
fn claude_send_rows(n: u32, to: &str, text: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "assistant",
            "uuid": format!("dddddddd-0000-4000-8000-0000{n:08}"),
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-11T22:02:00.000Z",
            "message": {
                "id": format!("msg_{n}"),
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_send1",
                    "name": "mcp__amux__send",
                    "input": {"to": to, "text": text},
                }],
                "stop_reason": "tool_use",
            },
        }),
        json!({
            "type": "user",
            "uuid": format!("dddddddd-0000-4000-8000-0000{:08}", n + 1),
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-11T22:02:01.000Z",
            "isMeta": false,
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_send1",
                    "content": "{\"id\":\"00000000-0000-4000-8000-0000000000a2\"}",
                }],
            },
        }),
    ]
}

/// An outbound `send` as Codex records it: an MCP call against the server
/// amux owns for the thread. Both the server and registered tool name are
/// needed to distinguish amux's work from somebody else's.
fn codex_send_row(to: &str, text: &str) -> Value {
    json!({
        "type": "item/completed",
        "item": {
            "id": "mcp-send",
            "type": "mcpToolCall",
            // This consumer does not link the daemon crate; the reducer specs
            // assert this wire spelling against the shared server constant.
            "server": "amux",
            "tool": "send",
            "arguments": {"to": to, "text": text},
            "status": "completed",
        },
    })
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
        Msg::Server(ServerMsg::HostUpserted {
            host: another_host(),
        }),
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
            && from_the_top.contains("· +2 more"),
        "the loudest is named and the rest are counted: {from_the_top}"
    );
    assert!(
        from_the_top.ends_with("· C-a a answer"),
        "and the row ends with the way to answer it (U2): {from_the_top}"
    );
}

/// The one named need is the loudest one, not the nearest. Here the
/// finished child is the first the family tree reaches and the blocked
/// grandchild hides two generations down behind a working branch, so a
/// banner that took whatever came first would spend its single row on the
/// agent that can wait and withhold the chord from the one that cannot.
fn buried_permission_model() -> Model {
    let mut msgs = family_msgs();
    // The nearer ask is answered, leaving the finished sibling in front of
    // the working branch the grandchild sits under.
    msgs.push(batch(
        RUNNER,
        NOW - 12,
        vec![json!({
            "type": "amux.codex_approval_resolved",
            "request_id": "approval-1",
            "reason": "answered",
        })],
    ));
    msgs.push(batch(
        HUNTER,
        NOW - 10,
        vec![
            json!({"type":"turn/started","turn":{"id":"turn-h","status":"inProgress"}}),
            json!({"type":"item/commandExecution/requestApproval","itemId":"exec-h","command":"rm -rf target","cwd":"/work","proposedNetworkPolicyAmendments":[]}),
            json!({"type":"amux.codex_approval_required","request_id":"approval-h","availableDecisions":["accept","decline"]}),
        ],
    ));
    fold(msgs)
}

#[test]
fn a2a_banner_names_the_loudest_need_not_the_nearest() {
    let model = buried_permission_model();
    let banner = banner_of(&model, &chat_on(&model, LEAD)).expect("two are asking");
    assert!(
        banner.starts_with("⚠ flake-hunter needs permission: rm -rf target"),
        "the blocked grandchild takes the row from the finished child: {banner}"
    );
    assert!(
        banner.contains("· +1 more"),
        "and the quieter need is counted, not named: {banner}"
    );
    assert!(
        banner.ends_with("· C-a a answer"),
        "the chord follows the need it can answer: {banner}"
    );
}

/// The panel the chord docks is the same need the banner named — one
/// derivation feeds both, so the row never advertises an answer for an
/// agent other than the one that appears.
#[test]
fn a2a_banner_and_the_docked_panel_name_the_same_agent() {
    let model = buried_permission_model();
    let frame = frame_of(&model, &docked(&model, LEAD));
    assert!(
        frame.contains("answering flake-hunter"),
        "the docked ask belongs to the agent the banner named:\n{frame}"
    );
}

#[test]
fn a2a_banner_buried_permission() {
    let model = buried_permission_model();
    assert_surface(
        "a2a_banner_buried_permission",
        &model,
        &chat_on(&model, LEAD),
        HEIGHT,
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

// --- message rows (U4) ----------------------------------------------------

/// A child's last message: long enough that closing it hides something.
const REPORT: &str = "migrated 14 call sites\n\nthe two in `legacy/` need a decision:\nthey pass the old shape through a macro.";

/// The conversation an agent has with the fleet: one inbound message, one
/// completion from a child, one exit, and one send going the other way.
fn conversation_msgs() -> Vec<Msg> {
    let mut msgs = family_msgs();
    let mut rows = vec![
        claude_message_row(
            10,
            "message",
            "test-runner",
            "the suite is green on the retry path",
        ),
        claude_message_row(11, "completed", "write-the-docs", REPORT),
        claude_message_row(12, "exited", "flake-hunter", ""),
    ];
    rows.extend(claude_send_rows(
        13,
        "test-runner",
        "rerun with --nocapture",
    ));
    msgs.push(batch(LEAD, NOW - 10, rows));

    msgs.push(batch(
        RUNNER,
        NOW - 8,
        vec![
            codex_message_row("message", "refactor-tunnels", "rerun with --nocapture"),
            codex_message_row("completed", "flake-hunter", REPORT),
            codex_send_row("refactor-tunnels", "green, 0 flakes in 20 runs"),
        ],
    ));
    msgs
}

fn conversation_model() -> Model {
    fold(conversation_msgs())
}

fn frame_of(model: &Model, view: &ViewState) -> String {
    buffer_text(&render_buffer(model, view, Theme::Dark, 40))
}

fn opened_reports(model: &Model, name: &str) -> ViewState {
    let mut view = chat_on(model, name);
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        press(KeyCode::Char('a'), KeyModifiers::CONTROL),
        (WIDTH, 40),
        at(NOW),
    );
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        press(KeyCode::Char('m'), KeyModifiers::NONE),
        (WIDTH, 40),
        at(NOW),
    );
    view
}

/// Inbound: the sender, then everything they said. Somebody is talking to
/// this agent, so nothing is held back.
#[test]
fn a2a_message_rows_show_an_inbound_message_whole() {
    let model = conversation_model();
    for name in [LEAD, RUNNER] {
        let frame = frame_of(&model, &chat_on(&model, name));
        assert!(
            frame.contains("← "),
            "one directional glyph marks it inbound:\n{frame}"
        );
        assert!(
            frame.contains("rerun with --nocapture") || frame.contains("the suite is green"),
            "the body renders in full:\n{frame}"
        );
    }
}

/// A completion wears a finished mark over a closed body: the first line,
/// then what is behind the fold and the chord that opens it.
#[test]
fn a2a_message_rows_close_a_completion_and_say_what_is_behind_it() {
    let model = conversation_model();
    for name in [LEAD, RUNNER] {
        let frame = frame_of(&model, &chat_on(&model, name));
        assert!(frame.contains("✔ "), "the finished mark:\n{frame}");
        assert!(frame.contains("migrated 14 call sites"), "{frame}");
        assert!(
            !frame.contains("through a macro"),
            "the rest is behind the fold:\n{frame}"
        );
        assert!(
            frame.contains("⌄ 2 more lines · C-a m"),
            "what is hidden, and the key that shows it:\n{frame}"
        );
    }
}

/// The chord opens every completion in the chat and offers to close it
/// again — one display state, not a per-row cursor the feed does not have.
#[test]
fn a2a_message_rows_open_every_completion_on_the_chord() {
    let model = conversation_model();
    for name in [LEAD, RUNNER] {
        let frame = frame_of(&model, &opened_reports(&model, name));
        assert!(
            frame.contains("through a macro"),
            "the whole report:\n{frame}"
        );
        assert!(frame.contains("⌃ close · C-a m"), "{frame}");
    }
}

/// An exit offers nothing to open, because the envelope carries nothing:
/// a fold marker over an empty body would be a promise about what is
/// behind it.
#[test]
fn a2a_message_rows_render_an_exit_as_a_bare_notice() {
    let model = conversation_model();
    let frame = frame_of(&model, &chat_on(&model, LEAD));
    let notice = frame
        .lines()
        .find(|line| line.contains("· flake-hunter"))
        .expect("the exit row");
    assert!(!notice.contains('⌄'), "nothing to open: {notice}");
}

/// Outbound: an ordinary tool row saying who it went to and what left —
/// the other half of the conversation, in both chats, in the same shape.
#[test]
fn a2a_message_rows_show_a_send_as_its_target_and_a_summary() {
    let model = conversation_model();
    let claude = frame_of(&model, &chat_on(&model, LEAD));
    assert!(
        claude.contains("→ test-runner · rerun with --nocapture"),
        "{claude}"
    );
    let codex = frame_of(&model, &chat_on(&model, RUNNER));
    assert!(
        codex.contains("→ refactor-tunnels · green, 0 flakes in 20 runs"),
        "{codex}"
    );
    assert!(
        !codex.contains("\"to\":"),
        "a send is talk, not a JSON argument dump:\n{codex}"
    );
}

/// Dynamic calls stay generic even when they borrow the name of an amux
/// tool: amux registers none, so the name alone cannot claim the work.
#[test]
fn a2a_message_rows_leave_the_other_amux_tools_generic() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        RUNNER,
        NOW - 8,
        vec![json!({
            "type": "item/completed",
            "item": {
                "id": "dynamic-spawn",
                "type": "dynamicToolCall",
                "tool": "spawn",
                "arguments": {"kind": "codex", "prompt": "hunt the flake"},
                "success": true,
                "status": "completed",
            },
        })],
    ));
    let model = fold(msgs);
    let frame = frame_of(&model, &chat_on(&model, RUNNER));
    assert!(frame.contains("tool spawn · done"), "{frame}");
    assert!(!frame.contains("→ "), "no direction to claim:\n{frame}");
}

#[test]
fn a2a_message_rows_in_a_claude_chat() {
    let model = conversation_model();
    assert_surface(
        "a2a_message_rows_claude",
        &model,
        &chat_on(&model, LEAD),
        40,
    );
}

#[test]
fn a2a_message_rows_in_a_codex_chat() {
    let model = conversation_model();
    assert_surface(
        "a2a_message_rows_codex",
        &model,
        &chat_on(&model, RUNNER),
        40,
    );
}

#[test]
fn a2a_message_rows_opened_in_a_claude_chat() {
    let model = conversation_model();
    assert_surface(
        "a2a_message_rows_claude_open",
        &model,
        &opened_reports(&model, LEAD),
        40,
    );
}

/// The sender marker is for a person to read: the name alone when the
/// message came from this agent's own machine, the host named when it
/// came from another one this inventory knows, and the address exactly as
/// it arrived when nobody here can place the host.
#[test]
fn a2a_message_rows_name_the_sender_and_only_a_foreign_host() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        LEAD,
        NOW - 10,
        vec![
            claude_message_row_from(20, "message", "test-runner", HERE, "same machine"),
            claude_message_row_from(21, "message", "far-scout", THERE, "another machine"),
            claude_message_row_from(
                22,
                "message",
                "ghost",
                "00000000-0000-0000-0000-0000000000ff",
                "a host nobody here knows",
            ),
        ],
    ));
    let model = fold(msgs);
    let frame = frame_of(&model, &chat_on(&model, LEAD));
    assert!(
        frame.contains("← test-runner\n") || frame.contains("← test-runner "),
        "its own host adds nothing:\n{frame}"
    );
    assert!(frame.contains("← far-scout @ tessin"), "{frame}");
    assert!(
        frame.contains("ghost/00000000-0000-0000-0000-0000000000ff"),
        "an address nobody can resolve is still where it came from:\n{frame}"
    );
}

// --- inline answering (U2) ------------------------------------------------

/// A Claude child that wants to run something, so a Codex parent has a
/// Claude ask to host and the pair is covered both ways round.
fn claude_child_asking() -> Model {
    let mut msgs = family_msgs();
    msgs.push(batch(SCRIBE, NOW - 15, vec![answerable_permission_row()]));
    // Re-parent the scribe under the Codex runner so a Codex chat is the
    // one hosting a Claude child's panel.
    msgs.push(agent_up(&an_agent(
        SCRIBE,
        "claude",
        CLAUDE_PROTOCOL,
        Some(RUNNER),
    )));
    // …and answer the runner's own approval, so its bottom block is free
    // to host a guest.
    msgs.push(batch(
        RUNNER,
        NOW - 12,
        vec![json!({
            "type": "amux.codex_approval_resolved",
            "request_id": "approval-1",
            "reason": "answered",
        })],
    ));
    fold(msgs)
}

/// A Claude permission ask in the verified one-suggestion shape, so the
/// panel offers its three actions rather than the C2 refusal.
fn answerable_permission_row() -> Value {
    json!({
        "type": "hook.permission_request",
        "tool_name": "Bash",
        "tool_input": {"command": "git push --force origin a2a"},
        "permission_mode": "default",
        "permission_suggestions": [{
            "type": "addDirectories",
            "destination": "session",
            "directories": ["/work"],
        }],
    })
}

/// Dock the ask the banner names.
fn docked(model: &Model, name: &str) -> ViewState {
    let mut view = chat_on(model, name);
    leader(&mut view, model, 'a');
    view
}

/// The panel in the parent's chat is the child's own, drawn by the child's
/// layer: the Codex approval title, the command it is blocked on, and the
/// decisions that layer offers — none of which the parent's chat knows
/// how to write.
#[test]
fn a2a_inline_answer_hosts_the_childs_own_panel() {
    let model = family_model();
    let frame = frame_of(&model, &docked(&model, LEAD));
    assert!(
        frame.contains("answering test-runner"),
        "the boundary says whose ask this is:\n{frame}"
    );
    assert!(
        frame.contains("approval — command") && frame.contains("cargo test --workspace"),
        "the Codex layer's own panel:\n{frame}"
    );
    assert!(
        frame.contains("esc back"),
        "and the way out of it:\n{frame}"
    );
}

/// The parent's own composer is what the guest replaces — one panel, one
/// cursor, and no second place to type.
#[test]
fn a2a_inline_answer_replaces_the_parents_composer() {
    let model = family_model();
    let before = frame_of(&model, &chat_on(&model, LEAD));
    assert!(
        before.contains("enter send"),
        "the composer footer:\n{before}"
    );
    let after = frame_of(&model, &docked(&model, LEAD));
    assert!(
        !after.contains("enter send"),
        "the composer is covered while a guest is docked:\n{after}"
    );
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "and the frame is the same height"
    );
}

/// Confirming dispatches the CHILD's own command, addressed to the child.
/// Nothing about the act says it happened in somebody else's chat.
#[test]
fn a2a_inline_answer_dispatches_the_childs_own_command() {
    let model = family_model();
    let mut view = docked(&model, LEAD);
    let chat = view.chat.as_mut().expect("an open chat");
    let action = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Enter, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    match action {
        Some(UiAction::Dispatch(amux_ui::Command::Codex(amux_ui::CodexCommand::Answer {
            agent,
            ..
        }))) => assert_eq!(agent, agent_id(RUNNER), "the child is the addressee"),
        other => panic!("expected the child's own Answer, got {other:?}"),
    }
}

/// The same act from the child's own chat produces the identical command
/// — there is one answer path, and the parent's chat is a second place to
/// reach it rather than a second way to do it.
#[test]
fn a2a_inline_answer_matches_answering_from_the_childs_own_view() {
    let model = family_model();

    let mut inline = docked(&model, LEAD);
    let chat = inline.chat.as_mut().expect("an open chat");
    let from_the_parent = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Enter, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );

    let mut own = chat_on(&model, RUNNER);
    let chat = own.chat.as_mut().expect("an open chat");
    let from_the_child = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Enter, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );

    assert_eq!(from_the_parent, from_the_child);
}

/// Answering from the child's own view is untouched by any of this: the
/// child's frame is the same whether or not its parent has its ask
/// docked, because nothing was moved out of the child's layer.
#[test]
fn a2a_inline_answer_leaves_the_childs_own_view_unchanged() {
    let model = family_model();
    let alone = frame_of(&model, &chat_on(&model, RUNNER));
    let _hosted = docked(&model, LEAD);
    let while_hosted = frame_of(&model, &chat_on(&model, RUNNER));
    assert_eq!(alone, while_hosted);
    assert!(
        while_hosted.contains("approval — command"),
        "the child still shows its own ask:\n{while_hosted}"
    );
}

/// A Codex parent hosting a Claude child: the panel is the Claude layer's
/// — its permission actions, in its words — and confirming dispatches the
/// Claude command for the Claude child.
#[test]
fn a2a_inline_answer_works_the_other_way_round() {
    let model = claude_child_asking();
    let frame = frame_of(&model, &docked(&model, RUNNER));
    assert!(
        frame.contains("answering write-the-docs"),
        "the boundary names the Claude child:\n{frame}"
    );
    assert!(
        frame.contains("Allow once") && frame.contains("git push --force origin a2a"),
        "the Claude layer's own panel:\n{frame}"
    );

    let mut view = docked(&model, RUNNER);
    let chat = view.chat.as_mut().expect("an open chat");
    let action = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Enter, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    match action {
        Some(UiAction::Dispatch(amux_ui::Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
            agent,
            ..
        }))) => assert_eq!(agent, agent_id(SCRIBE), "the child is the addressee"),
        other => panic!("expected the child's own AnswerAsk, got {other:?}"),
    }
}

/// The panel is a derivation like the banner above it: an ask answered in
/// the child's own chat, or on another device, takes it away on the next
/// fold with nothing to dismiss.
#[test]
fn a2a_inline_answer_clears_when_the_ask_is_answered_anywhere() {
    let mut msgs = family_msgs();
    let model = fold(msgs.clone());
    let mut view = docked(&model, LEAD);
    assert!(frame_of(&model, &view).contains("answering test-runner"));

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
    view.chat
        .as_mut()
        .expect("an open chat")
        .reconcile(&answered);
    let frame = frame_of(&answered, &view);
    assert!(
        !frame.contains("answering test-runner"),
        "the guest left with the ask:\n{frame}"
    );
    assert!(
        frame.contains("enter send"),
        "and the parent's composer came back:\n{frame}"
    );
}

/// Esc sends the guest back. Answering a child is something the human
/// opted into; leaving it is one key, and the parent's own chat returns
/// exactly as it was.
#[test]
fn a2a_inline_answer_esc_returns_the_parents_composer() {
    let model = family_model();
    let before = frame_of(&model, &chat_on(&model, LEAD));
    let mut view = docked(&model, LEAD);
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Esc, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    assert_eq!(frame_of(&model, &view), before);
}

/// An agent's own obligations come before its children's: while this chat
/// has an ask of its own, the bottom block is taken, the banner withholds
/// the chord, and pressing it does nothing.
#[test]
fn a2a_inline_answer_yields_to_the_parents_own_ask() {
    let mut msgs = family_msgs();
    msgs.push(batch(SCRIBE, NOW - 15, vec![answerable_permission_row()]));
    msgs.push(agent_up(&an_agent(
        SCRIBE,
        "claude",
        CLAUDE_PROTOCOL,
        Some(RUNNER),
    )));
    // The runner keeps its own pending approval from the fixture.
    let model = fold(msgs);

    let banner = banner_of(&model, &chat_on(&model, RUNNER)).expect("its child is asking");
    assert!(
        !banner.contains("answer"),
        "the chord is not advertised where it would do nothing: {banner}"
    );
    let frame = frame_of(&model, &docked(&model, RUNNER));
    assert!(
        !frame.contains("answering write-the-docs"),
        "and pressing it changes nothing:\n{frame}"
    );
    assert!(
        frame.contains("approval — command"),
        "this agent's own ask still holds the panel:\n{frame}"
    );
}

/// A child that needs a person but has nothing to answer — one that
/// finished — gets no chord either: the hint tracks what the key would
/// actually do (P10).
#[test]
fn a2a_inline_answer_is_not_offered_for_a_child_with_nothing_to_answer() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        RUNNER,
        NOW - 5,
        vec![json!({
            "type": "amux.codex_approval_resolved",
            "request_id": "approval-1",
            "reason": "answered",
        })],
    ));
    let model = fold(msgs);
    let banner = banner_of(&model, &chat_on(&model, LEAD)).expect("a finished child remains");
    assert!(banner.contains("write-the-docs finished"), "{banner}");
    assert!(
        !banner.contains("answer"),
        "nothing to answer, nothing to offer: {banner}"
    );
}

/// Ctrl+X interrupts the agent whose ask is on screen — the child while
/// its panel is docked here, the parent again once it is dismissed.
#[test]
fn a2a_inline_answer_points_the_interrupt_at_the_agent_on_screen() {
    let model = family_model();
    let mut view = docked(&model, LEAD);
    let chat = view.chat.as_mut().expect("an open chat");
    let while_docked = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Char('x'), KeyModifiers::CONTROL),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    assert_eq!(
        while_docked,
        Some(UiAction::Dispatch(amux_ui::Command::Codex(
            amux_ui::CodexCommand::Interrupt {
                agent: agent_id(RUNNER)
            }
        )))
    );

    let mut view = chat_on(&model, LEAD);
    let chat = view.chat.as_mut().expect("an open chat");
    let plain = amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Char('x'), KeyModifiers::CONTROL),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    assert_eq!(
        plain,
        Some(UiAction::Dispatch(amux_ui::Command::Claude(
            amux_ui::ClaudeCommand::Interrupt {
                agent: agent_id(LEAD)
            }
        )))
    );
}

/// Typing behind a docked guest never reaches the parent's draft: the
/// panel has no open field here, so the paste is dropped rather than
/// landing invisibly in a message to the wrong agent.
#[test]
fn a2a_inline_answer_keeps_typing_out_of_the_parents_draft() {
    let model = family_model();
    let mut view = docked(&model, LEAD);
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_paste(chat, &model, "not for the lead");
    let chat = view.chat.as_mut().expect("an open chat");
    amux_tui::chat::handle_chat_key(
        chat,
        &model,
        press(KeyCode::Esc, KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    let frame = frame_of(&model, &view);
    assert!(
        !frame.contains("not for the lead"),
        "the parent's composer is untouched:\n{frame}"
    );
}

#[test]
fn a2a_inline_answer_in_a_claude_parents_chat() {
    let model = family_model();
    assert_surface(
        "a2a_inline_answer_claude_parent",
        &model,
        &docked(&model, LEAD),
        HEIGHT,
    );
}

#[test]
fn a2a_inline_answer_in_a_codex_parents_chat() {
    let model = claude_child_asking();
    assert_surface(
        "a2a_inline_answer_codex_parent",
        &model,
        &docked(&model, RUNNER),
        HEIGHT,
    );
}

// --- the family keys, listed only where they work -------------------------

fn help_overlay(model: &Model, name: &str) -> String {
    let mut view = chat_on(model, name);
    view.chat.as_mut().expect("an open chat").set_help(true);
    buffer_text(&render_buffer(model, &view, Theme::Dark, 46))
}

/// A parent with children below it, a completion to open and a child
/// waiting: all three family chords are in its overlay.
#[test]
fn a2a_bindings_list_every_live_family_chord() {
    let overlay = help_overlay(&conversation_model(), LEAD);
    for action in [
        "next agent in this family",
        "open / close completions",
        "answer the waiting child here",
    ] {
        assert!(overlay.contains(action), "{action}:\n{overlay}");
    }
}

/// A Codex chat has the same chords and now says so: its overlay used to
/// name none of them, which taught the human that its chat had no family
/// keys at all.
#[test]
fn a2a_bindings_reach_the_codex_overlay_too() {
    let overlay = help_overlay(&claude_child_asking(), RUNNER);
    assert!(overlay.contains("codex chat"), "{overlay}");
    for action in ["next agent in this family", "answer the waiting child here"] {
        assert!(overlay.contains(action), "{action}:\n{overlay}");
    }
}

/// An agent alone: no family to cycle, no completion to open, nobody to
/// answer — and no rows saying otherwise.
#[test]
fn a2a_bindings_omit_the_family_chords_from_a_solitary_chat() {
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
    let overlay = help_overlay(&fold(msgs), LEAD);
    for action in [
        "next agent in this family",
        "open / close completions",
        "answer the waiting child here",
    ] {
        assert!(!overlay.contains(action), "{action} is inert:\n{overlay}");
    }
    assert!(
        overlay.contains("back to fleet"),
        "the chords that always work are still there:\n{overlay}"
    );
}

/// The answer chord tracks the same fact the banner does: a child that
/// finished wants a person, not an answer, so neither offers the key.
#[test]
fn a2a_bindings_offer_the_answer_chord_only_with_an_ask_to_dock() {
    let mut msgs = family_msgs();
    msgs.push(batch(
        RUNNER,
        NOW - 5,
        vec![json!({
            "type": "amux.codex_approval_resolved",
            "request_id": "approval-1",
            "reason": "answered",
        })],
    ));
    let model = fold(msgs);
    let overlay = help_overlay(&model, LEAD);
    assert!(
        overlay.contains("next agent in this family"),
        "the family is still there:\n{overlay}"
    );
    assert!(
        !overlay.contains("answer the waiting child here"),
        "but there is nothing left to answer:\n{overlay}"
    );
}

/// A parent whose own ask holds the bottom block has nowhere to host a
/// guest, so the chord is out of the overlay for the same reason it is
/// out of the banner.
#[test]
fn a2a_bindings_withhold_the_answer_chord_behind_the_parents_own_ask() {
    let mut msgs = family_msgs();
    msgs.push(batch(SCRIBE, NOW - 15, vec![answerable_permission_row()]));
    msgs.push(agent_up(&an_agent(
        SCRIBE,
        "claude",
        CLAUDE_PROTOCOL,
        Some(RUNNER),
    )));
    let model = fold(msgs);
    let overlay = help_overlay(&model, RUNNER);
    assert!(
        !overlay.contains("answer the waiting child here"),
        "the runner's own approval owns the bottom block:\n{overlay}"
    );
}

/// A viewport too small to draw is still a viewport a key press arrives
/// at, and the scroll keys ask the chat's layout how tall its bottom block
/// is before anything decides the frame is too small to draw. The docked
/// guest's rule is measured on that path, so its width arithmetic has to
/// survive widths no frame would ever use — a measurement that panics
/// takes the whole client down over a terminal somebody dragged shut.
#[test]
fn a2a_inline_answer_survives_a_viewport_too_small_to_draw() {
    for (parent, model) in [(LEAD, family_model()), (RUNNER, claude_child_asking())] {
        for width in [0u16, 1, 2, 3, 12] {
            let mut view = docked(&model, parent);
            let chat = view.chat.as_mut().expect("an open chat");
            amux_tui::chat::handle_chat_key(
                chat,
                &model,
                press(KeyCode::PageUp, KeyModifiers::NONE),
                (width, 0),
                at(NOW),
            );
        }
    }
}
