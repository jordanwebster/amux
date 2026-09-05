//! The chat of a Claude session driven over stream-JSON, drawn from rows
//! the daemon really recorded: what the session said while it was still
//! saying it, what it ran, what its subagents did, and what the person
//! can do about any of it.

use amux_tui::view::{UiAction, ViewState};
use amux_tui::{ChatView, ColorMode, FrameContext, Theme, render};
use amux_ui::claude_sdk::{ClaudeSdkCommand, FeedEntryKind, Finality};
use amux_ui::{
    Agent, AgentId, Command, HostEntry, HostId, Model, Msg, ServerMsg, StreamEntry, StreamMsg,
    update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use uuid::Uuid;

const NOW: &str = "2026-08-12T09:12:30Z";
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

/// The model and permission mode the recorded sessions ran under. Both
/// belong on screen, so both are asserted by name rather than by shape.
const RECORDED_MODEL: &str = "claude-haiku-4-5-20251001";
const RECORDED_MODE: &str = "default";

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
        name: Some("fix-sync".to_string()),
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

/// The recorded rows of one session, exactly as the daemon wrote them.
fn recorded(name: &str) -> Vec<Value> {
    let raw = match name {
        "text" => include_str!("../../amux/tests/fixtures/rows/claude-sdk/text_turn.rows.jsonl"),
        "streamed" => {
            include_str!("../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
        }
        "tasks" => {
            include_str!("../../amux/tests/fixtures/rows/claude-sdk/subagent_task.rows.jsonl")
        }
        "messaged" => include_str!("../../amux/tests/fixtures/a2a/sdk_recipient.rows.jsonl"),
        "introspection" => {
            include_str!("../../amux/tests/fixtures/rows/claude-sdk/introspection.rows.jsonl")
        }
        other => panic!("unknown recording {other}"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).expect("recorded row"))
        .collect()
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

fn batch(seq: u64, rows: Vec<Value>) -> Msg {
    Msg::Stream {
        agent: agent_id(),
        event: StreamMsg::Batch {
            at: at("2026-08-12T09:12:00Z"),
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(offset, payload)| StreamEntry {
                    seq: seq + offset as u64,
                    payload,
                })
                .collect(),
        },
    }
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

/// A whole recorded session, folded row by row exactly as it arrived.
fn session(name: &str) -> Model {
    let mut msgs = base();
    for (index, row) in recorded(name).into_iter().enumerate() {
        msgs.push(batch(index as u64, vec![row]));
    }
    fold(msgs)
}

/// The same recording stopped at the first frame that has a reply
/// half-written: the session is still speaking and the feed has to show
/// that without pretending the block is finished.
fn mid_reply() -> Model {
    let mut msgs = base();
    let mut model = fold(msgs.clone());
    for (index, row) in recorded("streamed").into_iter().enumerate() {
        let msg = batch(index as u64, vec![row]);
        msgs.push(msg.clone());
        update(&mut model, msg);
        let streaming = model
            .claude_sdk(agent_id())
            .expect("the session layer")
            .entries()
            .any(|entry| match &entry.kind {
                FeedEntryKind::Message(message) => {
                    !message.text.is_empty() && message.finality == Finality::Streaming
                }
                _ => false,
            });
        if streaming {
            return fold(msgs);
        }
    }
    panic!("the recording never streamed a reply");
}

/// A finished session with one plan it put up and got through, so the
/// chord that reopens plans has something to reopen. Synthetic, because
/// no recording in the corpus proposes a plan.
fn approved_plan() -> Model {
    let mut msgs = base();
    for (index, row) in recorded("text").into_iter().enumerate() {
        msgs.push(batch(index as u64, vec![row]));
    }
    msgs.push(batch(
        900,
        vec![
            json!({
                "type": "assistant",
                "parent_tool_use_id": null,
                "message": {
                    "id": "msg_plan",
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_plan",
                        "name": "ExitPlanMode",
                        "input": {"plan": "# ship it\n\n- read the rows\n- draw the rows"}
                    }]
                }
            }),
            json!({
                "type": "user",
                "parent_tool_use_id": null,
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_plan",
                        "content": "approved"
                    }]
                }
            }),
        ],
    ));
    fold(msgs)
}

/// The recorded session with a Codex child stopped on an approval, so the
/// family banner reaches a parent whose own rows this chat folds.
fn asking_child() -> Model {
    let mut msgs = base();
    for (index, row) in recorded("text").into_iter().enumerate() {
        msgs.push(batch(index as u64, vec![row]));
    }
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
                json!({"type":"amux.codex_ready"}),
                json!({"type":"turn/started","turn":{"id":"t1","status":"inProgress"}}),
                json!({"type":"item/started","item":{"id":"exec-1","type":"commandExecution","command":"cargo test","cwd":"/work","status":"inProgress"}}),
                json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1","command":"cargo test","cwd":"/work","reason":"run tests?"}),
                json!({"type":"amux.codex_approval_required","request_id":7,"availableDecisions":["accept","cancel"]}),
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

/// The same recording stopped while a subagent is still out — the frame a
/// person watching the work would be looking at.
fn mid_task() -> Model {
    let mut msgs = base();
    let mut model = fold(msgs.clone());
    for (index, row) in recorded("tasks").into_iter().enumerate() {
        let msg = batch(index as u64, vec![row]);
        msgs.push(msg.clone());
        update(&mut model, msg);
        let running = model
            .claude_sdk(agent_id())
            .expect("the session layer")
            .tasks()
            .any(|task| matches!(task.state, amux_ui::claude_sdk::TaskState::Running));
        if running {
            return fold(msgs);
        }
    }
    panic!("the recording never started a subagent");
}

fn open_chat(model: &Model) -> ChatView {
    let mut chat = ChatView::open(model, agent_id(), 'a', false).expect("the session opens a chat");
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
    assert_surface_with(name, model, open_chat)
}

/// The same, for a screen a person had to press something to reach.
fn assert_surface_with(name: &str, model: &Model, chat: impl Fn(&Model) -> ChatView) -> String {
    let mut dark = String::new();
    for (theme_name, theme) in [
        ("dark", Theme::default()),
        ("light", Theme::light(ColorMode::TrueColor)),
    ] {
        let buffer = render_buffer(model, chat(model), theme, (WIDTH, HEIGHT));
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

fn key(chat: &mut ChatView, model: &Model, key: KeyEvent) -> Option<UiAction> {
    amux_tui::chat::handle_chat_key(chat, model, key, (WIDTH, HEIGHT), at(NOW))
}

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

/// The whole conversation: what was asked, what came back, and the rule
/// that closed the turn — with the model and the permission mode stated
/// where a person can see them before the next thing they type.
#[test]
fn sdk_chat_paints_a_finished_turn() {
    let text = assert_surface("sdk_chat_turn", &session("text"));
    assert!(
        text.contains(RECORDED_MODEL) && text.contains(RECORDED_MODE),
        "the header states what this session runs on and under: {text}"
    );
    assert!(
        text.contains("turn ·"),
        "the turn closes with a rule: {text}"
    );
}

/// A reply still arriving reads as unfinished, not as a short answer.
#[test]
fn sdk_chat_paints_a_streaming_reply() {
    let text = assert_surface("sdk_chat_streaming", &mid_reply());
    assert!(
        text.contains('▌'),
        "an open block carries the caret that says more is coming: {text}"
    );
    assert!(
        text.contains("working"),
        "and the session reads as working: {text}"
    );
}

/// Tools and the subagents they start each get their own row.
#[test]
fn sdk_chat_paints_tools_and_tasks() {
    let model = session("tasks");
    let text = assert_surface("sdk_chat_tools", &model);
    let layer = model.claude_sdk(agent_id()).expect("the session layer");
    let task = layer
        .tasks()
        .next()
        .expect("the recording started a subagent");
    assert!(
        text.contains(&format!("task {}", task.description)),
        "the subagent's work is named: {text}"
    );
}

/// A message from another agent is drawn the way every chat draws one.
#[test]
fn sdk_chat_paints_an_agent_message() {
    let text = assert_surface("sdk_chat_agent_message", &session("messaged"));
    assert!(
        text.contains('←'),
        "an inbound message wears its direction: {text}"
    );
}

/// A child's ask still reaches this parent.
#[test]
fn sdk_chat_still_shows_a_child_ask_banner() {
    let text = assert_surface("sdk_chat_family", &asking_child());
    assert!(
        text.contains("flake-hunter needs permission"),
        "the child's banner reaches the parent: {text}"
    );
}

/// Every viewport draws something and nothing panics — including sizes
/// below the frame's minimum.
#[test]
fn sdk_chat_renders_at_every_viewport() {
    let model = session("tasks");
    for size in [(20, 6), (24, 10), (40, 12), (80, 24), (200, 60)] {
        let _ = render_buffer(&model, open_chat(&model), Theme::default(), size);
    }
}

/// Enter sends what was typed, and the draft leaves with it.
#[test]
fn sdk_chat_enter_sends_the_draft() {
    let model = session("text");
    let mut chat = open_chat(&model);
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "one more thing");
    let action = key(&mut chat, &model, KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        action,
        Some(UiAction::Dispatch(Command::ClaudeSdk(
            ClaudeSdkCommand::SendPrompt {
                agent: agent_id(),
                text: "one more thing".to_string(),
            }
        ))),
        "Enter sends the draft"
    );
    assert!(
        chat.composer_mut().is_empty(),
        "and the draft leaves with it"
    );
}

/// Ctrl+X interrupts a session that is in the middle of something.
#[test]
fn sdk_chat_ctrl_x_interrupts_a_working_session() {
    let model = mid_reply();
    let mut chat = open_chat(&model);
    assert_eq!(
        key(&mut chat, &model, ctrl('x')),
        Some(UiAction::Dispatch(Command::ClaudeSdk(
            ClaudeSdkCommand::Interrupt { agent: agent_id() }
        ))),
        "Ctrl+X interrupts"
    );
}

/// Shift+Tab asks the session for its next permission mode.
#[test]
fn sdk_chat_shift_tab_cycles_the_permission_mode() {
    let model = session("text");
    let mut chat = open_chat(&model);
    assert_eq!(
        key(&mut chat, &model, KeyEvent::from(KeyCode::BackTab)),
        Some(UiAction::Dispatch(Command::ClaudeSdk(
            ClaudeSdkCommand::CyclePermissionMode { agent: agent_id() }
        ))),
        "Shift+Tab cycles the mode the header states"
    );
}

/// Ctrl+V attaches what the clipboard holds; the draft gains a token for
/// it rather than the bytes.
#[test]
fn sdk_chat_ctrl_v_attaches_the_clipboard() {
    let model = session("text");
    let mut chat = open_chat(&model);
    amux_tui::chat::handle_chat_clipboard(
        &mut chat,
        &model,
        amux_tui::clipboard::ClipboardContent::Image {
            mime: "image/png".to_string(),
            bytes: vec![b'p'; 512],
        },
    );
    assert!(
        !chat.composer_mut().tokens().is_empty(),
        "the draft carries the attachment"
    );
}

/// The review chord asks for the diff the page is frozen against.
#[test]
fn sdk_chat_leader_r_asks_for_the_diff_to_review() {
    let model = session("text");
    let mut chat = open_chat(&model);
    assert!(key(&mut chat, &model, ctrl('a')).is_none(), "leader pends");
    assert!(
        matches!(
            key(&mut chat, &model, KeyEvent::from(KeyCode::Char('r'))),
            Some(UiAction::Dispatch(Command::RequestDiff { agent, .. })) if agent == agent_id()
        ),
        "the chord asks for a diff to review"
    );
}

/// Ctrl+T reopens the plan this session already got through.
#[test]
fn sdk_chat_ctrl_t_opens_the_accepted_plan() {
    let model = approved_plan();
    let mut chat = open_chat(&model);
    assert!(
        key(&mut chat, &model, ctrl('t')).is_none(),
        "the reader opens"
    );
    let text = buffer_text(&render_buffer(
        &model,
        chat,
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("ship it") && text.contains("read the rows"),
        "the reader shows the plan: {text}"
    );
}

/// The row under the feed carries the passive context meter and how many
/// subagents are still out.
#[test]
fn sdk_chat_activity_line_states_the_context_and_open_tasks() {
    let text = buffer_text(&render_buffer(
        &session("text"),
        open_chat(&session("text")),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("ctx 27.9k/200.0k"),
        "the meter states what the last turn actually saw, against the window: {text}"
    );

    let model = mid_task();
    let running = buffer_text(&render_buffer(
        &model,
        open_chat(&model),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        running.contains("1 task running"),
        "and how many subagents are still out: {running}"
    );

    // A session that has not reported usage says so rather than guessing.
    let fresh = fold(base());
    let quiet = buffer_text(&render_buffer(
        &fresh,
        open_chat(&fresh),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        quiet.contains("ctx unknown"),
        "an unreported meter is unknown, not zero: {quiet}"
    );
}

/// `<leader> c` asks the session where its context went and opens the
/// answer over the frame.
#[test]
fn sdk_chat_context_overlay_lists_the_breakdown() {
    let model = session("introspection");
    let mut chat = open_chat(&model);
    assert!(key(&mut chat, &model, ctrl('a')).is_none(), "leader pends");
    assert_eq!(
        key(&mut chat, &model, KeyEvent::from(KeyCode::Char('c'))),
        Some(UiAction::Dispatch(Command::ClaudeSdk(
            ClaudeSdkCommand::RequestContextBreakdown { agent: agent_id() }
        ))),
        "the chord costs one round trip, and only when asked"
    );
    let text = buffer_text(&render_buffer(
        &model,
        chat.clone(),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("context · 23,394 of 200,000 tokens"),
        "the overlay states the total against the window: {text}"
    );
    assert!(
        text.contains("System prompt") && text.contains("Messages"),
        "and every category the session reported: {text}"
    );
    assert!(text.contains("esc close"), "with the way out: {text}");

    assert!(
        key(&mut chat, &model, KeyEvent::from(KeyCode::Esc)).is_none(),
        "esc closes it"
    );
    let closed = buffer_text(&render_buffer(
        &model,
        chat,
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        !closed.contains("System prompt"),
        "and the chat is back: {closed}"
    );
    assert_surface_with("sdk_chat_context", &model, |model| {
        let mut chat = open_chat(model);
        let _ = key(&mut chat, model, ctrl('a'));
        let _ = key(&mut chat, model, KeyEvent::from(KeyCode::Char('c')));
        chat
    });
}

/// A subagent's block says what it was asked to do, what it came back
/// with, and what it cost.
#[test]
fn sdk_chat_task_block_states_what_the_subagent_did() {
    let model = session("tasks");
    let text = buffer_text(&render_buffer(
        &model,
        open_chat(&model),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("done ·"),
        "a finished task reads as finished: {text}"
    );
    let running = mid_task();
    let watching = buffer_text(&render_buffer(
        &running,
        open_chat(&running),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        watching.contains("running"),
        "and one still out reads as running: {watching}"
    );
}

/// An MCP server that is not ready is stated once, above the feed; a
/// session whose servers are all connected says nothing.
#[test]
fn sdk_chat_mcp_status_line_names_what_is_not_ready() {
    let model = session("text");
    let text = buffer_text(&render_buffer(
        &model,
        open_chat(&model),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("mcp · ") && text.contains("needs-auth"),
        "the line names the state and who is in it: {text}"
    );
    let fresh = fold(base());
    let quiet = buffer_text(&render_buffer(
        &fresh,
        open_chat(&fresh),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        !quiet.contains("mcp · "),
        "a session with no servers to report says nothing: {quiet}"
    );
}

/// The rule that closes a turn carries what the turn cost.
#[test]
fn sdk_chat_turn_rule_states_the_cost() {
    let model = session("text");
    let text = buffer_text(&render_buffer(
        &model,
        open_chat(&model),
        Theme::default(),
        (WIDTH, HEIGHT),
    ));
    assert!(
        text.contains("$0.0225"),
        "the turn rule prices the turn the session priced: {text}"
    );
}
