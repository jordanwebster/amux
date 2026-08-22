//! Tier-3 golden frames for the chat screen: the renderer is a pure
//! function of (Model, ViewState, FrameContext), so every frame renders
//! from a fixture-built Model and diffs against checked-in text — no
//! network, no clocks, no flake. The frames follow `docs/CHAT.md`
//! §Wireframes (the normative visual language).
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test
//! chat_golden` and review the diff like code. Style maps (one char per
//! cell classifying fg color/modifier) lock the semantic-token application
//! per theme — the surface the text goldens are blind to.

use amux_tui::view::ViewState;
use amux_tui::{ChatView, FrameContext, Theme, render};
use amux_ui::claude::encoding::{AskAnswer, PermissionAnswer};
use amux_ui::claude::{DiffArtifact, DiffHunk, DiffMagnitude, DiffNumbering};
use amux_ui::{
    Agent, AgentId, Command, HostEntry, HostId, Model, Msg, OpId, ServerMsg, StreamEntry,
    StreamMsg, update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use uuid::Uuid;

// --- fixture builders -------------------------------------------------------

const SESSION: &str = "22222222-2222-4222-8222-222222222222";

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn agent_id() -> AgentId {
    Uuid::from_u128(7)
}

fn host_id() -> HostId {
    Uuid::from_u128(1)
}

fn op(n: u8) -> OpId {
    OpId(Uuid::from_u128((3u128 << 64) | u128::from(n)))
}

fn uuid_row(n: u32) -> String {
    format!("dddddddd-0000-4000-8000-0000{n:08}")
}

fn base_msgs() -> Vec<Msg> {
    base_msgs_readonly(false)
}

fn base_msgs_readonly(readonly: bool) -> Vec<Msg> {
    let agent = Agent {
        id: agent_id(),
        host_id: host_id(),
        name: Some(if readonly { "ci-triage" } else { "fix-auth" }.to_string()),
        command: "claude".to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        agent_type: "claude".to_string(),
        io_protocols: vec![
            "terminal_v1".to_string(),
            "claude_pty_transcript_v1".to_string(),
        ],
        readonly,
        args: Vec::new(),
        created_at: at("2026-08-12T08:00:00Z"),
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
    ]
}

fn opened(truncated: bool) -> Vec<Msg> {
    vec![
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::Opened { truncated },
        },
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::ReplayComplete,
        },
    ]
}

fn batch(arrived: &str, first_seq: u64, rows: Vec<Value>) -> Msg {
    Msg::Stream {
        agent: agent_id(),
        event: StreamMsg::Batch {
            at: at(arrived),
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

fn fold(msgs: Vec<Msg>) -> Model {
    let mut model = Model::default();
    for msg in msgs {
        update(&mut model, msg);
    }
    let violations = model.check_invariants();
    assert!(violations.is_empty(), "fixture coherent: {violations:?}");
    model
}

// --- row builders (shapes per notes/chat-v1/transcript-semantics.md) --------

fn ready_row() -> Value {
    json!({"type": "amux.transcript_ready"})
}

fn mode_row(mode: &str) -> Value {
    json!({"type": "permission-mode", "permissionMode": mode})
}

fn prompt_row(n: u32, ts: &str, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {"role": "user", "content": text},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn assistant_text_row(n: u32, ts: &str, message_id: &str, text: &str, stop: Option<&str>) -> Value {
    json!({
        "type": "assistant",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {
            "id": message_id,
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": stop,
        },
    })
}

fn tool_use_row(n: u32, ts: &str, message_id: &str, blocks: Vec<Value>) -> Value {
    json!({
        "type": "assistant",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {
            "id": message_id,
            "role": "assistant",
            "content": blocks,
            "stop_reason": "tool_use",
        },
    })
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

fn tool_result_row(n: u32, ts: &str, results: Vec<Value>, sidecar: Option<Value>) -> Value {
    let mut row = json!({
        "type": "user",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {"role": "user", "content": results},
    });
    if let Some(sidecar) = sidecar {
        row["toolUseResult"] = sidecar;
    }
    row
}

fn tool_result(id: &str, content: &str) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": content})
}

fn turn_duration_row(n: u32, ts: &str, ms: u64) -> Value {
    json!({
        "type": "system",
        "subtype": "turn_duration",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "durationMs": ms,
    })
}

/// An Edit sidecar whose `structuredPatch` states the FACT magnitude.
fn edit_sidecar(path: &str, added: usize, removed: usize) -> Value {
    let lines: Vec<String> = (0..added)
        .map(|i| format!("+line {i}"))
        .chain((0..removed).map(|i| format!("-line {i}")))
        .collect();
    json!({"filePath": path, "structuredPatch": [{"lines": lines}]})
}

// --- ask fixtures (hook rows per transcript-semantics §18) -------------------

/// A `hook.permission_request` row with `count` copies of the verified
/// one-suggestion shape (`addDirectories`/`session`).
fn hook_row(tool: &str, input: Value, suggestions: usize) -> Value {
    let mut row = json!({
        "type": "hook.permission_request",
        "tool_name": tool,
        "tool_input": input,
        "permission_mode": "default",
    });
    if suggestions > 0 {
        let entries: Vec<Value> = (0..suggestions)
            .map(|_| {
                json!({
                    "type": "addDirectories",
                    "destination": "session",
                    "directories": ["/work"],
                })
            })
            .collect();
        row["permission_suggestions"] = Value::Array(entries);
    }
    row
}

fn edit_hook(path: &str, old: &str, new: &str) -> Value {
    hook_row(
        "Edit",
        json!({"file_path": path, "old_string": old, "new_string": new}),
        1,
    )
}

/// The wireframe plan (C3), padded so the reader has something to scroll.
fn wireframe_plan() -> String {
    let mut plan = String::from(
        "## Approach\n\n1. Wrap Client::reconnect in retry_with_backoff\n   \
         - exponential base 200 ms, cap 5 s, jitter bounded\n2. Thread RetryConfig \
         through SyncOptions\n3. Spec chapter sync::retry — cold start, mid-stream \
         drop, give-up-after-cap\n\n## Out of scope\n\n- relay-side backpressure\n",
    );
    for step in 1..=20 {
        plan.push_str(&format!("\nStep detail {step}: keep the reader scrolling."));
    }
    plan
}

/// A working conversation with an Edit permission ask heading the queue
/// (the C2 wireframe's shape: a real snippet diff, one suggestion).
fn edit_ask_msgs() -> Vec<Msg> {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![edit_hook(
            "sync/config.rs",
            "pub struct RetryConfig {\n    pub max_attempts: u8,\n    pub base_delay: Duration,\n    pub max_delay: Duration,\n    pub multiplier: f32,\n}",
            "pub struct RetryConfig {\n    pub max_attempts: u8,        // capped at 6\n    pub jitter_ms: u16,\n    pub retry_budget: u32,\n    pub on_exhausted: Behavior,\n    pub base_delay: Duration,\n    pub max_delay: Duration,\n    pub multiplier: f32,\n}",
        )],
    ));
    msgs
}

/// A chat view reconciled against the model (what the run loop does after
/// every fold) — panels and readers sync to the ask head.
fn reconciled_view(model: &Model) -> ViewState {
    let mut view = chat_view();
    view.chat.as_mut().expect("chat open").reconcile(model);
    view
}

fn press(view: &mut ViewState, model: &Model, code: KeyCode) {
    let chat = view.chat.as_mut().expect("chat open");
    amux_tui::chat::handle_chat_key(
        chat,
        model,
        KeyEvent::new(code, KeyModifiers::NONE),
        (80, 24),
        at(IDLE_NOW),
    );
}

fn type_text(view: &mut ViewState, model: &Model, text: &str) {
    for c in text.chars() {
        press(view, model, KeyCode::Char(c));
    }
}

// --- the idle conversation (mirrors the idle wireframe) ---------------------

fn idle_msgs() -> Vec<Msg> {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(1, "2026-08-12T09:00:00Z", "add retry with backoff to the sync client"),
            assistant_text_row(
                2,
                "2026-08-12T09:01:20Z",
                "msg_01",
                "I added exponential backoff to Client::reconnect — 6 attempts,\njitter capped at 500 ms. SyncOptions grew a RetryConfig.",
                Some("end_turn"),
            ),
            tool_use_row(
                3,
                "2026-08-12T09:01:25Z",
                "msg_02",
                vec![
                    tool_use("toolu_01", "Read", json!({"file_path": "sync/client.rs"})),
                    tool_use("toolu_02", "Grep", json!({"pattern": "reconnect"})),
                ],
            ),
            tool_result_row(
                4,
                "2026-08-12T09:01:26Z",
                vec![
                    tool_result("toolu_01", "…file content…"),
                    tool_result("toolu_02", "4 matches"),
                ],
                None,
            ),
            tool_use_row(
                5,
                "2026-08-12T09:01:30Z",
                "msg_03",
                vec![tool_use(
                    "toolu_03",
                    "Edit",
                    json!({"file_path": "sync/client.rs", "old_string": "a", "new_string": "b"}),
                )],
            ),
            tool_result_row(
                6,
                "2026-08-12T09:01:31Z",
                vec![tool_result("toolu_03", "ok")],
                Some(edit_sidecar("sync/client.rs", 18, 4)),
            ),
            tool_use_row(
                7,
                "2026-08-12T09:01:35Z",
                "msg_04",
                vec![tool_use(
                    "toolu_04",
                    "Bash",
                    json!({"command": "cargo test -p amux-sync"}),
                )],
            ),
            tool_result_row(
                8,
                "2026-08-12T09:01:40Z",
                vec![tool_result("toolu_04", "34 passed, 0 failed (2.1s)")],
                None,
            ),
            turn_duration_row(9, "2026-08-12T09:01:42Z", 102_000),
        ],
    ));
    msgs
}

fn idle_model() -> Model {
    fold(idle_msgs())
}

/// The working conversation (mirrors the working wireframe): thinking
/// marker, open Bash, working line ticking from the prompt row timestamp.
fn working_msgs() -> Vec<Msg> {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:10:10Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(1, "2026-08-12T09:10:00Z", "now make the retry count configurable"),
            json!({
                "type": "assistant",
                "uuid": uuid_row(2),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:10:06Z",
                "message": {
                    "id": "msg_11",
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "…"},
                        {"type": "text", "text": "The cap belongs in RetryConfig; I'll thread it through SyncOptions."},
                    ],
                    "stop_reason": "end_turn",
                },
            }),
            tool_use_row(
                3,
                "2026-08-12T09:10:08Z",
                "msg_12",
                vec![
                    tool_use("toolu_11", "Read", json!({"file_path": "sync/config.rs"})),
                    tool_use("toolu_12", "Bash", json!({"command": "cargo check -p amux-sync"})),
                ],
            ),
            tool_result_row(
                4,
                "2026-08-12T09:10:09Z",
                vec![tool_result("toolu_11", "…file content…")],
                None,
            ),
        ],
    ));
    msgs
}

fn working_model() -> Model {
    fold(working_msgs())
}

/// `now` for the working frames: 24 s after the prompt row.
const WORKING_NOW: &str = "2026-08-12T09:10:24Z";

// --- rendering --------------------------------------------------------------

fn chat_view() -> ViewState {
    ViewState {
        chat: Some(ChatView::open_claude(agent_id(), 'a', false)),
        ..ViewState::default()
    }
}

fn render_buffer(
    model: &Model,
    view: &ViewState,
    width: u16,
    height: u16,
    theme: Theme,
    now: &str,
) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (width, height),
        theme,
        now: at(now),
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
            out.push_str(buffer.cell((x, y)).expect("cell in area").symbol());
        }
        out.push('\n');
    }
    out
}

/// One char per cell classifying its style: fg color first (r/y/g/c/B/a),
/// then modifiers (b bold, i italic, d dim), '.' plain — the semantic-token
/// surface text goldens cannot see.
fn buffer_styles(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell in area").style();
            let class = match style.fg {
                Some(Color::Red) => 'r',
                Some(Color::Yellow) => 'y',
                Some(Color::Green) => 'g',
                Some(Color::Cyan) => 'c',
                Some(Color::Blue) => 'B',
                Some(Color::DarkGray) => 'a',
                _ => {
                    if style.add_modifier.contains(Modifier::BOLD) {
                        'b'
                    } else if style.add_modifier.contains(Modifier::ITALIC) {
                        'i'
                    } else if style.add_modifier.contains(Modifier::DIM) {
                        'd'
                    } else {
                        '.'
                    }
                }
            };
            out.push(class);
        }
        out.push('\n');
    }
    out
}

fn render_frame(model: &Model, view: &ViewState, width: u16, height: u16, now: &str) -> String {
    buffer_text(&render_buffer(model, view, width, height, Theme::Dark, now))
}

fn assert_golden(name: &str, rendered: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name} — run with UPDATE_GOLDENS=1"));
    assert_eq!(
        rendered, expected,
        "frame {name} diverged from its golden; if intended, regenerate with UPDATE_GOLDENS=1 and review"
    );
}

const IDLE_NOW: &str = "2026-08-12T09:02:10Z";

// --- the named frames -------------------------------------------------------

/// The idle wireframe: prompt, markdown-lite message, grouped read/search
/// line, Edit magnitude, Bash output continuation, measured turn rule,
/// placeholder composer, ready-footer with the mode on the right.
#[test]
fn chat_idle() {
    let rendered = render_frame(&idle_model(), &chat_view(), 80, 20, IDLE_NOW);
    assert_golden("chat_idle", &rendered);
}

/// The `?` overlay: the chat's full effective key list with tier
/// annotations, from the one binding table — plain terminal (no kitty
/// row; ctrl+home/end and ctrl+←/→ marked terminal-dependent), tall
/// enough for the full list.
#[test]
fn chat_help_overlay() {
    let model = idle_model();
    let mut view = chat_view();
    view.chat.as_mut().expect("chat open").set_help(true);
    let rendered = render_frame(&model, &view, 80, 46, IDLE_NOW);
    assert_golden("chat_help_overlay", &rendered);
}

/// The overlay on a kitty terminal grows the shift+enter row (tier
/// named); on a short viewport the tail gives way behind an honest `⋮`.
#[test]
fn chat_help_overlay_kitty_short() {
    let model = idle_model();
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.set_help(true);
        chat.set_kitty(true);
    }
    let rendered = render_frame(&model, &view, 80, 24, IDLE_NOW);
    assert_golden("chat_help_overlay_kitty_short", &rendered);
}

/// The armed quit guard (chrome-wide Ctrl+C): the footer hint line
/// becomes `press ctrl+c again to quit` in warning color — hints
/// replaced, the mode segment kept. Warning color is theme-independent
/// (named ANSI yellow in both themes), so one frame locks it.
#[test]
fn chat_quit_armed() {
    let model = idle_model();
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .quit_guard_mut()
        .press(at(IDLE_NOW));
    let rendered = render_frame(&model, &view, 80, 20, IDLE_NOW);
    assert_golden("chat_quit_armed", &rendered);
}

/// The armed guard with a docked panel: the panel's hint row is the
/// footer hint line there — same rule, same message.
#[test]
fn chat_quit_armed_panel() {
    let model = fold(edit_ask_msgs());
    let mut view = reconciled_view(&model);
    view.chat
        .as_mut()
        .expect("chat open")
        .quit_guard_mut()
        .press(at(WORKING_NOW));
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_quit_armed_panel", &rendered);
}

/// At narrow widths the normal panel hint wraps. Arming the quit guard
/// replaces that complete wrapped range with one row before tail
/// truncation, leaving every action visible and no hint fragment behind.
#[test]
fn chat_quit_armed_panel_narrow() {
    let model = fold(edit_ask_msgs());
    let mut view = reconciled_view(&model);
    let unarmed = render_frame(&model, &view, 60, 20, WORKING_NOW);
    let hint_rows: Vec<&str> = unarmed
        .lines()
        .filter(|line| line.contains("select · enter confirm") || line.contains("never answers"))
        .collect();
    assert_eq!(hint_rows.len(), 2, "the fixture's normal hint wraps");
    view.chat
        .as_mut()
        .expect("chat open")
        .quit_guard_mut()
        .press(at(WORKING_NOW));
    let rendered = render_frame(&model, &view, 60, 20, WORKING_NOW);
    assert!(rendered.contains("1. Allow once"));
    assert!(rendered.contains("2. Always allow access to /work"));
    assert!(rendered.contains("3. Deny — tell the agent why"));
    assert!(rendered.contains("press ctrl+c again to quit"));
    assert!(!rendered.contains("select · enter confirm"));
    assert!(!rendered.contains("esc back (never answers)"));
    assert_golden("chat_quit_armed_panel_narrow", &rendered);
}

/// The working wireframe: thinking marker, running Bash, the reserved
/// queue-preview blank, the 1 Hz working line (spinner + elapsed from the
/// prompt row), and the D2 gate stated with a kept draft.
#[test]
fn chat_working() {
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer_mut()
        .insert_str("and please document it");
    let rendered = render_frame(&working_model(), &view, 80, 20, WORKING_NOW);
    assert_golden("chat_working", &rendered);
}

/// Scrolled back mid-history: the paused rule with new-entry count and
/// position percent, the working line still visible (scroll never hides
/// it), the draft preserved while reading.
#[test]
fn chat_scrolled_back() {
    let mut msgs = working_msgs();
    // More prompts land while the user reads: the honest counter's food.
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        (0..6u32)
            .map(|n| {
                prompt_row(
                    100 + n,
                    "2026-08-12T09:10:15Z",
                    &format!("follow-up question {n}"),
                )
            })
            .collect(),
    ));
    let model = fold(msgs);
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.composer_mut()
            .insert_str("draft preserved while reading");
        chat.set_scroll(amux_tui::chat::FeedScroll::Paused {
            top_line: 3,
            entry_watermark: amux_tui::chat::entry_watermark(&model, agent_id()).saturating_sub(3),
        });
    }
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_scrolled_back", &rendered);
}

/// A truncated window scrolled to its very top: the rule row states the
/// honest boundary (`─ earlier history unavailable ─`).
#[test]
fn chat_truncated_top() {
    let mut msgs = base_msgs();
    msgs.extend(opened(true));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        900,
        vec![
            ready_row(),
            prompt_row(
                1,
                "2026-08-12T09:00:00Z",
                "what does the dispatcher do on a seq mismatch?",
            ),
            assistant_text_row(
                2,
                "2026-08-12T09:00:10Z",
                "msg_21",
                "It refuses the write and requests a dump — dispatch.rs::guard_seq.",
                Some("end_turn"),
            ),
            turn_duration_row(3, "2026-08-12T09:00:12Z", 12_000),
            prompt_row(4, "2026-08-12T09:01:00Z", "and on replay?"),
            assistant_text_row(
                5,
                "2026-08-12T09:01:10Z",
                "msg_22",
                "Replay folds but never executes Effects; the differential property proves it.",
                Some("end_turn"),
            ),
            turn_duration_row(6, "2026-08-12T09:01:12Z", 10_000),
        ],
    ));
    let model = fold(msgs);
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .set_scroll(amux_tui::chat::FeedScroll::Paused {
            top_line: 0,
            entry_watermark: amux_tui::chat::entry_watermark(&model, agent_id()),
        });
    let rendered = render_frame(&model, &view, 80, 18, IDLE_NOW);
    assert_golden("chat_truncated_top", &rendered);
}

/// Mid-replay: the loading band where the feed will be — visually
/// distinct from an empty chat (B10).
#[test]
fn chat_loading() {
    let mut msgs = base_msgs();
    msgs.push(Msg::Stream {
        agent: agent_id(),
        event: StreamMsg::Opened { truncated: false },
    });
    // Rows are replaying; ReplayComplete has not arrived.
    msgs.push(batch(
        "2026-08-12T09:00:01Z",
        1,
        vec![prompt_row(
            1,
            "2026-08-12T08:59:00Z",
            "earlier prompt still replaying",
        )],
    ));
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 16, IDLE_NOW);
    assert_golden("chat_loading", &rendered);
}

/// A fresh session: no transcript file exists yet — an empty chat with
/// the placeholder composer and no band (B10).
#[test]
fn chat_empty() {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 16, IDLE_NOW);
    assert_golden("chat_empty", &rendered);
}

/// An optimistic echo awaiting its transcript row: the prompt renders
/// immediately with the dim `sending…` marker, and the footer states the
/// in-flight gate (one echo at a time).
#[test]
fn chat_echo_sending() {
    let mut msgs = idle_msgs();
    msgs.push(Msg::Command {
        op: op(1),
        command: Command::Claude(amux_ui::ClaudeCommand::SendPrompt {
            agent: agent_id(),
            text: "and please add jitter docs".to_string(),
        }),
    });
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 22, IDLE_NOW);
    assert_golden("chat_echo_sending", &rendered);
}

/// A failed send: the finished op carries the fact, the draft resurfaces
/// from ViewState, and the footer states the failure until a keypress.
#[test]
fn chat_send_failure() {
    let mut msgs = idle_msgs();
    let command = Command::Claude(amux_ui::ClaudeCommand::SendPrompt {
        agent: agent_id(),
        text: "and please add jitter docs".to_string(),
    });
    msgs.push(Msg::Command {
        op: op(1),
        command: command.clone(),
    });
    msgs.push(Msg::OpResult {
        op: op(1),
        outcome: amux_ui::OpOutcome::Error {
            error: amux_ui::OpError {
                message: "input raced the session — it moved on before the keys landed".to_string(),
                auth_required: false,
                subscription_required: false,
            },
        },
    });
    let model = fold(msgs);
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.note_dispatched(op(1), &command);
        chat.reconcile(&model);
    }
    let rendered = render_frame(&model, &view, 80, 20, IDLE_NOW);
    assert_golden("chat_send_failure", &rendered);
}

/// The B2 markdown scope on one screen: headings keeping `#`, a list with
/// hanging wrap, a fenced code block, inline emphasis/code, a preformatted
/// table, and a URL that wraps whole.
#[test]
fn chat_markdown() {
    let source = "## Approach\n\
        \n\
        1. Wrap Client::reconnect in retry_with_backoff\n\
        - exponential base 200 ms, cap 5 s, jitter bounded and documented for reviewers\n\
        \n\
        Inline `RetryConfig` moves, **bold claims**, and *asides* survive.\n\
        \n\
        ```rust\n\
        let cfg  =  RetryConfig::default();\n\
        ```\n\
        \n\
        | attempt | delay |\n\
        |---------|-------|\n\
        | 1       | 200ms |\n\
        \n\
        Details: https://docs.rs/backoff/latest/backoff/index.html for the full derivation";
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(1, "2026-08-12T09:00:00Z", "explain the retry design"),
            assistant_text_row(
                2,
                "2026-08-12T09:00:30Z",
                "msg_31",
                source,
                Some("end_turn"),
            ),
            turn_duration_row(3, "2026-08-12T09:00:32Z", 32_000),
        ],
    ));
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 30, IDLE_NOW);
    assert_golden("chat_markdown", &rendered);
}

/// Marker and status entries on one screen: redacted and chain-broken
/// thinking, an interruption with its inferred (`~`) turn rule, an API
/// error entry, an unrecognized row, a task notification, a compaction
/// rule with token counts, and the compact summary.
#[test]
fn chat_entries_edge() {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(1, "2026-08-12T09:00:00Z", "try the migration"),
            json!({
                "type": "assistant",
                "uuid": uuid_row(2),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:03Z",
                "message": {
                    "id": "msg_41",
                    "role": "assistant",
                    "content": [{"type": "redacted_thinking", "data": "…"}],
                    "stop_reason": null,
                },
            }),
            json!({
                "type": "user",
                "uuid": uuid_row(3),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:06Z",
                "message": {"role": "user", "content": [
                    {"type": "text", "text": "[Request interrupted by user]"}
                ]},
                "interruptedMessageId": "msg_41",
            }),
            // The chain broke at the interrupt: this thinking marker has
            // no honest duration.
            json!({
                "type": "assistant",
                "uuid": uuid_row(4),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:20Z",
                "message": {
                    "id": "msg_42",
                    "role": "assistant",
                    "content": [{"type": "thinking", "thinking": "…"}],
                    "stop_reason": "end_turn",
                },
            }),
            json!({
                "type": "assistant",
                "uuid": uuid_row(5),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:30Z",
                "isApiErrorMessage": true,
                "error": "server_error",
                "message": {
                    "id": "msg_43",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "API error — upstream 529"}],
                    "stop_reason": null,
                },
            }),
            json!({
                "type": "progress",
                "uuid": uuid_row(6),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:40Z",
            }),
            json!({
                "type": "user",
                "uuid": uuid_row(7),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:50Z",
                "message": {"role": "user", "content": "Background task finished: tests are green"},
                "origin": {"kind": "task-notification"},
            }),
            json!({
                "type": "system",
                "subtype": "compact_boundary",
                "uuid": uuid_row(8),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:01:00Z",
                "compactMetadata": {"trigger": "manual", "preTokens": 31641, "postTokens": 1795},
            }),
            json!({
                "type": "user",
                "uuid": uuid_row(9),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:01:05Z",
                "isCompactSummary": true,
                "message": {"role": "user", "content": "The session added retry with backoff and began the migration."},
            }),
        ],
    ));
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 26, IDLE_NOW);
    assert_golden("chat_entries_edge", &rendered);
}

/// Tool-line edges on one screen: a Write-create magnitude, a typed
/// denial, a failure, collapsed question answers, an approved plan with
/// its truncated preview, sync and background subagents, and an orphan
/// result from outside the window.
#[test]
fn chat_tools_edge() {
    let plan = (1..=9)
        .map(|n| format!("step {n}: do the thing"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("acceptEdits"),
            prompt_row(1, "2026-08-12T09:00:00Z", "apply the plan"),
            tool_use_row(
                2,
                "2026-08-12T09:00:05Z",
                "msg_51",
                vec![tool_use("toolu_51", "Write", json!({"file_path": "plans/x.md"}))],
            ),
            tool_result_row(
                3,
                "2026-08-12T09:00:06Z",
                vec![tool_result("toolu_51", "ok")],
                Some(json!({
                    "filePath": "plans/x.md",
                    "structuredPatch": [],
                    "type": "create",
                    "content": (0..20).map(|n| format!("l{n}")).collect::<Vec<_>>().join("\n"),
                })),
            ),
            tool_use_row(
                4,
                "2026-08-12T09:00:10Z",
                "msg_52",
                vec![tool_use("toolu_52", "Bash", json!({"command": "rm -rf /tmp/scratch"}))],
            ),
            json!({
                "type": "user",
                "uuid": uuid_row(5),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:12Z",
                "toolDenialKind": "user-rejected",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_52", "is_error": true,
                     "content": "The user doesn't want to proceed"}
                ]},
            }),
            tool_use_row(
                6,
                "2026-08-12T09:00:20Z",
                "msg_53",
                vec![tool_use("toolu_53", "Bash", json!({"command": "cargo test"}))],
            ),
            json!({
                "type": "user",
                "uuid": uuid_row(7),
                "sessionId": SESSION,
                "timestamp": "2026-08-12T09:00:22Z",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_53", "is_error": true,
                     "content": "error[E0308]: mismatched types\nmore detail"}
                ]},
            }),
            tool_use_row(
                8,
                "2026-08-12T09:00:30Z",
                "msg_54",
                vec![tool_use("toolu_54", "AskUserQuestion", json!({"questions": [
                    {"header": "Storage", "question": "Which stores should the migration cover?",
                     "multiSelect": true, "options": [
                        {"label": "trust store"}, {"label": "recorder dumps"}
                     ]}
                ]}))],
            ),
            tool_result_row(
                9,
                "2026-08-12T09:00:35Z",
                vec![tool_result("toolu_54", "ok")],
                Some(json!({
                    "questions": ["Which stores should the migration cover?"],
                    "answers": {"Which stores should the migration cover?": "trust store, recorder dumps"},
                })),
            ),
            tool_use_row(
                10,
                "2026-08-12T09:00:40Z",
                "msg_55",
                vec![tool_use("toolu_55", "ExitPlanMode", json!({"plan": plan, "planFilePath": "~/.claude/plans/x.md"}))],
            ),
            tool_result_row(
                11,
                "2026-08-12T09:00:45Z",
                vec![tool_result("toolu_55", "User has approved your plan.")],
                Some(json!({"plan": plan, "filePath": "~/.claude/plans/x.md"})),
            ),
            tool_use_row(
                12,
                "2026-08-12T09:00:50Z",
                "msg_56",
                vec![
                    tool_use("toolu_56", "Task", json!({"description": "audit the trust store", "subagent_type": "Explore"})),
                    tool_use("toolu_57", "Task", json!({"description": "long refactor", "run_in_background": true})),
                ],
            ),
            tool_result_row(
                13,
                "2026-08-12T09:00:55Z",
                vec![tool_result("toolu_56", "done")],
                Some(json!({"status": "completed", "agentId": "a1", "totalDurationMs": 34000, "totalToolUseCount": 12})),
            ),
            tool_result_row(
                14,
                "2026-08-12T09:00:56Z",
                vec![tool_result("toolu_57", "launched")],
                Some(json!({"status": "async_launched", "agentId": "a2"})),
            ),
            // An orphan result: its tool_use fell outside the window.
            tool_result_row(
                15,
                "2026-08-12T09:00:58Z",
                vec![tool_result("toolu_00", "late output from evicted history")],
                None,
            ),
            turn_duration_row(16, "2026-08-12T09:01:00Z", 60_000),
        ],
    ));
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 40, IDLE_NOW);
    assert_golden("chat_tools_edge", &rendered);
}

/// A pending permission ask docks its panel over the composer area (C1):
/// the header says needs-you, the draft survives untouched beneath
/// (D1 — invisible while docked, back the moment the ask resolves).
#[test]
fn chat_needs_you() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![edit_hook(
            "sync/config.rs",
            "pub struct RetryConfig {\n    pub max_attempts: u8,\n    pub base_delay: Duration,\n}",
            "pub struct RetryConfig {\n    pub max_attempts: u8,        // capped at 6\n    pub jitter_ms: u16,\n    pub base_delay: Duration,\n}",
        )],
    ));
    let model = fold(msgs);
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.composer_mut().insert_str("wait — use a u16");
        chat.reconcile(&model);
    }
    let rendered = render_frame(&model, &view, 80, 22, WORKING_NOW);
    assert_golden("chat_needs_you", &rendered);
}

/// A multiline draft auto-grows the composer (1–6 rows).
#[test]
fn chat_composer_multiline() {
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer_mut()
        .insert_str("first line of the draft\nsecond line\nthird line");
    let rendered = render_frame(&idle_model(), &view, 80, 20, IDLE_NOW);
    assert_golden("chat_composer_multiline", &rendered);
}

// --- ask panels (C1/C2) -----------------------------------------------------

/// The C2 wireframe: an Edit permission panel with the ask-time
/// numberless mini-diff (leading context dropped to one row, the
/// remainder line stating the arithmetic), suggestion-derived option 2,
/// and the honest `(1 of 2)` with a second ask queued.
#[test]
fn chat_ask_permission_edit() {
    let mut msgs = edit_ask_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:21Z",
        41,
        vec![hook_row("Bash", json!({"command": "cargo fmt"}), 1)],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_ask_permission_edit", &rendered);
}

/// Wide graphemes wrap inside the mini-diff with a blank gutter — the
/// sign column never lies about rows and the border never drifts.
#[test]
fn chat_ask_permission_edit_cjk() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![edit_hook(
            "sync/リトライ.rs",
            "fn 設定() {\n    let 上限 = 3;\n}",
            "fn 設定() {\n    let 上限 = 6; // 指数バックオフの再試行回数の上限をここで決める — 長い行は折り返して右枠を壊さないこと\n}",
        )],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let buffer = render_buffer(&model, &view, 80, 22, Theme::Dark, WORKING_NOW);
    for y in 0..22u16 {
        let symbol = buffer.cell((79, y)).expect("border cell").symbol();
        assert!(
            matches!(symbol, "│" | "┐" | "┘"),
            "row {y} must end with a border cell, got {symbol:?}"
        );
    }
    assert_golden("chat_ask_permission_edit_cjk", &buffer_text(&buffer));
}

/// An edit that only adds the missing final newline: without the
/// jsdiff/git `\ No newline at end of file` marker the -/+ pair would be
/// visually identical and the user could not tell what they are
/// approving.
#[test]
fn chat_ask_permission_newline() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![edit_hook("sync/.env", "VALUE=1", "VALUE=1\n")],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_ask_permission_newline", &rendered);
}

/// A Write ask: `+` block head with `(N lines)` — create-vs-overwrite is
/// unknowable before the tool runs and the header claims neither.
#[test]
fn chat_ask_permission_write() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "Write",
            json!({
                "file_path": "sync/retry.rs",
                "content": "use std::time::Duration;\n\npub struct RetryPolicy {\n    pub max_attempts: u8,\n    pub base_delay: Duration,\n    pub jitter_ms: u16,\n    pub cap: Duration,\n    pub budget: u32,\n    pub on_exhausted: Behavior,\n    pub telemetry: bool,\n    pub docs: &'static str,\n}",
            }),
            1,
        )],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_ask_permission_write", &rendered);
}

/// A Bash ask: the `$ command` body.
#[test]
fn chat_ask_permission_bash() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "Bash",
            json!({"command": "cargo test -p amux-sync -- --nocapture"}),
            1,
        )],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_ask_permission_bash", &rendered);
}

/// An unknown tool: the compact typed fallback — header identity, no
/// body, the same three actions (C2 covers unknown tools too).
#[test]
fn chat_ask_permission_fallback() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "mcp__deploy__release",
            json!({"environment": "staging"}),
            1,
        )],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 18, WORKING_NOW);
    assert_golden("chat_ask_permission_fallback", &rendered);
}

/// A permission menu shape no capture verified (two suggestions): the
/// panel renders read-only-style with the typed refusal stated —
/// consistent with the encoder, which would refuse the same shape (C2).
fn unverified_edit_ask_msgs() -> Vec<Msg> {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "Edit",
            json!({"file_path": "sync/config.rs", "old_string": "a\n", "new_string": "b\n"}),
            2,
        )],
    ));
    msgs
}

#[test]
fn chat_ask_permission_unverified() {
    let model = fold(unverified_edit_ask_msgs());
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_ask_permission_unverified", &rendered);
}

#[test]
fn unverified_menu_reader_is_readable_but_pager_only() {
    let model = fold(unverified_edit_ask_msgs());
    assert!(
        amux_ui::claude::allows_answer(&model, agent_id()),
        "session classification keeps the ask authoritative; encoding owns menu safety"
    );
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('f'));
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);

    assert!(
        rendered.contains("sync/config.rs"),
        "the artifact remains readable: {rendered}"
    );
    assert!(
        rendered.contains("j/k scroll") && rendered.contains("q close"),
        "the reader retains pager affordances: {rendered}"
    );
    assert!(
        !rendered.contains("select") && !rendered.contains("enter confirm"),
        "an unverified menu must expose no answer actions: {rendered}"
    );
}

/// Deny opens the optional one-line feedback stage (C2): Enter with empty
/// text is a plain deny; the typed text rides the same program.
#[test]
fn chat_ask_deny_feedback() {
    let model = fold(edit_ask_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('3'));
    press(&mut view, &model, KeyCode::Enter);
    type_text(&mut view, &model, "use a u16 instead");
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_ask_deny_feedback", &rendered);
}

/// The optimistic collapse (C5): a dispatched answer collapses the panel
/// to the pending marker until the transcript confirms.
#[test]
fn chat_ask_pending() {
    let mut msgs = edit_ask_msgs();
    msgs.push(Msg::Command {
        op: op(2),
        command: Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
            agent: agent_id(),
            ask: 0,
            answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
        }),
    });
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 18, WORKING_NOW);
    assert_golden("chat_ask_pending", &rendered);
}

/// A failed send resurfaces the ask with the failure stated verbatim —
/// never a stuck spinner (C5).
#[test]
fn chat_ask_send_failed() {
    let mut msgs = edit_ask_msgs();
    msgs.push(Msg::Command {
        op: op(2),
        command: Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
            agent: agent_id(),
            ask: 0,
            answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
        }),
    });
    msgs.push(Msg::OpResult {
        op: op(2),
        outcome: amux_ui::OpOutcome::Error {
            error: amux_ui::OpError {
                message: "input raced the session — it moved on before the keys landed".to_string(),
                auth_required: false,
                subscription_required: false,
            },
        },
    });
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_ask_send_failed", &rendered);
}

// --- question forms (C4) ----------------------------------------------------

fn question_hook(questions: Value) -> Value {
    hook_row("AskUserQuestion", json!({"questions": questions}), 1)
}

fn single_question_msgs() -> Vec<Msg> {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![question_hook(json!([
            {"header": "Color", "question": "Which color do you prefer?",
             "multiSelect": false, "options": [
                {"label": "Red", "description": "warm, loud"},
                {"label": "Blue", "description": "cool, calm"},
             ]}
        ]))],
    ));
    msgs
}

/// The two-question wireframe fixture: a multi-select with descriptions
/// plus a single-select, so the tab row and submit step exist.
fn tabbed_question_msgs() -> Vec<Msg> {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![question_hook(json!([
            {"header": "storage", "question": "Which stores should the migration cover?",
             "multiSelect": true, "options": [
                {"label": "trust store", "description": "pairing + relay trust records"},
                {"label": "session index", "description": "bounded tail metadata"},
                {"label": "recorder dumps", "description": "panic-hook recordings"},
             ]},
            {"header": "rollout", "question": "How should it roll out?",
             "multiSelect": false, "options": [
                {"label": "all at once"},
                {"label": "store by store"},
             ]}
        ]))],
    ));
    msgs
}

/// A single question, single-select: numbered options with dim
/// descriptions, no tab row — Enter on the selection submits (C4).
#[test]
fn chat_ask_question_single() {
    let model = fold(single_question_msgs());
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 18, WORKING_NOW);
    assert_golden("chat_ask_question_single", &rendered);
}

/// The C4 wireframe: tab row with the current tab starred, `[x]`
/// checkboxes toggled with Space, the appended `Other…`.
#[test]
fn chat_ask_question_tabs() {
    let model = fold(tabbed_question_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char(' ')); // toggle trust store
    press(&mut view, &model, KeyCode::Down);
    press(&mut view, &model, KeyCode::Down);
    press(&mut view, &model, KeyCode::Char(' ')); // toggle recorder dumps
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_ask_question_tabs", &rendered);
}

/// The `Other…` inline free-text field, open with typed text (C4: one
/// free-text idiom, not two).
#[test]
fn chat_ask_question_other() {
    let model = fold(single_question_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('3')); // the appended Other row
    type_text(&mut view, &model, "a warm ochre");
    let rendered = render_frame(&model, &view, 80, 18, WORKING_NOW);
    assert_golden("chat_ask_question_other", &rendered);
}

/// The submit tab's review list: answered questions summarized,
/// unanswered ones in error color; Enter submits only when complete (C4).
#[test]
fn chat_ask_question_review() {
    let model = fold(tabbed_question_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char(' ')); // answer storage
    press(&mut view, &model, KeyCode::Enter); // advance to rollout
    press(&mut view, &model, KeyCode::Tab); // skip to submit, rollout unanswered
    let rendered = render_frame(&model, &view, 80, 18, WORKING_NOW);
    assert_golden("chat_ask_question_review", &rendered);
}

// --- plan review (C3/B6) ----------------------------------------------------

fn plan_ask_msgs() -> Vec<Msg> {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "ExitPlanMode",
            json!({"plan": wireframe_plan(), "planFilePath": "~/.claude/plans/retry.md"}),
            0,
        )],
    ));
    msgs
}

/// A plan-review ask opens the READER directly (C3): fullscreen plan,
/// position indicator, the three-way action row.
#[test]
fn chat_plan_reader() {
    let model = fold(plan_ask_msgs());
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_plan_reader", &rendered);
}

/// Esc drops the plan reader to the docked panel form: truncated plan,
/// the same three actions, `f` back to the full reader.
#[test]
fn chat_plan_docked() {
    let model = fold(plan_ask_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Esc);
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_plan_docked", &rendered);
}

/// Request-changes swaps the reader's action row for the mandatory
/// feedback stage (C3: will not submit empty; `q` types here — P2).
#[test]
fn chat_plan_reader_feedback() {
    let model = fold(plan_ask_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('3'));
    press(&mut view, &model, KeyCode::Enter);
    type_text(&mut view, &model, "sequence the rollout store by store");
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_plan_reader_feedback", &rendered);
}

/// An accepted-plans model: two approved plans retained as session state
/// (B6), reopenable after resolution.
fn accepted_plans_msgs() -> Vec<Msg> {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    let plan_b = wireframe_plan();
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(1, "2026-08-12T09:00:00Z", "plan the retry work"),
            tool_use_row(
                2,
                "2026-08-12T09:00:10Z",
                "msg_81",
                vec![tool_use(
                    "toolu_81",
                    "ExitPlanMode",
                    json!({"plan": "# earlier plan\n\n- step one\n- step two"}),
                )],
            ),
            tool_result_row(
                3,
                "2026-08-12T09:00:20Z",
                vec![tool_result("toolu_81", "User has approved your plan.")],
                Some(json!({"plan": "# earlier plan\n\n- step one\n- step two"})),
            ),
            tool_use_row(
                4,
                "2026-08-12T09:00:30Z",
                "msg_82",
                vec![tool_use(
                    "toolu_82",
                    "ExitPlanMode",
                    json!({"plan": plan_b}),
                )],
            ),
            tool_result_row(
                5,
                "2026-08-12T09:00:40Z",
                vec![tool_result("toolu_82", "User has approved your plan.")],
                Some(json!({"plan": plan_b})),
            ),
            turn_duration_row(6, "2026-08-12T09:00:50Z", 50_000),
        ],
    ));
    msgs
}

/// Ctrl+T reopens the newest accepted plan in the reader — no action row
/// once resolved; ←/→ steps between plans when several exist (B6).
#[test]
fn chat_plan_resolved_reader() {
    let model = fold(accepted_plans_msgs());
    let mut view = reconciled_view(&model);
    {
        let chat = view.chat.as_mut().expect("chat open");
        amux_tui::chat::handle_chat_key(
            chat,
            &model,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            (80, 24),
            at(IDLE_NOW),
        );
    }
    press(&mut view, &model, KeyCode::Left); // step to the older plan
    let rendered = render_frame(&model, &view, 80, 24, IDLE_NOW);
    assert_golden("chat_plan_resolved_reader", &rendered);
}

// --- the reader over diffs and files ----------------------------------------

/// `f` on an Edit ask opens the full diff in the reader: ask-time
/// numberless form, action row live (§4.3).
#[test]
fn chat_reader_diff_ask() {
    let model = fold(edit_ask_msgs());
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('f'));
    let rendered = render_frame(&model, &view, 80, 24, WORKING_NOW);
    assert_golden("chat_reader_diff_ask", &rendered);
}

/// `f` on a Write ask: the numbered `+` block (shared with Diff's create
/// case).
#[test]
fn chat_reader_newfile() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![hook_row(
            "Write",
            json!({"file_path": "sync/retry.rs", "content": "use std::time::Duration;\n\npub struct RetryPolicy {\n    pub max_attempts: u8,\n}"}),
            1,
        )],
    ));
    let model = fold(msgs);
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('f'));
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_reader_newfile", &rendered);
}

/// The post-hoc numbered form (`structuredPatch` restated): absolute
/// numbers walked from the hunk starts, replaced pairs repeating theirs,
/// `⋮` between hunks. Rendered through the same pure body renderer the
/// reader mounts — no Model path reaches this form in V1 (post-hoc diff
/// reading is a recorded door), so the body is golden-locked directly.
fn numbered_diff() -> DiffArtifact {
    DiffArtifact {
        numbering: DiffNumbering::Absolute,
        magnitude: DiffMagnitude::Fact {
            added: 4,
            removed: 2,
        },
        hunks: vec![
            DiffHunk {
                old_start: 14,
                new_start: 14,
                lines: vec![
                    " pub struct RetryConfig {".to_string(),
                    "-    pub max_attempts: u8,".to_string(),
                    "+    pub max_attempts: u8,        // capped at 6".to_string(),
                    "+    pub jitter_ms: u16,".to_string(),
                    "     pub base_delay: Duration,".to_string(),
                    " }".to_string(),
                ],
            },
            DiffHunk {
                old_start: 64,
                new_start: 65,
                lines: vec![
                    " impl SyncOptions {".to_string(),
                    "-    pub fn defaults() -> Self { Self { retries: 3 } }".to_string(),
                    "+    pub fn defaults() -> Self {".to_string(),
                    "+        Self { retries: RetryConfig::default() }".to_string(),
                    "+    }".to_string(),
                    " }".to_string(),
                ],
            },
        ],
    }
}

fn diff_body_buffer(theme: Theme) -> ratatui::buffer::Buffer {
    let lines = amux_tui::chat::diff::reader_rows(&numbered_diff(), 72, theme);
    let backend = TestBackend::new(72, lines.len() as u16);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(lines.clone()),
                frame.area(),
            )
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

#[test]
fn chat_reader_diff_numbered() {
    assert_golden(
        "chat_reader_diff_numbered",
        &buffer_text(&diff_body_buffer(Theme::Dark)),
    );
}

#[test]
fn chat_reader_diff_styles_dark() {
    assert_golden(
        "chat_reader_diff_styles_dark",
        &buffer_styles(&diff_body_buffer(Theme::Dark)),
    );
}

#[test]
fn chat_reader_diff_styles_light() {
    assert_golden(
        "chat_reader_diff_styles_light",
        &buffer_styles(&diff_body_buffer(Theme::Light)),
    );
}

// --- read-only chats (F1) ---------------------------------------------------

fn readonly_msgs() -> Vec<Msg> {
    let mut msgs = base_msgs_readonly(true);
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            prompt_row(1, "2026-08-12T09:00:00Z", "fix the flaky teardown"),
            assistant_text_row(
                2,
                "2026-08-12T09:01:00Z",
                "msg_91",
                "The flake is a teardown race in the testnet harness; serializing the shutdown.",
                Some("end_turn"),
            ),
            tool_use_row(
                3,
                "2026-08-12T09:01:10Z",
                "msg_92",
                vec![tool_use(
                    "toolu_91",
                    "Bash",
                    json!({"command": "cargo test -p amux --test spec"}),
                )],
            ),
            tool_result_row(
                4,
                "2026-08-12T09:01:20Z",
                vec![tool_result(
                    "toolu_91",
                    "2 failed: token_refresh, expired_session",
                )],
                None,
            ),
            turn_duration_row(5, "2026-08-12T09:01:30Z", 90_000),
        ],
    ));
    msgs
}

/// The read-only chat: same feed, `⊘ read-only` where the composer would
/// be, pager hints, no composer, no interrupt (F1 — write affordances
/// absent, not disabled).
#[test]
fn chat_readonly() {
    let model = fold(readonly_msgs());
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 18, IDLE_NOW);
    assert_golden("chat_readonly", &rendered);
}

/// A pending ask in a read-only chat renders as a fact panel: the
/// identical preview, the honest wait, read affordances only — and the
/// header says needs OWNER, not needs you.
#[test]
fn chat_readonly_ask() {
    let mut msgs = readonly_msgs();
    msgs.push(batch(
        "2026-08-12T09:02:10Z",
        20,
        vec![
            prompt_row(10, "2026-08-12T09:02:05Z", "serialize the shutdown"),
            edit_hook(
                "testnet/harness.rs",
                "fn teardown() {\n    kill();\n}",
                "fn teardown() {\n    drain();\n    kill();\n}",
            ),
        ],
    ));
    let model = fold(msgs);
    let view = reconciled_view(&model);
    let rendered = render_frame(&model, &view, 80, 22, IDLE_NOW);
    assert_golden("chat_readonly_ask", &rendered);
}

/// `f` in the read-only chat opens the reader with NO action row — read
/// affordances only (F1).
#[test]
fn chat_readonly_reader() {
    let mut msgs = readonly_msgs();
    msgs.push(batch(
        "2026-08-12T09:02:10Z",
        20,
        vec![
            prompt_row(10, "2026-08-12T09:02:05Z", "serialize the shutdown"),
            edit_hook(
                "testnet/harness.rs",
                "fn teardown() {\n    kill();\n}",
                "fn teardown() {\n    drain();\n    kill();\n}",
            ),
        ],
    ));
    let model = fold(msgs);
    let mut view = reconciled_view(&model);
    press(&mut view, &model, KeyCode::Char('f'));
    let rendered = render_frame(&model, &view, 80, 20, IDLE_NOW);
    assert_golden("chat_readonly_reader", &rendered);
}

// --- ask panel style maps ---------------------------------------------------

#[test]
fn chat_ask_permission_styles_dark() {
    let model = fold(edit_ask_msgs());
    let view = reconciled_view(&model);
    let styles = buffer_styles(&render_buffer(
        &model,
        &view,
        80,
        24,
        Theme::Dark,
        WORKING_NOW,
    ));
    assert_golden("chat_ask_permission_styles_dark", &styles);
}

#[test]
fn chat_ask_permission_styles_light() {
    let model = fold(edit_ask_msgs());
    let view = reconciled_view(&model);
    let styles = buffer_styles(&render_buffer(
        &model,
        &view,
        80,
        24,
        Theme::Light,
        WORKING_NOW,
    ));
    assert_golden("chat_ask_permission_styles_light", &styles);
}

// --- themes (style maps lock the token application) -------------------------

#[test]
fn chat_idle_styles_dark() {
    let styles = buffer_styles(&render_buffer(
        &idle_model(),
        &chat_view(),
        80,
        20,
        Theme::Dark,
        IDLE_NOW,
    ));
    assert_golden("chat_idle_styles_dark", &styles);
}

#[test]
fn chat_idle_styles_light() {
    let styles = buffer_styles(&render_buffer(
        &idle_model(),
        &chat_view(),
        80,
        20,
        Theme::Light,
        IDLE_NOW,
    ));
    assert_golden("chat_idle_styles_light", &styles);
}

#[test]
fn chat_markdown_styles_dark() {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            prompt_row(1, "2026-08-12T09:00:00Z", "explain"),
            assistant_text_row(
                2,
                "2026-08-12T09:00:30Z",
                "msg_61",
                "## Head\n`code` and **bold**\n```\nfenced\n```",
                Some("end_turn"),
            ),
            turn_duration_row(3, "2026-08-12T09:00:32Z", 5_000),
        ],
    ));
    let model = fold(msgs);
    let styles = buffer_styles(&render_buffer(
        &model,
        &chat_view(),
        80,
        18,
        Theme::Dark,
        IDLE_NOW,
    ));
    assert_golden("chat_markdown_styles_dark", &styles);
}

#[test]
fn chat_markdown_styles_light() {
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            prompt_row(1, "2026-08-12T09:00:00Z", "explain"),
            assistant_text_row(
                2,
                "2026-08-12T09:00:30Z",
                "msg_61",
                "## Head\n`code` and **bold**\n```\nfenced\n```",
                Some("end_turn"),
            ),
            turn_duration_row(3, "2026-08-12T09:00:32Z", 5_000),
        ],
    ));
    let model = fold(msgs);
    let styles = buffer_styles(&render_buffer(
        &model,
        &chat_view(),
        80,
        18,
        Theme::Light,
        IDLE_NOW,
    ));
    assert_golden("chat_markdown_styles_light", &styles);
}

/// Wide graphemes (CJK, emoji) and combining marks measure in display
/// cells, not codepoints: wrapping stays inside the frame and the right
/// border never drifts out of the last column.
#[test]
fn chat_unicode_width() {
    let source = "指数バックオフを実装しました。テストは全部緑です。🎉 café noe\u{0308}l bleibt ausgerichtet.\n\n- 日本語の箇条書き項目が折り返しでも右枠を壊さないことを確認するための長い行です";
    let mut msgs = base_msgs();
    msgs.extend(opened(false));
    msgs.push(batch(
        "2026-08-12T09:02:00Z",
        1,
        vec![
            ready_row(),
            mode_row("default"),
            prompt_row(
                1,
                "2026-08-12T09:00:00Z",
                "レビューコメントを反映して、リトライ処理を直してください",
            ),
            assistant_text_row(
                2,
                "2026-08-12T09:00:30Z",
                "msg_71",
                source,
                Some("end_turn"),
            ),
            turn_duration_row(3, "2026-08-12T09:00:32Z", 9_000),
        ],
    ));
    let model = fold(msgs);
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer_mut()
        .insert_str("繁体字と emoji 🚀 のドラフト");
    let buffer = render_buffer(&model, &view, 80, 22, Theme::Dark, IDLE_NOW);
    for y in 0..22u16 {
        let symbol = buffer.cell((79, y)).expect("border cell").symbol();
        assert!(
            matches!(symbol, "│" | "┐" | "┘"),
            "row {y} must end with a border cell, got {symbol:?}"
        );
    }
    assert_golden("chat_unicode", &buffer_text(&buffer));
}

/// Both themes lay out identically — tokens change styles, never cells.
#[test]
fn themes_agree_on_text_layout() {
    let model = idle_model();
    let view = chat_view();
    let dark = buffer_text(&render_buffer(&model, &view, 80, 20, Theme::Dark, IDLE_NOW));
    let light = buffer_text(&render_buffer(
        &model,
        &view,
        80,
        20,
        Theme::Light,
        IDLE_NOW,
    ));
    assert_eq!(dark, light);
}

/// Stability: two renders of the same fixture are identical.
#[test]
fn chat_frames_are_stable_across_runs() {
    let model = working_model();
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer_mut()
        .insert_str("draft");
    let first = render_frame(&model, &view, 80, 20, WORKING_NOW);
    let second = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_eq!(first, second);
}

/// No viewport size may panic the chat renderer, and on every layout-
/// viable size the frame stays inside the viewport: the composer's growth
/// is clamped so the footer and the bottom border survive at every
/// height (the feed gives way first).
#[test]
fn chat_rendering_never_panics_at_any_viewport_size() {
    /// The renderer's layout minimums (below them: the too-small notice).
    const MIN_WIDTH: u16 = 24;
    const MIN_HEIGHT: u16 = 10;
    let model = fold(idle_msgs());
    let working = working_model();
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.composer_mut().insert_str("draft\nwith lines");
        chat.set_scroll(amux_tui::chat::FeedScroll::Paused {
            top_line: 2,
            entry_watermark: 0,
        });
    }
    // Panel, reader, and read-only states join the sweep: their bottom
    // blocks clamp (tail kept, body rows give way) so the footer and
    // border survive every height.
    let ask = fold(edit_ask_msgs());
    let questions = fold(tabbed_question_msgs());
    let plan = fold(plan_ask_msgs());
    let readonly = fold({
        let mut msgs = readonly_msgs();
        msgs.push(batch(
            "2026-08-12T09:02:10Z",
            20,
            vec![edit_hook("harness.rs", "a\n", "b\n")],
        ));
        msgs
    });
    let mut docked_plan_view = reconciled_view(&plan);
    press(&mut docked_plan_view, &plan, KeyCode::Esc);
    let mut help_view = chat_view();
    help_view.chat.as_mut().expect("chat open").set_help(true);
    let states: Vec<(&Model, ViewState, &str)> = vec![
        (&model, view.clone(), IDLE_NOW),
        (&working, view, WORKING_NOW),
        (&ask, reconciled_view(&ask), WORKING_NOW),
        (&questions, reconciled_view(&questions), WORKING_NOW),
        (&plan, reconciled_view(&plan), WORKING_NOW),
        (&plan, docked_plan_view, WORKING_NOW),
        (&readonly, reconciled_view(&readonly), IDLE_NOW),
        (&model, help_view, IDLE_NOW),
    ];

    for width in 1..=120u16 {
        for height in 1..=40u16 {
            for (model, view, now) in &states {
                let rendered = render_frame(model, view, width, height, now);
                if width >= MIN_WIDTH && height >= MIN_HEIGHT {
                    let lines: Vec<&str> = rendered.lines().collect();
                    assert_eq!(lines.len(), height as usize);
                    assert!(
                        lines[height as usize - 1].starts_with('└'),
                        "bottom border survives at {width}x{height}"
                    );
                    assert!(
                        lines[height as usize - 2].starts_with('│'),
                        "footer row survives at {width}x{height}"
                    );
                }
            }
        }
    }
}
