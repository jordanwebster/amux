//! Tier-3 golden frames: the renderer is a pure function of
//! (Model, ViewState, FrameContext), so render one frame from a fixture and
//! diff against the checked-in text. The frames match the aligned mockups
//! in the TUI V1 spec verbatim. No network, no clocks, no flake.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test golden`
//! and review the diff like code.

use amux_tui::view::{Mode, UiAction, ViewState};
use amux_tui::{FrameContext, Theme, render};
use amux_ui::{
    Agent, AgentId, AgentParent, Command, DisconnectReason, HostEntry, HostId, Model, Msg, OpId,
    ServerMsg, StreamCloseReason, StreamEntry, StreamMsg, WorkingOn, update,
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
        kind: match agent_type {
            "claude" => amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
            "codex" => amux_ui::AgentKind::Codex,
            other => panic!("unsupported fixture kind {other}"),
        },
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
        parent: None,
        working_on: None,
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

fn render_buffer(
    model: &Model,
    view: &ViewState,
    width: u16,
    height: u16,
    theme: Theme,
) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (width, height),
        theme,
        now: at(NOW),
    };
    terminal
        .draw(|frame| render(model, view, &ctx, frame))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_frame(model: &Model, view: &ViewState, width: u16, height: u16) -> String {
    let buffer = render_buffer(model, view, width, height, Theme::default());
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).expect("cell in area").symbol());
        }
        out.push('\n');
    }
    out
}

/// One class letter per cell: what the text goldens cannot see. Same
/// classes the chat style goldens use.
fn buffer_styles(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell in area").style();
            out.push(match style.fg {
                Some(Color::Red) => 'r',
                Some(Color::Yellow) => 'y',
                Some(Color::Green) => 'g',
                Some(Color::DarkGray) => 'a',
                _ if style.add_modifier.contains(Modifier::DIM) => 'd',
                _ => '.',
            });
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

/// The armed quit guard (chrome-wide Ctrl+C, CHAT.md §Keybindings): the
/// status line's hints are replaced by `press ctrl+c again to quit` in
/// warning color; a fresh second press quits, anything else disarms.
#[test]
fn fleet_quit_armed() {
    let mut view = view_default();
    view.quit_guard.press(at(NOW));
    let rendered = render_frame(&fleet_model(), &view, 68, 11);
    assert_golden("fleet_quit_armed", &rendered);
}

/// A read-only agent surfaces in the fleet (CHAT.md A3): the normal row
/// idioms with `read-only` as its resting status word. The entry keys
/// open it in chat only — raw attach is absent, not disabled.
#[test]
fn fleet_readonly_row() {
    let mut msgs = fleet_msgs();
    let mut captured = an_agent("ci-triage", "claude", "nova");
    captured.readonly = true;
    msgs.push(agent_up(&captured));
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 12);
    assert_golden("fleet_readonly_row", &rendered);
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
            agent_type: amux_ui::AgentType::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
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
                subscription_required: false,
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

/// A missing cloud subscription is a degraded banner over the working local
/// fleet, with account recovery guidance.
#[test]
fn fleet_cloud_subscription_banner() {
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
                message: "Cloud subscription required".to_string(),
                auth_required: false,
                subscription_required: true,
            },
        },
    });
    let model = fold(msgs);
    let view = ViewState {
        dismissed_error_seq: u64::MAX,
        ..view_default()
    };
    let rendered = render_frame(&model, &view, 68, 11);
    assert_golden("fleet_cloud_subscription_banner", &rendered);
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
                agent_type: amux_ui::AgentType::Claude {
                    driver: amux_ui::ClaudeDriver::Pty,
                },
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
            agent_type: amux_ui::AgentType::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
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
    let rendered = render_frame(&fleet_model(), &view, 68, 21);
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

// --- families in the fleet -----------------------------------------------

/// A child agent: the same row as any other, plus the edge its owning
/// daemon recorded. Nothing else about a child is special.
fn a_child(name: &str, agent_type: &str, on: &str, parent: &str) -> Agent {
    Agent {
        parent: Some(AgentParent {
            agent_id: agent_id(parent),
            host_id: host_id(on),
        }),
        ..an_agent(name, agent_type, on)
    }
}

fn working_on(agent: &mut Agent, text: &str, said_at: i64) {
    agent.working_on = Some(WorkingOn {
        text: text.to_string(),
        updated_at: at(said_at),
    });
}

/// The canonical fleet plus one three-deep family under `refactor-tunnels`:
/// a child asking for permission, a grandchild under it, and an idle
/// sibling saying what it is on. The family's loudest attention is the
/// grandchild's, which is the point — a folded row must show it.
fn family_msgs() -> Vec<Msg> {
    let mut msgs = fleet_msgs();
    let mut lead = an_agent("refactor-tunnels", "claude", "nova");
    working_on(&mut lead, "split the tunnel supervisor", NOW - 900);
    msgs.push(agent_up(&lead));

    let mut scribe = a_child("write-the-docs", "claude", "nova", "refactor-tunnels");
    working_on(&mut scribe, "document the new handshake", NOW - 240);
    msgs.push(agent_up(&scribe));

    let mut runner = a_child("test-runner", "codex", "nova", "refactor-tunnels");
    working_on(&mut runner, "run the tunnel suite end to end", NOW - 60);
    msgs.push(agent_up(&runner));

    msgs.push(agent_up(&a_child(
        "flake-hunter",
        "codex",
        "nova",
        "test-runner",
    )));

    msgs.extend(stream_rows(
        "write-the-docs",
        NOW - 300,
        vec![ready_row(), prompt_row(5), stop_row()],
    ));
    msgs.extend(stream_rows(
        "test-runner",
        NOW - 30,
        vec![ready_row(), prompt_row(6)],
    ));
    msgs.extend(stream_rows(
        "flake-hunter",
        NOW - 20,
        vec![ready_row(), prompt_row(7), permission_row()],
    ));
    msgs
}

fn family_model() -> Model {
    fold(family_msgs())
}

fn expanded_view(names: &[&str]) -> ViewState {
    ViewState {
        expanded: names.iter().map(|name| agent_id(name)).collect(),
        ..view_default()
    }
}

/// Folded: the family is ONE row wearing the loudest badge anywhere inside
/// it and a `⋯3` marker for what it stands in for, and `working_on` shows
/// with the age of the claim.
#[test]
fn a2a_fleet_family_folded() {
    let rendered = render_frame(&family_model(), &view_default(), 80, 14);
    assert_golden("a2a_fleet_family_folded", &rendered);
}

/// Open: the parent keeps its own badge, descendants indent one step per
/// generation, and the family still occupies one place in the ranking.
#[test]
fn a2a_fleet_family_open() {
    let view = expanded_view(&["refactor-tunnels"]);
    let rendered = render_frame(&family_model(), &view, 80, 14);
    assert_golden("a2a_fleet_family_open", &rendered);
}

/// 60 columns: `working_on` collapses first, the status word second, and
/// the family marker survives both — it is structure, not decoration.
#[test]
fn a2a_fleet_family_60col() {
    let view = expanded_view(&["refactor-tunnels"]);
    let rendered = render_frame(&family_model(), &view, 60, 14);
    assert_golden("a2a_fleet_family_60col", &rendered);
}

/// The fleet's styles are fixed rather than themed (see `Theme`), so these
/// two goldens are the standing proof that a light terminal gets exactly
/// the frame a dark one does — including the family badge.
#[test]
fn a2a_fleet_family_styles_dark() {
    let view = expanded_view(&["refactor-tunnels"]);
    let styles = buffer_styles(&render_buffer(&family_model(), &view, 80, 14, Theme::Dark));
    assert_golden("a2a_fleet_family_styles_dark", &styles);
}

#[test]
fn a2a_fleet_family_styles_light() {
    let view = expanded_view(&["refactor-tunnels"]);
    let styles = buffer_styles(&render_buffer(&family_model(), &view, 80, 14, Theme::Light));
    assert_golden("a2a_fleet_family_styles_light", &styles);
}

/// The fold key opens the family under the cursor and shuts it again;
/// shutting from a descendant leaves the cursor on the row that swallowed
/// it, never on whatever slid into that index.
#[test]
fn a2a_fleet_fold_key_opens_and_shuts_the_family() {
    let model = family_model();
    let mut view = view_default();
    let top = amux_tui::view::visible_rows(&model, &view)
        .iter()
        .position(|row| row.display_name() == "refactor-tunnels")
        .expect("the folded family row");
    view.selected = top;

    amux_tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));
    let open = amux_tui::view::visible_rows(&model, &view);
    assert_eq!(open.len(), 8, "every descendant is a row while open");

    // Put the cursor on the deepest descendant, then shut from there.
    view.selected = open
        .iter()
        .position(|row| row.display_name() == "flake-hunter")
        .expect("the grandchild is visible while open");
    amux_tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));

    let shut = amux_tui::view::visible_rows(&model, &view);
    assert_eq!(shut.len(), 5, "the family is one row again");
    assert_eq!(
        shut[view.selected].display_name(),
        "refactor-tunnels",
        "the cursor follows the fold up to the row that swallowed it"
    );
}

/// A filter searches every agent: nothing hides behind a fold from a name
/// the human typed.
#[test]
fn a2a_fleet_filter_never_hides_behind_a_fold() {
    let model = family_model();
    let view = view_default();
    assert!(
        !amux_tui::view::visible_rows(&model, &view)
            .iter()
            .any(|row| row.display_name() == "flake-hunter"),
        "the grandchild is folded away with nothing typed"
    );

    let filtered = ViewState {
        filter: "flake".to_string(),
        ..view_default()
    };
    let rows = amux_tui::view::visible_rows(&model, &filtered);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].display_name(), "flake-hunter");
}

/// An agent in no family has nothing to fold, and the key says so by
/// changing nothing.
#[test]
fn a2a_fleet_fold_key_is_inert_on_a_childless_row() {
    let model = family_model();
    let mut view = view_default();
    view.selected = amux_tui::view::visible_rows(&model, &view)
        .iter()
        .position(|row| row.display_name() == "docs-cleanup")
        .expect("a childless row");
    let before = amux_tui::view::visible_rows(&model, &view).len();
    amux_tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));
    assert_eq!(amux_tui::view::visible_rows(&model, &view).len(), before);
    assert!(view.expanded.is_empty());
}

/// `working_on` renders with the age of the claim, and is absent — not
/// filled with something invented — for an agent that never said.
#[test]
fn a2a_fleet_working_on_states_the_claim_and_its_age() {
    let view = expanded_view(&["refactor-tunnels"]);
    let rendered = render_frame(&family_model(), &view, 80, 14);
    assert!(
        rendered.contains("run the t… 1m"),
        "the claim, clipped to the room left over, then how long ago it was made:\n{rendered}"
    );
    let silent = rendered
        .lines()
        .find(|line| line.contains("flake-hunter"))
        .expect("the grandchild row");
    let cell: String = silent.chars().skip(65).take(78 - 65).collect();
    assert!(
        cell.trim().is_empty(),
        "an agent that said nothing gets an empty cell, not a guess: {cell:?}"
    );
}

/// The family with `test-runner` dead of a Windows access violation, still
/// wearing the work it claimed before it died: nobody cleared the claim,
/// because nobody was left to clear it.
fn exited_with_work_model() -> Model {
    let mut msgs = family_msgs();
    msgs.push(Msg::Stream {
        agent: agent_id("test-runner"),
        event: StreamMsg::Closed {
            reason: StreamCloseReason::AgentExited {
                exit_code: Some(-1_073_741_819),
            },
        },
    });
    fold(msgs)
}

/// The status cell holds a closed set of words on a good day and an
/// operating system's exit code on a bad one. It is clipped to its column
/// like every other cell on the row, so a long code cannot write itself
/// over the work the agent claimed before it died.
#[test]
fn a2a_fleet_exited_status_stays_in_its_column() {
    let view = expanded_view(&["refactor-tunnels"]);
    let rendered = render_frame(&exited_with_work_model(), &view, 80, 14);
    let row = rendered
        .lines()
        .find(|line| line.contains("test-runner"))
        .expect("the exited row");
    assert!(
        row.contains("exited(-1…"),
        "the code is clipped rather than allowed to run on: {row}"
    );
    assert!(
        row.contains("run the t…"),
        "and the work claim keeps its own column: {row}"
    );
    assert_golden("fleet_exited_with_work", &rendered);
}

fn press(key: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char(key),
        crossterm::event::KeyModifiers::NONE,
    )
}

// --- deleting a family (U6) ----------------------------------------------

/// Put the cursor on a row and press `d`.
fn confirming_delete(model: &Model, name: &str) -> ViewState {
    confirming_delete_in(model, view_default(), name)
}

fn confirming_delete_in(model: &Model, mut view: ViewState, name: &str) -> ViewState {
    view.selected = amux_tui::view::visible_rows(model, &view)
        .iter()
        .position(|row| row.display_name() == name)
        .unwrap_or_else(|| panic!("{name} is a visible row"));
    amux_tui::keys::handle_key(&mut view, model, press('d'), 20, at(NOW));
    view
}

/// The confirmation names the whole subtree, not the row the cursor was
/// on: the family was ONE row on screen, and the delete takes everything
/// that row was standing in for.
#[test]
fn a2a_delete_confirm_lists_the_whole_subtree() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    let rendered = render_frame(&model, &view, 80, 14);
    assert!(
        rendered.contains("deleting refactor-tunnels also deletes the 3 agents under it:"),
        "{rendered}"
    );
    for name in ["write-the-docs", "test-runner", "flake-hunter"] {
        assert!(rendered.contains(name), "{name} is named:\n{rendered}");
    }
    assert!(
        rendered.contains("delete refactor-tunnels? y/n"),
        "the prompt and its keys are where they have always been:\n{rendered}"
    );
}

/// Which of them is mid-task is the fact worth having: it is flagged on
/// the row and counted at the foot, with what it says it is doing, so the
/// warning is actionable rather than merely alarming.
#[test]
fn a2a_delete_confirm_flags_the_working_ones() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    let rendered = render_frame(&model, &view, 80, 14);
    let runner = rendered
        .lines()
        .find(|line| line.contains("test-runner"))
        .expect("the working child's row");
    assert!(runner.contains('●'), "flagged: {runner}");
    assert!(runner.contains("working"), "and said so: {runner}");
    assert!(
        runner.contains("run the tunnel suite end to end"),
        "with what it is on: {runner}"
    );

    let idle = rendered
        .lines()
        .find(|line| line.contains("write-the-docs"))
        .expect("the idle child's row");
    assert!(!idle.contains('●'), "an idle child carries no flag: {idle}");

    assert!(
        rendered.contains("1 is working — deleting stops it"),
        "and the count is stated once, at the foot:\n{rendered}"
    );
}

/// Listed, not blocking: idle children cost no extra keystroke, and the
/// one `y` the confirmation has always taken still deletes.
#[test]
fn a2a_delete_confirm_does_not_block_on_idle_children() {
    let model = family_model();
    let mut view = confirming_delete(&model, "refactor-tunnels");
    let action = amux_tui::keys::handle_key(&mut view, &model, press('y'), 20, at(NOW));
    assert_eq!(
        action,
        Some(UiAction::Dispatch(Command::DeleteAgent {
            agent: agent_id("refactor-tunnels")
        })),
        "one press, exactly as for an agent that started nobody"
    );
}

/// A working child is flagged, never refused: the person is looking
/// straight at the list, which is a better guard than a second prompt.
#[test]
fn a2a_delete_confirm_flags_a_working_child_without_refusing() {
    let model = family_model();
    let mut view =
        confirming_delete_in(&model, expanded_view(&["refactor-tunnels"]), "test-runner");
    let rendered = render_frame(&model, &view, 80, 14);
    assert!(rendered.contains("flake-hunter"), "{rendered}");
    let action = amux_tui::keys::handle_key(&mut view, &model, press('y'), 20, at(NOW));
    assert_eq!(
        action,
        Some(UiAction::Dispatch(Command::DeleteAgent {
            agent: agent_id("test-runner")
        }))
    );
}

/// Nothing changes for an agent that started nobody: no list, because
/// there is nothing the human cannot already see.
#[test]
fn a2a_delete_confirm_keeps_the_fleet_for_a_childless_agent() {
    let model = family_model();
    let view = confirming_delete(&model, "docs-cleanup");
    let rendered = render_frame(&model, &view, 80, 14);
    assert!(rendered.contains("delete docs-cleanup? y/n"), "{rendered}");
    assert!(
        !rendered.contains("also deletes"),
        "no cascade to describe:\n{rendered}"
    );
    assert!(
        rendered.contains("refactor-tunnels"),
        "and the fleet is still on screen:\n{rendered}"
    );
}

/// On a viewport too short for the list, the confirmation says how many
/// names it could not show. Silently dropping one would be the single
/// thing this screen must not do.
#[test]
fn a2a_delete_confirm_counts_what_it_could_not_show() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    let rendered = render_frame(&model, &view, 80, 13);
    assert!(
        rendered.contains("… and 2 more"),
        "the elision counts what is behind it:\n{rendered}"
    );
}

#[test]
fn a2a_delete_confirm_frame() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    assert_golden("a2a_delete_confirm", &render_frame(&model, &view, 80, 14));
}

/// The flag and the warning are the only color on the screen; the rest of
/// the list reads as list.
#[test]
fn a2a_delete_confirm_styles() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    let styles = buffer_styles(&render_buffer(&model, &view, 80, 14, Theme::Dark));
    assert_golden("a2a_delete_confirm_styles", &styles);
}

// --- hints tell the truth about the family keys ---------------------------

/// The fold key joins the status-line hints where something folds, and
/// stays out of them where nothing does.
#[test]
fn a2a_bindings_hint_the_fold_key_only_with_a_family_on_screen() {
    let with_family = render_frame(&family_model(), &view_default(), 80, 14);
    assert!(
        with_family.contains("z fold"),
        "the hint row names it:\n{with_family}"
    );

    let flat = render_frame(&fold(fleet_msgs()), &view_default(), 80, 14);
    assert!(
        flat.contains("n new") && !flat.contains("z fold"),
        "nothing folds here, so nothing says it does:\n{flat}"
    );
}

/// A narrow terminal keeps the hints it always had rather than losing the
/// whole row to make space for one more: the `?` overlay is where the
/// full list lives.
#[test]
fn a2a_bindings_keep_the_hint_row_when_the_fold_key_will_not_fit() {
    let narrow = render_frame(&family_model(), &view_default(), 68, 14);
    assert!(
        narrow.contains("n new") && narrow.contains("? help"),
        "the row survives:\n{narrow}"
    );
    assert!(
        !narrow.contains("z fold"),
        "without pretending it fits:\n{narrow}"
    );
}

/// The fleet's `?` overlay follows the same rule as its hint row.
#[test]
fn a2a_bindings_fleet_overlay_lists_the_fold_key_only_with_a_family() {
    let helped = ViewState {
        mode: Mode::Help,
        ..view_default()
    };
    let with_family = render_frame(&family_model(), &helped, 80, 24);
    assert!(with_family.contains("open/shut a family"), "{with_family}");

    let flat = render_frame(&fold(fleet_msgs()), &helped, 80, 24);
    assert!(
        !flat.contains("open/shut a family"),
        "no family, no row:\n{flat}"
    );
}
