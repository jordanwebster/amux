use tokio::sync::{mpsc, watch};
use tracing::Instrument;

use super::context::{ConnectionContext, ConnectionError, Incoming, MessageMetadata, Result};
use super::heartbeat::{
    ConnectionActivity, HeartbeatState, heartbeat_deadlines, refresh_has_priority,
};
use super::reauth::{REFRESH_RESPONSE_TIMEOUT, TokenRefresher};
use crate::auth::cloud::TokenRefreshState;
use crate::protocol::link::Link;
use crate::protocol::message::{
    FrameBody, Message, RoutedFrame, RoutedFrameMessage, ShutdownReason,
};
use crate::protocol::method;
use crate::protocol::route::Route;
use crate::server::dispatch::handle_message;
use crate::server::routing::{TopologyEffect, broadcast_topology_event};
use crate::server::{
    ConnectionHandle, ServerUserState, cancel_open_sessions_for_closed_link,
    cancel_open_sessions_for_owner_link, finish_open_session_cleanup_jobs,
};
use crate::transport::{MessageReader, MessageWriter, TransportSplit};

pub(in crate::server) struct RunConnection<T> {
    pub(in crate::server) transport: T,
    pub(in crate::server) outgoing_rx: mpsc::Receiver<Message>,
    pub(in crate::server) initial_messages: Vec<Message>,
    pub(in crate::server) response_tx: mpsc::Sender<Message>,
    pub(in crate::server) close_rx: watch::Receiver<Option<String>>,
    pub(in crate::server) ctx: ConnectionContext,
    pub(in crate::server) token_refresh: Option<TokenRefreshState>,
    pub(in crate::server) span: tracing::Span,
}

/// Reader task: reads from transport, sends to channel. Never cancelled.
///
/// Decode errors on the top-level protobuf frame are connection protocol
/// errors. The framing layer has already consumed the full frame, but after a
/// malformed `TransportMessage` the peer's protocol state is no longer
/// trustworthy, so the connection is closed.
pub(super) async fn reader_loop<R: MessageReader>(mut reader: R, tx: mpsc::Sender<Incoming>) {
    loop {
        match reader.read_message().await {
            Ok(msg) => {
                if tx.send(Incoming::Msg(Box::new(msg))).await.is_err() {
                    break;
                }
            }
            Err(crate::transport::TransportError::Io(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                let _ = tx.send(Incoming::Eof).await;
                break;
            }
            Err(e @ crate::transport::TransportError::ProtocolDecode(_)) => {
                let _ = tx.send(Incoming::TransportErr(e)).await;
                break;
            }
            Err(e) => {
                let _ = tx.send(Incoming::TransportErr(e)).await;
                break;
            }
        }
    }
}

async fn write_one<W: MessageWriter>(
    writer: &mut W,
    msg: &Message,
    tx: &mpsc::Sender<Incoming>,
) -> bool {
    match writer.write_message(msg).await {
        Ok(()) => {
            let _ = tx.try_send(Incoming::Wrote(MessageMetadata::from_message(msg)));
            true
        }
        Err(e) => {
            let _ = tx.send(Incoming::TransportErr(e)).await;
            false
        }
    }
}

async fn write_outgoing<W: MessageWriter>(
    writer: &mut W,
    msg: Message,
    tx: &mpsc::Sender<Incoming>,
) -> bool {
    match msg {
        Message::PeerSnapshot { messages } => {
            for msg in messages {
                if !write_one(writer, &msg, tx).await {
                    return false;
                }
            }
            true
        }
        msg => write_one(writer, &msg, tx).await,
    }
}

/// Writer task: drains message channel, writes to transport.
/// Also handles transport-specific background I/O (e.g., WebSocket pong responses).
pub(super) async fn writer_loop<W: MessageWriter>(
    mut writer: W,
    mut rx: mpsc::Receiver<Message>,
    initial_messages: Vec<Message>,
    tx: mpsc::Sender<Incoming>,
) {
    for msg in initial_messages {
        if !write_one(&mut writer, &msg, &tx).await {
            return;
        }
    }

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(msg) => {
                        if !write_outgoing(&mut writer, msg, &tx).await {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = writer.background() => {}
        }
    }
}

/// Run a connection through its full lifecycle: split the transport into reader
/// and writer tasks, run the connection loop, and shut down gracefully.
///
/// Handles the split → spawn → loop → cleanup → shutdown pattern that is common
/// to all connection types (Unix, TCP, cloud). The caller sets up routes and
/// peer state before calling this function. On exit, the route is removed,
/// stream tasks are cancelled, and the writer task is allowed to drain.
pub(in crate::server) async fn run_connection<T: TransportSplit>(
    args: RunConnection<T>,
) -> Result<()> {
    let RunConnection {
        transport,
        outgoing_rx,
        initial_messages,
        response_tx,
        mut close_rx,
        ctx,
        token_refresh,
        span,
    } = args;
    let user_state = ctx.user_state.clone();
    let link = ctx.link.clone();
    let is_local = ctx.is_local;

    let (reader, writer) = transport.into_split();
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle =
        tokio::spawn(reader_loop(reader, incoming_tx.clone()).instrument(span.clone()));
    let writer_handle = tokio::spawn(
        writer_loop(writer, outgoing_rx, initial_messages, incoming_tx.clone())
            .instrument(span.clone()),
    );

    let result = tokio::select! {
        result = connection_loop(incoming_rx, response_tx, ctx, token_refresh).instrument(span.clone()) => result,
        changed = close_rx.changed() => {
            let reason = match changed {
                Ok(()) => close_rx
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "connection close requested".to_string()),
                Err(_) => "connection handle closed".to_string(),
            };
            Err(ConnectionError::Protocol(reason))
        }
    };

    if let Err(ref e) = result {
        tracing::warn!(parent: &span, error = %e, "connection error");
    }

    let (open_session_cleanup_jobs, local_origin_messages) = {
        let mut us = user_state.write().await;
        if !is_local {
            tracing::info!(peer = %link, "peer disconnected");
            let change = us.topology.apply_link_closed(&link);
            us.remove_peer_routing_calls_for_link(&link);

            let TopologyEffect::CancelSessionsForClosedLink { link: closed_link } = &change.effect
            else {
                unreachable!("link-close topology change returned non-link-close effect");
            };
            let local_origin_messages = drain_local_origin_routed_unreachable_for_route(
                &mut us,
                &Route::from_link(closed_link.clone()),
                "route closed",
            );
            let (cancelled_open_sessions, cleanup_jobs) =
                cancel_open_sessions_for_closed_link(&mut us, closed_link);
            let removed_inbound_rpc_calls =
                remove_generic_inbound_rpc_calls_for_owner_link(&mut us, closed_link);
            if cancelled_open_sessions != 0 {
                tracing::info!(
                    count = cancelled_open_sessions,
                    peer = %link,
                    "cancelled open sessions for disconnected peer"
                );
            }
            if removed_inbound_rpc_calls != 0 {
                tracing::info!(
                    count = removed_inbound_rpc_calls,
                    peer = %link,
                    "removed inbound RPC calls for disconnected peer"
                );
            }
            if change.removed_agents != 0 {
                tracing::info!(count = change.removed_agents, peer = %link, "removed agents for disconnected peer");
            }
            if change.removed_hosts != 0 {
                tracing::info!(count = change.removed_hosts, peer = %link, "removed hosts for disconnected peer");
            }
            for event in change.events {
                tracing::info!(peer = %link, event = event.to_routing_event().type_label(), "withdrawing topology event");
                broadcast_topology_event(&mut us, &event, None);
            }
            (cleanup_jobs, local_origin_messages)
        } else {
            let local_origin_messages = drain_local_origin_routed_cancels(&mut us, &link);
            us.topology.remove_link(&link);
            let (_cancelled_open_sessions, cleanup_jobs) =
                cancel_open_sessions_for_owner_link(&mut us, &link);
            remove_generic_inbound_rpc_calls_for_owner_link(&mut us, &link);
            (cleanup_jobs, local_origin_messages)
        }
    };
    for (handle, message) in local_origin_messages {
        let _ = handle.send(message).await;
    }
    finish_open_session_cleanup_jobs(&user_state, open_session_cleanup_jobs).await;

    let (rpc_inbound_calls, rpc_outbound_calls, rpc_inbound_dedup_keys, rpc_snapshot) = {
        let us = user_state.read().await;
        (
            us.rpc.inbound_len(),
            us.rpc.outbound_len(),
            us.rpc.dedup_len(),
            us.rpc.debug_snapshot(),
        )
    };
    tracing::debug!(
        parent: &span,
        rpc_inbound_calls,
        rpc_outbound_calls,
        rpc_inbound_dedup_keys,
        rpc = ?rpc_snapshot,
        "user RPC state after connection cleanup"
    );

    let _ = writer_handle.await;
    reader_handle.abort();

    tracing::info!(parent: &span, "connection closed");

    result
}

fn drain_local_origin_routed_cancels(
    us: &mut ServerUserState,
    owner_link: &Link,
) -> Vec<(ConnectionHandle, Message)> {
    let cancel_payload = match crate::protocol::wire::encode_frame_body(&FrameBody::Cancel) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(error = %error, "failed to encode local-origin routed cancel");
            return Vec::new();
        }
    };
    let calls = us
        .rpc
        .remove_local_origin_outbound_for_owner_link(owner_link);

    calls
        .into_iter()
        .filter_map(|call| {
            let mut dst = call.request_dst;
            let next_hop = dst.pop()?;
            let handle = us.topology.route(&next_hop)?;
            let mut src = call.request_src;
            src.push(next_hop);
            Some((
                handle,
                Message::Routed(RoutedFrame {
                    src,
                    dst,
                    call_id: call.call_id,
                    message: RoutedFrameMessage::Payload(cancel_payload.clone()),
                }),
            ))
        })
        .collect()
}

fn remove_generic_inbound_rpc_calls_for_owner_link(
    us: &mut ServerUserState,
    owner_link: &Link,
) -> usize {
    us.rpc
        .remove_inbound_for_owner_link_except_method(owner_link, method::AGENT_OPEN_SESSION)
        .len()
}

pub(in crate::server) fn drain_local_origin_routed_unreachable_for_route(
    us: &mut ServerUserState,
    route_prefix: &Route,
    reason: &str,
) -> Vec<(ConnectionHandle, Message)> {
    let calls = us
        .rpc
        .remove_local_origin_outbound_for_route_prefix(route_prefix);

    calls
        .into_iter()
        .filter_map(|call| {
            let handle = us.topology.route(&call.owner_link)?;
            Some((
                handle,
                Message::routing_error_for_route(
                    Route::from_link(call.owner_link),
                    Route::empty(),
                    call.call_id,
                    call.counterparty_route,
                    crate::protocol::message::ProtocolError::Unreachable {
                        message: format!("{reason}: {route_prefix}"),
                    },
                ),
            ))
        })
        .collect()
}

/// Shared connection loop for all transports. Pure channel I/O — cancellation-safe.
pub(super) async fn connection_loop(
    mut incoming_rx: mpsc::Receiver<Incoming>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    token_refresh: Option<TokenRefreshState>,
) -> Result<()> {
    let mut refresher = token_refresh.map(TokenRefresher::new);
    let mut activity = ConnectionActivity::new();
    let mut heartbeat = ctx.heartbeat.map(HeartbeatState::new);

    loop {
        let (refresh_deadline, refresh_timeout) = refresher
            .as_ref()
            .map(|r| r.deadlines())
            .unwrap_or((None, None));
        let refresh_awaiting_response = matches!(
            refresher.as_ref(),
            Some(refresher) if refresher.is_awaiting_response()
        );
        let refresh_has_priority =
            refresh_has_priority(refresh_deadline, refresh_awaiting_response);
        if refresh_has_priority && !refresh_awaiting_response {
            refresher
                .as_mut()
                .expect("refresher present")
                .send_refresh(&response_tx)
                .await?;
            continue;
        }
        let (heartbeat_deadline, heartbeat_timeout) =
            heartbeat_deadlines(heartbeat.as_ref(), &activity, refresh_has_priority);

        tokio::select! {
            incoming = incoming_rx.recv() => {
                match incoming {
                    Some(Incoming::Msg(msg)) => {
                        activity.note_inbound();
                        if let Some(ref mut r) = refresher
                            && r.try_intercept(&msg)?
                        {
                            continue;
                        }
                        handle_message(&response_tx, *msg, &ctx).await?;
                    }
                    Some(Incoming::Wrote(meta)) => {
                        activity.note_outbound();
                        if let Some(ref mut heartbeat) = heartbeat {
                            heartbeat.note_outbound_write(meta);
                        }
                    }
                    Some(Incoming::Eof) | None => {
                        log_connection_state(
                            "disconnected",
                            &activity,
                            heartbeat.as_ref(),
                            refresher.as_ref(),
                        );
                        return Ok(());
                    }
                    Some(Incoming::TransportErr(e)) => {
                        log_connection_state(
                            "transport error",
                            &activity,
                            heartbeat.as_ref(),
                            refresher.as_ref(),
                        );
                        if matches!(e, crate::transport::TransportError::ProtocolDecode(_)) {
                            send_protocol_error_goaway(&response_tx);
                        }
                        return Err(e.into());
                    }
                }
            }
            _ = maybe_sleep_until(refresh_deadline), if refresh_deadline.is_some() => {
                refresher
                    .as_mut()
                    .expect("refresher present")
                    .send_refresh(&response_tx)
                    .await?;
            }
            _ = maybe_sleep_until(refresh_timeout), if refresh_timeout.is_some() => {
                tracing::error!("token refresh response timeout");
                return Err(ConnectionError::Config(format!(
                    "cloud token refresh timed out after {}s — the cloud server may be unresponsive",
                    REFRESH_RESPONSE_TIMEOUT.as_secs()
                )));
            }
            _ = maybe_sleep_until(heartbeat_deadline), if heartbeat_deadline.is_some() => {
                heartbeat
                    .as_mut()
                    .expect("heartbeat present")
                    .queue_heartbeat(&response_tx)
                    .await?;
            }
            _ = maybe_sleep_until(heartbeat_timeout), if heartbeat_timeout.is_some() => {
                log_connection_state_for_heartbeat_timeout(
                    &activity,
                    heartbeat.as_ref(),
                    refresher.as_ref(),
                );
                return Err(ConnectionError::HeartbeatTimeout);
            }
        }
    }
}

fn send_protocol_error_goaway(tx: &mpsc::Sender<Message>) {
    let _ = tx.try_send(Message::GoAway(crate::protocol::message::GoAway {
        reason: ShutdownReason::ProtocolError,
    }));
}

fn log_connection_state(
    event: &'static str,
    activity: &ConnectionActivity,
    heartbeat: Option<&HeartbeatState>,
    refresher: Option<&TokenRefresher>,
) {
    let now = tokio::time::Instant::now();
    let (refresh_deadline, _) = refresher
        .as_ref()
        .map(|r| r.deadlines())
        .unwrap_or((None, None));
    let refresh_awaiting_response = matches!(
        refresher,
        Some(refresher) if refresher.is_awaiting_response()
    );
    let heartbeat_role = heartbeat.map(|h| h.role().as_str()).unwrap_or("disabled");
    tracing::debug!(
        heartbeat_role,
        event,
        time_since_last_inbound = ?now.duration_since(activity.last_inbound_at),
        time_since_last_outbound = ?now.duration_since(activity.last_outbound_at),
        token_refresh_suppressed = refresh_has_priority(refresh_deadline, refresh_awaiting_response),
        "connection state"
    );
}

fn log_connection_state_for_heartbeat_timeout(
    activity: &ConnectionActivity,
    heartbeat: Option<&HeartbeatState>,
    refresher: Option<&TokenRefresher>,
) {
    let now = tokio::time::Instant::now();
    let (refresh_deadline, _) = refresher
        .as_ref()
        .map(|r| r.deadlines())
        .unwrap_or((None, None));
    let refresh_awaiting_response = matches!(
        refresher,
        Some(refresher) if refresher.is_awaiting_response()
    );
    let heartbeat = heartbeat.expect("heartbeat timeout requires heartbeat state");
    tracing::warn!(
        heartbeat_role = heartbeat.role().as_str(),
        time_since_last_inbound = ?now.duration_since(activity.last_inbound_at),
        time_since_last_outbound = ?now.duration_since(activity.last_outbound_at),
        token_refresh_suppressed = refresh_has_priority(refresh_deadline, refresh_awaiting_response),
        "peer idle timeout exceeded"
    );
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::message::RoutedCallId;
    use crate::protocol::method::MethodKind;
    use crate::rpc::{
        OutboundCallState, RpcLocalOriginOutboundStart, RpcRoutedBidiStart, RpcRoutedUnaryStart,
    };

    fn call_id(n: u128) -> RoutedCallId {
        RoutedCallId::from(Uuid::from_u128(n))
    }

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn route_stack(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn register_call(
        us: &mut ServerUserState,
        owner_link: &str,
        call_id: RoutedCallId,
        method: crate::protocol::method::MethodSpec,
    ) {
        let (tx, _rx) = mpsc::channel(1);
        match method.kind {
            MethodKind::Unary => {
                us.rpc
                    .register_routed_unary(RpcRoutedUnaryStart {
                        tx,
                        owner_link: Link::new(owner_link).unwrap(),
                        reply_src: route("server"),
                        reply_dst: route("client"),
                        counterparty_route: route("client"),
                        call_id,
                        method,
                    })
                    .unwrap();
            }
            MethodKind::ServerStreaming | MethodKind::BidiStreaming => {
                us.rpc
                    .register_routed_bidi(RpcRoutedBidiStart {
                        tx,
                        owner_link: Link::new(owner_link).unwrap(),
                        reply_src: route("server"),
                        reply_dst: route("client"),
                        counterparty_route: route("client"),
                        call_id,
                        method,
                        dedup_key: None,
                        stream_capacity: 1,
                    })
                    .unwrap();
            }
        }
    }

    fn register_local_origin_call(
        us: &mut ServerUserState,
        owner_link: &str,
        request_dst: Route,
        call_id: RoutedCallId,
        method: crate::protocol::method::MethodSpec,
    ) -> Route {
        let owner_link = Link::new(owner_link).unwrap();
        let request_src = Route::from_link(owner_link.clone());
        let counterparty_route = Route::from_links(
            request_src
                .iter()
                .chain(request_dst.iter())
                .map(|link| link.as_str().to_string()),
        )
        .unwrap();
        us.rpc
            .register_local_origin_outbound(RpcLocalOriginOutboundStart {
                call_id,
                counterparty_route: counterparty_route.clone(),
                method,
                state: OutboundCallState::AwaitingResponse,
                owner_link,
                request_src,
                request_dst,
            })
            .unwrap();
        counterparty_route
    }

    #[test]
    fn owner_link_cleanup_removes_generic_inbound_calls_only() {
        let mut us = ServerUserState::new();
        let owner = Link::new("owner").unwrap();
        let generic_call_id = call_id(1);
        let open_session_call_id = call_id(2);
        let other_owner_call_id = call_id(3);

        register_call(
            &mut us,
            "owner",
            generic_call_id.clone(),
            method::AGENT_CREATE,
        );
        register_call(
            &mut us,
            "owner",
            open_session_call_id.clone(),
            method::AGENT_OPEN_SESSION,
        );
        register_call(
            &mut us,
            "other",
            other_owner_call_id.clone(),
            method::AGENT_CREATE,
        );

        assert_eq!(
            remove_generic_inbound_rpc_calls_for_owner_link(&mut us, &owner),
            1
        );

        assert!(
            us.rpc
                .inbound_for_route(&route("client"), &generic_call_id)
                .is_none()
        );
        assert!(
            us.rpc
                .inbound_for_route(&route("client"), &open_session_call_id)
                .is_some()
        );
        assert!(
            us.rpc
                .inbound_for_route(&route("client"), &other_owner_call_id)
                .is_some()
        );
    }

    #[test]
    fn local_owner_cleanup_cancels_generic_local_origin_routed_calls() {
        let mut us = ServerUserState::new();
        let owner = Link::new("local").unwrap();
        let (_peer_handle, _peer_rx) = us
            .topology
            .try_reserve_link(Link::new("peer").unwrap())
            .unwrap();
        let generic_call_id = call_id(10);
        let other_owner_call_id = call_id(11);

        let generic_route = register_local_origin_call(
            &mut us,
            "local",
            route("peer"),
            generic_call_id.clone(),
            method::AGENT_CREATE,
        );
        let other_owner_route = register_local_origin_call(
            &mut us,
            "other",
            route("peer"),
            other_owner_call_id.clone(),
            method::AGENT_CREATE,
        );

        let messages = drain_local_origin_routed_cancels(&mut us, &owner);

        assert_eq!(messages.len(), 1);
        assert!(
            us.rpc
                .outbound_for_route(&generic_route, &generic_call_id)
                .is_none()
        );
        assert!(
            us.rpc
                .outbound_for_route(&other_owner_route, &other_owner_call_id)
                .is_some()
        );

        let (_handle, message) = messages.into_iter().next().unwrap();
        let Message::Routed(frame) = message else {
            panic!("expected routed cancel");
        };
        assert_eq!(frame.src, route_stack(&["peer", "local"]));
        assert_eq!(frame.dst, Route::empty());
        assert_eq!(frame.call_id, generic_call_id);
        let RoutedFrameMessage::Payload(payload) = frame.message else {
            panic!("expected cancel payload");
        };
        assert_eq!(
            crate::protocol::wire::decode_frame_body(&payload).unwrap(),
            FrameBody::Cancel
        );
    }

    #[test]
    fn route_loss_sends_unreachable_for_generic_local_origin_routed_calls() {
        let mut us = ServerUserState::new();
        let owner = Link::new("local").unwrap();
        let (_owner_handle, _owner_rx) = us.topology.try_reserve_link(owner.clone()).unwrap();
        let generic_call_id = call_id(20);
        let other_route_call_id = call_id(21);

        let generic_route = register_local_origin_call(
            &mut us,
            "local",
            route_stack(&["peer", "remote"]),
            generic_call_id.clone(),
            method::AGENT_CREATE,
        );
        let other_route = register_local_origin_call(
            &mut us,
            "local",
            route("other-peer"),
            other_route_call_id.clone(),
            method::AGENT_CREATE,
        );

        let messages = drain_local_origin_routed_unreachable_for_route(
            &mut us,
            &route("peer"),
            "route withdrawn",
        );

        assert_eq!(messages.len(), 1);
        assert!(
            us.rpc
                .outbound_for_route(&generic_route, &generic_call_id)
                .is_none()
        );
        assert!(
            us.rpc
                .outbound_for_route(&other_route, &other_route_call_id)
                .is_some()
        );

        let (_handle, message) = messages.into_iter().next().unwrap();
        let Message::Routed(frame) = message else {
            panic!("expected routed routing error");
        };
        assert_eq!(frame.src, route("local"));
        assert_eq!(frame.dst, Route::empty());
        assert_eq!(frame.call_id, generic_call_id);
        let RoutedFrameMessage::RoutingError {
            failed_route,
            error,
        } = frame.message
        else {
            panic!("expected routing error");
        };
        assert_eq!(failed_route, generic_route);
        assert!(matches!(
            error,
            crate::protocol::message::ProtocolError::Unreachable { .. }
        ));
    }
}
