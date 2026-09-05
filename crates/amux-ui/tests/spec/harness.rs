//! Spec harness: fixture builders for Msg sequences and pure fold helpers.
//!
//! No tokio, no network, no clocks — chapters build ordered Msg vectors,
//! fold them, and assert on the Model. Ids and times are derived
//! deterministically from names so sequences replay identically forever.

use amux::{Agent, AgentId, Capabilities, HostEntry, HostId, HostTrustStatus};
use amux_ui::claude::ClaudeLayer;
use amux_ui::codex::CodexLayer;
use amux_ui::{
    Command, DisconnectReason, Effect, Model, Msg, OpId, OpOutcome, ServerMsg, StreamEntry,
    StreamMsg, update,
};
use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

/// Fixed epoch for all fixture times: 2026-08-09 00:00:00 UTC.
pub fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_754_697_600, 0).expect("valid fixture epoch")
}

pub fn t0_plus(seconds: i64) -> DateTime<Utc> {
    t0() + TimeDelta::seconds(seconds)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Deterministic host id, domain-tagged so host and agent names never
/// collide.
pub fn host_id(name: &str) -> HostId {
    Uuid::from_u128((1u128 << 64) | u128::from(fnv1a(name.as_bytes())))
}

pub fn agent_id(name: &str) -> AgentId {
    Uuid::from_u128((2u128 << 64) | u128::from(fnv1a(name.as_bytes())))
}

pub fn op(n: u8) -> OpId {
    OpId(Uuid::from_u128((3u128 << 64) | u128::from(n)))
}

/// An online, trusted host.
pub fn a_host(name: &str) -> HostEntry {
    HostEntry {
        id: host_id(name),
        name: name.to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(Capabilities::default()),
        trust_status: HostTrustStatus::Trusted,
        last_dial_error: None,
    }
}

/// A known-but-offline host (renders dim with unknown agents).
pub fn an_offline_host(name: &str) -> HostEntry {
    HostEntry {
        id: host_id(name),
        name: name.to_string(),
        online: false,
        version: None,
        capabilities: None,
        trust_status: HostTrustStatus::Trusted,
        last_dial_error: Some("dial tcp: connection refused".to_string()),
    }
}

/// A claude agent on the named host, created at `t0`.
pub fn an_agent(name: &str, on: &str) -> Agent {
    Agent {
        id: agent_id(name),
        host_id: host_id(on),
        name: Some(name.to_string()),
        command: "claude".to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        kind: amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
        parent: None,
        working_on: None,
    }
}

/// A Codex agent advertising its native structured protocol.
pub fn a_codex_agent(name: &str, on: &str) -> Agent {
    Agent {
        id: agent_id(name),
        host_id: host_id(on),
        name: Some(name.to_string()),
        command: "codex".to_string(),
        working_dir: std::path::PathBuf::from("/work"),
        kind: amux::AgentKind::Codex,
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
        parent: None,
        working_on: None,
    }
}

// --- Msg constructors -----------------------------------------------------

pub fn connected(local: &str) -> Msg {
    Msg::Server(ServerMsg::Connected {
        local_host_id: Some(host_id(local)),
    })
}

pub fn disconnected(reason: DisconnectReason) -> Msg {
    Msg::Server(ServerMsg::Disconnected { reason })
}

pub fn host_up(host: &HostEntry) -> Msg {
    Msg::Server(ServerMsg::HostUpserted { host: host.clone() })
}

pub fn hosts_synced() -> Msg {
    Msg::Server(ServerMsg::HostsSynchronized)
}

pub fn agent_up(agent: &Agent) -> Msg {
    Msg::Server(ServerMsg::AgentUpserted {
        agent: agent.clone(),
    })
}

pub fn agent_gone(name: &str) -> Msg {
    Msg::Server(ServerMsg::AgentRemoved { id: agent_id(name) })
}

pub fn agents_synced() -> Msg {
    Msg::Server(ServerMsg::AgentsSynchronized)
}

/// Both snapshot markers: the transition from catch-up to live.
pub fn synced() -> Vec<Msg> {
    vec![hosts_synced(), agents_synced()]
}

pub fn command(op: OpId, command: Command) -> Msg {
    Msg::Command { op, command }
}

pub fn create_cmd(name: &str, host: Option<&str>) -> Command {
    Command::CreateAgent {
        host: host.map(host_id),
        name: name.to_string(),
        agent_type: amux::AgentType::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        working_dir: std::path::PathBuf::from("/work"),
    }
}

pub fn rename_cmd(agent: &str, name: &str) -> Command {
    Command::RenameAgent {
        agent: agent_id(agent),
        name: name.to_string(),
    }
}

pub fn delete_cmd(agent: &str) -> Command {
    Command::DeleteAgent {
        agent: agent_id(agent),
    }
}

pub fn op_result(op: OpId, outcome: OpOutcome) -> Msg {
    Msg::OpResult { op, outcome }
}

pub fn op_failed(op: OpId, message: &str) -> Msg {
    op_result(
        op,
        OpOutcome::Error {
            error: amux_ui::OpError::general(message),
        },
    )
}

pub fn op_failed_auth(op: OpId) -> Msg {
    op_result(
        op,
        OpOutcome::Error {
            error: amux_ui::OpError::classified("Invalid or missing credentials", true, false),
        },
    )
}

pub fn op_failed_subscription(op: OpId) -> Msg {
    op_result(
        op,
        OpOutcome::Error {
            error: amux_ui::OpError::classified("Cloud subscription required", false, true),
        },
    )
}

pub fn stream(agent: &str, event: StreamMsg) -> Msg {
    Msg::Stream {
        agent: agent_id(agent),
        event,
    }
}

pub fn batch(agent: &str, at_seconds: i64, rows: Vec<serde_json::Value>) -> Msg {
    let base = 1 + at_seconds.max(0) as u64;
    stream(
        agent,
        StreamMsg::Batch {
            at: t0_plus(at_seconds),
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(offset, payload)| StreamEntry {
                    seq: base + offset as u64,
                    payload,
                })
                .collect(),
        },
    )
}

pub fn tick(at_seconds: i64) -> Msg {
    Msg::Tick {
        now: t0_plus(at_seconds),
    }
}

// --- Chat fixtures ----------------------------------------------------------

/// Rows of a committed Claude PTY fixture: a redacted, provenance-stamped
/// stream derived from a canonical Claude PTY provider recording
/// (`crates/amux/tests/fixtures/rows/claude-pty/`). Referenced across
/// crates by compile-time include so the spec suite stays IO-free.
pub fn chat_rows(fixture: &str) -> Vec<serde_json::Value> {
    let raw = match fixture {
        "pong" => include_str!("../../../amux/tests/fixtures/rows/claude-pty/pong.rows.jsonl"),
        "tools" => include_str!("../../../amux/tests/fixtures/rows/claude-pty/tools.rows.jsonl"),
        "permission" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/permission.rows.jsonl")
        }
        "question_single" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/question_single.rows.jsonl")
        }
        "question_multi" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/question_multi.rows.jsonl")
        }
        "interrupt" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/interrupt.rows.jsonl")
        }
        "plan_approve" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/plan_approve.rows.jsonl")
        }
        "plan_reject" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/plan_reject.rows.jsonl")
        }
        "compact" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/compact.rows.jsonl")
        }
        "clear" => include_str!("../../../amux/tests/fixtures/rows/claude-pty/clear.rows.jsonl"),
        "mode_cycle" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/mode_cycle.rows.jsonl")
        }
        "permission_session" => {
            include_str!(
                "../../../amux/tests/fixtures/rows/claude-pty/permission_session.rows.jsonl"
            )
        }
        "permission_deny_feedback" => {
            include_str!(
                "../../../amux/tests/fixtures/rows/claude-pty/permission_deny_feedback.rows.jsonl"
            )
        }
        "question_tabs" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/question_tabs.rows.jsonl")
        }
        "question_mixed" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/question_mixed.rows.jsonl")
        }
        "question_other_single" => {
            include_str!(
                "../../../amux/tests/fixtures/rows/claude-pty/question_other_single.rows.jsonl"
            )
        }
        "plan_auto" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/plan_auto.rows.jsonl")
        }
        "prompt_multiline" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-pty/prompt_multiline.rows.jsonl")
        }
        other => panic!("unknown chat fixture {other}"),
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture row parses"))
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub enum ChatAnchor {
    Prompt(usize),
    PermissionMode(usize),
    TranscriptReady,
    PermissionRequest(usize),
    ToolUse {
        name: &'static str,
        occurrence: usize,
    },
    ToolResult(usize),
    StopHook(usize),
    TurnDuration(usize),
}

fn chat_anchor_index(rows: &[serde_json::Value], anchor: ChatAnchor) -> usize {
    let mut matched = 0;
    rows.iter()
        .position(|row| {
            let is_match = match anchor {
                ChatAnchor::Prompt(_) => row["type"] == "user" && row["origin"]["kind"] == "human",
                ChatAnchor::PermissionMode(_) => row["type"] == "permission-mode",
                ChatAnchor::TranscriptReady => row["type"] == "amux.transcript_ready",
                ChatAnchor::PermissionRequest(_) => row["type"] == "hook.permission_request",
                ChatAnchor::ToolUse { name, .. } => {
                    row["message"]["content"].as_array().is_some_and(|content| {
                        content
                            .iter()
                            .any(|block| block["type"] == "tool_use" && block["name"] == name)
                    })
                }
                ChatAnchor::ToolResult(_) => {
                    row["message"]["content"].as_array().is_some_and(|content| {
                        content.iter().any(|block| block["type"] == "tool_result")
                    })
                }
                ChatAnchor::StopHook(_) => row["type"] == "hook.stop",
                ChatAnchor::TurnDuration(_) => {
                    row["type"] == "system" && row["subtype"] == "turn_duration"
                }
            };
            if !is_match {
                return false;
            }
            let wanted = match anchor {
                ChatAnchor::Prompt(occurrence)
                | ChatAnchor::PermissionMode(occurrence)
                | ChatAnchor::PermissionRequest(occurrence)
                | ChatAnchor::ToolResult(occurrence)
                | ChatAnchor::StopHook(occurrence)
                | ChatAnchor::TurnDuration(occurrence) => occurrence,
                ChatAnchor::ToolUse { occurrence, .. } => occurrence,
                ChatAnchor::TranscriptReady => 0,
            };
            if matched == wanted {
                true
            } else {
                matched += 1;
                false
            }
        })
        .unwrap_or_else(|| panic!("chat anchor {anchor:?} is absent"))
}

pub fn chat_row(fixture: &str, anchor: ChatAnchor) -> serde_json::Value {
    let rows = chat_rows(fixture);
    rows[chat_anchor_index(&rows, anchor)].clone()
}

pub fn chat_rows_before(fixture: &str, anchor: ChatAnchor) -> Vec<serde_json::Value> {
    let rows = chat_rows(fixture);
    rows[..chat_anchor_index(&rows, anchor)].to_vec()
}

pub fn chat_rows_through(fixture: &str, anchor: ChatAnchor) -> Vec<serde_json::Value> {
    let rows = chat_rows(fixture);
    rows[..=chat_anchor_index(&rows, anchor)].to_vec()
}

pub fn chat_rows_from_through(
    fixture: &str,
    start: ChatAnchor,
    end: ChatAnchor,
) -> Vec<serde_json::Value> {
    let rows = chat_rows(fixture);
    let start = chat_anchor_index(&rows, start);
    let end = chat_anchor_index(&rows, end);
    assert!(start <= end, "chat anchor range is ordered");
    rows[start..=end].to_vec()
}

pub fn chat_feed_through(agent: &str, fixture: &str, anchor: ChatAnchor) -> Vec<Msg> {
    seq([
        chat_base(agent),
        vec![batch(agent, 10, chat_rows_through(fixture, anchor))],
    ])
}

pub fn chat_session_id(fixture: &str) -> String {
    chat_rows(fixture)
        .into_iter()
        .find_map(|row| {
            row.get("sessionId")
                .or_else(|| row.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .expect("chat fixture carries a session id")
}

/// Rows of a graduated a2a carrier capture: a redacted, provenance-stamped
/// recording of a real Claude 2.1.240 receiving an agent message over each
/// carrier (`crates/amux/tests/fixtures/a2a/`).
pub fn a2a_rows(fixture: &str) -> Vec<serde_json::Value> {
    let raw = match fixture {
        "socket_delivery" => {
            include_str!("../../../amux/tests/fixtures/a2a/socket_delivery.jsonl")
        }
        "pty_delivery" => include_str!("../../../amux/tests/fixtures/a2a/pty_delivery.jsonl"),
        "mcp_tools" => include_str!("../../../amux/tests/fixtures/a2a/mcp_tools.jsonl"),
        other => panic!("unknown a2a fixture {other}"),
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture row parses"))
        .collect()
}

/// A local claude agent with a live structured stream (complete window):
/// the base every feed chapter folds fixture batches onto.
pub fn chat_base(agent: &str) -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent(agent, "nova")),
        ],
        synced(),
        vec![
            stream(agent, StreamMsg::Opened { truncated: false }),
            stream(agent, StreamMsg::ReplayComplete),
        ],
    ])
}

/// The base plus one coalesced batch of a fixture's rows.
pub fn chat_feed(agent: &str, fixture: &str) -> Vec<Msg> {
    seq([chat_base(agent), vec![batch(agent, 10, chat_rows(fixture))]])
}

/// The folded Claude layer for an agent.
pub fn claude_layer<'m>(model: &'m Model, agent: &str) -> &'m ClaudeLayer {
    model.claude(agent_id(agent)).expect("claude layer folded")
}

/// Backend rows derived from every recording in the canonical Codex corpus.
pub fn codex_fixture_rows() -> Vec<serde_json::Value> {
    [
        include_str!("../../../amux/tests/fixtures/rows/codex/initialize_and_start.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/turn_round_trip.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/approval_allow.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/approval_deny.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/interrupt.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/thread_list_and_resume.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/dynamic_tools.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/inject_idle.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/inject_busy.rows.jsonl"),
        include_str!("../../../amux/tests/fixtures/rows/codex/two_assistant_messages.rows.jsonl"),
    ]
    .into_iter()
    .flat_map(str::lines)
    .filter(|line| !line.trim().is_empty())
    .map(|line| serde_json::from_str(line).expect("Codex fixture row parses"))
    .collect()
}

pub fn codex_base(agent: &str) -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&a_codex_agent(agent, "nova")),
        ],
        synced(),
        vec![
            stream(agent, StreamMsg::Opened { truncated: false }),
            stream(agent, StreamMsg::ReplayComplete),
        ],
    ])
}

pub fn codex_layer<'m>(model: &'m Model, agent: &str) -> &'m CodexLayer {
    model.codex(agent_id(agent)).expect("Codex layer folded")
}

// --- Folding --------------------------------------------------------------

/// Fold a sequence into a Model, discarding effects (replay semantics).
pub fn fold(msgs: impl IntoIterator<Item = Msg>) -> Model {
    fold_with_effects(msgs).0
}

/// Fold a sequence, collecting every effect in order.
pub fn fold_with_effects(msgs: impl IntoIterator<Item = Msg>) -> (Model, Vec<Effect>) {
    let mut model = Model::default();
    let mut effects = Vec::new();
    for msg in msgs {
        effects.extend(update(&mut model, msg));
    }
    (model, effects)
}

/// Concatenate sequence fragments.
pub fn seq(fragments: impl IntoIterator<Item = Vec<Msg>>) -> Vec<Msg> {
    fragments.into_iter().flatten().collect()
}

/// Every chapter's registered sequences: the differential spec wraps each of
/// them (`wire_free::differential_fold_matches_live_state_after_every_msg`).
pub fn all_sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let mut sequences = Vec::new();
    sequences.extend(crate::attachments::sequences());
    sequences.extend(crate::draft::sequences());
    sequences.extend(crate::queue::sequences());
    sequences.extend(crate::connection::sequences());
    sequences.extend(crate::inventory::sequences());
    sequences.extend(crate::ops::sequences());
    sequences.extend(crate::sessions::sequences());
    sequences.extend(crate::attention::sequences());
    sequences.extend(crate::feed_replay::sequences());
    sequences.extend(crate::feed_turns::sequences());
    sequences.extend(crate::feed_tools::sequences());
    sequences.extend(crate::claude_runs::sequences());
    sequences.extend(crate::feed_edges::sequences());
    sequences.extend(crate::asks::sequences());
    sequences.extend(crate::phase::sequences());
    sequences.extend(crate::write::sequences());
    sequences.extend(crate::codex_feed::sequences());
    sequences.extend(crate::codex_asks::sequences());
    sequences.extend(crate::codex_write::sequences());
    sequences.extend(crate::model_effort::sequences());
    sequences.extend(crate::provider_commands::sequences());
    sequences.extend(crate::todos::sequences());
    sequences.extend(crate::codex_agreement::sequences());
    sequences.extend(crate::claude_agreement::sequences());
    sequences.extend(crate::a2a_fleet::sequences());
    sequences.extend(crate::a2a_claude_inbound::sequences());
    sequences.extend(crate::a2a_claude_send_row::sequences());
    sequences.extend(crate::a2a_codex_inbound::sequences());
    sequences.extend(crate::a2a_family_needs::sequences());
    sequences.extend(crate::a2a_completed_row::sequences());
    sequences
}
