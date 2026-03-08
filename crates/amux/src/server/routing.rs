//! Agent lifecycle, peer management, and route-level operations.
//!
//! Provides the building blocks used by [`connection`](super::connection) handlers:
//! agent creation/shutdown, subscription, peer broadcast, initial announcement
//! exchange on connect, and the peer disconnect cascade (route removal, agent/host
//! withdrawal propagation, stream cancellation).

use super::ServerUserState;
use super::connection::cancel_streams_matching;
use crate::agents::{AgentSession, SessionEvent, StopPolicy, SuspendedServerState};
use crate::buffer::MultiplexByteReader;
use crate::error::{AmuxError, Result};
use crate::message::{AgentType, CreateAgentRequest, DirectMessage, Message, TerminalSize};
use crate::route::Route;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

pub(super) async fn connection_tx(
    user_state: &Arc<RwLock<ServerUserState>>,
    link_name: &str,
) -> Option<mpsc::Sender<crate::message::Message>> {
    let us = user_state.read().await;
    us.routes.get(link_name).cloned()
}

/// Maximum local agents per user. Each agent holds a PTY and several tokio
/// tasks, so this provides an upper bound on per-user resource consumption.
const MAX_LOCAL_AGENTS: usize = 1024;

pub(super) async fn create_agent(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: CreateAgentRequest,
    user_id: Uuid,
) -> Result<()> {
    let mut us = user_state.write().await;

    if us.agents.len() >= MAX_LOCAL_AGENTS {
        return Err(AmuxError::ServerError(format!(
            "agent limit reached ({MAX_LOCAL_AGENTS} max)"
        )));
    }

    if us.registry.contains(&req.agent_id) {
        return Err(AmuxError::ServerError(format!(
            "Agent already exists: {}",
            &req.agent_id
        )));
    }

    if let Some(ref a) = req.name
        && us.registry.name_taken(a)
    {
        return Err(AmuxError::ServerError(format!(
            "Agent already exists: {}",
            a
        )));
    }

    let agent_id = req.agent_id;
    let mut session = match &req.agent_type {
        AgentType::Claude => AgentSession::Claude(crate::agents::ClaudeSession::new(&req)),
        #[cfg(any(debug_assertions, test))]
        AgentType::TestAgent(cmd) => {
            AgentSession::TestAgent(crate::agents::TestAgentSession::new(&req, cmd.clone()))
        }
    };
    let exit_handle = session.start()?;
    let info = session.to_agent();
    let name = req.name.clone();
    let command = session.command().to_string();
    let working_dir = session.working_dir().to_path_buf();
    us.agents.insert(agent_id, session);
    us.registry.register_local(info).map_err(|e| {
        AmuxError::ServerError(format!("failed to register local agent {agent_id}: {e}",))
    })?;

    // Task: Monitor exit handle and notify server when agent exits
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        let _ = exit_handle.await;
        let _ = event_tx
            .send(SessionEvent::Ended { agent_id, user_id })
            .await;
    });

    broadcast_to_peers(
        &mut us,
        &DirectMessage::AnnounceAgent {
            agent_id,
            name,
            command,
            working_dir,
            route: Route::empty(),
        },
        None,
    );

    tracing::info!(agent_id = %agent_id, name = ?req.name, agents = us.agents.len(), "agent created");
    Ok(())
}

/// Handle subscribe request by UUID.
pub(super) async fn handle_subscribe(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: &Uuid,
    terminal_size: Option<TerminalSize>,
) -> Result<MultiplexByteReader> {
    let us = user_state.read().await;
    let session = us
        .agents
        .get(agent_id)
        .ok_or(AmuxError::AgentNotFound(agent_id.to_string()))?;

    let pty = session
        .get_pty_handle()
        .ok_or(AmuxError::ServerError(format!(
            "agent {agent_id} has no PTY"
        )))?;

    if let Some(size) = terminal_size {
        pty.resize(size.rows, size.cols).await?;
    }

    pty.subscribe().await.ok_or(AmuxError::ServerError(format!(
        "agent {agent_id} session has ended"
    )))
}

/// Shutdown the server — shuts down all agents for the given user state
pub(super) async fn shutdown_server(user_state: &Arc<RwLock<ServerUserState>>) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut us = user_state.write().await;
        std::mem::take(&mut us.agents)
    };
    for (id, session) in &sessions {
        tracing::info!(agent_id = %id, "shutting down agent");
        session.stop(StopPolicy::Interrupt).await;
    }
}

/// Suspend the server — stop all agents and return serializable state.
/// Returns (suspended_agents, errors) where errors are agent IDs that failed to suspend.
pub(super) async fn suspend_server(
    user_state: &Arc<RwLock<ServerUserState>>,
) -> (SuspendedServerState, Vec<String>) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut us = user_state.write().await;
        std::mem::take(&mut us.agents)
    };

    let mut suspended = Vec::new();
    let mut errors = Vec::new();

    for (id, session) in sessions {
        // Remove from registry before suspending
        {
            let mut us = user_state.write().await;
            us.registry.remove(&id);
            broadcast_to_peers(
                &mut us,
                &DirectMessage::WithdrawAgent { agent_id: id },
                None,
            );
        }

        tracing::info!(agent_id = %id, "suspending agent");
        match session.suspend().await {
            Ok(sa) => suspended.push(sa),
            Err(e) => {
                tracing::error!(agent_id = %id, error = %e, "failed to suspend agent");
                errors.push(format!("agent {id}: {e}"));
            }
        }
    }
    (SuspendedServerState { agents: suspended }, errors)
}

/// Resume agents from suspended state. Returns (resumed_count, failed_count).
pub(super) async fn resume_agents(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    user_id: Uuid,
    suspended: Vec<crate::agents::SuspendedAgent>,
) -> (usize, usize) {
    let mut resumed = 0usize;
    let mut failed = 0usize;

    for sa in suspended {
        let agent_id = sa.agent_id();
        let name = sa.name().map(String::from);
        tracing::info!(agent_id = %agent_id, name = ?name, "resuming agent");

        let mut session = sa.into_session();
        match session.start() {
            Ok(exit_handle) => {
                let info = session.to_agent();
                let command = session.command().to_string();
                let working_dir = session.working_dir().to_path_buf();
                {
                    let mut us = user_state.write().await;
                    us.agents.insert(agent_id, session);
                    if let Err(e) = us.registry.register_local(info) {
                        tracing::error!(agent_id = %agent_id, error = %e, "failed to register resumed agent");
                    }
                    broadcast_to_peers(
                        &mut us,
                        &DirectMessage::AnnounceAgent {
                            agent_id,
                            name: name.clone(),
                            command,
                            working_dir,
                            route: Route::empty(),
                        },
                        None,
                    );
                }

                // Task: Monitor exit handle and notify server when agent exits
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    let _ = exit_handle.await;
                    let _ = event_tx
                        .send(SessionEvent::Ended { agent_id, user_id })
                        .await;
                });

                resumed += 1;
            }
            Err(e) => {
                tracing::error!(agent_id = %agent_id, error = %e, "failed to resume agent");
                failed += 1;
            }
        }
    }

    tracing::info!(resumed, failed, "resume complete");
    (resumed, failed)
}

/// Send a DirectMessage to all peer links, optionally excluding one.
pub(super) fn broadcast_to_peers(
    us: &mut ServerUserState,
    msg: &DirectMessage,
    exclude_link: Option<&str>,
) {
    let wire_msg = Message::Direct(msg.clone());
    let mut sent = 0usize;
    let mut failed = 0usize;
    for link in &us.peer_links {
        if exclude_link == Some(link.as_str()) {
            continue;
        }
        if let Some(tx) = us.routes.get(link) {
            if tx.try_send(wire_msg.clone()).is_err() {
                tracing::warn!(peer = %link, "failed to send to peer");
                failed += 1;
            } else {
                sent += 1;
            }
        }
    }
    tracing::debug!(sent, failed, "broadcast to peers");
}

/// Announce all known agents (local + remote) to a newly connected peer.
/// Filters out agents that were learned from this same peer (no echo-back).
/// Returns the number of agents announced.
fn send_initial_agent_announcements(us: &ServerUserState, peer_link: &str) -> usize {
    let Some(tx) = us.routes.get(peer_link) else {
        return 0;
    };

    let mut count = 0usize;
    for (uuid, info) in us.registry.iter_entries() {
        if let Some(link) = info.route.peek()
            && link == peer_link
        {
            continue;
        }
        let msg = Message::Direct(DirectMessage::AnnounceAgent {
            agent_id: *uuid,
            name: info.name.clone(),
            command: info.command.clone(),
            working_dir: info.working_dir.clone(),
            route: info.route.clone(),
        });
        if tx.try_send(msg).is_err() {
            tracing::warn!(agent_id = %uuid, peer = %peer_link, "failed to announce agent");
        } else {
            count += 1;
        }
    }
    count
}

/// Announce all known hosts (remote + own) to a newly connected peer.
/// Filters out hosts that were learned from this same peer (no echo-back).
/// Cloud servers are stateless relays and don't announce themselves as hosts.
/// Returns the number of hosts announced.
fn send_initial_host_announcements(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &str,
) -> usize {
    let Some(tx) = us.routes.get(peer_link) else {
        return 0;
    };

    let mut count = 0usize;
    for info in us.hosts.values() {
        if let Some(link) = info.route.peek()
            && link == peer_link
        {
            continue;
        }
        let msg = Message::Direct(DirectMessage::AnnounceHost {
            id: info.id,
            name: info.name.clone(),
            route: info.route.clone(),
            version: info.version.clone(),
        });
        if tx.try_send(msg).is_err() {
            tracing::warn!(host_id = %info.id, peer = %peer_link, "failed to announce host");
        } else {
            count += 1;
        }
    }

    if !is_cloud_server {
        let msg = Message::Direct(DirectMessage::AnnounceHost {
            id: host_id,
            name: host_name.to_string(),
            route: crate::route::Route::empty(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        });
        if tx.try_send(msg).is_err() {
            tracing::warn!(peer = %peer_link, "failed to announce own host");
        } else {
            count += 1;
        }
    }
    count
}

/// Send all initial announcements (agents + hosts) to a newly connected peer.
/// `host_id`, `host_name`, and `is_cloud_server` are extracted from global state by the caller
/// to avoid holding both locks simultaneously.
pub(super) fn send_initial_announcements(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &str,
) {
    let agents = send_initial_agent_announcements(us, peer_link);
    let hosts = send_initial_host_announcements(us, host_id, host_name, is_cloud_server, peer_link);
    tracing::info!(peer = %peer_link, agents, hosts, "sent initial announcements");
}

/// Handle a peer disconnecting: remove route, peer_links entry, remote agents,
/// cancel unreachable streams, and propagate withdrawals to remaining peers.
pub(super) fn handle_peer_disconnect(us: &mut ServerUserState, link_name: &str) {
    tracing::info!(peer = %link_name, "peer disconnected");

    us.routes.remove(link_name);
    us.peer_links.remove(link_name);

    // Cancel streams spawned for subscribers on this link (they hold cloned senders
    // to the link's outgoing channel — must be dropped for writer task to exit)
    // and streams whose route passes through this link (unreachable).
    let cancelled = cancel_streams_matching(us, |entry| {
        entry.link == link_name || entry.dst.contains_link(link_name)
    });
    if cancelled > 0 {
        tracing::info!(count = cancelled, peer = %link_name, "cancelled streams for disconnected peer");
    }

    let removed_ids = us.registry.remove_for_link(link_name);
    if !removed_ids.is_empty() {
        tracing::info!(count = removed_ids.len(), peer = %link_name, "removed agents for disconnected peer");
    }

    // Withdraw hosts learned from the dead link
    let removed_host_ids: Vec<Uuid> = us
        .hosts
        .iter()
        .filter(|(_, info)| matches!(info.route.peek(), Some(link) if link == link_name))
        .map(|(id, _)| *id)
        .collect();
    for id in removed_host_ids {
        tracing::info!(host_id = %id, peer = %link_name, "withdrawing host");
        us.hosts.remove(&id);
        broadcast_to_peers(us, &DirectMessage::WithdrawHost { id }, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_registry::Agent;
    use crate::message::Host;
    use crate::server::StreamEntry;
    use std::path::PathBuf;
    use tokio::sync::{mpsc, oneshot};

    /// Insert a peer link (adds to both routes and peer_links) and return
    /// the receiver for inspecting broadcast messages.
    fn add_peer(us: &mut ServerUserState, name: &str) -> mpsc::Receiver<Message> {
        let (tx, rx) = mpsc::channel::<Message>(64);
        us.routes.insert(name.to_string(), tx);
        us.peer_links.insert(name.to_string());
        rx
    }

    /// Register a remote agent reachable through the given link.
    fn add_remote_agent(us: &mut ServerUserState, link: &str, name: Option<&str>) -> Uuid {
        let id = Uuid::new_v4();
        us.registry
            .register_remote(Agent {
                id,
                name: name.map(String::from),
                command: "test".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(link),
            })
            .unwrap();
        id
    }

    /// Add a host reachable through the given link.
    fn add_host(us: &mut ServerUserState, link: &str, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        us.hosts.insert(
            id,
            Host {
                id,
                name: name.to_string(),
                route: Route::from_link(link),
                version: "0.1.0".to_string(),
            },
        );
        id
    }

    /// Register a stream and return the cancel receiver (completes when stream is cancelled).
    fn add_stream(
        us: &mut ServerUserState,
        agent_id: Uuid,
        link: &str,
        dst: Route,
    ) -> oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let sid = us.next_stream_id;
        us.next_stream_id += 1;
        us.active_streams
            .entry(agent_id)
            .or_default()
            .push(StreamEntry {
                stream_id: sid,
                cancel: cancel_tx,
                dst,
                link: link.to_string(),
            });
        cancel_rx
    }

    // --- handle_peer_disconnect tests ---

    #[test]
    fn peer_disconnect_removes_route_and_peer_link() {
        let mut us = ServerUserState::new();
        let _rx = add_peer(&mut us, "dead-peer");
        let _rx2 = add_peer(&mut us, "alive-peer");

        handle_peer_disconnect(&mut us, "dead-peer");

        assert!(!us.routes.contains_key("dead-peer"));
        assert!(!us.peer_links.contains("dead-peer"));
        assert!(us.routes.contains_key("alive-peer"));
        assert!(us.peer_links.contains("alive-peer"));
    }

    #[test]
    fn peer_disconnect_cancels_streams_on_link() {
        let mut us = ServerUserState::new();
        let _rx = add_peer(&mut us, "dead-peer");

        let agent_id = Uuid::new_v4();
        let mut cancel_rx_dead = add_stream(
            &mut us,
            agent_id,
            "dead-peer",
            Route::from_link("somewhere"),
        );
        let _cancel_rx_alive = add_stream(
            &mut us,
            agent_id,
            "alive-link",
            Route::from_link("somewhere"),
        );

        handle_peer_disconnect(&mut us, "dead-peer");

        // Stream on dead-peer should be cancelled (sender dropped)
        assert!(cancel_rx_dead.try_recv().is_err());
        // Stream on alive-link survives
        let remaining = us.active_streams.get(&agent_id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].link, "alive-link");
    }

    #[test]
    fn peer_disconnect_cancels_streams_routed_through_link() {
        let mut us = ServerUserState::new();
        let _rx = add_peer(&mut us, "dead-peer");

        let agent_id = Uuid::new_v4();
        // Stream whose dst route passes through dead-peer (but originates from a different link)
        let mut through_route = Route::from_link("host-b");
        through_route.push("dead-peer");
        let mut cancel_rx = add_stream(&mut us, agent_id, "local-link", through_route);

        handle_peer_disconnect(&mut us, "dead-peer");

        // Stream routed through dead-peer should be cancelled
        assert!(cancel_rx.try_recv().is_err());
        assert!(!us.active_streams.contains_key(&agent_id));
    }

    #[test]
    fn peer_disconnect_removes_agents_for_link() {
        let mut us = ServerUserState::new();
        let _rx = add_peer(&mut us, "dead-peer");

        let dead_agent = add_remote_agent(&mut us, "dead-peer", Some("doomed"));
        let alive_agent = add_remote_agent(&mut us, "other-peer", Some("safe"));

        handle_peer_disconnect(&mut us, "dead-peer");

        assert!(!us.registry.contains(&dead_agent));
        assert!(us.registry.contains(&alive_agent));
    }

    #[test]
    fn peer_disconnect_withdraws_hosts_and_propagates() {
        let mut us = ServerUserState::new();
        let _dead_rx = add_peer(&mut us, "dead-peer");
        let mut alive_rx = add_peer(&mut us, "alive-peer");

        let dead_host = add_host(&mut us, "dead-peer", "remote-laptop");
        let alive_host = add_host(&mut us, "alive-peer", "other-laptop");

        handle_peer_disconnect(&mut us, "dead-peer");

        assert!(!us.hosts.contains_key(&dead_host));
        assert!(us.hosts.contains_key(&alive_host));

        // WithdrawHost should be broadcast to alive-peer
        let msg = alive_rx
            .try_recv()
            .expect("alive-peer should receive WithdrawHost");
        match msg {
            Message::Direct(DirectMessage::WithdrawHost { id }) => {
                assert_eq!(id, dead_host);
            }
            other => panic!("expected WithdrawHost, got {:?}", other),
        }
    }

    #[test]
    fn peer_disconnect_full_cascade() {
        let mut us = ServerUserState::new();
        let _dead_rx = add_peer(&mut us, "dead-peer");
        let mut alive_rx = add_peer(&mut us, "alive-peer");

        let dead_agent = add_remote_agent(&mut us, "dead-peer", Some("remote-agent"));
        let dead_host = add_host(&mut us, "dead-peer", "remote-host");
        let mut cancel_rx = add_stream(
            &mut us,
            dead_agent,
            "dead-peer",
            Route::from_link("somewhere"),
        );

        let alive_agent = add_remote_agent(&mut us, "alive-peer", Some("local-agent"));
        let alive_host = add_host(&mut us, "alive-peer", "local-host");

        handle_peer_disconnect(&mut us, "dead-peer");

        // All dead state cleaned up
        assert!(!us.routes.contains_key("dead-peer"));
        assert!(!us.peer_links.contains("dead-peer"));
        assert!(!us.registry.contains(&dead_agent));
        assert!(!us.hosts.contains_key(&dead_host));
        assert!(cancel_rx.try_recv().is_err());
        assert!(!us.active_streams.contains_key(&dead_agent));

        // All alive state preserved
        assert!(us.routes.contains_key("alive-peer"));
        assert!(us.peer_links.contains("alive-peer"));
        assert!(us.registry.contains(&alive_agent));
        assert!(us.hosts.contains_key(&alive_host));

        // WithdrawHost broadcast to alive-peer
        let msg = alive_rx
            .try_recv()
            .expect("alive-peer should receive WithdrawHost");
        assert!(
            matches!(msg, Message::Direct(DirectMessage::WithdrawHost { id }) if id == dead_host)
        );
    }

    // --- broadcast_to_peers tests ---

    #[test]
    fn broadcast_to_peers_excludes_specified_link() {
        let mut us = ServerUserState::new();
        let mut rx_a = add_peer(&mut us, "peer-a");
        let mut rx_b = add_peer(&mut us, "peer-b");

        let agent_id = Uuid::new_v4();
        broadcast_to_peers(
            &mut us,
            &DirectMessage::WithdrawAgent { agent_id },
            Some("peer-a"),
        );

        assert!(rx_a.try_recv().is_err(), "excluded peer should not receive");
        let msg = rx_b.try_recv().expect("non-excluded peer should receive");
        assert!(matches!(
            msg,
            Message::Direct(DirectMessage::WithdrawAgent { .. })
        ));
    }

    #[test]
    fn broadcast_to_peers_sends_to_all_when_no_exclude() {
        let mut us = ServerUserState::new();
        let mut rx_a = add_peer(&mut us, "peer-a");
        let mut rx_b = add_peer(&mut us, "peer-b");

        let agent_id = Uuid::new_v4();
        broadcast_to_peers(&mut us, &DirectMessage::WithdrawAgent { agent_id }, None);

        assert!(rx_a.try_recv().is_ok());
        assert!(rx_b.try_recv().is_ok());
    }

    // --- send_initial_announcements tests ---

    #[test]
    fn initial_announcements_filter_agent_echo_back() {
        let mut us = ServerUserState::new();
        let mut rx = add_peer(&mut us, "peer-a");

        // Agent learned from peer-a (should NOT be re-announced to peer-a)
        let _echo_id = add_remote_agent(&mut us, "peer-a", Some("echo-agent"));
        // Agent learned from peer-b (SHOULD be announced to peer-a)
        let forwarded_id = add_remote_agent(&mut us, "peer-b", Some("forward-agent"));

        let count = send_initial_agent_announcements(&us, "peer-a");

        assert_eq!(count, 1, "only non-echo agents should be announced");
        let msg = rx.try_recv().expect("should receive one announcement");
        match msg {
            Message::Direct(DirectMessage::AnnounceAgent { agent_id, .. }) => {
                assert_eq!(agent_id, forwarded_id);
            }
            other => panic!("expected AnnounceAgent, got {:?}", other),
        }
        assert!(rx.try_recv().is_err(), "should be no more messages");
    }

    #[test]
    fn initial_announcements_filter_host_echo_back() {
        let mut us = ServerUserState::new();
        let mut rx = add_peer(&mut us, "peer-a");

        // Host learned from peer-a (should NOT be re-announced)
        let _echo_host = add_host(&mut us, "peer-a", "echo-host");
        // Host learned from peer-b (SHOULD be announced)
        let forwarded_host = add_host(&mut us, "peer-b", "forward-host");

        let host_id = Uuid::new_v4();
        let count = send_initial_host_announcements(&us, host_id, "myhost", false, "peer-a");

        // Should announce: forward-host + own host = 2
        assert_eq!(count, 2);
        let mut received = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            received.push(msg);
        }
        assert_eq!(received.len(), 2);

        // Verify forward-host is included but echo-host is not
        let announced_ids: Vec<Uuid> = received
            .iter()
            .filter_map(|m| match m {
                Message::Direct(DirectMessage::AnnounceHost { id, .. }) => Some(*id),
                _ => None,
            })
            .collect();
        assert!(announced_ids.contains(&forwarded_host));
        assert!(announced_ids.contains(&host_id)); // own host
    }

    #[test]
    fn initial_announcements_cloud_skips_own_host() {
        let mut us = ServerUserState::new();
        let mut rx = add_peer(&mut us, "peer-a");

        let host_id = Uuid::new_v4();
        send_initial_announcements(&us, host_id, "cloud-server", true, "peer-a");

        // Cloud servers don't announce themselves as hosts — no messages expected
        // (no agents or remote hosts registered either)
        assert!(
            rx.try_recv().is_err(),
            "cloud server should not announce own host"
        );
    }

    // --- create_agent tests ---

    #[tokio::test]
    async fn create_agent_rejects_when_at_limit() {
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let (event_tx, _event_rx) = mpsc::channel(16);

        // Fill the agents map to MAX_LOCAL_AGENTS with unstarted sessions.
        {
            let mut us = user_state.write().await;
            for _ in 0..MAX_LOCAL_AGENTS {
                let id = Uuid::new_v4();
                let req = crate::message::CreateAgentRequest {
                    agent_id: id,
                    name: None,
                    agent_type: crate::message::AgentType::TestAgent("/bin/cat".to_string()),
                    working_dir: PathBuf::from("/tmp"),
                    terminal_size: None,
                };
                let inner = crate::agents::TestAgentSession::new(&req, "/bin/cat".to_string());
                us.agents
                    .insert(id, crate::agents::AgentSession::TestAgent(inner));
            }
        }

        // The next create_agent should be rejected
        let new_req = crate::message::CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            name: Some("one-too-many".to_string()),
            agent_type: crate::message::AgentType::TestAgent("/bin/cat".to_string()),
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
        };
        let err = create_agent(
            &user_state,
            &event_tx,
            new_req,
            crate::server::LOCAL_USER_ID,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("agent limit reached"),
            "expected agent limit error, got: {err}"
        );
    }

    #[test]
    fn initial_announcements_non_cloud_includes_own_host() {
        let mut us = ServerUserState::new();
        let mut rx = add_peer(&mut us, "peer-a");

        let host_id = Uuid::new_v4();
        send_initial_announcements(&us, host_id, "my-laptop", false, "peer-a");

        let msg = rx.try_recv().expect("should announce own host");
        match msg {
            Message::Direct(DirectMessage::AnnounceHost { id, name, .. }) => {
                assert_eq!(id, host_id);
                assert_eq!(name, "my-laptop");
            }
            other => panic!("expected AnnounceHost, got {:?}", other),
        }
    }
}
