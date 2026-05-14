//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts Unix, TCP, and WebSocket listeners concurrently and spawns per-connection
//! tasks via [`accept`]. Shared state is `Arc<RwLock<ServerState>>` with per-user
//! isolation in [`ServerUserState`] (keyed by JWT-derived user ID, or [`LOCAL_USER_ID`]
//! for unauthenticated local connections).

mod events;
mod notify;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub(in crate::server) use events::handle_session_event;
pub(in crate::server) use notify::notify_local_clients;
use prost::Message as _;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;

use self::notify::notify_other_clients;
use super::accept::{local_accept, tcp_accept, websocket_accept};
use super::cloud::establish_cloud_connection;
use super::routing::{shutdown_server, suspend_server};
use super::{LOCAL_USER_ID, ServerState, ShutdownRequest};
use crate::agent::SessionEvent;
use crate::auth::jwt::JwtValidator;
use crate::config::{Config, ConfigError};
use crate::protocol::message::{
    Frame, FrameBody, Message, ProtocolError, ResponseFrame, ShutdownReason,
};
use crate::protocol::{Route, wire};
use crate::transport::{LocalListener, TcpTransport, TransportError, create_tls_acceptor};

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent network connections (TCP + WebSocket).
/// Safety net against resource exhaustion. Each network connection holds a
/// semaphore permit for its lifetime; new connections are rejected at capacity.
const MAX_CONNECTIONS: usize = 16384;

type Result<T> = std::result::Result<T, ServerError>;

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

/// The amux server
pub(crate) struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
    shutdown_rx: Option<mpsc::Receiver<ShutdownRequest>>,
}

impl Server {
    pub(crate) fn with_config(config: Config) -> Result<Self> {
        let host_id = crate::state::load_or_create_host_id(&config.state_path)
            .map_err(|e| ServerError::State(format!("failed to load host state: {e}")))?;
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
            establish_cloud_connection(config, self.state.clone(), self.event_tx.clone());

            // Task: Periodic update check (every hour)
            let (state_path, check_for_updates) = {
                let state = self.state.read().await;
                (
                    state.config.state_path.clone(),
                    state.config.check_for_updates,
                )
            };
            if check_for_updates {
                crate::update::spawn_update_checker(
                    cloud_url.clone(),
                    env!("CARGO_PKG_VERSION").to_string(),
                    Duration::from_secs(3600),
                    state_path,
                );
            }
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
