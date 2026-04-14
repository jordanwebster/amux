//! Message dispatch handlers for the three protocol message categories.
//!
//! Each handler is independent: `handle_routable` processes hop-by-hop forwarded
//! messages and local delivery, `handle_command` processes CLI-only commands from
//! local connections, and `handle_direct` processes peer-to-peer discovery messages.

use super::accept::tcp_connect;
use super::connection::{
    ConnectionContext, cancel_subscriptions_matching, cleanup_subscription, extend_subscription,
    register_subscription, unsubscribe_subscription,
};
use super::routing::{
    announce_agent_message, broadcast_to_peers, connection_tx, create_agent, delete_local_agent,
    handle_subscribe, rename_local_agent, resume_agents, withdraw_agent,
};
use super::{
    SUBSCRIPTION_LEASE_DURATION, SubscriptionMode, send_routable_via_full_dst,
    subscription_lease_ms,
};
use crate::agent_registry::Agent;
use crate::agents::{AgentSession, ClaudeSession, StopPolicy};
use crate::buffer::{BroadcastReader, BufferPolicy};
use crate::claude::types::{ClaudeHook, Hook};
use crate::error::{AmuxError, Result};
use crate::message::{
    Command, DirectMessage, Message, ProtocolError, RoutableMessage, SubscribeQuery,
    SubscriptionCloseReason, SubscriptionId,
};
use crate::route::Route;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::Instrument;
use uuid::Uuid;

/// A readable subscription stream. Implemented for all [`BroadcastReader`]
/// instantiations to enable shared stream spawning logic.
trait SubscriptionReader: Send + 'static {
    type Item: Send;
    fn recv(&mut self) -> impl Future<Output = Option<Self::Item>> + Send;
}

impl<P: BufferPolicy> SubscriptionReader for BroadcastReader<P> {
    type Item = P::Item;
    fn recv(&mut self) -> impl Future<Output = Option<P::Item>> + Send {
        self.read()
    }
}

struct SubscriptionHandle {
    subscription_id: SubscriptionId,
    agent_id: Uuid,
    mode: SubscriptionMode,
    reply_src: Route,
    reply_dst: Route,
}

impl SubscriptionHandle {
    async fn register(
        ctx: &ConnectionContext,
        subscription_id: SubscriptionId,
        agent_id: Uuid,
        mode: SubscriptionMode,
        reply_src: Route,
        reply_dst: Route,
    ) -> (Self, oneshot::Receiver<()>) {
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let mut full_dst = reply_dst.clone();
        full_dst.push(ctx.link_name.clone());
        let mut us = ctx.user_state.write().await;
        register_subscription(
            &mut us,
            subscription_id,
            agent_id,
            mode,
            cancel_tx,
            full_dst,
            Instant::now() + SUBSCRIPTION_LEASE_DURATION,
        );
        (
            Self {
                subscription_id,
                agent_id,
                mode,
                reply_src,
                reply_dst,
            },
            cancel_rx,
        )
    }

    fn stream_span(&self) -> tracing::Span {
        tracing::info_span!(
            "stream",
            subscription_id = %self.subscription_id,
            agent_id = %self.agent_id,
            mode = self.mode.as_str()
        )
    }

    async fn send_source_closed(&self, tx: &mpsc::Sender<Message>, request_id: u64) {
        let _ = tx
            .send(Message::routable(
                self.reply_src.clone(),
                self.reply_dst.clone(),
                request_id,
                &RoutableMessage::SubscriptionClosed {
                    subscription_id: self.subscription_id,
                    reason: SubscriptionCloseReason::SourceClosed,
                },
            ))
            .await;
    }
}

/// Spawn a subscription stream task that reads from a buffer, wraps each item
/// into a RoutableMessage, and forwards it to the subscriber. Handles cancellation
/// and cleanup automatically.
async fn spawn_subscription_stream<R: SubscriptionReader>(
    mut reader: R,
    handle: SubscriptionHandle,
    cancel_rx: oneshot::Receiver<()>,
    wrap_item: fn(SubscriptionId, R::Item) -> RoutableMessage,
    ctx: &ConnectionContext,
) {
    let Some(tx) = connection_tx(&ctx.user_state, &ctx.link_name).await else {
        let _ = cleanup_subscription(&ctx.user_state, handle.subscription_id).await;
        return;
    };

    let stream_span = handle.stream_span();
    let stream_user_state = ctx.user_state.clone();
    let next_rid = ctx.next_request_id.clone();
    tokio::spawn(
        async move {
            let subscription_id = handle.subscription_id;
            tokio::select! {
                source_closed = async {
                    while let Some(item) = reader.recv().await {
                        let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                        if tx
                            .send(Message::routable(
                                handle.reply_src.clone(),
                                handle.reply_dst.clone(),
                                rid,
                                &wrap_item(subscription_id, item),
                            ))
                            .await
                            .is_err()
                        {
                            return false;
                        }
                    }
                    true
                } => {
                    if source_closed {
                        if cleanup_subscription(&stream_user_state, subscription_id)
                            .await
                            .is_some()
                        {
                            let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                            handle.send_source_closed(&tx, rid).await;
                        }
                    } else {
                        let _ = cleanup_subscription(&stream_user_state, subscription_id).await;
                    }
                }
                _ = cancel_rx => {
                    tracing::debug!("stream cancelled");
                    let _ = cleanup_subscription(&stream_user_state, subscription_id).await;
                }
            }
            tracing::debug!("stream ended");
        }
        .instrument(stream_span),
    );
}

fn subscribe_error(err: &AmuxError) -> ProtocolError {
    match err {
        AmuxError::AgentNotFound(_) => ProtocolError::NoAgentFound,
        _ => ProtocolError::ServerError {
            message: err.to_string(),
        },
    }
}

fn descendant_host_ids(
    us: &super::ServerUserState,
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
    us: &mut super::ServerUserState,
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
    us: &mut super::ServerUserState,
    root_host_id: uuid::Uuid,
    route_prefix: &Route,
) -> usize {
    descendant_host_ids(us, root_host_id, route_prefix)
        .into_iter()
        .filter(|id| us.hosts.remove(id).is_some())
        .count()
}

pub(super) async fn handle_message(
    tx: &mpsc::Sender<Message>,
    msg: Message,
    ctx: &ConnectionContext,
) -> Result<()> {
    match &msg {
        Message::Routable { .. } => tracing::trace!("received routable"),
        _ => tracing::debug!(msg_type = msg.type_label(), "received message"),
    }

    match msg {
        Message::Routable {
            src,
            dst,
            request_id,
            payload,
        } => handle_routable(tx, src, dst, request_id, payload, ctx).await,
        Message::Direct { message: direct } => handle_direct(tx, direct, ctx).await,
        Message::Command { command: cmd } => {
            if !ctx.is_local {
                tracing::warn!(cmd = cmd.type_label(), "rejecting command from remote peer");
                return Ok(());
            }
            handle_command(tx, cmd, ctx).await
        }
        Message::Unknown => {
            tracing::warn!("dropping unknown top-level message");
            Ok(())
        }
    }
}

fn reply_routes(src: Route, msg_type: &str) -> Option<(Route, Route)> {
    match Route::reply(src) {
        Some(routes) => Some(routes),
        None => {
            tracing::warn!(msg_type, "dropping routable message with empty src route");
            None
        }
    }
}

async fn handle_routable(
    tx: &mpsc::Sender<Message>,
    mut src: Route,
    mut dst: Route,
    request_id: u64,
    payload: Vec<u8>,
    ctx: &ConnectionContext,
) -> Result<()> {
    // Check if this message needs forwarding — payload forwarded verbatim
    if let Some(next_hop) = dst.pop() {
        let route_tx = {
            let us = ctx.user_state.read().await;
            us.routes.get(&next_hop).cloned()
        };

        match route_tx {
            Some(route_tx) => {
                // src is consumed by the forwarded message. If the channel is
                // closed (peer disconnected between lookup and send), we can't
                // send Unreachable — but that's fine: a successful send() only
                // means the message landed in the channel buffer, not that the
                // peer processed it. Reliable delivery requires application-level
                // acks (*Result messages), so the sender must use timeouts for
                // in-flight losses regardless.
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
                        drop(unsubscribe_subscription(&ctx.user_state, subscription_id).await);
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
                drop(unsubscribe_subscription(&ctx.user_state, subscription_id).await);
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
                let super::SubscriptionEntry {
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
            let result = if is_cloud_server {
                Err(AmuxError::ServerError(
                    "cloud relays do not host local agents".to_string(),
                ))
            } else {
                create_agent(&ctx.user_state, &ctx.event_tx, req, ctx.user_id, host_id).await
            };
            let response_message = match result {
                Ok(()) => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: None,
                },
                Err(e) => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: Some(ProtocolError::ServerError {
                        message: e.to_string(),
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
                delete_local_agent(&mut us, agent_id).ok()
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
                && let Some(pty) = session.get_pty_handle()
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

/// Handle CLI-only commands (only accepted from local connections).
async fn handle_command(
    tx: &mpsc::Sender<Message>,
    command: Command,
    ctx: &ConnectionContext,
) -> Result<()> {
    match command {
        Command::Shutdown => {
            tracing::info!("shutdown requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(super::ShutdownRequest::Shutdown {
                    reply: tx.clone(),
                    link_name: ctx.link_name.clone(),
                })
                .await;
            Ok(())
        }

        Command::ConnectToServer { address } => {
            // block_in_place + block_on breaks the async type recursion cycle:
            // handle_message -> tcp_connect -> connection_loop -> handle_message
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(tcp_connect(
                    &address,
                    &ctx.state,
                    &ctx.user_state,
                    ctx.user_id,
                    ctx.event_tx.clone(),
                ))
            });
            let response = match result {
                Ok(()) => Message::Command {
                    command: Command::ConnectToServerResult { error: None },
                },
                Err(e) => Message::Command {
                    command: Command::ConnectToServerResult {
                        error: Some(ProtocolError::ServerError {
                            message: e.to_string(),
                        }),
                    },
                },
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::Debug { verbose, format } => {
            let dump = crate::debug::dump_server_debug_info(&ctx.state, format, verbose).await;
            let _ = tx
                .send(Message::Command {
                    command: Command::DebugResult { dump },
                })
                .await;
            Ok(())
        }

        Command::ListAgents => {
            let agents = {
                let us = ctx.user_state.read().await;
                us.registry.list_all(&us.hosts)
            };
            let _ = tx
                .send(Message::Command {
                    command: Command::ListAgentsResult {
                        agents: agents.into_iter().collect(),
                    },
                })
                .await;
            Ok(())
        }

        Command::HandleHook { agent_id, hook } => {
            let hook_type = match hook.as_ref() {
                Hook::Claude(ClaudeHook::SessionStart(_), _) => "SessionStart",
                Hook::Claude(ClaudeHook::PermissionRequest(_), _) => "PermissionRequest",
                Hook::Claude(ClaudeHook::Stop(_), _) => "Stop",
                Hook::Claude(ClaudeHook::SessionEnd(_), _) => "SessionEnd",
                Hook::Claude(ClaudeHook::Notification(_), _) => "Notification",
                Hook::Claude(ClaudeHook::Unknown, _) => "Unknown",
            };
            tracing::debug!(hook_type, %agent_id, "received hook event");

            // Unknown hook variants should be filtered client-side; warn and ack
            if matches!(hook.as_ref(), Hook::Claude(ClaudeHook::Unknown, _)) {
                tracing::warn!(%agent_id, "received unknown hook variant");
                let _ = tx
                    .send(Message::Command {
                        command: Command::HandleHookResult { error: None },
                    })
                    .await;
                return Ok(());
            }

            let mut session_to_stop = None;
            let result = {
                let mut us = ctx.user_state.write().await;
                let is_session_end =
                    matches!(hook.as_ref(), Hook::Claude(ClaudeHook::SessionEnd(_), _));
                if let Some(session) = us.agents.get_mut(&agent_id) {
                    let is_readonly = session.readonly();
                    let r =
                        session
                            .handle_hook(*hook)
                            .await
                            .map_err(|e| ProtocolError::ServerError {
                                message: format!("hook handling failed: {e}"),
                            });
                    if r.is_ok() && is_session_end && is_readonly {
                        session_to_stop = withdraw_agent(&mut us, agent_id);
                    }
                    r
                } else if is_session_end {
                    tracing::debug!(%agent_id, "ignoring SessionEnd for unknown session");
                    Ok(())
                } else {
                    // External session — create readonly agent from hook data
                    let Hook::Claude(claude_hook, _) = hook.as_ref();
                    if let Some(cwd) = claude_hook.cwd()
                        && let Some(_transcript_path) = claude_hook.transcript_path()
                    {
                        let wd = PathBuf::from(cwd);
                        let mut session =
                            AgentSession::Claude(ClaudeSession::new_readonly(agent_id, wd.clone()));
                        if let Err(e) = session.handle_hook(*hook).await {
                            Err(ProtocolError::ServerError {
                                message: format!("hook handling failed: {e}"),
                            })
                        } else {
                            let host_id = {
                                let state = ctx.state.read().await;
                                state.host_id
                            };
                            let info = session.to_agent(host_id);
                            let announce = announce_agent_message(&info);
                            us.agents.insert(agent_id, session);
                            if let Err(e) = us.registry.register_local(info) {
                                Err(ProtocolError::ServerError {
                                    message: format!(
                                        "failed to register readonly agent {agent_id}: {e}"
                                    ),
                                })
                            } else {
                                if let Some(session) = us.agents.get_mut(&agent_id) {
                                    session.maybe_start_name_sniffer(ctx.user_id, &ctx.event_tx);
                                }
                                broadcast_to_peers(&mut us, &announce, None);
                                tracing::info!(%agent_id, "created readonly session from external hook");
                                Ok(())
                            }
                        }
                    } else {
                        tracing::warn!(%agent_id, "no agent found for hook");
                        Err(ProtocolError::ServerError {
                            message: format!("No agent found with agent_id: {agent_id}"),
                        })
                    }
                }
            };

            if let Some(session) = session_to_stop {
                session.stop(StopPolicy::Interrupt).await;
            }

            let response = match result {
                Ok(()) => Message::Command {
                    command: Command::HandleHookResult { error: None },
                },
                Err(e) => Message::Command {
                    command: Command::HandleHookResult { error: Some(e) },
                },
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::ResolveAgent { identifier } => {
            let us = ctx.user_state.read().await;
            let agent = us.registry.resolve(&us.hosts, &identifier);
            let _ = tx
                .send(Message::Command {
                    command: Command::ResolveAgentResult { agent },
                })
                .await;
            Ok(())
        }

        Command::Suspend => {
            tracing::info!("suspend requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(super::ShutdownRequest::Suspend {
                    reply: tx.clone(),
                    link_name: ctx.link_name.clone(),
                })
                .await;
            Ok(())
        }

        Command::Resume => {
            tracing::info!("resume requested");
            let (state_path, host_id, is_cloud_server) = {
                let state = ctx.state.read().await;
                (
                    state.config.state_path.clone(),
                    state.host_id,
                    state.is_cloud_server,
                )
            };
            if is_cloud_server {
                let _ = tx
                    .send(Message::Command {
                        command: Command::ResumeResult {
                            resumed_count: 0,
                            failed_count: 0,
                            error: Some(ProtocolError::ServerError {
                                message: "cloud relays do not host local agents".to_string(),
                            }),
                        },
                    })
                    .await;
                return Ok(());
            }
            let suspended = match crate::state::load_and_remove_suspended(&state_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "failed to load suspended agents");
                    let _ = tx
                        .send(Message::Command {
                            command: Command::ResumeResult {
                                resumed_count: 0,
                                failed_count: 0,
                                error: Some(ProtocolError::ServerError {
                                    message: format!("failed to load state: {e}"),
                                }),
                            },
                        })
                        .await;
                    return Ok(());
                }
            };
            let (resumed_count, failed_count) = resume_agents(
                &ctx.user_state,
                &ctx.event_tx,
                ctx.user_id,
                suspended.agents,
                host_id,
            )
            .await;
            let _ = tx
                .send(Message::Command {
                    command: Command::ResumeResult {
                        resumed_count: resumed_count as u64,
                        failed_count: failed_count as u64,
                        error: None,
                    },
                })
                .await;
            Ok(())
        }

        // Response variants — should not arrive at the server
        Command::ListAgentsResult { .. }
        | Command::ResolveAgentResult { .. }
        | Command::ShutdownNotification { .. }
        | Command::DebugResult { .. }
        | Command::ConnectToServerResult { .. }
        | Command::HandleHookResult { .. }
        | Command::SuspendResult { .. }
        | Command::ResumeResult { .. } => {
            tracing::warn!("unexpected command response variant");
            Ok(())
        }
        Command::Unknown => {
            tracing::warn!("dropping unknown command");
            Ok(())
        }
    }
}

async fn handle_direct(
    tx: &mpsc::Sender<Message>,
    message: DirectMessage,
    ctx: &ConnectionContext,
) -> Result<()> {
    match message {
        // In-band re-authentication for token refresh on established connections.
        DirectMessage::Reauth { token } => {
            let (is_cloud, min_version) = {
                let state = ctx.state.read().await;
                (
                    state.is_cloud_server,
                    state.config.minimum_client_version.clone(),
                )
            };

            // Re-check minimum client version (config may have changed since connect)
            if let Some(ref min_ver_str) = min_version {
                let reject = match (
                    semver::Version::parse(&ctx.client_version),
                    semver::Version::parse(min_ver_str),
                ) {
                    (Ok(client), Ok(minimum)) => client < minimum,
                    _ => true,
                };
                if reject {
                    tracing::warn!(
                        client_version = %ctx.client_version,
                        minimum_version = %min_ver_str,
                        "re-auth: client version below minimum"
                    );
                    let _ = tx
                        .send(Message::Direct {
                            message: DirectMessage::ReauthResult {
                                error: Some(ProtocolError::UpgradeRequired {
                                    minimum_version: min_ver_str.clone(),
                                    client_version: ctx.client_version.clone(),
                                }),
                            },
                        })
                        .await;
                    return Err(AmuxError::UpgradeRequired {
                        minimum_version: min_ver_str.clone(),
                        client_version: ctx.client_version.clone(),
                    });
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
                            AmuxError::InvalidCredentials
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
                            return Err(AmuxError::InvalidCredentials);
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

            let info = Agent {
                id: agent_id,
                host_id,
                name: name.clone(),
                command: command.clone(),
                working_dir: working_dir.clone(),
                route: Route::empty(),
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

            let info = crate::message::Host {
                id,
                name: name.clone(),
                route: our_route.clone(),
                version: version.clone(),
            };

            us.hosts.insert(id, info);

            // Keep descendant host routes aligned with the selected route for this host.
            // This is local normalization, not a new topology fact. For example, if we
            // previously knew `H` via `old-link` and a child host `C` via
            // `old-link.child`, then learning a new route `H = test-link` does not
            // prove `old-link.child` stopped working. Direct disconnects and explicit
            // withdrawals already clean up dead paths.
            //
            // We still rewrite the subtree locally so the in-memory topology is easier
            // to reason about: once we pick `H = test-link`, its descendants read as
            // `test-link.child`, `test-link.child.grand`, etc. We do not rebroadcast
            // those descendant rewrites; the parent `AnnounceHost` already follows the
            // topology, and each receiving hop can apply the same local normalization.
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

            let super::ServerUserState {
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
                AmuxError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "outgoing channel closed while sending heartbeat ack",
                ))
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
    use super::*;
    use crate::agents::{AgentSession, LocalAgentNameSource, SessionEvent};
    use crate::claude::types::{
        BashToolInput, ClaudePermissionRequest, ClaudePermissionTool, ClaudeSessionEnd,
        ClaudeSessionStart, ClaudeStop,
    };
    use crate::message::{
        AgentType, Command, CreateAgentRequest, DirectMessage, RenameAgentRequest, SubscribeQuery,
    };
    use crate::route::Route;
    use crate::server::test_helpers::{test_ctx, test_state};
    use crate::server::{
        ConnectionHandle, LOCAL_USER_ID, SUBSCRIPTION_LEASE_DURATION, ServerUserState,
        SubscriptionMode, sweep_expired_subscriptions,
    };
    use chrono::Utc;
    use serde::Serialize;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use tokio::sync::{RwLock, mpsc, oneshot};
    use tokio::time::Instant;
    use uuid::Uuid;

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

    fn claude_agent_type() -> String {
        "claude".to_string()
    }

    fn claude_structured_protocol() -> Option<String> {
        Some("claude_pty_v1".to_string())
    }

    /// Create an AgentSession::TestAgent from a CreateAgentRequest.
    fn create_test_session(req: &crate::message::CreateAgentRequest) -> AgentSession {
        let cmd = match &req.agent_type {
            crate::message::AgentType::TestAgent { command: cmd } => cmd.clone(),
            _ => panic!("expected TestAgent"),
        };
        let mut inner = crate::agents::TestAgentSession::new(req, cmd);
        inner.start().unwrap();
        AgentSession::TestAgent(inner)
    }

    /// Create a response channel and collect written messages
    fn mock_tx() -> (mpsc::Sender<Message>, Arc<tokio::sync::Mutex<Vec<Message>>>) {
        let (tx, mut rx) = mpsc::channel::<Message>(16);
        let written = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let written_clone = written.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                written_clone.lock().await.push(msg);
            }
        });
        (tx, written)
    }

    async fn add_peer_link(
        user_state: &Arc<RwLock<ServerUserState>>,
        link_name: &str,
    ) -> mpsc::Receiver<Message> {
        let (tx, rx) = mpsc::channel::<Message>(16);
        let mut us = user_state.write().await;
        us.routes.insert(
            link_name.to_string(),
            ConnectionHandle::new(tx, Arc::new(AtomicU64::new(1))),
        );
        us.peer_links.insert(link_name.to_string());
        rx
    }

    async fn insert_remote_agent(user_state: &Arc<RwLock<ServerUserState>>, info: Agent) {
        let mut us = user_state.write().await;
        us.hosts
            .entry(info.host_id)
            .or_insert_with(|| crate::message::Host {
                id: info.host_id,
                name: format!("host-{}", info.host_id),
                route: info.route.clone(),
                version: "0.1.0".to_string(),
            });
        us.registry.register_remote(info).unwrap();
    }

    async fn insert_local_claude(
        user_state: &Arc<RwLock<ServerUserState>>,
        agent_id: Uuid,
        name: Option<&str>,
        source: LocalAgentNameSource,
    ) {
        let req = CreateAgentRequest {
            agent_id,
            name: name.map(str::to_owned),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![],
        };
        let mut session = AgentSession::Claude(crate::agents::ClaudeSession::new(&req));
        session.set_local_name(name.map(str::to_owned), source);
        let info = session.to_agent(Uuid::new_v4());

        let mut us = user_state.write().await;
        us.agents.insert(agent_id, session);
        us.registry.register_local(info).unwrap();
    }

    fn decode_written_routable(msg: &Message) -> RoutableMessage {
        let Message::Routable { payload, .. } = msg else {
            panic!("expected Routable, got {:?}", msg);
        };
        RoutableMessage::decode(payload).unwrap()
    }

    fn drain_direct_messages(rx: &mut mpsc::Receiver<Message>) -> Vec<DirectMessage> {
        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            let Message::Direct { message: msg } = msg else {
                panic!("expected Direct message, got {:?}", msg);
            };
            messages.push(msg);
        }
        messages
    }

    #[tokio::test]
    async fn reauth_succeeds_in_non_cloud_mode() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = DirectMessage::Reauth {
            token: "test-token".to_string(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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

        let msg = DirectMessage::ReauthResult { error: None };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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

    async fn populate_debug_state(
        state: &Arc<RwLock<crate::server::ServerState>>,
        user_state: &Arc<RwLock<ServerUserState>>,
    ) {
        let _term_rx = setup_named_route(user_state, "term-debug").await;
        let _peer_rx = add_peer_link(user_state, "peer-debug").await;

        let local_host_id = state.read().await.host_id;
        let local_agent_id = Uuid::new_v4();
        let mut local_session = AgentSession::Claude(ClaudeSession::new_readonly(
            local_agent_id,
            PathBuf::from("/tmp/local-agent"),
        ));
        local_session.set_local_name(Some("local-agent".to_string()), LocalAgentNameSource::Amux);
        let local_info = local_session.to_agent(local_host_id);

        let remote_host_id = Uuid::new_v4();
        let remote_agent_id = Uuid::new_v4();
        let mut us = user_state.write().await;
        us.hosts.insert(
            remote_host_id,
            crate::message::Host {
                id: remote_host_id,
                name: "remote-host".to_string(),
                route: Route::from_link("peer-debug"),
                version: "9.9.9".to_string(),
            },
        );
        us.agents.insert(local_agent_id, local_session);
        us.registry.register_local(local_info).unwrap();
        us.registry
            .register_remote(Agent {
                id: remote_agent_id,
                host_id: remote_host_id,
                name: Some("remote-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp/remote-agent"),
                route: Route::from_link("peer-debug"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--model".to_string(), "sonnet".to_string()],
                created_at: Utc::now(),
            })
            .unwrap();

        let (raw_cancel_tx, _raw_cancel_rx) = oneshot::channel();
        register_subscription(
            &mut us,
            Uuid::new_v4(),
            local_agent_id,
            SubscriptionMode::Raw,
            raw_cancel_tx,
            Route::from_link("term-debug"),
            Instant::now() + SUBSCRIPTION_LEASE_DURATION,
        );
    }

    #[tokio::test]
    async fn debug_yaml_dump_is_non_empty_and_parses() {
        let (state, user_state) = test_state().await;
        populate_debug_state(&state, &user_state).await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::Debug {
                verbose: true,
                format: crate::message::DebugFormat::Yaml,
            },
            &ctx,
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        let Some(Message::Command {
            command: Command::DebugResult { dump },
        }) = msgs.first()
        else {
            panic!("expected DebugResult, got {:?}", msgs.first());
        };
        assert!(!dump.is_empty(), "yaml dump should be non-empty");
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(dump).expect("dump should be valid yaml");
        assert!(parsed.get("user_count").is_some());
        assert!(parsed.get("local_host").is_some());
    }

    #[tokio::test]
    async fn debug_json_dump_is_non_empty_and_parses() {
        let (state, user_state) = test_state().await;
        populate_debug_state(&state, &user_state).await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::Debug {
                verbose: true,
                format: crate::message::DebugFormat::Json,
            },
            &ctx,
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        let Some(Message::Command {
            command: Command::DebugResult { dump },
        }) = msgs.first()
        else {
            panic!("expected DebugResult, got {:?}", msgs.first());
        };
        assert!(!dump.is_empty(), "json dump should be non-empty");
        let parsed: serde_json::Value =
            serde_json::from_str(dump).expect("dump should be valid json");
        assert!(parsed.get("user_count").is_some());
        assert!(parsed.get("local_host").is_some());
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
                crate::message::Host {
                    id: remote_host_id,
                    name: "remote-host".to_string(),
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
        }
        let msg = DirectMessage::AnnounceAgent {
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
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
                    id: host_id,
                    name: "remote-host".to_string(),
                    route,
                    version: "0.1.0".to_string(),
                },
            );
        }
        let msg = DirectMessage::AnnounceAgent {
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
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
                    id: host_id,
                    name: "remote-host".to_string(),
                    route: Route::from_link("other-link"),
                    version: "0.1.0".to_string(),
                },
            );
        }

        let msg = DirectMessage::AnnounceAgent {
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
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
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

        let msg = DirectMessage::AnnounceAgent {
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
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("selected-name"));
        assert_eq!(entry.command, "claude");
        assert!(entry.args.is_empty());
    }

    #[tokio::test]
    async fn announce_agent_skips_local_agent() {
        let (state, user_state) = test_state().await;

        // Insert a local agent and register in registry
        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            let req = crate::message::CreateAgentRequest {
                agent_id,
                name: Some("local".to_string()),
                agent_type: crate::message::AgentType::TestAgent {
                    command: dummy_pty_command(),
                },
                working_dir: dummy_working_dir(),
                terminal_size: Some(crate::message::TerminalSize { rows: 24, cols: 80 }),
                args: vec![],
            };
            let session = create_test_session(&req);
            let info = session.to_agent(Uuid::new_v4());
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        // Try to announce same agent_id from remote
        let msg = DirectMessage::AnnounceAgent {
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
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        // Should still be local (not overwritten)
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

        let msg = DirectMessage::WithdrawAgent { agent_id };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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

        // Withdraw from "test-link" but agent is stored from "other-link"
        let msg = DirectMessage::WithdrawAgent { agent_id };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        // Should still be there (link mismatch)
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
                crate::message::Host {
                    id: first_host_id,
                    name: "first-host".to_string(),
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                second_host_id,
                crate::message::Host {
                    id: second_host_id,
                    name: "second-host".to_string(),
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
        }

        // First announce
        let msg = DirectMessage::AnnounceAgent {
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
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        // Second announce with same agent_id
        let msg = DirectMessage::AnnounceAgent {
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
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
        let session = AgentSession::Claude(crate::agents::ClaudeSession::new_readonly(
            agent_id,
            PathBuf::from("/tmp"),
        ));
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
    async fn resolve_agent_by_name() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: Uuid::new_v4(),
                name: Some("my-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link("peer-a"),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let cmd = Command::ResolveAgent {
            identifier: "my-agent".to_string(),
        };
        handle_command(&tx, cmd, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::ResolveAgentResult { agent },
        } = &msgs[0]
        else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_some());
        assert_eq!(agent.as_ref().unwrap().id, agent_id);
    }

    #[tokio::test]
    async fn resolve_agent_not_found() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let cmd = Command::ResolveAgent {
            identifier: "nonexistent".to_string(),
        };
        handle_command(&tx, cmd, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::ResolveAgentResult { agent },
        } = &msgs[0]
        else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_none());
    }

    #[tokio::test]
    async fn announce_host_stores_in_hosts() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let host_id = Uuid::new_v4();
        let msg = DirectMessage::AnnounceHost {
            id: host_id,
            name: "remote-laptop".to_string(),
            route: Route::empty(),
            version: "0.1.0".to_string(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(us.hosts.contains_key(&host_id));
        let info = &us.hosts[&host_id];
        assert_eq!(info.name, "remote-laptop");
        assert_eq!(info.version, "0.1.0");
        // Route should have ctx.link_name prepended
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
        let msg = DirectMessage::AnnounceHost {
            id: host_id,
            name: "far-server".to_string(),
            route: Route::from_link("peer-a"),
            version: "0.2.0".to_string(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let info = &us.hosts[&host_id];
        let mut route = info.route.clone();
        // Should be test-link.peer-a (test-link prepended)
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
                crate::message::Host {
                    id: host_id,
                    name: "remote-parent".to_string(),
                    route: Route::from_link("old-link"),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                child_host_id,
                crate::message::Host {
                    id: child_host_id,
                    name: "remote-child".to_string(),
                    route: child_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                grandchild_host_id,
                crate::message::Host {
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
                route: child_route,
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let msg = DirectMessage::AnnounceHost {
            id: host_id,
            name: "remote-parent".to_string(),
            route: Route::empty(),
            version: "0.2.0".to_string(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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
        let expected_hosts = vec![(host_id, "test-link".to_string())];

        assert_eq!(announced_hosts, expected_hosts);
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

        let msg = DirectMessage::AnnounceHost {
            id: host_id,
            name: "myself".to_string(),
            route: Route::from_link("cloud"),
            version: "0.1.0".to_string(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let msg = DirectMessage::WithdrawHost {
            id: host_id,
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
                    id: host_id,
                    name: "mobile".to_string(),
                    route: full_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            register_subscription(
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

        let msg = DirectMessage::WithdrawHost {
            id: host_id,
            route: Route::from_link("child"),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link("other-link"),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                child_host_id,
                crate::message::Host {
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

        let msg = DirectMessage::WithdrawHost {
            id: host_id,
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
                crate::message::Host {
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

        let msg = DirectMessage::WithdrawHost {
            id: host_id,
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
    async fn command_from_remote_peer_is_rejected() {
        let (state, user_state) = test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "remote-peer".to_string(),
            is_local: false,
            heartbeat_role: crate::server::connection::HeartbeatRole::Acceptor,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let (tx, written) = mock_tx();

        // Remote peer sends Shutdown — should be silently rejected
        let msg = Message::Command {
            command: Command::Shutdown,
        };
        handle_message(&tx, msg, &ctx).await.unwrap();

        // Remote peer sends ListAgents — should also be rejected
        let msg = Message::Command {
            command: Command::ListAgents,
        };
        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "remote peer should receive no response to commands"
        );
    }

    #[tokio::test]
    async fn forwarding_to_nonexistent_route_sends_unreachable() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("nonexistent-hop"),
            42,
            &RoutableMessage::RawInput {
                agent_id,
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

        // Set up a route for "dead-peer" then immediately drop the receiver
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

        // src was consumed by the send attempt, so no Unreachable can be sent
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

        // Send a routable message with garbage payload (empty dst = local delivery)
        let msg = Message::Routable {
            src: Route::from_link("sender"),
            dst: Route::empty(),
            request_id: 42,
            payload: vec![0xFF, 0xFE, 0xFD], // garbage, not valid msgpack
        };

        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        // Should be a routable reply containing InvalidMessage
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
            payload: vec![0xFF], // invalid msgpack
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

        // Set up a route for "peer-a" in the routing table
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

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("peer-a"),
            1,
            &RoutableMessage::RawInput {
                agent_id,
                data: vec![0x41],
            },
        );

        handle_message(&tx, msg, &ctx).await.unwrap();

        // Message should arrive at peer-a's channel
        let forwarded = peer_rx
            .try_recv()
            .expect("message should be forwarded to peer-a");
        let Message::Routable {
            src, dst, payload, ..
        } = forwarded
        else {
            panic!("expected Routable, got {:?}", forwarded);
        };

        // dst should be empty (peer-a was popped)
        assert!(dst.is_empty());
        // src should have peer-a pushed (building return path)
        let mut src = src;
        assert_eq!(src.pop(), Some("peer-a".to_string()));
        assert_eq!(src.pop(), Some("sender".to_string()));

        // Payload should be forwarded verbatim (opaque routing)
        let inner = RoutableMessage::decode(&payload).unwrap();
        assert!(matches!(inner, RoutableMessage::RawInput { .. }));
    }

    #[tokio::test]
    async fn response_variants_are_noops_at_destination() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        // Response messages arriving at their destination (empty dst) should be silently ignored
        let responses: Vec<RoutableMessage> = vec![
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
    async fn resume_is_rejected_on_cloud_relay() {
        let (state, user_state) = test_state().await;
        {
            let mut s = state.write().await;
            s.is_cloud_server = true;
        }
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(&tx, Command::Resume, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command:
                Command::ResumeResult {
                    resumed_count,
                    failed_count,
                    error,
                },
        } = &msgs[0]
        else {
            panic!("expected ResumeResult, got {:?}", msgs[0]);
        };
        assert_eq!(*resumed_count, 0);
        assert_eq!(*failed_count, 0);
        assert!(matches!(
            error,
            Some(ProtocolError::ServerError { message: msg })
                if msg.contains("cloud relays do not host local agents")
        ));
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
        let session = AgentSession::Claude(crate::agents::ClaudeSession::new_readonly(
            agent_id,
            PathBuf::from("/tmp"),
        ));
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
        let session = AgentSession::Claude(crate::agents::ClaudeSession::new_readonly(
            agent_id,
            PathBuf::from("/tmp"),
        ));
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
                crate::message::Host {
                    id: host_id,
                    name: "remote".to_string(),
                    route: Route::from_link("test-link"),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                deep_host_id,
                crate::message::Host {
                    id: deep_host_id,
                    name: "deep-remote".to_string(),
                    route: deep_route.clone(),
                    version: "0.1.0".to_string(),
                },
            );
            us.hosts.insert(
                other_host_id,
                crate::message::Host {
                    id: other_host_id,
                    name: "other-remote".to_string(),
                    route: Route::from_link("other-link"),
                    version: "0.1.0".to_string(),
                },
            );
        }

        // Agents reachable via test-link (should be removed)
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

        let msg = DirectMessage::WithdrawHost {
            id: host_id,
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

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
        let expected_withdrawals = vec![(host_id, "test-link".to_string())];

        assert_eq!(withdrawn_hosts, expected_withdrawals);
    }

    /// Insert a local test session into user_state.agents.
    async fn insert_test_session(user_state: &Arc<RwLock<ServerUserState>>, agent_id: Uuid) {
        let mut us = user_state.write().await;
        let req = crate::message::CreateAgentRequest {
            agent_id,
            name: Some("hook-test".to_string()),
            agent_type: crate::message::AgentType::TestAgent {
                command: dummy_pty_command(),
            },
            working_dir: dummy_working_dir(),
            terminal_size: Some(crate::message::TerminalSize { rows: 24, cols: 80 }),
            args: vec![],
        };
        let session = create_test_session(&req);
        us.agents.insert(agent_id, session);
    }

    #[tokio::test]
    async fn handle_hook_session_start_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::from_claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        // Verify readonly session was created
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_session_end_no_session_is_ignored() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::from_claude(ClaudeHook::SessionEnd(ClaudeSessionEnd {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "SessionEnd for an unknown session should be ignored: {:?}",
            error
        );
        drop(msgs);

        let us = user_state.read().await;
        assert!(
            !us.agents.contains_key(&agent_id),
            "unknown SessionEnd should not create a readonly session"
        );
    }

    #[tokio::test]
    async fn handle_hook_session_start_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = Hook::from_claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/nonexistent_transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_permission_request_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::from_claude(ClaudeHook::PermissionRequest(Box::new(
            ClaudePermissionRequest {
                session_id: Uuid::new_v4(),
                transcript_path: "/tmp".to_string(),
                cwd: "/tmp".to_string(),
                tool: ClaudePermissionTool::Bash {
                    tool_input: BashToolInput {
                        command: "ls".to_string(),
                        description: None,
                        timeout: None,
                        run_in_background: None,
                        dangerously_disable_sandbox: None,
                    },
                },
            },
        )));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        // Verify readonly session was created
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_permission_request_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let tool = ClaudePermissionTool::Bash {
            tool_input: BashToolInput {
                command: "cargo test".to_string(),
                description: Some("Run tests".to_string()),
                timeout: None,
                run_in_background: None,
                dangerously_disable_sandbox: None,
            },
        };
        let hook = Hook::from_claude(ClaudeHook::PermissionRequest(Box::new(
            ClaudePermissionRequest {
                session_id: Uuid::new_v4(),
                transcript_path: "/tmp".to_string(),
                cwd: "/tmp".to_string(),
                tool: tool.clone(),
            },
        )));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_stop_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::from_claude(ClaudeHook::Stop(ClaudeStop {
            session_id: Uuid::new_v4(),
            stop_hook_active: true,
            last_assistant_message: "Done.".to_string(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_stop_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = Hook::from_claude(ClaudeHook::Stop(ClaudeStop {
            session_id: Uuid::new_v4(),
            stop_hook_active: true,
            last_assistant_message: "I've completed the refactoring.".to_string(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_session_end_with_readonly_session_withdraws_agent() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let session = AgentSession::Claude(crate::agents::ClaudeSession::new_readonly(
            agent_id,
            PathBuf::from("/tmp"),
        ));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let hook = Hook::from_claude(ClaudeHook::SessionEnd(ClaudeSessionEnd {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "SessionEnd for a readonly session should succeed: {:?}",
            error
        );
        drop(msgs);

        let us = user_state.read().await;
        assert!(
            !us.agents.contains_key(&agent_id),
            "readonly session should be withdrawn on SessionEnd"
        );
        assert!(
            us.registry.get(&agent_id).is_none(),
            "readonly session should be removed from registry on SessionEnd"
        );
    }

    #[tokio::test]
    async fn readonly_external_claude_session_gets_name_updates() {
        let (state, user_state) = test_state().await;
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state: state.clone(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "test-link".to_string(),
            is_local: true,
            heartbeat_role: crate::server::connection::HeartbeatRole::Disabled,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        let hook = Hook::from_claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));

        handle_command(
            &tx,
            Command::HandleHook {
                agent_id,
                hook: Box::new(hook),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let log_source = {
            let us = user_state.read().await;
            us.agents
                .get(&agent_id)
                .and_then(|session| session.log_source())
                .expect("readonly session should have a log source")
        };

        log_source
            .write(json!({
                "type": "user",
                "message": {"content": "hello"},
                "uuid": "u1",
                "timestamp": "2026-04-03T10:00:00Z",
                "slug": "readonly-slug"
            }))
            .await;

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            SessionEvent::NameCandidateChanged {
                agent_id: id,
                ref name,
                source: crate::agents::LocalAgentNameSource::ProviderSlug,
                ..
            } if id == agent_id && name == "readonly-slug"
        ));

        crate::server::handle_session_event(&state, event).await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(error.is_none());
        drop(msgs);

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("readonly-slug"));
        assert_eq!(
            us.agents.get(&agent_id).and_then(|session| session.name()),
            Some("readonly-slug")
        );
        drop(us);

        let initial = peer_rx
            .try_recv()
            .expect("readonly creation should announce unnamed agent");
        assert!(matches!(
            initial,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: None,
                    readonly: true,
                    ..
                }
            } if id == agent_id
        ));

        let renamed = peer_rx
            .try_recv()
            .expect("readonly rename should be re-announced");
        assert!(matches!(
            renamed,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    readonly: true,
                    ..
                }
            } if id == agent_id && name == "readonly-slug"
        ));
    }

    // --- Subscription stream lifecycle tests ---

    /// Mock subscription reader backed by a channel for controlled test sequencing.
    struct MockReader {
        rx: mpsc::Receiver<Vec<u8>>,
    }

    impl SubscriptionReader for MockReader {
        type Item = Vec<u8>;
        fn recv(&mut self) -> impl Future<Output = Option<Vec<u8>>> + Send {
            self.rx.recv()
        }
    }

    /// Set up a route channel for a test link, returning the receiver for collecting forwarded
    /// messages.
    async fn setup_named_route(
        user_state: &Arc<RwLock<ServerUserState>>,
        link_name: &str,
    ) -> mpsc::Receiver<Message> {
        let (route_tx, route_rx) = mpsc::channel::<Message>(64);
        let mut us = user_state.write().await;
        us.routes.insert(
            link_name.to_string(),
            ConnectionHandle::new(route_tx, Arc::new(AtomicU64::new(1))),
        );
        route_rx
    }

    async fn setup_route(user_state: &Arc<RwLock<ServerUserState>>) -> mpsc::Receiver<Message> {
        setup_named_route(user_state, "test-link").await
    }

    /// Receive the next message from the route channel, decode its routable payload.
    async fn recv_routable(rx: &mut mpsc::Receiver<Message>) -> RoutableMessage {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for stream message")
            .expect("route channel closed");
        let Message::Routable { payload, .. } = msg else {
            panic!("expected Routable, got {:?}", msg);
        };
        RoutableMessage::decode(&payload).unwrap()
    }

    async fn register_test_subscription(
        ctx: &ConnectionContext,
        agent_id: Uuid,
        subscription_id: Uuid,
        reply_src: Route,
        reply_dst: &Route,
        mode: SubscriptionMode,
    ) -> (SubscriptionHandle, oneshot::Receiver<()>) {
        SubscriptionHandle::register(
            ctx,
            subscription_id,
            agent_id,
            mode,
            reply_src,
            reply_dst.clone(),
        )
        .await
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
            register_subscription(
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
            register_subscription(
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
            register_subscription(
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
    async fn stream_source_eof_sends_source_closed_and_cleans_up_subscription() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let reply_dst = Route::from_link("client");
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link("server"),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        mock_tx.send(b"hello".to_vec()).await.unwrap();
        mock_tx.send(b"world".to_vec()).await.unwrap();
        drop(mock_tx);

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::RawOutput { subscription_id: id, data } if id == subscription_id && data == b"hello"
        ));
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::RawOutput { subscription_id: id, data } if id == subscription_id && data == b"world"
        ));
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::SourceClosed,
            } if id == subscription_id
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn withdrawn_agent_stream_still_sends_source_closed_on_eof() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let session = AgentSession::Claude(crate::agents::ClaudeSession::new_readonly(
            agent_id,
            PathBuf::from("/tmp"),
        ));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let subscription_id = Uuid::new_v4();
        let reply_dst = Route::from_link("client");
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link("server"),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        {
            let mut us = user_state.write().await;
            let removed = withdraw_agent(&mut us, agent_id);
            assert!(
                removed.is_some(),
                "withdrawal should return the removed session"
            );
            assert!(
                us.active_subscriptions.contains_key(&subscription_id),
                "active subscription should remain registered until EOF"
            );
        }

        drop(mock_tx);

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::SourceClosed,
            } if id == subscription_id
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn stream_cancelled_stops_without_subscription_closed() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let reply_dst = Route::from_link("client");
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link("server"),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        {
            let mut us = user_state.write().await;
            let cancelled = cancel_subscriptions_matching(&mut us, |_| true);
            assert_eq!(cancelled.len(), 1);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            route_rx.try_recv().is_err(),
            "cancelled stream should not send synthetic SubscriptionClosed"
        );
    }

    #[tokio::test]
    async fn stream_no_route_skips_spawn() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (_cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);
        let handle = SubscriptionHandle {
            subscription_id,
            agent_id,
            mode: SubscriptionMode::Raw,
            reply_src: Route::from_link("server"),
            reply_dst: Route::from_link("client"),
        };

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        let us = user_state.read().await;
        assert!(
            !us.active_subscriptions.contains_key(&subscription_id),
            "no subscription should be registered when route doesn't exist"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lease_expiry_removes_subscription_and_emits_closed() {
        let (state, user_state) = test_state().await;
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client");
        full_dst.push("test-link");

        {
            let mut us = user_state.write().await;
            register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        tokio::time::advance(SUBSCRIPTION_LEASE_DURATION + Duration::from_secs(1)).await;
        sweep_expired_subscriptions(&state).await;

        assert!(
            cancel_rx.try_recv().is_err(),
            "lease expiry should drop the cancellation sender"
        );
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::LeaseExpired,
            } if id == subscription_id
        ));

        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn lease_expiry_does_not_block_on_full_route_channel() {
        let (state, user_state) = test_state().await;

        let (route_tx, _route_rx) = mpsc::channel::<Message>(1);
        route_tx
            .try_send(Message::Direct {
                message: DirectMessage::InitialSyncComplete,
            })
            .unwrap();
        {
            let mut us = user_state.write().await;
            us.routes.insert(
                "test-link".to_string(),
                ConnectionHandle::new(route_tx, Arc::new(AtomicU64::new(1))),
            );
        }

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client");
        full_dst.push("test-link");

        {
            let mut us = user_state.write().await;
            register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now(),
            );
        }

        tokio::time::timeout(
            Duration::from_millis(50),
            sweep_expired_subscriptions(&state),
        )
        .await
        .expect("sweep should not block on a full route channel");

        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
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
            register_subscription(
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

    #[tokio::test(start_paused = true)]
    async fn owner_lease_expiry_cleans_up_remote_shaped_subscription_without_intermediate_state() {
        let (state, user_state) = test_state().await;
        let mut route_rx = setup_named_route(&user_state, "peer-hop").await;

        let agent_id = Uuid::new_v4();
        let subscription_id = Uuid::new_v4();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link("client-link");
        full_dst.push("peer-hop");

        {
            let mut us = user_state.write().await;
            register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        tokio::time::advance(SUBSCRIPTION_LEASE_DURATION + Duration::from_secs(1)).await;
        sweep_expired_subscriptions(&state).await;

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::LeaseExpired,
            } if id == subscription_id
        ));
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }
}
