//! Tier-3 golden frames: the renderer is a pure function of
//! (Model, ViewState, FrameContext), so render one frame from a fixture and
//! diff against the checked-in text. The frames match the aligned mockups
//! in the TUI V1 spec verbatim. No network, no clocks, no flake.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test golden`
//! and review the diff like code.

use amux_tui::view::{Mode, ViewState};
use amux_tui::{FrameContext, Theme, render};
use amux_ui::{
    Agent, AgentId, Command, DisconnectReason, HostEntry, HostId, Model, Msg, OpId, ServerMsg,
    StreamEntry, StreamMsg, update,
};
use chrono::{DateTime, TimeDelta, Utc};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use uuid::Uuid;

// --- fixture builders (mirroring the amux-ui spec harness) ----------------

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture epoch")
}

fn at(seconds: i64) -> DateTime<Utc> {
    t0() + TimeDelta::seconds(seconds)
}

/// The frame's "now": all fixture ages are relative to this.
const NOW: i64 = 4000;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn host_id(name: &str) -> HostId {
    Uuid::from_u128((1u128 << 64) | u128::from(fnv1a(name.as_bytes())))
}

fn agent_id(name: &str) -> AgentId {
    Uuid::from_u128((2u128 << 64) | u128::from(fnv1a(name.as_bytes())))
}

fn op(n: u8) -> OpId {
    OpId(Uuid::from_u128((3u128 << 64) | u128::from(n)))
}

fn a_host(name: &str) -> HostEntry {
    HostEntry {
        id: host_id(name),
        name: name.to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(amux_ui::Capabilities::default()),
        trust_status: amux_ui::HostTrustStatus::Trusted,
        last_dial_error: None,
    }
}

fn an_offline_host(name: &str) -> HostEntry {
    HostEntry {
        online: false,
        version: None,
        capabilities: None,
        last_dial_error: Some("dial tcp: connection refused".to_string()),
        ..a_host(name)
    }
}

fn an_agent(name: &str, agent_type: &str, on: &str) -> Agent {
    Agent {
        id: agent_id(name),
        host_id: host_id(on),
        name: Some(name.to_string()),
        command: agent_type.to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        agent_type: agent_type.to_string(),
        io_protocols: vec![
            "claude_raw_v1".to_string(),
            "claude_pty_transcript_v1".to_string(),
        ],
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
    }
}

fn fold(msgs: Vec<Msg>) -> Model {
    let mut model = Model::default();
    for msg in msgs {
        update(&mut model, msg);
    }
    model
}

fn server(msg: ServerMsg) -> Msg {
    Msg::Server(msg)
}

fn agent_up(agent: &Agent) -> Msg {
    server(ServerMsg::AgentUpserted {
        agent: agent.clone(),
    })
}

fn synced() -> Vec<Msg> {
    vec![
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
    ]
}

/// One live stream batch: open (complete window) then rows at `at_seconds`.
fn stream_rows(agent: &str, at_seconds: i64, rows: Vec<serde_json::Value>) -> Vec<Msg> {
    vec![
        Msg::Stream {
            agent: agent_id(agent),
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: agent_id(agent),
            event: StreamMsg::ReplayComplete,
        },
        Msg::Stream {
            agent: agent_id(agent),
            event: StreamMsg::Batch {
                at: at(at_seconds),
                entries: rows
                    .into_iter()
                    .enumerate()
                    .map(|(offset, payload)| StreamEntry {
                        seq: 2 + offset as u64,
                        payload,
                    })
                    .collect(),
            },
        },
    ]
}

/// The replay-complete marker: everything after is live. Attention over a
/// window that never reached it stays honestly Unknown, so every fixture
/// stream leads with it.
fn ready_row() -> serde_json::Value {
    serde_json::json!({"type": "amux.transcript_ready"})
}

/// A human prompt: the turn-start fact working/finished derive from.
fn prompt_row(n: u8) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "uuid": format!("dddddddd-0000-4000-8000-0000000000{n:02}"),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": "do the thing"},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn permission_row() -> serde_json::Value {
    serde_json::json!({
        "type": "hook.permission_request",
        "tool_name": "Bash",
        "tool_input": {"command": "echo probe"},
    })
}

/// A pending question is the AskUserQuestion permission-request hook —
/// routed on `tool_name`, never notification wording (CHAT.md E2).
fn question_row() -> serde_json::Value {
    serde_json::json!({
        "type": "hook.permission_request",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": []},
    })
}

fn stop_row() -> serde_json::Value {
    serde_json::json!({"type": "hook.stop"})
}

fn weak_row() -> serde_json::Value {
    serde_json::json!({"type": "summary", "summary": "compaction"})
}

/// The canonical five-agent fleet from the spec's fleet frame: attention
/// states and ages chosen to reproduce it exactly.
fn fleet_msgs() -> Vec<Msg> {
    let mut msgs = vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("hetzner"),
        }),
        server(ServerMsg::HostUpserted {
            host: an_offline_host("tessin"),
        }),
        agent_up(&an_agent("fix-auth-bug", "claude", "nova")),
        agent_up(&an_agent("migration-plan", "claude", "hetzner")),
        agent_up(&an_agent("nightly-refactor", "codex", "hetzner")),
        agent_up(&an_agent("refactor-tunnels", "claude", "nova")),
        agent_up(&an_agent("docs-cleanup", "claude", "nova")),
    ];
    msgs.extend(synced());
    msgs.extend(stream_rows(
        "fix-auth-bug",
        NOW - 120,
        vec![ready_row(), prompt_row(1), permission_row()],
    ));
    msgs.extend(stream_rows(
        "migration-plan",
        NOW - 45,
        vec![ready_row(), prompt_row(2), question_row()],
    ));
    msgs.extend(stream_rows(
        "nightly-refactor",
        NOW - 180,
        vec![ready_row(), prompt_row(3), stop_row()],
    ));
    msgs.extend(stream_rows(
        "refactor-tunnels",
        NOW - 12,
        vec![ready_row(), prompt_row(4)],
    ));
    msgs.extend(stream_rows(
        "docs-cleanup",
        NOW - 3600,
        vec![ready_row(), weak_row()],
    ));
    msgs
}

fn fleet_model() -> Model {
    fold(fleet_msgs())
}

// --- rendering ------------------------------------------------------------

fn render_frame(model: &Model, view: &ViewState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (width, height),
        theme: Theme::default(),
        now: at(NOW),
    };
    terminal
        .draw(|frame| render(model, view, &ctx, frame))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).expect("cell in area").symbol());
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

fn view_default() -> ViewState {
    ViewState::default()
}

// --- the named frames -----------------------------------------------------

#[test]
fn fleet_ranked() {
    let rendered = render_frame(&fleet_model(), &view_default(), 68, 11);
    assert_golden("fleet_ranked", &rendered);
}

/// Stability: two renders of the same fixture are identical.
#[test]
fn frames_are_stable_across_runs() {
    let model = fleet_model();
    let first = render_frame(&model, &view_default(), 68, 11);
    let second = render_frame(&model, &view_default(), 68, 11);
    assert_eq!(first, second);
}

/// The narrow-width pair locks the column-collapse rule: 80 columns keeps
/// the status word, 60 collapses it first.
#[test]
fn fleet_ranked_80col() {
    let rendered = render_frame(&fleet_model(), &view_default(), 80, 11);
    assert_golden("fleet_ranked_80col", &rendered);
}

#[test]
fn fleet_ranked_60col() {
    let rendered = render_frame(&fleet_model(), &view_default(), 60, 11);
    assert_golden("fleet_ranked_60col", &rendered);
}

/// Every badge glyph on one screen: ! ? ✓ ⋯ (blank) – ◌.
#[test]
fn fleet_attention_badges() {
    let mut msgs = fleet_msgs();
    msgs.push(agent_up(&an_agent("log-archaeology", "claude", "tessin")));
    msgs.push(Msg::Command {
        op: op(1),
        command: Command::CreateAgent {
            host: Some(host_id("nova")),
            name: "claude-4".to_string(),
            agent_type: amux_ui::AgentType::Claude,
            working_dir: std::path::PathBuf::from("/work"),
        },
    });
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 13);
    assert_golden("fleet_attention_badges", &rendered);
}

/// Rows on an offline host render dim with `–`/unknown, never a stale badge.
#[test]
fn fleet_offline_host_rows() {
    let mut msgs = fleet_msgs();
    // hetzner goes offline after the attention evidence arrived.
    msgs.push(server(ServerMsg::HostUpserted {
        host: an_offline_host("hetzner"),
    }));
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 11);
    assert_golden("fleet_offline_host_rows", &rendered);
}

/// Cloud-auth expiry is a degraded banner over a working fleet — never a
/// blocking screen.
#[test]
fn fleet_cloud_auth_banner() {
    let mut msgs = fleet_msgs();
    msgs.push(Msg::Command {
        op: op(1),
        command: Command::RenameAgent {
            agent: agent_id("migration-plan"),
            name: "plan".to_string(),
        },
    });
    msgs.push(Msg::OpResult {
        op: op(1),
        outcome: amux_ui::OpOutcome::Error {
            error: amux_ui::OpError {
                message: "Invalid or missing credentials".to_string(),
                auth_required: true,
            },
        },
    });
    let model = fold(msgs);
    let view = ViewState {
        // The op failure itself was seen and dismissed; the banner stays.
        dismissed_error_seq: u64::MAX,
        ..view_default()
    };
    let rendered = render_frame(&model, &view, 68, 11);
    assert_golden("fleet_cloud_auth_banner", &rendered);
}

#[test]
fn fleet_empty_no_agents() {
    let mut msgs = vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
    ];
    msgs.extend(synced());
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 11);
    assert_golden("fleet_empty_no_agents", &rendered);
}

/// Below the layout minimum (the right-info anchor at width−13) the frame
/// degrades to the too-small notice instead of underflowing the column grid.
#[test]
fn fleet_too_narrow() {
    let rendered = render_frame(&fleet_model(), &view_default(), 12, 11);
    assert_golden("fleet_too_narrow", &rendered);
}

/// No viewport size may panic the renderer: sweep the fleet across every
/// width and height a terminal could plausibly report. (Regression: widths
/// below the layout minimum underflowed the right-info column arithmetic.)
#[test]
fn rendering_never_panics_at_any_viewport_size() {
    let model = fleet_model();
    let view = view_default();
    for width in 1..=200u16 {
        for height in 1..=60u16 {
            let _ = render_frame(&model, &view, width, height);
        }
    }
}

/// A subscription-driven fleet shrink can leave ViewState's scroll and
/// selection pointing past the rows; render clamps the stale values against
/// the Model instead of drawing an empty, marker-less list until the next
/// keypress. The frame equals the one an in-range ViewState produces.
#[test]
fn stale_scroll_after_fleet_shrink_clamps_at_render() {
    let model = fleet_model(); // five rows, all fitting at height 11
    let stale = ViewState {
        selected: 12,
        scroll: 9,
        ..view_default()
    };
    let clamped = ViewState {
        selected: 4,
        scroll: 0,
        ..view_default()
    };
    let rendered = render_frame(&model, &stale, 68, 11);
    assert!(
        rendered.contains("▸"),
        "the selection marker survives the shrink"
    );
    assert_eq!(rendered, render_frame(&model, &clamped, 68, 11));
}

#[test]
fn fleet_daemon_starting() {
    let rendered = render_frame(&Model::default(), &view_default(), 68, 11);
    assert_golden("fleet_daemon_starting", &rendered);
}

#[test]
fn fleet_daemon_unreachable() {
    let model = fold(vec![server(ServerMsg::Disconnected {
        reason: DisconnectReason::TransportError {
            message: "connection refused".to_string(),
        },
    })]);
    let rendered = render_frame(&model, &view_default(), 68, 11);
    assert_golden("fleet_daemon_unreachable", &rendered);
}

/// Filter mode: typing narrows, the count shows matches, enter attaches.
#[test]
fn picker_filtered() {
    let view = ViewState {
        mode: Mode::Filter,
        filter: "auth".to_string(),
        ..view_default()
    };
    let rendered = render_frame(&fleet_model(), &view, 68, 11);
    assert_golden("picker_filtered", &rendered);
}

/// Inline rename edits in place; the cursor block marks the draft.
#[test]
fn row_rename_inline() {
    let view = ViewState {
        mode: Mode::Rename {
            agent: agent_id("refactor-tunnels"),
            draft: "refactor-tunnels".to_string(),
        },
        selected: 3,
        ..view_default()
    };
    let rendered = render_frame(&fleet_model(), &view, 68, 11);
    assert_golden("row_rename_inline", &rendered);
}

/// Delete confirmation lives in the status line.
#[test]
fn delete_confirm_statusline() {
    let view = ViewState {
        mode: Mode::ConfirmDelete {
            agent: agent_id("docs-cleanup"),
            name: "docs-cleanup".to_string(),
        },
        selected: 4,
        ..view_default()
    };
    let rendered = render_frame(&fleet_model(), &view, 68, 11);
    assert_golden("delete_confirm_statusline", &rendered);
}

/// A pending create renders an optimistic row; op failures surface in the
/// status line until a keypress dismisses them.
#[test]
fn op_pending_and_failed() {
    let mut msgs = vec![
        // Dispatched before the daemon came up: fails fast with the exact
        // status-line message.
        Msg::Command {
            op: op(1),
            command: Command::CreateAgent {
                host: Some(host_id("nova")),
                name: "claude-3".to_string(),
                agent_type: amux_ui::AgentType::Claude,
                working_dir: std::path::PathBuf::from("/work"),
            },
        },
    ];
    msgs.extend(fleet_msgs());
    msgs.push(Msg::Command {
        op: op(2),
        command: Command::CreateAgent {
            host: Some(host_id("nova")),
            name: "claude-4".to_string(),
            agent_type: amux_ui::AgentType::Claude,
            working_dir: std::path::PathBuf::from("/work"),
        },
    });
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 12);
    assert_golden("op_pending_and_failed", &rendered);
}

#[test]
fn help_overlay() {
    let view = ViewState {
        mode: Mode::Help,
        ..view_default()
    };
    let rendered = render_frame(&fleet_model(), &view, 68, 18);
    assert_golden("help_overlay", &rendered);
}

// --- style assertions (what the text goldens cannot see) ------------------

#[test]
fn badge_styles_and_offline_dim() {
    let mut msgs = fleet_msgs();
    msgs.push(server(ServerMsg::HostUpserted {
        host: an_offline_host("hetzner"),
    }));
    let model = fold(msgs);
    let view = view_default();
    let backend = TestBackend::new(68, 11);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (68, 11),
        theme: Theme::default(),
        now: at(NOW),
    };
    terminal
        .draw(|frame| render(&model, &view, &ctx, frame))
        .expect("draw");
    let buffer = terminal.backend().buffer();

    // Row 3 is the permission row: `!` badge is red.
    let badge = buffer.cell((4, 3)).expect("badge cell");
    assert_eq!(badge.symbol(), "!");
    assert_eq!(badge.style().fg, Some(Color::Red));

    // Offline-host rows render dim (name cell of an offline row).
    let offline_name = (0..11u16)
        .find_map(|y| {
            let cell = buffer.cell((6, y))?;
            (cell.symbol() == "m").then_some(buffer.cell((6, y)).expect("cell"))
        })
        .expect("migration-plan row present");
    assert!(
        offline_name.style().add_modifier.contains(Modifier::DIM),
        "offline rows are dim"
    );
}
