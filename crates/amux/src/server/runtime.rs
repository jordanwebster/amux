//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts Unix, TCP, and WebSocket listeners concurrently and spawns per-connection
//! tasks via [`accept`]. Shared state is `Arc<RwLock<ServerState>>` with per-user
//! isolation in [`ServerUserState`] (keyed by JWT-derived user ID, or [`LOCAL_USER_ID`]
//! for unauthenticated local connections).

mod events;
mod notify;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(in crate::server) use events::handle_session_event;
pub(in crate::server) use notify::notify_local_clients;
use prost::Message as _;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use self::notify::notify_other_clients;
use super::accept::{local_accept, tcp_accept, websocket_accept};
use super::cloud::establish_cloud_connection;
use super::routing::{shutdown_server, suspend_server};
use super::{LOCAL_USER_ID, ServerState, ShutdownRequest};
use crate::agent::SessionEvent;
use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::client::{Client, ConnectError, Connection, connect_existing};
use crate::config::{Config, ConfigError};
use crate::protocol::handshake::RoutingRole;
use crate::protocol::message::{
    Frame, FrameBody, Message, ProtocolError, ResponseFrame, ShutdownReason,
};
use crate::protocol::route::generate_terminal_link;
use crate::protocol::{Route, wire};
use crate::transport::{
    LocalListener, TcpTransport, TransportError, connect_handshake, create_tls_acceptor, memory,
};
use crate::update::{UpdateReporter, UpdateStatus};

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent network connections (TCP + WebSocket).
/// Safety net against resource exhaustion. Each network connection holds a
/// semaphore permit for its lifetime; new connections are rejected at capacity.
const MAX_CONNECTIONS: usize = 16384;

type Result<T> = std::result::Result<T, ServerError>;
type BuilderParts = (
    Config,
    Option<Arc<dyn CredentialProvider>>,
    bool,
    Option<Arc<dyn UpdateReporter>>,
);

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("accept error: {0}")]
    Accept(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("{0}")]
    State(String),
}

pub struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
    shutdown_rx: Option<mpsc::Receiver<ShutdownRequest>>,
}

pub struct ServerBuilder {
    config: Option<Config>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    update_reporter: Option<Arc<dyn UpdateReporter>>,
    as_cloud_relay: bool,
}

pub struct EmbeddedBuilder {
    inner: ServerBuilder,
}

pub struct DaemonBuilder {
    inner: ServerBuilder,
}

struct EmbeddedServerGuard {
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl EmbeddedServerGuard {
    fn new(tasks: Arc<Mutex<Vec<JoinHandle<()>>>>) -> Self {
        Self { tasks }
    }

    fn abort_tasks(&self) {
        abort_embedded_tasks(&self.tasks);
    }
}

impl Drop for EmbeddedServerGuard {
    fn drop(&mut self) {
        self.abort_tasks();
    }
}

fn push_embedded_task(tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>, task: JoinHandle<()>) {
    tasks
        .lock()
        .expect("embedded server task list mutex poisoned")
        .push(task);
}

fn abort_embedded_tasks(tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for task in tasks
        .lock()
        .expect("embedded server task list mutex poisoned")
        .drain(..)
    {
        task.abort();
    }
}

impl Server {
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            config: None,
            credentials: None,
            update_reporter: None,
            as_cloud_relay: false,
        }
    }

    pub(crate) fn with_config_and_credentials(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
    ) -> Result<Self> {
        let host_id = crate::state::load_or_create_host_id(&config.state_path)
            .map_err(|e| ServerError::State(format!("failed to load host state: {e}")))?;
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        Ok(Self {
            state: Arc::new(RwLock::new(ServerState::new(
                config,
                host_id,
                shutdown_tx,
                credentials,
                update_reporter,
            ))),
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
    pub(crate) async fn run(&mut self, is_cloud_server: bool) -> Result<()> {
        let (socket_path, tcp_port, ws_port, cloud_url, enforce_tls, prevent_idle_sleep) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port,
                state.config.websocket_port,
                state.config.cloud_url.clone(),
                state.config.enforce_tls_in_cloud_mode,
                state.config.prevent_idle_sleep.unwrap_or(false),
            )
        };

        // Validate server-specific config (cloud requires tcp + ws ports)
        {
            let state = self.state.read().await;
            state.config.validate(is_cloud_server)?;
        }

        let _sleep_inhibitor = crate::sleep_inhibitor::SleepInhibitor::new(prevent_idle_sleep);

        // Configure cloud server: enable JWT validation (and optionally TLS)
        let tls_acceptor: Option<TlsAcceptor> = if is_cloud_server {
            let mut state = self.state.write().await;
            state.is_cloud_server = true;
            state.jwt_validator = Some(Arc::new(JwtValidator::new(&cloud_url)));

            if enforce_tls {
                // Cloud mode requires TLS certificates via environment variables
                let cert_path = std::env::var("AMUX_TLS_CERT").map_err(|_| {
                    ServerError::Config(ConfigError::Invalid(
                        "AMUX_TLS_CERT environment variable required for cloud mode".into(),
                    ))
                })?;
                let key_path = std::env::var("AMUX_TLS_KEY").map_err(|_| {
                    ServerError::Config(ConfigError::Invalid(
                        "AMUX_TLS_KEY environment variable required for cloud mode".into(),
                    ))
                })?;

                let cert_pem = std::fs::read(&cert_path).map_err(|e| {
                    ServerError::Config(ConfigError::Invalid(format!(
                        "Failed to read TLS cert from {}: {}",
                        cert_path, e
                    )))
                })?;
                let key_pem = std::fs::read(&key_path).map_err(|e| {
                    ServerError::Config(ConfigError::Invalid(format!(
                        "Failed to read TLS key from {}: {}",
                        key_path, e
                    )))
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
            let _cloud_task =
                establish_cloud_connection(config, self.state.clone(), self.event_tx.clone());

            // Task: Periodic update check (every hour)
            let update_reporter = {
                let state = self.state.read().await;
                state.update_reporter.clone()
            };
            let _update_task = spawn_periodic_update_check(
                update_reporter,
                cloud_url.clone(),
                env!("CARGO_PKG_VERSION").to_string(),
                Duration::from_secs(3600),
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
        // Deferred reply: built inside the select arm, sent after listener teardown
        // so clients can't reconnect to the old socket before it's removed.
        let mut deferred_reply: Option<(mpsc::Sender<Message>, Message)> = None;

        loop {
            tokio::select! {
                // Shutdown/suspend request from a connection handler
                Some(req) = shutdown_rx.recv() => {
                    match req {
                        ShutdownRequest::Shutdown { reply, reply_call_id, link } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.user_state(&LOCAL_USER_ID)
                                    .expect("local user state is always initialized")
                            };
                            // Notify before shutdown so clients see it before streams close
                            notify_other_clients(
                                &user_state,
                                &link,
                                ShutdownReason::UserRequested,
                            ).await;
                            shutdown_server(&user_state).await;
                            let message = Message::Frame(Frame {
                                src: Route::from_link(link.clone()),
                                dst: Route::empty(),
                                call_id: reply_call_id,
                                body: FrameBody::Response(ResponseFrame::Payload(
                                    wire::Empty {}.encode_to_vec(),
                                )),
                            });
                            deferred_reply = Some((
                                reply,
                                message,
                            ));
                        }
                        ShutdownRequest::Suspend {
                            reply,
                            reply_call_id,
                            link,
                            reason,
                        } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.user_state(&LOCAL_USER_ID)
                                    .expect("local user state is always initialized")
                            };
                            // Notify before suspend so clients see it before streams close
                            notify_other_clients(
                                &user_state,
                                &link,
                                reason,
                            ).await;
                            let (suspended, errors) = suspend_server(&user_state).await;
                            let suspended_count = suspended.agents.len();
                            let error = if !errors.is_empty() {
                                Some(ProtocolError::ServerError {
                                    message: errors.join("; "),
                                })
                            } else {
                                None
                            };
                            if !suspended.agents.is_empty() {
                                let state_path = {
                                    let state = self.state.read().await;
                                    state.config.state_path.clone()
                                };
                                if let Err(e) =
                                    crate::suspend::save_suspended(&state_path, &suspended)
                                {
                                    tracing::error!(error = %e, "failed to save suspended agents");
                                    let _ = reply
                                        .send(Message::Frame(Frame {
                                            src: Route::from_link(link.clone()),
                                            dst: Route::empty(),
                                            call_id: reply_call_id,
                                            body: FrameBody::Response(ResponseFrame::Error(
                                                ProtocolError::ServerError {
                                                    message: format!("failed to save state: {e}"),
                                                },
                                            )),
                                        }))
                                        .await;
                                    // Don't shut down on save failure
                                    continue;
                                }
                            }
                            deferred_reply = Some((
                                reply,
                                Message::Frame(Frame {
                                    src: Route::from_link(link.clone()),
                                    dst: Route::empty(),
                                    call_id: reply_call_id,
                                    body: FrameBody::Response(match error {
                                        Some(error) => ResponseFrame::Error(error),
                                        None => ResponseFrame::Payload(wire::SuspendResponse {
                                        suspended_count: suspended_count as u64,
                                    }.encode_to_vec()),
                                    }),
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
                                    let _permit = permit;
                                    let tls_result = tokio::time::timeout(
                                        TLS_HANDSHAKE_TIMEOUT,
                                        acceptor.accept(stream),
                                    ).await;
                                    match tls_result {
                                        Ok(Ok(tls_stream)) => {
                                            let transport = TcpTransport::new(tls_stream);
                                            let _ = tcp_accept(transport, state, event_tx, verify_token).await;
                                        }
                                        Ok(Err(e)) => {
                                            tracing::warn!(peer = %addr, error = %e, "TLS handshake failed");
                                        }
                                        Err(_) => {
                                            tracing::warn!(peer = %addr, "TLS handshake timed out");
                                        }
                                    }
                                });
                            } else {
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    let transport = TcpTransport::new(stream);
                                    let _ = tcp_accept(transport, state, event_tx, verify_token).await;
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
                                let _permit = permit;
                                let _ = websocket_accept(stream, state, event_tx, verify_token).await;
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

impl ServerBuilder {
    pub fn config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn credentials(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = Some(provider);
        self
    }

    pub fn update_reporter(mut self, reporter: Arc<dyn UpdateReporter>) -> Self {
        self.update_reporter = Some(reporter);
        self
    }

    pub fn as_cloud_relay(mut self) -> Self {
        self.as_cloud_relay = true;
        self
    }

    pub fn embedded(self) -> EmbeddedBuilder {
        EmbeddedBuilder { inner: self }
    }

    pub fn daemon(self) -> DaemonBuilder {
        DaemonBuilder { inner: self }
    }
}

impl EmbeddedBuilder {
    pub async fn open(self) -> Result<Client> {
        let (config, credentials, as_cloud_relay, update_reporter) = self.inner.into_parts()?;
        config.validate(as_cloud_relay)?;
        let mut server = Server::with_config_and_credentials(config, credentials, update_reporter)?;
        if as_cloud_relay {
            let mut state = server.state.write().await;
            state.is_cloud_server = true;
            state.jwt_validator = Some(Arc::new(JwtValidator::new(&state.config.cloud_url)));
        }

        let tasks = Arc::new(Mutex::new(Vec::new()));
        let mut event_rx = server.event_rx.take().expect("open() called after run()");
        let state = server.state.clone();
        push_embedded_task(
            &tasks,
            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    handle_session_event(&state, event).await;
                }
            }),
        );

        if !as_cloud_relay {
            let config = {
                let state = server.state.read().await;
                state.config.clone()
            };
            let cloud_url = config.cloud_url.clone();
            push_embedded_task(
                &tasks,
                establish_cloud_connection(config, server.state.clone(), server.event_tx.clone()),
            );
            let update_reporter = {
                let state = server.state.read().await;
                state.update_reporter.clone()
            };
            if let Some(task) = spawn_periodic_update_check(
                update_reporter,
                cloud_url,
                env!("CARGO_PKG_VERSION").to_string(),
                Duration::from_secs(3600),
            ) {
                push_embedded_task(&tasks, task);
            }
        }

        let user_state = crate::server::ensure_user_state(&server.state, LOCAL_USER_ID).await;
        let mut shutdown_rx = server
            .shutdown_rx
            .take()
            .expect("open() called after run()");
        let shutdown_user_state = user_state.clone();
        let shutdown_tasks = tasks.clone();
        let state_path = {
            let state = server.state.read().await;
            state.config.state_path.clone()
        };
        push_embedded_task(
            &tasks,
            tokio::spawn(async move {
                if let Some(req) = shutdown_rx.recv().await {
                    handle_embedded_shutdown(req, shutdown_user_state, state_path, shutdown_tasks)
                        .await;
                }
            }),
        );

        let (server_transport, mut client_transport) = memory::pair(2048);
        let accept_state = server.state.clone();
        let accept_event_tx = server.event_tx.clone();
        push_embedded_task(
            &tasks,
            tokio::spawn(async move {
                if let Err(error) =
                    local_accept(server_transport, accept_state, accept_event_tx).await
                {
                    tracing::debug!(error = %error, "embedded local connection closed");
                }
            }),
        );

        let outcome = connect_handshake(
            &mut client_transport,
            generate_terminal_link,
            None,
            RoutingRole::Observer,
        )
        .await
        .map_err(connect_error_to_server_error)?;

        let guard = Arc::new(EmbeddedServerGuard::new(tasks));
        Ok(Client::new_with_guard(
            Connection::new_memory(client_transport, outcome.link),
            guard,
        ))
    }

    pub async fn run(self) -> Result<()> {
        let (config, credentials, as_cloud_relay, update_reporter) = self.inner.into_parts()?;
        let mut server = Server::with_config_and_credentials(config, credentials, update_reporter)?;
        server.run(as_cloud_relay).await
    }
}

impl DaemonBuilder {
    pub async fn open(self) -> std::result::Result<Client, ConnectError> {
        let config = self
            .inner
            .config
            .ok_or_else(|| ConfigError::Invalid("server config is required".to_string()))
            .map_err(ConnectError::Config)?;
        connect_existing(&config).await.map(Client::new)
    }
}

impl ServerBuilder {
    fn into_parts(self) -> std::result::Result<BuilderParts, ConfigError> {
        let config = self
            .config
            .ok_or_else(|| ConfigError::Invalid("server config is required".to_string()))?;
        if self.as_cloud_relay && self.credentials.is_some() {
            return Err(ConfigError::Invalid(
                "credentials() and as_cloud_relay() are mutually exclusive".to_string(),
            ));
        }
        if !self.as_cloud_relay
            && crate::setup::cloud_enabled(&config)
            && self.credentials.is_none()
        {
            return Err(ConfigError::Invalid(
                "credentials provider is required when cloud mode is enabled".to_string(),
            ));
        }
        Ok((
            config,
            self.credentials,
            self.as_cloud_relay,
            self.update_reporter,
        ))
    }
}

fn spawn_periodic_update_check(
    reporter: Option<Arc<dyn UpdateReporter>>,
    cloud_url: String,
    current_version: String,
    interval: Duration,
) -> Option<JoinHandle<()>> {
    let reporter = reporter?;
    Some(tokio::spawn(async move {
        loop {
            match crate::update::check_for_update(&cloud_url, &current_version).await {
                Some(info) => {
                    tracing::info!(
                        current = %info.current_version,
                        latest = %info.update_version,
                        "update available"
                    );
                    reporter.report(UpdateStatus::Available(Some(info)));
                }
                None => {
                    reporter.report(UpdateStatus::Available(None));
                }
            }
            tokio::time::sleep(interval).await;
        }
    }))
}

async fn handle_embedded_shutdown(
    req: ShutdownRequest,
    user_state: Arc<RwLock<super::ServerUserState>>,
    state_path: std::path::PathBuf,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    let should_abort = match req {
        ShutdownRequest::Shutdown {
            reply,
            reply_call_id,
            link,
        } => {
            notify_other_clients(&user_state, &link, ShutdownReason::UserRequested).await;
            shutdown_server(&user_state).await;
            let _ = reply
                .send(Message::Frame(Frame {
                    src: Route::from_link(link),
                    dst: Route::empty(),
                    call_id: reply_call_id,
                    body: FrameBody::Response(ResponseFrame::Payload(
                        wire::Empty {}.encode_to_vec(),
                    )),
                }))
                .await;
            true
        }
        ShutdownRequest::Suspend {
            reply,
            reply_call_id,
            link,
            reason,
        } => {
            notify_other_clients(&user_state, &link, reason).await;
            let (suspended, errors) = suspend_server(&user_state).await;
            let suspended_count = suspended.agents.len();
            if !suspended.agents.is_empty()
                && let Err(error) = crate::suspend::save_suspended(&state_path, &suspended)
            {
                let _ = reply
                    .send(Message::Frame(Frame {
                        src: Route::from_link(link),
                        dst: Route::empty(),
                        call_id: reply_call_id,
                        body: FrameBody::Response(ResponseFrame::Error(
                            ProtocolError::ServerError {
                                message: format!("failed to save state: {error}"),
                            },
                        )),
                    }))
                    .await;
                false
            } else {
                let response = if errors.is_empty() {
                    ResponseFrame::Payload(
                        wire::SuspendResponse {
                            suspended_count: suspended_count as u64,
                        }
                        .encode_to_vec(),
                    )
                } else {
                    ResponseFrame::Error(ProtocolError::ServerError {
                        message: errors.join("; "),
                    })
                };
                let _ = reply
                    .send(Message::Frame(Frame {
                        src: Route::from_link(link),
                        dst: Route::empty(),
                        call_id: reply_call_id,
                        body: FrameBody::Response(response),
                    }))
                    .await;
                errors.is_empty()
            }
        }
    };
    if should_abort {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            abort_embedded_tasks(&tasks);
        });
    }
}

fn connect_error_to_server_error(error: crate::transport::HandshakeError) -> ServerError {
    match error {
        crate::transport::HandshakeError::Transport(error) => ServerError::Transport(error),
        crate::transport::HandshakeError::Timeout => {
            ServerError::Connection("embedded handshake timed out".to_string())
        }
        crate::transport::HandshakeError::InvalidMessage(message) => {
            ServerError::Connection(message)
        }
        crate::transport::HandshakeError::Protocol(error) => {
            ServerError::Connection(error.to_string())
        }
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Arc;

    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    use crate::config::Config;
    use crate::server::{LOCAL_USER_ID, ServerState, ServerUserState};

    pub(crate) async fn test_state() -> (Arc<RwLock<ServerState>>, Arc<RwLock<ServerUserState>>) {
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            Uuid::new_v4(),
            shutdown_tx,
            None,
            None,
        )));
        let user_state = {
            let s = state.read().await;
            s.user_state(&LOCAL_USER_ID)
                .expect("local user state is always initialized")
        };
        user_state
            .write()
            .await
            .ensure_route_rpc(crate::protocol::Route::empty());
        (state, user_state)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn update_checker_is_not_spawned_without_reporter() {
        let task = super::spawn_periodic_update_check(
            None,
            "https://example.com".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        );

        assert!(task.is_none());
    }
}
