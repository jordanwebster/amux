//! Message dispatch handlers for the three protocol message categories.
//!
//! Each handler is independent: `handle_routable` processes hop-by-hop forwarded
//! messages and local delivery, `handle_command` processes CLI-only commands from
//! local connections, and `handle_direct` processes peer-to-peer discovery messages.

use super::accept::tcp_connect;
use super::connection::{
    ConnectionContext, cancel_streams_matching, cleanup_stream, register_stream,
};
use super::routing::{
    broadcast_to_peers, connection_tx, create_agent, handle_subscribe, resume_agents,
};
use crate::agent_registry::Agent;
use crate::buffer::{BroadcastReader, BufferPolicy};
use crate::claude::types::{ClaudeHook, Hook};
use crate::error::{AmuxError, Result};
use crate::message::{
    Command, DirectMessage, Message, ProtocolError, RoutableMessage, ServerDebugInfo,
};
use crate::route::Route;
use crate::state::State;
use std::future::Future;
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument;

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

/// Spawn a subscription stream task that reads from a buffer, wraps each item
/// into a RoutableMessage, and forwards it to the subscriber. Handles cancellation
/// and cleanup automatically.
async fn spawn_subscription_stream<R: SubscriptionReader>(
    mut reader: R,
    agent_id: uuid::Uuid,
    mode: &'static str,
    wrap_item: fn(uuid::Uuid, R::Item) -> RoutableMessage,
    reply_src: Route,
    reply_dst: Route,
    ctx: &ConnectionContext,
) {
    let Some(tx) = connection_tx(&ctx.user_state, &ctx.link_name).await else {
        return;
    };

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let stream_id = {
        let mut us = ctx.user_state.write().await;
        register_stream(
            &mut us,
            agent_id,
            cancel_tx,
            reply_dst.clone(),
            ctx.link_name.clone(),
        )
    };

    let stream_span = tracing::info_span!("stream", stream_id, agent_id = %agent_id, mode);
    let stream_user_state = ctx.user_state.clone();
    let next_rid = ctx.next_request_id.clone();
    tokio::spawn(
        async move {
            tokio::select! {
                _ = async {
                    while let Some(item) = reader.recv().await {
                        let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                        if tx
                            .send(Message::routable(
                                reply_src.clone(),
                                reply_dst.clone(),
                                rid,
                                &wrap_item(agent_id, item),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                    let _ = tx
                        .send(Message::routable(
                            reply_src.clone(),
                            reply_dst.clone(),
                            rid,
                            &RoutableMessage::SubscriptionClosed { agent_id },
                        ))
                        .await;
                } => {}
                _ = cancel_rx => {
                    tracing::debug!("stream cancelled");
                }
            }
            cleanup_stream(&stream_user_state, agent_id, stream_id).await;
            tracing::debug!("stream ended");
        }
        .instrument(stream_span),
    );
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
        Message::Direct(direct) => handle_direct(tx, direct, ctx).await,
        Message::Command(cmd) => {
            if !ctx.is_local {
                tracing::warn!(cmd = cmd.type_label(), "rejecting command from remote peer");
                return Ok(());
            }
            handle_command(tx, cmd, ctx).await
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
        src.push(&next_hop);

        let route_tx = {
            let us = ctx.user_state.read().await;
            us.routes.get(&next_hop).cloned()
        };

        match route_tx {
            Some(route_tx) => {
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
                tracing::debug!(next_hop = %next_hop, "no route");
            }
        }

        return Ok(());
    }

    // Local delivery — two-step decode
    let message = match RoutableMessage::decode(&payload) {
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
                    &RoutableMessage::UnknownMessage,
                ))
                .await;
            return Ok(());
        }
    };

    match &message {
        RoutableMessage::RawInput { .. }
        | RoutableMessage::RawOutput { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::StructuredInput { .. } => {}
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
                    let _ = tx
                        .send(Message::routable(
                            reply_src.clone(),
                            reply_dst.clone(),
                            request_id,
                            &RoutableMessage::SubscribeRawResult {
                                agent_id,
                                error: None,
                            },
                        ))
                        .await;

                    tracing::info!(agent_id = %agent_id, mode = "raw", "subscribed");

                    spawn_subscription_stream(
                        buffer_reader,
                        agent_id,
                        "raw",
                        |id, data| RoutableMessage::RawOutput { agent_id: id, data },
                        reply_src,
                        reply_dst,
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
                                agent_id,
                                error: Some(ProtocolError::ServerError(e.to_string())),
                            },
                        ))
                        .await;
                    Ok(())
                }
            }
        }

        RoutableMessage::SubscribeStructured { agent_id } => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "SubscribeStructured") else {
                return Ok(());
            };
            let subscribed = {
                let us = ctx.user_state.read().await;
                let Some(session) = us.agents.get(&agent_id) else {
                    let _ = tx
                        .send(Message::routable(
                            reply_src,
                            reply_dst,
                            request_id,
                            &RoutableMessage::SubscribeStructuredResult {
                                agent_id,
                                seq: 0,
                                error: Some(ProtocolError::ServerError(format!(
                                    "agent {agent_id} not found"
                                ))),
                            },
                        ))
                        .await;
                    return Ok(());
                };
                session.subscribe_with_current_seq().await
            };

            let Some((reader, current_seq)) = subscribed else {
                let _ = tx
                    .send(Message::routable(
                        reply_src,
                        reply_dst,
                        request_id,
                        &RoutableMessage::SubscribeStructuredResult {
                            agent_id,
                            seq: 0,
                            error: Some(ProtocolError::ServerError(format!(
                                "agent {agent_id} session ended"
                            ))),
                        },
                    ))
                    .await;
                return Ok(());
            };

            let _ = tx
                .send(Message::routable(
                    reply_src.clone(),
                    reply_dst.clone(),
                    request_id,
                    &RoutableMessage::SubscribeStructuredResult {
                        agent_id,
                        seq: current_seq,
                        error: None,
                    },
                ))
                .await;

            tracing::info!(agent_id = %agent_id, mode = "structured", "subscribed");

            spawn_subscription_stream(
                reader,
                agent_id,
                "structured",
                |id, envelope| RoutableMessage::StructuredOutput {
                    agent_id: id,
                    seq: envelope.seq,
                    data: Box::new(envelope.data),
                },
                reply_src,
                reply_dst,
                ctx,
            )
            .await;

            Ok(())
        }

        RoutableMessage::CreateAgent(req) => {
            let Some((reply_src, reply_dst)) = reply_routes(src, "CreateAgent") else {
                return Ok(());
            };
            let agent_id = req.agent_id;
            let result = create_agent(&ctx.user_state, &ctx.event_tx, req, ctx.user_id).await;
            let response_message = match result {
                Ok(()) => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: None,
                },
                Err(e) => RoutableMessage::CreateAgentResult {
                    agent_id,
                    error: Some(ProtocolError::ServerError(e.to_string())),
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
            data,
        } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id)
                && let Err(error) = session.send_structured_input(client_seq, data).await
            {
                if let Some((reply_src, reply_dst)) = reply_routes(src, "StructuredInput") {
                    let _ = tx
                        .send(Message::routable(
                            reply_src,
                            reply_dst,
                            request_id,
                            &RoutableMessage::StructuredInputResult {
                                agent_id,
                                error: Some(error),
                            },
                        ))
                        .await;
                }
                return Ok(());
            }
            Ok(())
        }

        // Response messages that arrived at their destination (empty dst)
        RoutableMessage::SubscribeRawResult { .. }
        | RoutableMessage::SubscribeStructuredResult { .. }
        | RoutableMessage::CreateAgentResult { .. }
        | RoutableMessage::RawOutput { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::StructuredInputResult { .. }
        | RoutableMessage::SubscriptionClosed { .. }
        | RoutableMessage::UnknownMessage => Ok(()),
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
                .send(super::ShutdownRequest::Shutdown { reply: tx.clone() })
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
                Ok(()) => Message::Command(Command::ConnectToServerResult { error: None }),
                Err(e) => Message::Command(Command::ConnectToServerResult {
                    error: Some(ProtocolError::ServerError(e.to_string())),
                }),
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::Debug => {
            let state = ctx.state.read().await;
            let use_cloud_mode = State::load(&state.config.state_path)
                .map(|s| s.cloud.use_cloud_mode == Some(true))
                .unwrap_or(false);
            let mut agent_count = 0;
            let mut remote_agent_count = 0;
            let mut host_count = 0;
            let mut route_count = 0;
            let mut peer_link_count = 0;
            for us in state.users.values() {
                let us = us.read().await;
                agent_count += us.agents.len();
                remote_agent_count += us.registry.count_remote();
                host_count += us.hosts.len();
                route_count += us.routes.len();
                peer_link_count += us.peer_links.len();
            }
            let info = ServerDebugInfo {
                is_cloud_server: state.is_cloud_server,
                use_cloud_mode,
                user_count: state.users.len(),
                agent_count,
                remote_agent_count,
                host_count,
                route_count,
                peer_link_count,
                config: state.config.clone(),
            };
            let _ = tx
                .send(Message::Command(Command::DebugResult { info }))
                .await;
            Ok(())
        }

        Command::ListAgents => {
            let agents = {
                let us = ctx.user_state.read().await;
                us.registry.list_all()
            };
            let _ = tx
                .send(Message::Command(Command::ListAgentsResult {
                    agents: agents.into_iter().collect(),
                }))
                .await;
            Ok(())
        }

        Command::HandleHook { agent_id, hook } => {
            let hook_type = match hook.as_ref() {
                Hook::Claude(ClaudeHook::SessionStart(_)) => "SessionStart",
                Hook::Claude(ClaudeHook::PermissionRequest(_)) => "PermissionRequest",
                Hook::Claude(ClaudeHook::PreToolUse(_)) => "PreToolUse",
                Hook::Claude(ClaudeHook::PostToolUse(_)) => "PostToolUse",
                Hook::Claude(ClaudeHook::PostToolUseFailure(_)) => "PostToolUseFailure",
                Hook::Claude(ClaudeHook::Stop(_)) => "Stop",
                Hook::Claude(ClaudeHook::Unknown) => "Unknown",
            };
            tracing::debug!(hook_type, %agent_id, "received hook event");

            // Unknown hook variants should be filtered client-side; warn and ack
            if matches!(hook.as_ref(), Hook::Claude(ClaudeHook::Unknown)) {
                tracing::warn!(%agent_id, "received unknown hook variant");
                let _ = tx
                    .send(Message::Command(Command::HandleHookResult { error: None }))
                    .await;
                return Ok(());
            }

            let result = {
                let mut us = ctx.user_state.write().await;
                match us.agents.get_mut(&agent_id) {
                    Some(session) => session.handle_hook(*hook).await.map_err(|e| {
                        ProtocolError::ServerError(format!("hook handling failed: {e}"))
                    }),
                    None => {
                        tracing::warn!(%agent_id, "no agent found for hook");
                        Err(ProtocolError::ServerError(format!(
                            "No agent found with agent_id: {agent_id}"
                        )))
                    }
                }
            };

            let response = match result {
                Ok(()) => Message::Command(Command::HandleHookResult { error: None }),
                Err(e) => Message::Command(Command::HandleHookResult { error: Some(e) }),
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::ResolveAgent { identifier } => {
            let us = ctx.user_state.read().await;
            let agent = us.registry.resolve(&identifier);
            let _ = tx
                .send(Message::Command(Command::ResolveAgentResult { agent }))
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
                .send(super::ShutdownRequest::Suspend { reply: tx.clone() })
                .await;
            Ok(())
        }

        Command::Resume => {
            tracing::info!("resume requested");
            let state_path = {
                let state = ctx.state.read().await;
                state.config.state_path.clone()
            };
            let suspended = match crate::state::load_and_remove_suspended(&state_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "failed to load suspended agents");
                    let _ = tx
                        .send(Message::Command(Command::ResumeResult {
                            resumed_count: 0,
                            failed_count: 0,
                            error: Some(ProtocolError::ServerError(format!(
                                "failed to load state: {e}"
                            ))),
                        }))
                        .await;
                    return Ok(());
                }
            };
            let (resumed_count, failed_count) = resume_agents(
                &ctx.user_state,
                &ctx.event_tx,
                ctx.user_id,
                suspended.agents,
            )
            .await;
            let _ = tx
                .send(Message::Command(Command::ResumeResult {
                    resumed_count,
                    failed_count,
                    error: None,
                }))
                .await;
            Ok(())
        }

        // Response variants — should not arrive at the server
        Command::ListAgentsResult { .. }
        | Command::ResolveAgentResult { .. }
        | Command::ShutdownNotification(_)
        | Command::DebugResult { .. }
        | Command::ConnectToServerResult { .. }
        | Command::HandleHookResult { .. }
        | Command::SuspendResult { .. }
        | Command::ResumeResult { .. } => {
            tracing::warn!("unexpected command response variant");
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
            let is_cloud = {
                let state = ctx.state.read().await;
                state.is_cloud_server
            };

            if is_cloud {
                let (validator, host, tcp_port) = {
                    let state = ctx.state.read().await;
                    let validator = state
                        .jwt_validator
                        .clone()
                        .expect("is_cloud_server requires jwt_validator");
                    (
                        validator,
                        state.config.host_name.clone(),
                        state.config.tcp_port,
                    )
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
                                .send(Message::Direct(DirectMessage::ReauthResult {
                                    error: Some(ProtocolError::InvalidCredentials),
                                }))
                                .await;
                            return Err(AmuxError::InvalidCredentials);
                        }
                        tracing::debug!("re-authenticated");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "re-auth token validation failed");
                        let _ = tx
                            .send(Message::Direct(DirectMessage::ReauthResult {
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await;
                        return Ok(());
                    }
                }
            }

            let _ = tx
                .send(Message::Direct(DirectMessage::ReauthResult { error: None }))
                .await;
            Ok(())
        }

        DirectMessage::AnnounceAgent {
            agent_id,
            name,
            command,
            working_dir,
            route: received_route,
        } => {
            let mut us = ctx.user_state.write().await;

            // Local agent takes precedence — skip if we own this agent
            if us.agents.contains_key(&agent_id) {
                tracing::debug!(agent_id = %agent_id, "ignoring announce for local agent");
                return Ok(());
            }

            // Compute our route: prepend the link this came from
            let mut our_route = received_route.clone();
            our_route.push(&ctx.link_name);

            let info = Agent {
                id: agent_id,
                name: name.clone(),
                command: command.clone(),
                working_dir: working_dir.clone(),
                route: our_route.clone(),
            };

            if let Err(e) = us.registry.register_remote(info) {
                tracing::warn!(error = %e, agent_id = %agent_id, "ignoring invalid remote announcement");
                return Ok(());
            }

            tracing::info!(agent_id = %agent_id, name = ?name, "stored remote agent");

            // Propagate to other peers with our stored route
            broadcast_to_peers(
                &mut us,
                &DirectMessage::AnnounceAgent {
                    agent_id,
                    name,
                    command,
                    working_dir,
                    route: our_route,
                },
                Some(&ctx.link_name),
            );

            Ok(())
        }

        DirectMessage::WithdrawAgent { agent_id } => {
            let mut us = ctx.user_state.write().await;

            // Only remove if the stored link matches the sender
            let should_remove = us
                .registry
                .get(&agent_id)
                .is_some_and(|e| matches!(&e.route.peek(), Some(link) if link == &ctx.link_name));

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

            let mut our_route = received_route;
            our_route.push(&ctx.link_name);

            let info = crate::message::Host {
                id,
                name: name.clone(),
                route: our_route.clone(),
                version: version.clone(),
            };

            us.hosts.insert(id, info);
            tracing::info!(host_id = %id, name = %name, "stored remote host");

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

        DirectMessage::WithdrawHost { id } => {
            let mut us = ctx.user_state.write().await;

            let should_remove = us
                .hosts
                .get(&id)
                .is_some_and(|h| matches!(h.route.peek(), Some(link) if link == ctx.link_name));

            if should_remove {
                let host_route = us.hosts.get(&id).map(|h| h.route.clone());
                us.hosts.remove(&id);
                tracing::info!(host_id = %id, "withdrew remote host");

                if let Some(ref host_route) = host_route {
                    let removed = us.registry.remove_for_route_prefix(host_route);
                    if !removed.is_empty() {
                        tracing::info!(count = removed.len(), host_id = %id, "removed agents for withdrawn host");
                    }

                    let cancelled = cancel_streams_matching(&mut us, |entry| {
                        entry.dst.starts_with_route(host_route)
                    });
                    if cancelled > 0 {
                        tracing::info!(count = cancelled, host_id = %id, "cancelled streams for withdrawn host");
                    }
                }

                broadcast_to_peers(
                    &mut us,
                    &DirectMessage::WithdrawHost { id },
                    Some(&ctx.link_name),
                );
            } else {
                tracing::debug!(host_id = %id, "ignoring withdraw host (link mismatch)");
            }

            Ok(())
        }

        DirectMessage::ReauthResult { .. } => {
            tracing::warn!("unexpected direct message");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentSession;
    use crate::claude::types::{
        BashToolInput, ClaudePermissionRequest, ClaudePermissionTool, ClaudeSessionStart,
        ClaudeStop,
    };
    use crate::route::Route;
    use crate::server::test_helpers::{test_ctx, test_state};
    use crate::server::{LOCAL_USER_ID, ServerUserState};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use tokio::sync::{RwLock, mpsc};
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

    /// Create an AgentSession::TestAgent from a CreateAgentRequest.
    fn create_test_session(req: &crate::message::CreateAgentRequest) -> AgentSession {
        let cmd = match &req.agent_type {
            crate::message::AgentType::TestAgent(cmd) => cmd.clone(),
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
        let Message::Direct(DirectMessage::ReauthResult { error }) = &msgs[0] else {
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
    async fn announce_agent_stores_in_registry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = DirectMessage::AnnounceAgent {
            agent_id,
            name: Some("remote-test".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(us.registry.contains(&agent_id));
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name, Some("remote-test".to_string()));
        assert!(entry.is_remote());
        let mut route = entry.route.clone();
        assert_eq!(route.pop(), Some("test-link".to_string()));
        assert_eq!(route.pop(), None);
    }

    #[tokio::test]
    async fn announce_agent_with_route_prepends_link() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = DirectMessage::AnnounceAgent {
            agent_id,
            name: None,
            command: "bash".to_string(),
            working_dir: PathBuf::from("/home"),
            route: Route::from_link("host-a"),
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert!(entry.is_remote());
        let mut route = entry.route.clone();
        // Should be test-link.host-a (test-link prepended)
        assert_eq!(route.pop(), Some("test-link".to_string()));
        assert_eq!(route.pop(), Some("host-a".to_string()));
        assert_eq!(route.pop(), None);
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
                agent_type: crate::message::AgentType::TestAgent(dummy_pty_command()),
                working_dir: dummy_working_dir(),
                terminal_size: Some(crate::message::TerminalSize { rows: 24, cols: 80 }),
            };
            let session = create_test_session(&req);
            let info = session.to_agent();
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        // Try to announce same agent_id from remote
        let msg = DirectMessage::AnnounceAgent {
            agent_id,
            name: Some("remote".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/remote"),
            route: Route::empty(),
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
        {
            let mut us = user_state.write().await;
            us.registry
                .register_remote(Agent {
                    id: agent_id,
                    name: None,
                    command: "bash".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: Route::from_link("test-link"),
                })
                .unwrap();
        }

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
        {
            let mut us = user_state.write().await;
            us.registry
                .register_remote(Agent {
                    id: agent_id,
                    name: None,
                    command: "bash".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: Route::from_link("other-link"),
                })
                .unwrap();
        }

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

        let agent_id = Uuid::new_v4();

        // First announce
        let msg = DirectMessage::AnnounceAgent {
            agent_id,
            name: Some("first".to_string()),
            command: "bash".to_string(),
            working_dir: PathBuf::from("/first"),
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        // Second announce with same agent_id
        let msg = DirectMessage::AnnounceAgent {
            agent_id,
            name: Some("second".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/second"),
            route: Route::empty(),
        };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name, Some("second".to_string()));
        assert_eq!(entry.working_dir, PathBuf::from("/second"));
    }

    #[tokio::test]
    async fn resolve_agent_by_name() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.registry
                .register_remote(Agent {
                    id: agent_id,
                    name: Some("my-agent".to_string()),
                    command: "claude".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: Route::from_link("peer-a"),
                })
                .unwrap();
        }

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let cmd = Command::ResolveAgent {
            identifier: "my-agent".to_string(),
        };
        handle_command(&tx, cmd, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command(Command::ResolveAgentResult { agent }) = &msgs[0] else {
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
        let Message::Command(Command::ResolveAgentResult { agent }) = &msgs[0] else {
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

        let msg = DirectMessage::WithdrawHost { id: host_id };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
    }

    #[tokio::test]
    async fn withdraw_host_ignores_link_mismatch() {
        let (state, user_state) = test_state().await;

        let host_id = Uuid::new_v4();
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
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        // Withdraw from "test-link" but host is stored from "other-link"
        let msg = DirectMessage::WithdrawHost { id: host_id };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        // Should still be there (link mismatch)
        let us = user_state.read().await;
        assert!(us.hosts.contains_key(&host_id));
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
            next_request_id: Arc::new(AtomicU64::new(1)),
        };
        let (tx, written) = mock_tx();

        // Remote peer sends Shutdown — should be silently rejected
        let msg = Message::Command(Command::Shutdown);
        handle_message(&tx, msg, &ctx).await.unwrap();

        // Remote peer sends ListAgents — should also be rejected
        let msg = Message::Command(Command::ListAgents);
        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "remote peer should receive no response to commands"
        );
    }

    #[tokio::test]
    async fn routable_to_nonexistent_route_is_silently_dropped() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = Message::routable(
            Route::from_link("sender"),
            Route::from_link("nonexistent-hop"),
            1,
            &RoutableMessage::RawInput {
                agent_id,
                data: vec![0x41],
            },
        );

        // Should not error — forwarding failures are silent drops
        handle_message(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "no error message should be sent for routing failures"
        );
    }

    #[tokio::test]
    async fn invalid_payload_returns_unknown_message() {
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
        // Should be a routable reply containing UnknownMessage
        let Message::Routable { payload, .. } = &msgs[0] else {
            panic!("expected Routable reply, got {:?}", msgs[0]);
        };
        let reply = RoutableMessage::decode(payload).unwrap();
        assert!(
            matches!(reply, RoutableMessage::UnknownMessage),
            "expected UnknownMessage, got {:?}",
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
            us.routes.insert("peer-a".to_string(), peer_tx);
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
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::SubscribeStructuredResult {
                agent_id: Uuid::new_v4(),
                seq: 0,
                error: None,
            },
            RoutableMessage::CreateAgentResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::RawOutput {
                agent_id: Uuid::new_v4(),
                data: vec![0x41],
            },
            RoutableMessage::StructuredOutput {
                agent_id: Uuid::new_v4(),
                seq: 1,
                data: Box::new(crate::claude::types::AgentStructuredOutput::Claude(
                    crate::claude::types::ClaudeStructuredOutput::AgentStopped,
                )),
            },
            RoutableMessage::StructuredInputResult {
                agent_id: Uuid::new_v4(),
                error: None,
            },
            RoutableMessage::SubscriptionClosed {
                agent_id: Uuid::new_v4(),
            },
            RoutableMessage::UnknownMessage,
        ];

        for response in responses {
            let msg = Message::routable(Route::from_link("sender"), Route::empty(), 1, &response);
            handle_message(&tx, msg, &ctx).await.unwrap();
        }

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert!(
            msgs.is_empty(),
            "response variants at destination should produce no output"
        );
    }

    #[tokio::test]
    async fn withdraw_host_removes_agents_with_matching_route() {
        let (state, user_state) = test_state().await;

        let host_id = Uuid::new_v4();
        let agent1 = Uuid::new_v4();
        let agent2 = Uuid::new_v4();
        let agent3 = Uuid::new_v4();
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
            // Agents reachable via test-link (should be removed)
            us.registry
                .register_remote(Agent {
                    id: agent1,
                    name: Some("a1".to_string()),
                    command: "claude".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: Route::from_link("test-link"),
                })
                .unwrap();
            let mut deep_route = Route::from_link("host-b");
            deep_route.push("test-link");
            us.registry
                .register_remote(Agent {
                    id: agent2,
                    name: Some("a2".to_string()),
                    command: "claude".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: deep_route,
                })
                .unwrap();
            // Agent on different link (should survive)
            us.registry
                .register_remote(Agent {
                    id: agent3,
                    name: Some("a3".to_string()),
                    command: "claude".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: Route::from_link("other-link"),
                })
                .unwrap();
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let msg = DirectMessage::WithdrawHost { id: host_id };
        handle_direct(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(!us.hosts.contains_key(&host_id));
        assert!(!us.registry.contains(&agent1));
        assert!(!us.registry.contains(&agent2));
        assert!(us.registry.contains(&agent3));
    }

    /// Insert a local test session into user_state.agents.
    async fn insert_test_session(user_state: &Arc<RwLock<ServerUserState>>, agent_id: Uuid) {
        let mut us = user_state.write().await;
        let req = crate::message::CreateAgentRequest {
            agent_id,
            name: Some("hook-test".to_string()),
            agent_type: crate::message::AgentType::TestAgent(dummy_pty_command()),
            working_dir: dummy_working_dir(),
            terminal_size: Some(crate::message::TerminalSize { rows: 24, cols: 80 }),
        };
        let session = create_test_session(&req);
        us.agents.insert(agent_id, session);
    }

    #[tokio::test]
    async fn handle_hook_session_start_no_session_returns_error() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_some(),
            "should return error when session not found"
        );
    }

    #[tokio::test]
    async fn handle_hook_session_start_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = Hook::Claude(ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/nonexistent_transcript.jsonl".to_string(),
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_permission_request_no_session_returns_error() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::PermissionRequest(ClaudePermissionRequest {
            session_id: Uuid::new_v4(),
            tool: ClaudePermissionTool::Bash {
                tool_input: BashToolInput {
                    command: "ls".to_string(),
                    description: None,
                    timeout: None,
                    run_in_background: None,
                    dangerously_disable_sandbox: None,
                },
            },
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_some(),
            "should return error when session not found"
        );
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
        let hook = Hook::Claude(ClaudeHook::PermissionRequest(ClaudePermissionRequest {
            session_id: Uuid::new_v4(),
            tool: tool.clone(),
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_stop_no_session_returns_error() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = Hook::Claude(ClaudeHook::Stop(ClaudeStop {
            session_id: Uuid::new_v4(),
            stop_hook_active: true,
            last_assistant_message: "Done.".to_string(),
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_some(),
            "should return error when session not found"
        );
    }

    #[tokio::test]
    async fn handle_hook_stop_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = Hook::Claude(ClaudeHook::Stop(ClaudeStop {
            session_id: Uuid::new_v4(),
            stop_hook_active: true,
            last_assistant_message: "I've completed the refactoring.".to_string(),
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
        let Message::Command(Command::HandleHookResult { error }) = &msgs[0] else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
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

    /// Set up a route channel for the test connection's link_name ("test-link"),
    /// returning the receiver for collecting forwarded messages.
    async fn setup_route(user_state: &Arc<RwLock<ServerUserState>>) -> mpsc::Receiver<Message> {
        let (route_tx, route_rx) = mpsc::channel::<Message>(64);
        let mut us = user_state.write().await;
        us.routes.insert("test-link".to_string(), route_tx);
        route_rx
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

    #[tokio::test]
    async fn stream_forwards_items_and_sends_subscription_closed() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            agent_id,
            "raw",
            |id, data| RoutableMessage::RawOutput { agent_id: id, data },
            Route::from_link("server"),
            Route::from_link("client"),
            &ctx,
        )
        .await;

        // Send items then close the reader
        mock_tx.send(b"hello".to_vec()).await.unwrap();
        mock_tx.send(b"world".to_vec()).await.unwrap();
        drop(mock_tx);

        // Verify forwarded items
        let msg = recv_routable(&mut route_rx).await;
        assert!(matches!(&msg, RoutableMessage::RawOutput { data, .. } if data == b"hello"));

        let msg = recv_routable(&mut route_rx).await;
        assert!(matches!(&msg, RoutableMessage::RawOutput { data, .. } if data == b"world"));

        // Verify SubscriptionClosed sent after reader exhaustion
        let msg = recv_routable(&mut route_rx).await;
        assert!(
            matches!(msg, RoutableMessage::SubscriptionClosed { agent_id: id } if id == agent_id)
        );
    }

    #[tokio::test]
    async fn stream_cleans_up_active_streams_on_close() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let _route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            agent_id,
            "raw",
            |id, data| RoutableMessage::RawOutput { agent_id: id, data },
            Route::from_link("server"),
            Route::from_link("client"),
            &ctx,
        )
        .await;

        // Stream should be registered in active_streams
        {
            let us = user_state.read().await;
            assert!(
                us.active_streams.contains_key(&agent_id),
                "stream should be registered after spawn"
            );
        }

        // Close the reader
        drop(mock_tx);

        // Give spawned task time to process close and clean up
        tokio::time::sleep(Duration::from_millis(100)).await;

        let us = user_state.read().await;
        assert!(
            !us.active_streams.contains_key(&agent_id),
            "stream should be cleaned up after reader close"
        );
    }

    #[tokio::test]
    async fn stream_cancelled_stops_without_subscription_closed() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        // Keep _mock_tx alive so the reader blocks (doesn't close)
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            agent_id,
            "raw",
            |id, data| RoutableMessage::RawOutput { agent_id: id, data },
            Route::from_link("server"),
            Route::from_link("client"),
            &ctx,
        )
        .await;

        // Cancel all streams (drops oneshot senders, triggering cancel_rx)
        {
            let mut us = user_state.write().await;
            let cancelled = cancel_streams_matching(&mut us, |_| true);
            assert_eq!(cancelled, 1);
        }

        // Give spawned task time to process cancellation
        tokio::time::sleep(Duration::from_millis(100)).await;

        // No SubscriptionClosed should have been sent (cancelled, not reader exhaustion)
        assert!(
            route_rx.try_recv().is_err(),
            "cancelled stream should not send SubscriptionClosed"
        );
    }

    #[tokio::test]
    async fn stream_no_route_skips_spawn() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        // Don't set up any route — connection_tx will return None

        let agent_id = Uuid::new_v4();
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            agent_id,
            "raw",
            |id, data| RoutableMessage::RawOutput { agent_id: id, data },
            Route::from_link("server"),
            Route::from_link("client"),
            &ctx,
        )
        .await;

        // No stream should be registered (early return when route not found)
        let us = user_state.read().await;
        assert!(
            !us.active_streams.contains_key(&agent_id),
            "no stream should be registered when route doesn't exist"
        );
    }
}
