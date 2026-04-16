//! Connection acceptance and handshake for all transport types.
//!
//! Each transport (Unix, TCP, TLS, WebSocket) has an `*_accept` entry point that
//! bootstraps the connection: upgrade (WebSocket) or wrap (TLS), run the
//! [`accept_handshake`] protocol, then hand off to
//! [`run_connection`](super::connection::run_connection) for the connection
//! lifecycle. [`tcp_connect`] handles the outbound (client-side) direction for
//! server-to-server peering.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::Instrument;
use uuid::Uuid;

use super::connection::{ConnectionContext, HeartbeatRole, run_connection};
use super::routing::send_initial_announcements;
use super::{ConnectionHandle, LOCAL_USER_ID, ServerState, ServerUserState, ensure_user_state};
use crate::agent::SessionEvent;
use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::protocol::link::Link;
use crate::protocol::message::{Message, ProtocolError};
use crate::protocol::route::generate_server_link;
use crate::transport::{
    HandshakeError, TcpTransport, Transport, TransportError, TransportSplit, WebSocketTransport,
    connect_handshake,
};

/// Maximum time allowed for a connection to complete its handshake.
/// Prevents slow-loris attacks where a client connects but never sends
/// (or slowly trickles) handshake data, holding a server task indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) type Result<T> = std::result::Result<T, AcceptError>;

#[derive(Debug, Error)]
pub(super) enum AcceptError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Connection(#[from] super::connection::ConnectionError),
    #[error("invalid handshake message: {0}")]
    InvalidHandshake(String),
    #[error("{0}")]
    Config(String),
    #[error(
        "Too many handshake attempts (link name collision) — this is usually transient, retry the command"
    )]
    TooManyHandshakeAttempts,
    #[error("Invalid or missing credentials")]
    InvalidCredentials,
    #[error(
        "protocol mismatch (server protocol v{server_version}, client protocol v{client_version})"
    )]
    ProtocolMismatch {
        server_version: u32,
        client_version: u32,
    },
    #[error("amux upgrade required (minimum v{minimum_version}, you have v{client_version})")]
    UpgradeRequired {
        minimum_version: String,
        client_version: String,
    },
    #[error("handshake timed out")]
    HandshakeTimeout,
}

fn map_handshake_error(error: HandshakeError) -> AcceptError {
    match error {
        HandshakeError::Transport(err) => AcceptError::Transport(err),
        HandshakeError::Timeout => AcceptError::HandshakeTimeout,
        HandshakeError::InvalidMessage(message) => AcceptError::InvalidHandshake(message),
        HandshakeError::Protocol(ProtocolError::InvalidCredentials) => {
            AcceptError::InvalidCredentials
        }
        HandshakeError::Protocol(ProtocolError::ProtocolMismatch {
            server_version,
            client_version,
        }) => AcceptError::ProtocolMismatch {
            server_version,
            client_version,
        },
        HandshakeError::Protocol(ProtocolError::UpgradeRequired {
            minimum_version,
            client_version,
        }) => AcceptError::UpgradeRequired {
            minimum_version,
            client_version,
        },
        HandshakeError::Protocol(other) => AcceptError::Config(other.to_string()),
        HandshakeError::TooManyAttempts => AcceptError::TooManyHandshakeAttempts,
    }
}

async fn write_connect_result<T: Transport>(
    transport: &mut T,
    error: Option<ProtocolError>,
) -> Result<()> {
    let response = ConnectResult { error };
    let payload = response
        .encode()
        .map_err(TransportError::SerializationEncode)?;
    transport.write_frame(&payload).await?;
    Ok(())
}

/// Validate a proposed link name: non-empty, max 128 bytes, `[a-zA-Z0-9_-]` only.
fn validate_link_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("must not be empty");
    }
    if name.len() > 128 {
        return Err("exceeds 128 byte limit");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("must contain only alphanumeric, hyphen, or underscore characters");
    }
    Ok(())
}

/// Accept-side handshake: client proposes link name, we validate against routes.
/// Atomically checks uniqueness and inserts the route under a write lock.
/// Returns the accepted link name, the outgoing message receiver, the user_id,
/// and the per-user state on success.
///
/// If `verify_token` is true (cloud server mode), the token in the Connect message
/// is validated via JWT. If validation fails, InvalidCredentials is returned.
pub(super) async fn accept_handshake<T: Transport>(
    transport: &mut T,
    state: &Arc<RwLock<ServerState>>,
    verify_token: bool,
) -> Result<(
    Link,
    ConnectionHandle,
    mpsc::Receiver<Message>,
    Uuid,
    Arc<RwLock<ServerUserState>>,
    Option<String>,
    Option<String>,
)> {
    for _attempt in 0..5 {
        let payload = transport.read_frame().await?;
        let connect = Connect::decode(&payload).map_err(|e| {
            AcceptError::InvalidHandshake(format!("expected Connect during handshake: {e}"))
        })?;
        let Connect {
            link: proposed_link,
            token,
            version,
            client_name,
            client_version,
        } = connect;

        if version != PROTOCOL_VERSION {
            tracing::warn!(
                client_version = version,
                server_version = PROTOCOL_VERSION,
                "protocol mismatch"
            );
            write_connect_result(
                transport,
                Some(ProtocolError::ProtocolMismatch {
                    server_version: PROTOCOL_VERSION,
                    client_version: version,
                }),
            )
            .await?;
            return Err(AcceptError::ProtocolMismatch {
                server_version: PROTOCOL_VERSION,
                client_version: version,
            });
        }

        // Check minimum client version if configured for this client_name
        if let Some(ref name) = client_name {
            let min_version = {
                let s = state.read().await;
                s.config.minimum_client_versions.get(name).cloned()
            };
            if let Some(ref min_ver_str) = min_version {
                let cv = client_version.as_deref().unwrap_or("");
                let reject = match (
                    semver::Version::parse(cv),
                    semver::Version::parse(min_ver_str),
                ) {
                    (Ok(client), Ok(minimum)) => client < minimum,
                    // Missing or unparseable client version is treated as below minimum
                    _ => true,
                };
                if reject {
                    let cv = cv.to_string();
                    tracing::warn!(
                        client_name = %name,
                        client_version = %cv,
                        minimum_version = %min_ver_str,
                        "client version below minimum"
                    );
                    write_connect_result(
                        transport,
                        Some(ProtocolError::UpgradeRequired {
                            minimum_version: min_ver_str.clone(),
                            client_version: cv.clone(),
                        }),
                    )
                    .await?;
                    return Err(AcceptError::UpgradeRequired {
                        minimum_version: min_ver_str.clone(),
                        client_version: cv,
                    });
                }
            }
        }

        if let Err(reason) = validate_link_name(proposed_link.as_str()) {
            tracing::warn!(link = %proposed_link, reason, "rejecting invalid link name");
            write_connect_result(transport, Some(ProtocolError::InvalidLinkName)).await?;
            return Err(AcceptError::Config(format!(
                "Invalid link name '{}': {}",
                proposed_link, reason
            )));
        }
        // validate_link_name rejects empty and `.`, so Link::new cannot fail here.
        let proposed_link =
            Link::new(proposed_link).expect("validate_link_name enforces Link::new invariants");

        let user_id = if verify_token {
            let (validator, host, tcp_port) = {
                let state = state.read().await;
                let validator = state
                    .jwt_validator
                    .clone()
                    .expect("verify_token=true requires jwt_validator");
                let tcp_port = state.config.tcp_port.expect("cloud mode requires tcp_port");
                (validator, state.config.host_name.clone(), tcp_port)
            };

            let token = token.ok_or_else(|| {
                tracing::warn!("token required but none provided");
                AcceptError::InvalidCredentials
            })?;

            match validator.validate(&token, &host, tcp_port).await {
                Ok(claims) => {
                    tracing::info!(user_id = %claims.sub, "authenticated");
                    match claims.sub.parse::<Uuid>() {
                        Ok(user_id) => user_id,
                        Err(_) => {
                            tracing::error!(sub = %claims.sub, "invalid user_id in token");
                            write_connect_result(
                                transport,
                                Some(ProtocolError::InvalidCredentials),
                            )
                            .await?;
                            return Err(AcceptError::InvalidCredentials);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "token validation failed");
                    write_connect_result(transport, Some(ProtocolError::InvalidCredentials))
                        .await?;
                    return Err(AcceptError::InvalidCredentials);
                }
            }
        } else {
            LOCAL_USER_ID
        };

        // Get or create user state (read lock fast path, write lock only on first connection)
        let user_state = ensure_user_state(state, user_id).await;

        let reservation = {
            let mut us = user_state.write().await;
            us.try_reserve_link(proposed_link.clone())
        };
        let (handle, outgoing_rx) = match reservation {
            Ok(pair) => pair,
            Err(_) => {
                write_connect_result(transport, Some(ProtocolError::LinkNameTaken)).await?;
                continue;
            }
        };

        // Route is inserted — if the success write fails, clean up the stale route
        if let Err(e) = write_connect_result(transport, None).await {
            let mut us = user_state.write().await;
            us.routes.remove(&proposed_link);
            return Err(e);
        }

        return Ok((
            proposed_link,
            handle,
            outgoing_rx,
            user_id,
            user_state,
            client_name,
            client_version,
        ));
    }

    tracing::warn!("handshake failed after 5 link-name retries");
    Err(AcceptError::TooManyHandshakeAttempts)
}

/// Accept a connection with transport split: handshake, then spawn reader/writer tasks
/// and run the connection loop on channels only.
pub(super) async fn accept_connection<T: TransportSplit>(
    mut transport: T,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    verify_token: bool,
    is_local: bool,
    log_label: &str,
) -> Result<()> {
    // Handshake uses the transport directly (safe — no select! involved).
    // Timeout prevents slow-loris: clients that connect but never send handshake data.
    // The span gives all handshake-phase logs transport context (before conn_span exists).
    let (link, route_handle, outgoing_rx, user_id, user_state, client_name, client_version) =
        async {
            match tokio::time::timeout(
                HANDSHAKE_TIMEOUT,
                accept_handshake(&mut transport, &state, verify_token),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("handshake timed out");
                    Err(AcceptError::HandshakeTimeout)
                }
            }
        }
        .instrument(tracing::info_span!("handshake", transport = log_label))
        .await?;

    let heartbeat_role = if is_local {
        HeartbeatRole::Disabled
    } else {
        HeartbeatRole::Acceptor
    };
    let conn_span = tracing::info_span!(
        "connection",
        link = %link,
        transport = log_label,
        user_id = %user_id,
        heartbeat_role = heartbeat_role.as_str(),
    );
    tracing::info!(parent: &conn_span, "connection established");

    if !is_local {
        let (host_id, host_name, is_cloud_server) = {
            let s = state.read().await;
            (s.host_id, s.config.host_name.clone(), s.is_cloud_server)
        };
        let mut us = user_state.write().await;
        us.peer_links.insert(link.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link);
    }

    let ctx = ConnectionContext {
        state: state.clone(),
        user_state: user_state.clone(),
        user_id,
        event_tx,
        link: link.clone(),
        is_local,
        heartbeat_role,
        next_request_id: route_handle.request_counter(),
        client_name,
        client_version,
    };

    run_connection(
        transport,
        outgoing_rx,
        route_handle.sender(),
        ctx,
        None,
        conn_span,
    )
    .await?;
    Ok(())
}

/// WebSocket connection bootstrap - accept, upgrade, and handshake
pub(super) async fn websocket_accept(
    stream: TcpStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    verify_token: bool,
) -> Result<()> {
    let ws_config = WebSocketConfig {
        max_message_size: Some(crate::transport::MAX_FRAME_SIZE),
        max_frame_size: Some(crate::transport::MAX_FRAME_SIZE),
        ..WebSocketConfig::default()
    };
    let ws_stream = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        accept_async_with_config(stream, Some(ws_config)),
    )
    .await
    .map_err(|_| {
        tracing::warn!("WebSocket upgrade timed out");
        AcceptError::HandshakeTimeout
    })?
    .map_err(|e| {
        tracing::warn!(error = %e, "WebSocket upgrade failed");
        AcceptError::Transport(TransportError::Io(std::io::Error::other(e.to_string())))
    })?;
    let transport = WebSocketTransport::new(ws_stream);
    accept_connection(transport, state, event_tx, verify_token, false, "websocket").await
}

/// Local transport connection bootstrap - accept and handshake
pub(super) async fn local_accept(
    transport: impl TransportSplit,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    accept_connection(transport, state, event_tx, false, true, "local").await
}

/// TCP peer bootstrap - accept inbound connection and run handshake.
///
/// Generic over transport type to support both plain TCP and TLS connections.
/// Set `verify_token` to true for cloud server mode (validates JWT in Connect message).
pub(super) async fn tcp_accept<T: TransportSplit>(
    transport: T,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    verify_token: bool,
) -> Result<()> {
    accept_connection(transport, state, event_tx, verify_token, false, "tcp").await
}

/// TCP outbound connection - connect and handshake
pub(super) async fn tcp_connect(
    address: &str,
    state: &Arc<RwLock<ServerState>>,
    user_state: &Arc<RwLock<ServerUserState>>,
    user_id: Uuid,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|_| AcceptError::Config(format!("Invalid address: {}", address)))?;

    let stream = TcpStream::connect(addr)
        .await
        .map_err(TransportError::from)?;
    stream.set_nodelay(true).map_err(TransportError::from)?;
    crate::transport::configure_tcp_keepalive(&stream);

    tracing::info!(addr = %addr, "connected to remote server");

    let mut transport = TcpTransport::new(stream);

    let (hostname, randomise) = {
        let state = state.read().await;
        (
            state.config.host_name.clone(),
            state.config.randomise_link_name,
        )
    };

    let link = connect_handshake(&mut transport, || {
        generate_server_link(&hostname, randomise)
    })
    .await
    .map_err(map_handshake_error)?;

    let conn_span = tracing::info_span!(
        "connection",
        link = %link,
        transport = "tcp",
        user_id = %user_id,
        heartbeat_role = HeartbeatRole::Dialer.as_str(),
    );
    tracing::info!(parent: &conn_span, "peer handshake complete");

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    let next_request_id = Arc::new(AtomicU64::new(1));
    {
        let (host_id, host_name, is_cloud_server) = {
            let s = state.read().await;
            (s.host_id, s.config.host_name.clone(), s.is_cloud_server)
        };
        let mut us = user_state.write().await;
        us.routes.insert(
            link.clone(),
            ConnectionHandle::new(outgoing_tx.clone(), next_request_id.clone()),
        );
        us.peer_links.insert(link.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link);
    }

    let state = state.clone();
    let user_state = user_state.clone();
    tokio::spawn(async move {
        let ctx = ConnectionContext {
            state,
            user_state,
            user_id,
            event_tx,
            link: link.clone(),
            is_local: false,
            heartbeat_role: HeartbeatRole::Dialer,
            next_request_id,
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        let _ = run_connection(transport, outgoing_rx, outgoing_tx, ctx, None, conn_span).await;
    });

    Ok(())
}
