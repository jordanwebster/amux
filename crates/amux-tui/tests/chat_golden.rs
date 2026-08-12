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
use amux_ui::{
    Agent, AgentId, Command, HostEntry, HostId, Model, Msg, OpId, ServerMsg, StreamEntry,
    StreamMsg, update,
};
use chrono::{DateTime, Utc};
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
    let agent = Agent {
        id: agent_id(),
        host_id: host_id(),
        name: Some("fix-auth".to_string()),
        command: "claude".to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        agent_type: "claude".to_string(),
        io_protocols: vec![
            "claude_raw_v1".to_string(),
            "claude_pty_transcript_v1".to_string(),
        ],
        readonly: false,
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
        chat: Some(ChatView::open(agent_id())),
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

/// The working wireframe: thinking marker, running Bash, the reserved
/// queue-preview blank, the 1 Hz working line (spinner + elapsed from the
/// prompt row), and the D2 gate stated with a kept draft.
#[test]
fn chat_working() {
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer
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
        chat.composer.insert_str("draft preserved while reading");
        chat.scroll = amux_tui::chat::FeedScroll::Paused {
            top_line: 3,
            entry_watermark: amux_tui::chat::entry_watermark(&model, agent_id()).saturating_sub(3),
        };
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
    view.chat.as_mut().expect("chat open").scroll = amux_tui::chat::FeedScroll::Paused {
        top_line: 0,
        entry_watermark: amux_tui::chat::entry_watermark(&model, agent_id()),
    };
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
        command: Command::SendPrompt {
            agent: agent_id(),
            text: "and please add jitter docs".to_string(),
        },
    });
    let rendered = render_frame(&fold(msgs), &chat_view(), 80, 22, IDLE_NOW);
    assert_golden("chat_echo_sending", &rendered);
}

/// A failed send: the finished op carries the fact, the draft resurfaces
/// from ViewState, and the footer states the failure until a keypress.
#[test]
fn chat_send_failure() {
    let mut msgs = idle_msgs();
    let command = Command::SendPrompt {
        agent: agent_id(),
        text: "and please add jitter docs".to_string(),
    };
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

/// A pending permission ask: the header says needs-you (the panel itself
/// is Phase 5), and the footer gates the kept draft honestly.
#[test]
fn chat_needs_you() {
    let mut msgs = working_msgs();
    msgs.push(batch(
        "2026-08-12T09:10:20Z",
        40,
        vec![json!({
            "type": "hook.permission_request",
            "tool_name": "Edit",
            "tool_input": {"file_path": "sync/config.rs", "old_string": "a", "new_string": "b"},
            "permission_mode": "default",
        })],
    ));
    let model = fold(msgs);
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer
        .insert_str("wait — use a u16");
    let rendered = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_golden("chat_needs_you", &rendered);
}

/// A multiline draft auto-grows the composer (1–6 rows).
#[test]
fn chat_composer_multiline() {
    let mut view = chat_view();
    view.chat
        .as_mut()
        .expect("chat open")
        .composer
        .insert_str("first line of the draft\nsecond line\nthird line");
    let rendered = render_frame(&idle_model(), &view, 80, 20, IDLE_NOW);
    assert_golden("chat_composer_multiline", &rendered);
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
        .composer
        .insert_str("draft");
    let first = render_frame(&model, &view, 80, 20, WORKING_NOW);
    let second = render_frame(&model, &view, 80, 20, WORKING_NOW);
    assert_eq!(first, second);
}

/// No viewport size may panic the chat renderer: sweep every plausible
/// terminal size across the richest states (working + scrolled + draft).
#[test]
fn chat_rendering_never_panics_at_any_viewport_size() {
    let model = fold(idle_msgs());
    let working = working_model();
    let mut view = chat_view();
    {
        let chat = view.chat.as_mut().expect("chat open");
        chat.composer.insert_str("draft\nwith lines");
        chat.scroll = amux_tui::chat::FeedScroll::Paused {
            top_line: 2,
            entry_watermark: 0,
        };
    }
    for width in 1..=120u16 {
        for height in 1..=40u16 {
            let _ = render_frame(&model, &view, width, height, IDLE_NOW);
            let _ = render_frame(&working, &view, width, height, WORKING_NOW);
        }
    }
}
