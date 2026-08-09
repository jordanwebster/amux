//! Spec harness: fixture builders for Msg sequences and pure fold helpers.
//!
//! No tokio, no network, no clocks — chapters build ordered Msg vectors,
//! fold them, and assert on the Model. Ids and times are derived
//! deterministically from names so sequences replay identically forever.

use amux::{Agent, AgentId, Capabilities, HostEntry, HostId, HostTrustStatus};
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
            "claude_raw_v1".to_string(),
            "claude_pty_transcript_v1".to_string(),
        ],
        readonly: false,
        args: Vec::new(),
        created_at: t0(),
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
    sequences
}
