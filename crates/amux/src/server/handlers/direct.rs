use crate::agent::Agent;
use crate::protocol::message::{DirectMessage, Message, ProtocolError};
use crate::protocol::route::Route;
use crate::server::ServerUserState;
use crate::server::connection::{
    ConnectionContext, ConnectionError, cancel_subscriptions_matching,
};
use crate::server::routing::{announce_agent_message, broadcast_to_peers};
use crate::transport::TransportError;
use tokio::sync::mpsc;

fn descendant_host_ids(
    us: &ServerUserState,
    root_host_id: uuid::Uuid,
    route_prefix: &Route,
) -> Vec<uuid::Uuid> {
    let mut ids: Vec<_> = us
        .hosts
        .iter()
        .filter(|(id, host)| **id != root_host_id && host.route.starts_with_route(route_prefix))
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable_by_key(|id| id.as_u128());
    ids
}

fn rewrite_descendant_host_routes(
    us: &mut ServerUserState,
    root_host_id: uuid::Uuid,
    old_route: &Route,
    new_route: &Route,
) -> usize {
    if old_route == new_route {
        return 0;
    }

    descendant_host_ids(us, root_host_id, old_route)
        .into_iter()
        .map(|id| {
            let host = us
                .hosts
                .get_mut(&id)
                .expect("descendant host should still exist while rewriting routes");
            let replaced = host.route.replace_prefix(old_route, new_route);
            debug_assert!(replaced, "descendant route should still match old prefix");
            1usize
        })
        .sum()
}

fn remove_descendant_hosts(
    us: &mut ServerUserState,
    root_host_id: uuid::Uuid,
    route_prefix: &Route,
) -> usize {
    descendant_host_ids(us, root_host_id, route_prefix)
        .into_iter()
        .filter(|id| us.hosts.remove(id).is_some())
        .count()
}

pub(super) async fn handle_direct(
    tx: &mpsc::Sender<Message>,
    message: DirectMessage,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match message {
        // In-band re-authentication for token refresh on established connections.
        DirectMessage::Reauth { token } => {
            let is_cloud = {
                let state = ctx.state.read().await;
                state.is_cloud_server
            };

            // Re-check minimum client version (config may have changed since connect)
            if let Some(ref name) = ctx.client_name {
                let min_version = {
                    let state = ctx.state.read().await;
                    state.config.minimum_client_versions.get(name).cloned()
                };
                if let Some(ref min_ver_str) = min_version {
                    let cv = ctx.client_version.as_deref().unwrap_or("");
                    let reject = match (
                        semver::Version::parse(cv),
                        semver::Version::parse(min_ver_str),
                    ) {
                        (Ok(client), Ok(minimum)) => client < minimum,
                        _ => true,
                    };
                    if reject {
                        let cv = cv.to_string();
                        tracing::warn!(
                            client_name = %name,
                            client_version = %cv,
                            minimum_version = %min_ver_str,
                            "re-auth: client version below minimum"
                        );
                        let _ = tx
                            .send(Message::Direct {
                                message: DirectMessage::ReauthResult {
                                    error: Some(ProtocolError::UpgradeRequired {
                                        minimum_version: min_ver_str.clone(),
                                        client_version: cv.clone(),
                                    }),
                                },
                            })
                            .await;
                        return Err(ConnectionError::UpgradeRequired {
                            minimum_version: min_ver_str.clone(),
                            client_version: cv,
                        });
                    }
                }
            }

            if is_cloud {
                let (validator, host, tcp_port) = {
                    let state = ctx.state.read().await;
                    let validator = state
                        .jwt_validator
                        .clone()
                        .expect("is_cloud_server requires jwt_validator");
                    // Safe: cloud mode validation guarantees tcp_port is Some
                    let tcp_port = state.config.tcp_port.expect("cloud mode requires tcp_port");
                    (validator, state.config.host_name.clone(), tcp_port)
                };

                match validator.validate(&token, &host, tcp_port).await {
                    Ok(claims) => {
                        let token_user_id = claims.sub.parse::<uuid::Uuid>().map_err(|_| {
                            tracing::error!(sub = %claims.sub, "re-auth invalid user_id");
                            ConnectionError::InvalidCredentials
                        })?;
                        if token_user_id != ctx.user_id {
                            tracing::error!("re-auth user_id mismatch");
                            let _ = tx
                                .send(Message::Direct {
                                    message: DirectMessage::ReauthResult {
                                        error: Some(ProtocolError::InvalidCredentials),
                                    },
                                })
                                .await;
                            return Err(ConnectionError::InvalidCredentials);
                        }
                        tracing::debug!("re-authenticated");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "re-auth token validation failed");
                        let _ = tx
                            .send(Message::Direct {
                                message: DirectMessage::ReauthResult {
                                    error: Some(ProtocolError::InvalidCredentials),
                                },
                            })
                            .await;
                        return Ok(());
                    }
                }
            }

            let _ = tx
                .send(Message::Direct {
                    message: DirectMessage::ReauthResult { error: None },
                })
                .await;
            Ok(())
        }

        DirectMessage::AnnounceAgent {
            agent_id,
            host_id,
            name,
            command,
            working_dir,
            agent_type,
            structured_protocol,
            readonly,
            args,
            created_at,
        } => {
            let mut us = ctx.user_state.write().await;

            // Local agent takes precedence — skip if we own this agent
            if us.agents.contains_key(&agent_id) {
                tracing::debug!(agent_id = %agent_id, "ignoring announce for local agent");
                return Ok(());
            }

            // Only accept agent metadata from the selected next hop for this host.
            // This prevents stale or alternate paths from republishing the agent on
            // a route we no longer consider canonical, which would then cause the
            // real sender's later WithdrawAgent to be ignored as a link mismatch.
            let host_ok = us.hosts.get(&host_id).is_some_and(
                |host| matches!(host.route.peek(), Some(link) if link == ctx.link_name),
            );
            if !host_ok {
                let reason = if us.hosts.contains_key(&host_id) {
                    "non-selected host route"
                } else {
                    "unknown host"
                };
                tracing::warn!(agent_id = %agent_id, host_id = %host_id, peer = %ctx.link_name, "ignoring remote agent announcement: {reason}");
                return Ok(());
            }

            let route = us
                .hosts
                .get(&host_id)
                .expect("host_ok implies host exists")
                .route
                .clone();
            let info = Agent {
                id: agent_id,
                host_id,
                name: name.clone(),
                command: command.clone(),
                working_dir: working_dir.clone(),
                route,
                agent_type: agent_type.clone(),
                structured_protocol: structured_protocol.clone(),
                readonly,
                args: args.clone(),
                created_at,
            };

            let announce = announce_agent_message(&info);
            if let Err(e) = us.registry.register_remote(info) {
                tracing::warn!(error = %e, agent_id = %agent_id, "ignoring invalid remote announcement");
                return Ok(());
            }

            tracing::info!(agent_id = %agent_id, name = ?name, "stored remote agent");

            // Propagate to other peers with our stored route
            broadcast_to_peers(&mut us, &announce, Some(&ctx.link_name));

            Ok(())
        }

        DirectMessage::WithdrawAgent { agent_id } => {
            let mut us = ctx.user_state.write().await;

            // Only remove if the stored link matches the sender
            let should_remove = us.registry.get(&agent_id).is_some_and(|e| {
                e.is_remote()
                    && us.hosts.get(&e.host_id).is_some_and(
                        |host| matches!(host.route.peek(), Some(link) if link == ctx.link_name),
                    )
            });

            if should_remove {
                us.registry.remove(&agent_id);
                tracing::info!(agent_id = %agent_id, "withdrew remote agent");

                // Propagate to other peers
                broadcast_to_peers(
                    &mut us,
                    &DirectMessage::WithdrawAgent { agent_id },
                    Some(&ctx.link_name),
                );
            } else {
                tracing::debug!(agent_id = %agent_id, "ignoring withdraw (link mismatch)");
            }

            Ok(())
        }

        DirectMessage::AnnounceHost {
            id,
            name,
            route: received_route,
            version,
        } => {
            let host_id = {
                let state = ctx.state.read().await;
                state.host_id
            };

            // Skip our own host announcement
            if id == host_id {
                tracing::debug!("ignoring announce for own host");
                return Ok(());
            }

            let mut us = ctx.user_state.write().await;
            let old_route = us.hosts.get(&id).map(|host| host.route.clone());

            let mut our_route = received_route;
            our_route.push(&ctx.link_name);

            let info = crate::protocol::message::Host {
                id,
                name: name.clone(),
                route: our_route.clone(),
                version: version.clone(),
            };

            us.hosts.insert(id, info);

            // Keep descendant host routes aligned with the selected route for this host.
            let rewritten_descendants = old_route
                .as_ref()
                .map(|old_route| rewrite_descendant_host_routes(&mut us, id, old_route, &our_route))
                .unwrap_or(0);

            tracing::info!(
                host_id = %id,
                name = %name,
                rewritten_descendants,
                "stored remote host"
            );

            broadcast_to_peers(
                &mut us,
                &DirectMessage::AnnounceHost {
                    id,
                    name,
                    route: our_route,
                    version,
                },
                Some(&ctx.link_name),
            );

            Ok(())
        }

        DirectMessage::WithdrawHost {
            id,
            route: received_route,
        } => {
            let mut us = ctx.user_state.write().await;

            let mut withdrawn_route = received_route;
            withdrawn_route.push(&ctx.link_name);

            let root_matches = us
                .hosts
                .get(&id)
                .is_some_and(|h| h.route == withdrawn_route);
            tracing::info!(host_id = %id, root_matches, "received withdraw host");

            let ServerUserState {
                ref hosts,
                ref mut registry,
                ..
            } = *us;
            let removed = registry.remove_where(
                |hid| hosts.get(&hid).map(|h| h.route.clone()),
                |r| r.starts_with_route(&withdrawn_route),
            );
            if !removed.is_empty() {
                tracing::info!(count = removed.len(), host_id = %id, "removed agents for withdrawn host");
            }

            let cancelled = cancel_subscriptions_matching(&mut us, |entry| {
                entry.dst.starts_with_route(&withdrawn_route)
            });
            if !cancelled.is_empty() {
                tracing::info!(
                    count = cancelled.len(),
                    host_id = %id,
                    "cancelled subscriptions for withdrawn host"
                );
            }

            let removed_descendants = remove_descendant_hosts(&mut us, id, &withdrawn_route);
            if removed_descendants > 0 {
                tracing::info!(
                    count = removed_descendants,
                    host_id = %id,
                    "removed descendant hosts for withdrawn host"
                );
            }

            if root_matches {
                us.hosts.remove(&id);
                tracing::info!(host_id = %id, "withdrew remote host");
            } else {
                tracing::debug!(host_id = %id, "propagating withdraw host without matching local root");
            }

            broadcast_to_peers(
                &mut us,
                &DirectMessage::WithdrawHost {
                    id,
                    route: withdrawn_route,
                },
                Some(&ctx.link_name),
            );

            Ok(())
        }

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
    use super::super::tests::*;
    use super::*;
    use crate::agent::Agent;
    use crate::protocol::message::{CreateAgentRequest, DirectMessage, Message, TerminalSize};
    use crate::protocol::route::Route;
    use crate::server::{SUBSCRIPTION_LEASE_DURATION, SubscriptionMode};
    use chrono::Utc;
    use std::path::{Path, PathBuf};
    use tokio::sync::oneshot;
    use tokio::time::Instant;
    use uuid::Uuid;

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
                    route: Route::from_link("test-link"),
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
        assert_eq!(route.pop(), Some("test-link".to_string()));
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
            let mut route = Route::from_link("host-a");
            route.push("test-link");
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
        assert_eq!(route.pop(), Some("test-link".to_string()));
        assert_eq!(route.pop(), Some("host-a".to_string()));
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
                    route: Route::from_link("other-link"),
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
                    route: Route::from_link("other-link"),
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
                route: Route::from_link("other-link"),
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
                route: Route::from_link("test-link"),
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
                route: Route::from_link("other-link"),
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
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                second_host_id,
                crate::protocol::message::Host {
                    id: second_host_id,
                    name: "second-host".to_string(),
                    route: Route::from_link("test-link"),
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

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name, Some("second".to_string()));
        assert_eq!(entry.working_dir, PathBuf::from("/second"));
        assert_eq!(
            entry.args,
            vec!["--allow-dangerously-skip-permissions".to_string()]
        );
        drop(us);

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
        assert_eq!(route.pop(), Some("test-link".to_string()));
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
                route: Route::from_link("peer-a"),
                version: "0.2.0".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        let info = &us.hosts[&host_id];
        let mut route = info.route.clone();
        assert_eq!(route.pop(), Some("test-link".to_string()));
        assert_eq!(route.pop(), Some("peer-a".to_string()));
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

        let mut child_route = Route::from_link("child");
        child_route.push("old-link");
        let mut grandchild_route = Route::from_link("grand");
        grandchild_route.push("child");
        grandchild_route.push("old-link");

        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote-parent".to_string(),
                    route: Route::from_link("old-link"),
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
        drop(us);

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
                route: Route::from_link("cloud"),
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
                    route: Route::from_link("test-link"),
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
        let mut full_route = Route::from_link("child");
        full_route.push("test-link");
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
                Uuid::new_v4(),
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
                route: Route::from_link("child"),
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
        let mut child_route = Route::from_link("child");
        child_route.push("test-link");
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link("other-link"),
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

        let us = user_state.read().await;
        assert!(us.hosts.contains_key(&host_id));
        assert!(!us.hosts.contains_key(&child_host_id));
        assert!(!us.registry.contains(&agent_id));
        drop(us);

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
        let mut child_route = Route::from_link("child");
        child_route.push("test-link");
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

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
        assert!(!us.hosts.contains_key(&child_host_id));
        assert!(!us.registry.contains(&agent_id));
        drop(us);

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
        let mut deep_route = Route::from_link("host-b");
        deep_route.push("test-link");
        {
            let mut us = user_state.write().await;
            us.hosts.insert(
                host_id,
                crate::protocol::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link("test-link"),
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
                    route: Route::from_link("other-link"),
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
                route: Route::from_link("test-link"),
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
                route: Route::from_link("other-link"),
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

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
        assert!(!us.hosts.contains_key(&deep_host_id));
        assert!(us.hosts.contains_key(&other_host_id));
        assert!(!us.registry.contains(&agent1));
        assert!(!us.registry.contains(&agent2));
        assert!(us.registry.contains(&agent3));
        drop(us);

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
