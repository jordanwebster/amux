//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts Unix, TCP, and WebSocket listeners concurrently and spawns per-connection
//! tasks via [`accept`]. Shared state is `Arc<RwLock<ServerState>>` with per-user
//! isolation in [`ServerUserState`] (keyed by JWT-derived user ID, or [`LOCAL_USER_ID`]
//! for unauthenticated local connections).

use crate::agent_registry::AgentRegistry;
use crate::agents::{AgentSession, SessionEvent};
use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::jwt::JwtValidator;
use crate::message::{AgentType, Command, Host, Message, ProtocolError, ShutdownReason};
use crate::route::Route;
use crate::transport::{TcpTransport, TransportSplit, create_tls_acceptor};
use routing::{shutdown_server, suspend_server};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore, mpsc, oneshot};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

/// Request from a connection handler to shut down or suspend the server.
/// Sent to `Server::run()` via `ServerState::shutdown_tx` so that the main
/// loop can orchestrate cleanup (stop/suspend agents, remove socket, grace
/// period) instead of `process::exit` from within a spawned connection task.
pub(super) enum ShutdownRequest {
    Shutdown { reply: mpsc::Sender<Message> },
    Suspend { reply: mpsc::Sender<Message> },
}

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent network connections (TCP + WebSocket).
/// Safety net against resource exhaustion. Each network connection holds a
/// semaphore permit for its lifetime; new connections are rejected at capacity.
const MAX_CONNECTIONS: usize = 16384;

/// Platform-abstracted local IPC listener (Unix socket on Unix, named pipe on Windows).
pub(crate) struct LocalListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    pipe_name: String,
}

impl LocalListener {
    /// Bind to the local transport (Unix socket or named pipe).
    pub fn bind(socket_path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            if let Some(parent) = socket_path.parent()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            let _ = std::fs::remove_file(socket_path);
            let listener = tokio::net::UnixListener::bind(socket_path)?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(Self { inner: listener })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                pipe_name: socket_path.to_string_lossy().into_owned(),
            })
        }
    }

    /// Accept one incoming connection, returning a transport that implements TransportSplit.
    pub async fn accept(&self) -> std::io::Result<impl TransportSplit + use<>> {
        #[cfg(unix)]
        {
            let (stream, _) = self.inner.accept().await?;
            Ok(crate::transport::unix::UnixTransport::new(stream))
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let server = ServerOptions::new().create(&self.pipe_name)?;
            server.connect().await?;
            Ok(crate::transport::named_pipe::NamedPipeTransport::new(
                server,
            ))
        }
    }
}

mod accept;
mod cloud;
mod connection;
mod handlers;
mod routing;

pub use accept::connect_handshake;
use accept::{local_accept, tcp_accept, websocket_accept};
use cloud::establish_cloud_connection;
use routing::withdraw_agent;

/// Default user for non-authenticated connections. Local amux servers are
/// single-user: all state (agents, routes, registry) lives under this ID.
/// User isolation is enforced on the cloud server via JWT authentication.
pub(crate) const LOCAL_USER_ID: Uuid = Uuid::nil();

/// An active stream (Output or StructuredOutput) that can be cancelled.
/// Dropping the entry drops `cancel`, which signals the stream task via `oneshot::Receiver`.
pub(super) struct StreamEntry {
    pub stream_id: u64,
    #[allow(dead_code)] // Held for drop: dropping Sender cancels the oneshot Receiver
    pub cancel: oneshot::Sender<()>,
    /// Destination route for this stream
    pub dst: Route,
    /// Link name this stream sends through (used for teardown cancellation)
    pub link: String,
}

/// Per-user state. Each authenticated user gets isolated agents, routes,
/// registry, peer links, and streams. JWT authentication at connection time
/// determines the user_id; all operations are scoped to that user's state.
/// This provides complete user isolation without per-message authorization checks.
pub(super) struct ServerUserState {
    pub(super) agents: HashMap<Uuid, AgentSession>,
    /// Per-user routing table. Link names are globally unique (random suffixes).
    /// Per-user for security: prevents cross-user message forwarding without
    /// explicit authorization.
    pub(super) routes: HashMap<String, mpsc::Sender<Message>>,
    /// Centralized agent registry (local + remote agents, name mapping)
    pub(super) registry: AgentRegistry,
    /// Link names of peer connections (non-local connections that receive announcements)
    pub(super) peer_links: HashSet<String>,
    /// Known remote hosts (announced via AnnounceHost)
    pub(super) hosts: HashMap<Uuid, Host>,
    /// Active streaming tasks keyed by agent_id, with cancellation tokens
    pub(super) active_streams: HashMap<Uuid, Vec<StreamEntry>>,
    pub(super) next_stream_id: u64,
}

impl ServerUserState {
    fn new() -> Self {
        Self {
            agents: HashMap::new(),
            routes: HashMap::new(),
            registry: AgentRegistry::new(),
            peer_links: HashSet::new(),
            hosts: HashMap::new(),
            active_streams: HashMap::new(),
            next_stream_id: 0,
        }
    }
}

/// Global server state. Per-user state (agents, routes, registry, streams, peer_links)
/// lives in `ServerUserState`, providing user isolation via JWT authentication.
pub(super) struct ServerState {
    pub(super) config: Config,
    /// Ephemeral host ID generated at startup (not persisted)
    pub(super) host_id: Uuid,
    /// Whether this server is a cloud relay (`amux serve --cloud`)
    pub(super) is_cloud_server: bool,
    /// JWT validator for cloud mode (validates incoming tokens)
    pub(super) jwt_validator: Option<Arc<JwtValidator>>,
    /// Per-user state map. Each authenticated user gets isolated state.
    pub(super) users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    /// Channel for connection handlers to request server shutdown/suspend
    pub(super) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

impl ServerState {
    fn new(config: Config, shutdown_tx: mpsc::Sender<ShutdownRequest>) -> Self {
        let mut users = HashMap::new();
        users.insert(LOCAL_USER_ID, Arc::new(RwLock::new(ServerUserState::new())));
        Self {
            config,
            host_id: Uuid::new_v4(),
            is_cloud_server: false,
            jwt_validator: None,
            users,
            shutdown_tx,
        }
    }

    /// Look up existing user state (read-only, no creation).
    pub(super) fn get_user_state(&self, user_id: &Uuid) -> Option<Arc<RwLock<ServerUserState>>> {
        self.users.get(user_id).cloned()
    }
}

/// Get or create per-user state. Tries a read lock first (fast path for existing
/// users), falling back to a write lock only when the user_id is seen for the
/// first time.
pub(super) async fn get_or_create_user_state(
    state: &Arc<RwLock<ServerState>>,
    user_id: Uuid,
) -> Arc<RwLock<ServerUserState>> {
    // Fast path: user already exists
    {
        let s = state.read().await;
        if let Some(us) = s.users.get(&user_id) {
            return us.clone();
        }
    }
    // Slow path: create under write lock, re-check for races
    let mut s = state.write().await;
    s.users
        .entry(user_id)
        .or_insert_with(|| Arc::new(RwLock::new(ServerUserState::new())))
        .clone()
}

/// The amux server
pub struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
    shutdown_rx: Option<mpsc::Receiver<ShutdownRequest>>,
}

impl Server {
    pub fn with_config(config: Config) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        Self {
            state: Arc::new(RwLock::new(ServerState::new(config, shutdown_tx))),
            event_tx,
            event_rx: Some(event_rx),
            shutdown_rx: Some(shutdown_rx),
        }
    }

    /// Run the server
    ///
    /// If `is_cloud_server` is true, the server runs as a cloud relay:
    /// - TCP connections use TLS
    /// - All connections require valid JWT tokens
    pub async fn run(&mut self, is_cloud_server: bool) -> Result<()> {
        let (socket_path, tcp_port, ws_port, cloud_url, enforce_tls) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port,
                state.config.websocket_port,
                state.config.cloud_url.clone(),
                state.config.enforce_tls_in_cloud_mode,
            )
        };

        // Configure cloud server: enable JWT validation (and optionally TLS)
        let tls_acceptor: Option<TlsAcceptor> = if is_cloud_server {
            let mut state = self.state.write().await;
            state.is_cloud_server = true;
            state.jwt_validator = Some(Arc::new(JwtValidator::new(&cloud_url)));

            if enforce_tls {
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
                tracing::info!("TLS configured for cloud mode");
                Some(acceptor)
            } else {
                tracing::info!("cloud mode with external TLS termination");
                None
            }
        } else {
            None
        };

        let local_listener = LocalListener::bind(&socket_path)?;
        tracing::info!(path = %socket_path.display(), "listening on local transport");

        let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_port));
        let tcp_listener = TcpListener::bind(tcp_addr).await?;
        if is_cloud_server && enforce_tls {
            tracing::info!(addr = %tcp_addr, "listening on TLS TCP");
        } else if is_cloud_server {
            tracing::info!(addr = %tcp_addr, "listening on TCP (external TLS)");
        } else {
            tracing::info!(addr = %tcp_addr, "listening on TCP");
        }

        let ws_addr = SocketAddr::from(([0, 0, 0, 0], ws_port));
        let ws_listener = TcpListener::bind(ws_addr).await?;
        tracing::info!(addr = %ws_addr, "listening on WebSocket");

        let mut event_rx = self.event_rx.take().expect("run() called twice");

        // Task: Handle session lifecycle events
        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    SessionEvent::Ended { agent_id, user_id } => {
                        let user_state = {
                            let s = state.read().await;
                            s.get_user_state(&user_id)
                        };
                        if let Some(user_state) = user_state {
                            let mut us = user_state.write().await;
                            withdraw_agent(&mut us, agent_id);
                        }
                    }
                    SessionEvent::Created {
                        agent_id,
                        user_id,
                        agent_type,
                        args,
                    } => {
                        // Check if this agent was forked from a readonly session
                        if matches!(agent_type, AgentType::Claude)
                            && args.contains(&"--fork-session".to_string())
                            && let Some(pos) = args.iter().position(|a| a == "--resume")
                            && let Some(source_id_str) = args.get(pos + 1)
                            && let Ok(source_id) = source_id_str.parse::<Uuid>()
                        {
                            let user_state = {
                                let s = state.read().await;
                                s.get_user_state(&user_id)
                            };
                            if let Some(user_state) = user_state {
                                let mut us = user_state.write().await;
                                let is_readonly =
                                    us.agents.get(&source_id).is_some_and(|s| s.readonly());
                                if is_readonly {
                                    withdraw_agent(&mut us, source_id);
                                    tracing::info!(
                                        source = %source_id,
                                        fork = %agent_id,
                                        "withdrew readonly session (forked)"
                                    );
                                }
                            }
                        }
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
            establish_cloud_connection(config, self.state.clone(), self.event_tx.clone());
        }

        // Network connection limiter (TCP + WebSocket): each connection holds a
        // permit for its lifetime. Unix socket control-path connections are not
        // limited so local admin commands still work under network saturation.
        let network_conn_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        tracing::info!(
            max_connections = MAX_CONNECTIONS,
            "network connection limit active"
        );

        let mut shutdown_rx = self.shutdown_rx.take().expect("run() called twice");

        // Deferred reply: built inside the select arm, sent after listener teardown
        // so clients can't reconnect to the old socket before it's removed.
        let mut deferred_reply: Option<(mpsc::Sender<Message>, Message)> = None;

        loop {
            tokio::select! {
                // Shutdown/suspend request from a connection handler
                Some(req) = shutdown_rx.recv() => {
                    match req {
                        ShutdownRequest::Shutdown { reply } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.get_user_state(&LOCAL_USER_ID).unwrap()
                            };
                            shutdown_server(&user_state).await;
                            deferred_reply = Some((
                                reply,
                                Message::Command(Command::ShutdownNotification(
                                    ShutdownReason::UserRequested,
                                )),
                            ));
                        }
                        ShutdownRequest::Suspend { reply } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.get_user_state(&LOCAL_USER_ID).unwrap()
                            };
                            let (suspended, errors) = suspend_server(&user_state).await;
                            let suspended_count = suspended.agents.len();
                            let error = if !errors.is_empty() {
                                Some(ProtocolError::ServerError(errors.join("; ")))
                            } else {
                                None
                            };
                            if !suspended.agents.is_empty() {
                                let state_path = {
                                    let state = self.state.read().await;
                                    state.config.state_path.clone()
                                };
                                if let Err(e) =
                                    crate::state::save_suspended(&state_path, &suspended)
                                {
                                    tracing::error!(error = %e, "failed to save suspended agents");
                                    let _ = reply
                                        .send(Message::Command(Command::SuspendResult {
                                            suspended_count: 0,
                                            error: Some(ProtocolError::ServerError(format!(
                                                "failed to save state: {e}"
                                            ))),
                                        }))
                                        .await;
                                    // Don't shut down on save failure
                                    continue;
                                }
                            }
                            deferred_reply = Some((
                                reply,
                                Message::Command(Command::SuspendResult {
                                    suspended_count,
                                    error,
                                }),
                            ));
                        }
                    }
                    break;
                }
                // Local transport connection
                result = local_listener.accept() => {
                    match result {
                        Ok(transport) => {
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                let _ = local_accept(transport, state, event_tx).await;
                            });
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "local accept error");
                            break;
                        }
                    }
                }
                // TCP connection - TLS in cloud mode, plain in local mode
                result = tcp_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            let permit = match network_conn_limit.clone().try_acquire_owned() {
                                Ok(p) => p,
                                Err(_) => {
                                    tracing::warn!("network connection limit reached, dropping TCP connection");
                                    drop(stream);
                                    continue;
                                }
                            };
                            if let Err(e) = stream.set_nodelay(true) {
                                tracing::warn!(error = %e, "failed to set TCP_NODELAY");
                            }
                            crate::transport::configure_tcp_keepalive(&stream);

                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            let verify_token = is_cloud_server;
                            if let Some(ref acceptor) = tls_acceptor {
                                let acceptor = acceptor.clone();
                                tokio::spawn(async move {
                                    let tls_result = tokio::time::timeout(
                                        TLS_HANDSHAKE_TIMEOUT,
                                        acceptor.accept(stream),
                                    ).await;
                                    match tls_result {
                                        Ok(Ok(tls_stream)) => {
                                            let transport = TcpTransport::new(tls_stream);
                                            let _ = tcp_accept(transport, state, event_tx, verify_token).await;
                                            drop(permit);
                                        }
                                        Ok(Err(e)) => {
                                            drop(permit);
                                            tracing::warn!(peer = %addr, error = %e, "TLS handshake failed");
                                        }
                                        Err(_) => {
                                            drop(permit);
                                            tracing::warn!(peer = %addr, "TLS handshake timed out");
                                        }
                                    }
                                });
                            } else {
                                tokio::spawn(async move {
                                    let transport = TcpTransport::new(stream);
                                    let _ = tcp_accept(transport, state, event_tx, verify_token).await;
                                    drop(permit);
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "TCP accept error");
                            break;
                        }
                    }
                }
                // WebSocket connection
                result = ws_listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            let permit = match network_conn_limit.clone().try_acquire_owned() {
                                Ok(p) => p,
                                Err(_) => {
                                    tracing::warn!("network connection limit reached, dropping WebSocket connection");
                                    drop(stream);
                                    continue;
                                }
                            };
                            crate::transport::configure_tcp_keepalive(&stream);
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            let verify_token = is_cloud_server;
                            tokio::spawn(async move {
                                let _ = websocket_accept(stream, state, event_tx, verify_token).await;
                                drop(permit);
                            });
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "websocket accept error");
                            break;
                        }
                    }
                }
            }
        }

        // Stop accepting connections and remove socket before replying, so
        // clients can't reconnect to the old server after receiving the response.
        drop(local_listener);
        drop(tcp_listener);
        drop(ws_listener);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&socket_path);

        // Send reply on the existing connection (writer task is still alive)
        if let Some((reply, msg)) = deferred_reply {
            let _ = reply.send(msg).await;
        }

        // Grace period: let agents handle SIGHUP from PTY master drop
        tokio::time::sleep(Duration::from_millis(200)).await;
        tracing::info!("server exiting");

        Ok(())
    }
}

#[cfg(test)]
pub(super) mod test_helpers {
    use super::connection::ConnectionContext;
    use super::{LOCAL_USER_ID, ServerState, ServerUserState};
    use crate::config::Config;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::{RwLock, mpsc};

    pub(super) fn test_ctx(
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
            next_request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) async fn test_state() -> (Arc<RwLock<ServerState>>, Arc<RwLock<ServerUserState>>) {
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            shutdown_tx,
        )));
        let user_state = {
            let s = state.read().await;
            s.get_user_state(&LOCAL_USER_ID).unwrap()
        };
        (state, user_state)
    }
}
