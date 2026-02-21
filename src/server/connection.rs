use super::accept::tcp_connect;
use super::routing::{
    broadcast_to_peers, connection_tx, create_agent, handle_subscribe, shutdown_server,
};
use super::{ServerState, ServerUserState, StreamEntry};
use crate::agent_registry::Agent;
use crate::cloud::TokenRefreshState;
use crate::error::{AmuxError, Result};
use crate::message::{
    ClaudeHook, Command, DirectMessage, Hook, Message, ProtocolError, RoutableMessage,
    ServerDebugInfo, ShutdownReason,
};
use crate::route::Route;
use crate::session::SessionEvent;
use crate::state::State;
use crate::transport::MessageReader;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::Instrument;
use uuid::Uuid;

fn msg_type_label(msg: &Message) -> &'static str {
    match msg {
        Message::Routable { message, .. } => match message {
            RoutableMessage::SubscribeRaw { .. } => "SubscribeRaw",
            RoutableMessage::SubscribeStructured { .. } => "SubscribeStructured",
            RoutableMessage::SubscribeRawResult { .. } => "SubscribeRawResult",
            RoutableMessage::SubscribeStructuredResult { .. } => "SubscribeStructuredResult",
            RoutableMessage::CreateAgent(_) => "CreateAgent",
            RoutableMessage::CreateAgentResult { .. } => "CreateAgentResult",
            RoutableMessage::RawInput { .. } => "RawInput",
            RoutableMessage::RawOutput { .. } => "RawOutput",
            RoutableMessage::StructuredOutput { .. } => "StructuredOutput",
            RoutableMessage::StructuredInput { .. } => "StructuredInput",
            RoutableMessage::AgentEnded { .. } => "AgentEnded",
        },
        Message::Direct(direct) => match direct {
            DirectMessage::Connect { .. } => "Connect",
            DirectMessage::ConnectResult { .. } => "ConnectResult",
            DirectMessage::AnnounceAgent { .. } => "AnnounceAgent",
            DirectMessage::WithdrawAgent { .. } => "WithdrawAgent",
            DirectMessage::AnnounceHost { .. } => "AnnounceHost",
            DirectMessage::WithdrawHost { .. } => "WithdrawHost",
        },
        Message::Command(cmd) => match cmd {
            Command::ListAgents => "ListAgents",
            Command::ListAgentsResult { .. } => "ListAgentsResult",
            Command::ResolveAgent { .. } => "ResolveAgent",
            Command::ResolveAgentResult { .. } => "ResolveAgentResult",
            Command::Shutdown => "Shutdown",
            Command::ShutdownNotification(_) => "ShutdownNotification",
            Command::Debug => "Debug",
            Command::DebugResult { .. } => "DebugResult",
            Command::ConnectToServer { .. } => "ConnectToServer",
            Command::ConnectToServerResult { .. } => "ConnectToServerResult",
            Command::HandleHook { .. } => "HandleHook",
            Command::HandleHookResult { .. } => "HandleHookResult",
        },
    }
}

/// Context for connection handlers.
pub(super) struct ConnectionContext {
    pub(super) state: Arc<RwLock<ServerState>>,
    pub(super) user_state: Arc<RwLock<ServerUserState>>,
    pub(super) user_id: Uuid,
    pub(super) event_tx: mpsc::Sender<SessionEvent>,
    pub(super) link_name: String,
    pub(super) is_local: bool,
}

/// Typed enum for reader task output — avoids encoding transport errors as protocol messages.
pub(super) enum Incoming {
    Msg(Message),
    ReadErr(AmuxError),
    Eof,
}

/// Reader task: reads from transport, sends to channel. Never cancelled.
pub(super) async fn reader_loop<R: MessageReader>(mut reader: R, tx: mpsc::Sender<Incoming>) {
    loop {
        match reader.read_message().await {
            Ok(msg) => {
                if tx.send(Incoming::Msg(msg)).await.is_err() {
                    break;
                }
            }
            Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                let _ = tx.send(Incoming::Eof).await;
                break;
            }
            Err(e) => {
                let _ = tx.send(Incoming::ReadErr(e)).await;
                break;
            }
        }
    }
}

/// Writer task: drains message channel, writes to transport.
/// Also handles transport-specific background I/O (e.g., WebSocket pong responses).
pub(super) async fn writer_loop<W: crate::transport::MessageWriter>(
    mut writer: W,
    mut rx: mpsc::Receiver<Message>,
) {
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(msg) => {
                        if writer.write_message(&msg).await.is_err() { break; }
                    }
                    None => break,
                }
            }
            _ = writer.background() => {}
        }
    }
}

/// Shared connection loop for all transports. Pure channel I/O — cancellation-safe.
pub(super) async fn connection_loop(
    mut incoming_rx: mpsc::Receiver<Incoming>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    mut token_refresh: Option<TokenRefreshState>,
) -> Result<()> {
    let mut refresh_deadline = token_refresh.as_ref().map(|t| t.refresh_deadline());
    let mut awaiting_refresh: Option<tokio::time::Instant> = None;
    // Tracks routes where forwarding failed (insert-only; no error sent back).
    // Naturally bounded: link names include random suffixes and are never reused after disconnect.
    let mut dead_routes: HashSet<String> = HashSet::new();

    loop {
        let refresh_timeout = awaiting_refresh.map(|t| t + Duration::from_secs(30));

        tokio::select! {
            incoming = incoming_rx.recv() => {
                match incoming {
                    Some(Incoming::Msg(msg)) => {
                        // Intercept ConnectResult for token refresh
                        if let Message::Direct(DirectMessage::ConnectResult { .. }) = &msg {
                            if awaiting_refresh.is_some() {
                                if let Some(ref mut rs) = token_refresh {
                                    match rs.handle_response(&msg) {
                                        Ok(()) => {
                                            refresh_deadline = Some(rs.refresh_deadline());
                                        }
                                        Err(crate::cloud::CloudError::HostChanged) => {
                                            tracing::warn!("cloud host changed, reconnection required");
                                            return Err(AmuxError::Config("Cloud host changed".to_string()));
                                        }
                                        Err(e) => {
                                            tracing::error!(error = %e, "token refresh response error");
                                            return Err(AmuxError::Config(format!("Token refresh failed: {}", e)));
                                        }
                                    }
                                }
                                awaiting_refresh = None;
                                continue;
                            }
                            tracing::warn!("unexpected ConnectResult");
                            continue;
                        }
                        handle_message(&response_tx, msg, &ctx, &mut dead_routes).await?;
                    }
                    Some(Incoming::Eof) | None => {
                        tracing::debug!("disconnected");
                        return Ok(());
                    }
                    Some(Incoming::ReadErr(e)) => {
                        return Err(e);
                    }
                }
            }
            _ = maybe_sleep_until(refresh_deadline), if awaiting_refresh.is_none() && refresh_deadline.is_some() => {
                if let Some(ref mut rs) = token_refresh {
                    tracing::debug!("refreshing cloud token");
                    match rs.send_connect(&response_tx).await {
                        Ok(()) => {
                            awaiting_refresh = Some(tokio::time::Instant::now());
                        }
                        Err(crate::cloud::CloudError::HostChanged) => {
                            tracing::warn!("cloud host changed, reconnection required");
                            return Err(AmuxError::Config("Cloud host changed".to_string()));
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "token refresh failed");
                            return Err(AmuxError::Config(format!("Token refresh failed: {}", e)));
                        }
                    }
                }
            }
            _ = maybe_sleep_until(refresh_timeout), if awaiting_refresh.is_some() => {
                tracing::error!("token refresh response timeout");
                return Err(AmuxError::Config("Token refresh timed out".to_string()));
            }
        }
    }
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Register a stream entry in active_streams. Returns the assigned stream_id.
fn register_stream(
    us: &mut ServerUserState,
    agent_id: uuid::Uuid,
    cancel_tx: oneshot::Sender<()>,
    dst: Route,
    link: String,
) -> u64 {
    let sid = us.next_stream_id;
    us.next_stream_id += 1;
    us.active_streams
        .entry(agent_id)
        .or_default()
        .push(StreamEntry {
            stream_id: sid,
            cancel: cancel_tx,
            dst,
            link,
        });
    sid
}

/// Remove a stream entry by stream_id after the task exits.
async fn cleanup_stream(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: uuid::Uuid,
    stream_id: u64,
) {
    let mut us = user_state.write().await;
    if let Some(entries) = us.active_streams.get_mut(&agent_id) {
        entries.retain(|e| e.stream_id != stream_id);
        if entries.is_empty() {
            us.active_streams.remove(&agent_id);
        }
    }
}

/// Cancel all active streams matching a predicate. Returns count cancelled.
pub(super) fn cancel_streams_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&StreamEntry) -> bool,
) -> usize {
    let mut cancelled = 0usize;
    for entries in us.active_streams.values_mut() {
        entries.retain(|entry| {
            if predicate(entry) {
                cancelled += 1;
                false
            } else {
                true
            }
        });
    }
    us.active_streams.retain(|_, v| !v.is_empty());
    cancelled
}

pub(super) async fn handle_message(
    tx: &mpsc::Sender<Message>,
    msg: Message,
    ctx: &ConnectionContext,
    dead_routes: &mut HashSet<String>,
) -> Result<()> {
    if !matches!(
        &msg,
        Message::Routable {
            message: RoutableMessage::RawInput { .. }
                | RoutableMessage::RawOutput { .. }
                | RoutableMessage::StructuredOutput { .. }
                | RoutableMessage::StructuredInput { .. },
            ..
        }
    ) {
        tracing::debug!(msg_type = msg_type_label(&msg), "received message");
    }

    match msg {
        Message::Routable { src, dst, message } => {
            handle_routable(tx, src, dst, message, ctx, dead_routes).await
        }
        Message::Direct(direct) => handle_direct(tx, direct, ctx).await,
        Message::Command(cmd) => {
            if !ctx.is_local {
                tracing::warn!(
                    cmd = msg_type_label(&Message::Command(cmd)),
                    "rejecting command from remote peer"
                );
                return Ok(());
            }
            handle_command(tx, cmd, ctx).await
        }
    }
}

async fn handle_routable(
    tx: &mpsc::Sender<Message>,
    mut src: Route,
    mut dst: Route,
    message: RoutableMessage,
    ctx: &ConnectionContext,
    dead_routes: &mut HashSet<String>,
) -> Result<()> {
    // Check if this message needs forwarding
    if let Some(next_hop) = dst.pop() {
        src.push(&next_hop);

        let route_tx = {
            let us = ctx.user_state.read().await;
            us.routes.get(&next_hop).cloned()
        };

        // Try to forward; on failure, get the failed message back
        let failed_msg = match route_tx {
            Some(route_tx) => {
                match route_tx.send(Message::Routable { src, dst, message }).await {
                    Ok(()) => None,
                    Err(send_error) => {
                        // Channel closed — conditionally clean up stale route
                        {
                            let mut us = ctx.user_state.write().await;
                            if let Some(current_tx) = us.routes.get(&next_hop)
                                && current_tx.is_closed()
                            {
                                us.routes.remove(&next_hop);
                                tracing::warn!(route = %next_hop, "removed stale route");
                            }
                        }
                        let Message::Routable {
                            message: failed_msg,
                            ..
                        } = send_error.0
                        else {
                            unreachable!()
                        };
                        Some(failed_msg)
                    }
                }
            }
            None => {
                tracing::debug!(next_hop = %next_hop, "no route");
                Some(message)
            }
        };

        // Track dead routes for stream messages (insert-only, no error sent back)
        if let Some(
            RoutableMessage::RawOutput { .. }
            | RoutableMessage::StructuredOutput { .. }
            | RoutableMessage::AgentEnded { .. },
        ) = failed_msg
        {
            dead_routes.insert(next_hop.to_string());
        }

        return Ok(());
    }

    // Local delivery — dst is empty, we are the final destination
    match message {
        RoutableMessage::SubscribeRaw {
            agent_id,
            terminal_size,
        } => {
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");
            let result = handle_subscribe(&ctx.user_state, &agent_id, terminal_size).await;

            match result {
                Ok(mut buffer_reader) => {
                    let _ = tx
                        .send(Message::Routable {
                            src: reply_src.clone(),
                            dst: reply_dst.clone(),
                            message: RoutableMessage::SubscribeRawResult {
                                agent_id,
                                error: None,
                            },
                        })
                        .await;

                    tracing::info!(agent_id = %agent_id, mode = "raw", "subscribed");

                    let outgoing_tx = connection_tx(&ctx.user_state, &ctx.link_name).await;
                    if let Some(tx) = outgoing_tx {
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

                        let stream_span = tracing::info_span!("stream", stream_id, agent_id = %agent_id, mode = "raw");
                        let stream_user_state = ctx.user_state.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = async {
                                    while let Some(data) = buffer_reader.read().await {
                                        if tx
                                            .send(Message::Routable {
                                                src: reply_src.clone(),
                                                dst: reply_dst.clone(),
                                                message: RoutableMessage::RawOutput { agent_id, data },
                                            })
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    let _ = tx.send(Message::Routable {
                                        src: reply_src.clone(),
                                        dst: reply_dst.clone(),
                                        message: RoutableMessage::AgentEnded { agent_id },
                                    }).await;
                                } => {}
                                _ = cancel_rx => {
                                    tracing::debug!("stream cancelled");
                                }
                            }
                            cleanup_stream(&stream_user_state, agent_id, stream_id).await;
                            tracing::debug!("stream ended");
                        }.instrument(stream_span));
                    }

                    Ok(())
                }
                Err(e) => {
                    let _ = tx
                        .send(Message::Routable {
                            src: reply_src,
                            dst: reply_dst,
                            message: RoutableMessage::SubscribeRawResult {
                                agent_id,
                                error: Some(ProtocolError::ServerError(e.to_string())),
                            },
                        })
                        .await;
                    Ok(())
                }
            }
        }

        RoutableMessage::SubscribeStructured { agent_id } => {
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");
            let session = {
                let us = ctx.user_state.read().await;
                us.agents.get(&agent_id).cloned()
            };

            let Some(session) = session else {
                let _ = tx
                    .send(Message::Routable {
                        src: reply_src,
                        dst: reply_dst,
                        message: RoutableMessage::SubscribeStructuredResult {
                            agent_id,
                            error: Some(ProtocolError::ServerError(
                                "Agent not found or ended".to_string(),
                            )),
                        },
                    })
                    .await;
                return Ok(());
            };

            let log_reader = session.subscribe_logs().await;

            let Some(mut reader) = log_reader else {
                let _ = tx
                    .send(Message::Routable {
                        src: reply_src,
                        dst: reply_dst,
                        message: RoutableMessage::SubscribeStructuredResult {
                            agent_id,
                            error: Some(ProtocolError::ServerError(
                                "Agent not found or ended".to_string(),
                            )),
                        },
                    })
                    .await;
                return Ok(());
            };

            let _ = tx
                .send(Message::Routable {
                    src: reply_src.clone(),
                    dst: reply_dst.clone(),
                    message: RoutableMessage::SubscribeStructuredResult {
                        agent_id,
                        error: None,
                    },
                })
                .await;

            tracing::info!(agent_id = %agent_id, mode = "structured", "subscribed");

            let outgoing_tx = connection_tx(&ctx.user_state, &ctx.link_name).await;
            if let Some(tx) = outgoing_tx {
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

                let stream_span = tracing::info_span!("stream", stream_id, agent_id = %agent_id, mode = "structured");
                let stream_user_state = ctx.user_state.clone();
                tokio::spawn(
                    async move {
                        tokio::select! {
                            _ = async {
                                while let Some(data) = reader.read().await {
                                    if tx
                                        .send(Message::Routable {
                                            src: reply_src.clone(),
                                            dst: reply_dst.clone(),
                                            message: RoutableMessage::StructuredOutput {
                                                agent_id,
                                                data,
                                            },
                                        })
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                let _ = tx.send(Message::Routable {
                                    src: reply_src.clone(),
                                    dst: reply_dst.clone(),
                                    message: RoutableMessage::AgentEnded { agent_id },
                                }).await;
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

            Ok(())
        }

        RoutableMessage::CreateAgent(req) => {
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");
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
                .send(Message::Routable {
                    src: reply_src,
                    dst: reply_dst,
                    message: response_message,
                })
                .await;
            Ok(())
        }

        RoutableMessage::RawInput { agent_id, data } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id) {
                let _ = session.send_input(data).await;
            }
            Ok(())
        }

        RoutableMessage::StructuredInput { agent_id, data } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id) {
                match data {
                    crate::message::StructuredInput::Claude(claude_input) => match claude_input {
                        crate::message::ClaudeStructuredInput::SubmitMessage { data } => {
                            let _ = session.send_input(data).await;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            let _ = session.send_input(vec![b'\r']).await;
                        }
                        crate::message::ClaudeStructuredInput::PermissionResponse(response) => {
                            let keystroke =
                                super::routing::permission_response_keystroke(&response);
                            tracing::info!(agent_id = %agent_id, ?response, "sending permission response");
                            let _ = session.send_input(keystroke.to_vec()).await;
                        }
                    },
                }
            }
            Ok(())
        }

        // Response messages that arrived at their destination (empty dst)
        RoutableMessage::SubscribeRawResult { .. }
        | RoutableMessage::SubscribeStructuredResult { .. }
        | RoutableMessage::CreateAgentResult { .. }
        | RoutableMessage::RawOutput { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::AgentEnded { .. } => Ok(()),
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
            shutdown_server(&ctx.user_state).await;
            // Let agents handle SIGHUP from PTY master drop before exiting
            tokio::time::sleep(Duration::from_millis(200)).await;

            let _ = tx
                .send(Message::Command(Command::ShutdownNotification(
                    ShutdownReason::UserRequested,
                )))
                .await;

            let socket_path = {
                let state = ctx.state.read().await;
                state.config.socket_path.clone()
            };
            let _ = std::fs::remove_file(socket_path);
            tracing::info!("server exiting");
            std::process::exit(0);
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

        Command::HandleHook { hook } => {
            tracing::debug!("received hook event");

            let result = match &hook {
                Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                    let session = {
                        let us = ctx.user_state.read().await;
                        us.agents.get(&session_start.session_id).cloned()
                    };
                    if let Some(session) = session {
                        tracing::debug!(agent_id = %session_start.session_id, "linking transcript");
                        session
                            .link_transcript(PathBuf::from(&session_start.transcript_path))
                            .await;
                        Ok(())
                    } else {
                        tracing::warn!(session_id = %session_start.session_id, "no agent found for hook");
                        Err(ProtocolError::ServerError(format!(
                            "No agent found with session_id: {}",
                            session_start.session_id
                        )))
                    }
                }
                Hook::Claude(ClaudeHook::PermissionRequest(perm_req)) => {
                    let session = {
                        let us = ctx.user_state.read().await;
                        us.agents.get(&perm_req.session_id).cloned()
                    };
                    if let Some(session) = session {
                        tracing::debug!(agent_id = %perm_req.session_id, "permission request");
                        session
                            .write_log(crate::message::StructuredOutput::Claude(
                                crate::message::ClaudeStructuredOutput::PermissionRequest {
                                    tool: perm_req.tool.clone(),
                                },
                            ))
                            .await;
                        Ok(())
                    } else {
                        tracing::warn!(session_id = %perm_req.session_id, "no agent found for hook");
                        Err(ProtocolError::ServerError(format!(
                            "No agent found with session_id: {}",
                            perm_req.session_id
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

        // Response variants — should not arrive at the server
        Command::ListAgentsResult { .. }
        | Command::ResolveAgentResult { .. }
        | Command::ShutdownNotification(_)
        | Command::DebugResult { .. }
        | Command::ConnectToServerResult { .. }
        | Command::HandleHookResult { .. } => {
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
        // The peer sends Connect with the same link_name and a fresh token.
        DirectMessage::Connect {
            link_name, token, ..
        } => {
            if link_name != ctx.link_name {
                let _ = tx
                    .send(Message::Direct(DirectMessage::ConnectResult {
                        error: Some(ProtocolError::ServerError(
                            "Link name mismatch on re-auth".to_string(),
                        )),
                    }))
                    .await;
                return Ok(());
            }

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

                let token = match token {
                    Some(t) => t,
                    None => {
                        let _ = tx
                            .send(Message::Direct(DirectMessage::ConnectResult {
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await;
                        return Ok(());
                    }
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
                                .send(Message::Direct(DirectMessage::ConnectResult {
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
                            .send(Message::Direct(DirectMessage::ConnectResult {
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await;
                        return Ok(());
                    }
                }
            }

            let _ = tx
                .send(Message::Direct(DirectMessage::ConnectResult {
                    error: None,
                }))
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

            us.registry.register_remote(info).unwrap();

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
                us.hosts.remove(&id);
                tracing::info!(host_id = %id, "withdrew remote host");

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

        DirectMessage::ConnectResult { .. } => {
            tracing::warn!("unexpected direct message");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::route::Route;
    use crate::server::LOCAL_USER_ID;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

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

    fn test_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "test-link".to_string(),
            is_local: true,
        }
    }

    async fn test_state() -> (Arc<RwLock<ServerState>>, Arc<RwLock<ServerUserState>>) {
        let state = Arc::new(RwLock::new(ServerState::new(Config::default())));
        let user_state = {
            let s = state.read().await;
            s.get_user_state(&LOCAL_USER_ID).unwrap()
        };
        (state, user_state)
    }

    #[tokio::test]
    async fn connect_reauth_matching_link_succeeds() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        // Re-auth with matching link name (non-cloud mode = no token needed)
        let msg = DirectMessage::Connect {
            link_name: "test-link".to_string(),
            token: None,
            version: crate::message::PROTOCOL_VERSION,
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Direct(DirectMessage::ConnectResult { error }) = &msgs[0] else {
            panic!("expected ConnectResult, got {:?}", msgs[0]);
        };
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn connect_reauth_mismatched_link_rejected() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        // Re-auth with wrong link name
        let msg = DirectMessage::Connect {
            link_name: "wrong-link".to_string(),
            token: None,
            version: crate::message::PROTOCOL_VERSION,
        };

        handle_direct(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Direct(DirectMessage::ConnectResult { error }) = &msgs[0] else {
            panic!("expected ConnectResult, got {:?}", msgs[0]);
        };
        assert!(error.is_some());
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
            let (event_tx, _rx) = mpsc::channel(16);
            let req = crate::message::CreateAgentRequest {
                agent_id,
                name: Some("local".to_string()),
                agent_type: crate::message::AgentType::TestAgent("/bin/cat".to_string()),
                working_dir: PathBuf::from("/tmp"),
                terminal_size: Some(crate::message::TerminalSize { rows: 24, cols: 80 }),
            };
            let session =
                crate::session::LocalAgentSession::new(&req, event_tx, LOCAL_USER_ID).unwrap();
            let info = session.to_agent();
            us.agents.insert(agent_id, Arc::new(session));
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
}
