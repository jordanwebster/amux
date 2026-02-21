use crate::agent_registry::AgentRegistry;
use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::jwt::JwtValidator;
use crate::message::{HostInfo, Message};
use crate::route::Route;
use crate::session::{LocalAgentSession, SessionEvent};
use crate::transport::{TcpTransport, create_tls_acceptor};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

mod accept;
mod cloud;
mod connection;
mod routing;

use accept::{tcp_accept, unix_accept, websocket_accept};
use cloud::establish_cloud_connection;
use routing::broadcast_to_peers;

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
    /// Destination route for this stream (used for matching NoRouteFound errors)
    pub dst: Route,
    /// Link name this stream sends through (used for teardown cancellation)
    pub link: String,
}

/// Per-user state. Each authenticated user gets isolated agents, routes,
/// registry, peer links, and streams. JWT authentication at connection time
/// determines the user_id; all operations are scoped to that user's state.
/// This provides complete user isolation without per-message authorization checks.
pub(super) struct ServerUserState {
    pub(super) agents: HashMap<Uuid, Arc<LocalAgentSession>>,
    /// Per-user routing table. Link names are globally unique (random suffixes).
    /// Per-user for security: prevents cross-user message forwarding without
    /// explicit authorization.
    pub(super) routes: HashMap<String, mpsc::Sender<Message>>,
    /// Centralized agent registry (local + remote agents, alias mapping)
    pub(super) registry: AgentRegistry,
    /// Link names of peer connections (non-local connections that receive announcements)
    pub(super) peer_links: HashSet<String>,
    /// Known remote hosts (announced via AnnounceHost)
    pub(super) hosts: HashMap<Uuid, HostInfo>,
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
}

impl ServerState {
    fn new(config: Config) -> Self {
        let mut users = HashMap::new();
        users.insert(LOCAL_USER_ID, Arc::new(RwLock::new(ServerUserState::new())));
        Self {
            config,
            host_id: Uuid::new_v4(),
            is_cloud_server: false,
            jwt_validator: None,
            users,
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

        // Unix socket - always available (for CLI commands like list-agents, kill-server)
        let _ = std::fs::remove_file(&socket_path);
        let unix_listener = UnixListener::bind(&socket_path)?;
        tracing::info!(path = %socket_path.display(), "listening on unix socket");

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
                        tracing::info!(agent_id = %agent_id, "session ended");
                        let user_state = {
                            let s = state.read().await;
                            s.get_user_state(&user_id)
                        };
                        if let Some(user_state) = user_state {
                            let mut us = user_state.write().await;
                            us.registry.remove(&agent_id);
                            us.agents.remove(&agent_id);
                            broadcast_to_peers(
                                &mut us,
                                &crate::message::LocalMessage::WithdrawAgent { agent_id },
                                None,
                            );
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

        loop {
            tokio::select! {
                // Unix socket connection
                result = unix_listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = unix_accept(stream, state, event_tx).await {
                                    tracing::debug!(error = %e, "unix connection error");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "unix accept error");
                            break;
                        }
                    }
                }
                // TCP connection - TLS in cloud mode, plain in local mode
                result = tcp_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
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
                                    match acceptor.accept(stream).await {
                                        Ok(tls_stream) => {
                                            let transport = TcpTransport::new(tls_stream);
                                            if let Err(e) = tcp_accept(transport, state, event_tx, verify_token).await {
                                                tracing::debug!(error = %e, "TLS TCP connection error");
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(peer = %addr, error = %e, "TLS handshake failed");
                                        }
                                    }
                                });
                            } else {
                                tokio::spawn(async move {
                                    let transport = TcpTransport::new(stream);
                                    if let Err(e) = tcp_accept(transport, state, event_tx, verify_token).await {
                                        tracing::debug!(error = %e, "TCP connection error");
                                    }
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
                            crate::transport::configure_tcp_keepalive(&stream);
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            let verify_token = is_cloud_server;
                            tokio::spawn(async move {
                                if let Err(e) = websocket_accept(stream, state, event_tx, verify_token).await {
                                    tracing::debug!(error = %e, "websocket connection error");
                                }
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

        Ok(())
    }
}
