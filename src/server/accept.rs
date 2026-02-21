use super::connection::{
    ConnectionContext, cancel_streams_matching, connection_loop, reader_loop, writer_loop,
};
use super::routing::{handle_peer_disconnect, send_initial_announcements};
use super::{LOCAL_USER_ID, ServerState, ServerUserState, get_or_create_user_state};
use crate::error::{AmuxError, Result};
use crate::message::{DirectMessage, Message, PROTOCOL_VERSION, ProtocolError};
use crate::route::generate_server_link;
use crate::transport::{
    TcpTransport, Transport, TransportSplit, UnixTransport, WebSocketTransport,
};
use std::sync::Arc;
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::accept_async;
use tracing::Instrument;
use uuid::Uuid;

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
    mpsc::Receiver<Message>,
    Uuid,
    Arc<RwLock<ServerUserState>>,
)> {
    for _attempt in 0..5 {
        let msg = transport.read_message().await?;
        let (proposed_link, token, version) = match msg {
            Message::Direct(DirectMessage::Connect {
                link_name,
                token,
                version,
            }) => (link_name, token, version),
            other => {
                tracing::error!("expected Connect, got unexpected message");
                drop(other);
                return Err(AmuxError::InvalidMessage);
            }
        };

        if version != PROTOCOL_VERSION {
            tracing::warn!(
                client_version = version,
                server_version = PROTOCOL_VERSION,
                "version mismatch"
            );
            transport
                .write_message(&Message::Direct(DirectMessage::ConnectResult {
                    success: false,
                    error: Some(ProtocolError::VersionMismatch {
                        server_version: PROTOCOL_VERSION,
                        client_version: version,
                    }),
                }))
                .await?;
            return Err(AmuxError::VersionMismatch(format!(
                "protocol v{}, client v{}",
                PROTOCOL_VERSION, version
            )));
        }

        if proposed_link.contains('.') {
            tracing::warn!(link = %proposed_link, "rejecting invalid link name (contains '.')");
            transport
                .write_message(&Message::Direct(DirectMessage::ConnectResult {
                    success: false,
                    error: Some(ProtocolError::InvalidLinkName),
                }))
                .await?;
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
                (
                    validator,
                    state.config.host_name.clone(),
                    state.config.tcp_port,
                )
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
                            transport
                                .write_message(&Message::Direct(DirectMessage::ConnectResult {
                                    success: false,
                                    error: Some(ProtocolError::InvalidCredentials),
                                }))
                                .await?;
                            return Err(AmuxError::InvalidCredentials);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "token validation failed");
                    transport
                        .write_message(&Message::Direct(DirectMessage::ConnectResult {
                            success: false,
                            error: Some(ProtocolError::InvalidCredentials),
                        }))
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
            transport
                .write_message(&Message::Direct(DirectMessage::ConnectResult {
                    success: false,
                    error: Some(ProtocolError::LinkNameTaken),
                }))
                .await?;
            continue;
        }

        let outgoing_rx = {
            let mut us = user_state.write().await;
            // Re-check under write lock to close the race window
            if us.routes.contains_key(&proposed_link) {
                drop(us);
                transport
                    .write_message(&Message::Direct(DirectMessage::ConnectResult {
                        success: false,
                        error: Some(ProtocolError::LinkNameTaken),
                    }))
                    .await?;
                continue;
            }

            let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
            us.routes.insert(proposed_link.clone(), outgoing_tx);
            outgoing_rx
        };

        // Route is inserted — if the success write fails, clean up the stale route
        if let Err(e) = transport
            .write_message(&Message::Direct(DirectMessage::ConnectResult {
                success: true,
                error: None,
            }))
            .await
        {
            let mut us = user_state.write().await;
            us.routes.remove(&proposed_link);
            return Err(e);
        }

        return Ok((proposed_link, outgoing_rx, user_id, user_state));
    }

    Err(AmuxError::TooManyHandshakeAttempts)
}

/// Connect-side handshake: we propose link name, remote validates.
/// Returns the accepted link name on success.
pub(super) async fn connect_handshake<T, F>(
    transport: &mut T,
    mut generate_link: F,
) -> Result<String>
where
    T: Transport,
    F: FnMut() -> String,
{
    for attempt in 0..5 {
        let proposed_link = generate_link();

        transport
            .write_message(&Message::Direct(DirectMessage::Connect {
                link_name: proposed_link.clone(),
                token: None,
                version: PROTOCOL_VERSION,
            }))
            .await?;

        let response = transport.read_message().await?;
        match response {
            Message::Direct(DirectMessage::ConnectResult { success: true, .. }) => {
                return Ok(proposed_link);
            }
            Message::Direct(DirectMessage::ConnectResult {
                success: false,
                error: Some(ProtocolError::LinkNameTaken),
            }) => {
                tracing::debug!(link = %proposed_link, attempt = attempt + 1, "link name taken, retrying");
                continue;
            }
            Message::Direct(DirectMessage::ConnectResult {
                success: false,
                error: Some(ProtocolError::InvalidCredentials),
            }) => {
                tracing::error!("authentication failed");
                return Err(AmuxError::InvalidCredentials);
            }
            Message::Direct(DirectMessage::ConnectResult {
                success: false,
                error: Some(ProtocolError::InvalidLinkName),
            }) => {
                return Err(AmuxError::Config(
                    ProtocolError::InvalidLinkName.to_string(),
                ));
            }
            Message::Direct(DirectMessage::ConnectResult {
                success: false,
                error:
                    Some(ProtocolError::VersionMismatch {
                        server_version,
                        client_version,
                    }),
            }) => {
                return Err(AmuxError::VersionMismatch(format!(
                    "protocol v{}, client v{}",
                    server_version, client_version
                )));
            }
            Message::Direct(DirectMessage::ConnectResult {
                success: false,
                error,
            }) => {
                let msg = error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection rejected".to_string());
                return Err(AmuxError::Config(msg));
            }
            Message::Direct(DirectMessage::Error { message }) => {
                return Err(AmuxError::ServerError(message));
            }
            _ => return Err(AmuxError::InvalidMessage),
        }
    }

    Err(AmuxError::Config(
        "Failed to connect after 5 attempts".to_string(),
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
    // Handshake uses the transport directly (safe — no select! involved)
    let (link_name, outgoing_rx, user_id, user_state) =
        match accept_handshake(&mut transport, &state, verify_token).await {
            Ok(result) => result,
            Err(e) => {
                let _ = transport.write_message(&Message::from(&e)).await;
                return Err(e);
            }
        };

    let conn_span = tracing::info_span!("connection", link = %link_name, transport = log_label);
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

    // Split transport into reader/writer halves
    let (reader, writer) = transport.into_split();

    // Get the route's tx for the response channel
    let response_tx = {
        let us = user_state.read().await;
        us.routes.get(&link_name).unwrap().clone()
    };

    // Spawn reader and writer tasks
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle =
        tokio::spawn(reader_loop(reader, incoming_tx).instrument(conn_span.clone()));
    let writer_handle =
        tokio::spawn(writer_loop(writer, outgoing_rx).instrument(conn_span.clone()));

    let ctx = ConnectionContext {
        state: state.clone(),
        user_state: user_state.clone(),
        user_id,
        event_tx,
        link_name: link_name.clone(),
    };

    let response_tx_cleanup = response_tx.clone();
    let result = connection_loop(incoming_rx, response_tx, ctx, None)
        .instrument(conn_span.clone())
        .await;

    // Error write-back through the channel (writer task may still be alive)
    if let Err(ref e) = result {
        tracing::debug!(parent: &conn_span, error = %e, "connection error");
        let _ = response_tx_cleanup.send(Message::from(e)).await;
    }

    // Cleanup: remove route, cancel streams, drop sender clones so writer task exits
    {
        let mut us = user_state.write().await;
        if !is_local {
            handle_peer_disconnect(&mut us, &link_name);
        } else {
            us.routes.remove(&link_name);
            // Cancel streams spawned for this local connection so their sender
            // clones are dropped and the writer task can exit
            cancel_streams_matching(&mut us, |entry| entry.link == link_name);
        }
    }
    // Drop last sender clone → writer rx returns None → writer exits
    drop(response_tx_cleanup);

    // Await writer (let it drain), then abort reader
    let _ = writer_handle.await;
    reader_handle.abort();

    tracing::info!(parent: &conn_span, "connection closed");

    result
}

/// WebSocket connection bootstrap - accept, upgrade, and handshake
pub(super) async fn websocket_accept(
    stream: TcpStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
    verify_token: bool,
) -> Result<()> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;
    let transport = WebSocketTransport::new(ws_stream);
    accept_connection(transport, state, event_tx, verify_token, false, "websocket").await
}

/// Unix socket connection bootstrap - accept and handshake
pub(super) async fn unix_accept(
    stream: UnixStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> Result<()> {
    let transport = UnixTransport::new(stream);
    accept_connection(transport, state, event_tx, false, true, "unix").await
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

    let conn_span = tracing::info_span!("connection", link = %link_name, transport = "tcp");
    tracing::info!(parent: &conn_span, "peer handshake complete");

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let (host_id, host_name, is_cloud_server) = {
            let s = state.read().await;
            (s.host_id, s.config.host_name.clone(), s.is_cloud_server)
        };
        let mut us = user_state.write().await;
        us.routes.insert(link_name.clone(), outgoing_tx.clone());
        us.peer_links.insert(link_name.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link_name);
    }

    // Split transport into reader/writer halves
    let (reader, writer) = transport.into_split();

    let state = state.clone();
    let user_state = user_state.clone();
    let link_name_clone = link_name.clone();
    let task_span = conn_span.clone();
    tokio::spawn(
        async move {
            // Spawn reader and writer tasks
            let (incoming_tx, incoming_rx) = mpsc::channel(256);
            let reader_handle =
                tokio::spawn(reader_loop(reader, incoming_tx).instrument(task_span.clone()));
            let writer_handle =
                tokio::spawn(writer_loop(writer, outgoing_rx).instrument(task_span.clone()));

            let ctx = ConnectionContext {
                state: state.clone(),
                user_state: user_state.clone(),
                user_id,
                event_tx,
                link_name: link_name_clone.clone(),
            };
            let result = connection_loop(incoming_rx, outgoing_tx.clone(), ctx, None).await;

            if let Err(ref e) = result {
                tracing::debug!(error = %e, "peer connection error");
                let _ = outgoing_tx.send(Message::from(e)).await;
            }

            let mut us = user_state.write().await;
            handle_peer_disconnect(&mut us, &link_name_clone);

            // Drop all sender clones so writer drains and exits
            drop(outgoing_tx);
            drop(us);

            let _ = writer_handle.await;
            reader_handle.abort();

            tracing::info!("peer connection closed");
        }
        .instrument(conn_span),
    );

    Ok(())
}
