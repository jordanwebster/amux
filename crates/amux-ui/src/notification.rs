use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
use tokio::sync::mpsc;

use super::cmd::{CmdId, CmdResult};
use super::error::AmuxError;
use super::types;

#[derive(Clone, Debug)]
pub enum Notification {
    Connected,
    Disconnected {
        reason: DisconnectReason,
    },
    AgentsSnapshot(Vec<types::Agent>),
    AgentAdded(types::Agent),
    AgentUpdated(types::Agent),
    AgentRemoved {
        id: types::AgentId,
        reason: Option<String>,
    },
    HostsSnapshot(Vec<types::Host>),
    HostAdded(types::Host),
    HostRemoved {
        id: types::HostId,
        reason: Option<String>,
    },
    SessionOpened(types::AgentId),
    SessionOutput {
        id: types::AgentId,
        payload: Bytes,
        phase: SessionPhase,
    },
    SessionReplayComplete(types::AgentId),
    SessionFailed {
        id: types::AgentId,
        reason: SessionFailureReason,
    },
    CommandCompleted {
        id: CmdId,
        result: CmdResult,
    },
    CommandFailed {
        id: CmdId,
        error: AmuxError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    Replay,
    Live,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    ServerShutdown,
    TransportError(String),
    ApplicationShutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionFailureReason {
    NotFound,
    Unsupported,
    AgentDeleted,
    AgentExited { exit_code: Option<i32> },
    HostUnreachable,
    InternalError(String),
    Transport(String),
    Other(String),
}

pub struct NotificationStream {
    rx: mpsc::Receiver<Notification>,
}

impl NotificationStream {
    pub(crate) fn new(rx: mpsc::Receiver<Notification>) -> Self {
        Self { rx }
    }
}

impl Stream for NotificationStream {
    type Item = Notification;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

pub(crate) fn closed_receiver() -> mpsc::Receiver<Notification> {
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    rx
}

pub(crate) async fn send(tx: &mpsc::Sender<Notification>, notification: Notification) {
    let _ = tx.send(notification).await;
}

pub(crate) async fn send_session_output(
    tx: &mpsc::Sender<Notification>,
    id: types::AgentId,
    payload: Bytes,
    phase: SessionPhase,
) {
    match tx.try_send(Notification::SessionOutput { id, payload, phase }) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("dropping session output notification because receiver is full");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}
