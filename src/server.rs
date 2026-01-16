use crate::buffer::MultiplexReader;
use crate::config::{Config, DEFAULT_TCP_PORT, DEFAULT_WEBSOCKET_PORT};
use crate::error::{AmuxError, Result};
use crate::message::{ClaudeHook, Hook, Message};
use crate::session::{AgentId, LocalAgentSession, SessionEvent};
use crate::transport::{TcpTransport, Transport, UnixTransport, WebSocketTransport};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::accept_async;

/// Determine how to route a message based on dst_host.
///
/// Route map only contains single-layer entries (no "/" in keys).
/// When routing downstream (responding):
/// - If dst_host starts with our_host/, strip that prefix
/// - Then find the next hop (first segment before any "/")
///
/// Returns (route_key, new_dst_host)
fn resolve_route(dst_host: &str, our_host: &str) -> (String, String) {
    // First, strip our prefix if present
    let our_prefix = format!("{}/", our_host);
    let remainder = if dst_host.starts_with(&our_prefix) {
        &dst_host[our_prefix.len()..]
    } else {
        dst_host
    };

    // Now find the next hop (first segment)
    if let Some(pos) = remainder.find('/') {
        let next_hop = &remainder[..pos];
        (next_hop.to_string(), remainder.to_string())
    } else {
        // No "/" means this is the final destination
        (remainder.to_string(), remainder.to_string())
    }
}

/// Server state shared across connection handlers
struct ServerState {
    config: Config,
    agents: HashMap<String, Arc<LocalAgentSession>>,
    /// Routes to other hosts. Each route is a channel sender for outgoing messages.
    /// The actual transport is owned by the connection handler task.
    routes: HashMap<String, mpsc::Sender<Message>>,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            agents: HashMap::new(),
            routes: HashMap::new(),
        }
    }
}

/// Context for Unix client connection handlers
struct UnixClientContext {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    client_host_id: String,
    our_host: String,
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
    pub async fn run(&mut self) -> Result<()> {
        let (socket_path, tcp_port, ws_port) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port.unwrap_or(DEFAULT_TCP_PORT),
                state
                    .config
                    .websocket_port
                    .unwrap_or(DEFAULT_WEBSOCKET_PORT),
            )
        };

        let _ = std::fs::remove_file(&socket_path);

        let unix_listener = UnixListener::bind(&socket_path)?;
        log!("server: listening on {:?}", socket_path);

        let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_port));
        let tcp_listener = TcpListener::bind(tcp_addr).await?;
        log!("server: listening on TCP {}", tcp_addr);

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
                        log!("server: session {} ended, removing", agent_id.agent_id);
                        let mut state = state.write().await;
                        state.agents.remove(&agent_id.agent_id);
                    }
                }
            }
        });

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
                // TCP connection (remote servers)
                result = tcp_listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let state = self.state.clone();
                            tokio::spawn(async move {
                                if let Err(e) = tcp_accept(stream, state).await {
                                    log!("server: tcp connection error: {}", e);
                                }
                            });
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

/// WebSocket client bootstrap - accept, upgrade, and handshake
async fn websocket_accept(stream: TcpStream, state: Arc<RwLock<ServerState>>) -> Result<()> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;

    let mut transport = WebSocketTransport::new(ws_stream);

    let msg = transport.read_message().await?;
    let local_client_id = match msg {
        Message::Connect { host_id } => host_id,
        _ => {
            log!("server: websocket expected Connect, got {:?}", msg);
            return Err(AmuxError::InvalidMessage);
        }
    };

    // Construct hierarchical client ID: our_host/client_id
    let (our_host, client_host_id) = {
        let state = state.read().await;
        let our_host = state.config.host_id.clone();
        let client_host_id = format!("{}/{}", our_host, local_client_id);
        (our_host, client_host_id)
    };

    transport
        .write_message(&Message::ConnectResponse {
            success: true,
            error: None,
            host_id: our_host.clone(),
        })
        .await?;

    log!("server: websocket client {} connected", client_host_id);

    let ctx = WebSocketClientContext {
        state: state.clone(),
        client_host_id: client_host_id.clone(),
        our_host,
    };

    let result = websocket_client_loop(transport, ctx).await;

    log!("server: websocket client {} disconnected", client_host_id);
    result
}

/// Context for WebSocket client message handling
struct WebSocketClientContext {
    state: Arc<RwLock<ServerState>>,
    client_host_id: String,
    our_host: String,
}

/// WebSocket client message loop
async fn websocket_client_loop(
    mut transport: WebSocketTransport,
    ctx: WebSocketClientContext,
) -> Result<()> {
    loop {
        let msg = match transport.read_message().await {
            Ok(msg) => msg,
            Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(e) => {
                log!("server: websocket read error: {}", e);
                return Err(e);
            }
        };

        if let Err(e) = websocket_handle_message(&mut transport, msg, &ctx).await {
            log!("server: websocket message error: {}", e);
        }
    }
}

/// Handle a message from a WebSocket client
async fn websocket_handle_message(
    transport: &mut WebSocketTransport,
    msg: Message,
    ctx: &WebSocketClientContext,
) -> Result<()> {
    log!(
        "server: websocket {} received {:?}",
        ctx.client_host_id,
        msg
    );

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

        // Subscribe for WebSocket clients uses structured logs
        Message::Subscribe {
            dst_host, agent_id, ..
        } => {
            // Only support local agents for now
            if dst_host != ctx.our_host {
                transport
                    .write_message(&Message::SubscribeResult {
                        src_host: dst_host.clone(),
                        dst_host: ctx.client_host_id.clone(),
                        agent_id,
                        success: false,
                        error: Some("Remote agents not supported via WebSocket".to_string()),
                    })
                    .await?;
                return Ok(());
            }

            // Get log reader from session
            let log_reader = {
                let state = ctx.state.read().await;
                if let Some(session) = state.agents.get(&agent_id) {
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
                            src_host: ctx.our_host.clone(),
                            dst_host: ctx.client_host_id.clone(),
                            agent_id: agent_id.clone(),
                            success: true,
                            error: None,
                        })
                        .await?;

                    // Stream structured log entries
                    while let Some(entry) = reader.read().await {
                        transport
                            .write_message(&Message::StructuredOutput {
                                src_host: ctx.our_host.clone(),
                                dst_host: ctx.client_host_id.clone(),
                                agent_id: agent_id.clone(),
                                entry,
                            })
                            .await?;
                    }

                    // Session ended
                    transport.write_message(&Message::AgentEnded).await?;
                    Ok(())
                }
                None => {
                    transport
                        .write_message(&Message::SubscribeResult {
                            src_host: ctx.our_host.clone(),
                            dst_host: ctx.client_host_id.clone(),
                            agent_id,
                            success: false,
                            error: Some("Agent not found or ended".to_string()),
                        })
                        .await?;
                    Ok(())
                }
            }
        }

        _ => {
            transport
                .write_message(&Message::Error {
                    code: 1,
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

    let msg = transport.read_message().await?;
    let local_client_id = match msg {
        Message::Connect { host_id } => host_id,
        _ => {
            log!("server: expected Connect, got {:?}", msg);
            return Err(AmuxError::InvalidMessage);
        }
    };

    // Construct hierarchical client ID: our_host/client_id
    let (our_host, client_host_id) = {
        let state = state.read().await;
        let our_host = state.config.host_id.clone();
        let client_host_id = format!("{}/{}", our_host, local_client_id);
        (our_host, client_host_id)
    };

    transport
        .write_message(&Message::ConnectResponse {
            success: true,
            error: None,
            host_id: our_host.clone(),
        })
        .await?;

    log!("server: client {} connected", client_host_id);

    // Routes table uses single-layer keys only (no "/" in keys)
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(local_client_id.clone(), outgoing_tx);
    }

    let ctx = UnixClientContext {
        state: state.clone(),
        event_tx,
        client_host_id,
        our_host,
    };

    let result = unix_client_loop(transport, outgoing_rx, ctx).await;

    {
        let mut state = state.write().await;
        state.routes.remove(&local_client_id);
        log!("server: removed route to {}", local_client_id);
    }

    result
}

/// Handle a single message from Unix client
async fn unix_handle_message(
    transport: &mut UnixTransport,
    msg: Message,
    ctx: &UnixClientContext,
) -> Result<()> {
    log!("server: client {} received {:?}", ctx.client_host_id, msg);

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

        Message::CreateAgent {
            agent_id,
            command,
            working_dir,
            rows,
            cols,
        } => {
            let result = create_agent(
                &ctx.state,
                &ctx.event_tx,
                &agent_id,
                &command,
                working_dir,
                rows,
                cols,
            )
            .await;

            let response = match result {
                Ok(()) => Message::CreateAgentResult {
                    success: true,
                    error: None,
                },
                Err(e) => Message::CreateAgentResult {
                    success: false,
                    error: Some(e.to_string()),
                },
            };
            transport.write_message(&response).await?;
            Ok(())
        }

        Message::Subscribe {
            src_host: _,
            dst_host,
            agent_id,
            rows,
            cols,
        } => {
            // Rewrite src_host to client's full hierarchical ID
            let src_host = ctx.client_host_id.clone();

            // Check if this subscribe is for a local agent or needs routing
            if dst_host != ctx.our_host {
                // Forward to remote host with client's host_id as src
                log!(
                    "server: forwarding Subscribe from {} to {}",
                    src_host,
                    dst_host
                );
                let route = {
                    let state = ctx.state.read().await;
                    state.routes.get(&dst_host).cloned()
                };

                if let Some(route) = route {
                    let _ = route
                        .send(Message::Subscribe {
                            src_host,
                            dst_host: dst_host.clone(),
                            agent_id,
                            rows,
                            cols,
                        })
                        .await;
                    // Response will come back through tcp_peer_loop
                    // and be routed to this client via outgoing_rx
                } else {
                    log!("server: no route to {}", dst_host);
                    transport
                        .write_message(&Message::SubscribeResult {
                            src_host: dst_host.clone(),
                            dst_host: src_host,
                            agent_id,
                            success: false,
                            error: Some("No route to host".to_string()),
                        })
                        .await?;
                }
                return Ok(());
            }

            // Local subscribe - spawn output streaming task
            let result = handle_subscribe(&ctx.state, &agent_id, rows, cols).await;

            match result {
                Ok((mut buffer_reader, input_tx)) => {
                    // Send success response
                    transport
                        .write_message(&Message::SubscribeResult {
                            src_host: ctx.our_host.clone(),
                            dst_host: src_host.clone(),
                            agent_id: agent_id.clone(),
                            success: true,
                            error: None,
                        })
                        .await?;

                    log!(
                        "server: client {} subscribed to agent {}",
                        ctx.client_host_id,
                        agent_id
                    );

                    // Get outgoing_tx for this client (stored in routes as local_client_id)
                    let outgoing_tx = {
                        let state = ctx.state.read().await;
                        // local_client_id = last segment of client_host_id
                        let local_id = ctx.client_host_id.rsplit('/').next().unwrap();
                        state.routes.get(local_id).cloned()
                    };

                    // Spawn output streaming task
                    if let Some(tx) = outgoing_tx {
                        let our_host = ctx.our_host.clone();
                        let client_host = ctx.client_host_id.clone();
                        let agent_id_clone = agent_id.clone();
                        tokio::spawn(async move {
                            while let Some(data) = buffer_reader.read().await {
                                if tx
                                    .send(Message::Output {
                                        src_host: our_host.clone(),
                                        dst_host: client_host.clone(),
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
                            log!("server: output stream to {} ended", client_host);
                        });
                    }

                    // Drop input_tx - we look up agent directly on Input
                    let _ = input_tx;

                    Ok(())
                }
                Err(e) => {
                    transport
                        .write_message(&Message::SubscribeResult {
                            src_host: ctx.our_host.clone(),
                            dst_host: src_host,
                            agent_id,
                            success: false,
                            error: Some(e.to_string()),
                        })
                        .await?;
                    Ok(())
                }
            }
        }

        Message::Shutdown => {
            log!("server: shutdown requested by {}", ctx.client_host_id);
            shutdown_server(&ctx.state).await;
            transport
                .write_message(&Message::Error {
                    code: 0,
                    message: "Server shutting down".to_string(),
                })
                .await?;

            // Handle shutdown directly
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
                    error: Some(e.to_string()),
                },
            };
            transport.write_message(&response).await?;
            Ok(())
        }

        // Input from client - forward to local agent or remote host
        Message::Input {
            dst_host,
            agent_id,
            data,
            ..
        } => {
            if dst_host == ctx.our_host {
                // Local agent - send directly
                let state = ctx.state.read().await;
                if let Some(session) = state.agents.get(&agent_id) {
                    let _ = session.send_input(data).await;
                }
            } else {
                // Remote agent - forward via route
                let route = {
                    let state = ctx.state.read().await;
                    state.routes.get(&dst_host).cloned()
                };
                if let Some(route) = route {
                    let _ = route
                        .send(Message::Input {
                            src_host: ctx.client_host_id.clone(),
                            dst_host,
                            agent_id,
                            data,
                        })
                        .await;
                }
            }
            Ok(())
        }

        // Hook event from CLI hook handler - link transcript to most recent session
        Message::HookEvent { hook } => {
            log!("server: HookEvent from {}: {:?}", ctx.client_host_id, hook);

            // Extract transcript path based on hook type
            let transcript_path = match &hook {
                Hook::Claude(ClaudeHook::SessionStart { transcript_path }) => {
                    Some(transcript_path.clone())
                }
            };

            // Find most recently created local session and link transcript
            let result = {
                let state = ctx.state.read().await;
                match transcript_path {
                    Some(path) => {
                        // Get the most recently created agent (last in iteration)
                        if let Some((agent_id, session)) = state.agents.iter().last() {
                            log!("server: linking transcript to agent {}", agent_id);
                            session.link_transcript(PathBuf::from(&path)).await;
                            Ok(())
                        } else {
                            Err("No agents running".to_string())
                        }
                    }
                    None => Err("Hook type has no transcript path".to_string()),
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
                    code: 1,
                    message: "Unexpected message".to_string(),
                })
                .await?;
            Ok(())
        }
    }
}

/// Unix client message loop
async fn unix_client_loop(
    mut transport: UnixTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    ctx: UnixClientContext,
) -> Result<()> {
    loop {
        tokio::select! {
            // Incoming message from client
            msg = transport.read_message() => {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        log!("server: client {} disconnected", ctx.client_host_id);
                        return Ok(());
                    }
                    Err(e) => {
                        log!("server: client {} read error: {}", ctx.client_host_id, e);
                        return Err(e);
                    }
                };

                unix_handle_message(&mut transport, msg, &ctx).await?;
            }

            // Outgoing message from routing (e.g., SubscribeResult, Output from local/remote)
            Some(msg) = outgoing_rx.recv() => {
                log!("server: routing message to {}: {:?}", ctx.client_host_id, msg);
                if transport.write_message(&msg).await.is_err() {
                    log!("server: failed to send routed message to {}", ctx.client_host_id);
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Create a new agent
async fn create_agent(
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    agent_id: &str,
    command: &str,
    working_dir: PathBuf,
    rows: u16,
    cols: u16,
) -> Result<()> {
    let mut state = state.write().await;

    // Check if agent already exists
    if state.agents.contains_key(agent_id) {
        return Err(AmuxError::AgentAlreadyExists(agent_id.to_string()));
    }

    // Create agent ID
    let id = AgentId::local(&state.config, agent_id);

    // Create session
    let session = LocalAgentSession::new(id, command, working_dir, rows, cols, event_tx.clone())?;

    state.agents.insert(agent_id.to_string(), Arc::new(session));

    log!("server: created agent {}", agent_id);
    Ok(())
}

/// Handle subscribe request
async fn handle_subscribe(
    state: &Arc<RwLock<ServerState>>,
    agent_id: &str,
    rows: u16,
    cols: u16,
) -> Result<(MultiplexReader, mpsc::Sender<Vec<u8>>)> {
    let state_guard = state.read().await;

    let session = state_guard
        .agents
        .get(agent_id)
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))?
        .clone();

    // Resize PTY if needed
    session.resize(rows, cols).await?;

    // Subscribe to the session - this atomically gives us a reader with
    // all existing output plus a stream of future output, with no gaps or duplicates
    session
        .subscribe()
        .await
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))
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
    let our_host = {
        let state = state.read().await;
        state.config.host_id.clone()
    };

    transport
        .write_message(&Message::Connect {
            host_id: our_host.clone(),
        })
        .await?;

    let response = transport.read_message().await?;
    let remote_host = match response {
        Message::ConnectResponse {
            success: true,
            host_id,
            ..
        } => host_id,
        Message::ConnectResponse {
            success: false,
            error,
            ..
        } => {
            log!("server: remote rejected connection: {:?}", error);
            return Err(AmuxError::Config(
                error.unwrap_or_else(|| "Connection rejected".to_string()),
            ));
        }
        _ => {
            log!("server: unexpected message during handshake, closing");
            return Err(AmuxError::InvalidMessage);
        }
    };

    log!(
        "server: handshake complete with remote server (host: {})",
        remote_host
    );

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(remote_host.clone(), outgoing_tx);
    }

    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) =
            tcp_peer_loop(transport, outgoing_rx, remote_host.clone(), state.clone()).await
        {
            log!("server: TCP connection error: {}", e);
        }
        let mut state = state.write().await;
        state.routes.remove(&remote_host);
        log!("server: removed route to {}", remote_host);
    });

    Ok(())
}

/// TCP peer bootstrap - accept inbound and handshake
async fn tcp_accept(stream: TcpStream, state: Arc<RwLock<ServerState>>) -> Result<()> {
    stream.set_nodelay(true)?;
    let peer_addr = stream.peer_addr().ok();
    log!("server: inbound TCP from {:?}", peer_addr);

    let mut transport = TcpTransport::new(stream);
    let our_host = {
        let state = state.read().await;
        state.config.host_id.clone()
    };

    let msg = transport.read_message().await?;
    let remote_host = match msg {
        Message::Connect { host_id } => host_id,
        _ => {
            log!("server: expected Connect, got {:?}, closing", msg);
            return Err(AmuxError::InvalidMessage);
        }
    };

    transport
        .write_message(&Message::ConnectResponse {
            success: true,
            error: None,
            host_id: our_host,
        })
        .await?;

    log!(
        "server: handshake complete with {:?} (host: {})",
        peer_addr,
        remote_host
    );

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(remote_host.clone(), outgoing_tx);
    }

    let result = tcp_peer_loop(transport, outgoing_rx, remote_host.clone(), state.clone()).await;

    {
        let mut state = state.write().await;
        state.routes.remove(&remote_host);
        log!("server: removed route to {}", remote_host);
    }

    result
}

/// TCP peer message loop
///
/// This is called after the handshake is complete, on both the initiator
/// and receiver sides. It routes messages between this server and the remote.
async fn tcp_peer_loop(
    mut transport: TcpTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    remote_host: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<()> {
    log!("server: handling TCP connection with {}", remote_host);

    loop {
        tokio::select! {
            // Read message from remote
            msg = transport.read_message() => {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        log!("server: {} disconnected", remote_host);
                        break;
                    }
                    Err(e) => {
                        log!("server: {} read error: {}", remote_host, e);
                        break;
                    }
                };

                log!("server: received from {}: {:?}", remote_host, msg);

                if let Err(e) = tcp_handle_message(msg, &state).await {
                    log!("server: error handling message from {}: {}", remote_host, e);
                }
            }

            // Outgoing message to send to remote
            Some(msg) = outgoing_rx.recv() => {
                log!("server: sending to {}: {:?}", remote_host, msg);
                if transport.write_message(&msg).await.is_err() {
                    log!("server: failed to send to {}", remote_host);
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Handle a single message from TCP peer
async fn tcp_handle_message(msg: Message, state: &Arc<RwLock<ServerState>>) -> Result<()> {
    match msg {
        Message::Subscribe {
            src_host,
            dst_host,
            agent_id,
            rows,
            cols,
        } => {
            let our_host = {
                let state = state.read().await;
                state.config.host_id.clone()
            };

            if dst_host == our_host {
                // Subscribe to local agent
                let result = handle_subscribe(state, &agent_id, rows, cols).await;

                match result {
                    Ok((mut buffer_reader, input_tx)) => {
                        // Send success response back via the route to src_host
                        // Use resolve_route to extract the next hop
                        let (route_to, _) = resolve_route(&src_host, &our_host);
                        let route = {
                            let state = state.read().await;
                            state.routes.get(&route_to).cloned()
                        };
                        if let Some(route) = route {
                            let _ = route
                                .send(Message::SubscribeResult {
                                    src_host: our_host.clone(),
                                    dst_host: src_host.clone(),
                                    agent_id: agent_id.clone(),
                                    success: true,
                                    error: None,
                                })
                                .await;
                        }

                        // Spawn task to stream output to the subscriber
                        let state_clone = state.clone();
                        let src_host_clone = src_host.clone();
                        let agent_id_clone = agent_id.clone();
                        let our_host_clone = our_host.clone();
                        tokio::spawn(async move {
                            while let Some(data) = buffer_reader.read().await {
                                // Use resolve_route to extract the next hop
                                let (route_to, _) = resolve_route(&src_host_clone, &our_host_clone);
                                let route = {
                                    let state = state_clone.read().await;
                                    state.routes.get(&route_to).cloned()
                                };
                                if let Some(route) = route {
                                    if route
                                        .send(Message::Output {
                                            src_host: our_host_clone.clone(),
                                            dst_host: src_host_clone.clone(),
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
                            log!("server: output stream to {} ended", src_host_clone);
                        });

                        // Store input_tx for forwarding input later
                        // For now, we'll look up the agent directly on Input
                        let _ = input_tx; // Will retrieve via agent on Input
                    }
                    Err(e) => {
                        // Use resolve_route to extract the next hop
                        let (route_to, _) = resolve_route(&src_host, &our_host);
                        let route = {
                            let state = state.read().await;
                            state.routes.get(&route_to).cloned()
                        };
                        if let Some(route) = route {
                            let _ = route
                                .send(Message::SubscribeResult {
                                    src_host: our_host,
                                    dst_host: src_host,
                                    agent_id,
                                    success: false,
                                    error: Some(e.to_string()),
                                })
                                .await;
                        }
                    }
                }
            } else {
                // Forward to the destination host, prefixing src_host with our host_id
                let prefixed_src = format!("{}/{}", our_host, src_host);
                let route = {
                    let state = state.read().await;
                    state.routes.get(&dst_host).cloned()
                };
                if let Some(route) = route {
                    let _ = route
                        .send(Message::Subscribe {
                            src_host: prefixed_src,
                            dst_host,
                            agent_id,
                            rows,
                            cols,
                        })
                        .await;
                }
            }
        }

        Message::SubscribeResult {
            src_host,
            dst_host,
            agent_id,
            success,
            error,
        } => {
            let our_host = {
                let state = state.read().await;
                state.config.host_id.clone()
            };

            let (route_to, new_dst) = resolve_route(&dst_host, &our_host);

            let route = {
                let state = state.read().await;
                state.routes.get(&route_to).cloned()
            };
            if let Some(route) = route {
                let _ = route
                    .send(Message::SubscribeResult {
                        src_host,
                        dst_host: new_dst,
                        agent_id,
                        success,
                        error,
                    })
                    .await;
            }
        }

        Message::Input {
            src_host,
            dst_host,
            agent_id,
            data,
        } => {
            let our_host = {
                let state = state.read().await;
                state.config.host_id.clone()
            };

            if dst_host == our_host {
                // Send input to local agent
                let state = state.read().await;
                if let Some(session) = state.agents.get(&agent_id) {
                    let _ = session.send_input(data).await;
                }
            } else {
                // Forward to destination, prefixing src_host with our host_id
                let prefixed_src = format!("{}/{}", our_host, src_host);
                let route = {
                    let state = state.read().await;
                    state.routes.get(&dst_host).cloned()
                };
                if let Some(route) = route {
                    let _ = route
                        .send(Message::Input {
                            src_host: prefixed_src,
                            dst_host,
                            agent_id,
                            data,
                        })
                        .await;
                }
            }
        }

        Message::Output {
            src_host,
            dst_host,
            agent_id,
            data,
        } => {
            let our_host = {
                let state = state.read().await;
                state.config.host_id.clone()
            };

            let (route_to, new_dst) = resolve_route(&dst_host, &our_host);

            let route = {
                let state = state.read().await;
                state.routes.get(&route_to).cloned()
            };
            if let Some(route) = route {
                let _ = route
                    .send(Message::Output {
                        src_host,
                        dst_host: new_dst,
                        agent_id,
                        data,
                    })
                    .await;
            }
        }

        _ => {
            log!("server: unexpected message: {:?}", msg);
        }
    }

    Ok(())
}
