//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts Unix, TCP, and WebSocket listeners concurrently and spawns per-connection
//! tasks via [`accept`]. Shared state is `Arc<RwLock<ServerState>>` with per-user
//! isolation in [`ServerUserState`] (keyed by JWT-derived user ID, or [`LOCAL_USER_ID`]
//! for unauthenticated local connections).

mod events;
mod forward;
mod notify;
mod sweep;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub(in crate::server) use events::handle_session_event;
pub(in crate::server) use forward::send_routable_via_full_dst;
pub(in crate::server) use notify::notify_local_clients;
pub(in crate::server) use sweep::sweep_expired_subscriptions;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore, mpsc};
use tokio::time::MissedTickBehavior;
use tokio_rustls::TlsAcceptor;

use self::notify::notify_other_clients;
use super::accept::{local_accept, tcp_accept, websocket_accept};
use super::cloud::establish_cloud_connection;
use super::routing::{shutdown_server, suspend_server};
use super::{LOCAL_USER_ID, ServerState, ShutdownRequest};
use crate::agent::SessionEvent;
use crate::auth::jwt::JwtValidator;
use crate::config::{Config, ConfigError};
use crate::protocol::message::{Command, Message, ProtocolError, ShutdownReason};
use crate::transport::{LocalListener, TcpTransport, TransportError, create_tls_acceptor};

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent network connections (TCP + WebSocket).
/// Safety net against resource exhaustion. Each network connection holds a
/// semaphore permit for its lifetime; new connections are rejected at capacity.
const MAX_CONNECTIONS: usize = 16384;

const SUBSCRIPTION_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

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
                        ShutdownRequest::Shutdown { reply, link } => {
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
                            deferred_reply = Some((
                                reply,
                                Message::Command {
                                    command: Command::ShutdownNotification {
                                        reason: ShutdownReason::UserRequested,
                                    },
                                },
                            ));
                        }
                        ShutdownRequest::Suspend { reply, link } => {
                            let user_state = {
                                let s = self.state.read().await;
                                s.user_state(&LOCAL_USER_ID)
                                    .expect("local user state is always initialized")
                            };
                            // Notify before suspend so clients see it before streams close
                            notify_other_clients(
                                &user_state,
                                &link,
                                ShutdownReason::Updating,
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
                                        .send(Message::Command {
                                            command: Command::SuspendResult {
                                                suspended_count: 0,
                                                error: Some(ProtocolError::ServerError {
                                                    message: format!("failed to save state: {e}"),
                                                }),
                                            },
                                        })
                                        .await;
                                    // Don't shut down on save failure
                                    continue;
                                }
                            }
                            deferred_reply = Some((
                                reply,
                                Message::Command {
                                    command: Command::SuspendResult {
                                        suspended_count: suspended_count as u64,
                                        error,
                                    },
                                },
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
    use std::sync::atomic::AtomicU64;

    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    use crate::config::Config;
    use crate::protocol::link::Link;
    use crate::server::connection::ConnectionContext;
    use crate::server::{LOCAL_USER_ID, ServerState, ServerUserState};

    pub(crate) fn test_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("test-link").unwrap(),
            is_local: true,
            heartbeat: None,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

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
        (state, user_state)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    use crate::agent::{AgentSession, LocalAgentNameSource, SessionEvent};
    use crate::protocol::link::Link;
    use crate::protocol::message::{AgentType, CreateAgentRequest, DirectMessage, Message};
    use crate::server::test_helpers::test_state;
    use crate::server::{ConnectionHandle, ServerUserState, handle_session_event};

    async fn add_peer(
        user_state: &Arc<RwLock<ServerUserState>>,
        link: &str,
    ) -> mpsc::Receiver<Message> {
        let (tx, rx) = mpsc::channel(16);
        let mut us = user_state.write().await;
        us.routes.insert(
            Link::new(link).unwrap(),
            ConnectionHandle::new(tx, Arc::new(AtomicU64::new(1))),
        );
        us.peer_links.insert(Link::new(link).unwrap());
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
        let mut session = AgentSession::try_new(&req).expect("known agent type");
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

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name.as_deref(), Some("merry-slug"));
            assert_eq!(
                us.agents.get(&agent_id).and_then(|session| session.name()),
                Some("merry-slug")
            );
        }

        let msg = peer_rx
            .try_recv()
            .expect("peer should receive rename announce");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    host_id: announced_host_id,
                    name: Some(name),
                    args,
                    ..
                }
            } if id == agent_id
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

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name.as_deref(), Some("final-agent-name"));
            assert_eq!(
                us.agents
                    .get(&agent_id)
                    .and_then(|session| session.local_name_source()),
                Some(LocalAgentNameSource::ProviderName)
            );
        }

        let msg = peer_rx
            .try_recv()
            .expect("agent-name rename should announce");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    ..
                }
            } if id == agent_id && name == "final-agent-name"
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

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name.as_deref(), Some("fixed-name"));
            assert_eq!(
                us.agents
                    .get(&agent_id)
                    .and_then(|session| session.local_name_source()),
                Some(LocalAgentNameSource::Amux)
            );
        }
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

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name.as_deref(), Some("shared-name"));
            assert_eq!(
                us.agents
                    .get(&agent_id)
                    .and_then(|session| session.local_name_source()),
                Some(LocalAgentNameSource::ProviderName)
            );
        }
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

        {
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
        }
        assert!(peer_rx.try_recv().is_err(), "collision should not announce");
    }
}
