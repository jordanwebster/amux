//! Connection acceptance and handshake for all transport types.
//!
//! Each transport (Unix, TCP, TLS, WebSocket) has an `*_accept` entry point that
//! bootstraps the connection: upgrade (WebSocket) or wrap (TLS), run the
//! [`accept_handshake`] protocol, then hand off to
//! [`run_connection`](super::connection::run_connection) for the connection
//! lifecycle. [`tcp_connect`] handles the outbound (client-side) direction for
//! server-to-server peering.

use super::connection::{ConnectionContext, HeartbeatRole, run_connection};
use super::routing::send_initial_announcements;
use super::{
    ConnectionHandle, LOCAL_USER_ID, ServerState, ServerUserState, get_or_create_user_state,
};
use crate::error::{AmuxError, Result};
use crate::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::message::{Message, ProtocolError};
use crate::route::generate_server_link;
use crate::transport::{TcpTransport, Transport, TransportSplit, WebSocketTransport};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::accept_async;
use tracing::Instrument;
use uuid::Uuid;

/// Maximum time allowed for a connection to complete its handshake.
/// Prevents slow-loris attacks where a client connects but never sends
/// (or slowly trickles) handshake data, holding a server task indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

async fn write_connect_result<T: Transport>(
    transport: &mut T,
    error: Option<ProtocolError>,
) -> Result<()> {
    let response = ConnectResult { error };
    let payload = response.encode().map_err(AmuxError::SerializationEncode)?;
    transport.write_frame(&payload).await
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
    String,
    ConnectionHandle,
    mpsc::Receiver<Message>,
    Uuid,
    Arc<RwLock<ServerUserState>>,
)> {
    for _attempt in 0..5 {
        let payload = transport.read_frame().await?;
        let connect = Connect::decode(&payload).map_err(|e| {
            AmuxError::InvalidMessage(format!("expected Connect during handshake: {e}"))
        })?;
        let Connect {
            link_name: proposed_link,
            token,
            version,
        } = connect;

        if version != PROTOCOL_VERSION {
            tracing::warn!(
                client_version = version,
                server_version = PROTOCOL_VERSION,
                "version mismatch"
            );
            write_connect_result(
                transport,
                Some(ProtocolError::VersionMismatch {
                    server_version: PROTOCOL_VERSION,
                    client_version: version,
                }),
            )
            .await?;
            return Err(AmuxError::VersionMismatch(format!(
                "protocol v{}, client v{}",
                PROTOCOL_VERSION, version
            )));
        }

        if proposed_link.contains('.') {
            tracing::warn!(link = %proposed_link, "rejecting invalid link name (contains '.')");
            write_connect_result(transport, Some(ProtocolError::InvalidLinkName)).await?;
            return Err(AmuxError::Config(format!(
                "Invalid link name '{}': must not contain '.'",
                proposed_link
            )));
        }

        let user_id = if verify_token {
            let (validator, host, tcp_port) = {
                let state = state.read().await;
                let validator = state
                    .jwt_validator
                    .clone()
                    .expect("verify_token=true requires jwt_validator");
                // Safe: cloud mode validation guarantees tcp_port is Some
                let tcp_port = state.config.tcp_port.expect("cloud mode requires tcp_port");
                (validator, state.config.host_name.clone(), tcp_port)
            };

            let token = token.ok_or_else(|| {
                tracing::warn!("token required but none provided");
                AmuxError::InvalidCredentials
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
                            return Err(AmuxError::InvalidCredentials);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "token validation failed");
                    write_connect_result(transport, Some(ProtocolError::InvalidCredentials))
                        .await?;
                    return Err(AmuxError::InvalidCredentials);
                }
            }
        } else {
            LOCAL_USER_ID
        };

        // Get or create user state (read lock fast path, write lock only on first connection)
        let user_state = get_or_create_user_state(state, user_id).await;

        // Check uniqueness under user state lock
        let link_taken = {
            let us = user_state.read().await;
            us.routes.contains_key(&proposed_link)
        };

        if link_taken {
            write_connect_result(transport, Some(ProtocolError::LinkNameTaken)).await?;
            continue;
        }

        let outgoing_rx = {
            let mut us = user_state.write().await;
            // Re-check under write lock to close the race window
            if us.routes.contains_key(&proposed_link) {
                drop(us);
                write_connect_result(transport, Some(ProtocolError::LinkNameTaken)).await?;
                continue;
            }

            let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
            let next_request_id = Arc::new(AtomicU64::new(1));
            let handle = ConnectionHandle::new(outgoing_tx, next_request_id);
            us.routes.insert(proposed_link.clone(), handle.clone());
            (handle, outgoing_rx)
        };

        // Route is inserted — if the success write fails, clean up the stale route
        if let Err(e) = write_connect_result(transport, None).await {
            let mut us = user_state.write().await;
            us.routes.remove(&proposed_link);
            return Err(e);
        }

        let (handle, outgoing_rx) = outgoing_rx;
        return Ok((proposed_link, handle, outgoing_rx, user_id, user_state));
    }

    tracing::warn!("handshake failed after 5 link-name retries");
    Err(AmuxError::TooManyHandshakeAttempts)
}

/// Connect-side handshake: we propose link name, remote validates.
/// Returns the accepted link name on success.
///
/// Used by both the CLI client (local transport) and server-to-server peering (TCP).
/// Retries up to 5 times on link name collision before giving up.
pub async fn connect_handshake<T, F>(transport: &mut T, mut generate_link: F) -> Result<String>
where
    T: Transport,
    F: FnMut() -> String,
{
    for attempt in 0..5 {
        let proposed_link = generate_link();

        let connect = Connect {
            link_name: proposed_link.clone(),
            token: None,
            version: PROTOCOL_VERSION,
        };
        let payload = connect.encode().map_err(AmuxError::SerializationEncode)?;
        transport.write_frame(&payload).await?;

        let payload = tokio::time::timeout(HANDSHAKE_TIMEOUT, transport.read_frame())
            .await
            .map_err(|_| AmuxError::HandshakeTimeout)??;
        let response = ConnectResult::decode(&payload).map_err(|e| {
            AmuxError::InvalidMessage(format!("expected ConnectResult during handshake: {e}"))
        })?;

        match response.error {
            None => return Ok(proposed_link),
            Some(ProtocolError::LinkNameTaken) => {
                tracing::debug!(link = %proposed_link, attempt = attempt + 1, "link name taken, retrying");
                continue;
            }
            Some(ProtocolError::InvalidCredentials) => {
                tracing::error!("authentication failed");
                return Err(AmuxError::InvalidCredentials);
            }
            Some(ProtocolError::InvalidLinkName) => {
                return Err(AmuxError::Config(
                    ProtocolError::InvalidLinkName.to_string(),
                ));
            }
            Some(ProtocolError::VersionMismatch {
                server_version,
                client_version,
            }) => {
                return Err(AmuxError::VersionMismatch(format!(
                    "protocol v{}, client v{}",
                    server_version, client_version
                )));
            }
            Some(other) => {
                let msg = other.to_string();
                return Err(AmuxError::Config(msg));
            }
        }
    }

    Err(AmuxError::Config(
        "handshake failed after 5 link-name collisions — this is usually transient, retry the command".to_string(),
    ))
}

/// Accept a connection with transport split: handshake, then spawn reader/writer tasks
/// and run the connection loop on channels only.
pub(super) async fn accept_connection<T: TransportSplit>(
    mut transport: T,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
    verify_token: bool,
    is_local: bool,
    log_label: &str,
) -> Result<()> {
    // Handshake uses the transport directly (safe — no select! involved).
    // Timeout prevents slow-loris: clients that connect but never send handshake data.
    // The span gives all handshake-phase logs transport context (before conn_span exists).
    let (link_name, route_handle, outgoing_rx, user_id, user_state) = async {
        match tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            accept_handshake(&mut transport, &state, verify_token),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!("handshake timed out");
                Err(AmuxError::HandshakeTimeout)
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
        link = %link_name,
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
        us.peer_links.insert(link_name.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link_name);
    }

    let ctx = ConnectionContext {
        state: state.clone(),
        user_state: user_state.clone(),
        user_id,
        event_tx,
        link_name: link_name.clone(),
        is_local,
        heartbeat_role,
        next_request_id: route_handle.request_counter(),
    };

    run_connection(
        transport,
        outgoing_rx,
        route_handle.sender(),
        ctx,
        None,
        conn_span,
    )
    .await
}

/// WebSocket connection bootstrap - accept, upgrade, and handshake
pub(super) async fn websocket_accept(
    stream: TcpStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
    verify_token: bool,
) -> Result<()> {
    let ws_stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, accept_async(stream))
        .await
        .map_err(|_| {
            tracing::warn!("WebSocket upgrade timed out");
            AmuxError::HandshakeTimeout
        })?
        .map_err(|e| {
            tracing::warn!(error = %e, "WebSocket upgrade failed");
            AmuxError::Io(std::io::Error::other(e.to_string()))
        })?;
    let transport = WebSocketTransport::new(ws_stream);
    accept_connection(transport, state, event_tx, verify_token, false, "websocket").await
}

/// Local transport connection bootstrap - accept and handshake
pub(super) async fn local_accept(
    transport: impl TransportSplit,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
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
    event_tx: mpsc::Sender<super::SessionEvent>,
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
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> Result<()> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|_| AmuxError::Config(format!("Invalid address: {}", address)))?;

    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
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

    let link_name = connect_handshake(&mut transport, || {
        generate_server_link(&hostname, randomise)
    })
    .await?;

    let conn_span = tracing::info_span!(
        "connection",
        link = %link_name,
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
            link_name.clone(),
            ConnectionHandle::new(outgoing_tx.clone(), next_request_id.clone()),
        );
        us.peer_links.insert(link_name.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link_name);
    }

    let state = state.clone();
    let user_state = user_state.clone();
    tokio::spawn(async move {
        let ctx = ConnectionContext {
            state,
            user_state,
            user_id,
            event_tx,
            link_name: link_name.clone(),
            is_local: false,
            heartbeat_role: HeartbeatRole::Dialer,
            next_request_id,
        };
        let _ = run_connection(transport, outgoing_rx, outgoing_tx, ctx, None, conn_span).await;
    });

    Ok(())
}
