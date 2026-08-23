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
        agent_type: "claude".to_string(),
        io_protocols: vec![
            "terminal_v1".to_string(),
            "claude_pty_transcript_v1".to_string(),
        ],
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
        agent_type: "codex".to_string(),
        io_protocols: vec!["terminal_v1".to_string(), "codex_sdk_v1".to_string()],
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
        agent_type: amux::AgentType::Claude,
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
            error: amux_ui::OpError {
                message: message.to_string(),
                auth_required: false,
                subscription_required: false,
            },
        },
    )
}

pub fn op_failed_auth(op: OpId) -> Msg {
    op_result(
        op,
        OpOutcome::Error {
            error: amux_ui::OpError {
                message: "Invalid or missing credentials".to_string(),
                auth_required: true,
                subscription_required: false,
            },
        },
    )
}

pub fn op_failed_subscription(op: OpId) -> Msg {
    op_result(
        op,
        OpOutcome::Error {
            error: amux_ui::OpError {
                message: "Cloud subscription required".to_string(),
                auth_required: false,
                subscription_required: true,
            },
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

/// Rows of a committed chat-v1 fixture: a redacted, provenance-stamped
/// capture of the `claude_pty_transcript_v1` stream from a real claude
/// (`crates/amux/tests/fixtures/chat-v1/`, Phase 0). Referenced across
/// crates by compile-time include so the spec suite stays IO-free.
pub fn chat_rows(fixture: &str) -> Vec<serde_json::Value> {
    let raw = match fixture {
        "pong" => include_str!("../../../amux/tests/fixtures/chat-v1/pong.rows.jsonl"),
        "tools" => include_str!("../../../amux/tests/fixtures/chat-v1/tools.rows.jsonl"),
        "permission" => include_str!("../../../amux/tests/fixtures/chat-v1/permission.rows.jsonl"),
        "question_single" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/question_single.rows.jsonl")
        }
        "question_multi" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/question_multi.rows.jsonl")
        }
        "interrupt" => include_str!("../../../amux/tests/fixtures/chat-v1/interrupt.rows.jsonl"),
        "plan_approve" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/plan_approve.rows.jsonl")
        }
        "plan_reject" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/plan_reject.rows.jsonl")
        }
        "compact" => include_str!("../../../amux/tests/fixtures/chat-v1/compact.rows.jsonl"),
        "mode_cycle" => include_str!("../../../amux/tests/fixtures/chat-v1/mode_cycle.rows.jsonl"),
        "permission_session" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/permission_session.rows.jsonl")
        }
        "permission_deny_feedback" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/permission_deny_feedback.rows.jsonl")
        }
        "question_tabs" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/question_tabs.rows.jsonl")
        }
        "question_mixed" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/question_mixed.rows.jsonl")
        }
        "question_other_single" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/question_other_single.rows.jsonl")
        }
        "plan_auto" => include_str!("../../../amux/tests/fixtures/chat-v1/plan_auto.rows.jsonl"),
        "prompt_multiline" => {
            include_str!("../../../amux/tests/fixtures/chat-v1/prompt_multiline.rows.jsonl")
        }
        other => panic!("unknown chat fixture {other}"),
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture row parses"))
        .collect()
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

/// The base plus a fixture PREFIX (rows before `end`): how the spec
/// observes mid-lifecycle states on captured reality.
pub fn chat_feed_prefix(agent: &str, fixture: &str, end: usize) -> Vec<Msg> {
    seq([
        chat_base(agent),
        vec![batch(agent, 10, chat_rows(fixture)[..end].to_vec())],
    ])
}

/// The folded Claude layer for an agent.
pub fn claude_layer<'m>(model: &'m Model, agent: &str) -> &'m ClaudeLayer {
    model.claude(agent_id(agent)).expect("claude layer folded")
}

/// Structural rows from the provenance-stamped P5b Codex capture fixture.
pub fn codex_fixture_rows() -> Vec<serde_json::Value> {
    include_str!("../../../amux/tests/fixtures/codex_backend/rows.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("Codex fixture row parses"))
        .collect()
}

/// The `codex_sdk_v1` rows a graduated Codex capture produces, projected
/// from the recorded wire exactly as the daemon projects a live one: the
/// notification's method becomes the row's `type` and its params become the
/// rest. Reading the capture rather than a transcription of it keeps the
/// spec anchored to what Codex actually sent.
pub fn codex_capture_rows(fixture: &str) -> Vec<serde_json::Value> {
    let raw = match fixture {
        "a2a_tools" => {
            include_str!("../../../amux/tests/fixtures/codex_backend/a2a_tools.io.jsonl")
        }
        other => panic!("unknown codex capture {other}"),
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry.get("dir").and_then(serde_json::Value::as_str) == Some("stdout"))
        .filter_map(|entry| {
            let message: serde_json::Value =
                serde_json::from_str(entry.get("line")?.as_str()?).ok()?;
            let method = message.get("method")?.as_str()?.to_string();
            let mut row = match message.get("params") {
                Some(serde_json::Value::Object(params)) => params.clone(),
                _ => serde_json::Map::new(),
            };
            row.insert("type".to_string(), serde_json::Value::String(method));
            Some(serde_json::Value::Object(row))
        })
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
    sequences.extend(crate::connection::sequences());
    sequences.extend(crate::inventory::sequences());
    sequences.extend(crate::ops::sequences());
    sequences.extend(crate::sessions::sequences());
    sequences.extend(crate::attention::sequences());
    sequences.extend(crate::feed_replay::sequences());
    sequences.extend(crate::feed_turns::sequences());
    sequences.extend(crate::feed_tools::sequences());
    sequences.extend(crate::feed_edges::sequences());
    sequences.extend(crate::asks::sequences());
    sequences.extend(crate::phase::sequences());
    sequences.extend(crate::write::sequences());
    sequences.extend(crate::codex_feed::sequences());
    sequences.extend(crate::codex_asks::sequences());
    sequences.extend(crate::codex_write::sequences());
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
