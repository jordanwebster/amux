//! Full-frame Codex chat goldens. Each named state is locked in both themes,
//! including a one-character-per-cell semantic style map.

use amux_tui::view::{UiAction, ViewState};
use amux_tui::{ChatView, FrameContext, Theme, render};
use amux_ui::codex::{CodexCommand, CodexDecision};
use amux_ui::{
    Agent, AgentId, Command, HostEntry, HostId, Model, Msg, OpId, ServerMsg, StreamCloseReason,
    StreamEntry, StreamMsg, update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use uuid::Uuid;

const NOW: &str = "2026-08-12T09:12:30Z";
const WIDTH: u16 = 88;
const HEIGHT: u16 = 34;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn agent_id() -> AgentId {
    Uuid::from_u128(8)
}

fn host_id() -> HostId {
    Uuid::from_u128(1)
}

fn op(n: u8) -> OpId {
    OpId(Uuid::from_u128((3u128 << 64) | u128::from(n)))
}

fn base() -> Vec<Msg> {
    let agent = Agent {
        id: agent_id(),
        host_id: host_id(),
        name: Some("codex-retry".to_string()),
        command: "codex".to_string(),
        working_dir: "/work/amux".into(),
        agent_type: "codex".to_string(),
        io_protocols: vec!["terminal_v1".to_string(), "codex_sdk_v1".to_string()],
        readonly: false,
        args: Vec::new(),
        created_at: at("2026-08-12T09:00:00Z"),
    };
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
        Msg::Server(ServerMsg::AgentUpserted { agent }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::ReplayComplete,
        },
    ]
}

fn batch(first_seq: u64, rows: Vec<Value>) -> Msg {
    Msg::Stream {
        agent: agent_id(),
        event: StreamMsg::Batch {
            at: at("2026-08-12T09:12:00Z"),
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(offset, payload)| StreamEntry {
                    seq: first_seq + offset as u64,
                    payload,
                })
                .collect(),
        },
    }
}

fn model(rows: Vec<Value>) -> Model {
    model_with_extra(rows, Vec::new())
}

fn model_with_extra(rows: Vec<Value>, extra: Vec<Msg>) -> Model {
    let mut msgs = base();
    msgs.push(batch(10, rows));
    msgs.extend(extra);
    let mut model = Model::default();
    for msg in msgs {
        update(&mut model, msg);
    }
    let violations = model.check_invariants();
    assert!(violations.is_empty(), "fixture coherent: {violations:?}");
    model
}

fn view(model: &Model) -> ViewState {
    let mut chat = ChatView::open(model, agent_id(), 'a', false);
    chat.set_codex_configuration_label(Some(
        "model=gpt-5.4 · approval=on-request · sandbox=workspace-write".to_string(),
    ));
    chat.reconcile(model);
    ViewState {
        chat: Some(chat),
        ..ViewState::default()
    }
}

fn render_buffer(model: &Model, theme: Theme, height: u16) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (WIDTH, height),
        theme,
        now: at(NOW),
    };
    let view = view(model);
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

fn buffer_styles(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell").style();
            let class = match style.fg {
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
            };
            out.push(class);
        }
        out.push('\n');
    }
    out
}

fn assert_surface(name: &str, model: &Model) {
    let height = if name == "streaming" { 50 } else { HEIGHT };
    for (theme_name, theme) in [("dark", Theme::Dark), ("light", Theme::Light)] {
        let buffer = render_buffer(model, theme, height);
        let rendered = format!(
            "--- text ---\n{}--- styles ---\n{}",
            buffer_text(&buffer),
            buffer_styles(&buffer)
        );
        let golden_name = format!("codex_chat_{name}_{theme_name}");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("{golden_name}.txt"));
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::write(&path, rendered).expect("write golden");
        } else {
            let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!("missing golden {golden_name} — run with UPDATE_GOLDENS=1")
            });
            assert_eq!(rendered, expected, "frame {golden_name} diverged");
        }
    }
}

fn ready() -> Value {
    json!({"type":"amux.codex_ready"})
}

fn live_rows() -> Vec<Value> {
    vec![
        ready(),
        json!({"type":"turn/started","turn":{"id":"turn-live","status":"inProgress"}}),
        json!({"type":"item/started","turnId":"turn-live","item":{"id":"user-1","type":"userMessage","content":[{"type":"text","text":"Make retries configurable."}]}}),
        json!({"type":"item/completed","turnId":"turn-live","item":{"id":"user-1","type":"userMessage","content":[{"type":"text","text":"Make retries configurable."}]}}),
        json!({"type":"item/started","item":{"id":"reason-1","type":"reasoning","content":[],"summary":[]}}),
        json!({"type":"item/reasoning/summaryTextDelta","itemId":"reason-1","summaryIndex":0,"delta":"Inspecting retry policy"}),
        json!({"type":"turn/plan/updated","turnId":"turn-live","explanation":"Keep the change narrow","plan":[{"step":"Update config","status":"completed"},{"step":"Run tests","status":"inProgress"}]}),
        json!({"type":"item/fileChange/patchUpdated","itemId":"file-1","changes":[{"path":"src/retry.rs","kind":{"type":"modify"},"diff":"@@ -1 +1 @@\n-const RETRIES: u8 = 3;\n+const RETRIES: u8 = 6;"}]}),
        json!({"type":"item/fileChange/outputDelta","itemId":"file-1","delta":"@@ -1 +1 @@\n-const RETRIES: u8 = 3;\n+const RETRIES: u8 = 6;"}),
        json!({"type":"item/started","item":{"id":"cmd-1","type":"commandExecution","command":"cargo test -p amux-ui","cwd":"/work/amux","status":"inProgress"}}),
        json!({"type":"item/commandExecution/outputDelta","itemId":"cmd-1","stream":"stdout","delta":"running 42 tests\n"}),
        json!({"type":"item/completed","item":{"id":"mcp-1","type":"mcpToolCall","server":"issues","tool":"lookup","arguments":{"id":42},"result":{"title":"Retry bug"},"status":"completed"}}),
        json!({"type":"item/completed","item":{"id":"dynamic-1","type":"dynamicToolCall","namespace":"ops","tool":"validate","arguments":{"target":"staging"},"success":true,"status":"completed"}}),
        json!({"type":"item/started","item":{"id":"web-1","type":"webSearch","query":"Rust retry jitter","action":{"type":"search"}}}),
        json!({"type":"thread/compacted","turnId":"turn-live"}),
        json!({"type":"item/started","item":{"id":"msg-1","type":"agentMessage","text":"Tests are still running.","phase":"commentary"}}),
    ]
}

fn idle_rows() -> Vec<Value> {
    vec![
        ready(),
        json!({"type":"turn/started","turn":{"id":"turn-done","status":"inProgress"}}),
        json!({"type":"item/completed","turnId":"turn-done","item":{"id":"user-2","type":"userMessage","content":[{"type":"text","text":"Run the focused tests."}]}}),
        json!({"type":"item/completed","item":{"id":"cmd-2","type":"commandExecution","command":"cargo test -p amux-ui","cwd":"/work/amux","status":"completed","exitCode":0,"aggregatedOutput":"42 passed"}}),
        json!({"type":"item/completed","item":{"id":"msg-2","type":"agentMessage","text":"All focused tests pass.","phase":"final_answer"}}),
        json!({"type":"thread/tokenUsage/updated","tokenUsage":{"total":{"inputTokens":120,"cachedInputTokens":40,"outputTokens":18,"reasoningOutputTokens":5,"totalTokens":138},"modelContextWindow":128000}}),
        json!({"type":"turn/completed","turn":{"id":"turn-done","status":"completed"}}),
    ]
}

fn approval_rows() -> Vec<Value> {
    vec![
        ready(),
        json!({"type":"turn/started","turn":{"id":"turn-ask","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"exec-ask","type":"commandExecution","command":"cargo test --workspace","cwd":"/work/amux","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-ask","command":"cargo test --workspace","cwd":"/work/amux","reason":"Run the repository test suite?"}),
        json!({"type":"amux.codex_approval_required","request_id":"approval-1","availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"rule":"cargo test"}},"decline","cancel"]}),
    ]
}

#[test]
fn codex_streaming_turn_both_themes() {
    let extra = vec![
        Msg::Command {
            op: op(1),
            command: Command::Codex(CodexCommand::Steer {
                agent: agent_id(),
                text: "also inspect the integration test".to_string(),
            }),
        },
        batch(
            100,
            vec![json!({"type":"amux.input_result","input_id":op(1).0.as_bytes(),"ok":{}})],
        ),
    ];
    assert_surface("streaming", &model_with_extra(live_rows(), extra));
}

#[test]
fn codex_idle_both_themes() {
    assert_surface("idle", &model(idle_rows()));
}

#[test]
fn codex_approval_pending_both_themes() {
    assert_surface("approval_pending", &model(approval_rows()));
}

#[test]
fn codex_approval_resolved_both_themes() {
    let mut rows = approval_rows();
    rows.extend([
        json!({"type":"amux.codex_approval_resolved","request_id":"approval-1","reason":"answered"}),
        json!({"type":"item/completed","item":{"id":"exec-ask","type":"commandExecution","command":"cargo test --workspace","cwd":"/work/amux","status":"completed","exitCode":0,"aggregatedOutput":"all tests passed"}}),
        json!({"type":"turn/completed","turn":{"id":"turn-ask","status":"completed"}}),
    ]);
    assert_surface("approval_resolved", &model(rows));
}

#[test]
fn codex_unsupported_blocked_both_themes() {
    assert_surface(
        "unsupported_blocked",
        &model(vec![
            ready(),
            json!({"type":"turn/started","turn":{"id":"turn-question","status":"inProgress"}}),
            json!({"type":"item/tool/requestUserInput","itemId":"question-1","questions":[{"id":"target","question":"Which deployment target?","options":["staging","production"]}]}),
        ]),
    );
}

#[test]
fn codex_error_gap_both_themes() {
    assert_surface(
        "error_gap",
        &model(vec![
            ready(),
            json!({"type":"warning","message":"context window nearly full"}),
            json!({"type":"amux.codex_gap","reason":"connection_lost"}),
            ready(),
            json!({"type":"error","error":{"message":"tool transport failed"},"willRetry":true}),
            json!({"type":"future/method","detail":"kept visible"}),
        ]),
    );
}

#[test]
fn codex_empty_new_session_both_themes() {
    assert_surface("empty", &model(vec![ready()]));
}

fn press(
    model: &Model,
    chat: &mut ChatView,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Option<UiAction> {
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        KeyEvent::new(code, modifiers),
        (WIDTH, HEIGHT),
        at(NOW),
    )
}

#[test]
fn codex_send_steer_interrupt_and_approval_keys_dispatch_native_commands() {
    let idle = model(vec![ready()]);
    let mut idle_chat = ChatView::open(&idle, agent_id(), 'a', false);
    idle_chat.composer_mut().insert_str("hello");
    assert!(matches!(
        press(&idle, &mut idle_chat, KeyCode::Enter, KeyModifiers::NONE),
        Some(UiAction::Dispatch(Command::Codex(CodexCommand::Prompt { text, .. }))) if text == "hello"
    ));

    let live = model(live_rows());
    let mut live_chat = ChatView::open(&live, agent_id(), 'a', false);
    live_chat.composer_mut().insert_str("check tests");
    assert!(matches!(
        press(&live, &mut live_chat, KeyCode::Enter, KeyModifiers::NONE),
        Some(UiAction::Dispatch(Command::Codex(CodexCommand::Steer { text, .. }))) if text == "check tests"
    ));
    assert!(matches!(
        press(&live, &mut live_chat, KeyCode::Char('x'), KeyModifiers::CONTROL),
        Some(UiAction::Dispatch(Command::Codex(CodexCommand::Interrupt { agent }))) if agent == agent_id()
    ));

    let approval = model(approval_rows());
    let mut approval_chat = ChatView::open(&approval, agent_id(), 'a', false);
    approval_chat.reconcile(&approval);
    assert!(matches!(
        press(
            &approval,
            &mut approval_chat,
            KeyCode::Enter,
            KeyModifiers::NONE
        ),
        Some(UiAction::Dispatch(Command::Codex(CodexCommand::Answer {
            decision: CodexDecision::Accept,
            ..
        })))
    ));
}

#[test]
fn codex_keys_follow_the_write_gate_and_preserve_a_refused_steer_draft() {
    let live = model(live_rows());
    let mut stale_rows = live_rows();
    stale_rows.push(json!({"type":"amux.codex_gap","reason":"connection_lost"}));
    let stale = model(stale_rows);
    assert_eq!(
        amux_ui::codex::send_gate(&stale, agent_id()),
        amux_ui::codex::SendGate::Unknown
    );

    let mut chat = ChatView::open(&stale, agent_id(), 'a', false);
    chat.composer_mut().insert_str("keep this steer");
    assert_eq!(
        press(&stale, &mut chat, KeyCode::Enter, KeyModifiers::NONE),
        None
    );
    assert_eq!(
        press(&stale, &mut chat, KeyCode::Char('x'), KeyModifiers::CONTROL),
        None
    );
    assert!(matches!(
        press(&live, &mut chat, KeyCode::Enter, KeyModifiers::NONE),
        Some(UiAction::Dispatch(Command::Codex(CodexCommand::Steer { text, .. })))
            if text == "keep this steer"
    ));

    let replaying_approval = model_with_extra(
        approval_rows(),
        vec![
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Closed {
                    reason: StreamCloseReason::HostUnreachable,
                },
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
        ],
    );
    assert_eq!(
        amux_ui::codex::send_gate(&replaying_approval, agent_id()),
        amux_ui::codex::SendGate::Replaying
    );
    let mut approval_chat = ChatView::open(&replaying_approval, agent_id(), 'a', false);
    approval_chat.reconcile(&replaying_approval);
    assert_eq!(
        press(
            &replaying_approval,
            &mut approval_chat,
            KeyCode::Enter,
            KeyModifiers::NONE
        ),
        None
    );
}
