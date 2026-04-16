use super::subscription::{
    SubscriptionHandle, remove_subscription_if_reply_failed, spawn_subscription_stream,
    subscribe_error,
};
use crate::agent::StopPolicy;
use crate::protocol::message::{
    Message, ProtocolError, RoutableMessage, SubscribeQuery, SubscriptionCloseReason,
};
use crate::protocol::route::Route;
use crate::server::connection::{ConnectionContext, extend_subscription, unsubscribe_subscription};
use crate::server::routing::{
    create_agent, delete_local_agent, handle_subscribe, rename_local_agent,
};
use crate::server::{
    SUBSCRIPTION_LEASE_DURATION, SubscriptionEntry, SubscriptionMode, send_routable_via_full_dst,
    subscription_lease_ms,
};
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

fn reply_routes(src: Route, msg_type: &str) -> Option<(Route, Route)> {
    match Route::reply(src) {
        Some(routes) => Some(routes),
        None => {
            tracing::warn!(msg_type, "dropping routable message with empty src route");
            None
        }
    }
}

pub(super) async fn handle_routable(
    tx: &mpsc::Sender<Message>,
    mut src: Route,
    mut dst: Route,
    request_id: u64,
    payload: Vec<u8>,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    // Check if this message needs forwarding — payload forwarded verbatim
    if let Some(next_hop) = dst.pop() {
        let route_tx = {
            let us = ctx.user_state.read().await;
            us.routes.get(&next_hop).cloned()
        };

        match route_tx {
            Some(route_tx) => {
                src.push(&next_hop);
                if route_tx
                    .send(Message::Routable {
                        src,
                        dst,
                        request_id,
                        payload,
                    })
                    .await
                    .is_err()
                {
                    tracing::debug!(next_hop = %next_hop, "forwarding failed (channel closed)");
                }
            }
            None => {
                tracing::debug!(next_hop = %next_hop, "no route, sending Unreachable");
                if let Some((reply_src, reply_dst)) = Route::reply(src) {
                    let _ = tx
                        .send(Message::routable(
                            reply_src,
                            reply_dst,
                            request_id,
                            &RoutableMessage::Unreachable { request_id },
                        ))
                        .await;
                }
            }
        }

        return Ok(());
    }

    // Local delivery — two-step decode
    let message = match RoutableMessage::decode(&payload) {
        Ok(RoutableMessage::Unknown) => {
            tracing::warn!("received unsupported routable message");
            let Some((reply_src, reply_dst)) = reply_routes(src, "UnsupportedMessage") else {
                return Ok(());
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &RoutableMessage::UnsupportedMessage,
                ))
                .await;
            return Ok(());
        }
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode routable payload");
            let Some((reply_src, reply_dst)) = reply_routes(src, "DecodeError") else {
                return Ok(());
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &RoutableMessage::InvalidMessage,
                ))
                .await;
            return Ok(());
        }
    };

    match &message {
        RoutableMessage::RawInput { .. }
        | RoutableMessage::RawOutput { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::StructuredInput { .. }
        | RoutableMessage::ExtendSubscription { .. } => {}
        other => tracing::debug!(
            msg_type = std::any::type_name_of_val(other),
            "decoded routable"
        ),
    }

    match message {
        RoutableMessage::SubscribeRaw {
            agent_id,
            terminal_size,
        } => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "SubscribeRaw") else {
                return Ok(());
            };
            let result = handle_subscribe(&ctx.user_state, &agent_id, terminal_size).await;

            match result {
                Ok(buffer_reader) => {
                    let subscription_id = Uuid::new_v4();
                    let (handle, cancel_rx) = SubscriptionHandle::register(
                        ctx,
                        subscription_id,
                        agent_id,
                        SubscriptionMode::Raw,
                        reply_src.clone(),
                        reply_dst.clone(),
                    )
                    .await;
                    if tx
                        .send(Message::routable(
                            reply_src.clone(),
                            reply_dst.clone(),
                            request_id,
                            &RoutableMessage::SubscribeRawResult {
                                subscription_id,
                                lease_ms: subscription_lease_ms(),
                                error: None,
                            },
                        ))
                        .await
                        .is_err()
                    {
                        remove_subscription_if_reply_failed(&ctx.user_state, subscription_id).await;
                        tracing::debug!(
                            subscription_id = %subscription_id,
                            agent_id = %agent_id,
                            "subscribe result send failed before stream spawn"
                        );
                        return Ok(());
                    }

                    tracing::info!(
                        subscription_id = %subscription_id,
                        agent_id = %agent_id,
                        mode = SubscriptionMode::Raw.as_str(),
                        lease_ms = subscription_lease_ms(),
                        "subscription created"
                    );

                    spawn_subscription_stream(
                        buffer_reader,
                        handle,
                        cancel_rx,
                        |subscription_id, data| RoutableMessage::RawOutput {
                            subscription_id,
                            data,
                        },
                        ctx,
                    )
                    .await;

                    Ok(())
                }
                Err(e) => {
                    let _ = tx
                        .send(Message::routable(
                            reply_src,
                            reply_dst,
                            request_id,
                            &RoutableMessage::SubscribeRawResult {
                                subscription_id: Uuid::nil(),
                                lease_ms: 0,
                                error: Some(subscribe_error(&e)),
                            },
                        ))
                        .await;
                    Ok(())
                }
            }
        }

        RoutableMessage::SubscribeStructured { agent_id, query } => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "SubscribeStructured") else {
                return Ok(());
            };
            if matches!(query, Some(SubscribeQuery::Unknown)) {
                let _ = tx
                    .send(Message::routable(
                        reply_src,
                        reply_dst,
                        request_id,
                        &RoutableMessage::SubscribeStructuredResult {
                            subscription_id: Uuid::nil(),
                            seq: 0,
                            structured_protocol: None,
                            lease_ms: 0,
                            error: Some(ProtocolError::UnsupportedSubscribeQuery),
                        },
                    ))
                    .await;
                return Ok(());
            }
            let subscribed = {
                let us = ctx.user_state.read().await;
                let Some(session) = us.agents.get(&agent_id) else {
                    let _ = tx
                        .send(Message::routable(
                            reply_src,
                            reply_dst,
                            request_id,
                            &RoutableMessage::SubscribeStructuredResult {
                                subscription_id: Uuid::nil(),
                                seq: 0,
                                structured_protocol: None,
                                lease_ms: 0,
                                error: Some(ProtocolError::NoAgentFound),
                            },
                        ))
                        .await;
                    return Ok(());
                };
                session
                    .subscribe_with_query(query)
                    .await
                    .map(|(reader, current_seq)| {
                        (reader, current_seq, session.structured_protocol())
                    })
            };

            let Some((reader, current_seq, structured_protocol)) = subscribed else {
                let _ = tx
                    .send(Message::routable(
                        reply_src,
                        reply_dst,
                        request_id,
                        &RoutableMessage::SubscribeStructuredResult {
                            subscription_id: Uuid::nil(),
                            seq: 0,
                            structured_protocol: None,
                            lease_ms: 0,
                            error: Some(ProtocolError::NoAgentFound),
                        },
                    ))
                    .await;
                return Ok(());
            };

            let subscription_id = Uuid::new_v4();
            let (handle, cancel_rx) = SubscriptionHandle::register(
                ctx,
                subscription_id,
                agent_id,
                SubscriptionMode::Structured,
                reply_src.clone(),
                reply_dst.clone(),
            )
            .await;
            if tx
                .send(Message::routable(
                    reply_src.clone(),
                    reply_dst.clone(),
                    request_id,
                    &RoutableMessage::SubscribeStructuredResult {
                        subscription_id,
                        seq: current_seq,
                        structured_protocol,
                        lease_ms: subscription_lease_ms(),
                        error: None,
                    },
                ))
                .await
                .is_err()
            {
                remove_subscription_if_reply_failed(&ctx.user_state, subscription_id).await;
                tracing::debug!(
                    subscription_id = %subscription_id,
                    agent_id = %agent_id,
                    "subscribe result send failed before stream spawn"
                );
                return Ok(());
            }

            tracing::info!(
                subscription_id = %subscription_id,
                agent_id = %agent_id,
                mode = SubscriptionMode::Structured.as_str(),
                lease_ms = subscription_lease_ms(),
                "subscription created"
            );

            spawn_subscription_stream(
                reader,
                handle,
                cancel_rx,
                |subscription_id, envelope| RoutableMessage::StructuredOutput {
                    subscription_id,
                    seq: envelope.seq,
                    payload: envelope.payload,
                },
                ctx,
            )
            .await;

            Ok(())
        }

        RoutableMessage::ExtendSubscription { subscription_id } => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "ExtendSubscription") else {
                return Ok(());
            };

            let lease_deadline = Instant::now() + SUBSCRIPTION_LEASE_DURATION;
            let response_message = match extend_subscription(
                &ctx.user_state,
                subscription_id,
                lease_deadline,
            )
            .await
            {
                Some(agent_id) => {
                    tracing::debug!(
                        subscription_id = %subscription_id,
                        agent_id = %agent_id,
                        lease_ms = subscription_lease_ms(),
                        "subscription extended"
                    );
                    RoutableMessage::ExtendSubscriptionResult {
                        subscription_id,
                        lease_ms: subscription_lease_ms(),
                        error: None,
                    }
                }
                None => {
                    tracing::debug!(subscription_id = %subscription_id, "late or unknown extend");
                    RoutableMessage::ExtendSubscriptionResult {
                        subscription_id,
                        lease_ms: 0,
                        error: Some(ProtocolError::UnknownSubscription),
                    }
                }
            };

            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &response_message,
                ))
                .await;
            Ok(())
        }

        RoutableMessage::Unsubscribe { subscription_id } => {
            if let Some(entry) = unsubscribe_subscription(&ctx.user_state, subscription_id).await {
                let SubscriptionEntry {
                    subscription_id,
                    agent_id,
                    cancel,
                    dst,
                    ..
                } = entry;
                drop(cancel);
                tracing::info!(
                    subscription_id = %subscription_id,
                    agent_id = %agent_id,
                    "explicit unsubscribe handled"
                );
                let _ = send_routable_via_full_dst(
                    &ctx.user_state,
                    &dst,
                    &RoutableMessage::SubscriptionClosed {
                        subscription_id,
                        reason: SubscriptionCloseReason::Unsubscribed,
                    },
                )
                .await;
            } else {
                tracing::debug!(subscription_id = %subscription_id, "late or unknown unsubscribe");
            }
            Ok(())
        }

        RoutableMessage::CreateAgent(req) => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "CreateAgent") else {
                return Ok(());
            };
            let agent_id = req.agent_id;
            let (host_id, is_cloud_server) = {
                let state = ctx.state.read().await;
                (state.host_id, state.is_cloud_server)
            };
            let error = if is_cloud_server {
                Some("cloud relays do not host local agents".to_string())
            } else {
                create_agent(&ctx.user_state, &ctx.event_tx, req, ctx.user_id, host_id)
                    .await
                    .err()
            };
            let response_message = match error {
                None => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: None,
                },
                Some(message) => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: Some(ProtocolError::ServerError { message }),
                },
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &response_message,
                ))
                .await;
            Ok(())
        }

        RoutableMessage::RenameAgent(req) => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "RenameAgent") else {
                return Ok(());
            };
            let agent_id = req.agent_id;
            let host_id = {
                let state = ctx.state.read().await;
                state.host_id
            };
            let response_message = {
                let mut us = ctx.user_state.write().await;
                match rename_local_agent(&mut us, host_id, &req) {
                    Ok(_) => RoutableMessage::RenameAgentResult {
                        agent_id,
                        error: None,
                    },
                    Err(e) => RoutableMessage::RenameAgentResult {
                        agent_id,
                        error: Some(ProtocolError::ServerError {
                            message: e.to_string(),
                        }),
                    },
                }
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &response_message,
                ))
                .await;
            Ok(())
        }

        RoutableMessage::DeleteAgent { agent_id } => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "DeleteAgent") else {
                return Ok(());
            };
            let session_to_stop = {
                let mut us = ctx.user_state.write().await;
                delete_local_agent(&mut us, agent_id)
            };
            let response_message = match session_to_stop {
                Some(session) => {
                    session.stop(StopPolicy::Interrupt).await;
                    RoutableMessage::DeleteAgentResult {
                        agent_id,
                        error: None,
                    }
                }
                None => RoutableMessage::DeleteAgentResult {
                    agent_id,
                    error: Some(ProtocolError::ServerError {
                        message: format!("Agent not found: {agent_id}"),
                    }),
                },
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &response_message,
                ))
                .await;
            Ok(())
        }

        RoutableMessage::RawInput { agent_id, data } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id)
                && let Some(pty) = session.pty_handle()
            {
                let _ = pty.send_input(data).await;
            }
            Ok(())
        }

        RoutableMessage::StructuredInput {
            agent_id,
            seq: client_seq,
            payload,
        } => {
            tracing::debug!(%agent_id, client_seq, "structured input received");
            let Some((reply_src, reply_dst)) = reply_routes(src, "StructuredInput") else {
                return Ok(());
            };
            let us = ctx.user_state.read().await;
            let error = if let Some(session) = us.agents.get(&agent_id) {
                session
                    .send_structured_input(client_seq, payload)
                    .await
                    .err()
            } else {
                tracing::warn!(%agent_id, "structured input rejected: agent not found");
                Some(ProtocolError::NoAgentFound)
            };
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &RoutableMessage::StructuredInputResult { agent_id, error },
                ))
                .await;
            Ok(())
        }

        // Response messages that arrived at their destination (empty dst)
        RoutableMessage::SubscribeRawResult { .. }
        | RoutableMessage::SubscribeStructuredResult { .. }
        | RoutableMessage::ExtendSubscriptionResult { .. }
        | RoutableMessage::CreateAgentResult { .. }
        | RoutableMessage::RenameAgentResult { .. }
        | RoutableMessage::DeleteAgentResult { .. }
        | RoutableMessage::RawOutput { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::StructuredInputResult { .. }
        | RoutableMessage::SubscriptionClosed { .. }
        | RoutableMessage::Unreachable { .. }
        | RoutableMessage::UnsupportedMessage
        | RoutableMessage::InvalidMessage
        | RoutableMessage::Unknown => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{handle_message, tests::*};
    use crate::agent::{Agent, AgentSession, LocalAgentNameSource};
    use crate::protocol::message::{
        AgentType, CreateAgentRequest, DirectMessage, Message, RenameAgentRequest, SubscribeQuery,
    };
    use crate::protocol::route::Route;
    use crate::protocol::{ProtocolError, RoutableMessage, SubscriptionCloseReason};
    use crate::server::{
        ConnectionHandle, SUBSCRIPTION_LEASE_DURATION, SubscriptionMode, subscription_lease_ms,
    };
    use chrono::Utc;
    use serde::Serialize;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot};
    use tokio::time::Instant;
    use uuid::Uuid;

    #[tokio::test]
    async fn rename_agent_renames_local_agent_and_reannounces() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        insert_local_claude(&user_state, agent_id, None, LocalAgentNameSource::Unset).await;

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::RenameAgent(RenameAgentRequest {
                agent_id,
                name: "renamed-agent".to_string(),
            }),
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::RenameAgentResult {
                agent_id: id,
                error: None,
            } if id == agent_id
        ));
        drop(msgs);

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("renamed-agent"));
        assert_eq!(
            us.agents
                .get(&agent_id)
                .and_then(|session| session.local_name_source()),
            Some(LocalAgentNameSource::Amux)
        );
        drop(us);

        let forwarded = peer_rx
            .try_recv()
            .expect("rename should be re-announced to peers");
        assert!(matches!(
            forwarded,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    ..
                }
            } if id == agent_id && name == "renamed-agent"
        ));
    }

    #[tokio::test]
    async fn rename_agent_same_name_upgrades_to_amux_without_reannounce() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            agent_id,
            Some("shared-name"),
            LocalAgentNameSource::ProviderSlug,
        )
        .await;

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::RenameAgent(RenameAgentRequest {
                agent_id,
                name: "shared-name".to_string(),
            }),
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::RenameAgentResult {
                agent_id: id,
                error: None,
            } if id == agent_id
        ));
        drop(msgs);

        let us = user_state.read().await;
        assert_eq!(
            us.registry.get(&agent_id).unwrap().name.as_deref(),
            Some("shared-name")
        );
        assert_eq!(
            us.agents
                .get(&agent_id)
                .and_then(|session| session.local_name_source()),
            Some(LocalAgentNameSource::Amux)
        );
        drop(us);

        assert!(
            peer_rx.try_recv().is_err(),
            "provenance-only manual updates should not re-announce"
        );
    }

    #[tokio::test]
    async fn rename_agent_collision_returns_error_without_reannounce() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let owner_id = Uuid::new_v4();
        let candidate_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            owner_id,
            Some("taken-name"),
            LocalAgentNameSource::Amux,
        )
        .await;
        insert_local_claude(&user_state, candidate_id, None, LocalAgentNameSource::Unset).await;

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::RenameAgent(RenameAgentRequest {
                agent_id: candidate_id,
                name: "taken-name".to_string(),
            }),
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::RenameAgentResult {
                agent_id: id,
                error: Some(ProtocolError::ServerError { message: ref err }),
            } if id == candidate_id && err == "Agent already exists: taken-name"
        ));
        drop(msgs);

        let us = user_state.read().await;
        assert_eq!(
            us.registry.resolve(&us.hosts, "taken-name").unwrap().id,
            owner_id
        );
        assert_eq!(us.registry.get(&candidate_id).unwrap().name, None);
        drop(us);

        assert!(
            peer_rx.try_recv().is_err(),
            "failed updates should not re-announce"
        );
    }

    #[tokio::test]
    async fn delete_agent_withdraws_local_agent_and_replies_success() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::DeleteAgent { agent_id },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::DeleteAgentResult {
                agent_id: id,
                error: None,
            } if id == agent_id
        ));
        drop(msgs);

        let us = user_state.read().await;
        assert!(!us.agents.contains_key(&agent_id));
        assert!(us.registry.get(&agent_id).is_none());
        drop(us);

        let forwarded = peer_rx
            .try_recv()
            .expect("delete should withdraw from peers");
        assert!(matches!(
            forwarded,
            Message::Direct {
                message: DirectMessage::WithdrawAgent { agent_id: id }
            } if id == agent_id
        ));
    }

    #[tokio::test]
    async fn delete_agent_rejects_remote_registry_entry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: Uuid::new_v4(),
                name: Some("remote-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link("upstream"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::DeleteAgent { agent_id },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::DeleteAgentResult {
                agent_id: id,
                error: Some(ProtocolError::ServerError { message: ref err }),
            } if id == agent_id && err == &format!("Agent not found: {agent_id}")
        ));
        drop(msgs);

        let us = user_state.read().await;
        assert!(us.registry.contains(&agent_id));
        drop(us);

        assert!(
            peer_rx.try_recv().is_err(),
            "failed delete should not withdraw from peers"
        );
    }

    #[tokio::test]
    async fn forwarding_to_nonexistent_route_sends_unreachable() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("nonexistent-hop"),
            42,
            &RoutableMessage::RawInput {
                agent_id: Uuid::new_v4(),
                data: vec![0x41],
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable {
            payload,
            request_id,
            ..
        } = &msgs[0]
        else {
            panic!("expected Routable, got {:?}", msgs[0]);
        };
        assert_eq!(*request_id, 42);
        let reply = RoutableMessage::decode(payload).unwrap();
        assert!(
            matches!(reply, RoutableMessage::Unreachable { request_id: 42 }),
            "expected Unreachable, got {:?}",
            reply
        );
    }

    #[tokio::test]
    async fn forwarding_over_closed_channel_is_silently_dropped() {
        let (state, user_state) = test_state().await;

        let (peer_tx, peer_rx) = mpsc::channel::<Message>(16);
        {
            let mut us = user_state.write().await;
            us.routes.insert(
                "dead-peer".to_string(),
                ConnectionHandle::new(peer_tx, Arc::new(AtomicU64::new(1))),
            );
        }
        drop(peer_rx);

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("dead-peer"),
            7,
            &RoutableMessage::SubscribeRaw {
                agent_id: Uuid::new_v4(),
                terminal_size: None,
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "channel-closed forwarding failures cannot send Unreachable (src consumed)"
        );
    }

    #[tokio::test]
    async fn forwarding_with_empty_src_does_not_send_unreachable() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = Message::Routable {
            src: Route::empty(),
            dst: Route::from_link("nonexistent"),
            request_id: 1,
            payload: vec![0x41],
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "empty-src forwarding failures should not attempt a reply"
        );
    }

    #[tokio::test]
    async fn invalid_payload_returns_invalid_message() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = Message::Routable {
            src: Route::from_link("sender"),
            dst: Route::empty(),
            request_id: 42,
            payload: vec![0xFF, 0xFE, 0xFD],
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { payload, .. } = &msgs[0] else {
            panic!("expected Routable reply, got {:?}", msgs[0]);
        };
        let reply = RoutableMessage::decode(payload).unwrap();
        assert!(
            matches!(reply, RoutableMessage::InvalidMessage),
            "expected InvalidMessage, got {:?}",
            reply
        );
    }

    #[tokio::test]
    async fn unknown_routable_variant_returns_unsupported_message() {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum FutureRoutableMessage {
            FancyPing { seq: u64 },
        }

        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let payload =
            rmp_serde::to_vec_named(&FutureRoutableMessage::FancyPing { seq: 7 }).unwrap();
        let msg = Message::Routable {
            src: Route::from_link("sender"),
            dst: Route::empty(),
            request_id: 42,
            payload,
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { payload, .. } = &msgs[0] else {
            panic!("expected Routable reply, got {:?}", msgs[0]);
        };
        let reply = RoutableMessage::decode(payload).unwrap();
        assert!(
            matches!(reply, RoutableMessage::UnsupportedMessage),
            "expected UnsupportedMessage, got {:?}",
            reply
        );
    }

    #[tokio::test]
    async fn invalid_payload_with_empty_src_is_dropped_without_panic() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = Message::Routable {
            src: Route::empty(),
            dst: Route::empty(),
            request_id: 1,
            payload: vec![0xFF],
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;
        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "empty-src decode errors should be dropped (no reply path)"
        );
    }

    #[tokio::test]
    async fn request_requiring_reply_with_empty_src_is_dropped_without_panic() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let payload = RoutableMessage::SubscribeRaw {
            agent_id: Uuid::new_v4(),
            terminal_size: None,
        }
        .encode()
        .unwrap();

        let msg = Message::Routable {
            src: Route::empty(),
            dst: Route::empty(),
            request_id: 2,
            payload,
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;
        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "empty-src requests should be dropped (no panic, no reply)"
        );
    }

    #[tokio::test]
    async fn routable_forwarded_to_next_hop() {
        let (state, user_state) = test_state().await;

        let (peer_tx, mut peer_rx) = mpsc::channel::<Message>(16);
        {
            let mut us = user_state.write().await;
            us.routes.insert(
                "peer-a".to_string(),
                ConnectionHandle::new(peer_tx, Arc::new(AtomicU64::new(1))),
            );
        }

        let ctx = test_ctx(state, user_state);
        let (tx, _written) = mock_tx();

        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("peer-a"),
            1,
            &RoutableMessage::RawInput {
                agent_id: Uuid::new_v4(),
                data: vec![0x41],
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();

        let forwarded = peer_rx
            .try_recv()
            .expect("message should be forwarded to peer-a");
        let Message::Routable {
            src, dst, payload, ..
        } = forwarded
        else {
            panic!("expected Routable, got {:?}", forwarded);
        };

        assert!(dst.is_empty());
        let mut src = src;
        assert_eq!(src.pop(), Some("peer-a".to_string()));
        assert_eq!(src.pop(), Some("sender".to_string()));

        let inner = RoutableMessage::decode(&payload).unwrap();
        assert!(matches!(inner, RoutableMessage::RawInput { .. }));
    }

    #[tokio::test]
    async fn response_variants_are_noops_at_destination() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let responses = vec![
            RoutableMessage::SubscribeRawResult {
                subscription_id: Uuid::new_v4(),
                lease_ms: subscription_lease_ms(),
                error: None,
            },
            RoutableMessage::SubscribeStructuredResult {
                subscription_id: Uuid::new_v4(),
                seq: 0,
                structured_protocol: None,
                lease_ms: subscription_lease_ms(),
                error: None,
            },
            RoutableMessage::ExtendSubscriptionResult {
                subscription_id: Uuid::new_v4(),
                lease_ms: subscription_lease_ms(),
                error: None,
            },
            RoutableMessage::CreateAgentResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::RenameAgentResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::DeleteAgentResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::RawOutput {
                subscription_id: Uuid::new_v4(),
                data: vec![0x41],
            },
            RoutableMessage::StructuredOutput {
                subscription_id: Uuid::new_v4(),
                seq: 1,
                payload: json!({"type": "hook.stop", "cwd": "/tmp", "stop_hook_active": false}),
            },
            RoutableMessage::StructuredInputResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::SubscriptionClosed {
                subscription_id: Uuid::new_v4(),
                reason: SubscriptionCloseReason::SourceClosed,
            },
            RoutableMessage::Unreachable { request_id: 99 },
            RoutableMessage::UnsupportedMessage,
            RoutableMessage::InvalidMessage,
        ];

        for response in responses {
            let response_type = response.type_label().to_string();
            let msg = Message::routable(Route::from_link("sender"), Route::empty(), 1, &response);
            handle_message(&tx, msg, &ctx).await.unwrap();
            tokio::task::yield_now().await;

            let mut msgs = written.lock().await;
            assert!(
                msgs.is_empty(),
                "response variant {response_type} at destination should produce no output, got {:?}",
                *msgs
            );
            msgs.clear();
        }
    }

    #[tokio::test]
    async fn create_agent_is_rejected_on_cloud_relay() {
        let (state, user_state) = test_state().await;
        {
            let mut s = state.write().await;
            s.is_cloud_server = true;
        }
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::CreateAgent(CreateAgentRequest {
                agent_id,
                name: Some("cloud-agent".to_string()),
                agent_type: AgentType::TestAgent {
                    command: dummy_pty_command(),
                },
                working_dir: dummy_working_dir(),
                terminal_size: None,
                args: vec![],
            }),
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::CreateAgentResult {
                agent_id: id,
                error: Some(ProtocolError::ServerError { message: ref msg }),
            } if id == agent_id && msg.contains("cloud relays do not host local agents")
        ));
        drop(msgs);

        let us = user_state.read().await;
        assert!(!us.agents.contains_key(&agent_id));
        assert!(!us.registry.contains(&agent_id));
    }

    #[tokio::test]
    async fn subscribe_structured_returns_no_agent_found_when_agent_is_missing() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::SubscribeStructured {
                agent_id,
                query: None,
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::SubscribeStructuredResult {
                subscription_id: id,
                seq: 0,
                structured_protocol: None,
                lease_ms: 0,
                error: Some(ProtocolError::NoAgentFound),
            } if id.is_nil()
        ));
    }

    #[tokio::test]
    async fn subscribe_structured_returns_unsupported_query_for_unknown_query() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::SubscribeStructured {
                agent_id,
                query: Some(SubscribeQuery::Unknown),
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::SubscribeStructuredResult {
                subscription_id: id,
                seq: 0,
                structured_protocol: None,
                lease_ms: 0,
                error: Some(ProtocolError::UnsupportedSubscribeQuery),
            } if id.is_nil()
        ));
    }

    #[tokio::test]
    async fn subscribe_structured_returns_no_agent_found_when_session_has_ended() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
        let info = session.to_agent(Uuid::new_v4());
        session
            .log_source()
            .expect("readonly session should expose structured logs")
            .close()
            .await;
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::SubscribeStructured {
                agent_id,
                query: None,
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::SubscribeStructuredResult {
                subscription_id: id,
                seq: 0,
                structured_protocol: None,
                lease_ms: 0,
                error: Some(ProtocolError::NoAgentFound),
            } if id.is_nil()
        ));
    }

    #[tokio::test]
    async fn subscribe_structured_returns_immediately_for_unlinked_claude_session() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let _route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::SubscribeStructured {
                agent_id,
                query: None,
            },
        );

        tokio::time::timeout(Duration::from_millis(100), handle_message(&tx, msg, &ctx))
            .await
            .unwrap()
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { payload, .. } = &msgs[0] else {
            panic!("expected Routable, got {:?}", msgs[0]);
        };
        let response = RoutableMessage::decode(payload).unwrap();
        let subscription_id = match response {
            RoutableMessage::SubscribeStructuredResult {
                subscription_id: id,
                seq: 0,
                structured_protocol: Some(protocol),
                lease_ms,
                error: None,
            } if !id.is_nil()
                && lease_ms == subscription_lease_ms()
                && protocol == "claude_pty_v1" =>
            {
                id
            }
            other => panic!("expected SubscribeStructuredResult, got {:?}", other),
        };
        drop(msgs);

        let us = user_state.read().await;
        let entry = us
            .active_subscriptions
            .get(&subscription_id)
            .expect("structured subscribe should register a subscription");
        assert_eq!(entry.agent_id, agent_id);
        assert_eq!(entry.mode, SubscriptionMode::Structured);
    }

    #[tokio::test]
    async fn raw_subscribe_returns_subscription_id_and_registers_entry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let _route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::SubscribeRaw {
                agent_id,
                terminal_size: None,
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let subscription_id = match decode_written_routable(&msgs[0]) {
            RoutableMessage::SubscribeRawResult {
                subscription_id,
                lease_ms,
                error: None,
            } => {
                assert_eq!(lease_ms, subscription_lease_ms());
                subscription_id
            }
            other => panic!("expected SubscribeRawResult, got {:?}", other),
        };
        drop(msgs);

        let us = user_state.read().await;
        let entry = us
            .active_subscriptions
            .get(&subscription_id)
            .expect("raw subscribe should register a subscription");
        assert_eq!(entry.agent_id, agent_id);
        assert_eq!(entry.mode, SubscriptionMode::Raw);
    }

    #[tokio::test]
    async fn extend_subscription_success_updates_deadline() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let old_deadline = Instant::now() + Duration::from_secs(1);
        let (cancel_tx, _cancel_rx) = oneshot::channel();

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                Route::from_link("test-link"),
                old_deadline,
            );
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::ExtendSubscription { subscription_id },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::ExtendSubscriptionResult {
                subscription_id: id,
                lease_ms,
                error: None,
            } if id == subscription_id && lease_ms == subscription_lease_ms()
        ));
        drop(msgs);

        let us = user_state.read().await;
        let entry = us.active_subscriptions.get(&subscription_id).unwrap();
        assert!(entry.lease_deadline > old_deadline);
    }

    #[tokio::test]
    async fn extend_subscription_unknown_returns_unknown_subscription() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let subscription_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::ExtendSubscription { subscription_id },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            decode_written_routable(&msgs[0]),
            RoutableMessage::ExtendSubscriptionResult {
                subscription_id: id,
                lease_ms: 0,
                error: Some(ProtocolError::UnknownSubscription),
            } if id == subscription_id
        ));
    }

    #[tokio::test]
    async fn unsubscribe_success_sends_unsubscribed_and_removes_subscription() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client");
        full_dst.push("test-link");

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::Unsubscribe { subscription_id },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        assert!(
            cancel_rx.try_recv().is_err(),
            "unsubscribe should drop the cancellation sender"
        );
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::Unsubscribed,
            } if id == subscription_id
        ));

        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
        drop(us);

        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "unsubscribe should not emit an ack");
    }

    #[tokio::test]
    async fn duplicate_unsubscribe_is_harmless() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client");
        full_dst.push("test-link");

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        let msg = Message::routable(
            Route::from_link("client"),
            Route::empty(),
            1,
            &RoutableMessage::Unsubscribe { subscription_id },
        );
        handle_message(&tx, msg.clone(), &ctx).await.unwrap();
        let _ = recv_routable(&mut route_rx).await;

        handle_message(&tx, msg, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        assert!(
            route_rx.try_recv().is_err(),
            "duplicate unsubscribe should not emit another close"
        );
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
        drop(us);

        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "unsubscribe remains fire-and-forget");
    }

    #[tokio::test]
    async fn remote_unsubscribe_forwarded_through_hop_removes_owner_subscription() {
        let (relay_state, relay_user_state) = test_state().await;
        let relay_ctx = test_ctx(relay_state, relay_user_state.clone());
        let (relay_tx, _relay_written) = mock_tx();

        let (peer_tx, mut peer_rx) = mpsc::channel::<Message>(16);
        {
            let mut us = relay_user_state.write().await;
            us.routes.insert(
                "peer-hop".to_string(),
                ConnectionHandle::new(peer_tx, Arc::new(AtomicU64::new(1))),
            );
        }

        let (owner_state, owner_user_state) = test_state().await;
        let mut owner_ctx = test_ctx(owner_state, owner_user_state.clone());
        owner_ctx.link_name = "peer-hop".to_string();
        let (owner_tx, _owner_written) = mock_tx();
        let _owner_route_rx = setup_named_route(&owner_user_state, "peer-hop").await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client-link");
        full_dst.push("peer-hop");
        {
            let mut us = owner_user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        let msg = Message::routable(
            Route::from_link("client-link"),
            Route::from_link("peer-hop"),
            1,
            &RoutableMessage::Unsubscribe { subscription_id },
        );
        handle_message(&relay_tx, msg, &relay_ctx).await.unwrap();

        let forwarded = peer_rx.try_recv().expect("unsubscribe should be forwarded");
        handle_message(&owner_tx, forwarded, &owner_ctx)
            .await
            .unwrap();
        tokio::task::yield_now().await;

        let us = owner_user_state.read().await;
        assert!(
            !us.active_subscriptions.contains_key(&subscription_id),
            "owner should remove forwarded remote subscription"
        );
    }
}
