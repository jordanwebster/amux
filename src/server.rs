use crate::buffer::MultiplexReader;
use crate::cloud::{CloudConnection, CloudError, TokenRefreshState};
use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::jwt::JwtValidator;
use crate::message::{
    ClaudeHook, CreateAgentRequest, Hook, Message, PermissionResponse, ProtocolError,
};
use crate::route::{generate_server_link, Route};
use crate::session::{LocalAgentSession, SessionEvent};
use crate::state::State;
use crate::transport::{
    create_tls_acceptor, TcpTransport, Transport, UnixTransport, WebSocketTransport,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_async;
use uuid::Uuid;

/// Server state shared across connection handlers
struct ServerState {
    config: Config,
    /// Whether running in cloud mode (TLS + token auth required)
    cloud_mode: bool,
    agents: HashMap<Uuid, Arc<LocalAgentSession>>,
    /// Routes to other hosts. Each route is a channel sender for outgoing messages.
    /// The actual transport is owned by the connection handler task.
    routes: HashMap<String, mpsc::Sender<Message>>,
    /// JWT validator for cloud mode (validates incoming tokens)
    jwt_validator: Option<Arc<JwtValidator>>,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            cloud_mode: false,
            agents: HashMap::new(),
            routes: HashMap::new(),
            jwt_validator: None,
        }
    }
}

/// Context for Unix client connection handlers
struct UnixClientContext {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    /// The link name for this client connection
    link_name: String,
}

/// The amux server
pub struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
}

impl Server {
    /// Create a new server with custom config
    pub fn with_config(config: Config) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);
        Self {
            state: Arc::new(RwLock::new(ServerState::new(config))),
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// Run the server
    ///
    /// If `is_cloud_server` is true, the server runs as a cloud relay:
    /// - TCP connections use TLS
    /// - All connections require valid JWT tokens
    pub async fn run(&mut self, is_cloud_server: bool) -> Result<()> {
        let (socket_path, tcp_port, ws_port, cloud_url) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port,
                state.config.websocket_port,
                state.config.cloud_url.clone(),
            )
        };

        // Set cloud mode and create JWT validator if needed
        let tls_acceptor: Option<TlsAcceptor> = if is_cloud_server {
            let mut state = self.state.write().await;
            state.cloud_mode = true;
            state.jwt_validator = Some(Arc::new(JwtValidator::new(&cloud_url)));

            // Cloud mode requires TLS certificates via environment variables
            let cert_path = std::env::var("AMUX_TLS_CERT").map_err(|_| {
                AmuxError::Config(
                    "AMUX_TLS_CERT environment variable required for cloud mode".into(),
                )
            })?;
            let key_path = std::env::var("AMUX_TLS_KEY").map_err(|_| {
                AmuxError::Config(
                    "AMUX_TLS_KEY environment variable required for cloud mode".into(),
                )
            })?;

            let cert_pem = std::fs::read(&cert_path).map_err(|e| {
                AmuxError::Config(format!("Failed to read TLS cert from {}: {}", cert_path, e))
            })?;
            let key_pem = std::fs::read(&key_path).map_err(|e| {
                AmuxError::Config(format!("Failed to read TLS key from {}: {}", key_path, e))
            })?;

            let acceptor = create_tls_acceptor(&cert_pem, &key_pem)?;
            log!("server: TLS configured for cloud mode");
            Some(acceptor)
        } else {
            None
        };

        // Unix socket - always available (for CLI commands like list-agents, kill-server)
        let _ = std::fs::remove_file(&socket_path);
        let unix_listener = UnixListener::bind(&socket_path)?;
        log!("server: listening on {:?}", socket_path);

        let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_port));
        let tcp_listener = TcpListener::bind(tcp_addr).await?;
        if is_cloud_server {
            log!("server: listening on TLS TCP {}", tcp_addr);
        } else {
            log!("server: listening on TCP {}", tcp_addr);
        }

        let ws_addr = SocketAddr::from(([0, 0, 0, 0], ws_port));
        let ws_listener = TcpListener::bind(ws_addr).await?;
        log!("server: listening on WebSocket {}", ws_addr);

        let mut event_rx = self.event_rx.take().expect("run() called twice");

        // Task: Handle session lifecycle events
        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    SessionEvent::Ended(agent_id) => {
                        log!("server: session {} ended, removing", agent_id);
                        let mut state = state.write().await;
                        state.agents.remove(&agent_id);
                    }
                }
            }
        });

        // Auto-connect to cloud (local server only, not cloud server)
        if !is_cloud_server {
            let config = {
                let state = self.state.read().await;
                state.config.clone()
            };
            establish_cloud_connection(config, self.state.clone());
        }

        loop {
            tokio::select! {
                // Unix socket connection (local clients)
                result = unix_listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            log!("server: client connected");
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = unix_accept(stream, state, event_tx).await {
                                    log!("server: unix connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            log!("server: unix accept error: {}", e);
                            break;
                        }
                    }
                }
                // TCP connection (remote servers) - TLS in cloud mode, plain in local mode
                result = tcp_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            // Set nodelay before any handshake (TLS or protocol)
                            if let Err(e) = stream.set_nodelay(true) {
                                log!("server: failed to set TCP_NODELAY: {}", e);
                            }

                            let state = self.state.clone();
                            if let Some(ref acceptor) = tls_acceptor {
                                // Cloud server: TLS with token validation
                                let acceptor = acceptor.clone();
                                tokio::spawn(async move {
                                    match acceptor.accept(stream).await {
                                        Ok(tls_stream) => {
                                            let transport = TcpTransport::new(tls_stream);
                                            if let Err(e) = tcp_accept(transport, state, true).await {
                                                log!("server: tls tcp connection error: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            log!("server: tls handshake error from {}: {}", addr, e);
                                        }
                                    }
                                });
                            } else {
                                // Local server: plain TCP
                                tokio::spawn(async move {
                                    let transport = TcpTransport::new(stream);
                                    if let Err(e) = tcp_accept(transport, state, false).await {
                                        log!("server: tcp connection error: {}", e);
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            log!("server: tcp accept error: {}", e);
                            break;
                        }
                    }
                }
                // WebSocket connection (rich clients)
                result = ws_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            log!("server: websocket client connected from {}", addr);
                            let state = self.state.clone();
                            tokio::spawn(async move {
                                if let Err(e) = websocket_accept(stream, state).await {
                                    log!("server: websocket connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            log!("server: websocket accept error: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Accept-side handshake: client proposes link name, we validate against routes.
/// Returns the accepted link name on success.
///
/// If `verify_token` is true (cloud server mode), the token in the Connect message
/// is validated via JWT. If validation fails, InvalidCredentials is returned.
async fn accept_handshake<T: Transport>(
    transport: &mut T,
    state: &Arc<RwLock<ServerState>>,
    verify_token: bool,
) -> Result<String> {
    for _attempt in 0..5 {
        let msg = transport.read_message().await?;
        let (proposed_link, token) = match msg {
            Message::Connect { link_name, token } => (link_name, token),
            _ => return Err(AmuxError::InvalidMessage),
        };

        // Validate token if required (cloud server mode)
        if verify_token {
            let (validator, host, tcp_port) = {
                let state = state.read().await;
                let validator = state
                    .jwt_validator
                    .clone()
                    .expect("verify_token=true requires jwt_validator");
                (
                    validator,
                    state.config.host_name.clone(),
                    state.config.tcp_port,
                )
            };

            let token = token.ok_or_else(|| {
                log!("server: token required but none provided");
                AmuxError::InvalidCredentials
            })?;

            match validator.validate(&token, &host, tcp_port).await {
                Ok(claims) => {
                    log!("server: authenticated connection from user {}", claims.sub);
                }
                Err(e) => {
                    log!("server: token validation failed: {}", e);
                    transport
                        .write_message(&Message::ConnectResponse {
                            success: false,
                            error: Some(ProtocolError::InvalidCredentials),
                        })
                        .await?;
                    return Err(AmuxError::InvalidCredentials);
                }
            }
        }

        // Check if link name is already taken
        {
            let state = state.read().await;
            if state.routes.contains_key(&proposed_link) {
                transport
                    .write_message(&Message::ConnectResponse {
                        success: false,
                        error: Some(ProtocolError::LinkNameTaken),
                    })
                    .await?;
                continue;
            }
        }

        transport
            .write_message(&Message::ConnectResponse {
                success: true,
                error: None,
            })
            .await?;

        return Ok(proposed_link);
    }

    Err(AmuxError::TooManyHandshakeAttempts)
}

/// Connect-side handshake: we propose link name, remote validates.
/// Returns the accepted link name on success.
async fn connect_handshake<T, F>(transport: &mut T, generate_link: F) -> Result<String>
where
    T: Transport,
    F: Fn() -> String,
{
    for attempt in 0..5 {
        let proposed_link = generate_link();

        transport
            .write_message(&Message::Connect {
                link_name: proposed_link.clone(),
                token: None,
            })
            .await?;

        let response = transport.read_message().await?;
        match response {
            Message::ConnectResponse { success: true, .. } => {
                return Ok(proposed_link);
            }
            Message::ConnectResponse {
                success: false,
                error: Some(ProtocolError::LinkNameTaken),
            } => {
                log!(
                    "link name {} taken, retrying (attempt {})",
                    proposed_link,
                    attempt + 1
                );
                continue;
            }
            Message::ConnectResponse {
                success: false,
                error: Some(ProtocolError::InvalidCredentials),
            } => {
                log!("server: invalid credentials - authentication failed");
                return Err(AmuxError::InvalidCredentials);
            }
            Message::ConnectResponse {
                success: false,
                error,
            } => {
                let msg = error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection rejected".to_string());
                return Err(AmuxError::Config(msg));
            }
            Message::Error { message } => {
                return Err(AmuxError::ServerError(message));
            }
            _ => return Err(AmuxError::InvalidMessage),
        }
    }

    Err(AmuxError::Config(
        "Failed to connect after 5 attempts".to_string(),
    ))
}

/// WebSocket client bootstrap - accept, upgrade, and handshake
async fn websocket_accept(stream: TcpStream, state: Arc<RwLock<ServerState>>) -> Result<()> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;

    let mut transport = WebSocketTransport::new(ws_stream);

    let link_name = match accept_handshake(&mut transport, &state, false).await {
        Ok(name) => name,
        Err(e) => {
            let _ = transport.write_message(&error_to_message(&e)).await;
            return Err(e);
        }
    };

    log!("server: websocket client {} connected", link_name);

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    let ctx = WebSocketClientContext {
        state: state.clone(),
        link_name: link_name.clone(),
    };

    let result = websocket_client_loop(&mut transport, outgoing_rx, ctx).await;

    // Log and send error to client before disconnecting
    if let Err(ref e) = result {
        log!("server: websocket client {} error: {}", link_name, e);
        let _ = transport.write_message(&error_to_message(e)).await;
    }

    // Clean up route on disconnect
    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }

    log!("server: websocket client {} disconnected", link_name);
    result
}

/// Context for WebSocket client message handling
struct WebSocketClientContext {
    state: Arc<RwLock<ServerState>>,
    link_name: String,
}

/// WebSocket client message loop
async fn websocket_client_loop(
    transport: &mut WebSocketTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    ctx: WebSocketClientContext,
) -> Result<()> {
    loop {
        tokio::select! {
            // Incoming message from WebSocket client
            result = transport.read_message() => {
                let msg = match result {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };

                websocket_handle_message(transport, msg, &ctx).await?;
            }

            // Outgoing message from routing (StructuredOutput, AgentEnded, etc.)
            Some(msg) = outgoing_rx.recv() => {
                log!("server: routing message to websocket {}: {:?}", ctx.link_name, msg);
                if transport.write_message(&msg).await.is_err() {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Handle a message from a WebSocket client
async fn websocket_handle_message(
    transport: &mut WebSocketTransport,
    msg: Message,
    ctx: &WebSocketClientContext,
) -> Result<()> {
    log!("server: websocket {} received {:?}", ctx.link_name, msg);

    match msg {
        Message::ListAgents => {
            let agents = {
                let state = ctx.state.read().await;
                state
                    .agents
                    .values()
                    .map(|s| s.to_agent_info())
                    .collect::<Vec<_>>()
            };
            transport
                .write_message(&Message::ListAgentsResult { agents })
                .await?;
            Ok(())
        }

        // Subscribe for WebSocket clients uses structured logs (local only)
        Message::Subscribe {
            src, dst, agent_id, ..
        } => {
            // Compute reply routes from incoming src
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");

            // Check if destination is local (empty route = local)
            if !dst.is_empty() {
                transport
                    .write_message(&Message::SubscribeResult {
                        src: reply_src,
                        dst: reply_dst,
                        agent_id,
                        success: false,
                        error: Some(ProtocolError::ServerError(
                            "Remote agents not supported via WebSocket".to_string(),
                        )),
                    })
                    .await?;
                return Ok(());
            }

            // Get log reader from session (agent_id can be UUID or alias)
            let log_reader = {
                let state = ctx.state.read().await;
                if let Some(session) = resolve_agent(&state.agents, &agent_id) {
                    session.subscribe_logs().await
                } else {
                    None
                }
            };

            match log_reader {
                Some(mut reader) => {
                    // Send success response
                    transport
                        .write_message(&Message::SubscribeResult {
                            src: reply_src.clone(),
                            dst: reply_dst.clone(),
                            agent_id: agent_id.clone(),
                            success: true,
                            error: None,
                        })
                        .await?;

                    log!(
                        "server: websocket client {} subscribed to agent {}",
                        ctx.link_name,
                        agent_id
                    );

                    // Get outgoing_tx for this client
                    let outgoing_tx = {
                        let state = ctx.state.read().await;
                        state.routes.get(&ctx.link_name).cloned()
                    };

                    // Spawn structured log streaming task
                    if let Some(tx) = outgoing_tx {
                        let agent_id_clone = agent_id.clone();
                        tokio::spawn(async move {
                            while let Some(entry) = reader.read().await {
                                if tx
                                    .send(Message::StructuredOutput {
                                        src: reply_src.clone(),
                                        dst: reply_dst.clone(),
                                        agent_id: agent_id_clone.clone(),
                                        entry,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            // Agent ended - send via same channel
                            let _ = tx.send(Message::AgentEnded).await;
                            log!("server: structured log stream ended");
                        });
                    }

                    Ok(())
                }
                None => {
                    transport
                        .write_message(&Message::SubscribeResult {
                            src: reply_src,
                            dst: reply_dst,
                            agent_id,
                            success: false,
                            error: Some(ProtocolError::ServerError(
                                "Agent not found or ended".to_string(),
                            )),
                        })
                        .await?;
                    Ok(())
                }
            }
        }

        // Submit input from WebSocket client - write data then Enter with delay
        Message::SubmitInput {
            mut src,
            mut dst,
            agent_id,
            data,
        } => {
            match dst.pop() {
                None => {
                    // Local agent - send data, wait, then send Enter
                    let state = ctx.state.read().await;
                    if let Some(session) = resolve_agent(&state.agents, &agent_id) {
                        let _ = session.send_input(data).await;
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        let _ = session.send_input(vec![b'\r']).await;
                    }
                }
                Some(next_hop) => {
                    // Remote agent - forward via route
                    let route = {
                        let state = ctx.state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::SubmitInput {
                                src,
                                dst,
                                agent_id,
                                data,
                            })
                            .await;
                    }
                }
            }
            Ok(())
        }

        // Permission request response from WebSocket client - send keystroke to agent
        Message::PermissionRequestResponse {
            mut src,
            mut dst,
            agent_id,
            response,
        } => {
            match dst.pop() {
                None => {
                    // Local agent - send keystroke
                    let state = ctx.state.read().await;
                    if let Some(session) = resolve_agent(&state.agents, &agent_id) {
                        let keystroke = permission_response_keystroke(&response);
                        log!(
                            "server: sending permission response {:?} to agent {} (keystroke: {:?})",
                            response,
                            agent_id,
                            keystroke
                        );
                        let _ = session.send_input(keystroke.to_vec()).await;
                    }
                }
                Some(next_hop) => {
                    // Remote agent - forward via route
                    let route = {
                        let state = ctx.state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::PermissionRequestResponse {
                                src,
                                dst,
                                agent_id,
                                response,
                            })
                            .await;
                    }
                }
            }
            Ok(())
        }

        _ => {
            transport
                .write_message(&Message::Error {
                    message: "Unsupported message for WebSocket clients".to_string(),
                })
                .await?;
            Ok(())
        }
    }
}

/// Unix client bootstrap - accept and handshake
async fn unix_accept(
    stream: UnixStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let mut transport = UnixTransport::new(stream);

    let link_name = match accept_handshake(&mut transport, &state, false).await {
        Ok(name) => name,
        Err(e) => {
            let _ = transport.write_message(&error_to_message(&e)).await;
            return Err(e);
        }
    };

    log!("server: client {} connected", link_name);

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    let ctx = UnixClientContext {
        state: state.clone(),
        event_tx,
        link_name: link_name.clone(),
    };

    let result = unix_client_loop(&mut transport, outgoing_rx, ctx).await;

    // Log and send error to client before disconnecting
    if let Err(ref e) = result {
        log!("server: client {} error: {}", link_name, e);
        let _ = transport.write_message(&error_to_message(e)).await;
    }

    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }

    result
}

/// Handle a single message from Unix client
async fn unix_handle_message(
    transport: &mut UnixTransport,
    msg: Message,
    ctx: &UnixClientContext,
) -> Result<()> {
    log!("server: client {} received {:?}", ctx.link_name, msg);

    match msg {
        Message::ListAgents => {
            let agents = {
                let state = ctx.state.read().await;
                state
                    .agents
                    .values()
                    .map(|s| s.to_agent_info())
                    .collect::<Vec<_>>()
            };
            transport
                .write_message(&Message::ListAgentsResult { agents })
                .await?;
            Ok(())
        }

        Message::CreateAgent(req) => {
            let result = create_agent(&ctx.state, &ctx.event_tx, req).await;

            let response = match result {
                Ok(()) => Message::CreateAgentResult {
                    success: true,
                    error: None,
                },
                Err(e) => Message::CreateAgentResult {
                    success: false,
                    error: Some(ProtocolError::ServerError(e.to_string())),
                },
            };
            transport.write_message(&response).await?;
            Ok(())
        }

        Message::Subscribe {
            src,
            mut dst,
            agent_id,
            rows,
            cols,
        } => {
            // Check if this subscribe is for a local agent or needs routing
            if let Some(next_hop) = dst.pop() {
                // Forward to next hop
                log!(
                    "server: forwarding Subscribe to {} via {}",
                    agent_id,
                    next_hop
                );
                let route = {
                    let state = ctx.state.read().await;
                    state.routes.get(&next_hop).cloned()
                };

                if let Some(route) = route {
                    // Push outgoing link to src before forwarding
                    let mut src = src;
                    src.push(&next_hop);
                    let _ = route
                        .send(Message::Subscribe {
                            src,
                            dst,
                            agent_id,
                            rows,
                            cols,
                        })
                        .await;
                    // Response will come back through tcp_peer_loop
                    // and be routed to this client via outgoing_rx
                } else {
                    log!("server: no route to {}", next_hop);
                    let (reply_src, reply_dst) =
                        Route::reply(src).expect("incoming message must have valid src");
                    transport
                        .write_message(&Message::SubscribeResult {
                            src: reply_src,
                            dst: reply_dst,
                            agent_id,
                            success: false,
                            error: Some(ProtocolError::ServerError("No route to host".to_string())),
                        })
                        .await?;
                }
                return Ok(());
            }

            // Compute reply routes from incoming src for local responses
            let (reply_src, reply_dst) =
                Route::reply(src).expect("incoming message must have valid src");

            // Local subscribe - spawn output streaming task
            let agent_id_str = agent_id.to_string();
            let result = handle_subscribe(&ctx.state, &agent_id_str, rows, cols).await;

            match result {
                Ok((mut buffer_reader, input_tx)) => {
                    // Send success response
                    transport
                        .write_message(&Message::SubscribeResult {
                            src: reply_src.clone(),
                            dst: reply_dst.clone(),
                            agent_id: agent_id.clone(),
                            success: true,
                            error: None,
                        })
                        .await?;

                    log!(
                        "server: client {} subscribed to agent {}",
                        ctx.link_name,
                        agent_id
                    );

                    // Get outgoing_tx for this client
                    let outgoing_tx = {
                        let state = ctx.state.read().await;
                        state.routes.get(&ctx.link_name).cloned()
                    };

                    // Spawn output streaming task
                    if let Some(tx) = outgoing_tx {
                        let agent_id_clone = agent_id.clone();
                        tokio::spawn(async move {
                            while let Some(data) = buffer_reader.read().await {
                                if tx
                                    .send(Message::Output {
                                        src: reply_src.clone(),
                                        dst: reply_dst.clone(),
                                        agent_id: agent_id_clone.clone(),
                                        data,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            // Agent ended - send via same channel
                            let _ = tx.send(Message::AgentEnded).await;
                            log!("server: output stream ended");
                        });
                    }

                    // Drop input_tx - we look up agent directly on Input
                    let _ = input_tx;

                    Ok(())
                }
                Err(e) => {
                    transport
                        .write_message(&Message::SubscribeResult {
                            src: reply_src,
                            dst: reply_dst,
                            agent_id,
                            success: false,
                            error: Some(ProtocolError::ServerError(e.to_string())),
                        })
                        .await?;
                    Ok(())
                }
            }
        }

        Message::Shutdown => {
            log!("server: shutdown requested by {}", ctx.link_name);
            shutdown_server(&ctx.state).await;

            // Try to send response, but don't fail if client disconnected
            let _ = transport
                .write_message(&Message::Error {
                    message: "Server shutting down".to_string(),
                })
                .await;

            // Clean up and exit (always reached now)
            let socket_path = {
                let state = ctx.state.read().await;
                state.config.socket_path.clone()
            };
            let _ = std::fs::remove_file(socket_path);
            log!("server: exiting");
            std::process::exit(0);
        }

        Message::ConnectToServer { address } => {
            let result = tcp_connect(&address, &ctx.state).await;
            let response = match result {
                Ok(()) => Message::ConnectToServerResult {
                    success: true,
                    error: None,
                },
                Err(e) => Message::ConnectToServerResult {
                    success: false,
                    error: Some(ProtocolError::ServerError(e.to_string())),
                },
            };
            transport.write_message(&response).await?;
            Ok(())
        }

        // Raw input bytes from client - forward to local agent or remote host
        Message::InputBytes {
            mut src,
            mut dst,
            agent_id,
            data,
        } => {
            match dst.pop() {
                None => {
                    // Local agent - send directly (agent_id can be UUID or alias)
                    let state = ctx.state.read().await;
                    let agent_id_str = agent_id.to_string();
                    if let Some(session) = resolve_agent(&state.agents, &agent_id_str) {
                        let _ = session.send_input(data).await;
                    }
                }
                Some(next_hop) => {
                    // Remote agent - forward via route
                    let route = {
                        let state = ctx.state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::InputBytes {
                                src,
                                dst,
                                agent_id,
                                data,
                            })
                            .await;
                    }
                }
            }
            Ok(())
        }

        // Hook event from CLI hook handler - link transcript by session_id
        Message::HookEvent { hook } => {
            log!("server: HookEvent from {}: {:?}", ctx.link_name, hook);

            // Look up agent by session_id and process hook
            let result = match &hook {
                Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                    let state = ctx.state.read().await;
                    // session_id is the agent_id we passed to claude --session-id
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
                        // Write permission request to log buffer for WebSocket subscribers
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
                Ok(()) => Message::HookEventResult {
                    success: true,
                    error: None,
                },
                Err(e) => Message::HookEventResult {
                    success: false,
                    error: Some(e),
                },
            };
            transport.write_message(&response).await?;
            Ok(())
        }

        _ => {
            transport
                .write_message(&Message::Error {
                    message: "Unexpected message".to_string(),
                })
                .await?;
            Ok(())
        }
    }
}

/// Convert an error to an Error message for sending to client
fn error_to_message(e: &AmuxError) -> Message {
    Message::Error {
        message: e.to_string(),
    }
}

/// Unix client message loop
async fn unix_client_loop(
    transport: &mut UnixTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    ctx: UnixClientContext,
) -> Result<()> {
    loop {
        tokio::select! {
            // Incoming message from client
            result = transport.read_message() => {
                let msg = match result {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };

                unix_handle_message(transport, msg, &ctx).await?;
            }

            // Outgoing message from routing (e.g., SubscribeResult, Output from local/remote)
            Some(msg) = outgoing_rx.recv() => {
                log!("server: routing message to {}: {:?}", ctx.link_name, msg);
                if transport.write_message(&msg).await.is_err() {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Resolve an agent by UUID or alias
fn resolve_agent<'a>(
    agents: &'a HashMap<Uuid, Arc<LocalAgentSession>>,
    identifier: &str,
) -> Option<&'a Arc<LocalAgentSession>> {
    // First try parsing as UUID for direct lookup
    if let Ok(uuid) = Uuid::parse_str(identifier) {
        if let Some(agent) = agents.get(&uuid) {
            return Some(agent);
        }
    }
    // Fall back to alias lookup
    agents
        .values()
        .find(|a| a.alias.as_deref() == Some(identifier))
}

/// Create a new agent
async fn create_agent(
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: CreateAgentRequest,
) -> Result<()> {
    let mut state = state.write().await;

    // Check if agent with this UUID already exists
    if state.agents.contains_key(&req.agent_id) {
        return Err(AmuxError::AgentAlreadyExists(req.agent_id.to_string()));
    }

    // Check if alias is already in use
    if let Some(ref a) = req.alias {
        if state.agents.values().any(|s| s.alias.as_deref() == Some(a)) {
            return Err(AmuxError::AgentAlreadyExists(a.clone()));
        }
    }

    // Create session
    let session = LocalAgentSession::new(&req, event_tx.clone())?;

    state.agents.insert(req.agent_id, Arc::new(session));

    log!(
        "server: created agent {} (alias={:?})",
        req.agent_id,
        req.alias
    );
    Ok(())
}

/// Handle subscribe request (identifier can be UUID or alias)
async fn handle_subscribe(
    state: &Arc<RwLock<ServerState>>,
    identifier: &str,
    rows: u16,
    cols: u16,
) -> Result<(MultiplexReader, mpsc::Sender<Vec<u8>>)> {
    let state = state.read().await;

    let session = resolve_agent(&state.agents, identifier)
        .ok_or_else(|| AmuxError::AgentNotFound(identifier.to_string()))?
        .clone();

    // Resize PTY if needed
    session.resize(rows, cols).await?;

    // Subscribe to the session - this atomically gives us a reader with
    // all existing output plus a stream of future output, with no gaps or duplicates
    session
        .subscribe()
        .await
        .ok_or_else(|| AmuxError::AgentNotFound(identifier.to_string()))
}

/// Shutdown the server
async fn shutdown_server(state: &Arc<RwLock<ServerState>>) {
    let mut state = state.write().await;
    for (name, session) in state.agents.iter() {
        log!("server: shutting down agent {}", name);
        session.shutdown().await;
    }
    state.agents.clear();
}

/// TCP outbound connection - connect and handshake
async fn tcp_connect(address: &str, state: &Arc<RwLock<ServerState>>) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .map_err(|_| AmuxError::Config(format!("Invalid address: {}", address)))?;

    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;

    log!("server: connected to remote server at {}", addr);

    let mut transport = TcpTransport::new(stream);

    let (hostname, randomise) = {
        let state = state.read().await;
        (
            state.config.host_name.clone(),
            state.config.randomise_link_name,
        )
    };

    let link_name = connect_handshake(&mut transport, || {
        generate_server_link(&hostname, randomise)
    })
    .await?;

    log!(
        "server: handshake complete with remote server (link: {})",
        link_name
    );

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    let state = state.clone();
    let link_name_clone = link_name.clone();
    tokio::spawn(async move {
        let mut transport = transport;
        let result = tcp_peer_loop(
            &mut transport,
            outgoing_rx,
            link_name_clone.clone(),
            state.clone(),
            None, // No token refresh for regular TCP connections
        )
        .await;

        // Log and send error to peer before disconnecting
        if let Err(ref e) = result {
            log!("server: tcp peer {} error: {}", link_name_clone, e);
            let _ = transport.write_message(&error_to_message(e)).await;
        }

        let mut state = state.write().await;
        state.routes.remove(&link_name_clone);
    });

    Ok(())
}

/// TCP peer bootstrap - accept inbound connection and run handshake.
///
/// Generic over transport type to support both plain TCP and TLS connections.
/// Set `verify_token` to true for cloud server mode (validates JWT in Connect message).
async fn tcp_accept<T: Transport>(
    mut transport: T,
    state: Arc<RwLock<ServerState>>,
    verify_token: bool,
) -> Result<()> {
    log!("server: inbound TCP connection");

    let link_name = match accept_handshake(&mut transport, &state, verify_token).await {
        Ok(name) => name,
        Err(e) => {
            let _ = transport.write_message(&error_to_message(&e)).await;
            return Err(e);
        }
    };

    log!("server: handshake complete (link: {})", link_name);

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    let result = tcp_peer_loop(
        &mut transport,
        outgoing_rx,
        link_name.clone(),
        state.clone(),
        None, // No token refresh for inbound connections
    )
    .await;

    // Log and send error to peer before disconnecting
    if let Err(ref e) = result {
        log!("server: tcp peer {} error: {}", link_name, e);
        let _ = transport.write_message(&error_to_message(e)).await;
    }

    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }

    result
}

/// Establish and maintain a cloud connection with automatic reconnection.
///
/// This function spawns a background task that:
/// 1. Checks if cloud mode is enabled in state
/// 2. Connects to the cloud server with exponential backoff on retriable errors
/// 3. Stops retrying on non-retriable errors (auth failures)
fn establish_cloud_connection(config: Config, state: Arc<RwLock<ServerState>>) {
    tokio::spawn(async move {
        // Check if cloud mode is enabled before attempting connection
        let should_connect = State::load(&config.state_path)
            .map(|s| s.cloud.use_cloud_mode == Some(true))
            .unwrap_or(false);

        if !should_connect {
            log!("cloud: cloud mode not enabled, skipping connection");
            return;
        }

        let mut backoff = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(300);

        loop {
            log!("cloud: attempting connection");
            match run_cloud_connection(&config, state.clone()).await {
                Ok(()) => {
                    log!("cloud: connection closed cleanly");
                    backoff = Duration::from_secs(1);
                }
                Err(CloudConnectionError::NonRetriable(msg)) => {
                    log!("cloud: non-retriable error, stopping: {}", msg);
                    return;
                }
                Err(CloudConnectionError::Retriable(msg)) => {
                    log!("cloud: retriable error: {}", msg);
                }
            }

            // Check if still enabled before reconnecting
            let should_reconnect = State::load(&config.state_path)
                .map(|s| s.cloud.use_cloud_mode == Some(true))
                .unwrap_or(false);

            if !should_reconnect {
                log!("cloud: cloud mode disabled, stopping reconnection");
                return;
            }

            log!("cloud: reconnecting in {:?}", backoff);
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
        }
    });
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable(String),
    /// Error that should stop reconnection attempts (auth failure)
    NonRetriable(String),
}

/// Run a single cloud connection attempt.
///
/// Returns Ok(()) on clean disconnect, or an error indicating whether to retry.
async fn run_cloud_connection(
    config: &Config,
    state: Arc<RwLock<ServerState>>,
) -> std::result::Result<(), CloudConnectionError> {
    // Establish cloud connection (handles OAuth token exchange)
    let conn = match CloudConnection::connect(config).await {
        Ok(conn) => conn,
        Err(CloudError::NotAuthenticated) | Err(CloudError::Auth(_)) => {
            return Err(CloudConnectionError::NonRetriable(
                "Authentication failed - run 'amux init' to re-authenticate".to_string(),
            ));
        }
        Err(CloudError::CloudDisabled) => {
            return Err(CloudConnectionError::NonRetriable(
                "Cloud mode disabled".to_string(),
            ));
        }
        Err(e) => {
            return Err(CloudConnectionError::Retriable(format!(
                "Connection failed: {}",
                e
            )));
        }
    };

    let (mut transport, token_refresh) = conn.into_parts();
    let link_name = token_refresh.link_name.clone();

    // Register route for outgoing messages
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    // Run peer loop with token refresh
    let result = tcp_peer_loop(
        &mut transport,
        outgoing_rx,
        link_name.clone(),
        state.clone(),
        Some(token_refresh),
    )
    .await;

    // Cleanup route on disconnect
    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }

    // Map result to retriable/non-retriable
    match result {
        Ok(()) => Ok(()),
        Err(AmuxError::InvalidCredentials) => Err(CloudConnectionError::NonRetriable(
            "Invalid credentials".to_string(),
        )),
        Err(e) => Err(CloudConnectionError::Retriable(e.to_string())),
    }
}

/// Sleep until the deadline, or wait forever if None.
/// This is used for optional token refresh in tcp_peer_loop.
async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// TCP peer message loop
///
/// This is called after the handshake is complete, on both the initiator
/// and receiver sides. It routes messages between this server and the remote.
///
/// If `token_refresh` is provided, the loop will automatically refresh the
/// cloud connection token before it expires.
async fn tcp_peer_loop<T: Transport>(
    transport: &mut T,
    mut outgoing_rx: mpsc::Receiver<Message>,
    link_name: String,
    state: Arc<RwLock<ServerState>>,
    mut token_refresh: Option<TokenRefreshState>,
) -> Result<()> {
    log!("server: handling TCP connection with {}", link_name);

    let mut refresh_deadline = token_refresh.as_ref().map(|t| t.refresh_deadline());

    loop {
        tokio::select! {
            // Read message from remote
            result = transport.read_message() => {
                let msg = match result {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };

                log!("server: received from {}: {:?}", link_name, msg);

                tcp_handle_message(msg, &link_name, &state).await?;
            }

            // Outgoing message to send to remote
            Some(msg) = outgoing_rx.recv() => {
                log!("server: sending to {}: {:?}", link_name, msg);
                if transport.write_message(&msg).await.is_err() {
                    return Ok(());
                }
            }

            // Token refresh (only fires if token_refresh is Some)
            _ = maybe_sleep_until(refresh_deadline) => {
                if let Some(ref mut rs) = token_refresh {
                    log!("cloud: refreshing token");
                    match rs.refresh_and_reconnect(transport).await {
                        Ok(()) => {
                            refresh_deadline = Some(rs.refresh_deadline());
                        }
                        Err(CloudError::HostChanged) => {
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

/// Handle a single message from TCP peer
async fn tcp_handle_message(
    msg: Message,
    _incoming_link: &str,
    state: &Arc<RwLock<ServerState>>,
) -> Result<()> {
    match msg {
        Message::Subscribe {
            mut src,
            mut dst,
            agent_id,
            rows,
            cols,
        } => {
            match dst.pop() {
                None => {
                    // Subscribe to local agent
                    let agent_id_str = agent_id.to_string();
                    let result = handle_subscribe(state, &agent_id_str, rows, cols).await;

                    match result {
                        Ok((mut buffer_reader, input_tx)) => {
                            // Send success response back via src route
                            // Pop next hop from src for routing reply
                            if let Some(next_hop) = src.pop() {
                                let route = {
                                    let state = state.read().await;
                                    state.routes.get(&next_hop).cloned()
                                };
                                if let Some(route) = route {
                                    let reply_dst = src.clone();
                                    // Push outgoing link to become new src for reply
                                    let reply_src = Route::from_link(&next_hop);

                                    let _ = route
                                        .send(Message::SubscribeResult {
                                            src: reply_src.clone(),
                                            dst: reply_dst.clone(),
                                            agent_id: agent_id.clone(),
                                            success: true,
                                            error: None,
                                        })
                                        .await;

                                    // Spawn task to stream output to the subscriber
                                    let state_clone = state.clone();
                                    let agent_id_clone = agent_id.clone();
                                    let next_hop_clone = next_hop.clone();
                                    tokio::spawn(async move {
                                        while let Some(data) = buffer_reader.read().await {
                                            let route = {
                                                let state = state_clone.read().await;
                                                state.routes.get(&next_hop_clone).cloned()
                                            };
                                            if let Some(route) = route {
                                                if route
                                                    .send(Message::Output {
                                                        src: reply_src.clone(),
                                                        dst: reply_dst.clone(),
                                                        agent_id: agent_id_clone.clone(),
                                                        data,
                                                    })
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            } else {
                                                break;
                                            }
                                        }
                                        log!("server: output stream ended");
                                    });
                                }
                            }

                            // Drop input_tx - we look up the agent directly on Input
                            let _ = input_tx;
                        }
                        Err(e) => {
                            // Send error response back via src route
                            if let Some(next_hop) = src.pop() {
                                let route = {
                                    let state = state.read().await;
                                    state.routes.get(&next_hop).cloned()
                                };
                                if let Some(route) = route {
                                    let reply_src = Route::from_link(&next_hop);
                                    let _ = route
                                        .send(Message::SubscribeResult {
                                            src: reply_src,
                                            dst: src,
                                            agent_id,
                                            success: false,
                                            error: Some(ProtocolError::ServerError(e.to_string())),
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                }
                Some(next_hop) => {
                    // Forward to next hop
                    let route = {
                        let state = state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        // Push outgoing link to src before forwarding
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::Subscribe {
                                src,
                                dst,
                                agent_id,
                                rows,
                                cols,
                            })
                            .await;
                    }
                }
            }
        }

        Message::SubscribeResult {
            mut src,
            mut dst,
            agent_id,
            success,
            error,
        } => {
            // Pop next hop from dst for routing
            if let Some(next_hop) = dst.pop() {
                let route = {
                    let state = state.read().await;
                    state.routes.get(&next_hop).cloned()
                };
                if let Some(route) = route {
                    // Push outgoing link to src before forwarding
                    src.push(&next_hop);
                    let _ = route
                        .send(Message::SubscribeResult {
                            src,
                            dst,
                            agent_id,
                            success,
                            error,
                        })
                        .await;
                }
            }
        }

        Message::InputBytes {
            mut src,
            mut dst,
            agent_id,
            data,
        } => {
            match dst.pop() {
                None => {
                    // Send input to local agent
                    let state = state.read().await;
                    let agent_id_str = agent_id.to_string();
                    if let Some(session) = resolve_agent(&state.agents, &agent_id_str) {
                        let _ = session.send_input(data).await;
                    }
                }
                Some(next_hop) => {
                    // Forward to next hop
                    let route = {
                        let state = state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::InputBytes {
                                src,
                                dst,
                                agent_id,
                                data,
                            })
                            .await;
                    }
                }
            }
        }

        Message::SubmitInput {
            mut src,
            mut dst,
            agent_id,
            data,
        } => {
            match dst.pop() {
                None => {
                    // Send input to local agent with delay then Enter
                    let state = state.read().await;
                    let agent_id_str = agent_id.to_string();
                    if let Some(session) = resolve_agent(&state.agents, &agent_id_str) {
                        let _ = session.send_input(data).await;
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        let _ = session.send_input(vec![b'\r']).await;
                    }
                }
                Some(next_hop) => {
                    // Forward to next hop
                    let route = {
                        let state = state.read().await;
                        state.routes.get(&next_hop).cloned()
                    };
                    if let Some(route) = route {
                        src.push(&next_hop);
                        let _ = route
                            .send(Message::SubmitInput {
                                src,
                                dst,
                                agent_id,
                                data,
                            })
                            .await;
                    }
                }
            }
        }

        Message::Output {
            mut src,
            mut dst,
            agent_id,
            data,
        } => {
            // Pop next hop from dst for routing
            if let Some(next_hop) = dst.pop() {
                let route = {
                    let state = state.read().await;
                    state.routes.get(&next_hop).cloned()
                };
                if let Some(route) = route {
                    src.push(&next_hop);
                    let _ = route
                        .send(Message::Output {
                            src,
                            dst,
                            agent_id,
                            data,
                        })
                        .await;
                }
            }
        }

        _ => {
            log!("server: unexpected message: {:?}", msg);
        }
    }

    Ok(())
}

/// Convert a permission response to the keystroke to send to Claude Code's TUI.
/// Claude Code's permission UI accepts:
/// - 1: Yes (accept this edit)
/// - 2: Yes (accept all edits)
/// - 3: No (deny)
fn permission_response_keystroke(response: &PermissionResponse) -> &'static [u8] {
    match response {
        PermissionResponse::Yes => b"1",
        PermissionResponse::YesAll => b"2",
        PermissionResponse::No => b"3",
    }
}
