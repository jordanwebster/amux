//! The chat of a Claude agent driven through the SDK. This build folds no
//! `claude_sdk_v1` rows, so the frame it owes a person is a placeholder that
//! names the gap — locked here in both themes, and proven inert to input.

use amux_tui::view::{UiAction, ViewState};
use amux_tui::{ChatView, ColorMode, FrameContext, Theme, render};
use amux_ui::{
    Agent, AgentId, HostEntry, HostId, Model, Msg, ServerMsg, StreamEntry, StreamMsg, update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use uuid::Uuid;

const NOW: &str = "2026-08-12T09:12:30Z";
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn host_id() -> HostId {
    Uuid::from_u128(1)
}

fn agent_id() -> AgentId {
    Uuid::from_u128(9)
}

fn child_id() -> AgentId {
    Uuid::from_u128(10)
}

fn an_sdk_agent() -> Agent {
    Agent {
        id: agent_id(),
        host_id: host_id(),
        name: Some("sdk-writer".to_string()),
        command: "claude".to_string(),
        working_dir: "/work/amux".into(),
        kind: amux_ui::AgentKind::Claude {
            driver: amux_ui::ClaudeDriver::Sdk,
        },
        readonly: false,
        args: Vec::new(),
        created_at: at("2026-08-12T09:00:00Z"),
        parent: None,
        working_on: None,
    }
}

fn base() -> Vec<Msg> {
    let host = HostEntry {
        id: host_id(),
        name: "mbp".to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(amux_ui::Capabilities::default()),
        trust_status: amux_ui::HostTrustStatus::Trusted,
        last_dial_error: None,
    };
    vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: an_sdk_agent(),
        }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ]
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

/// The SDK agent alone: the only frame this build can honestly draw.
fn model() -> Model {
    fold(base())
}

/// The same agent with a Codex child stopped on an approval, so the family
/// banner still reaches a parent whose own rows nobody folds.
fn model_with_asking_child() -> Model {
    let mut msgs = base();
    let mut child = an_sdk_agent();
    child.id = child_id();
    child.name = Some("flake-hunter".to_string());
    child.kind = amux_ui::AgentKind::Codex;
    child.command = "codex".to_string();
    child.parent = Some(amux_ui::AgentParent {
        agent_id: agent_id(),
        host_id: host_id(),
    });
    msgs.push(Msg::Server(ServerMsg::AgentUpserted { agent: child }));
    msgs.push(Msg::Stream {
        agent: child_id(),
        event: StreamMsg::Opened { truncated: false },
    });
    msgs.push(Msg::Stream {
        agent: child_id(),
        event: StreamMsg::ReplayComplete,
    });
    msgs.push(Msg::Stream {
        agent: child_id(),
        event: StreamMsg::Batch {
            at: at("2026-08-12T09:12:00Z"),
            entries: vec![
                serde_json::json!({"type":"amux.codex_ready"}),
                serde_json::json!({"type":"turn/started","turn":{"id":"t1","status":"inProgress"}}),
                serde_json::json!({"type":"item/started","item":{"id":"exec-1","type":"commandExecution","command":"cargo test","cwd":"/work","status":"inProgress"}}),
                serde_json::json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1","command":"cargo test","cwd":"/work","reason":"run tests?"}),
                serde_json::json!({"type":"amux.codex_approval_required","request_id":7,"availableDecisions":["accept","cancel"]}),
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, payload)| StreamEntry {
                seq: 2 + offset as u64,
                payload,
            })
            .collect(),
        },
    });
    fold(msgs)
}

fn open_chat(model: &Model) -> ChatView {
    let mut chat = ChatView::open(model, agent_id(), 'a', false)
        .expect("an SDK-driven Claude opens its placeholder chat");
    chat.reconcile(model);
    chat
}

fn render_buffer(
    model: &Model,
    chat: ChatView,
    theme: Theme,
    size: (u16, u16),
) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(size.0, size.1);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: size,
        theme,
        now: at(NOW),
    };
    let view = ViewState {
        chat: Some(chat),
        ..ViewState::default()
    };
    terminal
        .draw(|frame| render(model, &view, &ctx, frame))
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

fn buffer_styles(buffer: &ratatui::buffer::Buffer, theme: Theme) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell").style();
            out.push(theme.classify(style));
        }
        out.push('\n');
    }
    out
}

fn assert_golden(name: &str, rendered: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name} — run with UPDATE_GOLDENS=1"));
    assert_eq!(rendered, expected, "frame {name} diverged");
}

fn assert_surface(name: &str, model: &Model) -> String {
    let mut dark = String::new();
    for (theme_name, theme) in [
        ("dark", Theme::default()),
        ("light", Theme::light(ColorMode::TrueColor)),
    ] {
        let buffer = render_buffer(model, open_chat(model), theme, (WIDTH, HEIGHT));
        let text = buffer_text(&buffer);
        let rendered = format!(
            "--- text ---\n{text}--- styles ---\n{}",
            buffer_styles(&buffer, theme)
        );
        assert_golden(&format!("{name}_{theme_name}"), &rendered);
        if theme_name == "dark" {
            dark = text;
        }
    }
    dark
}

/// The placeholder itself: it names the agent, its driver, and the protocol
/// this build cannot read, and it offers the keys that leave.
#[test]
fn sdk_chat_renders_the_unsupported_placeholder() {
    let text = assert_surface("sdk_chat_placeholder", &model());
    assert!(
        text.contains("unsupported"),
        "the frame must say so in words: {text}"
    );
    assert!(
        text.contains("claude_sdk_v1"),
        "and name the protocol: {text}"
    );
}

/// A child's ask still reaches this parent: the family banner is chrome,
/// not a fact folded from the parent's own stream.
#[test]
fn sdk_chat_still_shows_a_child_ask_banner() {
    let text = assert_surface("sdk_chat_placeholder_family", &model_with_asking_child());
    assert!(
        text.contains("flake-hunter needs permission"),
        "the child's banner must survive an unfolded parent: {text}"
    );
}

/// Every viewport draws something and nothing panics — including sizes
/// below the frame's minimum.
#[test]
fn sdk_chat_renders_at_every_viewport() {
    let model = model();
    for size in [(20, 6), (24, 10), (40, 12), (80, 24), (200, 60)] {
        let _ = render_buffer(&model, open_chat(&model), Theme::default(), size);
    }
}

/// Typing goes nowhere: the chat has no input to accept, so the composer
/// stays empty and no command is dispatched.
#[test]
fn sdk_chat_accepts_no_composer_input() {
    let model = model();
    let mut chat = open_chat(&model);
    for key in [
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    ] {
        let action =
            amux_tui::chat::handle_chat_key(&mut chat, &model, key, (WIDTH, HEIGHT), at(NOW));
        assert!(action.is_none(), "{key:?} must do nothing");
    }
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "pasted");
    assert!(
        chat.composer_mut().is_empty(),
        "nothing may reach a composer"
    );
}

/// The keys that leave still work, so a person is never stuck in a frame
/// that cannot talk to its agent.
#[test]
fn sdk_chat_closes_on_the_leader_chord() {
    let model = model();
    let mut chat = open_chat(&model);
    let leader = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        amux_tui::chat::handle_chat_key(&mut chat, &model, leader, (WIDTH, HEIGHT), at(NOW))
            .is_none()
    );
    let action = amux_tui::chat::handle_chat_key(
        &mut chat,
        &model,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        (WIDTH, HEIGHT),
        at(NOW),
    );
    assert!(matches!(action, Some(UiAction::CloseChat)));
}
