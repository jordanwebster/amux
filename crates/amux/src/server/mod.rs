//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts Unix, TCP, and WebSocket listeners concurrently and spawns per-connection
//! tasks via [`accept`]. Shared state is `Arc<RwLock<ServerState>>` with per-user
//! isolation in [`ServerUserState`] (keyed by JWT-derived user ID, or [`LOCAL_USER_ID`]
//! for unauthenticated local connections).

use crate::agent_registry::AgentRegistry;
use crate::agents::{AgentSession, SessionEvent, StopPolicy};
use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::jwt::JwtValidator;
use crate::message::{
    AgentType, Command, Host, Message, ProtocolError, RoutableMessage, ShutdownReason,
    SubscriptionCloseReason, SubscriptionId,
};
use crate::route::Route;
use crate::transport::{TcpTransport, TransportSplit, create_tls_acceptor};
use routing::{apply_local_name_candidate, shutdown_server, suspend_server};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

/// Request from a connection handler to shut down or suspend the server.
/// Sent to `Server::run()` via `ServerState::shutdown_tx` so that the main
/// loop can orchestrate cleanup (stop/suspend agents, remove socket, grace
/// period) instead of `process::exit` from within a spawned connection task.
pub(super) enum ShutdownRequest {
    Shutdown {
        reply: mpsc::Sender<Message>,
        link_name: String,
    },
    Suspend {
        reply: mpsc::Sender<Message>,
        link_name: String,
    },
}

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent network connections (TCP + WebSocket).
/// Safety net against resource exhaustion. Each network connection holds a
/// semaphore permit for its lifetime; new connections are rejected at capacity.
const MAX_CONNECTIONS: usize = 16384;

pub(super) const SUBSCRIPTION_LEASE_DURATION: Duration = Duration::from_secs(300);
const SUBSCRIPTION_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscriptionMode {
    Raw,
    Structured,
}

impl SubscriptionMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Structured => "structured",
        }
    }
}

#[derive(Clone)]
pub(super) struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    next_request_id: Arc<AtomicU64>,
}

impl ConnectionHandle {
    pub(super) fn new(tx: mpsc::Sender<Message>, next_request_id: Arc<AtomicU64>) -> Self {
        Self {
            tx,
            next_request_id,
        }
    }

    pub(super) fn sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    pub(super) fn request_counter(&self) -> Arc<AtomicU64> {
        self.next_request_id.clone()
    }

    pub(super) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) async fn send(
        &self,
        msg: Message,
    ) -> std::result::Result<(), mpsc::error::SendError<Message>> {
        self.tx.send(msg).await
    }

    pub(super) fn try_send(&self, msg: Message) -> bool {
        self.tx.try_send(msg).is_ok()
    }
}

/// An active subscription that can be cancelled.
/// Dropping the entry drops `cancel`, which signals the stream task via `oneshot::Receiver`.
pub(crate) struct SubscriptionEntry {
    pub subscription_id: SubscriptionId,
    pub agent_id: Uuid,
    pub mode: SubscriptionMode,
    #[allow(dead_code)] // Held for drop: dropping Sender cancels the oneshot Receiver
    pub cancel: oneshot::Sender<()>,
    /// Full destination route for this subscription, including the immediate next hop.
    pub dst: Route,
    pub lease_deadline: Instant,
}

/// Per-user state. Each authenticated user gets isolated agents, routes,
/// registry, peer links, and streams. JWT authentication at connection time
/// determines the user_id; all operations are scoped to that user's state.
/// This provides complete user isolation without per-message authorization checks.
pub(crate) struct ServerUserState {
    pub(crate) agents: HashMap<Uuid, AgentSession>,
    /// Per-user routing table. Link names are globally unique (random suffixes).
    /// Per-user for security: prevents cross-user message forwarding without
    /// explicit authorization.
    pub(crate) routes: HashMap<String, ConnectionHandle>,
    /// Centralized agent registry (local + remote agents, name mapping)
    pub(crate) registry: AgentRegistry,
    /// Link names of peer connections (non-local connections that receive announcements)
    pub(crate) peer_links: HashSet<String>,
    /// Known remote hosts (announced via AnnounceHost)
    pub(crate) hosts: HashMap<Uuid, Host>,
    /// Active subscriptions keyed by server-assigned subscription_id.
    pub(crate) active_subscriptions: HashMap<SubscriptionId, SubscriptionEntry>,
}

impl ServerUserState {
    fn new() -> Self {
        Self {
            agents: HashMap::new(),
            routes: HashMap::new(),
            registry: AgentRegistry::new(),
            peer_links: HashSet::new(),
            hosts: HashMap::new(),
            active_subscriptions: HashMap::new(),
        }
    }
}

pub(super) fn subscription_lease_ms() -> u64 {
    SUBSCRIPTION_LEASE_DURATION
        .as_millis()
        .try_into()
        .expect("subscription lease should fit in u64 milliseconds")
}

/// Global server state. Per-user state (agents, routes, registry, streams, peer_links)
/// lives in `ServerUserState`, providing user isolation via JWT authentication.
pub(crate) struct ServerState {
    pub(crate) config: Config,
    /// Stable host ID loaded from persistent state
    pub(crate) host_id: Uuid,
    /// Whether this server is a cloud relay (`amux serve --cloud`)
    pub(crate) is_cloud_server: bool,
    /// JWT validator for cloud mode (validates incoming tokens)
    pub(super) jwt_validator: Option<Arc<JwtValidator>>,
    /// Per-user state map. Each authenticated user gets isolated state.
    pub(crate) users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    /// Channel for connection handlers to request server shutdown/suspend
    pub(super) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

impl ServerState {
    fn new(config: Config, host_id: Uuid, shutdown_tx: mpsc::Sender<ShutdownRequest>) -> Self {
        let mut users = HashMap::new();
        users.insert(LOCAL_USER_ID, Arc::new(RwLock::new(ServerUserState::new())));
        Self {
            config,
            host_id,
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
    pub fn with_config(config: Config) -> Result<Self> {
        let host_id = crate::state::load_or_create_host_id(&config.state_path)
            .map_err(|e| AmuxError::Config(format!("failed to load host state: {e}")))?;
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        Ok(Self {
            state: Arc::new(RwLock::new(ServerState::new(config, host_id, shutdown_tx))),
            event_tx,
            event_rx: Some(event_rx),
            shutdown_rx: Some(shutdown_rx),
        })
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

        // Validate server-specific config (cloud requires tcp + ws ports)
        {
            let state = self.state.read().await;
            state.config.validate(is_cloud_server)?;
        }

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

        let tcp_listener = if let Some(port) = tcp_port {
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = TcpListener::bind(addr).await?;
            if is_cloud_server && enforce_tls {
                tracing::info!(addr = %addr, "listening on TLS TCP");
            } else if is_cloud_server {
                tracing::info!(addr = %addr, "listening on TCP (external TLS)");
            } else {
                tracing::info!(addr = %addr, "listening on TCP");
            }
            Some(listener)
        } else {
            None
        };

        let ws_listener = if let Some(port) = ws_port {
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = TcpListener::bind(addr).await?;
            tracing::info!(addr = %addr, "listening on WebSocket");
            Some(listener)
        } else {
            None
        };

        let mut event_rx = self.event_rx.take().expect("run() called twice");

        // Task: Handle session lifecycle events
        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                handle_session_event(&state, event).await;
            }
        });

        // Auto-connect to cloud (local server only, not cloud server)
        if !is_cloud_server {
            let config = {
                let state = self.state.read().await;
                state.config.clone()
            };
            establish_cloud_connection(config, self.state.clone(), self.event_tx.clone());

            // Task: Periodic update check (every hour)
            let state_path = {
                let state = self.state.read().await;
                state.config.state_path.clone()
            };
            crate::update::spawn_update_checker(
                cloud_url.clone(),
                env!("CARGO_PKG_VERSION").to_string(),
                Duration::from_secs(3600),
                state_path,
            );
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
        let mut subscription_sweep = tokio::time::interval(SUBSCRIPTION_SWEEP_INTERVAL);
        subscription_sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);
        subscription_sweep.tick().await;

        // Deferred reply: built inside the select arm, sent after listener teardown
        // so clients can't reconnect to the old socket before it's removed.
        let mut deferred_reply: Option<(mpsc::Sender<Message>, Message)> = None;

        loop {
            tokio::select! {
                _ = subscription_sweep.tick() => {
                    sweep_expired_subscriptions(&self.state).await;
                }
                // Shutdown/suspend request from a connection handler
                Some(req) = shutdown_rx.recv() => {
                    match req {
                        ShutdownRequest::Shutdown { reply, link_name } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.get_user_state(&LOCAL_USER_ID).unwrap()
                            };
                            // Notify before shutdown so clients see it before streams close
                            notify_other_clients(
                                &user_state,
                                &link_name,
                                ShutdownReason::UserRequested,
                            ).await;
                            shutdown_server(&user_state).await;
                            deferred_reply = Some((
                                reply,
                                Message::Command(Command::ShutdownNotification(
                                    ShutdownReason::UserRequested,
                                )),
                            ));
                        }
                        ShutdownRequest::Suspend { reply, link_name } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.get_user_state(&LOCAL_USER_ID).unwrap()
                            };
                            // Notify before suspend so clients see it before streams close
                            notify_other_clients(
                                &user_state,
                                &link_name,
                                ShutdownReason::Updating,
                            ).await;
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
                result = async {
                    match &tcp_listener {
                        Some(l) => l.accept().await,
                        None => std::future::pending().await,
                    }
                } => {
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
                result = async {
                    match &ws_listener {
                        Some(l) => l.accept().await,
                        None => std::future::pending().await,
                    }
                } => {
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

/// Best-effort notification for attached clients on shutdown/suspend.
///
/// This intentionally uses `try_send` so server shutdown does not block on a
/// slow or backlogged client link. Missing this notice only affects the exit
/// banner; update availability still comes from the marker file.
async fn notify_other_clients(
    user_state: &Arc<RwLock<ServerUserState>>,
    exclude_link: &str,
    reason: ShutdownReason,
) {
    let us = user_state.read().await;
    let msg = Message::Command(Command::ShutdownNotification(reason));
    for (link, handle) in &us.routes {
        if link == exclude_link {
            continue;
        }
        let _ = handle.try_send(msg.clone());
    }
}

pub(super) async fn send_routable_via_full_dst(
    user_state: &Arc<RwLock<ServerUserState>>,
    full_dst: &Route,
    message: &RoutableMessage,
) -> bool {
    let Some((src, dst)) = Route::send(full_dst.clone()) else {
        return false;
    };
    let Some(next_hop) = src.peek() else {
        return false;
    };

    let route_handle = {
        let us = user_state.read().await;
        us.routes.get(next_hop).cloned()
    };

    let Some(route_handle) = route_handle else {
        return false;
    };

    let request_id = route_handle.next_request_id();
    route_handle
        .send(Message::routable(src, dst, request_id, message))
        .await
        .is_ok()
}

pub(super) async fn try_send_routable_via_full_dst(
    user_state: &Arc<RwLock<ServerUserState>>,
    full_dst: &Route,
    message: &RoutableMessage,
) -> bool {
    let Some((src, dst)) = Route::send(full_dst.clone()) else {
        return false;
    };
    let Some(next_hop) = src.peek() else {
        return false;
    };

    let route_handle = {
        let us = user_state.read().await;
        us.routes.get(next_hop).cloned()
    };

    let Some(route_handle) = route_handle else {
        return false;
    };

    let request_id = route_handle.next_request_id();
    route_handle.try_send(Message::routable(src, dst, request_id, message))
}

pub(super) async fn sweep_expired_subscriptions(state: &Arc<RwLock<ServerState>>) {
    let user_states: Vec<_> = {
        let s = state.read().await;
        s.users.values().cloned().collect()
    };
    let now = Instant::now();

    for user_state in user_states {
        let expired = {
            let mut us = user_state.write().await;
            let expired_ids: Vec<_> = us
                .active_subscriptions
                .iter()
                .filter_map(|(subscription_id, entry)| {
                    (entry.lease_deadline <= now).then_some(*subscription_id)
                })
                .collect();

            expired_ids
                .into_iter()
                .filter_map(|subscription_id| us.active_subscriptions.remove(&subscription_id))
                .collect::<Vec<_>>()
        };

        for entry in expired {
            let SubscriptionEntry {
                subscription_id,
                agent_id,
                mode,
                cancel,
                dst,
                ..
            } = entry;
            drop(cancel);
            tracing::info!(
                subscription_id = %subscription_id,
                agent_id = %agent_id,
                mode = mode.as_str(),
                "subscription lease expired"
            );
            let _ = try_send_routable_via_full_dst(
                &user_state,
                &dst,
                &RoutableMessage::SubscriptionClosed {
                    subscription_id,
                    reason: SubscriptionCloseReason::LeaseExpired,
                },
            )
            .await;
        }
    }
}

async fn handle_session_event(state: &Arc<RwLock<ServerState>>, event: SessionEvent) {
    match event {
        SessionEvent::Ended { agent_id, user_id } => {
            let user_state = {
                let s = state.read().await;
                s.get_user_state(&user_id)
            };
            if let Some(user_state) = user_state {
                let mut us = user_state.write().await;
                let _ = withdraw_agent(&mut us, agent_id);
            }
        }
        SessionEvent::Created {
            agent_id,
            user_id,
            agent_type,
            args,
        } => {
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
                    let withdrawn_session = {
                        let mut us = user_state.write().await;
                        let is_readonly = us.agents.get(&source_id).is_some_and(|s| s.readonly());
                        if is_readonly {
                            withdraw_agent(&mut us, source_id)
                        } else {
                            None
                        }
                    };
                    if let Some(session) = withdrawn_session {
                        session.stop(StopPolicy::Interrupt).await;
                        tracing::info!(
                            source = %source_id,
                            fork = %agent_id,
                            "withdrew readonly session (forked)"
                        );
                    }
                }
            }
            tracing::debug!(agent_id = %agent_id, ?agent_type, ?args, "session created");
        }
        SessionEvent::NameCandidateChanged {
            agent_id,
            user_id,
            name,
            source,
        } => {
            let (user_state, host_id) = {
                let s = state.read().await;
                (s.get_user_state(&user_id), s.host_id)
            };
            if let Some(user_state) = user_state {
                let outcome = {
                    let mut us = user_state.write().await;
                    apply_local_name_candidate(&mut us, host_id, agent_id, name.clone(), source)
                };
                tracing::debug!(
                    agent_id = %agent_id,
                    candidate = %name,
                    ?source,
                    ?outcome,
                    "processed local name candidate"
                );
            }
        }
    }
}

#[cfg(test)]
pub(super) mod test_helpers {
    use super::connection::{ConnectionContext, HeartbeatRole};
    use super::{LOCAL_USER_ID, ServerState, ServerUserState};
    use crate::config::Config;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

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
            heartbeat_role: HeartbeatRole::Disabled,
            next_request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) async fn test_state() -> (Arc<RwLock<ServerState>>, Arc<RwLock<ServerUserState>>) {
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            Uuid::new_v4(),
            shutdown_tx,
        )));
        let user_state = {
            let s = state.read().await;
            s.get_user_state(&LOCAL_USER_ID).unwrap()
        };
        (state, user_state)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionHandle, ServerUserState, handle_session_event, test_helpers::test_state,
    };
    use crate::agents::{AgentSession, LocalAgentNameSource, SessionEvent};
    use crate::message::{AgentType, CreateAgentRequest, DirectMessage, Message};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    async fn add_peer(
        user_state: &Arc<RwLock<ServerUserState>>,
        link_name: &str,
    ) -> mpsc::Receiver<Message> {
        let (tx, rx) = mpsc::channel(16);
        let mut us = user_state.write().await;
        us.routes.insert(
            link_name.to_string(),
            ConnectionHandle::new(tx, Arc::new(AtomicU64::new(1))),
        );
        us.peer_links.insert(link_name.to_string());
        rx
    }

    async fn insert_local_claude(
        user_state: &Arc<RwLock<ServerUserState>>,
        host_id: Uuid,
        agent_id: Uuid,
        name: Option<&str>,
        source: LocalAgentNameSource,
    ) {
        insert_local_claude_with_args(user_state, host_id, agent_id, name, source, vec![]).await;
    }

    async fn insert_local_claude_with_args(
        user_state: &Arc<RwLock<ServerUserState>>,
        host_id: Uuid,
        agent_id: Uuid,
        name: Option<&str>,
        source: LocalAgentNameSource,
        args: Vec<String>,
    ) {
        let req = CreateAgentRequest {
            agent_id,
            name: name.map(str::to_owned),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args,
        };
        let mut session = AgentSession::Claude(crate::agents::ClaudeSession::new(&req));
        session.set_local_name(name.map(str::to_owned), source);
        let info = session.to_agent(host_id);

        let mut us = user_state.write().await;
        us.agents.insert(agent_id, session);
        us.registry.register_local(info).unwrap();
    }

    #[tokio::test]
    async fn unnamed_local_claude_agent_gets_slug_rename_and_rebroadcast() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer(&user_state, "peer-a").await;
        let host_id = {
            let s = state.read().await;
            s.host_id
        };
        let agent_id = Uuid::new_v4();
        insert_local_claude_with_args(
            &user_state,
            host_id,
            agent_id,
            None,
            LocalAgentNameSource::Unset,
            vec!["--dangerously-skip-permissions".to_string()],
        )
        .await;

        handle_session_event(
            &state,
            SessionEvent::NameCandidateChanged {
                agent_id,
                user_id: super::LOCAL_USER_ID,
                name: "merry-slug".to_string(),
                source: LocalAgentNameSource::ProviderSlug,
            },
        )
        .await;

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("merry-slug"));
        assert_eq!(
            us.agents.get(&agent_id).and_then(|session| session.name()),
            Some("merry-slug")
        );
        drop(us);

        let msg = peer_rx
            .try_recv()
            .expect("peer should receive rename announce");
        assert!(matches!(
            msg,
            Message::Direct(DirectMessage::AnnounceAgent {
                agent_id: id,
                host_id: announced_host_id,
                name: Some(name),
                args,
                ..
            }) if id == agent_id
                && announced_host_id == host_id
                && name == "merry-slug"
                && args == vec!["--dangerously-skip-permissions".to_string()]
        ));
    }

    #[tokio::test]
    async fn later_agent_name_overrides_earlier_slug_rename_and_rebroadcast() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer(&user_state, "peer-a").await;
        let host_id = {
            let s = state.read().await;
            s.host_id
        };
        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            host_id,
            agent_id,
            None,
            LocalAgentNameSource::Unset,
        )
        .await;

        handle_session_event(
            &state,
            SessionEvent::NameCandidateChanged {
                agent_id,
                user_id: super::LOCAL_USER_ID,
                name: "slug-name".to_string(),
                source: LocalAgentNameSource::ProviderSlug,
            },
        )
        .await;
        let _ = peer_rx.try_recv().expect("slug rename should announce");

        handle_session_event(
            &state,
            SessionEvent::NameCandidateChanged {
                agent_id,
                user_id: super::LOCAL_USER_ID,
                name: "final-agent-name".to_string(),
                source: LocalAgentNameSource::ProviderName,
            },
        )
        .await;

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("final-agent-name"));
        assert_eq!(
            us.agents
                .get(&agent_id)
                .and_then(|session| session.local_name_source()),
            Some(LocalAgentNameSource::ProviderName)
        );
        drop(us);

        let msg = peer_rx
            .try_recv()
            .expect("agent-name rename should announce");
        assert!(matches!(
            msg,
            Message::Direct(DirectMessage::AnnounceAgent {
                agent_id: id,
                name: Some(name),
                ..
            }) if id == agent_id && name == "final-agent-name"
        ));
    }

    #[tokio::test]
    async fn amux_supplied_name_blocks_slug_and_agent_name_updates() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer(&user_state, "peer-a").await;
        let host_id = {
            let s = state.read().await;
            s.host_id
        };
        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            host_id,
            agent_id,
            Some("fixed-name"),
            LocalAgentNameSource::Amux,
        )
        .await;

        for (name, source) in [
            ("slug-name", LocalAgentNameSource::ProviderSlug),
            ("agent-name", LocalAgentNameSource::ProviderName),
        ] {
            handle_session_event(
                &state,
                SessionEvent::NameCandidateChanged {
                    agent_id,
                    user_id: super::LOCAL_USER_ID,
                    name: name.to_string(),
                    source,
                },
            )
            .await;
        }

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("fixed-name"));
        assert_eq!(
            us.agents
                .get(&agent_id)
                .and_then(|session| session.local_name_source()),
            Some(LocalAgentNameSource::Amux)
        );
        drop(us);
        assert!(
            peer_rx.try_recv().is_err(),
            "blocked updates should not announce"
        );
    }

    #[tokio::test]
    async fn same_string_higher_precedence_source_upgrades_provenance_without_announce() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer(&user_state, "peer-a").await;
        let host_id = {
            let s = state.read().await;
            s.host_id
        };
        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            host_id,
            agent_id,
            Some("shared-name"),
            LocalAgentNameSource::ProviderSlug,
        )
        .await;

        handle_session_event(
            &state,
            SessionEvent::NameCandidateChanged {
                agent_id,
                user_id: super::LOCAL_USER_ID,
                name: "shared-name".to_string(),
                source: LocalAgentNameSource::ProviderName,
            },
        )
        .await;

        let us = user_state.read().await;
        let entry = us.registry.get(&agent_id).unwrap();
        assert_eq!(entry.name.as_deref(), Some("shared-name"));
        assert_eq!(
            us.agents
                .get(&agent_id)
                .and_then(|session| session.local_name_source()),
            Some(LocalAgentNameSource::ProviderName)
        );
        drop(us);
        assert!(
            peer_rx.try_recv().is_err(),
            "provenance-only upgrades should not announce"
        );
    }

    #[tokio::test]
    async fn candidate_rename_collision_is_skipped_and_not_announced() {
        let (state, user_state) = test_state().await;
        let mut peer_rx = add_peer(&user_state, "peer-a").await;
        let host_id = {
            let s = state.read().await;
            s.host_id
        };
        let owner_id = Uuid::new_v4();
        let candidate_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            host_id,
            owner_id,
            Some("taken-name"),
            LocalAgentNameSource::Amux,
        )
        .await;
        insert_local_claude(
            &user_state,
            host_id,
            candidate_id,
            None,
            LocalAgentNameSource::Unset,
        )
        .await;

        handle_session_event(
            &state,
            SessionEvent::NameCandidateChanged {
                agent_id: candidate_id,
                user_id: super::LOCAL_USER_ID,
                name: "taken-name".to_string(),
                source: LocalAgentNameSource::ProviderSlug,
            },
        )
        .await;

        let us = user_state.read().await;
        assert_eq!(
            us.registry.resolve(&us.hosts, "taken-name").unwrap().id,
            owner_id
        );
        assert_eq!(us.registry.get(&candidate_id).unwrap().name, None);
        assert_eq!(
            us.agents
                .get(&candidate_id)
                .and_then(|session| session.name()),
            None
        );
        drop(us);
        assert!(peer_rx.try_recv().is_err(), "collision should not announce");
    }
}
