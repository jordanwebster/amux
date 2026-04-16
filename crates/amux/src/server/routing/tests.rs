use std::path::PathBuf;

use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

use super::agents::MAX_LOCAL_AGENTS;
use super::peers::{send_initial_agent_announcements, send_initial_host_announcements};
use super::*;
use crate::agent::{Agent, AgentSession};
use crate::protocol::Route;
use crate::protocol::link::Link;
use crate::protocol::message::{DirectMessage, Host, SubscriptionId};
use crate::server::{SUBSCRIPTION_LEASE_DURATION, SubscriptionEntry, SubscriptionMode};

fn dummy_pty_command() -> String {
    #[cfg(unix)]
    {
        "/bin/cat".to_string()
    }
    #[cfg(windows)]
    {
        "cmd.exe".to_string()
    }
}

fn dummy_working_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Insert a peer link (adds to both routes and peer_links) and return
/// the receiver for inspecting broadcast messages.
fn add_peer(us: &mut ServerUserState, name: &str) -> mpsc::Receiver<Message> {
    let (tx, rx) = mpsc::channel::<Message>(64);
    us.routes.insert(
        Link::new(name).unwrap(),
        ConnectionHandle::new(tx, Arc::new(std::sync::atomic::AtomicU64::new(1))),
    );
    us.peer_links.insert(Link::new(name).unwrap());
    rx
}

/// Register a remote agent and its host reachable through the given link.
fn add_remote_agent(us: &mut ServerUserState, link: &str, name: Option<&str>) -> (Uuid, Uuid) {
    let id = Uuid::new_v4();
    let host_id = Uuid::new_v4();
    us.hosts.insert(
        host_id,
        Host {
            id: host_id,
            name: format!("host-{host_id}"),
            route: Route::from_link(Link::new(link).unwrap()),
            version: "0.1.0".to_string(),
        },
    );
    us.registry
        .register_remote(Agent {
            id,
            host_id,
            name: name.map(String::from),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::from_link(Link::new(link).unwrap()),
            agent_type: "test_agent".to_string(),
            structured_protocol: None,
            readonly: false,
            args: vec![],
            created_at: Utc::now(),
        })
        .unwrap();
    (id, host_id)
}

/// Add a host reachable through the given link.
fn add_host(us: &mut ServerUserState, link: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    us.hosts.insert(
        id,
        Host {
            id,
            name: name.to_string(),
            route: Route::from_link(Link::new(link).unwrap()),
            version: "0.1.0".to_string(),
        },
    );
    id
}

/// Register a subscription and return the cancel receiver (completes when it is cancelled).
fn add_subscription(
    us: &mut ServerUserState,
    agent_id: Uuid,
    dst: Route,
) -> (SubscriptionId, oneshot::Receiver<()>) {
    let subscription_id = SubscriptionId::random();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    us.active_subscriptions.insert(
        subscription_id,
        SubscriptionEntry {
            subscription_id,
            agent_id,
            mode: SubscriptionMode::Raw,
            cancel: cancel_tx,
            dst,
            lease_deadline: Instant::now() + SUBSCRIPTION_LEASE_DURATION,
        },
    );
    (subscription_id, cancel_rx)
}

#[test]
fn withdraw_agent_preserves_subscriptions_and_returns_session() {
    let mut us = ServerUserState::new();
    let mut peer_rx = add_peer(&mut us, "peer-a");

    let agent_id = Uuid::new_v4();
    let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
    let info = session.to_agent(Uuid::new_v4());
    us.agents.insert(agent_id, session);
    us.registry.register_local(info).unwrap();

    let (_subscription_id, mut cancel_rx) = add_subscription(
        &mut us,
        agent_id,
        Route::from_link(Link::new("peer-a").unwrap()),
    );

    let removed = withdraw_agent(&mut us, agent_id);

    assert!(
        removed.is_some(),
        "local session should be returned for cleanup"
    );
    assert!(!us.registry.contains(&agent_id));
    assert!(
        us.active_subscriptions
            .values()
            .any(|entry| entry.agent_id == agent_id),
        "withdrawal should preserve active subscriptions until they observe EOF"
    );
    assert!(
        matches!(
            cancel_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "subscription cancel sender should remain alive after withdrawal"
    );

    let msg = peer_rx
        .try_recv()
        .expect("peers should receive WithdrawAgent");
    assert!(matches!(
        msg,
        Message::Direct {
            message: DirectMessage::WithdrawAgent { agent_id: id }
        } if id == agent_id
    ));
}

// --- handle_peer_disconnect tests ---

#[test]
fn peer_disconnect_removes_route_and_peer_link() {
    let mut us = ServerUserState::new();
    let _rx = add_peer(&mut us, "dead-peer");
    let _rx2 = add_peer(&mut us, "alive-peer");

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    assert!(!us.routes.contains_key("dead-peer"));
    assert!(!us.peer_links.contains("dead-peer"));
    assert!(us.routes.contains_key("alive-peer"));
    assert!(us.peer_links.contains("alive-peer"));
}

#[test]
fn peer_disconnect_cancels_subscriptions_on_link() {
    let mut us = ServerUserState::new();
    let _rx = add_peer(&mut us, "dead-peer");

    let agent_id = Uuid::new_v4();
    let (_dead_id, mut cancel_rx_dead) = add_subscription(
        &mut us,
        agent_id,
        Route::from_link(Link::new("dead-peer").unwrap()),
    );
    let (alive_id, _cancel_rx_alive) = add_subscription(
        &mut us,
        agent_id,
        Route::from_link(Link::new("alive-link").unwrap()),
    );

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    // Subscription on dead-peer should be cancelled (sender dropped)
    assert!(cancel_rx_dead.try_recv().is_err());
    // Subscription on alive-link survives
    let remaining = us.active_subscriptions.get(&alive_id).unwrap();
    assert_eq!(
        remaining.dst,
        Route::from_link(Link::new("alive-link").unwrap())
    );
}

#[test]
fn peer_disconnect_cancels_subscriptions_routed_through_link() {
    let mut us = ServerUserState::new();
    let _rx = add_peer(&mut us, "dead-peer");

    let agent_id = Uuid::new_v4();
    // Stream whose dst route passes through dead-peer (but originates from a different link)
    let mut through_route = Route::from_link(Link::new("host-b").unwrap());
    through_route.push(Link::new("dead-peer").unwrap());
    through_route.push(Link::new("local-link").unwrap());
    let (_subscription_id, mut cancel_rx) = add_subscription(&mut us, agent_id, through_route);

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    // Subscription routed through dead-peer should be cancelled
    assert!(cancel_rx.try_recv().is_err());
    assert!(
        !us.active_subscriptions
            .values()
            .any(|entry| entry.agent_id == agent_id)
    );
}

#[test]
fn peer_disconnect_removes_agents_for_link() {
    let mut us = ServerUserState::new();
    let _rx = add_peer(&mut us, "dead-peer");

    let (dead_agent, _dead_host) = add_remote_agent(&mut us, "dead-peer", Some("doomed"));
    let (alive_agent, _alive_host) = add_remote_agent(&mut us, "other-peer", Some("safe"));

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

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

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    assert!(!us.hosts.contains_key(&dead_host));
    assert!(us.hosts.contains_key(&alive_host));

    // WithdrawHost should be broadcast to alive-peer
    let msg = alive_rx
        .try_recv()
        .expect("alive-peer should receive WithdrawHost");
    match msg {
        Message::Direct {
            message: DirectMessage::WithdrawHost { id, route },
        } => {
            assert_eq!(id, dead_host);
            assert_eq!(route, Route::from_link(Link::new("dead-peer").unwrap()));
        }
        other => panic!("expected WithdrawHost, got {:?}", other),
    }
}

#[test]
fn peer_disconnect_withdraws_only_root_hosts() {
    let mut us = ServerUserState::new();
    let _dead_rx = add_peer(&mut us, "dead-peer");
    let mut alive_rx = add_peer(&mut us, "alive-peer");

    let root_a = Uuid::new_v4();
    let child_a = Uuid::new_v4();
    let root_b = Uuid::new_v4();

    let mut root_a_route = Route::from_link(Link::new("a").unwrap());
    root_a_route.push(Link::new("dead-peer").unwrap());
    let mut child_a_route = Route::from_link(Link::new("child").unwrap());
    child_a_route.push(Link::new("a").unwrap());
    child_a_route.push(Link::new("dead-peer").unwrap());
    let mut root_b_route = Route::from_link(Link::new("b").unwrap());
    root_b_route.push(Link::new("dead-peer").unwrap());

    us.hosts.insert(
        root_a,
        Host {
            id: root_a,
            name: "root-a".to_string(),
            route: root_a_route.clone(),
            version: "0.1.0".to_string(),
        },
    );
    us.hosts.insert(
        child_a,
        Host {
            id: child_a,
            name: "child-a".to_string(),
            route: child_a_route,
            version: "0.1.0".to_string(),
        },
    );
    us.hosts.insert(
        root_b,
        Host {
            id: root_b,
            name: "root-b".to_string(),
            route: root_b_route.clone(),
            version: "0.1.0".to_string(),
        },
    );

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    assert!(!us.hosts.contains_key(&root_a));
    assert!(!us.hosts.contains_key(&child_a));
    assert!(!us.hosts.contains_key(&root_b));

    let mut received = Vec::new();
    while let Ok(msg) = alive_rx.try_recv() {
        received.push(msg);
    }
    received.sort_unstable_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));

    assert_eq!(received.len(), 2);
    assert!(received.iter().any(|msg| {
        matches!(
            msg,
            Message::Direct {
                message: DirectMessage::WithdrawHost { id, route }
            }
                if *id == root_a && *route == root_a_route
        )
    }));
    assert!(received.iter().any(|msg| {
        matches!(
            msg,
            Message::Direct {
                message: DirectMessage::WithdrawHost { id, route }
            }
                if *id == root_b && *route == root_b_route
        )
    }));
}

#[test]
fn peer_disconnect_full_cascade() {
    let mut us = ServerUserState::new();
    let _dead_rx = add_peer(&mut us, "dead-peer");
    let mut alive_rx = add_peer(&mut us, "alive-peer");

    let (dead_agent, dead_host) = add_remote_agent(&mut us, "dead-peer", Some("remote-agent"));
    let (_subscription_id, mut cancel_rx) = add_subscription(
        &mut us,
        dead_agent,
        Route::from_link(Link::new("dead-peer").unwrap()),
    );

    let (alive_agent, alive_host) = add_remote_agent(&mut us, "alive-peer", Some("local-agent"));

    handle_peer_disconnect(&mut us, &Link::new("dead-peer").unwrap());

    // All dead state cleaned up
    assert!(!us.routes.contains_key("dead-peer"));
    assert!(!us.peer_links.contains("dead-peer"));
    assert!(!us.registry.contains(&dead_agent));
    assert!(!us.hosts.contains_key(&dead_host));
    assert!(cancel_rx.try_recv().is_err());
    assert!(
        !us.active_subscriptions
            .values()
            .any(|entry| entry.agent_id == dead_agent)
    );

    // All alive state preserved
    assert!(us.routes.contains_key("alive-peer"));
    assert!(us.peer_links.contains("alive-peer"));
    assert!(us.registry.contains(&alive_agent));
    assert!(us.hosts.contains_key(&alive_host));

    // WithdrawHost broadcast to alive-peer
    let msg = alive_rx
        .try_recv()
        .expect("alive-peer should receive WithdrawHost");
    assert!(matches!(
        msg,
        Message::Direct {
            message: DirectMessage::WithdrawHost { id, route }
        } if id == dead_host && route == Route::from_link(Link::new("dead-peer").unwrap())
    ));
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
        Some(&Link::new("peer-a").unwrap()),
    );

    assert!(rx_a.try_recv().is_err(), "excluded peer should not receive");
    let msg = rx_b.try_recv().expect("non-excluded peer should receive");
    assert!(matches!(
        msg,
        Message::Direct {
            message: DirectMessage::WithdrawAgent { .. }
        }
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
    let (_echo_id, _echo_host) = add_remote_agent(&mut us, "peer-a", Some("echo-agent"));
    // Agent learned from peer-b (SHOULD be announced to peer-a)
    let (forwarded_id, _forwarded_host) =
        add_remote_agent(&mut us, "peer-b", Some("forward-agent"));

    let count = send_initial_agent_announcements(&us, &Link::new("peer-a").unwrap());

    assert_eq!(count, 1, "only non-echo agents should be announced");
    let msg = rx.try_recv().expect("should receive one announcement");
    match msg {
        Message::Direct {
            message: DirectMessage::AnnounceAgent { agent_id, .. },
        } => {
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
    let count = send_initial_host_announcements(
        &us,
        host_id,
        "myhost",
        false,
        &Link::new("peer-a").unwrap(),
    );

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
            Message::Direct {
                message: DirectMessage::AnnounceHost { id, .. },
            } => Some(*id),
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
    send_initial_announcements(
        &us,
        host_id,
        "cloud-server",
        true,
        &Link::new("peer-a").unwrap(),
    );

    assert!(matches!(
        rx.try_recv(),
        Ok(Message::Direct {
            message: DirectMessage::InitialSyncComplete
        })
    ));
    assert!(rx.try_recv().is_err(), "should send sync marker only once");
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
            let req = crate::protocol::message::CreateAgentRequest {
                agent_id: id,
                name: None,
                agent_type: crate::protocol::message::AgentType::TestAgent {
                    command: dummy_pty_command(),
                },
                working_dir: dummy_working_dir(),
                terminal_size: None,
                args: vec![],
            };
            let inner = crate::agent::TestAgentSession::new(&req, dummy_pty_command());
            us.agents
                .insert(id, crate::agent::AgentSession::TestAgent(inner));
        }
    }

    // The next create_agent should be rejected
    let new_req = crate::protocol::message::CreateAgentRequest {
        agent_id: Uuid::new_v4(),
        name: Some("one-too-many".to_string()),
        agent_type: crate::protocol::message::AgentType::TestAgent {
            command: dummy_pty_command(),
        },
        working_dir: dummy_working_dir(),
        terminal_size: None,
        args: vec![],
    };
    let err = create_agent(
        &user_state,
        &event_tx,
        new_req,
        crate::server::LOCAL_USER_ID,
        Uuid::new_v4(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("agent limit reached"),
        "expected agent limit error, got: {err}"
    );
}

#[tokio::test]
async fn resume_agents_uses_current_host_id_for_registry_and_announce() {
    let user_state = Arc::new(RwLock::new(ServerUserState::new()));
    let (event_tx, _event_rx) = mpsc::channel(16);
    let mut peer_rx = {
        let mut us = user_state.write().await;
        add_peer(&mut us, "peer-a")
    };

    let agent_id = Uuid::new_v4();
    let host_id = Uuid::new_v4();
    let suspended = vec![crate::suspend::SuspendedAgent::TestAgent {
        agent_id,
        name: Some("resumed-agent".to_string()),
        command: dummy_pty_command(),
        working_dir: dummy_working_dir(),
        terminal_size: None,
        created_at: Utc::now(),
    }];

    let (resumed, failed) = resume_agents(
        &user_state,
        &event_tx,
        crate::server::LOCAL_USER_ID,
        suspended,
        host_id,
    )
    .await;

    assert_eq!(resumed, 1);
    assert_eq!(failed, 0);

    {
        let us = user_state.read().await;
        let entry = us
            .registry
            .get(&agent_id)
            .expect("resumed agent should register");
        assert_eq!(entry.host_id, host_id);
    }

    let msg = peer_rx
        .try_recv()
        .expect("resumed agent should be announced to peers");
    assert!(matches!(
        msg,
        Message::Direct {
            message: DirectMessage::AnnounceAgent {
                agent_id: id,
                host_id: announced_host_id,
                ..
            }
        } if id == agent_id && announced_host_id == host_id
    ));

    let session = {
        let mut us = user_state.write().await;
        withdraw_agent(&mut us, agent_id).expect("resumed session should still exist")
    };
    session.stop(crate::agent::StopPolicy::Interrupt).await;
}

#[test]
fn initial_announcements_non_cloud_includes_own_host() {
    let mut us = ServerUserState::new();
    let mut rx = add_peer(&mut us, "peer-a");

    let host_id = Uuid::new_v4();
    send_initial_announcements(
        &us,
        host_id,
        "my-laptop",
        false,
        &Link::new("peer-a").unwrap(),
    );

    let msg = rx.try_recv().expect("should announce own host");
    match msg {
        Message::Direct {
            message: DirectMessage::AnnounceHost { id, name, .. },
        } => {
            assert_eq!(id, host_id);
            assert_eq!(name, "my-laptop");
        }
        other => panic!("expected AnnounceHost, got {:?}", other),
    }
    assert!(matches!(
        rx.try_recv(),
        Ok(Message::Direct {
            message: DirectMessage::InitialSyncComplete
        })
    ));
    assert!(rx.try_recv().is_err(), "should send sync marker only once");
}

#[test]
fn initial_announcements_finish_with_sync_complete() {
    let mut us = ServerUserState::new();
    let mut rx = add_peer(&mut us, "peer-a");

    let host_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    us.registry
        .register_local(Agent {
            id: agent_id,
            host_id,
            name: Some("local-agent".to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
            agent_type: "test_agent".to_string(),
            structured_protocol: None,
            readonly: false,
            args: vec![],
            created_at: Utc::now(),
        })
        .unwrap();

    send_initial_announcements(
        &us,
        host_id,
        "my-laptop",
        false,
        &Link::new("peer-a").unwrap(),
    );

    assert!(matches!(
        rx.try_recv(),
        Ok(Message::Direct {
            message: DirectMessage::AnnounceHost {
                id,
                name,
                route,
                ..
            }
        }) if id == host_id && name == "my-laptop" && route == Route::empty()
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(Message::Direct {
            message: DirectMessage::AnnounceAgent { agent_id: id, .. }
        }) if id == agent_id
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(Message::Direct {
            message: DirectMessage::InitialSyncComplete
        })
    ));
    assert!(rx.try_recv().is_err(), "should send sync marker only once");
}
