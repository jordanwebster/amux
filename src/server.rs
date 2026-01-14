use crate::buffer::MultiplexReader;
use crate::config::{Config, DEFAULT_TCP_PORT};
use crate::connection::ConnectionId;
use crate::error::{AmuxError, Result};
use crate::message::Message;
use crate::session::{AgentId, LocalAgentSession, SessionEvent};
use crate::transport::{TcpTransport, Transport, UnixTransport};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};

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
    next_connection_id: u64,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            agents: HashMap::new(),
            routes: HashMap::new(),
            next_connection_id: 0,
        }
    }

    fn next_conn_id(&mut self) -> ConnectionId {
        let id = ConnectionId(self.next_connection_id);
        self.next_connection_id += 1;
        id
    }
}

/// The amux server
pub struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
}

impl Server {
    /// Create a new server with default config
    pub fn new() -> Self {
        Self::with_config(Config::new())
    }

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
        let (socket_path, tcp_port) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port.unwrap_or(DEFAULT_TCP_PORT),
            )
        };

        // Clean up old socket
        let _ = std::fs::remove_file(&socket_path);

        let unix_listener = UnixListener::bind(&socket_path)?;
        log!("server: listening on {:?}", socket_path);

        // Start TCP listener
        let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_port));
        let tcp_listener = TcpListener::bind(tcp_addr).await?;
        log!("server: listening on TCP {}", tcp_addr);

        // Take the event receiver
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
                                if let Err(e) = handle_connection(stream, state, event_tx).await {
                                    log!("server: connection error: {}", e);
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
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_inbound_tcp(stream, state, event_tx).await {
                                    log!("server: TCP connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            log!("server: tcp accept error: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Handle a single connection
async fn handle_connection(
    stream: UnixStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let conn_id = {
        let mut state = state.write().await;
        state.next_conn_id()
    };

    log!("server: {} connected, waiting for handshake", conn_id);

    let mut transport = UnixTransport::new(stream);

    // Wait for Connect message from client
    let msg = transport.read_message().await?;
    let local_client_id = match msg {
        Message::Connect { host_id } => host_id,
        _ => {
            log!("server: {} expected Connect, got {:?}", conn_id, msg);
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

    // Send ConnectResponse with our host_id
    transport
        .write_message(&Message::ConnectResponse {
            success: true,
            error: None,
            host_id: our_host.clone(),
        })
        .await?;

    log!(
        "server: {} handshake complete, client_host_id: {}",
        conn_id,
        client_host_id
    );

    // Create channel for outgoing messages to this client
    // Routes table uses single-layer keys only (no "/" in keys)
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(local_client_id.clone(), outgoing_tx);
    }

    // Message handling loop
    // Pass both the local client ID (for routing) and full hierarchical ID (for src_host rewriting)
    let result = handle_unix_client_loop(
        transport,
        outgoing_rx,
        &local_client_id,
        &client_host_id,
        conn_id,
        state.clone(),
        event_tx,
    )
    .await;

    // Clean up route on disconnect (use local_client_id since that's what's in routes)
    {
        let mut state = state.write().await;
        state.routes.remove(&local_client_id);
        log!("server: removed route to {}", local_client_id);
    }

    result
}

/// Handle the message loop for a Unix client
///
/// - `transport`: The transport to the client (owned, not shared)
/// - `outgoing_rx`: Channel receiver for messages to send to this client (from routing)
/// - `local_client_id`: The client's local ID (used as key in routes table)
/// - `client_host_id`: The full hierarchical ID (our_host/local_client_id, used for src_host)
async fn handle_unix_client_loop(
    mut transport: UnixTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    local_client_id: &str,
    client_host_id: &str,
    conn_id: ConnectionId,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let _ = local_client_id; // Used for routes cleanup in caller

    loop {
        // Select between incoming messages from client and outgoing messages from routes
        tokio::select! {
            // Incoming message from client
            msg = transport.read_message() => {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        log!("server: {} disconnected", conn_id);
                        return Ok(());
                    }
                    Err(e) => {
                        log!("server: {} read error: {}", conn_id, e);
                        return Err(e);
                    }
                };

                log!("server: {} received {:?}", conn_id, msg);

                match msg {
                    Message::ListAgents => {
                        let agents = {
                            let state = state.read().await;
                            state
                                .agents
                                .values()
                                .map(|s| s.to_agent_info())
                                .collect::<Vec<_>>()
                        };
                        transport
                            .write_message(&Message::ListAgentsResult { agents })
                            .await?;
                    }

                    Message::CreateAgent {
                        agent_id,
                        command,
                        working_dir,
                        rows,
                        cols,
                    } => {
                        let result = create_agent(
                            &state,
                            &event_tx,
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
                    }

                    Message::Subscribe {
                        src_host: _,
                        dst_host,
                        agent_id,
                        rows,
                        cols,
                    } => {
                        // Rewrite src_host to client's full hierarchical ID
                        let src_host = client_host_id.to_string();
                        let our_host = {
                            let state = state.read().await;
                            state.config.host_id.clone()
                        };

                        // Check if this subscribe is for a local agent or needs routing
                        if dst_host != our_host {
                            // Forward to remote host with client's host_id as src
                            log!(
                                "server: {} forwarding Subscribe to {} (src={})",
                                conn_id,
                                dst_host,
                                src_host
                            );
                            let route = {
                                let state = state.read().await;
                                state.routes.get(&dst_host).cloned()
                            };

                            if let Some(route) = route {
                                log!("server: {} found route to {}", conn_id, dst_host);
                                if route
                                    .send(Message::Subscribe {
                                        src_host,
                                        dst_host: dst_host.clone(),
                                        agent_id,
                                        rows,
                                        cols,
                                    })
                                    .await
                                    .is_ok()
                                {
                                    log!("server: {} forwarded Subscribe to {}", conn_id, dst_host);
                                }
                                // Response will come back through handle_tcp_connection
                                // and be routed to this client via outgoing_rx
                            } else {
                                log!(
                                    "server: {} no route to {}, sending error",
                                    conn_id,
                                    dst_host
                                );
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
                            continue;
                        }

                        // Local subscribe - enter subscribed mode
                        let result = handle_subscribe(&state, &agent_id, rows, cols).await;

                        match result {
                            Ok((buffer_reader, input_tx)) => {
                                // Send success response
                                transport
                                    .write_message(&Message::SubscribeResult {
                                        src_host: our_host.clone(),
                                        dst_host: src_host.clone(),
                                        agent_id: agent_id.clone(),
                                        success: true,
                                        error: None,
                                    })
                                    .await?;

                                log!("server: {} entering subscribed mode", conn_id);

                                // Enter subscribed mode with dedicated loop
                                return handle_subscribed_mode(
                                    transport,
                                    outgoing_rx,
                                    buffer_reader,
                                    input_tx,
                                    our_host,
                                    src_host,
                                    agent_id,
                                    conn_id,
                                )
                                .await;
                            }
                            Err(e) => {
                                transport
                                    .write_message(&Message::SubscribeResult {
                                        src_host: our_host,
                                        dst_host: src_host,
                                        agent_id,
                                        success: false,
                                        error: Some(e.to_string()),
                                    })
                                    .await?;
                            }
                        }
                    }

                    Message::Shutdown => {
                        log!("server: {} requested shutdown", conn_id);
                        shutdown_server(&state).await;
                        transport
                            .write_message(&Message::Error {
                                code: 0,
                                message: "Server shutting down".to_string(),
                            })
                            .await?;

                        // Remove socket and exit
                        let socket_path = {
                            let state = state.read().await;
                            state.config.socket_path.clone()
                        };
                        let _ = std::fs::remove_file(socket_path);
                        log!("server: exiting");
                        std::process::exit(0);
                    }

                    Message::ConnectToServer { address } => {
                        let result = handle_connect_to_server(&address, &state, &event_tx).await;
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
                    }

                    // Input from client - forward to remote host if subscribed remotely
                    Message::Input {
                        dst_host,
                        agent_id,
                        data,
                        ..
                    } => {
                        // Forward input to the destination
                        let route = {
                            let state = state.read().await;
                            state.routes.get(&dst_host).cloned()
                        };
                        if let Some(route) = route {
                            let _ = route
                                .send(Message::Input {
                                    src_host: client_host_id.to_string(),
                                    dst_host,
                                    agent_id,
                                    data,
                                })
                                .await;
                        }
                    }

                    _ => {
                        transport
                            .write_message(&Message::Error {
                                code: 1,
                                message: "Unexpected message".to_string(),
                            })
                            .await?;
                    }
                }
            }

            // Outgoing message from routing (e.g., SubscribeResult, Output from remote)
            Some(msg) = outgoing_rx.recv() => {
                log!("server: {} sending routed message {:?}", conn_id, msg);
                if transport.write_message(&msg).await.is_err() {
                    log!("server: {} failed to send routed message", conn_id);
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Handle subscribed mode - streaming output to client
#[allow(clippy::too_many_arguments)]
async fn handle_subscribed_mode(
    mut transport: UnixTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    mut buffer_reader: crate::buffer::MultiplexReader,
    input_tx: mpsc::Sender<Vec<u8>>,
    our_host: String,
    client_host: String,
    agent_id: String,
    conn_id: ConnectionId,
) -> Result<()> {
    loop {
        tokio::select! {
            // PTY output ready → send to client
            output = buffer_reader.read() => {
                match output {
                    Some(data) => {
                        if transport.write_message(&Message::Output {
                            src_host: our_host.clone(),
                            dst_host: client_host.clone(),
                            agent_id: agent_id.clone(),
                            data,
                        }).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        log!("server: {} session ended", conn_id);
                        let _ = transport.write_message(&Message::AgentEnded).await;
                        break;
                    }
                }
            }

            // Client message (input)
            msg = transport.read_message() => {
                match msg {
                    Ok(Message::Input { data, .. }) => {
                        let _ = input_tx.send(data).await;
                    }
                    Err(_) => {
                        log!("server: {} client disconnected", conn_id);
                        break;
                    }
                    _ => {} // Ignore unexpected messages
                }
            }

            // Outgoing message from routing (shouldn't happen in subscribed mode, but handle it)
            Some(msg) = outgoing_rx.recv() => {
                if transport.write_message(&msg).await.is_err() {
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

/// Handle client request to connect to a remote server
async fn handle_connect_to_server(
    address: &str,
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
) -> Result<()> {
    // Parse address
    let addr: SocketAddr = address
        .parse()
        .map_err(|_| AmuxError::Config(format!("Invalid address: {}", address)))?;

    // Connect to remote server
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;

    log!("server: connected to remote server at {}", addr);

    // Perform handshake as initiator
    let mut transport = TcpTransport::new(stream);
    let our_host = {
        let state = state.read().await;
        state.config.host_id.clone()
    };

    // Send Connect with our host_id
    transport
        .write_message(&Message::Connect {
            host_id: our_host.clone(),
        })
        .await?;

    // Expect ConnectResponse
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

    // Create channel for outgoing messages and register in routes
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(remote_host.clone(), outgoing_tx);
    }

    let state = state.clone();
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = handle_tcp_connection(
            transport,
            outgoing_rx,
            remote_host.clone(),
            state.clone(),
            event_tx,
        )
        .await
        {
            log!("server: TCP connection error: {}", e);
        }
        // Clean up route on disconnect
        let mut state = state.write().await;
        state.routes.remove(&remote_host);
        log!("server: removed route to {}", remote_host);
    });

    Ok(())
}

/// Handle incoming TCP connection (from accept loop)
async fn handle_inbound_tcp(
    stream: TcpStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    stream.set_nodelay(true)?;
    let peer_addr = stream.peer_addr().ok();
    log!("server: inbound TCP from {:?}", peer_addr);

    let mut transport = TcpTransport::new(stream);
    let our_host = {
        let state = state.read().await;
        state.config.host_id.clone()
    };

    // Expect Connect as first message
    let msg = transport.read_message().await?;
    let remote_host = match msg {
        Message::Connect { host_id } => host_id,
        _ => {
            log!("server: expected Connect, got {:?}, closing", msg);
            return Err(AmuxError::InvalidMessage);
        }
    };

    // Send success response with our host_id
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

    // Create channel for outgoing messages and register in routes
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(remote_host.clone(), outgoing_tx);
    }

    let result = handle_tcp_connection(
        transport,
        outgoing_rx,
        remote_host.clone(),
        state.clone(),
        event_tx,
    )
    .await;

    // Clean up route on disconnect
    {
        let mut state = state.write().await;
        state.routes.remove(&remote_host);
        log!("server: removed route to {}", remote_host);
    }

    result
}

/// Handle an established TCP connection with another server
///
/// This is called after the handshake is complete, on both the initiator
/// and receiver sides. It routes messages between this server and the remote.
async fn handle_tcp_connection(
    mut transport: TcpTransport,
    mut outgoing_rx: mpsc::Receiver<Message>,
    remote_host: String,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    log!("server: handling TCP connection with {}", remote_host);
    let _ = event_tx; // May be used later for session events

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

                if let Err(e) = handle_tcp_message(msg, &mut transport, &state).await {
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

/// Handle a single message received on a TCP connection
async fn handle_tcp_message(
    msg: Message,
    transport: &mut TcpTransport,
    state: &Arc<RwLock<ServerState>>,
) -> Result<()> {
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

    let _ = transport; // Used for potential direct responses
    Ok(())
}
