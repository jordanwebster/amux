use std::time::Duration;

use tokio::sync::mpsc;

use super::context::{ConnectionError, HeartbeatRole, MessageMetadata, Result};
use crate::protocol::message::{DirectMessage, Message};
use crate::transport::TransportError;

const HEARTBEAT_IDLE_INTERVAL: Duration = Duration::from_secs(60);
const HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
pub(super) struct HeartbeatConfig {
    pub(super) idle_interval: Duration,
    pub(super) ack_timeout: Duration,
}

pub(super) fn heartbeat_config_for_role(role: HeartbeatRole) -> Option<HeartbeatConfig> {
    (!matches!(role, HeartbeatRole::Disabled)).then_some(HeartbeatConfig {
        idle_interval: HEARTBEAT_IDLE_INTERVAL,
        ack_timeout: HEARTBEAT_ACK_TIMEOUT,
    })
}

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

pub(super) struct DialerHeartbeatState {
    pub(super) config: HeartbeatConfig,
    pub(super) last_tx_at: tokio::time::Instant,
    pub(super) probe_deadline: Option<tokio::time::Instant>,
}

pub(super) struct AcceptorHeartbeatState {
    pub(super) config: HeartbeatConfig,
    pub(super) last_rx_at: tokio::time::Instant,
}

/// Tracks heartbeat ownership for a single peer connection.
pub(super) enum HeartbeatState {
    Dialer(DialerHeartbeatState),
    Acceptor(AcceptorHeartbeatState),
}

impl HeartbeatState {
    pub(super) fn new(role: HeartbeatRole, config: HeartbeatConfig) -> Option<Self> {
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
    pub(super) fn deadlines(&self) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
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
    pub(super) fn note_inbound_activity(&mut self) {
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
    pub(super) fn note_outbound_write(&mut self, meta: MessageMetadata) {
        if let Self::Dialer(state) = self
            && !meta.is_heartbeat
        {
            state.last_tx_at = tokio::time::Instant::now();
        }
    }

    /// Suspend heartbeat timers while waiting for an in-band refresh response.
    pub(super) fn pause_for_refresh(&mut self) {
        if let Self::Dialer(state) = self {
            state.probe_deadline = None;
        }
    }

    pub(super) async fn queue_heartbeat(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
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
                    ConnectionError::Transport(TransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "outgoing channel closed while sending heartbeat",
                    )))
                })?;
                Ok(())
            }
            Self::Acceptor(_) => unreachable!("acceptors never initiate heartbeats"),
        }
    }

    pub(super) fn role(&self) -> HeartbeatRole {
        match self {
            Self::Dialer(_) => HeartbeatRole::Dialer,
            Self::Acceptor(_) => HeartbeatRole::Acceptor,
        }
    }

    pub(super) fn ack_pending(&self) -> bool {
        matches!(self, Self::Dialer(state) if state.probe_deadline.is_some())
    }
}

pub(super) fn heartbeat_deadlines(
    heartbeat: Option<&HeartbeatState>,
    refresh_has_priority: bool,
) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
    if refresh_has_priority {
        (None, None)
    } else {
        heartbeat.map(|h| h.deadlines()).unwrap_or((None, None))
    }
}

pub(super) fn refresh_has_priority(
    refresh_deadline: Option<tokio::time::Instant>,
    refresh_awaiting_response: bool,
) -> bool {
    refresh_awaiting_response
        || refresh_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now())
}
