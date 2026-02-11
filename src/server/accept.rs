use super::connection::{
    cancel_streams_matching, connection_loop, reader_loop, writer_loop, ConnectionContext,
};
use super::routing::{handle_peer_disconnect, send_initial_announcements};
use super::ServerState;
use crate::error::{AmuxError, Result};
use crate::message::{LocalMessage, Message, ProtocolError};
use crate::route::generate_server_link;
use crate::transport::{
    TcpTransport, Transport, TransportSplit, UnixTransport, WebSocketTransport,
};
use std::sync::Arc;
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::accept_async;

/// Accept-side handshake: client proposes link name, we validate against routes.
/// Atomically checks uniqueness and inserts the route under a write lock.
/// Returns the accepted link name and the outgoing message receiver on success.
///
/// If `verify_token` is true (cloud server mode), the token in the Connect message
/// is validated via JWT. If validation fails, InvalidCredentials is returned.
pub(super) async fn accept_handshake<T: Transport>(
    transport: &mut T,
    state: &Arc<RwLock<ServerState>>,
    verify_token: bool,
) -> Result<(String, mpsc::Receiver<Message>)> {
    for _attempt in 0..5 {
        let msg = transport.read_message().await?;
        let (proposed_link, token) = match msg {
            Message::Local(LocalMessage::Connect { link_name, token }) => (link_name, token),
            other => {
                log!("server: expected Connect message, got {:?}", other);
                return Err(AmuxError::InvalidMessage);
            }
        };

        if proposed_link.contains('.') {
            log!(
                "server: rejecting invalid link name '{}' (contains '.')",
                proposed_link
            );
            transport
                .write_message(&Message::Local(LocalMessage::ConnectResponse {
                    success: false,
                    error: Some(ProtocolError::InvalidLinkName),
                }))
                .await?;
            return Err(AmuxError::Config(format!(
                "Invalid link name '{}': must not contain '.'",
                proposed_link
            )));
        }

        if verify_token {
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
                log!("server: token required but none provided");
                AmuxError::InvalidCredentials
            })?;

            match validator.validate(&token, &host, tcp_port).await {
                Ok(claims) => {
                    log!("server: authenticated connection from user {}", claims.sub);
                }
                Err(e) => {
                    log!("server: token validation failed: {}", e);
                    transport
                        .write_message(&Message::Local(LocalMessage::ConnectResponse {
                            success: false,
                            error: Some(ProtocolError::InvalidCredentials),
                        }))
                        .await?;
                    return Err(AmuxError::InvalidCredentials);
                }
            }
        }

        // Atomically check uniqueness and insert route under write lock.
        // The lock is dropped before any I/O to avoid stalling other tasks.
        let link_taken = {
            let state = state.read().await;
            state.routes.contains_key(&proposed_link)
        };

        if link_taken {
            transport
                .write_message(&Message::Local(LocalMessage::ConnectResponse {
                    success: false,
                    error: Some(ProtocolError::LinkNameTaken),
                }))
                .await?;
            continue;
        }

        let outgoing_rx = {
            let mut state = state.write().await;
            // Re-check under write lock to close the race window
            if state.routes.contains_key(&proposed_link) {
                drop(state);
                transport
                    .write_message(&Message::Local(LocalMessage::ConnectResponse {
                        success: false,
                        error: Some(ProtocolError::LinkNameTaken),
                    }))
                    .await?;
                continue;
            }

            let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
            state.routes.insert(proposed_link.clone(), outgoing_tx);
            outgoing_rx
        };

        // Route is inserted — if the success write fails, clean up the stale route
        if let Err(e) = transport
            .write_message(&Message::Local(LocalMessage::ConnectResponse {
                success: true,
                error: None,
            }))
            .await
        {
            let mut state = state.write().await;
            state.routes.remove(&proposed_link);
            return Err(e);
        }

        return Ok((proposed_link, outgoing_rx));
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
            .write_message(&Message::Local(LocalMessage::Connect {
                link_name: proposed_link.clone(),
                token: None,
            }))
            .await?;

        let response = transport.read_message().await?;
        match response {
            Message::Local(LocalMessage::ConnectResponse { success: true, .. }) => {
                return Ok(proposed_link);
            }
            Message::Local(LocalMessage::ConnectResponse {
                success: false,
                error: Some(ProtocolError::LinkNameTaken),
            }) => {
                log!(
                    "link name {} taken, retrying (attempt {})",
                    proposed_link,
                    attempt + 1
                );
                continue;
            }
            Message::Local(LocalMessage::ConnectResponse {
                success: false,
                error: Some(ProtocolError::InvalidCredentials),
            }) => {
                log!("server: invalid credentials - authentication failed");
                return Err(AmuxError::InvalidCredentials);
            }
            Message::Local(LocalMessage::ConnectResponse {
                success: false,
                error: Some(ProtocolError::InvalidLinkName),
            }) => {
                return Err(AmuxError::Config(
                    ProtocolError::InvalidLinkName.to_string(),
                ));
            }
            Message::Local(LocalMessage::ConnectResponse {
                success: false,
                error,
            }) => {
                let msg = error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection rejected".to_string());
                return Err(AmuxError::Config(msg));
            }
            Message::Local(LocalMessage::Error { message }) => {
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
    let (link_name, outgoing_rx) =
        match accept_handshake(&mut transport, &state, verify_token).await {
            Ok(result) => result,
            Err(e) => {
                let _ = transport.write_message(&Message::from(&e)).await;
                return Err(e);
            }
        };

    log!("server: {} connection {} established", log_label, link_name);

    if !is_local {
        let mut s = state.write().await;
        s.peer_links.insert(link_name.clone());
        send_initial_announcements(&s, &link_name);
    }

    // Split transport into reader/writer halves
    let (reader, writer) = transport.into_split();

    // Get the route's tx for the response channel
    let response_tx = {
        let s = state.read().await;
        s.routes.get(&link_name).unwrap().clone()
    };

    // Spawn reader and writer tasks
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle = tokio::spawn(reader_loop(reader, incoming_tx));
    let writer_handle = tokio::spawn(writer_loop(writer, outgoing_rx));

    let ctx = ConnectionContext {
        state: state.clone(),
        event_tx,
        link_name: link_name.clone(),
    };

    let response_tx_cleanup = response_tx.clone();
    let result = connection_loop(incoming_rx, response_tx, ctx, None).await;

    // Error write-back through the channel (writer task may still be alive)
    if let Err(ref e) = result {
        log!("server: {} {} error: {}", log_label, link_name, e);
        let _ = response_tx_cleanup.send(Message::from(e)).await;
    }

    // Cleanup: remove route, cancel streams, drop sender clones so writer task exits
    {
        let mut s = state.write().await;
        if !is_local {
            handle_peer_disconnect(&mut s, &link_name);
        } else {
            s.routes.remove(&link_name);
            // Cancel streams spawned for this local connection so their sender
            // clones are dropped and the writer task can exit
            cancel_streams_matching(&mut s, |entry| entry.link == link_name);
        }
    }
    // Drop last sender clone → writer rx returns None → writer exits
    drop(response_tx_cleanup);

    // Await writer (let it drain), then abort reader
    let _ = writer_handle.await;
    reader_handle.abort();

    log!("server: {} connection {} closed", log_label, link_name);

    result
}

/// WebSocket connection bootstrap - accept, upgrade, and handshake
// TODO: In cloud mode, WebSocket connections should require token validation
// (verify_token should be true when state.cloud_mode is true). Currently all
// WebSocket connections bypass authentication.
pub(super) async fn websocket_accept(
    stream: TcpStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> Result<()> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;
    let transport = WebSocketTransport::new(ws_stream);
    accept_connection(transport, state, event_tx, false, false, "websocket").await
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
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> Result<()> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|_| AmuxError::Config(format!("Invalid address: {}", address)))?;

    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;

    log!("server: connected to remote server at {}", addr);

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

    log!(
        "server: handshake complete with remote server (link: {})",
        link_name
    );

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx.clone());
        state.peer_links.insert(link_name.clone());
        send_initial_announcements(&state, &link_name);
    }

    // Split transport into reader/writer halves
    let (reader, writer) = transport.into_split();

    let state = state.clone();
    let link_name_clone = link_name.clone();
    tokio::spawn(async move {
        // Spawn reader and writer tasks
        let (incoming_tx, incoming_rx) = mpsc::channel(256);
        let reader_handle = tokio::spawn(reader_loop(reader, incoming_tx));
        let writer_handle = tokio::spawn(writer_loop(writer, outgoing_rx));

        let ctx = ConnectionContext {
            state: state.clone(),
            event_tx,
            link_name: link_name_clone.clone(),
        };
        let result = connection_loop(incoming_rx, outgoing_tx.clone(), ctx, None).await;

        if let Err(ref e) = result {
            log!("server: tcp peer {} error: {}", link_name_clone, e);
            let _ = outgoing_tx.send(Message::from(e)).await;
        }

        let mut state = state.write().await;
        handle_peer_disconnect(&mut state, &link_name_clone);

        // Drop all sender clones so writer drains and exits
        drop(outgoing_tx);
        drop(state);

        let _ = writer_handle.await;
        reader_handle.abort();

        log!("server: tcp peer {} closed", link_name_clone);
    });

    Ok(())
}
