//! Per-connection message loop and stream lifecycle.
//!
//! Each connection runs a [`connection_loop`] that receives messages from the
//! reader task and dispatches them via [`handle_message`](super::handlers::handle_message).
//! Reader and writer tasks ([`reader_loop`], [`writer_loop`]) bridge the transport
//! to channels. Stream management ([`register_stream`], [`cleanup_stream`],
//! [`cancel_streams_matching`]) tracks active subscription streams per agent.

use super::handlers::handle_message;
use super::{ServerState, ServerUserState, StreamEntry};
use crate::agents::SessionEvent;
use crate::cloud::{CloudError, TokenRefreshState};
use crate::error::{AmuxError, Result};
use crate::message::{DirectMessage, Message};
use crate::route::Route;
use crate::transport::{MessageReader, TransportSplit};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::Instrument;
use uuid::Uuid;

/// Context for connection handlers.
pub(super) struct ConnectionContext {
    pub(super) state: Arc<RwLock<ServerState>>,
    pub(super) user_state: Arc<RwLock<ServerUserState>>,
    pub(super) user_id: Uuid,
    pub(super) event_tx: mpsc::Sender<SessionEvent>,
    pub(super) link_name: String,
    pub(super) is_local: bool,
    pub(super) next_request_id: Arc<AtomicU64>,
}

/// Typed enum for reader task output — avoids encoding transport errors as protocol messages.
pub(super) enum Incoming {
    Msg(Box<Message>),
    ReadErr(AmuxError),
    Eof,
}

/// Reader task: reads from transport, sends to channel. Never cancelled.
///
/// Decode errors (e.g. unknown message variant from a newer peer) are logged and
/// skipped rather than killing the connection. This is safe because the framing
/// layer reads the complete frame before decoding — a decode failure doesn't
/// corrupt the stream position. I/O errors remain fatal.
pub(super) async fn reader_loop<R: MessageReader>(mut reader: R, tx: mpsc::Sender<Incoming>) {
    loop {
        match reader.read_message().await {
            Ok(msg) => {
                if tx.send(Incoming::Msg(Box::new(msg))).await.is_err() {
                    break;
                }
            }
            Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                let _ = tx.send(Incoming::Eof).await;
                break;
            }
            Err(AmuxError::SerializationDecode(e)) => {
                // Unknown message variant or unrecognized field layout from a
                // newer peer. The frame was fully consumed by the framing layer,
                // so the stream position is correct for the next read. Skip this
                // message to keep the connection alive during rolling upgrades.
                tracing::debug!(error = %e, "skipping undecodable message");
            }
            Err(e) => {
                let _ = tx.send(Incoming::ReadErr(e)).await;
                break;
            }
        }
    }
}

/// Writer task: drains message channel, writes to transport.
/// Also handles transport-specific background I/O (e.g., WebSocket pong responses).
pub(super) async fn writer_loop<W: crate::transport::MessageWriter>(
    mut writer: W,
    mut rx: mpsc::Receiver<Message>,
) {
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(msg) => {
                        if writer.write_message(&msg).await.is_err() { break; }
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
pub(super) async fn run_connection<T: TransportSplit>(
    transport: T,
    outgoing_rx: mpsc::Receiver<Message>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    token_refresh: Option<TokenRefreshState>,
    span: tracing::Span,
) -> Result<()> {
    // Save fields needed for cleanup before ctx is consumed by connection_loop
    let user_state = ctx.user_state.clone();
    let link_name = ctx.link_name.clone();
    let is_local = ctx.is_local;

    let (reader, writer) = transport.into_split();
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle = tokio::spawn(reader_loop(reader, incoming_tx).instrument(span.clone()));
    let writer_handle = tokio::spawn(writer_loop(writer, outgoing_rx).instrument(span.clone()));

    let result = connection_loop(incoming_rx, response_tx, ctx, token_refresh)
        .instrument(span.clone())
        .await;

    if let Err(ref e) = result {
        tracing::warn!(parent: &span, error = %e, "connection error");
    }

    // Remove route and cancel streams so all sender clones are dropped,
    // allowing the writer task to drain remaining messages and exit.
    {
        let mut us = user_state.write().await;
        if !is_local {
            super::routing::handle_peer_disconnect(&mut us, &link_name);
        } else {
            us.routes.remove(&link_name);
            cancel_streams_matching(&mut us, |entry| entry.link == link_name);
        }
    }

    // Let writer drain, then abort reader
    let _ = writer_handle.await;
    reader_handle.abort();

    tracing::info!(parent: &span, "connection closed");

    result
}

const REFRESH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const HEARTBEAT_IDLE_INTERVAL: Duration = Duration::from_secs(60);
const HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct HeartbeatConfig {
    idle_interval: Duration,
    ack_timeout: Duration,
}

fn heartbeat_config_for_connection(is_local: bool) -> Option<HeartbeatConfig> {
    (!is_local).then_some(HeartbeatConfig {
        idle_interval: HEARTBEAT_IDLE_INTERVAL,
        ack_timeout: HEARTBEAT_ACK_TIMEOUT,
    })
}

/// Manages token refresh lifecycle within a connection loop.
///
/// Encapsulates the two-phase refresh protocol: wait for a deadline, send a
/// Reauth with a fresh token, then await the ReauthResult response (with a
/// timeout). The connection loop uses [`deadlines`](Self::deadlines) for
/// select! guards, [`send_refresh`](Self::send_refresh) to initiate, and
/// [`try_intercept`](Self::try_intercept) to consume ReauthResult responses.
struct TokenRefresher {
    inner: TokenRefreshState,
    deadline: tokio::time::Instant,
    awaiting_since: Option<tokio::time::Instant>,
}

impl TokenRefresher {
    fn new(state: TokenRefreshState) -> Self {
        let deadline = state.refresh_deadline();
        Self {
            inner: state,
            deadline,
            awaiting_since: None,
        }
    }

    /// Returns (refresh_deadline, refresh_timeout) for use in select! guards.
    ///
    /// When idle, returns `(Some(deadline), None)`.
    /// When awaiting a response, returns `(None, Some(timeout))`.
    fn deadlines(&self) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
        if let Some(since) = self.awaiting_since {
            (None, Some(since + REFRESH_RESPONSE_TIMEOUT))
        } else {
            (Some(self.deadline), None)
        }
    }

    fn is_awaiting_response(&self) -> bool {
        self.awaiting_since.is_some()
    }

    /// Send the token refresh request. Call when refresh_deadline fires.
    async fn send_refresh(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
        tracing::debug!("refreshing cloud token");
        self.inner
            .send_reauth(tx)
            .await
            .map_err(cloud_err_to_amux)?;
        self.awaiting_since = Some(tokio::time::Instant::now());
        Ok(())
    }

    /// Try to consume an incoming ReauthResult as a refresh response.
    /// Returns `true` if consumed, `false` if the message is not a ReauthResult.
    fn try_intercept(&mut self, msg: &Message) -> Result<bool> {
        if !matches!(msg, Message::Direct(DirectMessage::ReauthResult { .. })) {
            return Ok(false);
        }
        if self.awaiting_since.is_none() {
            tracing::warn!("unexpected ReauthResult");
            return Ok(true);
        }
        self.inner
            .handle_reauth_result(msg)
            .map_err(cloud_err_to_amux)?;
        self.deadline = self.inner.refresh_deadline();
        self.awaiting_since = None;
        Ok(true)
    }
}

/// Tracks idle heartbeats for a single peer connection.
struct HeartbeatState {
    config: HeartbeatConfig,
    last_rx_at: tokio::time::Instant,
    ack_deadline: Option<tokio::time::Instant>,
}

impl HeartbeatState {
    fn new(config: HeartbeatConfig) -> Self {
        Self {
            config,
            last_rx_at: tokio::time::Instant::now(),
            ack_deadline: None,
        }
    }

    /// Returns (idle_deadline, ack_deadline) for use in select! guards.
    fn deadlines(&self) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
        if let Some(deadline) = self.ack_deadline {
            (None, Some(deadline))
        } else {
            (Some(self.last_rx_at + self.config.idle_interval), None)
        }
    }

    /// Any inbound message proves the peer app loop is alive.
    fn note_activity(&mut self) {
        self.last_rx_at = tokio::time::Instant::now();
        self.ack_deadline = None;
    }

    /// Suspend heartbeat timers while waiting for an in-band refresh response.
    fn pause_for_refresh(&mut self) {
        self.last_rx_at = tokio::time::Instant::now();
        self.ack_deadline = None;
    }

    async fn send_heartbeat(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
        tracing::debug!("sending heartbeat");
        tx.send(Message::Direct(DirectMessage::Heartbeat))
            .await
            .map_err(|_| {
                AmuxError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "outgoing channel closed while sending heartbeat",
                ))
            })?;
        self.ack_deadline = Some(tokio::time::Instant::now() + self.config.ack_timeout);
        Ok(())
    }
}

fn heartbeat_deadlines(
    heartbeat: Option<&HeartbeatState>,
    refresh_has_priority: bool,
) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
    if refresh_has_priority {
        (None, None)
    } else {
        heartbeat.map(|h| h.deadlines()).unwrap_or((None, None))
    }
}

fn refresh_has_priority(
    refresh_deadline: Option<tokio::time::Instant>,
    refresh_awaiting_response: bool,
) -> bool {
    refresh_awaiting_response
        || refresh_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now())
}

fn cloud_err_to_amux(e: CloudError) -> AmuxError {
    match e {
        CloudError::HostChanged => {
            tracing::warn!("cloud host changed, reconnection required");
            AmuxError::Config(
                "cloud host changed — will reconnect to new host automatically".to_string(),
            )
        }
        other => {
            tracing::error!(error = %other, "token refresh failed");
            AmuxError::Config(format!("token refresh failed: {other}"))
        }
    }
}

/// Shared connection loop for all transports. Pure channel I/O — cancellation-safe.
pub(super) async fn connection_loop(
    incoming_rx: mpsc::Receiver<Incoming>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    token_refresh: Option<TokenRefreshState>,
) -> Result<()> {
    let heartbeat_config = heartbeat_config_for_connection(ctx.is_local);
    connection_loop_with_heartbeat(
        incoming_rx,
        response_tx,
        ctx,
        token_refresh,
        heartbeat_config,
    )
    .await
}

async fn connection_loop_with_heartbeat(
    mut incoming_rx: mpsc::Receiver<Incoming>,
    response_tx: mpsc::Sender<Message>,
    ctx: ConnectionContext,
    token_refresh: Option<TokenRefreshState>,
    heartbeat_config: Option<HeartbeatConfig>,
) -> Result<()> {
    let mut refresher = token_refresh.map(TokenRefresher::new);
    let mut heartbeat = heartbeat_config.map(HeartbeatState::new);

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
                .unwrap()
                .send_refresh(&response_tx)
                .await?;
            if let Some(ref mut heartbeat) = heartbeat {
                heartbeat.pause_for_refresh();
            }
            continue;
        }
        let (heartbeat_deadline, heartbeat_timeout) =
            heartbeat_deadlines(heartbeat.as_ref(), refresh_has_priority);

        tokio::select! {
            incoming = incoming_rx.recv() => {
                match incoming {
                    Some(Incoming::Msg(msg)) => {
                        if let Some(ref mut heartbeat) = heartbeat {
                            heartbeat.note_activity();
                        }
                        if let Some(ref mut r) = refresher
                            && r.try_intercept(&msg)?
                        {
                            continue;
                        }
                        handle_message(&response_tx, *msg, &ctx).await?;
                    }
                    Some(Incoming::Eof) | None => {
                        tracing::debug!("disconnected");
                        return Ok(());
                    }
                    Some(Incoming::ReadErr(e)) => {
                        return Err(e);
                    }
                }
            }
            _ = maybe_sleep_until(refresh_deadline), if refresh_deadline.is_some() => {
                refresher.as_mut().unwrap().send_refresh(&response_tx).await?;
                if let Some(ref mut heartbeat) = heartbeat {
                    heartbeat.pause_for_refresh();
                }
            }
            _ = maybe_sleep_until(refresh_timeout), if refresh_timeout.is_some() => {
                tracing::error!("token refresh response timeout");
                return Err(AmuxError::Config(format!(
                    "cloud token refresh timed out after {}s — the cloud server may be unresponsive",
                    REFRESH_RESPONSE_TIMEOUT.as_secs()
                )));
            }
            _ = maybe_sleep_until(heartbeat_deadline), if heartbeat_deadline.is_some() => {
                heartbeat.as_mut().unwrap().send_heartbeat(&response_tx).await?;
            }
            _ = maybe_sleep_until(heartbeat_timeout), if heartbeat_timeout.is_some() => {
                // If refresh and heartbeat timeout become ready in the same select!
                // wait, this branch may win and force a reconnect instead of in-band
                // reauth. We accept that tradeoff to keep this loop simpler.
                tracing::warn!("heartbeat ack timed out");
                return Err(AmuxError::HeartbeatTimeout);
            }
        }
    }
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Register a stream entry in active_streams. Returns the assigned stream_id.
pub(super) fn register_stream(
    us: &mut ServerUserState,
    agent_id: uuid::Uuid,
    cancel_tx: oneshot::Sender<()>,
    dst: Route,
    link: String,
) -> u64 {
    let sid = us.next_stream_id;
    us.next_stream_id += 1;
    us.active_streams
        .entry(agent_id)
        .or_default()
        .push(StreamEntry {
            stream_id: sid,
            cancel: cancel_tx,
            dst,
            link,
        });
    sid
}

/// Remove a stream entry by stream_id after the task exits.
pub(super) async fn cleanup_stream(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: uuid::Uuid,
    stream_id: u64,
) {
    let mut us = user_state.write().await;
    if let Some(entries) = us.active_streams.get_mut(&agent_id) {
        entries.retain(|e| e.stream_id != stream_id);
        if entries.is_empty() {
            us.active_streams.remove(&agent_id);
        }
    }
    tracing::trace!(stream_id, agent_id = %agent_id, "stream cleaned up");
}

/// Cancel all active streams matching a predicate. Returns count cancelled.
pub(super) fn cancel_streams_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&StreamEntry) -> bool,
) -> usize {
    let mut cancelled = 0usize;
    for entries in us.active_streams.values_mut() {
        entries.retain(|entry| {
            if predicate(entry) {
                cancelled += 1;
                false
            } else {
                true
            }
        });
    }
    us.active_streams.retain(|_, v| !v.is_empty());
    cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Command, DirectMessage, Message};
    use crate::server::LOCAL_USER_ID;
    use crate::server::test_helpers::{test_ctx, test_state};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    // --- Mock MessageReader for reader_loop tests ---

    /// A mock reader that yields a pre-configured sequence of results then EOF.
    struct MockReader {
        results: std::collections::VecDeque<crate::error::Result<Message>>,
    }

    impl MockReader {
        fn new(results: Vec<crate::error::Result<Message>>) -> Self {
            Self {
                results: results.into(),
            }
        }
    }

    impl crate::transport::MessageReader for MockReader {
        async fn read_message(&mut self) -> crate::error::Result<Message> {
            match self.results.pop_front() {
                Some(result) => result,
                None => Err(AmuxError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "mock reader exhausted",
                ))),
            }
        }
    }

    /// Drain all messages from an Incoming receiver without blocking.
    async fn drain_incoming(rx: &mut mpsc::Receiver<Incoming>) -> Vec<String> {
        let mut labels = Vec::new();
        while let Ok(item) = rx.try_recv() {
            labels.push(match item {
                Incoming::Msg(_) => "Msg".to_string(),
                Incoming::Eof => "Eof".to_string(),
                Incoming::ReadErr(e) => format!("ReadErr({e})"),
            });
        }
        labels
    }

    fn test_peer_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "test-peer".to_string(),
            is_local: false,
            next_request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    #[tokio::test]
    async fn reader_loop_forwards_messages_then_eof() {
        let reader = MockReader::new(vec![
            Ok(Message::Command(Command::ListAgents)),
            Ok(Message::Command(Command::Debug)),
            // MockReader auto-sends EOF when exhausted
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items, vec!["Msg", "Msg", "Eof"]);
    }

    #[tokio::test]
    async fn reader_loop_eof_sends_eof_variant() {
        let reader = MockReader::new(vec![Err(AmuxError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed",
        )))]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items, vec!["Eof"]);
    }

    #[tokio::test]
    async fn reader_loop_skips_decode_errors_and_continues() {
        // Simulate: good message → decode error → good message → EOF
        let decode_err = rmp_serde::decode::Error::Syntax("unknown variant".to_string());
        let reader = MockReader::new(vec![
            Ok(Message::Command(Command::ListAgents)),
            Err(AmuxError::SerializationDecode(decode_err)),
            Ok(Message::Command(Command::Debug)),
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        // Decode error should be skipped — two messages plus EOF
        let items = drain_incoming(&mut rx).await;
        assert_eq!(
            items,
            vec!["Msg", "Msg", "Eof"],
            "decode error should be skipped, not forwarded"
        );
    }

    #[tokio::test]
    async fn reader_loop_fatal_io_error_sends_read_err() {
        let reader = MockReader::new(vec![Err(AmuxError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        )))]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items.len(), 1);
        assert!(
            items[0].starts_with("ReadErr("),
            "fatal I/O error should produce ReadErr, got {:?}",
            items[0]
        );
    }

    #[tokio::test]
    async fn reader_loop_stops_when_receiver_dropped() {
        // If the receiver is dropped, reader_loop should exit on next send
        let reader = MockReader::new(vec![
            Ok(Message::Command(Command::ListAgents)),
            Ok(Message::Command(Command::ListAgents)),
            Ok(Message::Command(Command::ListAgents)),
        ]);
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        // Should not hang — exits because send fails
        reader_loop(reader, tx).await;
    }

    #[tokio::test]
    async fn connection_loop_eof_returns_ok() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok(), "EOF should return Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn connection_loop_channel_close_returns_ok() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        // Dropping the sender closes the channel — acts like EOF
        drop(incoming_tx);

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(
            result.is_ok(),
            "channel close should return Ok, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_read_error_propagates() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "peer reset");
        incoming_tx
            .send(Incoming::ReadErr(AmuxError::Io(io_err)))
            .await
            .unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_err(), "ReadErr should propagate as Err");
        assert!(
            matches!(result, Err(AmuxError::Io(_))),
            "should preserve Io error variant"
        );
    }

    #[tokio::test]
    async fn connection_loop_dispatches_command_and_returns_response() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        // Send a ListAgents command, then EOF to exit the loop
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Command(
                Command::ListAgents,
            ))))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // ListAgents should have produced a ListAgentsResult
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(msg, Message::Command(Command::ListAgentsResult { .. })),
            "expected ListAgentsResult, got {:?}",
            msg
        );
    }

    #[tokio::test]
    async fn connection_loop_skips_unexpected_reauth_result() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        // Send an unexpected ReauthResult (no refresh pending), then a real
        // command, then EOF. The ReauthResult should be skipped and the
        // command should still be dispatched.
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct(
                DirectMessage::ReauthResult { error: None },
            ))))
            .await
            .unwrap();
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Command(
                Command::ListAgents,
            ))))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // The ReauthResult should be skipped; only ListAgentsResult should appear
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(msg, Message::Command(Command::ListAgentsResult { .. })),
            "expected ListAgentsResult after skipped ReauthResult, got {:?}",
            msg
        );
    }

    #[tokio::test]
    async fn connection_loop_local_connections_do_not_send_heartbeats() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop(incoming_rx, response_tx, ctx, None));

        let recv_result = tokio::time::timeout(Duration::from_millis(40), response_rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "local connections should not emit idle heartbeats"
        );

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "local connection should exit cleanly");
    }

    #[tokio::test]
    async fn connection_loop_sends_heartbeat_after_idle_period() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(20),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(80), response_rx.recv())
            .await
            .expect("heartbeat should be sent before timeout")
            .expect("response channel should remain open");
        assert!(matches!(msg, Message::Direct(DirectMessage::Heartbeat)));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_times_out_when_heartbeat_unacked() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (_incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let result = connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(15),
                ack_timeout: Duration::from_millis(15),
            }),
        )
        .await;

        assert!(
            matches!(result, Err(AmuxError::HeartbeatTimeout)),
            "expected heartbeat timeout, got {:?}",
            result
        );
        let msg = response_rx
            .try_recv()
            .expect("heartbeat should have been sent");
        assert!(matches!(msg, Message::Direct(DirectMessage::Heartbeat)));
    }

    #[tokio::test]
    async fn connection_loop_inbound_message_clears_pending_heartbeat_ack() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(100), response_rx.recv())
            .await
            .expect("heartbeat should be sent before timeout")
            .expect("response channel should remain open");
        assert!(matches!(msg, Message::Direct(DirectMessage::Heartbeat)));

        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct(
                DirectMessage::HeartbeatAck,
            ))))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "inbound activity should clear the pending heartbeat timeout"
        );
        assert!(
            response_rx.try_recv().is_err(),
            "no second heartbeat should be sent before the next idle interval"
        );
    }

    #[tokio::test]
    async fn heartbeat_deadlines_are_suppressed_while_refresh_response_is_pending() {
        let heartbeat = HeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_rx_at: tokio::time::Instant::now(),
            ack_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        };

        let (heartbeat_deadline, heartbeat_timeout) = heartbeat_deadlines(Some(&heartbeat), true);

        assert!(
            heartbeat_deadline.is_none(),
            "idle heartbeats should be paused while awaiting ReauthResult"
        );
        assert!(
            heartbeat_timeout.is_none(),
            "pending heartbeat acks should not time out while awaiting ReauthResult"
        );
    }

    #[tokio::test]
    async fn refresh_due_now_takes_priority_over_heartbeat_timeouts() {
        let refresh_deadline = Some(tokio::time::Instant::now() - Duration::from_millis(1));

        assert!(
            refresh_has_priority(refresh_deadline, false),
            "a due refresh should preempt heartbeat timeout handling"
        );
    }

    #[tokio::test]
    async fn heartbeat_pause_for_refresh_clears_pending_ack() {
        let mut heartbeat = HeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_rx_at: tokio::time::Instant::now(),
            ack_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        };
        let previous_last_rx_at = heartbeat.last_rx_at;

        tokio::time::sleep(Duration::from_millis(2)).await;
        heartbeat.pause_for_refresh();

        assert!(
            heartbeat.ack_deadline.is_none(),
            "refresh start should clear any pending heartbeat ack timeout"
        );
        assert!(
            heartbeat.last_rx_at > previous_last_rx_at,
            "refresh start should reset the idle heartbeat timer"
        );
    }
}
