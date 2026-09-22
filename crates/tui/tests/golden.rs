//! Golden frames: the renderer is a pure function of
//! (Model, ViewState, FrameContext), so render one frame from a fixture and
//! diff against the checked-in text. The frames match the aligned mockups
//! in the TUI V1 spec verbatim. No network, no clocks, no flake.
//!
//! Regenerate with `UPDATE_GOLDENS=1 just test-crate tui -- --features fixtures --test golden`
//! and review the diff like code.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fold::{
    CommitResult, ExpectedHead, Generations, HeadState, JsonBytes, Loaded, MutationOracle,
    Placement, Stored,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tui::chat::FeedScroll;
use tui::chrome::{Chrome, ChromeConfig, TraceEvent};
use tui::replay::capture_frame;
use tui::view::{Mode, UiAction, ViewState, visible_rows};
use tui::{ColorMode, FrameContext, Theme, render};
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::{
    Agent, AgentId, AgentParent, Boundary, BoundaryAt, ChatCommand, ChatStreamMsg, CloudState,
    Command, DisconnectReason, Effect, FleetDelta, HostEntry, HostId, LoadedDto, Model, Msg,
    MutationBatchDto, OpId, ProfileGeneration, RelayCarrier, ReplayFactsDto, ReplayOutcomeDto,
    ServerMsg, StoreMsg, StoreOp, StreamCloseReason, StreamEntry, StreamMsg, Tier, WorkingOn,
    update,
};
use uuid::Uuid;

const GOLDEN_VIEWPORT: (u16, u16) = (120, 40);

// --- fixture builders (mirroring the ui-state spec harness) ---------------

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
        capabilities: Some(ui_state::Capabilities::default()),
        trust_status: ui_state::HostTrustStatus::Trusted,
        last_dial_error: None,
        via: ui_state::HostVia::Direct,
        signed_in: Some(true),
        platform: None,
    }
}

fn an_offline_host(name: &str) -> HostEntry {
    HostEntry {
        online: false,
        version: None,
        capabilities: None,
        last_dial_error: Some("dial tcp: connection refused".to_string()),
        via: ui_state::HostVia::Offline,
        platform: None,
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
            "claude" => ui_state::AgentKind::Claude {
                driver: ui_state::ClaudeDriver::Pty,
            },
            "codex" => ui_state::AgentKind::Codex,
            other => panic!("unsupported fixture kind {other}"),
        },
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
        last_activity: t0(),
        parent: None,
        working_on: None,
        summary: None,
        progress: None,
        inventory_revision: 0,
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
                    .map(|(offset, payload)| {
                        StreamEntry::observed(2 + offset as u64, at(at_seconds), payload)
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

fn stored_prompt_row(n: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "uuid": Uuid::from_u128(n as u128 + 1).to_string(),
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": format!("stored row {n:04}")},
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

/// The Codex rows with the same meaning as the Claude ones above. A Codex
/// agent's kind exposes only Codex protocols, so its fixture states have to
/// be reached through its own vocabulary.
fn codex_ready_row() -> serde_json::Value {
    serde_json::json!({"type": "amux.codex_ready"})
}

fn codex_turn_started_row(turn: &str) -> serde_json::Value {
    serde_json::json!({"type":"turn/started","turn":{"id":turn,"status":"inProgress"}})
}

fn codex_turn_completed_row(turn: &str) -> serde_json::Value {
    serde_json::json!({"type":"turn/completed","turn":{"id":turn,"status":"completed"}})
}

/// A Codex agent stopped on a command approval: the ask the fleet badges.
fn codex_approval_rows() -> Vec<serde_json::Value> {
    vec![
        codex_ready_row(),
        codex_turn_started_row("turn-approval"),
        serde_json::json!({"type":"item/started","item":{"id":"exec-1","type":"commandExecution",
            "command":"cargo test","cwd":"/work","status":"inProgress"}}),
        serde_json::json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1",
            "command":"cargo test","cwd":"/work","reason":"run tests?"}),
        serde_json::json!({"type":"amux.codex_approval_required","item_id":"exec-1","request_id":7,
            "availableDecisions":["accept","cancel"]}),
    ]
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
        server(ServerMsg::CloudState(CloudState::Connected {
            tier: Tier::Pro,
            carrier: RelayCarrier::Tcp,
        })),
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
        vec![
            codex_ready_row(),
            codex_turn_started_row("turn-nightly"),
            codex_turn_completed_row("turn-nightly"),
        ],
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

fn render_buffer_at(
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

fn render_buffer(
    model: &Model,
    view: &ViewState,
    _width: u16,
    _height: u16,
    theme: Theme,
) -> ratatui::buffer::Buffer {
    render_buffer_at(model, view, GOLDEN_VIEWPORT.0, GOLDEN_VIEWPORT.1, theme)
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

fn render_frame_at(model: &Model, view: &ViewState, width: u16, height: u16) -> String {
    let buffer = render_buffer_at(model, view, width, height, Theme::default());
    capture_frame(&buffer, Theme::default()).text
}

/// One class letter per cell: what the text goldens cannot see. The theme
/// itself names the class, so a cell painted from a colour literal instead
/// of a token shows up as `?` rather than passing for one. This is the
/// same serializer a captured report stores its frame with, so a golden
/// and a report frame are comparable without translation.
fn buffer_styles(buffer: &ratatui::buffer::Buffer, theme: Theme) -> String {
    capture_frame(buffer, theme).styles
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
    let rendered = render_frame_at(&fleet_model(), &view_default(), 80, 11);
    assert_golden("fleet_ranked_80col", &rendered);
}

#[test]
fn fleet_ranked_60col() {
    let rendered = render_frame_at(&fleet_model(), &view_default(), 60, 11);
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
            agent_type: ui_state::AgentType::Claude {
                driver: ui_state::ClaudeDriver::Pty,
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

#[test]
fn fleet_rows_draw_daemon_summary_freshness_and_incompatible_age() {
    let mut stale = an_agent("stale-agent", "claude", "nova");
    stale.summary = Some(ui_state::SummaryEnvelope {
        through: 7,
        producer_version: ui_state::summary_producer_version(
            ui_state::StructuredProtocol::ClaudePtyTranscript,
        ),
        observed_at: at(NOW - 20),
        stale: true,
        revision: 4,
        summary: ui_state::Summary {
            attention: ui_state::Attention::Working,
            phase: ui_state::AgentPhase::Running,
            last_activity: Some(at(NOW - 30)),
            todo: None,
            context: None,
            model: None,
            unknown: vec![ui_state::SummaryField::Todo],
        },
    });
    let mut foreign = an_agent("foreign-agent", "claude", "nova");
    foreign.summary = Some(ui_state::SummaryEnvelope {
        through: 8,
        producer_version: 99,
        observed_at: at(NOW - 120),
        stale: false,
        revision: 5,
        summary: stale.summary.as_ref().expect("summary").summary.clone(),
    });
    let rendered = render_frame(
        &fold(vec![
            server(ServerMsg::Connected {
                local_host_id: Some(host_id("nova")),
            }),
            server(ServerMsg::HostUpserted {
                host: a_host("nova"),
            }),
            agent_up(&stale),
            agent_up(&foreign),
        ]),
        &view_default(),
        120,
        10,
    );
    let text = rendered;
    assert!(text.contains("stale-agent"));
    assert!(text.contains("30s"));
    assert!(text.contains("stale"));
    assert!(text.contains("foreign-agent"));
    assert!(text.contains("2m"));
    assert!(text.contains("unknown"));
}

fn fixture_protocol(agent: &Agent) -> ui_state::StructuredProtocol {
    match agent.kind {
        ui_state::AgentKind::Claude {
            driver: ui_state::ClaudeDriver::Pty,
        } => ui_state::StructuredProtocol::ClaudePtyTranscript,
        ui_state::AgentKind::Claude {
            driver: ui_state::ClaudeDriver::Sdk,
        } => ui_state::StructuredProtocol::ClaudeSdk,
        ui_state::AgentKind::Codex => ui_state::StructuredProtocol::Codex,
        _ => panic!("fixture agent has no structured protocol"),
    }
}

fn empty_loaded_for(protocol: ui_state::StructuredProtocol) -> LoadedDto {
    const GENERATIONS: Generations = Generations {
        fleet: 1,
        chat: 1,
        provider: 1,
    };
    macro_rules! empty {
        ($fold:ty, $variant:ident) => {
            LoadedDto::$variant(Loaded::<$fold> {
                generations: GENERATIONS,
                fence: 0,
                content_revision: 0,
                segment_high_water: 0,
                head: HeadState::None,
                window: Vec::new(),
                boundaries: Vec::new(),
                first_page: None,
                aliases: Vec::new(),
                host: None,
                progress: None,
            })
        };
    }
    match protocol {
        ui_state::StructuredProtocol::ClaudePtyTranscript => {
            empty!(fold::claude_pty::ClaudeFold, Claude)
        }
        ui_state::StructuredProtocol::ClaudeSdk => {
            empty!(fold::claude_sdk::ClaudeSdkFold, ClaudeSdk)
        }
        ui_state::StructuredProtocol::Codex => empty!(fold::codex::CodexFold, Codex),
    }
}

fn install_commit_result(model: &mut Model, agent: AgentId, effects: Vec<Effect>) {
    let (attempt, op, mutations) = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Commit {
                attempt,
                op,
                mutations,
                ..
            }) => Some((attempt, op, mutations)),
            _ => None,
        })
        .expect("stream batch commits canonical rows");
    macro_rules! canonical {
        ($mutations:expr) => {{
            let mut oracle = MutationOracle::default();
            let placed = oracle.apply(&$mutations).expect("valid fixture mutations");
            let bodies = oracle
                .entries()
                .into_iter()
                .map(|entry| Stored {
                    key: entry.key,
                    segment: entry.segment,
                    order: entry.order,
                    revision: entry.revision,
                    entry: JsonBytes(
                        postcard::to_allocvec(&entry.entry).expect("fixture entry serializes"),
                    ),
                })
                .collect();
            (placed, bodies)
        }};
    }
    let (placed, bodies): (Vec<Placement>, Vec<Stored<JsonBytes>>) = match mutations {
        MutationBatchDto::Claude(mutations) => canonical!(mutations),
        MutationBatchDto::ClaudeSdk(mutations) => canonical!(mutations),
        MutationBatchDto::Codex(mutations) => canonical!(mutations),
    };
    update(
        model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent,
            result: CommitResult {
                expected: ExpectedHead::Present {
                    fence: 1,
                    version: 1,
                },
                content_revision: 1,
                placed,
                bodies,
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        }),
    );
}

fn stored_chat_model(replay_complete: bool) -> Model {
    let agent = an_agent("stored-chat", "claude", "nova");
    let protocol = fixture_protocol(&agent);
    let mut model = fold(vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
        agent_up(&agent),
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
    ]);
    let effects = update(&mut model, Msg::Chat(ChatCommand::Open { agent: agent.id }));
    let (attempt, load_op) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Load { attempt, op, .. }) => Some((*attempt, *op)),
            _ => None,
        })
        .expect("chat open loads the store");
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Loaded {
            profile: ProfileGeneration(0),
            attempt,
            op: load_op,
            agent: agent.id,
            loaded: Box::new(empty_loaded_for(protocol)),
        }),
    );
    let stream = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::OpenStoreStream { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .expect("stored chat opens a stream");
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 2,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: at(NOW - 10),
            },
        },
    );
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: at(NOW - 9),
                entries: vec![
                    StreamEntry::observed(1, at(NOW - 9), ready_row()),
                    StreamEntry::observed(2, at(NOW - 9), prompt_row(8)),
                ],
            },
        },
    );
    install_commit_result(&mut model, agent.id, effects);
    if replay_complete {
        update(
            &mut model,
            Msg::ChatStream {
                agent: agent.id,
                attempt: stream,
                event: ChatStreamMsg::ReplayComplete { at: at(NOW - 8) },
            },
        );
    }
    model
}

fn patch_chat(mut model: Model, patch: impl FnOnce(&mut serde_json::Value)) -> Model {
    let mut value = serde_json::to_value(&model).expect("model serializes");
    let chat = value
        .pointer_mut(&format!(
            "/store/chats/{}",
            agent_id("stored-chat")
                .to_string()
                .replace('~', "~0")
                .replace('/', "~1")
        ))
        .expect("stored chat in serialized model");
    patch(chat);
    model = serde_json::from_value(value).expect("patched model deserializes");
    model
}

fn chat_view(model: &Model) -> ViewState {
    let mut view = view_default();
    view.open_chat(model, agent_id("stored-chat"));
    view
}

fn boundary_chat_view(model: &Model) -> ViewState {
    let mut view = chat_view(model);
    view.chat
        .as_mut()
        .expect("chat open")
        .set_scroll(FeedScroll::Paused {
            top_line: 0,
            entry_watermark: 0,
        });
    view
}

fn durable_boundaries_model() -> Model {
    let model = stored_chat_model(true);
    let entry = model
        .chat(agent_id("stored-chat"))
        .and_then(|chat| chat.entries.first())
        .expect("stored prompt");
    let (segment, order, key) = entry.position();
    let boundaries = vec![
        BoundaryAt {
            segment,
            before: None,
            boundary: Boundary::Gap,
        },
        BoundaryAt {
            segment,
            before: Some((order, key.clone())),
            boundary: Boundary::VersionGap,
        },
        BoundaryAt {
            segment: segment + 1,
            before: None,
            boundary: Boundary::Evicted,
        },
    ];
    patch_chat(model, |chat| {
        chat["boundaries"] = serde_json::to_value(boundaries).expect("boundaries serialize");
    })
}

fn healthy_stored_chat_model(replay_complete: bool) -> Model {
    stored_chat_model(replay_complete)
}

fn provider_store_model(agent: Agent, rows: Vec<serde_json::Value>) -> Model {
    let protocol = fixture_protocol(&agent);
    let mut model = fold(vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
        agent_up(&agent),
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
    ]);
    let effects = update(&mut model, Msg::Chat(ChatCommand::Open { agent: agent.id }));
    let (attempt, load_op) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Load { attempt, op, .. }) => Some((*attempt, *op)),
            _ => None,
        })
        .expect("chat open loads the store");
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Loaded {
            profile: ProfileGeneration(0),
            attempt,
            op: load_op,
            agent: agent.id,
            loaded: Box::new(empty_loaded_for(protocol)),
        }),
    );
    let stream = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::OpenStoreStream { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .expect("stored chat opens a stream");
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: rows.len() as u64,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: at(NOW - 10),
            },
        },
    );
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: at(NOW - 9),
                entries: rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, payload)| {
                        StreamEntry::observed(index as u64 + 1, at(NOW - 9), payload)
                    })
                    .collect(),
            },
        },
    );
    install_commit_result(&mut model, agent.id, effects);
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: at(NOW - 8) },
        },
    );
    model
}

fn provider_store_rows(protocol: ui_state::StructuredProtocol) -> Vec<serde_json::Value> {
    match protocol {
        ui_state::StructuredProtocol::ClaudePtyTranscript => vec![
            ready_row(),
            prompt_row(41),
            serde_json::json!({"type":"assistant","uuid":"aaaaaaaa-0000-4000-8000-000000000041","timestamp":"2026-08-11T22:00:00.000Z","message":{"id":"m41","stop_reason":"tool_use","content":[{"type":"text","text":"I will inspect it."},{"type":"thinking","thinking":"checking"},{"type":"tool_use","id":"tool-41","name":"Bash","input":{"command":"echo stored"}}]}}),
            serde_json::json!({"type":"user","uuid":"bbbbbbbb-0000-4000-8000-000000000041","timestamp":"2026-08-11T22:00:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool-41","content":"stored result"}]}}),
            permission_row(),
        ],
        ui_state::StructuredProtocol::ClaudeSdk => vec![
            session_ready_row(),
            serde_json::json!({"type":"user","uuid":"sdk-user","message":{"content":"do the session thing"}}),
            serde_json::json!({"type":"assistant","uuid":"sdk-assistant","message":{"id":"sdk-message","stop_reason":"tool_use","content":[{"type":"text","text":"I will inspect it."},{"type":"thinking","thinking":"checking"},{"type":"tool_use","id":"sdk-tool","name":"Bash","input":{"command":"echo stored"}}]}}),
            serde_json::json!({"type":"user","uuid":"sdk-result","message":{"content":[{"type":"tool_result","tool_use_id":"sdk-tool","content":"stored result"}]}}),
            session_permission_row(),
        ],
        ui_state::StructuredProtocol::Codex => vec![
            codex_ready_row(),
            codex_turn_started_row("stored-turn"),
            serde_json::json!({"type":"item/completed","item":{"id":"stored-user","type":"userMessage","content":[{"type":"text","text":"do the Codex thing"}]}}),
            serde_json::json!({"type":"item/completed","item":{"id":"stored-reply","type":"agentMessage","text":"I will inspect it.","phase":"final_answer"}}),
            serde_json::json!({"type":"item/completed","item":{"id":"stored-command","type":"commandExecution","command":"echo stored","cwd":"/work","status":"completed","aggregatedOutput":"stored result","exitCode":0}}),
            serde_json::json!({"type":"item/commandExecution/requestApproval","itemId":"pending-command","command":"cargo test","cwd":"/work","reason":"verify it"}),
            serde_json::json!({"type":"amux.codex_approval_required","item_id":"pending-command","request_id":7,"availableDecisions":["accept","cancel"]}),
        ],
    }
}

fn provider_live_model(agent: Agent, rows: Vec<serde_json::Value>) -> Model {
    let id = agent.id;
    let protocol = fixture_protocol(&agent);
    let stored_rows = rows.clone();
    let mut messages = vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
        agent_up(&agent),
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: id,
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: id,
            event: StreamMsg::Batch {
                at: at(NOW - 9),
                entries: rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, payload)| {
                        StreamEntry::observed(index as u64 + 1, at(NOW - 9), payload)
                    })
                    .collect(),
            },
        },
    ];
    messages.push(Msg::Stream {
        agent: id,
        event: StreamMsg::ReplayComplete,
    });
    let mut model = fold(messages);
    tui::fixtures::install_static_store_rows_for(&mut model, id, protocol, stored_rows);
    model
}

#[test]
fn stored_provider_entries_match_live_provider_rendering_in_both_themes() {
    for (name, agent, protocol) in [
        (
            "claude",
            an_agent("stored-claude", "claude", "nova"),
            ui_state::StructuredProtocol::ClaudePtyTranscript,
        ),
        (
            "sdk",
            a_session_agent("stored-session", "nova"),
            ui_state::StructuredProtocol::ClaudeSdk,
        ),
        (
            "codex",
            an_agent("stored-codex", "codex", "nova"),
            ui_state::StructuredProtocol::Codex,
        ),
    ] {
        let agent_id = agent.id;
        let rows = provider_store_rows(protocol);
        let stored = provider_store_model(agent.clone(), rows.clone());
        let live = provider_live_model(agent, rows);
        for (theme_name, theme) in [
            ("dark", Theme::default()),
            ("light", Theme::light(ColorMode::TrueColor)),
        ] {
            let mut stored_view = view_default();
            stored_view.open_chat(&stored, agent_id);
            let stored_capture = store_state_golden(&stored, &stored_view, theme);
            let mut live_view = view_default();
            live_view.open_chat(&live, agent_id);
            let live_capture = store_state_golden(&live, &live_view, theme);
            assert_eq!(stored_capture, live_capture, "{name} {theme_name}");
            assert_golden(
                &format!("store_provider_{name}_{theme_name}"),
                &stored_capture,
            );
        }
    }
}

#[test]
fn stored_sdk_history_keeps_the_todo_and_never_answers_an_old_permission() {
    let agent = a_session_agent("stored-session-history", "nova");
    let agent_id = agent.id;
    let mut model = provider_store_model(
        agent,
        vec![
            serde_json::json!({"type":"amux.claude_sdk.history_begin"}),
            serde_json::json!({"type":"assistant","uuid":"todo-row","message":{"id":"todo-message","content":[{"type":"tool_use","id":"todo","name":"TodoWrite","input":{"todos":[{"content":"ship","activeForm":"shipping","status":"in_progress"}]}}]}}),
            serde_json::json!({"type":"user","uuid":"todo-result","message":{"content":[{"type":"tool_result","tool_use_id":"todo","content":"ok"}]}}),
            serde_json::json!({"type":"assistant","uuid":"old-permission","message":{"id":"old-message","content":[{"type":"tool_use","id":"old-write","name":"Write","input":{"file_path":"/tmp/old","content":"old"}}]}}),
            serde_json::json!({"type":"amux.claude_sdk.history_complete"}),
            serde_json::json!({"type":"amux.claude_sdk.ready","session_id":"stored","resumed":true}),
        ],
    );
    let stream = model.chat(agent_id).expect("stored chat").stream_attempt;
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 6,
                    selected_from: 0,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: at(NOW - 7),
            },
        },
    );
    update(
        &mut model,
        Msg::ChatStream {
            agent: agent_id,
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: at(NOW - 6) },
        },
    );
    assert!(
        model
            .claude_sdk(agent_id)
            .is_some_and(|layer| layer.todos().is_none()),
        "the reconnect must clear live-only todo state"
    );
    for (theme_name, theme) in [
        ("dark", Theme::default()),
        ("light", Theme::light(ColorMode::TrueColor)),
    ] {
        let mut view = view_default();
        view.open_chat(&model, agent_id);
        let capture = store_state_golden(&model, &view, theme);
        assert!(capture.contains("Write /tmp/old"));
        assert!(capture.contains("shipping"), "{capture}");
        assert!(!capture.contains("Allow once"));
        assert_golden(&format!("store_sdk_history_{theme_name}"), &capture);
    }
}

fn behind_model() -> Model {
    patch_chat(healthy_stored_chat_model(true), |chat| {
        chat["progress"] = serde_json::json!({"through": 8, "at": at(NOW - 1), "revision": 9});
    })
}

fn remembered_and_stale_model() -> Model {
    let mut remembered = an_agent("remembered-card", "claude", "nova");
    remembered.summary = Some(ui_state::SummaryEnvelope {
        through: 6,
        producer_version: ui_state::summary_producer_version(
            ui_state::StructuredProtocol::ClaudePtyTranscript,
        ),
        observed_at: at(NOW - 60),
        stale: false,
        revision: 3,
        summary: ui_state::Summary {
            attention: ui_state::Attention::NeedsYou {
                why: ui_state::Why::Permission,
            },
            phase: ui_state::AgentPhase::Running,
            last_activity: Some(at(NOW - 60)),
            todo: None,
            context: None,
            model: None,
            unknown: Vec::new(),
        },
    });
    let mut stale = an_agent("stale-summary", "claude", "nova");
    stale.summary = Some(ui_state::SummaryEnvelope {
        through: 7,
        producer_version: ui_state::summary_producer_version(
            ui_state::StructuredProtocol::ClaudePtyTranscript,
        ),
        observed_at: at(NOW - 20),
        stale: true,
        revision: 4,
        summary: ui_state::Summary {
            attention: ui_state::Attention::Working,
            phase: ui_state::AgentPhase::Running,
            last_activity: Some(at(NOW - 30)),
            todo: None,
            context: None,
            model: None,
            unknown: vec![ui_state::SummaryField::Todo],
        },
    });
    let model = fold(vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: an_offline_host("nova"),
        }),
        agent_up(&remembered),
        agent_up(&stale),
    ]);
    let mut value = serde_json::to_value(model).expect("model serializes");
    let remembered = value
        .pointer_mut(&format!(
            "/agents/{}/remembered",
            agent_id("remembered-card")
        ))
        .expect("remembered card field");
    *remembered = serde_json::Value::Bool(true);
    value["store"]["remembered_chat"] =
        serde_json::Value::String(agent_id("remembered-card").to_string());
    serde_json::from_value(value).expect("remembered model deserializes")
}

fn remembered_auth_model() -> Model {
    let mut value =
        serde_json::to_value(remembered_and_stale_model()).expect("remembered model serializes");
    value["connection"] = serde_json::json!({
        "connection": "disconnected",
        "reason": {"reason": "authentication_required"}
    });
    serde_json::from_value(value).expect("authentication model deserializes")
}

fn store_state_golden(model: &Model, view: &ViewState, theme: Theme) -> String {
    let buffer = render_buffer_at(model, view, 120, 40, theme);
    let capture = capture_frame(&buffer, theme);
    format!(
        "--- text ---\n{}--- styles ---\n{}",
        capture.text, capture.styles
    )
}

fn assert_store_state_goldens(theme: Theme, theme_name: &str) {
    let catching = healthy_stored_chat_model(false);
    let boundaries = durable_boundaries_model();
    let behind = behind_model();
    let states = [
        (
            "remembered_stale",
            remembered_and_stale_model(),
            view_default(),
        ),
        ("remembered_auth", remembered_auth_model(), view_default()),
        ("catching_up", catching.clone(), chat_view(&catching)),
        (
            "boundaries",
            boundaries.clone(),
            boundary_chat_view(&boundaries),
        ),
        ("behind", behind.clone(), chat_view(&behind)),
    ];
    for (label, model, view) in states {
        assert_golden(
            &format!("store_{label}_{theme_name}"),
            &store_state_golden(&model, &view, theme),
        );
    }
}

#[test]
fn store_states_dark() {
    let theme = Theme::default();
    assert_store_state_goldens(theme, "dark");
}

#[test]
fn store_states_light() {
    let theme = Theme::light(ColorMode::TrueColor);
    assert_store_state_goldens(theme, "light");
}

#[test]
fn catching_up_waits_before_showing_a_non_error_indicator() {
    let model = patch_chat(healthy_stored_chat_model(false), |chat| {
        chat["catching_up_since"] = serde_json::to_value(at(NOW)).expect("time serializes");
    });
    let view = chat_view(&model);
    assert!(
        view.chat.as_ref().expect("chat open").needs_tick(&model),
        "the delayed indicator must schedule the tick that reveals it"
    );
    let theme = Theme::default();
    let immediate = capture_frame(&render_buffer_at(&model, &view, 100, 20, theme), theme);
    assert!(!immediate.text.contains("catching up"));

    let delayed = healthy_stored_chat_model(false);
    let delayed = capture_frame(
        &render_buffer_at(&delayed, &chat_view(&delayed), 100, 20, theme),
        theme,
    );
    assert!(delayed.text.contains("catching up from saved history"));
    assert_eq!(
        delayed
            .styles
            .lines()
            .nth(1)
            .and_then(|line| line.chars().nth(2)),
        Some('m'),
        "catch-up is muted status, never an error"
    );
}

#[test]
fn offline_warm_start_and_gap_reconnect_keep_remembered_history_scrollable() {
    let remembered = remembered_and_stale_model();
    let fleet = render_frame_at(&remembered, &view_default(), 100, 20);
    assert!(fleet.contains("remembered"));
    assert!(fleet.contains("last permission"));
    assert_eq!(
        ui_state::claude::send_gate(&remembered, agent_id("remembered-card")),
        ui_state::claude::SendGate::Unavailable,
        "a remembered-only card cannot send before this connection confirms it"
    );

    let mut offline = serde_json::to_value(&remembered).expect("remembered model serializes");
    offline["connection"] = serde_json::json!({
        "connection": "disconnected",
        "reason": {"reason": "transport_error", "message": "daemon is offline"}
    });
    let offline: Model = serde_json::from_value(offline).expect("offline model deserializes");
    let frame = render_frame_at(&offline, &view_default(), 100, 20);
    assert!(frame.contains("remembered-card"));
    assert!(frame.contains("daemon unreachable"));
    assert!(frame.contains("start it with: amux server start"));

    let model = durable_boundaries_model();
    let frame = capture_frame(
        &render_buffer_at(
            &model,
            &boundary_chat_view(&model),
            100,
            20,
            Theme::default(),
        ),
        Theme::default(),
    )
    .text;
    assert!(
        frame.contains("do the thing"),
        "the old segment remains visible"
    );
    assert!(frame.contains("missing history"));
    assert!(frame.contains("history version changed"));
    assert!(frame.contains("earlier history evicted"));
    assert!(frame.contains("scrolled back"));
}

#[test]
fn remembered_chat_places_the_initial_fleet_cursor_without_opening_it() {
    let model = remembered_and_stale_model();
    let mut chrome = Chrome::new(
        view_default(),
        ChromeConfig {
            theme: Theme::default(),
        },
    );
    chrome.step(&model, &TraceEvent::Msg(Msg::Tick { now: at(NOW) }));

    let selected = visible_rows(&model, &chrome.view)
        .get(chrome.view.selected)
        .and_then(|row| row.card())
        .map(|card| card.agent.id);
    assert_eq!(selected, Some(agent_id("remembered-card")));
    assert!(chrome.view.chat.is_none());
}

async fn runtime_message(runtime: &mut Runtime, message: Msg) {
    runtime
        .shell_edge()
        .report(message)
        .await
        .expect("runtime edge remains open");
    tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
        .await
        .expect("runtime message timed out");
}

async fn next_runtime_message(runtime: &mut Runtime) {
    tokio::time::timeout(Duration::from_secs(5), runtime.next_message())
        .await
        .expect("runtime message timed out");
}

#[tokio::test]
async fn seeded_sqlite_warm_start_and_gap_reconnect_paint_at_the_tui_boundary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("store.sqlite");
    let agent = an_agent("stored-chat", "claude", "nova");
    let host = a_host("nova");
    let options = || RuntimeOptions {
        store_path: Some(path.clone()),
        ..RuntimeOptions::default()
    };

    let mut first = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
    for _ in 0..3 {
        next_runtime_message(&mut first).await;
    }
    for message in [
        server(ServerMsg::Connected {
            local_host_id: Some(host.id),
        }),
        server(ServerMsg::HostUpserted { host: host.clone() }),
        agent_up(&agent),
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
    ] {
        runtime_message(&mut first, message).await;
    }
    for delta in [
        FleetDelta::Host {
            host: host.clone(),
            revision: 1,
        },
        FleetDelta::AgentUp {
            agent: agent.clone(),
            revision: 2,
        },
    ] {
        runtime_message(&mut first, Msg::FleetDelta(delta)).await;
        next_runtime_message(&mut first).await;
    }

    first.open_chat(agent.id);
    while first
        .model()
        .chat(agent.id)
        .is_none_or(|chat| !chat.is_painted())
    {
        next_runtime_message(&mut first).await;
    }
    let stream = first
        .model()
        .chat(agent.id)
        .expect("chat loaded")
        .stream_attempt;
    runtime_message(
        &mut first,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 2,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: at(NOW - 10),
            },
        },
    )
    .await;
    runtime_message(
        &mut first,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: at(NOW - 9),
                entries: vec![
                    StreamEntry::observed(1, at(NOW - 9), ready_row()),
                    StreamEntry::observed(2, at(NOW - 9), prompt_row(8)),
                ],
            },
        },
    )
    .await;
    while first
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.pending_bytes() > 0)
    {
        next_runtime_message(&mut first).await;
    }
    runtime_message(
        &mut first,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: at(NOW - 8) },
        },
    )
    .await;
    while first
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.pending_bytes() > 0)
    {
        next_runtime_message(&mut first).await;
    }
    drop(first);

    let mut reopened = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
    for _ in 0..8 {
        if reopened.model().remembered_chat() == Some(agent.id)
            && reopened.model().agent(agent.id).is_some()
        {
            break;
        }
        next_runtime_message(&mut reopened).await;
    }
    assert!(
        reopened
            .model()
            .agent(agent.id)
            .is_some_and(|card| card.remembered)
    );
    assert!(reopened.model().chat(agent.id).is_none());
    reopened.open_chat(agent.id);
    while reopened
        .model()
        .chat(agent.id)
        .is_none_or(|chat| chat.entries.is_empty())
    {
        next_runtime_message(&mut reopened).await;
    }
    let warm = reopened.model();
    assert!(
        !warm
            .chat(agent.id)
            .expect("remembered chat")
            .entries
            .is_empty()
    );
    let warm_frame = render_frame_at(warm, &chat_view(warm), 120, 40);
    assert!(warm_frame.contains("do the thing"));

    let stream = warm.chat(agent.id).expect("remembered chat").stream_attempt;
    reopened
        .shell_edge()
        .report(Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 3,
                    through: 3,
                    selected_from: 3,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Truncated { missing_after: 2 },
                },
                at: at(NOW - 2),
            },
        })
        .await
        .expect("gap open reaches runtime");
    while reopened.model().chat(agent.id).expect("gap chat").state
        != ui_state::ChatState::CatchingUp
    {
        next_runtime_message(&mut reopened).await;
    }
    let mut after_gap = prompt_row(9);
    after_gap["message"]["content"] = serde_json::Value::String("new after gap".to_owned());
    reopened
        .shell_edge()
        .report(Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: at(NOW - 1),
                entries: vec![StreamEntry::observed(3, at(NOW - 1), after_gap)],
            },
        })
        .await
        .expect("gap batch reaches runtime");
    while !reopened
        .model()
        .chat(agent.id)
        .expect("gap chat")
        .entries
        .iter()
        .any(|entry| entry.text() == Some("new after gap"))
    {
        next_runtime_message(&mut reopened).await;
    }
    while reopened
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.pending_bytes() > 0)
    {
        next_runtime_message(&mut reopened).await;
    }
    let chat = reopened.model().chat(agent.id).expect("gap chat");
    assert!(
        chat.boundaries
            .iter()
            .any(|item| item.boundary == Boundary::Gap),
        "gap commit did not install its boundary: state={:?} boundaries={:?} pending={}",
        chat.state,
        chat.boundaries,
        chat.pending_bytes()
    );
    assert!(
        chat.entries
            .iter()
            .any(|entry| entry.text() == Some("do the thing"))
    );
    assert!(
        chat.entries
            .iter()
            .any(|entry| entry.text() == Some("new after gap"))
    );
    let gap_frame = capture_frame(
        &render_buffer_at(
            reopened.model(),
            &boundary_chat_view(reopened.model()),
            120,
            40,
            Theme::default(),
        ),
        Theme::default(),
    )
    .text;
    assert!(gap_frame.contains("do the thing"));
    assert!(gap_frame.contains("missing history"));
}

#[tokio::test]
async fn sqlite_chat_pages_past_the_loaded_window_and_returns_to_the_tip() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("store.sqlite");
    let agent = an_agent("stored-chat", "claude", "nova");
    let host = a_host("nova");
    let options = || RuntimeOptions {
        store_path: Some(path.clone()),
        ..RuntimeOptions::default()
    };

    let mut seed = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
    for _ in 0..3 {
        next_runtime_message(&mut seed).await;
    }
    for message in [
        server(ServerMsg::Connected {
            local_host_id: Some(host.id),
        }),
        server(ServerMsg::HostUpserted { host: host.clone() }),
        agent_up(&agent),
        server(ServerMsg::HostsSynchronized),
        server(ServerMsg::AgentsSynchronized),
    ] {
        runtime_message(&mut seed, message).await;
    }
    for delta in [
        FleetDelta::Host {
            host: host.clone(),
            revision: 1,
        },
        FleetDelta::AgentUp {
            agent: agent.clone(),
            revision: 2,
        },
    ] {
        runtime_message(&mut seed, Msg::FleetDelta(delta)).await;
        next_runtime_message(&mut seed).await;
    }

    seed.open_chat(agent.id);
    while seed
        .model()
        .chat(agent.id)
        .is_none_or(|chat| !chat.is_painted())
    {
        next_runtime_message(&mut seed).await;
    }
    let stream = seed.model().chat(agent.id).unwrap().stream_attempt;
    runtime_message(
        &mut seed,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Opened {
                facts: ReplayFactsDto {
                    retained_from: 1,
                    through: 2_401,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ReplayOutcomeDto::Continuous,
                },
                at: at(NOW - 10),
            },
        },
    )
    .await;
    let mut entries = Vec::with_capacity(2_401);
    entries.push(StreamEntry::observed(1, at(NOW - 9), ready_row()));
    entries.extend(
        (1..=2_400).map(|n| StreamEntry::observed(n as u64 + 1, at(NOW - 9), stored_prompt_row(n))),
    );
    runtime_message(
        &mut seed,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::Batch {
                at: at(NOW - 9),
                entries,
            },
        },
    )
    .await;
    while seed
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.pending_bytes() > 0)
    {
        next_runtime_message(&mut seed).await;
    }
    runtime_message(
        &mut seed,
        Msg::ChatStream {
            agent: agent.id,
            attempt: stream,
            event: ChatStreamMsg::ReplayComplete { at: at(NOW - 8) },
        },
    )
    .await;
    while seed
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.pending_bytes() > 0)
    {
        next_runtime_message(&mut seed).await;
    }
    drop(seed);

    let mut runtime = Runtime::start(Box::new(|| Box::pin(std::future::pending())), options());
    for _ in 0..8 {
        if runtime.model().agent(agent.id).is_some() {
            break;
        }
        next_runtime_message(&mut runtime).await;
    }
    runtime.open_chat(agent.id);
    while runtime
        .model()
        .chat(agent.id)
        .is_none_or(|chat| chat.entries.is_empty())
    {
        next_runtime_message(&mut runtime).await;
    }
    let mut view = chat_view(runtime.model());
    assert!(render_frame_at(runtime.model(), &view, 120, 40).contains("stored row 2400"));

    while runtime
        .model()
        .chat(agent.id)
        .is_some_and(|chat| chat.first_page.is_some())
    {
        let action = tui::chat::handle_chat_key(
            view.chat.as_mut().expect("chat open"),
            runtime.model(),
            KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
            (120, 40),
            at(NOW),
        );
        assert_eq!(action, Some(UiAction::PageChatOlder(agent.id)));
        let before = runtime.model().chat(agent.id).unwrap().view_epoch;
        runtime.page_chat_older(agent.id);
        while runtime.model().chat(agent.id).unwrap().view_epoch == before {
            next_runtime_message(&mut runtime).await;
        }
        let window = runtime.model().chat(agent.id).unwrap();
        assert!(window.entries.len() <= ui_state::store::WINDOW_MAX_ENTRIES);
        assert!(window.encoded_window_bytes() <= ui_state::store::WINDOW_MAX_BYTES);
        view.chat.as_mut().unwrap().reconcile(runtime.model());
        let _ = render_frame_at(runtime.model(), &view, 120, 40);
    }
    let oldest = render_frame_at(runtime.model(), &view, 120, 40);
    assert!(oldest.contains("stored row 0001"), "{oldest}");

    let action = tui::chat::handle_chat_key(
        view.chat.as_mut().expect("chat open"),
        runtime.model(),
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
        (120, 40),
        at(NOW),
    );
    assert_eq!(action, Some(UiAction::FollowChatTip(agent.id)));
    let before = runtime.model().chat(agent.id).unwrap().view_epoch;
    runtime.follow_chat_tip(agent.id);
    while runtime.model().chat(agent.id).unwrap().view_epoch == before {
        next_runtime_message(&mut runtime).await;
    }
    view.chat.as_mut().unwrap().reconcile(runtime.model());
    let newest = render_frame_at(runtime.model(), &view, 120, 40);
    assert!(newest.contains("stored row 2400"), "{newest}");
}

/// A free relay link keeps presence: the host and banner call that state away.
#[test]
fn fleet_away_banner() {
    let mut msgs = fleet_msgs();
    let mut host = a_host("hetzner");
    host.via = ui_state::HostVia::Relay;
    msgs.push(server(ServerMsg::HostUpserted { host }));
    msgs.push(server(ServerMsg::CloudState(CloudState::Connected {
        tier: Tier::Free,
        carrier: RelayCarrier::Tcp,
    })));
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 11);
    assert!(rendered.contains("hetzner ·away"), "{rendered}");
    assert_golden("fleet_away_banner", &rendered);
}

/// Signed out with an offline trusted host, the banner offers remote reach.
#[test]
fn fleet_signed_out_offline_banner() {
    let mut msgs = fleet_msgs();
    msgs.push(server(ServerMsg::CloudState(CloudState::SignedOut)));
    let rendered = render_frame(&fold(msgs), &view_default(), 68, 11);
    assert!(
        rendered.contains("sign in to reach your agents from anywhere · amux login"),
        "{rendered}"
    );
    assert_golden("fleet_signed_out_offline_banner", &rendered);
}

fn hosts_overlay_model() -> Model {
    let mut msgs = fleet_msgs();
    let mut away = a_host("hetzner");
    away.via = ui_state::HostVia::Relay;
    msgs.push(server(ServerMsg::HostUpserted { host: away }));

    let mut relay = a_host("relay-no-account");
    relay.via = ui_state::HostVia::Relay;
    relay.signed_in = Some(false);
    msgs.push(server(ServerMsg::HostUpserted { host: relay }));

    let mut ssh = a_host("bastion");
    ssh.via = ui_state::HostVia::Ssh;
    msgs.push(server(ServerMsg::HostUpserted { host: ssh }));

    let mut found = a_host("new-mac");
    found.trust_status = ui_state::HostTrustStatus::UntrustedButOnline;
    msgs.push(server(ServerMsg::HostUpserted { host: found }));
    msgs.push(server(ServerMsg::CloudState(CloudState::Connected {
        tier: Tier::Free,
        carrier: RelayCarrier::Tcp,
    })));
    fold(msgs)
}

fn hosts_overlay_view() -> ViewState {
    ViewState {
        mode: Mode::Hosts,
        ..view_default()
    }
}

#[test]
fn fleet_hosts_overlay_dark() {
    let model = hosts_overlay_model();
    let view = hosts_overlay_view();
    let theme = Theme::default();
    let buffer = render_buffer(&model, &view, 120, 40, theme);
    let capture = capture_frame(&buffer, theme);
    let rendered = &capture.text;
    for text in [
        "·direct",
        "·relay, not signed in",
        "·ssh",
        "·away",
        "·offline",
        "found · run amux pair new-mac",
    ] {
        assert!(rendered.contains(text), "missing {text:?}:\n{rendered}");
    }
    assert_golden(
        "fleet_hosts_overlay_dark",
        &format!(
            "--- text ---\n{}--- styles ---\n{}",
            capture.text, capture.styles
        ),
    );
}

#[test]
fn fleet_hosts_overlay_light() {
    let model = hosts_overlay_model();
    let view = hosts_overlay_view();
    let theme = Theme::light(ColorMode::TrueColor);
    let buffer = render_buffer(&model, &view, 120, 40, theme);
    let capture = capture_frame(&buffer, theme);
    assert_golden(
        "fleet_hosts_overlay_light",
        &format!(
            "--- text ---\n{}--- styles ---\n{}",
            capture.text, capture.styles
        ),
    );
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
    let rendered = render_frame_at(&fleet_model(), &view_default(), 12, 11);
    assert_golden("fleet_too_narrow", &rendered);
}

/// No viewport size may panic the renderer: sweep the fleet across every
/// width and height a terminal could plausibly report. (Regression: widths
/// below the layout minimum underflowed the right-info column arithmetic.)
mod rendering_never_panics_at_any_viewport_size {
    use super::*;

    fn check_widths(first: u16, last: u16) {
        static MODEL: std::sync::OnceLock<Model> = std::sync::OnceLock::new();
        let model = MODEL.get_or_init(fleet_model);
        let view = view_default();
        for width in first..=last {
            for height in 1..=60u16 {
                let _ = render_frame_at(model, &view, width, height);
            }
        }
    }

    // Independent width ranges let libtest distribute the full viewport matrix.
    macro_rules! widths {
        ($($name:ident: $first:literal..=$last:literal),+ $(,)?) => {
            $(#[test]
            fn $name() {
                check_widths($first, $last);
            })+
        };
    }

    widths! {
        widths_001_010: 1..=10,
        widths_011_020: 11..=20,
        widths_021_030: 21..=30,
        widths_031_040: 31..=40,
        widths_041_050: 41..=50,
        widths_051_060: 51..=60,
        widths_061_070: 61..=70,
        widths_071_080: 71..=80,
        widths_081_090: 81..=90,
        widths_091_100: 91..=100,
        widths_101_110: 101..=110,
        widths_111_120: 111..=120,
        widths_121_130: 121..=130,
        widths_131_140: 131..=140,
        widths_141_150: 141..=150,
        widths_151_160: 151..=160,
        widths_161_170: 161..=170,
        widths_171_180: 171..=180,
        widths_181_190: 181..=190,
        widths_191_200: 191..=200,
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
        rendered.contains('\u{258e}'),
        "the selection bar survives the shrink"
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
                agent_type: ui_state::AgentType::Claude {
                    driver: ui_state::ClaudeDriver::Pty,
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
            agent_type: ui_state::AgentType::Claude {
                driver: ui_state::ClaudeDriver::Pty,
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

/// The badge and the offline row name tokens, not colour literals: the
/// error token carries the permission badge, and an offline host's row
/// falls back to muted rather than wearing a DIM modifier over body text.
#[test]
fn badges_and_offline_rows_wear_semantic_tokens() {
    let mut msgs = fleet_msgs();
    msgs.push(server(ServerMsg::HostUpserted {
        host: an_offline_host("hetzner"),
    }));
    let model = fold(msgs);
    let view = view_default();
    let theme = Theme::default();
    let buffer = render_buffer(&model, &view, 68, 11, theme);

    // Row 3 is the permission row: the `!` badge is the error token.
    let badge = buffer.cell((4, 3)).expect("badge cell");
    assert_eq!(badge.symbol(), "!");
    assert_eq!(theme.classify(badge.style()), 'x', "the badge is an error");

    // Offline-host rows render muted (name cell of an offline row).
    let offline_name = (0..11u16)
        .find_map(|y| {
            let cell = buffer.cell((6, y))?;
            (cell.symbol() == "m").then_some(cell)
        })
        .expect("migration-plan row present");
    assert_eq!(
        theme.classify(offline_name.style()),
        'm',
        "an offline row is de-emphasis all the way across"
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

    let flake = a_child("flake-hunter", "codex", "nova", "test-runner");
    msgs.push(agent_up(&flake));

    msgs.extend(stream_rows(
        "write-the-docs",
        NOW - 300,
        vec![ready_row(), prompt_row(5), stop_row()],
    ));
    msgs.extend(stream_rows(
        "test-runner",
        NOW - 30,
        vec![codex_ready_row(), codex_turn_started_row("turn-runner")],
    ));
    msgs.extend(stream_rows("flake-hunter", NOW - 20, codex_approval_rows()));
    msgs
}

fn family_model() -> Model {
    let model = fold(family_msgs());
    assert_eq!(
        model.fleet_attention(
            model
                .agent(agent_id("flake-hunter"))
                .expect("flake-hunter card")
        ),
        ui_state::Attention::NeedsYou {
            why: ui_state::Why::Permission,
        },
        "the enriched Codex rows drive flake-hunter's fleet standing",
    );
    model
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
    let rendered = render_frame_at(&family_model(), &view, 60, 14);
    assert_golden("a2a_fleet_family_60col", &rendered);
}

/// Every cell of the fleet comes from a semantic token, so the two themes
/// produce the same class map: a light terminal gets the same frame a dark
/// one does, family badge included, in its own colours.
#[test]
fn a2a_fleet_family_styles_dark() {
    let view = expanded_view(&["refactor-tunnels"]);
    let theme = Theme::default();
    let styles = buffer_styles(&render_buffer(&family_model(), &view, 80, 14, theme), theme);
    assert_golden("a2a_fleet_family_styles_dark", &styles);
}

#[test]
fn a2a_fleet_family_styles_light() {
    let view = expanded_view(&["refactor-tunnels"]);
    let theme = Theme::light(ColorMode::TrueColor);
    let styles = buffer_styles(&render_buffer(&family_model(), &view, 80, 14, theme), theme);
    assert_golden("a2a_fleet_family_styles_light", &styles);
}

/// The fold key opens the family under the cursor and shuts it again;
/// shutting from a descendant leaves the cursor on the row that swallowed
/// it, never on whatever slid into that index.
#[test]
fn a2a_fleet_fold_key_opens_and_shuts_the_family() {
    let model = family_model();
    let mut view = view_default();
    let top = tui::view::visible_rows(&model, &view)
        .iter()
        .position(|row| row.display_name() == "refactor-tunnels")
        .expect("the folded family row");
    view.selected = top;

    tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));
    let open = tui::view::visible_rows(&model, &view);
    assert_eq!(open.len(), 8, "every descendant is a row while open");

    // Put the cursor on the deepest descendant, then shut from there.
    view.selected = open
        .iter()
        .position(|row| row.display_name() == "flake-hunter")
        .expect("the grandchild is visible while open");
    tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));

    let shut = tui::view::visible_rows(&model, &view);
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
        !tui::view::visible_rows(&model, &view)
            .iter()
            .any(|row| row.display_name() == "flake-hunter"),
        "the grandchild is folded away with nothing typed"
    );

    let filtered = ViewState {
        filter: "flake".to_string(),
        ..view_default()
    };
    let rows = tui::view::visible_rows(&model, &filtered);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].display_name(), "flake-hunter");
}

/// An agent in no family has nothing to fold, and the key says so by
/// changing nothing.
#[test]
fn a2a_fleet_fold_key_is_inert_on_a_childless_row() {
    let model = family_model();
    let mut view = view_default();
    view.selected = tui::view::visible_rows(&model, &view)
        .iter()
        .position(|row| row.display_name() == "docs-cleanup")
        .expect("a childless row");
    let before = tui::view::visible_rows(&model, &view).len();
    tui::keys::handle_key(&mut view, &model, press('z'), 20, at(NOW));
    assert_eq!(tui::view::visible_rows(&model, &view).len(), before);
    assert!(view.expanded.is_empty());
}

/// `working_on` renders with the age of the claim, and is absent — not
/// filled with something invented — for an agent that never said.
#[test]
fn a2a_fleet_working_on_states_the_claim_and_its_age() {
    let view = expanded_view(&["refactor-tunnels"]);
    let rendered = render_frame_at(&family_model(), &view, 80, 14);
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
        row.contains("run the tunnel suite end to end 1m"),
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
    view.selected = tui::view::visible_rows(model, &view)
        .iter()
        .position(|row| row.display_name() == name)
        .unwrap_or_else(|| panic!("{name} is a visible row"));
    tui::keys::handle_key(&mut view, model, press('d'), 20, at(NOW));
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
    let action = tui::keys::handle_key(&mut view, &model, press('y'), 20, at(NOW));
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
    let action = tui::keys::handle_key(&mut view, &model, press('y'), 20, at(NOW));
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
    let rendered = render_frame_at(&model, &view, 80, 13);
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
    let theme = Theme::default();
    let styles = buffer_styles(&render_buffer(&model, &view, 80, 14, theme), theme);
    assert_golden("a2a_delete_confirm_styles", &styles);
}

#[test]
fn a2a_delete_confirm_styles_light() {
    let model = family_model();
    let view = confirming_delete(&model, "refactor-tunnels");
    let theme = Theme::light(ColorMode::TrueColor);
    let styles = buffer_styles(&render_buffer(&model, &view, 80, 14, theme), theme);
    assert_golden("a2a_delete_confirm_styles_light", &styles);
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
    let narrow = render_frame_at(&family_model(), &view_default(), 68, 14);
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

// --- one fleet, two kinds of Claude session -------------------------------

/// The same Claude agent, reached over stream-JSON instead of a terminal.
/// Everything a fleet row reads — the command it was started with, the
/// host, the ages, the badge — is the same fact from the same agent; only
/// the machinery behind it differs, and the fleet never asks about that.
fn a_session_agent(name: &str, on: &str) -> Agent {
    Agent {
        kind: ui_state::AgentKind::Claude {
            driver: ui_state::ClaudeDriver::Sdk,
        },
        ..an_agent(name, "claude", on)
    }
}

fn session_ready_row() -> serde_json::Value {
    serde_json::json!({
        "type": "amux.claude_sdk.ready",
        "session_id": "33333333-3333-4333-8333-333333333333",
        "resumed": false,
    })
}

fn session_prompt_row() -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "parent_tool_use_id": null,
        "message": {"role": "user", "content": "do the thing"},
    })
}

fn session_permission_row() -> serde_json::Value {
    serde_json::json!({
        "type": "amux.claude_sdk.permission_required",
        "request_id": "permission-1",
        "tool_name": "Bash",
        "input": {"command": "echo probe"},
        "suggestions": [],
    })
}

/// Two pairs of Claude agents on one host: in each pair one session is
/// driven over a terminal and the other over stream-JSON, both stopped on
/// the same thing at the same moment. A pair that renders as two identical
/// rows is the whole claim.
fn mixed_fleet_msgs() -> Vec<Msg> {
    let mut msgs = vec![
        server(ServerMsg::Connected {
            local_host_id: Some(host_id("nova")),
        }),
        server(ServerMsg::HostUpserted {
            host: a_host("nova"),
        }),
        agent_up(&an_agent("fix-auth", "claude", "nova")),
        agent_up(&a_session_agent("fix-sync", "nova")),
        agent_up(&an_agent("docs-auth", "claude", "nova")),
        agent_up(&a_session_agent("docs-sync", "nova")),
    ];
    msgs.extend(synced());
    msgs.extend(stream_rows(
        "fix-auth",
        NOW - 120,
        vec![ready_row(), prompt_row(1), permission_row()],
    ));
    msgs.extend(stream_rows(
        "fix-sync",
        NOW - 120,
        vec![
            session_ready_row(),
            session_prompt_row(),
            session_permission_row(),
        ],
    ));
    msgs.extend(stream_rows(
        "docs-auth",
        NOW - 12,
        vec![ready_row(), prompt_row(2)],
    ));
    msgs.extend(stream_rows(
        "docs-sync",
        NOW - 12,
        vec![session_ready_row(), session_prompt_row()],
    ));
    msgs
}

fn mixed_fleet_model() -> Model {
    fold(mixed_fleet_msgs())
}

/// Where a named agent sits in the ranked fleet, so a test can select it
/// without hard-coding an order the ranking owns.
fn fleet_row_index(model: &Model, name: &str) -> usize {
    model
        .fleet()
        .iter()
        .position(|item| match item {
            ui_state::FleetItem::Agent(card) => card.display_name() == name,
            ui_state::FleetItem::Family { parent, .. } => parent.display_name() == name,
            ui_state::FleetItem::PendingCreate { .. } => false,
        })
        .unwrap_or_else(|| panic!("{name} has a fleet row"))
}

/// The row a name is drawn on, with the name itself blanked out: what is
/// left is every column the fleet decided for that agent.
fn fleet_row_without_name(frame: &str, name: &str) -> String {
    frame
        .lines()
        .find(|line| line.contains(name))
        .unwrap_or_else(|| panic!("no row for {name} in:\n{frame}"))
        .replace(name, "————————")
}

#[test]
fn fleet_mixed_rows() {
    let rendered = render_frame(&mixed_fleet_model(), &view_default(), 120, 40);
    assert_golden("fleet_mixed_rows", &rendered);
}

/// Two Claude sessions stopped on the same ask at the same moment draw
/// the same row: same badge, same command, same host, same age, same
/// state word. Blank the names and the two lines are one line. The
/// selection bar repaints whichever row it sits on, so each pair is read
/// off a frame whose selection is on the other pair.
#[test]
fn fleet_mixed_rows_are_identical_apart_from_the_name() {
    let model = mixed_fleet_model();
    for (selected, terminal, session) in [
        ("docs-auth", "fix-auth", "fix-sync"),
        ("fix-auth", "docs-auth", "docs-sync"),
    ] {
        let view = ViewState {
            selected: fleet_row_index(&model, selected),
            ..view_default()
        };
        let frame = render_frame(&model, &view, 120, 40);
        assert_eq!(
            fleet_row_without_name(&frame, terminal),
            fleet_row_without_name(&frame, session),
            "{terminal} and {session} are the same row:\n{frame}"
        );
    }
}

/// The badges are the same colour too, which the text frame cannot see:
/// a permission badge that read as a warning on one row and an error on
/// the other would still pass the text comparison above. The selection
/// bar repaints whichever row it sits on, so each pair is compared from
/// a frame whose selection is on the other pair.
#[test]
fn fleet_mixed_badges_wear_the_same_tokens() {
    let model = mixed_fleet_model();
    let theme = Theme::default();
    let style_row = |selected: &str, of: &str| {
        let view = ViewState {
            selected: fleet_row_index(&model, selected),
            ..view_default()
        };
        let frame = render_frame(&model, &view, 120, 40);
        let row = frame
            .lines()
            .position(|line| line.contains(of))
            .unwrap_or_else(|| panic!("no row for {of}:\n{frame}"));
        let styles = buffer_styles(&render_buffer(&model, &view, 120, 40, theme), theme);
        styles.lines().nth(row).expect("a style row").to_string()
    };
    for (selected, terminal, session) in [
        ("docs-auth", "fix-auth", "fix-sync"),
        ("fix-auth", "docs-auth", "docs-sync"),
    ] {
        assert_eq!(
            style_row(selected, terminal),
            style_row(selected, session),
            "{terminal} and {session} are painted from the same tokens"
        );
    }
}

/// A session with no terminal behind it has one way in, and the overlay
/// says so: `enter` opens the chat and there is no second mode to offer.
#[test]
fn fleet_mixed_help_overlay_for_a_session_agent() {
    let model = mixed_fleet_model();
    let view = ViewState {
        mode: Mode::Help,
        selected: fleet_row_index(&model, "fix-sync"),
        ..view_default()
    };
    let rendered = render_frame(&model, &view, 120, 40);
    assert!(rendered.contains("open in chat"), "{rendered}");
    assert!(
        !rendered.contains("raw attach"),
        "there is no terminal to attach to:\n{rendered}"
    );
    assert_golden("fleet_mixed_help_overlay_session", &rendered);
}

/// The same overlay for the agent beside it, which does have a terminal:
/// the difference is the agent's own capability, not the screen's mood.
#[test]
fn fleet_mixed_help_overlay_for_a_terminal_agent() {
    let model = mixed_fleet_model();
    let view = ViewState {
        mode: Mode::Help,
        selected: fleet_row_index(&model, "fix-auth"),
        ..view_default()
    };
    let rendered = render_frame(&model, &view, 120, 40);
    assert!(rendered.contains("open in raw attach"), "{rendered}");
    assert!(rendered.contains("open in chat"), "{rendered}");
    assert_golden("fleet_mixed_help_overlay_terminal", &rendered);
}
