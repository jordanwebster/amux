use tokio::sync::mpsc;
use tracing::Instrument;

use super::context::{ConnectionContext, ConnectionError, Incoming, MessageMetadata, Result};
use super::heartbeat::{
    ConnectionActivity, HeartbeatState, heartbeat_deadlines, refresh_has_priority,
};
use super::reauth::{REFRESH_RESPONSE_TIMEOUT, TokenRefresher};
use super::subscription::cancel_subscriptions_matching;
use crate::auth::cloud::TokenRefreshState;
use crate::protocol::message::Message;
use crate::server::handlers::handle_message;
use crate::transport::{MessageReader, MessageWriter, TransportSplit};

/// Reader task: reads from transport, sends to channel. Never cancelled.
///
/// Decode errors on the top-level wire frame are logged and skipped rather than
/// killing the connection. This is safe because the framing layer reads the
/// complete frame before decoding, so an undecodable frame does not corrupt the
/// stream position. I/O errors remain fatal.
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
            Err(crate::transport::TransportError::SerializationDecode(e)) => {
                tracing::debug!(error = %e, "skipping undecodable message");
            }
            Err(e) => {
                let _ = tx.send(Incoming::TransportErr(e)).await;
                break;
            }
        }
    }
}

/// Writer task: drains message channel, writes to transport.
/// Also handles transport-specific background I/O (e.g., WebSocket pong responses).
pub(super) async fn writer_loop<W: MessageWriter>(
    mut writer: W,
    mut rx: mpsc::Receiver<Message>,
    tx: mpsc::Sender<Incoming>,
) {
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(msg) => {
                        match writer.write_message(&msg).await {
                            Ok(()) => {
                                let _ = tx
                                    .send(Incoming::Wrote(MessageMetadata::from_message(&msg)))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx.send(Incoming::TransportErr(e)).await;
                                break;
                            }
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
    transport: T,
    outgoing_rx: mpsc::Receiver<Message>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    token_refresh: Option<TokenRefreshState>,
    span: tracing::Span,
) -> Result<()> {
    let user_state = ctx.user_state.clone();
    let link = ctx.link.clone();
    let is_local = ctx.is_local;

    let (reader, writer) = transport.into_split();
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle =
        tokio::spawn(reader_loop(reader, incoming_tx.clone()).instrument(span.clone()));
    let writer_handle = tokio::spawn(
        writer_loop(writer, outgoing_rx, incoming_tx.clone()).instrument(span.clone()),
    );

    let result = connection_loop(incoming_rx, response_tx, ctx, token_refresh)
        .instrument(span.clone())
        .await;

    if let Err(ref e) = result {
        tracing::warn!(parent: &span, error = %e, "connection error");
    }

    {
        let mut us = user_state.write().await;
        if !is_local {
            crate::server::routing::handle_peer_disconnect(&mut us, &link);
        } else {
            us.routes.remove(&link);
            // Dropping the returned entries cancels their streams.
            let _cancelled = cancel_subscriptions_matching(
                &mut us,
                |entry| matches!(entry.dst.peek(), Some(hop) if *hop == link),
            );
        }
    }

    let _ = writer_handle.await;
    reader_handle.abort();

    tracing::info!(parent: &span, "connection closed");

    result
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
