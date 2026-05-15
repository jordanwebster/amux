//! Inventory task: tracks host topology and agent inventory, emits semantic
//! deltas to the notification stream.
//!
//! Architecture:
//! - One `SubscribeRoutingEvents` stream from the local server tracks the host
//!   set; events feed the parent loop.
//! - For each known host, one `SubscribeAgentEvents(host_id)` task pushes
//!   agent events into a shared mpsc consumed by the parent loop. The parent
//!   maintains a global `HashMap<AgentId, AgentEntry>` (the `AgentCache`) and
//!   emits `AgentAdded` / `AgentUpdated` / `AgentRemoved` notifications.
//! - When a host appears (`HostUp` mid-life) a new per-host agent task is
//!   spawned. When a host disappears (`HostDown`) its task is cancelled and
//!   the parent synthesises `AgentRemoved` for every agent it had on that
//!   host (the protocol's `HostDown` does not emit per-agent events).
//! - `AgentsSnapshot` is emitted exactly once, after every host known at
//!   startup has delivered its agent-stream `SnapshotComplete`.

use std::collections::{HashMap, HashSet};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::agent_cache::{AgentCache, InsertOutcome};
use super::error::disconnect_reason;
use super::notification::{self, Notification};
use super::types;

const HOST_AGENT_EVENT_BUFFER: usize = 512;

pub(crate) async fn run(client: amux::Client, tx: mpsc::Sender<Notification>, agents: AgentCache) {
    let routing = match client.subscribe_routing_events().await {
        Ok(stream) => stream,
        Err(error) => {
            notification::send(
                &tx,
                Notification::Disconnected {
                    reason: disconnect_reason(error),
                },
            )
            .await;
            return;
        }
    };

    // Phase 1: drain routing snapshot to learn the initial host set.
    let mut hosts: HashMap<types::HostId, HostRecord> = HashMap::new();
    loop {
        match routing.recv().await {
            Ok(amux::protocol::RoutingEvent::HostUp { host, route }) => {
                hosts.insert(host.id, HostRecord { host, route });
            }
            Ok(amux::protocol::RoutingEvent::HostDown { id, .. }) => {
                hosts.remove(&id);
            }
            Ok(amux::protocol::RoutingEvent::SnapshotComplete) => break,
            Ok(amux::protocol::RoutingEvent::Unknown) => {}
            Err(error) => {
                notification::send(
                    &tx,
                    Notification::Disconnected {
                        reason: disconnect_reason(error),
                    },
                )
                .await;
                return;
            }
        }
    }

    let hosts_snapshot: Vec<_> = hosts.values().map(|record| record.host.clone()).collect();
    notification::send(&tx, Notification::HostsSnapshot(hosts_snapshot)).await;

    // Phase 2: spawn one per-host agent subscription task per known host.
    // The agent_tx handle stays alive in scope so we can clone it for new
    // host tasks spawned mid-life when a HostUp arrives.
    let (agent_tx, mut agent_rx) = mpsc::channel::<HostAgentEvent>(HOST_AGENT_EVENT_BUFFER);
    let mut subs: HashMap<types::HostId, HostSubscription> = HashMap::new();
    let mut snapshot_pending: HashSet<types::HostId> = HashSet::new();
    for host_id in hosts.keys() {
        snapshot_pending.insert(*host_id);
        subs.insert(
            *host_id,
            spawn_host_agent_task(client.clone(), *host_id, agent_tx.clone()),
        );
    }

    let mut initial_snapshot_emitted = false;
    if snapshot_pending.is_empty() {
        emit_initial_snapshot(&tx, &agents).await;
        initial_snapshot_emitted = true;
    }

    // Phase 3: steady state. Multiplex routing events (host churn) and
    // per-host agent events (inventory deltas).
    loop {
        tokio::select! {
            event = routing.recv() => match event {
                Ok(amux::protocol::RoutingEvent::HostUp { host, route }) => {
                    let host_id = host.id;
                    let was_new = !hosts.contains_key(&host_id);
                    hosts.insert(host_id, HostRecord { host: host.clone(), route });
                    if was_new {
                        subs.insert(
                            host_id,
                            spawn_host_agent_task(client.clone(), host_id, agent_tx.clone()),
                        );
                        notification::send(&tx, Notification::HostAdded(host)).await;
                    }
                }
                Ok(amux::protocol::RoutingEvent::HostDown { id, .. }) => {
                    hosts.remove(&id);
                    if let Some(sub) = subs.remove(&id) {
                        sub.cancel();
                    }
                    for agent_id in agents.remove_host(id).await {
                        notification::send(
                            &tx,
                            Notification::AgentRemoved { id: agent_id, reason: None },
                        )
                        .await;
                    }
                    notification::send(&tx, Notification::HostRemoved { id, reason: None }).await;
                    // A host that hadn't yet delivered its initial agent
                    // snapshot can disappear before completing; stop waiting
                    // on it so the global snapshot isn't blocked forever.
                    snapshot_pending.remove(&id);
                    if !initial_snapshot_emitted && snapshot_pending.is_empty() {
                        emit_initial_snapshot(&tx, &agents).await;
                        initial_snapshot_emitted = true;
                    }
                }
                Ok(amux::protocol::RoutingEvent::SnapshotComplete | amux::protocol::RoutingEvent::Unknown) => {}
                Err(error) => {
                    notification::send(
                        &tx,
                        Notification::Disconnected { reason: disconnect_reason(error) },
                    )
                    .await;
                    return;
                }
            },
            Some(event) = agent_rx.recv() => match event {
                HostAgentEvent::Up { host_id, agent } => {
                    let Some(record) = hosts.get(&host_id) else {
                        // Stale event after the host already disappeared.
                        continue;
                    };
                    let entry = types::AgentEntry { agent, route: record.route.clone() };
                    match agents.insert_with_outcome(entry).await {
                        InsertOutcome::Added(agent) => {
                            notification::send(&tx, Notification::AgentAdded(agent)).await;
                        }
                        InsertOutcome::Updated(agent) => {
                            notification::send(&tx, Notification::AgentUpdated(agent)).await;
                        }
                        InsertOutcome::Same => {}
                    }
                }
                HostAgentEvent::Down { agent_id, reason } => {
                    if agents.remove(agent_id).await {
                        notification::send(
                            &tx,
                            Notification::AgentRemoved { id: agent_id, reason },
                        )
                        .await;
                    }
                }
                HostAgentEvent::SnapshotComplete { host_id } => {
                    snapshot_pending.remove(&host_id);
                    if !initial_snapshot_emitted && snapshot_pending.is_empty() {
                        emit_initial_snapshot(&tx, &agents).await;
                        initial_snapshot_emitted = true;
                    }
                }
                HostAgentEvent::Failed { host_id, error } => {
                    tracing::warn!(host_id = %host_id, error = %error, "agent event subscription ended");
                    subs.remove(&host_id);
                    for agent_id in agents.remove_host(host_id).await {
                        notification::send(
                            &tx,
                            Notification::AgentRemoved { id: agent_id, reason: None },
                        )
                        .await;
                    }
                    snapshot_pending.remove(&host_id);
                    if !initial_snapshot_emitted && snapshot_pending.is_empty() {
                        emit_initial_snapshot(&tx, &agents).await;
                        initial_snapshot_emitted = true;
                    }
                }
            },
        }
    }
}

async fn emit_initial_snapshot(tx: &mpsc::Sender<Notification>, agents: &AgentCache) {
    let snapshot = agents.snapshot().await;
    notification::send(tx, Notification::AgentsSnapshot(snapshot)).await;
    notification::send(tx, Notification::Connected).await;
}

struct HostRecord {
    host: types::Host,
    route: types::Route,
}

struct HostSubscription {
    cancel: Option<oneshot::Sender<()>>,
    _task: JoinHandle<()>,
}

impl HostSubscription {
    fn cancel(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

enum HostAgentEvent {
    Up {
        host_id: types::HostId,
        agent: types::Agent,
    },
    Down {
        agent_id: types::AgentId,
        reason: Option<String>,
    },
    SnapshotComplete {
        host_id: types::HostId,
    },
    Failed {
        host_id: types::HostId,
        error: String,
    },
}

fn spawn_host_agent_task(
    client: amux::Client,
    host_id: types::HostId,
    tx: mpsc::Sender<HostAgentEvent>,
) -> HostSubscription {
    let (cancel_tx, mut cancel_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let stream = match client.subscribe_agent_events(host_id).await {
            Ok(stream) => stream,
            Err(error) => {
                let _ = tx
                    .send(HostAgentEvent::Failed {
                        host_id,
                        error: error.to_string(),
                    })
                    .await;
                return;
            }
        };

        loop {
            let event = tokio::select! {
                _ = &mut cancel_rx => {
                    let _ = stream.cancel().await;
                    return;
                }
                event = stream.recv() => event,
            };
            match event {
                Ok(amux::protocol::AgentEvent::AgentUp {
                    agent_id,
                    host_id: agent_host_id,
                    name,
                    command,
                    working_dir,
                    agent_type,
                    io_protocols,
                    readonly,
                    args,
                    created_at,
                }) => {
                    let agent = types::Agent {
                        id: agent_id,
                        host_id: agent_host_id,
                        name,
                        command,
                        working_dir,
                        agent_type,
                        io_protocols,
                        readonly,
                        args,
                        created_at,
                    };
                    if tx
                        .send(HostAgentEvent::Up { host_id, agent })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(amux::protocol::AgentEvent::AgentDown { agent_id }) => {
                    if tx
                        .send(HostAgentEvent::Down {
                            agent_id,
                            reason: None,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(amux::protocol::AgentEvent::SnapshotComplete) => {
                    if tx
                        .send(HostAgentEvent::SnapshotComplete { host_id })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(amux::protocol::AgentEvent::Unknown) => {}
                Err(error) => {
                    let _ = tx
                        .send(HostAgentEvent::Failed {
                            host_id,
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
            }
        }
    });
    HostSubscription {
        cancel: Some(cancel_tx),
        _task: task,
    }
}
