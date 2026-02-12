use super::accept::tcp_connect;
use super::routing::{
    broadcast_to_peers, connection_tx, create_agent, handle_subscribe, shutdown_server,
};
use super::{ServerState, ServerUserState, StreamEntry};
use crate::agent_registry::AgentKind;
use crate::cloud::TokenRefreshState;
use crate::error::{AmuxError, Result};
use crate::message::{
    AgentInfo, ClaudeHook, Hook, LocalMessage, Message, ProtocolError, RoutableMessage,
    ServerDebugInfo, SubscribeMode,
};
use crate::route::Route;
use crate::session::SessionEvent;
use crate::state::State;
use crate::transport::MessageReader;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

/// Context for connection handlers.
pub(super) struct ConnectionContext {
    pub(super) state: Arc<RwLock<ServerState>>,
    pub(super) user_state: Arc<RwLock<ServerUserState>>,
    pub(super) user_id: Uuid,
    pub(super) event_tx: mpsc::Sender<SessionEvent>,
    pub(super) link_name: String,
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
    // Tracks routes that already got a NoRouteFound error sent back for stream messages.
    // Naturally bounded: link names include random suffixes and are never reused after disconnect.
    let mut dead_routes: HashSet<String> = HashSet::new();

    loop {
        let refresh_timeout = awaiting_refresh.map(|t| t + Duration::from_secs(30));

        tokio::select! {
            incoming = incoming_rx.recv() => {
                match incoming {
                    Some(Incoming::Msg(msg)) => {
                        // Intercept ConnectResponse for token refresh
                        if let Message::Local(LocalMessage::ConnectResponse { .. }) = &msg {
                            if awaiting_refresh.is_some() {
                                if let Some(ref mut rs) = token_refresh {
                                    match rs.handle_response(&msg) {
                                        Ok(()) => {
                                            refresh_deadline = Some(rs.refresh_deadline());
                                        }
                                        Err(crate::cloud::CloudError::HostChanged) => {
                                            log!("cloud: host changed, reconnection required");
                                            return Err(AmuxError::Config("Cloud host changed".to_string()));
                                        }
                                        Err(e) => {
                                            log!("cloud: token refresh response error: {}", e);
                                            return Err(AmuxError::Config(format!("Token refresh failed: {}", e)));
                                        }
                                    }
                                }
                                awaiting_refresh = None;
                                continue;
                            }
                            log!("server: unexpected ConnectResponse on {}", ctx.link_name);
                            continue;
                        }
                        handle_message(&response_tx, msg, &ctx, &mut dead_routes).await?;
                    }
                    Some(Incoming::Eof) | None => {
                        log!("server: {} disconnected", ctx.link_name);
                        return Ok(());
                    }
                    Some(Incoming::ReadErr(e)) => {
                        return Err(e);
                    }
                }
            }
            _ = maybe_sleep_until(refresh_deadline), if awaiting_refresh.is_none() && refresh_deadline.is_some() => {
                if let Some(ref mut rs) = token_refresh {
                    log!("cloud: refreshing token");
                    match rs.send_connect(&response_tx).await {
                        Ok(()) => {
                            awaiting_refresh = Some(tokio::time::Instant::now());
                        }
                        Err(crate::cloud::CloudError::HostChanged) => {
                            log!("cloud: host changed, reconnection required");
                            return Err(AmuxError::Config("Cloud host changed".to_string()));
                        }
                        Err(e) => {
                            log!("cloud: token refresh failed: {}", e);
                            return Err(AmuxError::Config(format!("Token refresh failed: {}", e)));
                        }
                    }
                }
            }
            _ = maybe_sleep_until(refresh_timeout), if awaiting_refresh.is_some() => {
                log!("server: refresh response timeout on {}", ctx.link_name);
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
            message: RoutableMessage::InputBytes { .. }
                | RoutableMessage::SubmitInput { .. }
                | RoutableMessage::Output { .. }
                | RoutableMessage::StructuredOutput { .. },
            ..
        }
    ) {
        log!("server: {} received {:?}", ctx.link_name, msg);
    }

    match msg {
        Message::Routable { src, dst, message } => {
            handle_routable(tx, src, dst, message, ctx, dead_routes).await
        }
        Message::Local(local) => handle_local(tx, local, ctx).await,
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
        // Save original src BEFORE mutation — used for error replies
        let original_src = src.clone();
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
                            if let Some(current_tx) = us.routes.get(&next_hop) {
                                if current_tx.is_closed() {
                                    us.routes.remove(&next_hop);
                                    log!("server: removed stale route {}", next_hop);
                                }
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
                log!("server: no route to {}", next_hop);
                Some(message)
            }
        };

        // Send error back to source for request-type messages only.
        // For Output/StructuredOutput: first failure sends NoRouteFound back,
        // subsequent failures for same route are suppressed silently.
        if let Some(failed_msg) = failed_msg {
            match failed_msg {
                RoutableMessage::Error(_) => {
                    log!("server: dropping failed routable error to avoid amplification");
                }
                RoutableMessage::Output { .. }
                | RoutableMessage::StructuredOutput { .. }
                | RoutableMessage::AgentEnded { .. } => {
                    if dead_routes.insert(next_hop.to_string()) {
                        // First failure for this route — notify source
                        let mut traversed = original_src.clone();
                        traversed.push(&next_hop);
                        if let Some((reply_src, reply_dst)) = Route::reply(original_src) {
                            let _ = tx
                                .send(Message::Routable {
                                    src: reply_src,
                                    dst: reply_dst,
                                    message: RoutableMessage::Error(ProtocolError::NoRouteFound(
                                        traversed,
                                    )),
                                })
                                .await;
                        }
                    }
                    // else: already notified, suppress silently
                }
                _ => {
                    // Build traversed path: original_src + the failed hop
                    let mut traversed = original_src.clone();
                    traversed.push(&next_hop);

                    if let Some((reply_src, reply_dst)) = Route::reply(original_src) {
                        let _ = tx
                            .send(Message::Routable {
                                src: reply_src,
                                dst: reply_dst,
                                message: RoutableMessage::Error(ProtocolError::NoRouteFound(
                                    traversed,
                                )),
                            })
                            .await;
                    }
                }
            }
        }

        return Ok(());
    }

    // Local delivery — dst is empty, we are the final destination
    match message {
        RoutableMessage::Subscribe {
            agent_id,
            rows,
            cols,
            mode,
        } => {
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");

            match mode {
                SubscribeMode::Structured => {
                    let session = {
                        let us = ctx.user_state.read().await;
                        us.agents.get(&agent_id).cloned()
                    };

                    let Some(session) = session else {
                        let _ = tx
                            .send(Message::Routable {
                                src: reply_src,
                                dst: reply_dst,
                                message: RoutableMessage::SubscribeResult {
                                    agent_id,
                                    success: false,
                                    error: Some(ProtocolError::ServerError(
                                        "Agent not found or ended".to_string(),
                                    )),
                                },
                            })
                            .await;
                        return Ok(());
                    };

                    session.resize(rows, cols).await?;
                    let log_reader = session.subscribe_logs().await;

                    let Some(mut reader) = log_reader else {
                        let _ = tx
                            .send(Message::Routable {
                                src: reply_src,
                                dst: reply_dst,
                                message: RoutableMessage::SubscribeResult {
                                    agent_id,
                                    success: false,
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
                            message: RoutableMessage::SubscribeResult {
                                agent_id,
                                success: true,
                                error: None,
                            },
                        })
                        .await;

                    log!(
                        "server: {} subscribed to agent {} (structured)",
                        ctx.link_name,
                        agent_id
                    );

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

                        let stream_user_state = ctx.user_state.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = async {
                                    while let Some(entry) = reader.read().await {
                                        if tx
                                            .send(Message::Routable {
                                                src: reply_src.clone(),
                                                dst: reply_dst.clone(),
                                                message: RoutableMessage::StructuredOutput {
                                                    agent_id,
                                                    entry,
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
                                    log!("server: structured stream {} cancelled", stream_id);
                                }
                            }
                            cleanup_stream(&stream_user_state, agent_id, stream_id).await;
                            log!("server: structured log stream ended");
                        });
                    }

                    Ok(())
                }
                SubscribeMode::Raw => {
                    let result = handle_subscribe(&ctx.user_state, &agent_id, rows, cols).await;

                    match result {
                        Ok(mut buffer_reader) => {
                            let _ = tx
                                .send(Message::Routable {
                                    src: reply_src.clone(),
                                    dst: reply_dst.clone(),
                                    message: RoutableMessage::SubscribeResult {
                                        agent_id,
                                        success: true,
                                        error: None,
                                    },
                                })
                                .await;

                            log!(
                                "server: {} subscribed to agent {} (raw)",
                                ctx.link_name,
                                agent_id
                            );

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

                                let stream_user_state = ctx.user_state.clone();
                                tokio::spawn(async move {
                                    tokio::select! {
                                        _ = async {
                                            while let Some(data) = buffer_reader.read().await {
                                                if tx
                                                    .send(Message::Routable {
                                                        src: reply_src.clone(),
                                                        dst: reply_dst.clone(),
                                                        message: RoutableMessage::Output { agent_id, data },
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
                                            log!("server: raw stream {} cancelled", stream_id);
                                        }
                                    }
                                    cleanup_stream(&stream_user_state, agent_id, stream_id).await;
                                    log!("server: output stream ended");
                                });
                            }

                            Ok(())
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Message::Routable {
                                    src: reply_src,
                                    dst: reply_dst,
                                    message: RoutableMessage::SubscribeResult {
                                        agent_id,
                                        success: false,
                                        error: Some(ProtocolError::ServerError(e.to_string())),
                                    },
                                })
                                .await;
                            Ok(())
                        }
                    }
                }
            }
        }

        RoutableMessage::InputBytes { agent_id, data } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id) {
                let _ = session.send_input(data).await;
            }
            Ok(())
        }

        RoutableMessage::SubmitInput { agent_id, data } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id) {
                let _ = session.send_input(data).await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let _ = session.send_input(vec![b'\r']).await;
            }
            Ok(())
        }

        RoutableMessage::PermissionRequestResponse { agent_id, response } => {
            let us = ctx.user_state.read().await;
            if let Some(session) = us.agents.get(&agent_id) {
                let keystroke = super::routing::permission_response_keystroke(&response);
                log!(
                    "server: sending permission response {:?} to agent {} (keystroke: {:?})",
                    response,
                    agent_id,
                    keystroke
                );
                let _ = session.send_input(keystroke.to_vec()).await;
            }
            Ok(())
        }

        // NoRouteFound arrived at destination — cancel streams whose route passes
        // through the dead link. The dead_route's first_hop is the failed next_hop
        // (pushed last via push_front during traversal).
        RoutableMessage::Error(ProtocolError::NoRouteFound(ref dead_route)) => {
            if let Some(dead_hop) = dead_route.first_hop() {
                let mut us = ctx.user_state.write().await;
                let cancelled =
                    cancel_streams_matching(&mut us, |entry| entry.dst.contains_link(dead_hop));
                if cancelled > 0 {
                    log!(
                        "server: cancelled {} streams targeting dead hop {}",
                        cancelled,
                        dead_hop
                    );
                }
            }
            Ok(())
        }

        // Response messages that arrived at their destination (empty dst)
        RoutableMessage::SubscribeResult { .. }
        | RoutableMessage::Output { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::AgentEnded { .. }
        | RoutableMessage::Error(_) => {
            log!(
                "server: routable response arrived with empty dst, dropping: {:?}",
                message
            );
            Ok(())
        }
    }
}

async fn handle_local(
    tx: &mpsc::Sender<Message>,
    message: LocalMessage,
    ctx: &ConnectionContext,
) -> Result<()> {
    match message {
        LocalMessage::Shutdown => {
            log!("server: shutdown requested by {}", ctx.link_name);
            shutdown_server(&ctx.user_state).await;

            let _ = tx
                .send(Message::Local(LocalMessage::Error {
                    message: "Server shutting down".to_string(),
                }))
                .await;

            let socket_path = {
                let state = ctx.state.read().await;
                state.config.socket_path.clone()
            };
            let _ = std::fs::remove_file(socket_path);
            log!("server: exiting");
            std::process::exit(0);
        }

        LocalMessage::ConnectToServer { address } => {
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
                Ok(()) => Message::Local(LocalMessage::ConnectToServerResult {
                    success: true,
                    error: None,
                }),
                Err(e) => Message::Local(LocalMessage::ConnectToServerResult {
                    success: false,
                    error: Some(ProtocolError::ServerError(e.to_string())),
                }),
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        LocalMessage::Debug => {
            let (cloud_mode, use_cloud_mode, config) = {
                let state = ctx.state.read().await;
                let use_cloud_mode = State::load(&state.config.state_path)
                    .map(|s| s.cloud.use_cloud_mode == Some(true))
                    .unwrap_or(false);
                (state.cloud_mode, use_cloud_mode, state.config.clone())
            };
            let info = {
                let us = ctx.user_state.read().await;
                ServerDebugInfo {
                    is_cloud_server: cloud_mode,
                    use_cloud_mode,
                    agent_count: us.agents.len(),
                    remote_agent_count: us.registry.count_remote(),
                    route_count: us.routes.len(),
                    routes: us.routes.keys().cloned().collect(),
                    peer_links: us.peer_links.iter().cloned().collect(),
                    config,
                }
            };
            let _ = tx
                .send(Message::Local(LocalMessage::DebugResult { info }))
                .await;
            Ok(())
        }

        LocalMessage::ListAgents => {
            let agents = {
                let us = ctx.user_state.read().await;
                us.registry.list_all()
            };
            let _ = tx
                .send(Message::Local(LocalMessage::ListAgentsResult { agents }))
                .await;
            Ok(())
        }

        LocalMessage::CreateAgent(req) => {
            let result = create_agent(&ctx.user_state, &ctx.event_tx, req, ctx.user_id).await;

            let response = match result {
                Ok(()) => Message::Local(LocalMessage::CreateAgentResult {
                    success: true,
                    error: None,
                }),
                Err(e) => Message::Local(LocalMessage::CreateAgentResult {
                    success: false,
                    error: Some(ProtocolError::ServerError(e.to_string())),
                }),
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        LocalMessage::HookEvent { hook } => {
            log!("server: HookEvent from {}: {:?}", ctx.link_name, hook);

            let result = match &hook {
                Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                    let session = {
                        let us = ctx.user_state.read().await;
                        us.agents.get(&session_start.session_id).cloned()
                    };
                    if let Some(session) = session {
                        log!(
                            "server: linking transcript to agent {}",
                            session_start.session_id
                        );
                        session
                            .link_transcript(PathBuf::from(&session_start.transcript_path))
                            .await;
                        Ok(())
                    } else {
                        log!(
                            "server: no agent with session_id {}",
                            session_start.session_id
                        );
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
                        log!(
                            "server: permission request for agent {}: {:?}",
                            perm_req.session_id,
                            perm_req.tool
                        );
                        session
                            .write_log(crate::structured_log::StructuredLog::PermissionRequest {
                                tool: perm_req.tool.clone().into(),
                            })
                            .await;
                        Ok(())
                    } else {
                        log!("server: no agent with session_id {}", perm_req.session_id);
                        Err(ProtocolError::ServerError(format!(
                            "No agent found with session_id: {}",
                            perm_req.session_id
                        )))
                    }
                }
            };

            let response = match result {
                Ok(()) => Message::Local(LocalMessage::HookEventResult {
                    success: true,
                    error: None,
                }),
                Err(e) => Message::Local(LocalMessage::HookEventResult {
                    success: false,
                    error: Some(e),
                }),
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        // In-band re-authentication for token refresh on established connections.
        // The peer sends Connect with the same link_name and a fresh token.
        LocalMessage::Connect {
            link_name, token, ..
        } => {
            if link_name != ctx.link_name {
                let _ = tx
                    .send(Message::Local(LocalMessage::ConnectResponse {
                        success: false,
                        error: Some(ProtocolError::ServerError(
                            "Link name mismatch on re-auth".to_string(),
                        )),
                    }))
                    .await;
                return Ok(());
            }

            let is_cloud = {
                let state = ctx.state.read().await;
                state.cloud_mode
            };

            if is_cloud {
                let (validator, host, tcp_port) = {
                    let state = ctx.state.read().await;
                    let validator = state
                        .jwt_validator
                        .clone()
                        .expect("cloud_mode requires jwt_validator");
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
                            .send(Message::Local(LocalMessage::ConnectResponse {
                                success: false,
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await;
                        return Ok(());
                    }
                };

                match validator.validate(&token, &host, tcp_port).await {
                    Ok(claims) => {
                        let token_user_id = claims.sub.parse::<uuid::Uuid>().map_err(|_| {
                            log!("server: re-auth invalid user_id in token: {}", claims.sub);
                            AmuxError::InvalidCredentials
                        })?;
                        if token_user_id != ctx.user_id {
                            log!(
                                "server: re-auth user_id mismatch on {}: token={} connection={}",
                                ctx.link_name,
                                token_user_id,
                                ctx.user_id
                            );
                            let _ = tx
                                .send(Message::Local(LocalMessage::ConnectResponse {
                                    success: false,
                                    error: Some(ProtocolError::InvalidCredentials),
                                }))
                                .await;
                            return Err(AmuxError::InvalidCredentials);
                        }
                        log!(
                            "server: re-authenticated {} (user {})",
                            ctx.link_name,
                            ctx.user_id
                        );
                    }
                    Err(e) => {
                        log!("server: re-auth token validation failed: {}", e);
                        let _ = tx
                            .send(Message::Local(LocalMessage::ConnectResponse {
                                success: false,
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await;
                        return Ok(());
                    }
                }
            }

            let _ = tx
                .send(Message::Local(LocalMessage::ConnectResponse {
                    success: true,
                    error: None,
                }))
                .await;
            Ok(())
        }

        LocalMessage::AnnounceAgent {
            agent_id,
            alias,
            command,
            working_dir,
            route: received_route,
        } => {
            let mut us = ctx.user_state.write().await;

            // Local agent takes precedence — skip if we own this agent
            if us.agents.contains_key(&agent_id) {
                log!(
                    "server: ignoring AnnounceAgent for local agent {}",
                    agent_id
                );
                return Ok(());
            }

            // Compute our route: prepend the link this came from
            let mut our_route = received_route.clone();
            our_route.push(&ctx.link_name);

            let info = AgentInfo {
                agent_id,
                alias: alias.clone(),
                command: command.clone(),
                working_dir: working_dir.clone(),
                route: None,
            };

            us.registry
                .register_remote(info, our_route.clone(), ctx.link_name.clone());

            log!(
                "server: stored remote agent {} (alias={:?}) via {} route={}",
                agent_id,
                alias,
                ctx.link_name,
                our_route
            );

            // Propagate to other peers with our stored route
            broadcast_to_peers(
                &mut us,
                &LocalMessage::AnnounceAgent {
                    agent_id,
                    alias,
                    command,
                    working_dir,
                    route: our_route,
                },
                Some(&ctx.link_name),
            );

            Ok(())
        }

        LocalMessage::WithdrawAgent { agent_id } => {
            let mut us = ctx.user_state.write().await;

            // Only remove if the stored link matches the sender
            let should_remove = us.registry.get(&agent_id).is_some_and(
                |e| matches!(&e.kind, AgentKind::Remote { link, .. } if link == &ctx.link_name),
            );

            if should_remove {
                us.registry.remove(&agent_id);
                log!(
                    "server: withdrew remote agent {} (from {})",
                    agent_id,
                    ctx.link_name
                );

                // Propagate to other peers
                broadcast_to_peers(
                    &mut us,
                    &LocalMessage::WithdrawAgent { agent_id },
                    Some(&ctx.link_name),
                );
            } else {
                log!(
                    "server: ignoring WithdrawAgent {} (link mismatch or unknown)",
                    agent_id
                );
            }

            Ok(())
        }

        LocalMessage::ResolveAgent { identifier } => {
            let us = ctx.user_state.read().await;
            let result = us.registry.resolve(&identifier);
            let _ = tx
                .send(Message::Local(LocalMessage::ResolveAgentResult {
                    error: if result.is_none() {
                        Some(ProtocolError::ServerError(format!(
                            "Agent not found: {}",
                            identifier
                        )))
                    } else {
                        None
                    },
                    agent: result,
                }))
                .await;
            Ok(())
        }

        _ => {
            let _ = tx
                .send(Message::Local(LocalMessage::Error {
                    message: "Unexpected message".to_string(),
                }))
                .await;
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
    use tokio::sync::{mpsc, RwLock};
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
    async fn missing_route_sends_error_back() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        let agent_id = Uuid::new_v4();

        // Route through "nonexistent" link
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("agent1");
        dst.push("nonexistent");

        let msg = RoutableMessage::Subscribe {
            agent_id,
            rows: 24,
            cols: 80,
            mode: SubscribeMode::Raw,
        };

        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        // Give the collector task a moment
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { message, .. } = &msgs[0] else {
            panic!("expected Routable message");
        };
        let RoutableMessage::Error(ProtocolError::NoRouteFound(_)) = message else {
            panic!("expected Error(NoRouteFound), got {:?}", message);
        };
    }

    #[tokio::test]
    async fn closed_channel_cleans_stale_route_and_sends_error() {
        let (state, user_state) = test_state().await;

        // Create a channel and immediately drop the receiver to close it
        let (route_tx, rx) = mpsc::channel::<Message>(1);
        drop(rx);

        {
            let mut us = user_state.write().await;
            us.routes.insert("stale-link".to_string(), route_tx);
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        let agent_id = Uuid::new_v4();

        let src = Route::from_link("origin");
        let mut dst = Route::from_link("agent1");
        dst.push("stale-link");

        let msg = RoutableMessage::InputBytes {
            agent_id,
            data: vec![1, 2, 3],
        };

        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        // Route should be removed
        {
            let us = user_state.read().await;
            assert!(!us.routes.contains_key("stale-link"));
        }

        // Give the collector task a moment
        tokio::task::yield_now().await;

        // Error should be sent back
        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { message, .. } = &msgs[0] else {
            panic!("expected Routable message");
        };
        let RoutableMessage::Error(ProtocolError::NoRouteFound(_)) = message else {
            panic!("expected Error(NoRouteFound), got {:?}", message);
        };
    }

    #[tokio::test]
    async fn failed_error_message_not_amplified() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        // Try to forward an Error message through a nonexistent route
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");

        let msg = RoutableMessage::Error(ProtocolError::NoRouteFound(Route::from_link("x")));

        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        // No error should be sent back (amplification prevention)
        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "expected no messages, got {:?}", *msgs);
    }

    #[tokio::test]
    async fn stream_message_first_failure_sends_error() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        let agent_id = Uuid::new_v4();

        // First Output failure should send NoRouteFound back
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");

        let msg = RoutableMessage::Output {
            agent_id,
            data: vec![1, 2, 3],
        };

        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1, "first failure should send error");
        let Message::Routable { message, .. } = &msgs[0] else {
            panic!("expected Routable message");
        };
        assert!(matches!(
            message,
            RoutableMessage::Error(ProtocolError::NoRouteFound(_))
        ));
    }

    #[tokio::test]
    async fn stream_message_second_failure_suppressed() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        let agent_id = Uuid::new_v4();

        // First failure
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");
        let msg = RoutableMessage::Output {
            agent_id,
            data: vec![1],
        };
        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        // Second failure — same route
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");
        let msg = RoutableMessage::Output {
            agent_id,
            data: vec![2],
        };
        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        // Only one error should have been sent (the first)
        let msgs = written.lock().await;
        assert_eq!(
            msgs.len(),
            1,
            "second failure should be suppressed, got {:?}",
            *msgs
        );
    }

    #[tokio::test]
    async fn no_route_found_includes_traversed_path() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();
        let mut dead_routes = HashSet::new();

        let agent_id = Uuid::new_v4();

        // Message traversed "origin" before arriving here, now fails at "nonexistent"
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("final-dest");
        dst.push("nonexistent");

        let msg = RoutableMessage::Subscribe {
            agent_id,
            rows: 24,
            cols: 80,
            mode: SubscribeMode::Raw,
        };

        handle_routable(&tx, src, dst, msg, &ctx, &mut dead_routes)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Routable { message, .. } = &msgs[0] else {
            panic!("expected Routable message");
        };
        let RoutableMessage::Error(ProtocolError::NoRouteFound(route)) = message else {
            panic!("expected Error(NoRouteFound), got {:?}", message);
        };
        // Traversed path should include the failed hop ("nonexistent") and the prior path ("origin")
        let route_str = format!("{}", route);
        assert!(
            route_str.contains("nonexistent"),
            "route should contain failed hop, got: {}",
            route_str
        );
        assert!(
            route_str.contains("origin"),
            "route should contain prior path, got: {}",
            route_str
        );
    }

    #[tokio::test]
    async fn connect_reauth_matching_link_succeeds() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        // Re-auth with matching link name (non-cloud mode = no token needed)
        let msg = LocalMessage::Connect {
            link_name: "test-link".to_string(),
            token: None,
            version: crate::message::PROTOCOL_VERSION,
        };

        handle_local(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Local(LocalMessage::ConnectResponse { success, error }) = &msgs[0] else {
            panic!("expected ConnectResponse, got {:?}", msgs[0]);
        };
        assert!(success);
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn connect_reauth_mismatched_link_rejected() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        // Re-auth with wrong link name
        let msg = LocalMessage::Connect {
            link_name: "wrong-link".to_string(),
            token: None,
            version: crate::message::PROTOCOL_VERSION,
        };

        handle_local(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Local(LocalMessage::ConnectResponse { success, error, .. }) = &msgs[0] else {
            panic!("expected ConnectResponse, got {:?}", msgs[0]);
        };
        assert!(!success);
        assert!(error.is_some());
    }

    #[tokio::test]
    async fn announce_agent_stores_in_registry() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = LocalMessage::AnnounceAgent {
            agent_id,
            alias: Some("remote-test".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
        };

        handle_local(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(us.registry.contains(&agent_id));
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.info.alias, Some("remote-test".to_string()));
        match &entry.kind {
            AgentKind::Remote { route, link } => {
                assert_eq!(link, "test-link");
                let mut route = route.clone();
                assert_eq!(route.pop(), Some("test-link".to_string()));
                assert_eq!(route.pop(), None);
            }
            _ => panic!("expected Remote agent kind"),
        }
    }

    #[tokio::test]
    async fn announce_agent_with_route_prepends_link() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let msg = LocalMessage::AnnounceAgent {
            agent_id,
            alias: None,
            command: "bash".to_string(),
            working_dir: PathBuf::from("/home"),
            route: Route::from_link("host-a"),
        };

        handle_local(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        match &entry.kind {
            AgentKind::Remote { route, .. } => {
                let mut route = route.clone();
                // Should be test-link.host-a (test-link prepended)
                assert_eq!(route.pop(), Some("test-link".to_string()));
                assert_eq!(route.pop(), Some("host-a".to_string()));
                assert_eq!(route.pop(), None);
            }
            _ => panic!("expected Remote agent kind"),
        }
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
                alias: Some("local".to_string()),
                agent_type: crate::message::AgentType::TestAgent("/bin/cat".to_string()),
                working_dir: PathBuf::from("/tmp"),
                rows: 24,
                cols: 80,
            };
            let session =
                crate::session::LocalAgentSession::new(&req, event_tx, LOCAL_USER_ID).unwrap();
            let info = session.to_agent_info();
            us.agents.insert(agent_id, Arc::new(session));
            us.registry.register_local(info).unwrap();
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        // Try to announce same agent_id from remote
        let msg = LocalMessage::AnnounceAgent {
            agent_id,
            alias: Some("remote".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/remote"),
            route: Route::empty(),
        };

        handle_local(&tx, msg, &ctx).await.unwrap();

        // Should still be local (not overwritten)
        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert!(matches!(entry.kind, AgentKind::Local));
    }

    #[tokio::test]
    async fn withdraw_agent_removes_matching_link() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.registry.register_remote(
                AgentInfo {
                    agent_id,
                    alias: None,
                    command: "bash".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: None,
                },
                Route::from_link("test-link"),
                "test-link".to_string(),
            );
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        let msg = LocalMessage::WithdrawAgent { agent_id };
        handle_local(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        assert!(!us.registry.contains(&agent_id));
    }

    #[tokio::test]
    async fn withdraw_agent_ignores_link_mismatch() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.registry.register_remote(
                AgentInfo {
                    agent_id,
                    alias: None,
                    command: "bash".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: None,
                },
                Route::from_link("other-link"),
                "other-link".to_string(),
            );
        }

        let ctx = test_ctx(state, user_state.clone());
        let (tx, _written) = mock_tx();

        // Withdraw from "test-link" but agent is stored from "other-link"
        let msg = LocalMessage::WithdrawAgent { agent_id };
        handle_local(&tx, msg, &ctx).await.unwrap();

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
        let msg = LocalMessage::AnnounceAgent {
            agent_id,
            alias: Some("first".to_string()),
            command: "bash".to_string(),
            working_dir: PathBuf::from("/first"),
            route: Route::empty(),
        };
        handle_local(&tx, msg, &ctx).await.unwrap();

        // Second announce with same agent_id
        let msg = LocalMessage::AnnounceAgent {
            agent_id,
            alias: Some("second".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/second"),
            route: Route::empty(),
        };
        handle_local(&tx, msg, &ctx).await.unwrap();

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.info.alias, Some("second".to_string()));
        assert_eq!(entry.info.working_dir, PathBuf::from("/second"));
    }

    #[tokio::test]
    async fn resolve_agent_by_alias() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        {
            let mut us = user_state.write().await;
            us.registry.register_remote(
                AgentInfo {
                    agent_id,
                    alias: Some("my-agent".to_string()),
                    command: "claude".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    route: None,
                },
                Route::from_link("peer-a"),
                "peer-a".to_string(),
            );
        }

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = LocalMessage::ResolveAgent {
            identifier: "my-agent".to_string(),
        };
        handle_local(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Local(LocalMessage::ResolveAgentResult { agent, error }) = &msgs[0] else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_some());
        assert!(error.is_none());
        assert_eq!(agent.as_ref().unwrap().agent_id, agent_id);
    }

    #[tokio::test]
    async fn resolve_agent_not_found() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let msg = LocalMessage::ResolveAgent {
            identifier: "nonexistent".to_string(),
        };
        handle_local(&tx, msg, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Local(LocalMessage::ResolveAgentResult { agent, error }) = &msgs[0] else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_none());
        assert!(error.is_some());
    }
}
