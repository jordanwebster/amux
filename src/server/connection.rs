use super::accept::tcp_connect;
use super::routing::{
    connection_tx, create_agent, handle_subscribe, resolve_agent, shutdown_server,
};
use super::ServerState;
use crate::cloud::TokenRefreshState;
use crate::error::{AmuxError, Result};
use crate::message::{
    ClaudeHook, ConfigDebugInfo, Hook, LocalMessage, Message, ProtocolError, RoutableMessage,
    ServerDebugInfo, SubscribeMode,
};
use crate::route::Route;
use crate::session::SessionEvent;
use crate::state::State;
use crate::transport::Transport;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Context for connection handlers.
pub(super) struct ConnectionContext {
    pub(super) state: Arc<RwLock<ServerState>>,
    pub(super) event_tx: mpsc::Sender<SessionEvent>,
    pub(super) link_name: String,
}

/// Shared connection loop for Unix/TCP/WebSocket transports.
pub(super) async fn connection_loop<T: Transport>(
    transport: &mut T,
    outgoing_rx: mpsc::Receiver<Message>,
    ctx: ConnectionContext,
) -> Result<()> {
    connection_loop_with_refresh(transport, outgoing_rx, ctx, None).await
}

pub(super) async fn connection_loop_with_refresh<T: Transport>(
    transport: &mut T,
    mut outgoing_rx: mpsc::Receiver<Message>,
    ctx: ConnectionContext,
    mut token_refresh: Option<TokenRefreshState>,
) -> Result<()> {
    let mut refresh_deadline = token_refresh.as_ref().map(|t| t.refresh_deadline());

    loop {
        tokio::select! {
            result = transport.read_message() => {
                let msg = match result {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };

                handle_message(transport, msg, &ctx).await?;
            }

            Some(msg) = outgoing_rx.recv() => {
                log!("server: routing message to {}: {:?}", ctx.link_name, msg);
                if transport.write_message(&msg).await.is_err() {
                    return Ok(());
                }
            }

            _ = maybe_sleep_until(refresh_deadline) => {
                if let Some(ref mut rs) = token_refresh {
                    log!("cloud: refreshing token");
                    match rs.refresh_and_reconnect(transport).await {
                        Ok(()) => {
                            refresh_deadline = Some(rs.refresh_deadline());
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
        }
    }
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

pub(super) async fn handle_message<T: Transport>(
    transport: &mut T,
    msg: Message,
    ctx: &ConnectionContext,
) -> Result<()> {
    log!("server: {} received {:?}", ctx.link_name, msg);

    match msg {
        Message::Routable { src, dst, message } => {
            handle_routable(transport, src, dst, message, ctx).await
        }
        Message::Local(local) => handle_local(transport, local, ctx).await,
    }
}

async fn handle_routable<T: Transport>(
    transport: &mut T,
    mut src: Route,
    mut dst: Route,
    message: RoutableMessage,
    ctx: &ConnectionContext,
) -> Result<()> {
    // Check if this message needs forwarding
    if let Some(next_hop) = dst.pop() {
        // Save original src BEFORE mutation — used for error replies
        let original_src = src.clone();
        src.push(&next_hop);

        let route_tx = {
            let state = ctx.state.read().await;
            state.routes.get(&next_hop).cloned()
        };

        // Try to forward; on failure, get the failed message back
        let failed_msg = match route_tx {
            Some(route_tx) => {
                match route_tx.send(Message::Routable { src, dst, message }).await {
                    Ok(()) => None,
                    Err(send_error) => {
                        // Channel closed — conditionally clean up stale route
                        {
                            let mut state = ctx.state.write().await;
                            if let Some(current_tx) = state.routes.get(&next_hop) {
                                if current_tx.is_closed() {
                                    state.routes.remove(&next_hop);
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
        // Suppress errors for: Error (amplification), Output/StructuredOutput (high-frequency
        // stream data — errors would cause churn without triggering teardown).
        if let Some(failed_msg) = failed_msg {
            match failed_msg {
                RoutableMessage::Error(_) => {
                    log!("server: dropping failed routable error to avoid amplification");
                }
                RoutableMessage::Output { .. } | RoutableMessage::StructuredOutput { .. } => {
                    log!("server: dropping failed stream message to {}", next_hop);
                }
                _ => {
                    // Build traversed path: original_src + the failed hop
                    let mut traversed = original_src.clone();
                    traversed.push(&next_hop);

                    if let Some((reply_src, reply_dst)) = Route::reply(original_src) {
                        transport
                            .write_message(&Message::Routable {
                                src: reply_src,
                                dst: reply_dst,
                                message: RoutableMessage::Error(ProtocolError::NoRouteFound(
                                    traversed,
                                )),
                            })
                            .await?;
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
                        let state = ctx.state.read().await;
                        resolve_agent(&state.agents, &agent_id).cloned()
                    };

                    let Some(session) = session else {
                        transport
                            .write_message(&Message::Routable {
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
                            .await?;
                        return Ok(());
                    };

                    session.resize(rows, cols).await?;
                    let log_reader = session.subscribe_logs().await;

                    let Some(mut reader) = log_reader else {
                        transport
                            .write_message(&Message::Routable {
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
                            .await?;
                        return Ok(());
                    };

                    transport
                        .write_message(&Message::Routable {
                            src: reply_src.clone(),
                            dst: reply_dst.clone(),
                            message: RoutableMessage::SubscribeResult {
                                agent_id: agent_id.clone(),
                                success: true,
                                error: None,
                            },
                        })
                        .await?;

                    log!(
                        "server: {} subscribed to agent {} (structured)",
                        ctx.link_name,
                        agent_id
                    );

                    let outgoing_tx = connection_tx(&ctx.state, &ctx.link_name).await;
                    if let Some(tx) = outgoing_tx {
                        let agent_id_clone = agent_id.clone();
                        tokio::spawn(async move {
                            while let Some(entry) = reader.read().await {
                                if tx
                                    .send(Message::Routable {
                                        src: reply_src.clone(),
                                        dst: reply_dst.clone(),
                                        message: RoutableMessage::StructuredOutput {
                                            agent_id: agent_id_clone.clone(),
                                            entry,
                                        },
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            let _ = tx.send(Message::Local(LocalMessage::AgentEnded)).await;
                            log!("server: structured log stream ended");
                        });
                    }

                    Ok(())
                }
                SubscribeMode::Raw => {
                    let agent_id_str = agent_id.to_string();
                    let result = handle_subscribe(&ctx.state, &agent_id_str, rows, cols).await;

                    match result {
                        Ok(mut buffer_reader) => {
                            transport
                                .write_message(&Message::Routable {
                                    src: reply_src.clone(),
                                    dst: reply_dst.clone(),
                                    message: RoutableMessage::SubscribeResult {
                                        agent_id: agent_id.clone(),
                                        success: true,
                                        error: None,
                                    },
                                })
                                .await?;

                            log!(
                                "server: {} subscribed to agent {} (raw)",
                                ctx.link_name,
                                agent_id
                            );

                            let outgoing_tx = connection_tx(&ctx.state, &ctx.link_name).await;
                            if let Some(tx) = outgoing_tx {
                                let agent_id_clone = agent_id.clone();
                                tokio::spawn(async move {
                                    while let Some(data) = buffer_reader.read().await {
                                        if tx
                                            .send(Message::Routable {
                                                src: reply_src.clone(),
                                                dst: reply_dst.clone(),
                                                message: RoutableMessage::Output {
                                                    agent_id: agent_id_clone.clone(),
                                                    data,
                                                },
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    let _ = tx.send(Message::Local(LocalMessage::AgentEnded)).await;
                                    log!("server: output stream ended");
                                });
                            }

                            Ok(())
                        }
                        Err(e) => {
                            transport
                                .write_message(&Message::Routable {
                                    src: reply_src,
                                    dst: reply_dst,
                                    message: RoutableMessage::SubscribeResult {
                                        agent_id,
                                        success: false,
                                        error: Some(ProtocolError::ServerError(e.to_string())),
                                    },
                                })
                                .await?;
                            Ok(())
                        }
                    }
                }
            }
        }

        RoutableMessage::InputBytes { agent_id, data } => {
            let state = ctx.state.read().await;
            if let Some(session) = resolve_agent(&state.agents, &agent_id) {
                let _ = session.send_input(data).await;
            }
            Ok(())
        }

        RoutableMessage::SubmitInput { agent_id, data } => {
            let state = ctx.state.read().await;
            if let Some(session) = resolve_agent(&state.agents, &agent_id) {
                let _ = session.send_input(data).await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let _ = session.send_input(vec![b'\r']).await;
            }
            Ok(())
        }

        RoutableMessage::PermissionRequestResponse { agent_id, response } => {
            let state = ctx.state.read().await;
            if let Some(session) = resolve_agent(&state.agents, &agent_id) {
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

        // Response messages that arrived at their destination (empty dst)
        RoutableMessage::SubscribeResult { .. }
        | RoutableMessage::Output { .. }
        | RoutableMessage::StructuredOutput { .. }
        | RoutableMessage::Error(_) => {
            log!(
                "server: routable response arrived with empty dst, dropping: {:?}",
                message
            );
            Ok(())
        }
    }
}

async fn handle_local<T: Transport>(
    transport: &mut T,
    message: LocalMessage,
    ctx: &ConnectionContext,
) -> Result<()> {
    match message {
        LocalMessage::Shutdown => {
            log!("server: shutdown requested by {}", ctx.link_name);
            shutdown_server(&ctx.state).await;

            let _ = transport
                .write_message(&Message::Local(LocalMessage::Error {
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
            transport.write_message(&response).await?;
            Ok(())
        }

        LocalMessage::Debug => {
            let state = ctx.state.read().await;
            let use_cloud_mode = State::load(&state.config.state_path)
                .map(|s| s.cloud.use_cloud_mode == Some(true))
                .unwrap_or(false);
            let info = ServerDebugInfo {
                is_cloud_server: state.cloud_mode,
                use_cloud_mode,
                agent_count: state.agents.len(),
                route_count: state.routes.len(),
                routes: state.routes.keys().cloned().collect(),
                config: ConfigDebugInfo::from(&state.config),
            };
            transport
                .write_message(&Message::Local(LocalMessage::DebugResult { info }))
                .await?;
            Ok(())
        }

        LocalMessage::ListAgents => {
            let agents = {
                let state = ctx.state.read().await;
                state
                    .agents
                    .values()
                    .map(|s| s.to_agent_info())
                    .collect::<Vec<_>>()
            };
            transport
                .write_message(&Message::Local(LocalMessage::ListAgentsResult { agents }))
                .await?;
            Ok(())
        }

        LocalMessage::CreateAgent(req) => {
            let result = create_agent(&ctx.state, &ctx.event_tx, req).await;

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
            transport.write_message(&response).await?;
            Ok(())
        }

        LocalMessage::HookEvent { hook } => {
            log!("server: HookEvent from {}: {:?}", ctx.link_name, hook);

            let result = match &hook {
                Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                    let state = ctx.state.read().await;
                    if let Some(session) = state.agents.get(&session_start.session_id) {
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
                            "server: no agent with session_id {}, agents: {:?}",
                            session_start.session_id,
                            state.agents.keys().collect::<Vec<_>>()
                        );
                        Err(ProtocolError::ServerError(format!(
                            "No agent found with session_id: {}",
                            session_start.session_id
                        )))
                    }
                }
                Hook::Claude(ClaudeHook::PermissionRequest(perm_req)) => {
                    let state = ctx.state.read().await;
                    if let Some(session) = state.agents.get(&perm_req.session_id) {
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
                        log!(
                            "server: no agent with session_id {}, agents: {:?}",
                            perm_req.session_id,
                            state.agents.keys().collect::<Vec<_>>()
                        );
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
            transport.write_message(&response).await?;
            Ok(())
        }

        // In-band re-authentication for token refresh on established connections.
        // The peer sends Connect with the same link_name and a fresh token.
        LocalMessage::Connect { link_name, token } => {
            if link_name != ctx.link_name {
                transport
                    .write_message(&Message::Local(LocalMessage::ConnectResponse {
                        success: false,
                        error: Some(ProtocolError::ServerError(
                            "Link name mismatch on re-auth".to_string(),
                        )),
                    }))
                    .await?;
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
                        transport
                            .write_message(&Message::Local(LocalMessage::ConnectResponse {
                                success: false,
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await?;
                        return Ok(());
                    }
                };

                match validator.validate(&token, &host, tcp_port).await {
                    Ok(claims) => {
                        log!(
                            "server: re-authenticated {} (user {})",
                            ctx.link_name,
                            claims.sub
                        );
                    }
                    Err(e) => {
                        log!("server: re-auth token validation failed: {}", e);
                        transport
                            .write_message(&Message::Local(LocalMessage::ConnectResponse {
                                success: false,
                                error: Some(ProtocolError::InvalidCredentials),
                            }))
                            .await?;
                        return Ok(());
                    }
                }
            }

            transport
                .write_message(&Message::Local(LocalMessage::ConnectResponse {
                    success: true,
                    error: None,
                }))
                .await?;
            Ok(())
        }

        // AgentEnded is sent by stream tasks to the subscribing link's outgoing channel.
        // On direct clients this signals session end. On peer links it's harmless — the peer
        // will propagate end-of-session to its own subscribers independently.
        LocalMessage::AgentEnded => {
            log!(
                "server: received AgentEnded on link {} (no-op on peer)",
                ctx.link_name
            );
            Ok(())
        }

        _ => {
            transport
                .write_message(&Message::Local(LocalMessage::Error {
                    message: "Unexpected message".to_string(),
                }))
                .await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex, RwLock};

    /// Mock transport that captures written messages
    struct MockTransport {
        written: Arc<Mutex<Vec<Message>>>,
    }

    impl MockTransport {
        fn new() -> (Self, Arc<Mutex<Vec<Message>>>) {
            let written = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    written: written.clone(),
                },
                written,
            )
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn read_message(&mut self) -> crate::error::Result<Message> {
            Err(AmuxError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mock",
            )))
        }

        async fn write_message(&mut self, msg: &Message) -> crate::error::Result<()> {
            self.written.lock().await.push(msg.clone());
            Ok(())
        }
    }

    fn test_ctx(state: Arc<RwLock<ServerState>>) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            event_tx,
            link_name: "test-link".to_string(),
        }
    }

    fn test_state() -> Arc<RwLock<ServerState>> {
        Arc::new(RwLock::new(ServerState::new(Config::default())))
    }

    #[tokio::test]
    async fn missing_route_sends_error_back() {
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Route through "nonexistent" link
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("agent1");
        dst.push("nonexistent");

        let msg = RoutableMessage::Subscribe {
            agent_id: "some-agent".to_string(),
            rows: 24,
            cols: 80,
            mode: SubscribeMode::Raw,
        };

        handle_routable(&mut transport, src, dst, msg, &ctx)
            .await
            .unwrap();

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
        let state = test_state();

        // Create a channel and immediately drop the receiver to close it
        let (tx, rx) = mpsc::channel::<Message>(1);
        drop(rx);

        {
            let mut s = state.write().await;
            s.routes.insert("stale-link".to_string(), tx);
        }

        let ctx = test_ctx(state.clone());
        let (mut transport, written) = MockTransport::new();

        let src = Route::from_link("origin");
        let mut dst = Route::from_link("agent1");
        dst.push("stale-link");

        let msg = RoutableMessage::InputBytes {
            agent_id: "some-agent".to_string(),
            data: vec![1, 2, 3],
        };

        handle_routable(&mut transport, src, dst, msg, &ctx)
            .await
            .unwrap();

        // Route should be removed
        {
            let s = state.read().await;
            assert!(!s.routes.contains_key("stale-link"));
        }

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
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Try to forward an Error message through a nonexistent route
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");

        let msg = RoutableMessage::Error(ProtocolError::NoRouteFound(Route::from_link("x")));

        handle_routable(&mut transport, src, dst, msg, &ctx)
            .await
            .unwrap();

        // No error should be sent back (amplification prevention)
        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "expected no messages, got {:?}", *msgs);
    }

    #[tokio::test]
    async fn stream_message_forwarding_failure_suppressed() {
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Try to forward Output through nonexistent route
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("somewhere");
        dst.push("nonexistent");

        let msg = RoutableMessage::Output {
            agent_id: "some-agent".to_string(),
            data: vec![1, 2, 3],
        };

        handle_routable(&mut transport, src, dst, msg, &ctx)
            .await
            .unwrap();

        // No error should be sent back (stream message suppression)
        let msgs = written.lock().await;
        assert!(msgs.is_empty(), "expected no messages, got {:?}", *msgs);
    }

    #[tokio::test]
    async fn no_route_found_includes_traversed_path() {
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Message traversed "origin" before arriving here, now fails at "nonexistent"
        let src = Route::from_link("origin");
        let mut dst = Route::from_link("final-dest");
        dst.push("nonexistent");

        let msg = RoutableMessage::Subscribe {
            agent_id: "some-agent".to_string(),
            rows: 24,
            cols: 80,
            mode: SubscribeMode::Raw,
        };

        handle_routable(&mut transport, src, dst, msg, &ctx)
            .await
            .unwrap();

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
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Re-auth with matching link name (non-cloud mode = no token needed)
        let msg = LocalMessage::Connect {
            link_name: "test-link".to_string(),
            token: None,
        };

        handle_local(&mut transport, msg, &ctx).await.unwrap();

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
        let state = test_state();
        let ctx = test_ctx(state);
        let (mut transport, written) = MockTransport::new();

        // Re-auth with wrong link name
        let msg = LocalMessage::Connect {
            link_name: "wrong-link".to_string(),
            token: None,
        };

        handle_local(&mut transport, msg, &ctx).await.unwrap();

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Local(LocalMessage::ConnectResponse { success, error, .. }) = &msgs[0] else {
            panic!("expected ConnectResponse, got {:?}", msgs[0]);
        };
        assert!(!success);
        assert!(error.is_some());
    }
}
