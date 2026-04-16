mod agent;
mod host;
mod reauth;

use tokio::sync::mpsc;

use crate::protocol::message::{DirectMessage, Message};
use crate::server::connection::{ConnectionContext, ConnectionError};
use crate::transport::TransportError;

pub(super) async fn handle_direct(
    tx: &mpsc::Sender<Message>,
    message: DirectMessage,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match message {
        DirectMessage::Reauth { token } => reauth::handle(tx, token, ctx).await,

        announce @ DirectMessage::AnnounceAgent { .. } => {
            agent::handle_announce(announce, ctx).await
        }

        DirectMessage::WithdrawAgent { agent_id } => agent::handle_withdraw(agent_id, ctx).await,

        DirectMessage::AnnounceHost {
            id,
            name,
            route,
            version,
        } => host::handle_announce(id, name, route, version, ctx).await,

        DirectMessage::WithdrawHost { id, route } => host::handle_withdraw(id, route, ctx).await,

        DirectMessage::Heartbeat => {
            tx.send(Message::Direct {
                message: DirectMessage::HeartbeatAck,
            })
            .await
            .map_err(|_| {
                ConnectionError::Transport(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "outgoing channel closed while sending heartbeat ack",
                )))
            })?;
            Ok(())
        }

        DirectMessage::HeartbeatAck => Ok(()),

        DirectMessage::InitialSyncComplete => Ok(()),

        DirectMessage::ReauthResult { .. } => {
            tracing::warn!("unexpected direct message");
            Ok(())
        }
        DirectMessage::Unknown => {
            tracing::warn!("dropping unknown direct message");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use chrono::Utc;
    use tokio::sync::oneshot;
    use tokio::time::Instant;
    use uuid::Uuid;

    use super::super::tests::*;
    use super::*;
    use crate::agent::Agent;
    use crate::protocol::link::Link;
    use crate::protocol::message::{
        CreateAgentRequest, DirectMessage, Message, SubscriptionId, TerminalSize,
    };
    use crate::protocol::route::Route;
    use crate::server::{SUBSCRIPTION_LEASE_DURATION, SubscriptionMode};

    #[tokio::test]
    async fn reauth_succeeds_in_non_cloud_mode() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::Reauth {
                token: "test-token".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Direct {
            message: DirectMessage::ReauthResult { error },
        } = &msgs[0]
        else {
            panic!("expected ReauthResult, got {:?}", msgs[0]);
        };
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn reauth_result_is_unexpected_response_variant() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_direct(&tx, DirectMessage::ReauthResult { error: None }, &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "unexpected response should not emit a reply"
        );
    }

    #[tokio::test]
    async fn heartbeat_sends_heartbeat_ack() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_direct(&tx, DirectMessage::Heartbeat, &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            msgs[0],
            Message::Direct {
                message: DirectMessage::HeartbeatAck
            }
        ));
    }

    #[tokio::test]
    async fn heartbeat_ack_is_accepted_without_reply() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_direct(&tx, DirectMessage::HeartbeatAck, &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "heartbeat ack should not emit a reply");
    }

    #[tokio::test]
    async fn announce_agent_stores_in_registry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let remote_host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                remote_host_id,
                crate::protocol::message::Host {
                    id: remote_host_id,
                    name: "remote-host".to_string(),
                    route: Route::from_link(Link::new("test-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }
        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id: remote_host_id,
                name: Some("remote-test".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--dangerously-skip-permissions".to_string()],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(us.registry.contains(&agent_id));
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.host_id, remote_host_id);
        assert_eq!(entry.name, Some("remote-test".to_string()));
        assert_eq!(entry.args, vec!["--dangerously-skip-permissions"]);
        assert!(entry.is_remote());
        let mut route = us.registry.materialize(&us.hosts, &agent_id).unwrap().route;
        assert_eq!(route.pop(), Some(Link::new("test-link").unwrap()));
        assert_eq!(route.pop(), None);
    }

    #[tokio::test]
    async fn announce_agent_uses_current_host_route() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            let mut route = Route::from_link(Link::new("host-a").unwrap());
            route.push(Link::new("test-link").unwrap());
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote-host".to_string(),
                    route,
                    version: "0.1.0".to_string(),
                },
            );
        }
        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id,
                name: None,
                command: "bash".to_string(),
                working_dir: PathBuf::from("/home"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert!(entry.is_remote());
        let mut route = us.registry.materialize(&us.hosts, &agent_id).unwrap().route;
        assert_eq!(route.pop(), Some(Link::new("test-link").unwrap()));
        assert_eq!(route.pop(), Some(Link::new("host-a").unwrap()));
        assert_eq!(route.pop(), None);
    }

    #[tokio::test]
    async fn announce_agent_ignores_non_selected_host_route() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote-host".to_string(),
                    route: Route::from_link(Link::new("other-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }

        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id,
                name: Some("remote-test".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(
            !us.registry.contains(&agent_id),
            "non-selected sender should not be able to publish the agent"
        );
    }

    #[tokio::test]
    async fn announce_agent_non_selected_route_does_not_overwrite_existing_entry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote-host".to_string(),
                    route: Route::from_link(Link::new("other-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id,
                name: Some("selected-name".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("other-link").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id,
                name: Some("stale-name".to_string()),
                command: "bash".to_string(),
                working_dir: PathBuf::from("/stale"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--stale".to_string()],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("selected-name"));
        assert_eq!(entry.command, "claude");
        assert!(entry.args.is_empty());
    }

    #[tokio::test]
    async fn announce_agent_skips_local_agent() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            let req = CreateAgentRequest {
                agent_id,
                name: Some("local".to_string()),
                agent_type: crate::protocol::message::AgentType::TestAgent {
                    command: dummy_pty_command(),
                },
                working_dir: dummy_working_dir(),
                terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
                args: vec![],
            };
            let session = create_test_session(&req);
            let info = session.to_agent(Uuid::new_v4());
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id: Uuid::new_v4(),
                name: Some("remote".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/remote"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert!(!entry.is_remote());
    }

    #[tokio::test]
    async fn withdraw_agent_removes_matching_link() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: Uuid::new_v4(),
                name: None,
                command: "bash".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("test-link").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(&tx, DirectMessage::WithdrawAgent { agent_id }, &ctx)
            .await
            .unwrap();

        let us = user_state.read().await;
        assert!(!us.registry.contains(&agent_id));
    }

    #[tokio::test]
    async fn withdraw_agent_ignores_link_mismatch() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: Uuid::new_v4(),
                name: None,
                command: "bash".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("other-link").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(&tx, DirectMessage::WithdrawAgent { agent_id }, &ctx)
            .await
            .unwrap();

        let us = user_state.read().await;
        assert!(us.registry.contains(&agent_id));
    }

    #[tokio::test]
    async fn duplicate_announce_overwrites() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        let first_host_id = Uuid::new_v4();
        let second_host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                first_host_id,
                crate::protocol::message::Host {
                    id: first_host_id,
                    name: "first-host".to_string(),
                    route: Route::from_link(Link::new("test-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                second_host_id,
                crate::protocol::message::Host {
                    id: second_host_id,
                    name: "second-host".to_string(),
                    route: Route::from_link(Link::new("test-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }

        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id: first_host_id,
                name: Some("first".to_string()),
                command: "bash".to_string(),
                working_dir: PathBuf::from("/first"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--dangerously-skip-permissions".to_string()],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        handle_direct(
            &tx,
            DirectMessage::AnnounceAgent {
                agent_id,
                host_id: second_host_id,
                name: Some("second".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/second"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--allow-dangerously-skip-permissions".to_string()],
                created_at: Utc::now(),
            },
            &ctx,
        )
        .await
        .unwrap();

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name, Some("second".to_string()));
            assert_eq!(entry.working_dir, PathBuf::from("/second"));
            assert_eq!(
                entry.args,
                vec!["--allow-dangerously-skip-permissions".to_string()]
            );
        }

        let forwarded = peer_rx
            .try_recv()
            .expect("first announce should be propagated");
        assert!(matches!(
            forwarded,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    ..
                }
            } if id == agent_id && name == "first"
        ));

        let forwarded = peer_rx
            .try_recv()
            .expect("updated announce should be propagated");
        assert!(matches!(
            forwarded,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    args,
                    working_dir,
                    ..
                }
            } if id == agent_id
                && name == "second"
                && args == vec!["--allow-dangerously-skip-permissions".to_string()]
                && working_dir == Path::new("/second")
        ));
    }

    #[tokio::test]
    async fn announce_host_stores_in_hosts() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let host_id = Uuid::new_v4();
        handle_direct(
            &tx,
            DirectMessage::AnnounceHost {
                id: host_id,
                name: "remote-laptop".to_string(),
                route: Route::empty(),
                version: "0.1.0".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(us.hosts.contains_key(&host_id));
        let info = &us.hosts[&host_id];
        assert_eq!(info.name, "remote-laptop");
        assert_eq!(info.version, "0.1.0");
        let mut route = info.route.clone();
        assert_eq!(route.pop(), Some(Link::new("test-link").unwrap()));
        assert_eq!(route.pop(), None);
    }

    #[tokio::test]
    async fn announce_host_with_route_prepends_link() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let host_id = Uuid::new_v4();
        handle_direct(
            &tx,
            DirectMessage::AnnounceHost {
                id: host_id,
                name: "far-server".to_string(),
                route: Route::from_link(Link::new("peer-a").unwrap()),
                version: "0.2.0".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        let info = &us.hosts[&host_id];
        let mut route = info.route.clone();
        assert_eq!(route.pop(), Some(Link::new("test-link").unwrap()));
        assert_eq!(route.pop(), Some(Link::new("peer-a").unwrap()));
        assert_eq!(route.pop(), None);
    }

    #[tokio::test]
    async fn announce_host_rewrites_descendant_prefixes_but_only_rebroadcasts_parent() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-b").await;

        let host_id = Uuid::new_v4();
        let child_host_id = Uuid::new_v4();
        let grandchild_host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();

        let mut child_route = Route::from_link(Link::new("child").unwrap());
        child_route.push(Link::new("old-link").unwrap());
        let mut grandchild_route = Route::from_link(Link::new("grand").unwrap());
        grandchild_route.push(Link::new("child").unwrap());
        grandchild_route.push(Link::new("old-link").unwrap());

        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote-parent".to_string(),
                    route: Route::from_link(Link::new("old-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                child_host_id,
                crate::protocol::message::Host {
                    id: child_host_id,
                    name: "remote-child".to_string(),
                    route: child_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                grandchild_host_id,
                crate::protocol::message::Host {
                    id: grandchild_host_id,
                    name: "remote-grandchild".to_string(),
                    route: grandchild_route,
                    version: "0.1.0".to_string(),
                },
            );
        }

        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: child_host_id,
                name: Some("remote-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: child_route.clone(),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        handle_direct(
            &tx,
            DirectMessage::AnnounceHost {
                id: host_id,
                name: "remote-parent".to_string(),
                route: Route::empty(),
                version: "0.2.0".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        {
            let us = user_state.read().await;
            assert_eq!(us.hosts[&host_id].route.to_string(), "test-link");
            assert_eq!(
                us.hosts[&child_host_id].route.to_string(),
                "test-link.child"
            );
            assert_eq!(
                us.hosts[&grandchild_host_id].route.to_string(),
                "test-link.child.grand"
            );
            assert_eq!(
                us.registry
                    .materialize(&us.hosts, &agent_id)
                    .unwrap()
                    .route
                    .to_string(),
                "test-link.child"
            );
        }

        let mut announced_hosts: Vec<_> = drain_direct_messages(&mut peer_rx)
            .into_iter()
            .map(|msg| match msg {
                DirectMessage::AnnounceHost { id, route, .. } => (id, route.to_string()),
                other => panic!("expected AnnounceHost, got {:?}", other),
            })
            .collect();
        announced_hosts.sort_unstable_by_key(|(id, _)| id.as_u128());
        assert_eq!(announced_hosts, vec![(host_id, "test-link".to_string())]);
    }

    #[tokio::test]
    async fn announce_host_skips_own_host_id() {
        let (state, user_state) = test_state().await;

        let host_id = {
            let s = state.read().await;
            s.host_id
        };

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::AnnounceHost {
                id: host_id,
                name: "myself".to_string(),
                route: Route::from_link(Link::new("cloud").unwrap()),
                version: "0.1.0".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
    }

    #[tokio::test]
    async fn withdraw_host_removes_matching_link() {
        let (state, user_state) = test_state().await;

        let host_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link(Link::new("test-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::WithdrawHost {
                id: host_id,
                route: Route::empty(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
    }

    #[tokio::test]
    async fn withdraw_host_cancels_streams_with_matching_full_route() {
        let (state, user_state) = test_state().await;

        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut full_route = Route::from_link(Link::new("child").unwrap());
        full_route.push(Link::new("test-link").unwrap());
        let (cancel_tx, mut cancel_rx) = oneshot::channel();

        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "mobile".to_string(),
                    route: full_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            crate::server::connection::register_subscription(
                &mut us,
                SubscriptionId::random(),
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_route.clone(),
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::WithdrawHost {
                id: host_id,
                route: Route::from_link(Link::new("child").unwrap()),
            },
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            cancel_rx.try_recv().is_err(),
            "matching withdraw should cancel the subscription"
        );

        let us = user_state.read().await;
        assert!(
            !us.active_subscriptions
                .values()
                .any(|entry| entry.agent_id == agent_id),
            "cancelled subscription should be removed from active_subscriptions"
        );
    }

    #[tokio::test]
    async fn withdraw_host_route_mismatch_preserves_root_but_cleans_stale_descendants() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer_link(&user_state, "peer-b").await;

        let host_id = Uuid::new_v4();
        let child_host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut child_route = Route::from_link(Link::new("child").unwrap());
        child_route.push(Link::new("test-link").unwrap());
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link(Link::new("other-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                child_host_id,
                crate::protocol::message::Host {
                    id: child_host_id,
                    name: "stale-child".to_string(),
                    route: child_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
        }
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: child_host_id,
                name: Some("stale-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: child_route,
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::WithdrawHost {
                id: host_id,
                route: Route::empty(),
            },
            &ctx,
        )
        .await
        .unwrap();

        {
            let us = user_state.read().await;
            assert!(us.hosts.contains_key(&host_id));
            assert!(!us.hosts.contains_key(&child_host_id));
            assert!(!us.registry.contains(&agent_id));
        }

        let withdrawn_hosts = drain_direct_messages(&mut peer_rx);
        assert_eq!(withdrawn_hosts.len(), 1);
        assert!(matches!(
            &withdrawn_hosts[0],
            DirectMessage::WithdrawHost { id, route }
                if *id == host_id && route.to_string() == "test-link"
        ));
    }

    #[tokio::test]
    async fn withdraw_host_missing_root_cleans_descendants_and_propagates() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer_link(&user_state, "peer-b").await;

        let host_id = Uuid::new_v4();
        let child_host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let mut child_route = Route::from_link(Link::new("child").unwrap());
        child_route.push(Link::new("test-link").unwrap());
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                child_host_id,
                crate::protocol::message::Host {
                    id: child_host_id,
                    name: "orphan-child".to_string(),
                    route: child_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
        }
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: child_host_id,
                name: Some("orphan-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: child_route,
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::WithdrawHost {
                id: host_id,
                route: Route::empty(),
            },
            &ctx,
        )
        .await
        .unwrap();

        {
            let us = user_state.read().await;
            assert!(!us.hosts.contains_key(&host_id));
            assert!(!us.hosts.contains_key(&child_host_id));
            assert!(!us.registry.contains(&agent_id));
        }

        let withdrawn_hosts = drain_direct_messages(&mut peer_rx);
        assert_eq!(withdrawn_hosts.len(), 1);
        assert!(matches!(
            &withdrawn_hosts[0],
            DirectMessage::WithdrawHost { id, route }
                if *id == host_id && route.to_string() == "test-link"
        ));
    }

    #[tokio::test]
    async fn withdraw_host_removes_agents_with_matching_route() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer_link(&user_state, "peer-b").await;

        let host_id = Uuid::new_v4();
        let agent1 = Uuid::new_v4();
        let agent2 = Uuid::new_v4();
        let agent3 = Uuid::new_v4();
        let deep_host_id = Uuid::new_v4();
        let other_host_id = Uuid::new_v4();
        let mut deep_route = Route::from_link(Link::new("host-b").unwrap());
        deep_route.push(Link::new("test-link").unwrap());
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link(Link::new("test-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                deep_host_id,
                crate::protocol::message::Host {
                    id: deep_host_id,
                    name: "deep-remote".to_string(),
                    route: deep_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                other_host_id,
                crate::protocol::message::Host {
                    id: other_host_id,
                    name: "other-remote".to_string(),
                    route: Route::from_link(Link::new("other-link").unwrap()),
                    version: "0.1.0".to_string(),
                },
            );
        }

        insert_remote_agent(
            &user_state,
            Agent {
                id: agent1,
                host_id,
                name: Some("a1".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("test-link").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent2,
                host_id: deep_host_id,
                name: Some("a2".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: deep_route,
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent3,
                host_id: other_host_id,
                name: Some("a3".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("other-link").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        handle_direct(
            &tx,
            DirectMessage::WithdrawHost {
                id: host_id,
                route: Route::empty(),
            },
            &ctx,
        )
        .await
        .unwrap();

        {
            let us = user_state.read().await;
            assert!(!us.hosts.contains_key(&host_id));
            assert!(!us.hosts.contains_key(&deep_host_id));
            assert!(us.hosts.contains_key(&other_host_id));
            assert!(!us.registry.contains(&agent1));
            assert!(!us.registry.contains(&agent2));
            assert!(us.registry.contains(&agent3));
        }

        let mut withdrawn_hosts: Vec<_> = drain_direct_messages(&mut peer_rx)
            .into_iter()
            .map(|msg| match msg {
                DirectMessage::WithdrawHost { id, route } => (id, route.to_string()),
                other => panic!("expected WithdrawHost, got {:?}", other),
            })
            .collect();
        withdrawn_hosts.sort_unstable_by_key(|(id, _)| id.as_u128());
        assert_eq!(withdrawn_hosts, vec![(host_id, "test-link".to_string())]);
    }
}
