//! Inventory task: tracks host topology and aggregate agent inventory, emits
//! semantic deltas to the notification stream.

use std::collections::HashMap;

use tokio::sync::mpsc;

use super::agent_cache::{AgentCache, InsertOutcome};
use super::error::disconnect_reason;
use super::notification::{self, Notification};
use super::types;

struct PendingAgents {
    events: HashMap<types::HostId, Vec<amux::AgentEvent>>,
    snapshot_ready: bool,
}

impl PendingAgents {
    fn new() -> Self {
        Self {
            events: HashMap::new(),
            snapshot_ready: false,
        }
    }

    fn has_pending(&self) -> bool {
        !self.events.is_empty()
    }
}

pub(crate) async fn run(client: amux::Client, tx: mpsc::Sender<Notification>, agents: AgentCache) {
    let mut hosts_stream = match client.subscribe_hosts().await {
        Ok(stream) => stream,
        Err(error) => {
            send_disconnected(&tx, error).await;
            return;
        }
    };
    let mut agent_stream = match client.subscribe_agents().await {
        Ok(stream) => stream,
        Err(error) => {
            send_disconnected(&tx, error).await;
            return;
        }
    };

    let mut hosts = match drain_host_snapshot(&mut hosts_stream, &tx).await {
        Some(hosts) => hosts,
        None => return,
    };
    let hosts_snapshot: Vec<_> = hosts.values().cloned().collect();
    notification::send(&tx, Notification::HostsSnapshot(hosts_snapshot)).await;

    let mut pending_agents = PendingAgents::new();
    if !drain_agent_snapshot(&mut agent_stream, &tx, &agents, &hosts, &mut pending_agents).await {
        return;
    }
    maybe_emit_initial_snapshot(&tx, &agents, &mut pending_agents).await;

    loop {
        tokio::select! {
            event = hosts_stream.recv() => match event {
                Ok(amux::HostEvent::HostUpdated { host }) => {
                    let host_id = host.id;
                    hosts.insert(host_id, host.clone());
                    notification::send(&tx, Notification::HostUpdated(host)).await;
                    apply_pending_for_host(&tx, &agents, host_id, &mut pending_agents).await;
                    maybe_emit_initial_snapshot(&tx, &agents, &mut pending_agents).await;
                }
                Ok(amux::HostEvent::HostRemoved { id }) => {
                    hosts.remove(&id);
                    pending_agents.events.remove(&id);
                    notification::send(&tx, Notification::HostRemoved { id, reason: None }).await;
                    maybe_emit_initial_snapshot(&tx, &agents, &mut pending_agents).await;
                }
                Ok(amux::HostEvent::SnapshotComplete) => {}
                Err(error) => {
                    send_disconnected(&tx, error).await;
                    return;
                }
            },
            event = agent_stream.recv() => match event {
                Ok(event) => {
                    apply_agent_event(&tx, &agents, event, &hosts, &mut pending_agents).await;
                    maybe_emit_initial_snapshot(&tx, &agents, &mut pending_agents).await;
                }
                Err(error) => {
                    send_disconnected(&tx, error).await;
                    return;
                }
            },
        }
    }
}

async fn drain_host_snapshot(
    stream: &mut amux::HostEventStream,
    tx: &mpsc::Sender<Notification>,
) -> Option<HashMap<types::HostId, types::Host>> {
    let mut hosts = HashMap::new();
    loop {
        match stream.recv().await {
            Ok(amux::HostEvent::HostUpdated { host }) => {
                hosts.insert(host.id, host);
            }
            Ok(amux::HostEvent::HostRemoved { id }) => {
                hosts.remove(&id);
            }
            Ok(amux::HostEvent::SnapshotComplete) => return Some(hosts),
            Err(error) => {
                send_disconnected(tx, error).await;
                return None;
            }
        }
    }
}

async fn drain_agent_snapshot(
    stream: &mut amux::AgentEventStream,
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    hosts: &HashMap<types::HostId, types::Host>,
    pending: &mut PendingAgents,
) -> bool {
    loop {
        match stream.recv().await {
            Ok(amux::AgentEvent::SnapshotComplete) => {
                pending.snapshot_ready = true;
                return true;
            }
            Ok(event) => apply_agent_event(tx, agents, event, hosts, pending).await,
            Err(error) => {
                send_disconnected(tx, error).await;
                return false;
            }
        }
    }
}

async fn apply_agent_event(
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    event: amux::AgentEvent,
    hosts: &HashMap<types::HostId, types::Host>,
    pending: &mut PendingAgents,
) {
    if let Some(host_id) = agent_event_host_id(&event)
        && !hosts.contains_key(&host_id)
    {
        pending.events.entry(host_id).or_default().push(event);
        return;
    }
    apply_known_agent_event(tx, agents, event).await;
}

async fn apply_known_agent_event(
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    event: amux::AgentEvent,
) {
    match event {
        amux::AgentEvent::AgentUp { agent } | amux::AgentEvent::AgentUpdated { agent } => {
            match agents.insert_with_outcome(agent).await {
                InsertOutcome::Added(agent) => {
                    notification::send(tx, Notification::AgentAdded(agent)).await;
                }
                InsertOutcome::Updated(agent) => {
                    notification::send(tx, Notification::AgentUpdated(agent)).await;
                }
                InsertOutcome::Same => {}
            }
        }
        amux::AgentEvent::AgentDown { agent_id } => {
            if agents.remove(agent_id).await {
                notification::send(
                    tx,
                    Notification::AgentRemoved {
                        id: agent_id,
                        reason: None,
                    },
                )
                .await;
            }
        }
        amux::AgentEvent::SnapshotComplete => {}
    }
}

fn agent_event_host_id(event: &amux::AgentEvent) -> Option<types::HostId> {
    match event {
        amux::AgentEvent::AgentUp { agent } | amux::AgentEvent::AgentUpdated { agent } => {
            Some(agent.host_id)
        }
        amux::AgentEvent::AgentDown { .. } | amux::AgentEvent::SnapshotComplete => None,
    }
}

async fn apply_pending_for_host(
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    host_id: types::HostId,
    pending: &mut PendingAgents,
) {
    let Some(events) = pending.events.remove(&host_id) else {
        return;
    };
    for event in events {
        apply_known_agent_event(tx, agents, event).await;
    }
}

async fn maybe_emit_initial_snapshot(
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    pending: &mut PendingAgents,
) {
    if !pending.snapshot_ready || pending.has_pending() {
        return;
    }
    pending.snapshot_ready = false;
    let snapshot = agents.snapshot().await;
    notification::send(tx, Notification::AgentsSnapshot(snapshot)).await;
    notification::send(tx, Notification::Connected).await;
}

async fn send_disconnected(tx: &mpsc::Sender<Notification>, error: amux::ClientError) {
    notification::send(
        tx,
        Notification::Disconnected {
            reason: disconnect_reason(error),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    fn agent_up(agent_id: Uuid, host_id: Uuid) -> amux::AgentEvent {
        amux::AgentEvent::AgentUp {
            agent: amux::Agent {
                id: agent_id,
                host_id,
                name: Some("agent".to_string()),
                command: "test-agent".to_string(),
                working_dir: std::env::temp_dir(),
                agent_type: "test-agent".to_string(),
                io_protocols: Vec::new(),
                readonly: false,
                args: Vec::new(),
                created_at: Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn unknown_host_agent_events_are_pending_until_host_arrives() {
        let host_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);
        let agents = AgentCache::new();
        let (tx, mut rx) = mpsc::channel(8);
        let hosts = HashMap::new();
        let mut pending = PendingAgents::new();

        apply_agent_event(
            &tx,
            &agents,
            agent_up(agent_id, host_id),
            &hosts,
            &mut pending,
        )
        .await;

        assert!(agents.snapshot().await.is_empty());
        assert!(rx.try_recv().is_err());
        assert!(pending.has_pending());

        apply_pending_for_host(&tx, &agents, host_id, &mut pending).await;

        assert_eq!(agents.snapshot().await[0].id, agent_id);
        assert!(
            matches!(rx.try_recv(), Ok(Notification::AgentAdded(agent)) if agent.id == agent_id)
        );
        assert!(!pending.has_pending());
    }
}
