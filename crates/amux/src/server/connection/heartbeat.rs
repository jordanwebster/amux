use std::time::Duration;

use tokio::sync::mpsc;

use super::context::{ConnectionError, HeartbeatRole, HeartbeatSetup, MessageMetadata, Result};
use crate::protocol::message::Message;
use crate::transport::TransportError;

#[derive(Clone, Copy)]
pub(super) struct ConnectionActivity {
    pub(super) last_inbound_at: tokio::time::Instant,
    pub(super) last_outbound_at: tokio::time::Instant,
}

impl ConnectionActivity {
    pub(super) fn new() -> Self {
        let now = tokio::time::Instant::now();
        Self {
            last_inbound_at: now,
            last_outbound_at: now,
        }
    }

    pub(super) fn note_inbound(&mut self) {
        self.last_inbound_at = tokio::time::Instant::now();
    }

    pub(super) fn note_outbound(&mut self) {
        self.last_outbound_at = tokio::time::Instant::now();
    }
}

/// Tracks heartbeat state for a single peer connection.
///
/// Inbound activity is read from [`ConnectionActivity`]; this enum only
/// tracks state that is specific to the heartbeat timer (the dialer's
/// send cadence).
pub(super) enum HeartbeatState {
    Dialer {
        idle_timeout: Duration,
        /// Updated on non-heartbeat writes (reactively) and in
        /// [`HeartbeatState::queue_heartbeat`] (preemptively). Drives the
        /// dialer's "send next heartbeat" deadline at
        /// `last_tx_at + idle_timeout / 3`.
        ///
        /// Distinct from [`ConnectionActivity::last_outbound_at`] because
        /// the write callback for a queued heartbeat arrives asynchronously
        /// — without a preemptive update here, the send deadline would
        /// re-fire immediately after every queue.
        last_tx_at: tokio::time::Instant,
    },
    Acceptor {
        idle_timeout: Duration,
    },
}

impl HeartbeatState {
    pub(super) fn new(setup: HeartbeatSetup) -> Self {
        match setup.role {
            HeartbeatRole::Dialer => Self::Dialer {
                idle_timeout: setup.idle_timeout,
                last_tx_at: tokio::time::Instant::now(),
            },
            HeartbeatRole::Acceptor => Self::Acceptor {
                idle_timeout: setup.idle_timeout,
            },
        }
    }

    /// Returns `(send_deadline, kill_deadline)` for use in select! guards.
    ///
    /// `send_deadline` is when the dialer should queue its next heartbeat
    /// (always `None` for acceptors). `kill_deadline` is when either side
    /// should drop the connection due to inbound silence.
    pub(super) fn deadlines(
        &self,
        activity: &ConnectionActivity,
    ) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
        match *self {
            Self::Dialer {
                idle_timeout,
                last_tx_at,
            } => (
                Some(last_tx_at + idle_timeout / 3),
                Some(activity.last_inbound_at + idle_timeout),
            ),
            Self::Acceptor { idle_timeout } => {
                (None, Some(activity.last_inbound_at + idle_timeout))
            }
        }
    }

    /// Successful non-heartbeat outbound writes reset the dialer's idle-send
    /// timer. Heartbeat writes are timed from `queue_heartbeat`, so a late
    /// write callback must not move the idle timer.
    pub(super) fn note_outbound_write(&mut self, meta: MessageMetadata) {
        if let Self::Dialer { last_tx_at, .. } = self
            && !meta.is_heartbeat
        {
            *last_tx_at = tokio::time::Instant::now();
        }
    }

    pub(super) async fn queue_heartbeat(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
        match self {
            Self::Dialer { last_tx_at, .. } => {
                tracing::debug!(role = HeartbeatRole::Dialer.as_str(), "sending heartbeat");
                *last_tx_at = tokio::time::Instant::now();
                tx.send(Message::Ping).await.map_err(|_| {
                    ConnectionError::Transport(TransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "outgoing channel closed while sending heartbeat",
                    )))
                })?;
                Ok(())
            }
            Self::Acceptor { .. } => unreachable!("acceptors never initiate heartbeats"),
        }
    }

    pub(super) fn role(&self) -> HeartbeatRole {
        match self {
            Self::Dialer { .. } => HeartbeatRole::Dialer,
            Self::Acceptor { .. } => HeartbeatRole::Acceptor,
        }
    }
}

pub(super) fn heartbeat_deadlines(
    heartbeat: Option<&HeartbeatState>,
    activity: &ConnectionActivity,
    refresh_has_priority: bool,
) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
    if refresh_has_priority {
        (None, None)
    } else {
        heartbeat
            .map(|h| h.deadlines(activity))
            .unwrap_or((None, None))
    }
}

pub(super) fn refresh_has_priority(
    refresh_deadline: Option<tokio::time::Instant>,
    refresh_awaiting_response: bool,
) -> bool {
    refresh_awaiting_response
        || refresh_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now())
}
