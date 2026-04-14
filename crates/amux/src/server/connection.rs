//! Per-connection message loop and stream lifecycle.
//!
//! Each connection runs a [`connection_loop`] that receives messages from the
//! reader task and dispatches them via [`handle_message`](super::handlers::handle_message).
//! Reader and writer tasks ([`reader_loop`], [`writer_loop`]) bridge the transport
//! to channels. Subscription management ([`register_subscription`],
//! [`cleanup_subscription`], [`cancel_subscriptions_matching`]) tracks active
//! subscriptions owned by this server.

use super::handlers::handle_message;
use super::{ServerState, ServerUserState, SubscriptionEntry, SubscriptionMode};
use crate::agents::SessionEvent;
use crate::cloud::{CloudError, TokenRefreshState};
use crate::error::{AmuxError, Result};
use crate::message::{DirectMessage, Message, SubscriptionId};
use crate::route::Route;
use crate::transport::{MessageReader, TransportSplit};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::Instant;
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
    pub(super) heartbeat_role: HeartbeatRole,
    pub(super) next_request_id: Arc<AtomicU64>,
    /// Client implementation name (from Connect handshake, e.g. "amux-cli").
    pub(super) client_name: Option<String>,
    /// Semantic version of the connecting client (from Connect handshake).
    pub(super) client_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HeartbeatRole {
    Disabled,
    Dialer,
    Acceptor,
}

impl HeartbeatRole {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Dialer => "dialer",
            Self::Acceptor => "acceptor",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MessageMetadata {
    pub(super) is_heartbeat: bool,
}

impl MessageMetadata {
    fn from_message(msg: &Message) -> Self {
        Self {
            is_heartbeat: matches!(
                msg,
                Message::Direct {
                    message: DirectMessage::Heartbeat
                }
            ),
        }
    }
}

/// Typed enum for connection-loop input from the reader/writer tasks.
pub(super) enum Incoming {
    Msg(Box<Message>),
    Wrote(MessageMetadata),
    TransportErr(AmuxError),
    Eof,
}

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
            Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                let _ = tx.send(Incoming::Eof).await;
                break;
            }
            Err(AmuxError::SerializationDecode(e)) => {
                // The frame was fully consumed by the framing layer, so the
                // stream position is correct for the next read. Skip undecodable
                // top-level frames to keep the connection alive.
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
pub(super) async fn writer_loop<W: crate::transport::MessageWriter>(
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
                                let _ = tx.send(Incoming::Wrote(MessageMetadata::from_message(&msg))).await;
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

    // Remove route and cancel streams so all sender clones are dropped,
    // allowing the writer task to drain remaining messages and exit.
    {
        let mut us = user_state.write().await;
        if !is_local {
            super::routing::handle_peer_disconnect(&mut us, &link_name);
        } else {
            us.routes.remove(&link_name);
            drop(cancel_subscriptions_matching(
                &mut us,
                |entry| matches!(entry.dst.peek(), Some(link) if link == link_name),
            ));
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

fn heartbeat_config_for_role(role: HeartbeatRole) -> Option<HeartbeatConfig> {
    (!matches!(role, HeartbeatRole::Disabled)).then_some(HeartbeatConfig {
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
        if !matches!(
            msg,
            Message::Direct {
                message: DirectMessage::ReauthResult { .. }
            }
        ) {
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

#[derive(Clone, Copy)]
struct ConnectionActivity {
    last_inbound_at: tokio::time::Instant,
    last_outbound_at: tokio::time::Instant,
}

impl ConnectionActivity {
    fn new() -> Self {
        let now = tokio::time::Instant::now();
        Self {
            last_inbound_at: now,
            last_outbound_at: now,
        }
    }

    fn note_inbound(&mut self) {
        self.last_inbound_at = tokio::time::Instant::now();
    }

    fn note_outbound(&mut self) {
        self.last_outbound_at = tokio::time::Instant::now();
    }
}

struct DialerHeartbeatState {
    config: HeartbeatConfig,
    last_tx_at: tokio::time::Instant,
    probe_deadline: Option<tokio::time::Instant>,
}

struct AcceptorHeartbeatState {
    config: HeartbeatConfig,
    last_rx_at: tokio::time::Instant,
}

/// Tracks heartbeat ownership for a single peer connection.
enum HeartbeatState {
    Dialer(DialerHeartbeatState),
    Acceptor(AcceptorHeartbeatState),
}

impl HeartbeatState {
    fn new(role: HeartbeatRole, config: HeartbeatConfig) -> Option<Self> {
        let now = tokio::time::Instant::now();
        match role {
            HeartbeatRole::Disabled => None,
            HeartbeatRole::Dialer => Some(Self::Dialer(DialerHeartbeatState {
                config,
                last_tx_at: now,
                probe_deadline: None,
            })),
            HeartbeatRole::Acceptor => Some(Self::Acceptor(AcceptorHeartbeatState {
                config,
                last_rx_at: now,
            })),
        }
    }

    /// Returns (idle_deadline, probe_deadline) for use in select! guards.
    fn deadlines(&self) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
        match self {
            Self::Dialer(state) => {
                if let Some(deadline) = state.probe_deadline {
                    (None, Some(deadline))
                } else {
                    (Some(state.last_tx_at + state.config.idle_interval), None)
                }
            }
            Self::Acceptor(state) => (
                None,
                Some(state.last_rx_at + state.config.idle_interval + state.config.ack_timeout),
            ),
        }
    }

    /// Any inbound message proves the peer app loop is alive.
    fn note_inbound_activity(&mut self) {
        match self {
            Self::Dialer(state) => {
                state.probe_deadline = None;
            }
            Self::Acceptor(state) => {
                state.last_rx_at = tokio::time::Instant::now();
            }
        }
    }

    /// Successful non-heartbeat outbound writes reset the dialer's idle-send
    /// timer. Heartbeat writes are timed from queue_heartbeat(), so a late
    /// write callback must not move the idle timer.
    fn note_outbound_write(&mut self, meta: MessageMetadata) {
        if let Self::Dialer(state) = self
            && !meta.is_heartbeat
        {
            state.last_tx_at = tokio::time::Instant::now();
        }
    }

    /// Suspend heartbeat timers while waiting for an in-band refresh response.
    fn pause_for_refresh(&mut self) {
        if let Self::Dialer(state) = self {
            state.probe_deadline = None;
        }
    }

    async fn queue_heartbeat(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
        match self {
            Self::Dialer(state) => {
                let now = tokio::time::Instant::now();
                tracing::debug!(role = HeartbeatRole::Dialer.as_str(), "sending heartbeat");
                state.last_tx_at = now;
                state.probe_deadline = Some(now + state.config.ack_timeout);
                tx.send(Message::Direct {
                    message: DirectMessage::Heartbeat,
                })
                .await
                .map_err(|_| {
                    AmuxError::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "outgoing channel closed while sending heartbeat",
                    ))
                })?;
                Ok(())
            }
            Self::Acceptor(_) => unreachable!("acceptors never initiate heartbeats"),
        }
    }

    fn role(&self) -> HeartbeatRole {
        match self {
            Self::Dialer(_) => HeartbeatRole::Dialer,
            Self::Acceptor(_) => HeartbeatRole::Acceptor,
        }
    }

    fn ack_pending(&self) -> bool {
        matches!(self, Self::Dialer(state) if state.probe_deadline.is_some())
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
    let heartbeat_config = heartbeat_config_for_role(ctx.heartbeat_role);
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
    let mut activity = ConnectionActivity::new();
    let mut heartbeat =
        heartbeat_config.and_then(|config| HeartbeatState::new(ctx.heartbeat_role, config));

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
                        activity.note_inbound();
                        if let Some(ref mut heartbeat) = heartbeat {
                            heartbeat.note_inbound_activity();
                        }
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
                            ctx.heartbeat_role,
                            &activity,
                            heartbeat.as_ref(),
                            refresher.as_ref(),
                        );
                        return Ok(());
                    }
                    Some(Incoming::TransportErr(e)) => {
                        log_connection_state(
                            "transport error",
                            ctx.heartbeat_role,
                            &activity,
                            heartbeat.as_ref(),
                            refresher.as_ref(),
                        );
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
                heartbeat.as_mut().unwrap().queue_heartbeat(&response_tx).await?;
            }
            _ = maybe_sleep_until(heartbeat_timeout), if heartbeat_timeout.is_some() => {
                // If refresh and heartbeat timeout become ready in the same select!
                // wait, this branch may win and force a reconnect instead of in-band
                // reauth. We accept that tradeoff to keep this loop simpler.
                log_connection_state_for_heartbeat_timeout(
                    &activity,
                    heartbeat.as_ref(),
                    refresher.as_ref(),
                );
                return Err(AmuxError::HeartbeatTimeout);
            }
        }
    }
}

fn log_connection_state(
    event: &'static str,
    heartbeat_role: HeartbeatRole,
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
    tracing::debug!(
        heartbeat_role = heartbeat_role.as_str(),
        event,
        time_since_last_inbound = ?now.duration_since(activity.last_inbound_at),
        time_since_last_outbound = ?now.duration_since(activity.last_outbound_at),
        heartbeat_ack_pending = heartbeat.is_some_and(HeartbeatState::ack_pending),
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
    match heartbeat.role() {
        HeartbeatRole::Dialer => {
            tracing::warn!(
                heartbeat_role = HeartbeatRole::Dialer.as_str(),
                time_since_last_inbound = ?now.duration_since(activity.last_inbound_at),
                time_since_last_outbound = ?now.duration_since(activity.last_outbound_at),
                heartbeat_ack_pending = heartbeat.ack_pending(),
                token_refresh_suppressed = refresh_has_priority(refresh_deadline, refresh_awaiting_response),
                "heartbeat ack timed out"
            );
        }
        HeartbeatRole::Acceptor => {
            tracing::warn!(
                heartbeat_role = HeartbeatRole::Acceptor.as_str(),
                time_since_last_inbound = ?now.duration_since(activity.last_inbound_at),
                time_since_last_outbound = ?now.duration_since(activity.last_outbound_at),
                heartbeat_ack_pending = heartbeat.ack_pending(),
                token_refresh_suppressed = refresh_has_priority(refresh_deadline, refresh_awaiting_response),
                "peer heartbeat overdue"
            );
        }
        HeartbeatRole::Disabled => {
            unreachable!("disabled connections do not schedule heartbeat timeouts")
        }
    }
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Register a subscription entry in active_subscriptions.
pub(super) fn register_subscription(
    us: &mut ServerUserState,
    subscription_id: SubscriptionId,
    agent_id: Uuid,
    mode: SubscriptionMode,
    cancel_tx: oneshot::Sender<()>,
    dst: Route,
    lease_deadline: Instant,
) {
    us.active_subscriptions.insert(
        subscription_id,
        SubscriptionEntry {
            subscription_id,
            agent_id,
            mode,
            cancel: cancel_tx,
            dst,
            lease_deadline,
        },
    );
}

/// Remove a subscription entry after the task exits.
pub(super) async fn cleanup_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
) -> Option<SubscriptionEntry> {
    let removed = user_state
        .write()
        .await
        .active_subscriptions
        .remove(&subscription_id);
    tracing::trace!(subscription_id = %subscription_id, "subscription cleaned up");
    removed
}

/// Push a subscription deadline out. Returns the owning agent_id when found.
pub(super) async fn extend_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
    lease_deadline: Instant,
) -> Option<Uuid> {
    let mut us = user_state.write().await;
    let entry = us.active_subscriptions.get_mut(&subscription_id)?;
    entry.lease_deadline = lease_deadline;
    Some(entry.agent_id)
}

/// Explicitly remove a subscription and cancel its stream task.
pub(super) async fn unsubscribe_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
) -> Option<SubscriptionEntry> {
    cleanup_subscription(user_state, subscription_id).await
}

/// Cancel all active subscriptions matching a predicate.
pub(super) fn cancel_subscriptions_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&SubscriptionEntry) -> bool,
) -> Vec<SubscriptionEntry> {
    let cancelled_ids: Vec<_> = us
        .active_subscriptions
        .iter()
        .filter_map(|(subscription_id, entry)| predicate(entry).then_some(*subscription_id))
        .collect();

    cancelled_ids
        .into_iter()
        .filter_map(|subscription_id| us.active_subscriptions.remove(&subscription_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Command, DirectMessage, Message};
    use crate::server::LOCAL_USER_ID;
    use crate::server::test_helpers::{test_ctx, test_state};
    use std::sync::Arc;
    use std::sync::Mutex;
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

    struct MockWriter {
        written: Arc<Mutex<Vec<String>>>,
    }

    impl crate::transport::MessageWriter for MockWriter {
        async fn write_message(&mut self, msg: &Message) -> crate::error::Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(msg.type_label().to_string());
            Ok(())
        }
    }

    /// Drain all messages from an Incoming receiver without blocking.
    async fn drain_incoming(rx: &mut mpsc::Receiver<Incoming>) -> Vec<String> {
        let mut labels = Vec::new();
        while let Ok(item) = rx.try_recv() {
            labels.push(match item {
                Incoming::Msg(_) => "Msg".to_string(),
                Incoming::Wrote(meta) => format!("Wrote(heartbeat={})", meta.is_heartbeat),
                Incoming::Eof => "Eof".to_string(),
                Incoming::TransportErr(e) => format!("TransportErr({e})"),
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
            heartbeat_role: HeartbeatRole::Dialer,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    fn test_acceptor_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "accepted-peer".to_string(),
            is_local: false,
            heartbeat_role: HeartbeatRole::Acceptor,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    #[tokio::test]
    async fn reader_loop_forwards_messages_then_eof() {
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::message::DebugFormat::Yaml,
                },
            }),
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
        // Simulate: good message → undecodable frame → good message → EOF
        let decode_err = rmp_serde::decode::Error::Syntax("unknown variant".to_string());
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Err(AmuxError::SerializationDecode(decode_err)),
            Ok(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::message::DebugFormat::Yaml,
                },
            }),
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        // Undecodable frame should be skipped — two messages plus EOF
        let items = drain_incoming(&mut rx).await;
        assert_eq!(
            items,
            vec!["Msg", "Msg", "Eof"],
            "decode error should be skipped, not forwarded"
        );
    }

    #[tokio::test]
    async fn reader_loop_fatal_io_error_sends_transport_err() {
        let reader = MockReader::new(vec![Err(AmuxError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        )))]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items.len(), 1);
        assert!(
            items[0].starts_with("TransportErr("),
            "fatal I/O error should produce TransportErr, got {:?}",
            items[0]
        );
    }

    #[tokio::test]
    async fn writer_loop_reports_successful_writes() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = MockWriter {
            written: written.clone(),
        };
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let (incoming_tx, mut incoming_rx) = mpsc::channel(16);

        let handle = tokio::spawn(writer_loop(writer, outgoing_rx, incoming_tx));

        outgoing_tx
            .send(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::message::DebugFormat::Yaml,
                },
            })
            .await
            .unwrap();
        outgoing_tx
            .send(Message::Direct {
                message: DirectMessage::Heartbeat,
            })
            .await
            .unwrap();
        drop(outgoing_tx);

        handle.await.unwrap();

        let items = drain_incoming(&mut incoming_rx).await;
        assert_eq!(
            items,
            vec!["Wrote(heartbeat=false)", "Wrote(heartbeat=true)"]
        );
        assert_eq!(
            &*written.lock().unwrap(),
            &vec![
                "Command::Debug".to_string(),
                "Direct::Heartbeat".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn reader_loop_stops_when_receiver_dropped() {
        // If the receiver is dropped, reader_loop should exit on next send
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
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
            .send(Incoming::TransportErr(AmuxError::Io(io_err)))
            .await
            .unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_err(), "TransportErr should propagate as Err");
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
            .send(Incoming::Msg(Box::new(Message::Command {
                command: Command::ListAgents,
            })))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // ListAgents should have produced a ListAgentsResult
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(
                msg,
                Message::Command {
                    command: Command::ListAgentsResult { .. }
                }
            ),
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
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::ReauthResult { error: None },
            })))
            .await
            .unwrap();
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Command {
                command: Command::ListAgents,
            })))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // The ReauthResult should be skipped; only ListAgentsResult should appear
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(
                msg,
                Message::Command {
                    command: Command::ListAgentsResult { .. }
                }
            ),
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
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_dialer_inbound_traffic_does_not_delay_heartbeat() {
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
                idle_interval: Duration::from_millis(25),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await;
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::HeartbeatAck,
            })))
            .await
            .unwrap();

        let msg = tokio::time::timeout(Duration::from_millis(40), response_rx.recv())
            .await
            .expect("inbound-only traffic should not suppress a dialer heartbeat")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_dialer_outbound_write_resets_heartbeat_timer() {
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
                idle_interval: Duration::from_millis(40),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        tokio::time::sleep(Duration::from_millis(20)).await;
        incoming_tx
            .send(Incoming::Wrote(MessageMetadata {
                is_heartbeat: false,
            }))
            .await
            .unwrap();

        let recv_result = tokio::time::timeout(Duration::from_millis(15), response_rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "outbound activity should reset the dialer heartbeat timer"
        );

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should fire after the reset idle interval")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_times_out_when_dialer_heartbeat_unacked() {
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
                idle_interval: Duration::from_millis(15),
                ack_timeout: Duration::from_millis(15),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx
            .send(Incoming::Wrote(MessageMetadata { is_heartbeat: true }))
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(AmuxError::HeartbeatTimeout)),
            "expected heartbeat timeout, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_times_out_when_heartbeat_write_callback_never_arrives() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (_incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(15),
                ack_timeout: Duration::from_millis(15),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(AmuxError::HeartbeatTimeout)),
            "expected heartbeat timeout without a write callback, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_acceptor_times_out_when_peer_heartbeat_is_overdue() {
        let (state, user_state) = test_state().await;
        let ctx = test_acceptor_ctx(state, user_state);
        let (_incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let result = connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(20),
                ack_timeout: Duration::from_millis(20),
            }),
        )
        .await;

        assert!(
            matches!(result, Err(AmuxError::HeartbeatTimeout)),
            "expected acceptor heartbeat timeout, got {:?}",
            result
        );
        assert!(
            response_rx.try_recv().is_err(),
            "acceptors should not initiate heartbeats"
        );
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
                idle_interval: Duration::from_millis(120),
                ack_timeout: Duration::from_millis(60),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(200), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        // The ack arrives before the writer reports the heartbeat write. That
        // late write callback must not expose a stale idle deadline or queue a
        // second heartbeat immediately.
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::HeartbeatAck,
            })))
            .await
            .unwrap();
        incoming_tx
            .send(Incoming::Wrote(MessageMetadata { is_heartbeat: true }))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

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
        let heartbeat = HeartbeatState::Dialer(DialerHeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_tx_at: tokio::time::Instant::now(),
            probe_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        });

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
        let mut heartbeat = HeartbeatState::Dialer(DialerHeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_tx_at: tokio::time::Instant::now(),
            probe_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        });
        let previous_last_tx_at = match &heartbeat {
            HeartbeatState::Dialer(state) => state.last_tx_at,
            HeartbeatState::Acceptor(_) => unreachable!(),
        };

        heartbeat.pause_for_refresh();

        assert!(
            !heartbeat.ack_pending(),
            "refresh start should clear any pending heartbeat ack timeout"
        );
        assert!(
            matches!(&heartbeat, HeartbeatState::Dialer(state) if state.last_tx_at == previous_last_tx_at),
            "refresh suppression should not invent outbound activity before the Reauth write succeeds"
        );
    }
}
