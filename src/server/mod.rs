use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::jwt::JwtValidator;
use crate::message::{AgentInfo, Message};
use crate::route::Route;
use crate::session::{LocalAgentSession, SessionEvent};
use crate::transport::{create_tls_acceptor, TcpTransport};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

mod accept;
mod cloud;
mod connection;
mod routing;

use accept::{tcp_accept, unix_accept, websocket_accept};
use cloud::establish_cloud_connection;
use routing::broadcast_to_peers;

/// A remote agent announced by a peer connection
pub(super) struct RemoteAgent {
    pub(super) info: AgentInfo,
    /// Full route from this server to the agent
    pub(super) route: Route,
    /// Direct peer link that announced this agent (for cleanup on disconnect)
    pub(super) link: String,
}

/// Server state shared across connection handlers
pub(super) struct ServerState {
    pub(super) config: Config,
    /// Whether running in cloud mode (TLS + token auth required)
    pub(super) cloud_mode: bool,
    pub(super) agents: HashMap<Uuid, Arc<LocalAgentSession>>,
    /// Routes keyed by link name. The actual transport is owned by the
    /// connection handler task; we only keep an outgoing message channel.
    pub(super) routes: HashMap<String, mpsc::Sender<Message>>,
    /// Remote agents announced by peer connections
    pub(super) remote_agents: HashMap<Uuid, RemoteAgent>,
    /// Link names of peer connections (non-local connections that receive announcements)
    pub(super) peer_links: HashSet<String>,
    /// JWT validator for cloud mode (validates incoming tokens)
    pub(super) jwt_validator: Option<Arc<JwtValidator>>,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            cloud_mode: false,
            agents: HashMap::new(),
            routes: HashMap::new(),
            remote_agents: HashMap::new(),
            peer_links: HashSet::new(),
            jwt_validator: None,
        }
    }
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

        // Set cloud mode and create JWT validator if needed
        let tls_acceptor: Option<TlsAcceptor> = if is_cloud_server {
            let mut state = self.state.write().await;
            state.cloud_mode = true;
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
                log!("server: TLS configured for cloud mode");
                Some(acceptor)
            } else {
                log!("server: cloud mode with external TLS termination (token auth enabled)");
                None
            }
        } else {
            None
        };

        // Unix socket - always available (for CLI commands like list-agents, kill-server)
        let _ = std::fs::remove_file(&socket_path);
        let unix_listener = UnixListener::bind(&socket_path)?;
        log!("server: listening on {:?}", socket_path);

        let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_port));
        let tcp_listener = TcpListener::bind(tcp_addr).await?;
        if is_cloud_server && enforce_tls {
            log!("server: listening on TLS TCP {}", tcp_addr);
        } else if is_cloud_server {
            log!(
                "server: listening on TCP {} (TLS terminated externally)",
                tcp_addr
            );
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
                        broadcast_to_peers(
                            &mut state,
                            &crate::message::LocalMessage::WithdrawAgent { agent_id },
                            None,
                        );
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
                // TCP connection - TLS in cloud mode, plain in local mode
                result = tcp_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            if let Err(e) = stream.set_nodelay(true) {
                                log!("server: failed to set TCP_NODELAY: {}", e);
                            }

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
                                                log!("server: tls tcp connection error: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            log!("server: tls handshake error from {}: {}", addr, e);
                                        }
                                    }
                                });
                            } else {
                                tokio::spawn(async move {
                                    let transport = TcpTransport::new(stream);
                                    if let Err(e) = tcp_accept(transport, state, event_tx, verify_token).await {
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
                // WebSocket connection
                result = ws_listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            log!("server: websocket client connected from {}", addr);
                            let state = self.state.clone();
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = websocket_accept(stream, state, event_tx).await {
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
