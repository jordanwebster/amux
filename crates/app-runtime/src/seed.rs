//! Remembered states a driving build writes into an account's store.
//!
//! The screens a launch draws from its store (a machine that stopped
//! answering, an agent a machine said was gone, a conversation opened before
//! anything has connected) can only be photographed if a store holding exactly
//! that already exists. These write one through the same store, runtime and
//! reducer a phone uses, and read it back the way a phone does.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use model::{Agent, AgentId, DisconnectReason, HostEntry, HostId, RelayConnection};
use serde::Deserialize;
use serde_json::Value;
use store::{FleetDelta, Store};
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::{ChatStreamMsg, Msg, ReplayFactsDto, ReplayOutcomeDto, StreamEntry};

use crate::cache::store_path;
use crate::projection::{Event, Projection};

/// What one account's store should remember.
#[derive(Debug, Deserialize)]
pub struct Remembered {
    /// This device, which the store records and a launch leaves out of the
    /// machines it draws.
    pub local: HostId,
    pub hosts: Vec<HostEntry>,
    pub agents: Vec<Agent>,
    /// Agents their machine has since said are gone.
    #[serde(default)]
    pub removed: Vec<AgentId>,
    /// Transcript rows each conversation's stream delivered before the
    /// device lost its connection, in the provider's own shape.
    #[serde(default)]
    pub chats: BTreeMap<AgentId, Vec<Value>>,
    /// Transcript rows a later connection delivered for a conversation in
    /// `chats` after its machine could no longer serve the rows in between,
    /// so the store holds a break in that conversation's history.
    #[serde(default)]
    pub after_gap: BTreeMap<AgentId, Vec<Value>>,
}

/// How long any one step of writing or reading a store may take before the
/// state is reported as unbuildable rather than waited on forever.
const STEP: Duration = Duration::from_secs(10);

/// Replaces an account's store with one remembering exactly `remembered`.
pub async fn seed(cache_dir: &Path, account: &str, remembered: Remembered) -> Result<(), String> {
    let path = store_path(cache_dir, account);
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
    std::fs::create_dir_all(path.parent().expect("a store path has a parent"))
        .map_err(|error| error.to_string())?;

    let store = Store::open(&path)
        .await
        .map_err(|error| format!("{error:?}"))?;
    let generations = store
        .generations()
        .for_provider("codex")
        .ok_or("the store has no generations")?;
    let mut revision = 0;
    let mut deltas = Vec::new();
    for host in &remembered.hosts {
        deltas.push(FleetDelta::Host {
            host: host.clone(),
            revision: 0,
        });
    }
    for agent in &remembered.agents {
        revision += 1;
        deltas.push(FleetDelta::AgentUp {
            agent: agent.clone(),
            revision,
        });
    }
    for id in &remembered.removed {
        let agent = remembered
            .agents
            .iter()
            .find(|agent| agent.id == *id)
            .ok_or_else(|| format!("removed agent {id} was never remembered"))?;
        revision += 1;
        deltas.push(FleetDelta::AgentDown {
            host_id: agent.host_id,
            agent_id: *id,
            revision,
            reason: None,
        });
    }
    for delta in deltas {
        store
            .apply_fleet(generations, delta)
            .await
            .map_err(|error| format!("{error:?}"))?;
    }
    let (kind, key) = ui_runtime::LOCAL_HOST_VIEW;
    store
        .view_set(kind, key, &remembered.local.to_string())
        .await
        .map_err(|error| format!("{error:?}"))?;
    store.close().await;

    let mut after_gap = remembered.after_gap;
    for (agent, rows) in remembered.chats {
        let through = rows.len() as u64;
        let facts = ReplayFactsDto {
            retained_from: 1,
            through,
            selected_from: 1,
            reset_at: 0,
            outcome: ReplayOutcomeDto::Continuous,
        };
        deliver(&path, agent, facts, 1, rows).await?;
        let Some(rows) = after_gap.remove(&agent) else {
            continue;
        };
        // One row past what the store holds is gone for good: the machine
        // retains only what follows it.
        let resumed = through + 2;
        let facts = ReplayFactsDto {
            retained_from: resumed,
            through: resumed + rows.len() as u64 - 1,
            selected_from: resumed,
            reset_at: 0,
            outcome: ReplayOutcomeDto::Truncated {
                missing_after: through,
            },
        };
        deliver(&path, agent, facts, resumed, rows).await?;
        let mut runtime = remembered_runtime(&path).await?;
        runtime.open_chat(agent);
        until(&mut runtime, |runtime| {
            runtime.model().chat(agent).is_some_and(|chat| {
                chat.is_painted()
                    && chat
                        .boundaries
                        .iter()
                        .any(|at| at.boundary == ui_state::Boundary::Gap)
            })
        })
        .await
        .map_err(|_| format!("the break in {agent}'s history was never stored"))?;
    }
    if let Some(agent) = after_gap.keys().next() {
        return Err(format!("{agent} has rows after a gap but none before it"));
    }
    Ok(())
}

/// Opens `agent`'s conversation on a fresh runtime and has its stream deliver
/// `rows`, numbered from `first`, as a replay described by `facts`, returning
/// once the store has committed every one.
async fn deliver(
    path: &Path,
    agent: AgentId,
    facts: ReplayFactsDto,
    first: u64,
    rows: Vec<Value>,
) -> Result<(), String> {
    let mut runtime = remembered_runtime(path).await?;
    runtime.open_chat(agent);
    until(&mut runtime, |runtime| {
        runtime
            .model()
            .chat(agent)
            .is_some_and(|chat| chat.is_painted())
    })
    .await?;
    let attempt = runtime
        .model()
        .chat(agent)
        .ok_or("the conversation did not open")?
        .stream_attempt;
    let at = chrono::Utc::now();
    let last = first + rows.len() as u64 - 1;
    let events = [
        ChatStreamMsg::Opened { facts, at },
        ChatStreamMsg::Batch {
            at,
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(index, row)| StreamEntry::observed(first + index as u64, at, row))
                .collect(),
        },
        ChatStreamMsg::ReplayComplete { at },
    ];
    for event in events {
        runtime
            .shell_edge()
            .report(Msg::ChatStream {
                agent,
                attempt,
                event,
            })
            .await
            .map_err(|_| "the runtime stopped")?;
    }
    until(&mut runtime, |runtime| {
        runtime.model().chat(agent).is_some_and(|chat| {
            chat.pending_bytes() == 0
                && !chat.live_only
                && chat
                    .head
                    .as_ref()
                    .is_some_and(|head| head.through() >= last)
        })
    })
    .await
}

/// What a phone draws for one conversation it opens before anything has
/// connected: the account's runtime installs its remembered fleet, opens the
/// conversation through its store and projects the window it painted.
pub async fn cached_chat(
    cache_dir: &Path,
    account: &str,
    agent: AgentId,
) -> Result<Vec<Event>, String> {
    let mut runtime = remembered_runtime(&store_path(cache_dir, account)).await?;
    runtime.open_chat(agent);
    until(&mut runtime, |runtime| {
        runtime
            .model()
            .chat(agent)
            .is_some_and(|chat| chat.is_painted())
    })
    .await?;
    let mut projection = Projection::default();
    projection.subscribe(agent);
    let mut events = Vec::new();
    projection.collect(
        runtime.model(),
        &RelayConnection::Disconnected {
            reason: DisconnectReason::Unreachable,
        },
        &mut events,
    );
    Ok(events)
}

/// A runtime on the store that never reaches a relay, once its remembered
/// fleet is installed.
async fn remembered_runtime(path: &Path) -> Result<Runtime, String> {
    let mut runtime = Runtime::start(
        Box::new(|| Box::pin(std::future::pending())),
        RuntimeOptions {
            store_path: Some(path.to_owned()),
            ..RuntimeOptions::default()
        },
    );
    until(&mut runtime, |runtime| !runtime.remembered_fleet_pending()).await?;
    Ok(runtime)
}

async fn until(runtime: &mut Runtime, ready: impl Fn(&Runtime) -> bool) -> Result<(), String> {
    tokio::time::timeout(STEP, async {
        while !ready(runtime) {
            if !runtime.next_message().await {
                return Err("the runtime stopped".to_string());
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| "the store never reached the remembered state".to_string())?
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ACCOUNT: &str = "door";

    fn remembered() -> Remembered {
        let agents: Vec<_> = [11, 12]
            .map(|id| {
                json!({
                    "id": format!("00000000-0000-0000-0000-0000000000{id}"),
                    "host_id": "00000000-0000-0000-0000-000000000001",
                    "name": format!("agent-{id}"), "command": "claude", "working_dir": "/work",
                    "kind": {"kind": "claude", "driver": "pty"}, "readonly": false, "args": [],
                    "created_at": "2026-09-16T12:00:00.000Z"
                })
            })
            .into();
        serde_json::from_value(json!({
            "local": "00000000-0000-0000-0000-000000000009",
            "hosts": [{
                "id": "00000000-0000-0000-0000-000000000001", "name": "studio",
                "online": false,
                "trust_status": "trusted"
            }],
            "agents": agents,
            "removed": ["00000000-0000-0000-0000-000000000012"],
            "chats": {"00000000-0000-0000-0000-000000000011": [
                {"type": "amux.transcript_ready"},
                {"type": "user", "uuid": "dddddddd-0000-4000-8000-000000000001",
                 "sessionId": "22222222-2222-4222-8222-222222222222",
                 "timestamp": "2026-09-16T12:00:00.000Z",
                 "message": {"role": "user", "content": "remember this"},
                 "origin": {"kind": "human"}, "promptSource": "typed"}
            ]}
        }))
        .unwrap()
    }

    /// A seeded store reads back as a phone would draw it: the survivor
    /// awaiting its machine, the removed agent absent, and the conversation's
    /// stored rows painted before anything connected.
    #[tokio::test]
    async fn a_seeded_store_reads_back_as_the_remembered_fleet_and_chat() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), ACCOUNT, remembered()).await.unwrap();

        let fleet = crate::cache::read_cached_fleet(root.path(), ACCOUNT).await;
        let fleet = serde_json::to_value(&fleet).unwrap();
        let agents = fleet["Fleet"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1, "{fleet}");
        assert_eq!(agents[0]["agent"]["name"], "agent-11");
        assert_eq!(agents[0]["awaiting"], true);

        let agent = "00000000-0000-0000-0000-000000000011".parse().unwrap();
        let events = cached_chat(root.path(), ACCOUNT, agent).await.unwrap();
        let events = serde_json::to_value(&events).unwrap();
        let feed = events
            .as_array()
            .unwrap()
            .iter()
            .find_map(|event| event.get("Feed"))
            .unwrap_or_else(|| panic!("no stored feed: {events}"));
        assert!(
            feed["append"].to_string().contains("remember this"),
            "{feed}"
        );
    }

    /// A conversation seeded with rows after a gap reads back with the break
    /// drawn between the rows from before it and the rows from after it.
    #[tokio::test]
    async fn a_seeded_gap_reads_back_between_the_rows_it_separates() {
        let root = tempfile::tempdir().unwrap();
        let mut remembered = remembered();
        let agent = "00000000-0000-0000-0000-000000000011".parse().unwrap();
        remembered.after_gap.insert(
            agent,
            vec![
                json!({"type": "user", "uuid": "dddddddd-0000-4000-8000-000000000002",
                "sessionId": "22222222-2222-4222-8222-222222222222",
                "timestamp": "2026-09-16T12:05:00.000Z",
                "message": {"role": "user", "content": "after the gap"},
                "origin": {"kind": "human"}, "promptSource": "typed"}),
            ],
        );
        seed(root.path(), ACCOUNT, remembered).await.unwrap();

        let events = cached_chat(root.path(), ACCOUNT, agent).await.unwrap();
        let events = serde_json::to_value(&events).unwrap();
        let rows: Vec<_> = events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event.get("Feed"))
            .flat_map(|feed| feed["append"].as_array().unwrap().clone())
            .map(|row| row.to_string())
            .collect();
        let at = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} row: {rows:?}"))
        };
        assert!(at("remember this") < at("\"history\""), "{rows:?}");
        assert!(at("\"history\"") < at("after the gap"), "{rows:?}");
    }
}
