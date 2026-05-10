//! Connection acceptance and handshake for all transport types.
//!
//! Each transport (Unix, TCP, TLS, WebSocket) has an `*_accept` entry point that
//! bootstraps the connection: upgrade (WebSocket) or wrap (TLS), run the
//! [`accept_handshake`] protocol, then hand off to
//! [`run_connection`](super::connection::run_connection) for the connection
//! lifecycle. [`tcp_connect`] handles the outbound (client-side) direction for
//! server-to-server peering.

use std::sync::Arc;
use std::time::Duration;

use prost::Message as _;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::Instrument;
use uuid::Uuid;

use super::connection::{
    ConnectionContext, HeartbeatRole, HeartbeatSetup, RunConnection, run_connection,
};
use super::{
    ConnectionHandle, LOCAL_USER_ID, ServerState, ServerUserState, ensure_user_state, local_host,
    validate_remote_host,
};
use crate::agent::SessionEvent;
use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION, RoutingRole};
use crate::protocol::link::Link;
use crate::protocol::message::{FrameBody, Host, Message, PeerFrame, ProtocolError, RequestFrame};
use crate::protocol::route::generate_server_link;
use crate::protocol::{method, wire};
use crate::rpc::RpcPeerStreamOutboundStart;
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
    #[error("amux update required (minimum v{minimum_version}, you have v{client_version})")]
    UpdateRequired {
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
            supported_versions,
            peer_supported_versions,
        }) => AcceptError::ProtocolMismatch {
            server_version: first_protocol_version(&supported_versions),
            client_version: first_protocol_version(&peer_supported_versions),
        },
        HandshakeError::Protocol(ProtocolError::UpdateRequired {
            minimum_version,
            client_version,
        }) => AcceptError::UpdateRequired {
            minimum_version,
            client_version,
        },
        HandshakeError::Protocol(other) => AcceptError::Config(other.to_string()),
    }
}

fn first_protocol_version(versions: &[u32]) -> u32 {
    versions.first().copied().unwrap_or_default()
}

async fn write_connect_result<T: Transport>(
    transport: &mut T,
    error: Option<ProtocolError>,
    idle_timeout_secs: Option<u32>,
    assigned_link_name: Option<&Link>,
    host: Option<&Host>,
    routing_role: Option<RoutingRole>,
) -> Result<()> {
    let payload = ConnectResult {
        error,
        idle_timeout_secs,
        assigned_link_name: assigned_link_name.map(|link| link.as_str().to_string()),
        host: host.cloned(),
        routing_role,
    }
    .encode()
    .map_err(TransportError::from)?;
    transport.write_frame(&payload).await?;
    Ok(())
}

async fn write_connect_response_payload<T: Transport>(
    transport: &mut T,
    payload: Vec<u8>,
) -> Result<()> {
    transport.write_frame(&payload).await?;
    Ok(())
}

const LINK_ASSIGNMENT_ATTEMPTS: usize = 5;
const ASSIGNED_LINK_SUFFIX_LEN: usize = 8;

fn reserve_authoritative_link(
    user_state: &mut ServerUserState,
    proposed: &Link,
) -> Option<(Link, ConnectionHandle, mpsc::Receiver<Message>)> {
    for attempt in 0..LINK_ASSIGNMENT_ATTEMPTS {
        let candidate = assigned_link_candidate(proposed, attempt);
        match user_state.try_reserve_link(candidate.clone()) {
            Ok((handle, outgoing_rx)) => return Some((candidate, handle, outgoing_rx)),
            Err(_) => continue,
        }
    }
    None
}

fn assigned_link_candidate(proposed: &Link, attempt: usize) -> Link {
    if attempt == 0 {
        return proposed.clone();
    }

    let suffix = Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..ASSIGNED_LINK_SUFFIX_LEN];
    let max_base_len = 128 - 1 - ASSIGNED_LINK_SUFFIX_LEN;
    let base = proposed.as_str();
    let base = &base[..base.len().min(max_base_len)];
    Link::new(format!("{base}-{suffix}")).expect("candidate link is derived from a valid link")
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
    is_local: bool,
    idle_timeout_secs: Option<u32>,
) -> Result<(
    Link,
    ConnectionHandle,
    mpsc::Receiver<Message>,
    Uuid,
    Arc<RwLock<ServerUserState>>,
    Option<Host>,
    RoutingRole,
    RoutingRole,
)> {
    let payload = transport.read_frame().await?;
    let connect = Connect::decode(&payload).map_err(|e| {
        AcceptError::InvalidHandshake(format!("expected Connect during handshake: {e}"))
    })?;
    let Connect {
        link_name: proposed_link,
        token,
        version,
        supported_versions,
        host,
        routing_role: remote_routing_role,
    } = connect;

    if version != PROTOCOL_VERSION {
        tracing::warn!(
            client_version = version,
            server_version = PROTOCOL_VERSION,
            "protocol mismatch"
        );
        write_connect_response_payload(
            transport,
            wire::encode_connect_protocol_version_mismatch_response(
                &[PROTOCOL_VERSION],
                &supported_versions,
            ),
        )
        .await?;
        return Err(AcceptError::ProtocolMismatch {
            server_version: PROTOCOL_VERSION,
            client_version: version,
        });
    }

    let local_routing_role = {
        let state = state.read().await;
        if is_local {
            RoutingRole::Observer
        } else if state.is_cloud_server() {
            RoutingRole::Relay
        } else {
            RoutingRole::Host
        }
    };

    let peer_host = match (is_local, host) {
        (true, None) if remote_routing_role == RoutingRole::Observer => None,
        (true, _) => {
            write_connect_result(
                transport,
                Some(ProtocolError::InvalidArgument {
                    message:
                        "local connections must use observer routing role and omit host identity"
                            .to_string(),
                }),
                None,
                None,
                None,
                None,
            )
            .await?;
            return Err(AcceptError::InvalidHandshake(
                "local connection used non-observer routing role or sent host identity".to_string(),
            ));
        }
        (false, None) => {
            write_connect_result(
                transport,
                Some(ProtocolError::InvalidArgument {
                    message: "non-local connections must send host identity".to_string(),
                }),
                None,
                None,
                None,
                None,
            )
            .await?;
            return Err(AcceptError::InvalidHandshake(
                "non-local connection omitted host identity".to_string(),
            ));
        }
        (false, Some(host)) => {
            if let Err(message) = validate_remote_host(&host) {
                write_connect_result(
                    transport,
                    Some(ProtocolError::InvalidArgument {
                        message: message.clone(),
                    }),
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
                return Err(AcceptError::InvalidHandshake(message));
            }
            Some(host)
        }
    };

    // Direct peer identity is established by the handshake, not by routed HostUp events.
    if let Some(ref host) = peer_host {
        let local_host_id = {
            let s = state.read().await;
            s.host_id()
        };
        if host.id == local_host_id {
            write_connect_result(
                transport,
                Some(ProtocolError::InvalidArgument {
                    message: "peer host_id must not match local host_id".to_string(),
                }),
                None,
                None,
                None,
                None,
            )
            .await?;
            return Err(AcceptError::InvalidHandshake(
                "peer host_id matched local host_id".to_string(),
            ));
        }
    }

    let proposed_link = match Link::new(proposed_link.clone()) {
        Ok(link) => link,
        Err(error) => {
            let reason = error.to_string();
            tracing::warn!(link = %proposed_link, reason, "rejecting invalid link name");
            write_connect_response_payload(
                transport,
                wire::encode_connect_invalid_link_name_response(proposed_link.as_str(), &reason),
            )
            .await?;
            return Err(AcceptError::InvalidHandshake(format!(
                "Invalid link name '{}': {}",
                proposed_link, reason
            )));
        }
    };

    let user_id = if verify_token {
        let Some(token) = token else {
            tracing::warn!("token required but none provided");
            write_connect_result(
                transport,
                Some(ProtocolError::InvalidCredentials),
                None,
                None,
                None,
                None,
            )
            .await?;
            return Err(AcceptError::InvalidCredentials);
        };

        let (validator, host, tcp_port) = {
            let state = state.read().await;
            let validator = state
                .jwt_validator
                .clone()
                .expect("verify_token=true requires jwt_validator");
            let tcp_port = state.config.tcp_port.expect("cloud mode requires tcp_port");
            (validator, state.config.host_name.clone(), tcp_port)
        };

        match validator.validate(&token, &host, tcp_port).await {
            Ok(claims) => {
                tracing::info!(user_id = %claims.sub, "authenticated");
                let user_id = match claims.sub.parse::<Uuid>() {
                    Ok(user_id) => user_id,
                    Err(_) => {
                        tracing::error!(sub = %claims.sub, "invalid user_id in token");
                        write_connect_result(
                            transport,
                            Some(ProtocolError::InvalidCredentials),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await?;
                        return Err(AcceptError::InvalidCredentials);
                    }
                };

                if let Some(ref host) = peer_host {
                    let client_id = &claims.client_id;
                    let min_version = {
                        let state = state.read().await;
                        state.config.minimum_client_versions.get(client_id).cloned()
                    };
                    if let Some(ref min_ver_str) = min_version {
                        let cv = host.version.as_str();
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
                                client_id = %client_id,
                                client_version = %cv,
                                minimum_version = %min_ver_str,
                                "client version below minimum"
                            );
                            write_connect_result(
                                transport,
                                Some(ProtocolError::UpdateRequired {
                                    minimum_version: min_ver_str.clone(),
                                    client_version: cv.clone(),
                                }),
                                None,
                                None,
                                None,
                                None,
                            )
                            .await?;
                            return Err(AcceptError::UpdateRequired {
                                minimum_version: min_ver_str.clone(),
                                client_version: cv,
                            });
                        }
                    }
                }

                user_id
            }
            Err(e) => {
                tracing::warn!(error = %e, "token validation failed");
                write_connect_result(
                    transport,
                    Some(ProtocolError::InvalidCredentials),
                    None,
                    None,
                    None,
                    None,
                )
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
        reserve_authoritative_link(&mut us, &proposed_link)
    };
    let (assigned_link, handle, outgoing_rx) = match reservation {
        Some(pair) => pair,
        None => {
            let message = format!(
                "unable to assign link name for proposed link `{}` after {} attempts",
                proposed_link, LINK_ASSIGNMENT_ATTEMPTS
            );
            write_connect_result(
                transport,
                Some(ProtocolError::ResourceExhausted {
                    message: message.clone(),
                }),
                None,
                None,
                None,
                None,
            )
            .await?;
            return Err(AcceptError::TooManyHandshakeAttempts);
        }
    };

    let accepted_host = if is_local {
        None
    } else {
        let state = state.read().await;
        Some(local_host(state.host_id(), state.host_name()))
    };

    // Route is inserted — if the success write fails, clean up the stale route
    if let Err(e) = write_connect_result(
        transport,
        None,
        idle_timeout_secs,
        Some(&assigned_link),
        accepted_host.as_ref(),
        Some(local_routing_role),
    )
    .await
    {
        let mut us = user_state.write().await;
        us.remove_link(&assigned_link);
        return Err(e);
    }

    Ok((
        assigned_link,
        handle,
        outgoing_rx,
        user_id,
        user_state,
        peer_host,
        remote_routing_role,
        local_routing_role,
    ))
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
    let idle_timeout_secs = if is_local {
        None
    } else {
        Some(state.read().await.config.idle_timeout_secs)
    };

    // Handshake uses the transport directly (safe — no select! involved).
    // Timeout prevents slow-loris: clients that connect but never send handshake data.
    // The span gives all handshake-phase logs transport context (before conn_span exists).
    let (
        link,
        route_handle,
        outgoing_rx,
        user_id,
        user_state,
        peer_host,
        remote_routing_role,
        local_routing_role,
    ) = async {
        match tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            accept_handshake(
                &mut transport,
                &state,
                verify_token,
                is_local,
                idle_timeout_secs,
            ),
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

    let heartbeat = idle_timeout_secs.map(|secs| HeartbeatSetup {
        role: HeartbeatRole::Acceptor,
        idle_timeout: std::time::Duration::from_secs(secs.into()),
    });
    let conn_span = tracing::info_span!(
        "connection",
        link = %link,
        transport = log_label,
        user_id = %user_id,
        heartbeat_role = heartbeat.map(|h| h.role.as_str()).unwrap_or("disabled"),
        local_routing_role = local_routing_role.as_str(),
        remote_routing_role = remote_routing_role.as_str(),
    );
    tracing::info!(parent: &conn_span, "connection established");

    let initial_messages = if !is_local {
        let rpc = {
            let mut us = user_state.write().await;
            us.mark_peer_link(link.clone());
            if remote_routing_role.is_direct_host() {
                let host = peer_host
                    .clone()
                    .expect("host routing role requires host identity");
                let change = us.apply_direct_peer_host_up(&link, host);
                for event in &change.events {
                    super::broadcast_topology_event(&mut us, event, Some(&link));
                }
            }
            us.rpc_for_link(&link)
                .expect("reserved peer route should have RPC state")
        };
        if remote_routing_role.serves_routing_events() {
            let routing_call_id = crate::protocol::CallId::from(Uuid::new_v4());
            rpc.register_peer_stream_outbound(RpcPeerStreamOutboundStart {
                call_id: routing_call_id.clone(),
                link: link.clone(),
                method: method::ROUTING_SUBSCRIBE_EVENTS,
            })
            .expect("fresh peer routing call id should not collide");
            vec![Message::Peer(PeerFrame {
                call_id: routing_call_id,
                body: FrameBody::Request(RequestFrame {
                    method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                    payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                }),
            })]
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let ctx = ConnectionContext {
        state: state.clone(),
        user_state: user_state.clone(),
        rpc: user_state
            .read()
            .await
            .rpc_for_link(&link)
            .expect("reserved route should have RPC state"),
        user_id,
        event_tx,
        link: link.clone(),
        is_local,
        heartbeat,
        routing_role: local_routing_role,
    };

    run_connection(RunConnection {
        transport,
        outgoing_rx,
        initial_messages,
        response_tx: route_handle.sender(),
        close_rx: route_handle.close_receiver(),
        ctx,
        token_refresh: None,
        span: conn_span,
    })
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

    let (host, randomise) = {
        let state = state.read().await;
        (
            local_host(state.host_id, &state.config.host_name),
            state.config.randomise_link_name,
        )
    };

    let hostname = host.name.clone();
    let local_host_id = host.id;
    let outcome = connect_handshake(
        &mut transport,
        || generate_server_link(&hostname, randomise),
        Some(host),
        RoutingRole::Host,
    )
    .await
    .map_err(map_handshake_error)?;
    let link = outcome.link;
    let remote_routing_role = outcome.routing_role;
    let heartbeat = outcome.idle_timeout_secs.map(|secs| HeartbeatSetup {
        role: HeartbeatRole::Dialer,
        idle_timeout: std::time::Duration::from_secs(secs.into()),
    });
    let peer_host = match outcome.host {
        Some(host) => {
            if let Err(message) = validate_remote_host(&host) {
                return Err(AcceptError::InvalidHandshake(format!(
                    "accepted peer host identity is invalid: {message}"
                )));
            }
            if host.id == local_host_id {
                return Err(AcceptError::InvalidHandshake(
                    "accepted peer host_id matched local host_id".to_string(),
                ));
            }
            Some(host)
        }
        None => {
            return Err(AcceptError::InvalidHandshake(
                "accepted peer connection omitted host identity".to_string(),
            ));
        }
    };

    let conn_span = tracing::info_span!(
        "connection",
        link = %link,
        transport = "tcp",
        user_id = %user_id,
        heartbeat_role = heartbeat.map(|h| h.role.as_str()).unwrap_or("disabled"),
        local_routing_role = RoutingRole::Host.as_str(),
        remote_routing_role = remote_routing_role.as_str(),
    );
    tracing::info!(parent: &conn_span, "peer handshake complete");

    let (route_handle, outgoing_rx, initial_messages, routing_call_id) = {
        let mut us = user_state.write().await;
        let (route_handle, outgoing_rx) = us.try_reserve_link(link.clone()).map_err(|_| {
            AcceptError::Config(format!("assigned link `{link}` is already connected"))
        })?;
        us.mark_peer_link(link.clone());
        if remote_routing_role.is_direct_host() {
            let host = peer_host.expect("host routing role requires host identity");
            let change = us.apply_direct_peer_host_up(&link, host);
            for event in &change.events {
                super::broadcast_topology_event(&mut us, event, Some(&link));
            }
        }
        let routing_call_id = remote_routing_role
            .serves_routing_events()
            .then(|| crate::protocol::CallId::from(Uuid::new_v4()));
        let initial_messages = routing_call_id
            .as_ref()
            .map(|call_id| {
                Message::Peer(PeerFrame {
                    call_id: call_id.clone(),
                    body: FrameBody::Request(RequestFrame {
                        method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                        payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                    }),
                })
            })
            .into_iter()
            .collect();
        (route_handle, outgoing_rx, initial_messages, routing_call_id)
    };
    let rpc = user_state
        .read()
        .await
        .rpc_for_link(&link)
        .expect("reserved peer route should have RPC state");
    if let Some(routing_call_id) = routing_call_id {
        rpc.register_peer_stream_outbound(RpcPeerStreamOutboundStart {
            call_id: routing_call_id.clone(),
            link: link.clone(),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
        })
        .expect("fresh peer routing call id should not collide");
    }

    let state = state.clone();
    let user_state = user_state.clone();
    tokio::spawn(async move {
        let rpc = user_state
            .read()
            .await
            .rpc_for_link(&link)
            .expect("reserved peer route should have RPC state");
        let ctx = ConnectionContext {
            state,
            rpc,
            user_state,
            user_id,
            event_tx,
            link: link.clone(),
            is_local: false,
            heartbeat,
            routing_role: RoutingRole::Host,
        };
        let _ = run_connection(RunConnection {
            transport,
            outgoing_rx,
            initial_messages,
            response_tx: route_handle.sender(),
            close_rx: route_handle.close_receiver(),
            ctx,
            token_refresh: None,
            span: conn_span,
        })
        .await;
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use prost::Message as ProstMessage;

    use super::*;
    use crate::protocol::wire;

    struct FakeTransport {
        reads: VecDeque<crate::transport::Result<Vec<u8>>>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(reads: Vec<crate::transport::Result<Vec<u8>>>) -> Self {
            Self {
                reads: reads.into(),
                writes: Vec::new(),
            }
        }
    }

    impl Transport for FakeTransport {
        async fn read_frame(&mut self) -> crate::transport::Result<Vec<u8>> {
            self.reads.pop_front().unwrap_or_else(|| {
                Err(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "fake transport exhausted",
                )))
            })
        }

        async fn write_frame(&mut self, data: &[u8]) -> crate::transport::Result<()> {
            self.writes.push(data.to_vec());
            Ok(())
        }

        async fn read_message(&mut self) -> crate::transport::Result<Message> {
            unreachable!("handshake tests use raw frames")
        }

        async fn write_message(&mut self, _msg: &Message) -> crate::transport::Result<()> {
            unreachable!("handshake tests use raw frames")
        }
    }

    fn connect_request(link_name: &str, versions: Vec<u32>) -> Vec<u8> {
        connect_request_with_host(
            link_name,
            versions,
            Some(local_host(Uuid::from_u128(77), "peer")),
        )
    }

    fn connect_request_with_host(
        link_name: &str,
        versions: Vec<u32>,
        host: Option<Host>,
    ) -> Vec<u8> {
        let routing_role = if host.is_some() {
            RoutingRole::Host
        } else {
            RoutingRole::Observer
        };
        connect_request_with_role(link_name, versions, host, routing_role)
    }

    fn connect_request_with_role(
        link_name: &str,
        versions: Vec<u32>,
        host: Option<Host>,
        routing_role: RoutingRole,
    ) -> Vec<u8> {
        Connect {
            link_name: link_name.to_string(),
            token: None,
            version: versions.first().copied().unwrap_or_default(),
            supported_versions: versions.clone(),
            host: host.clone(),
            routing_role,
        }
        .encode()
        .map(|bytes| {
            if versions == vec![PROTOCOL_VERSION] {
                bytes
            } else {
                wire::ConnectRequest {
                    supported_protocol_versions: versions,
                    proposed_link_name: link_name.to_string(),
                    auth_token: None,
                    host: host.as_ref().map(wire::host_to_wire),
                    routing_role: routing_role.to_wire(),
                }
                .encode_to_vec()
            }
        })
        .unwrap()
    }

    fn decode_connect_result(bytes: &[u8]) -> ConnectResult {
        ConnectResult::decode(bytes).expect("response should decode")
    }

    fn decode_connect_error(bytes: &[u8]) -> wire::Error {
        let response = wire::ConnectResponse::decode(bytes).expect("response should decode");
        let Some(wire::connect_response::Outcome::Error(error)) = response.outcome else {
            panic!("expected ConnectResponse.error");
        };
        error
    }

    #[tokio::test]
    async fn accept_handshake_reads_protobuf_request_and_writes_assigned_link() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "peer-link",
            vec![PROTOCOL_VERSION],
        ))]);

        let (
            link,
            _handle,
            _outgoing_rx,
            _user_id,
            _user_state,
            peer_host,
            remote_role,
            local_role,
        ) = accept_handshake(&mut transport, &state, false, false, Some(180))
            .await
            .unwrap();

        assert_eq!(link, Link::new("peer-link").unwrap());
        assert_eq!(
            peer_host.as_ref().map(|host| host.name.as_str()),
            Some("peer")
        );
        assert_eq!(
            peer_host.as_ref().map(|host| host.version.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(transport.writes.len(), 1);
        assert_eq!(remote_role, RoutingRole::Host);
        assert_eq!(local_role, RoutingRole::Host);

        let response = decode_connect_result(&transport.writes[0]);
        assert!(response.error.is_none());
        assert_eq!(response.idle_timeout_secs, Some(180));
        assert_eq!(response.assigned_link_name.as_deref(), Some("peer-link"));
        assert_eq!(response.routing_role, Some(RoutingRole::Host));
        let accepted_host = response.host.expect("acceptor host should be present");
        let state = state.read().await;
        assert_eq!(accepted_host.id, state.host_id());
        assert_eq!(accepted_host.name, state.host_name());
    }

    #[tokio::test]
    async fn accept_handshake_rejects_peer_connection_without_host() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request_with_role(
            "peer-link",
            vec![PROTOCOL_VERSION],
            None,
            RoutingRole::Host,
        ))]);

        let result = accept_handshake(&mut transport, &state, false, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::InvalidHandshake(_))));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::InvalidArgument {
                message: "non-local connections must send host identity".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn accept_handshake_rejects_peer_observer_without_host() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request_with_role(
            "observer-link",
            vec![PROTOCOL_VERSION],
            None,
            RoutingRole::Observer,
        ))]);

        let result = accept_handshake(&mut transport, &state, false, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::InvalidHandshake(_))));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::InvalidArgument {
                message: "non-local connections must send host identity".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn accept_handshake_accepts_peer_observer_with_host_info() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request_with_role(
            "observer-link",
            vec![PROTOCOL_VERSION],
            Some(local_host(Uuid::from_u128(88), "observer")),
            RoutingRole::Observer,
        ))]);

        let (
            _link,
            _handle,
            _outgoing_rx,
            _user_id,
            _user_state,
            peer_host,
            remote_role,
            _local_role,
        ) = accept_handshake(&mut transport, &state, false, false, Some(180))
            .await
            .unwrap();

        assert_eq!(
            peer_host.as_ref().map(|host| host.name.as_str()),
            Some("observer")
        );
        assert_eq!(remote_role, RoutingRole::Observer);
    }

    #[tokio::test]
    async fn cloud_accept_handshake_advertises_relay_with_host_info() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        state.write().await.is_cloud_server = true;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "host-link",
            vec![PROTOCOL_VERSION],
        ))]);

        let (
            _link,
            _handle,
            _outgoing_rx,
            _user_id,
            _user_state,
            peer_host,
            remote_role,
            local_role,
        ) = accept_handshake(&mut transport, &state, false, false, Some(180))
            .await
            .unwrap();

        assert!(peer_host.is_some());
        assert_eq!(remote_role, RoutingRole::Host);
        assert_eq!(local_role, RoutingRole::Relay);
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(response.routing_role, Some(RoutingRole::Relay));
        let accepted_host = response
            .host
            .expect("relay handshake should include host info");
        let state = state.read().await;
        assert_eq!(accepted_host.id, state.host_id());
        assert_eq!(accepted_host.name, state.host_name());
    }

    #[tokio::test]
    async fn relay_connection_starts_routing_subscription_without_direct_host_up() {
        let (state, user_state) = crate::server::test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (mut client, server) = crate::transport::memory::pair(16);
        let task = tokio::spawn(accept_connection(
            server,
            state.clone(),
            event_tx,
            false,
            false,
            "memory-test",
        ));

        client
            .write_frame(&connect_request_with_role(
                "relay-link",
                vec![PROTOCOL_VERSION],
                Some(local_host(Uuid::from_u128(88), "relay")),
                RoutingRole::Relay,
            ))
            .await
            .unwrap();

        let response = decode_connect_result(&client.read_frame().await.unwrap());
        assert_eq!(response.routing_role, Some(RoutingRole::Host));
        assert!(response.host.is_some());

        let initial = client.read_message().await.unwrap();
        let Message::Peer(PeerFrame {
            body: FrameBody::Request(request),
            ..
        }) = initial
        else {
            panic!("expected initial routing subscription request, got {initial:?}");
        };
        assert_eq!(request.method, method::ROUTING_SUBSCRIBE_EVENTS_NAME);

        let us = user_state.read().await;
        assert!(
            us.hosts.is_empty(),
            "relay role must not create a direct HostUp"
        );
        drop(us);
        drop(client);
        task.abort();
    }

    #[tokio::test]
    async fn accept_handshake_rejects_local_connection_with_host() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "local-link",
            vec![PROTOCOL_VERSION],
        ))]);

        let result = accept_handshake(&mut transport, &state, false, true, None).await;

        assert!(matches!(result, Err(AcceptError::InvalidHandshake(_))));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::InvalidArgument {
                message: "local connections must use observer routing role and omit host identity"
                    .to_string(),
            })
        );
    }

    #[tokio::test]
    async fn accept_handshake_rejects_peer_connection_with_own_host_id() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let own_host_id = state.read().await.host_id();
        let mut transport = FakeTransport::new(vec![Ok(connect_request_with_host(
            "peer-link",
            vec![PROTOCOL_VERSION],
            Some(local_host(own_host_id, "same-host")),
        ))]);

        let result = accept_handshake(&mut transport, &state, false, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::InvalidHandshake(_))));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::InvalidArgument {
                message: "peer host_id must not match local host_id".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn accept_handshake_assigns_suffixed_link_when_proposal_is_taken() {
        let (state, user_state) = crate::server::test_helpers::test_state().await;
        {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us
                .try_reserve_link(Link::new("peer-link").unwrap())
                .unwrap();
        }
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "peer-link",
            vec![PROTOCOL_VERSION],
        ))]);

        let (link, _handle, _outgoing_rx, ..) =
            accept_handshake(&mut transport, &state, false, false, Some(180))
                .await
                .unwrap();

        assert_ne!(link, Link::new("peer-link").unwrap());
        assert!(link.as_str().starts_with("peer-link-"));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(response.assigned_link_name.as_deref(), Some(link.as_str()));
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn accept_handshake_invalid_link_writes_protobuf_error() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "bad.link",
            vec![PROTOCOL_VERSION],
        ))]);

        let result = accept_handshake(&mut transport, &state, false, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::InvalidHandshake(_))));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::InvalidLinkName {
                name: "bad.link".to_string(),
                reason: "link name must not contain '.' (route separator): bad.link".to_string(),
            })
        );
        assert!(response.assigned_link_name.is_none());

        let error = decode_connect_error(&transport.writes[0]);
        assert_eq!(error.code, 2);
        assert_eq!(error.details.len(), 1);
        assert_eq!(error.details[0].r#type, "amux.v1.InvalidLinkName");
        let detail = wire::InvalidLinkName::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.name, "bad.link");
        assert!(detail.reason.contains("route separator"));
    }

    #[tokio::test]
    async fn accept_handshake_missing_token_writes_protobuf_error() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "peer-link",
            vec![PROTOCOL_VERSION],
        ))]);

        let result = accept_handshake(&mut transport, &state, true, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::InvalidCredentials)));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(response.error, Some(ProtocolError::InvalidCredentials));
        let error = decode_connect_error(&transport.writes[0]);
        assert_eq!(error.code, 6);
    }

    #[tokio::test]
    async fn accept_handshake_protocol_mismatch_writes_typed_protobuf_error() {
        let (state, _user_state) = crate::server::test_helpers::test_state().await;
        let mut transport = FakeTransport::new(vec![Ok(connect_request(
            "peer-link",
            vec![PROTOCOL_VERSION - 2, PROTOCOL_VERSION - 1],
        ))]);

        let result = accept_handshake(&mut transport, &state, false, false, Some(180)).await;

        assert!(matches!(result, Err(AcceptError::ProtocolMismatch { .. })));
        let response = decode_connect_result(&transport.writes[0]);
        assert_eq!(
            response.error,
            Some(ProtocolError::ProtocolMismatch {
                supported_versions: vec![PROTOCOL_VERSION],
                peer_supported_versions: vec![PROTOCOL_VERSION - 2, PROTOCOL_VERSION - 1],
            })
        );

        let error = decode_connect_error(&transport.writes[0]);
        assert_eq!(error.code, 7);
        assert_eq!(error.details.len(), 1);
        assert_eq!(error.details[0].r#type, "amux.v1.ProtocolVersionMismatch");
        let detail =
            wire::ProtocolVersionMismatch::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.supported_protocol_versions, vec![PROTOCOL_VERSION]);
        assert_eq!(
            detail.peer_supported_protocol_versions,
            vec![PROTOCOL_VERSION - 2, PROTOCOL_VERSION - 1]
        );
    }
}
