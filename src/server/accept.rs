use super::connection::{connection_loop, ConnectionContext};
use super::ServerState;
use crate::error::{AmuxError, Result};
use crate::message::{LocalMessage, Message, ProtocolError};
use crate::route::generate_server_link;
use crate::transport::{TcpTransport, Transport, UnixTransport, WebSocketTransport};
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
            _ => return Err(AmuxError::InvalidMessage),
        };

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

pub(super) async fn accept_connection<T: Transport>(
    mut transport: T,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
    verify_token: bool,
    log_label: &str,
) -> Result<()> {
    let (link_name, outgoing_rx) =
        match accept_handshake(&mut transport, &state, verify_token).await {
            Ok(result) => result,
            Err(e) => {
                let _ = transport.write_message(&Message::from(&e)).await;
                return Err(e);
            }
        };

    log!("server: {} connection {} established", log_label, link_name);

    let ctx = ConnectionContext {
        state: state.clone(),
        event_tx,
        link_name: link_name.clone(),
    };

    let result = connection_loop(&mut transport, outgoing_rx, ctx).await;

    if let Err(ref e) = result {
        log!("server: {} {} error: {}", log_label, link_name, e);
        let _ = transport.write_message(&Message::from(e)).await;
    }

    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }
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
    accept_connection(transport, state, event_tx, false, "websocket").await
}

/// Unix socket connection bootstrap - accept and handshake
pub(super) async fn unix_accept(
    stream: UnixStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> Result<()> {
    let transport = UnixTransport::new(stream);
    accept_connection(transport, state, event_tx, false, "unix").await
}

/// TCP peer bootstrap - accept inbound connection and run handshake.
///
/// Generic over transport type to support both plain TCP and TLS connections.
/// Set `verify_token` to true for cloud server mode (validates JWT in Connect message).
pub(super) async fn tcp_accept<T: Transport>(
    transport: T,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
    verify_token: bool,
) -> Result<()> {
    accept_connection(transport, state, event_tx, verify_token, "tcp").await
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
        state.routes.insert(link_name.clone(), outgoing_tx);
    }

    let state = state.clone();
    let link_name_clone = link_name.clone();
    tokio::spawn(async move {
        let mut transport = transport;
        let ctx = ConnectionContext {
            state: state.clone(),
            event_tx,
            link_name: link_name_clone.clone(),
        };
        let result = connection_loop(&mut transport, outgoing_rx, ctx).await;

        if let Err(ref e) = result {
            log!("server: tcp peer {} error: {}", link_name_clone, e);
            let _ = transport.write_message(&Message::from(e)).await;
        }

        let mut state = state.write().await;
        state.routes.remove(&link_name_clone);
        log!("server: tcp peer {} closed", link_name_clone);
    });

    Ok(())
}
