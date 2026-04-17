use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::super::{ServerState, ServerUserState};
use crate::agent::SessionEvent;
use crate::protocol::link::Link;
use crate::protocol::message::{DirectMessage, Message};
use crate::transport::TransportError;

pub(in crate::server) type Result<T> = std::result::Result<T, ConnectionError>;

#[derive(Debug, Error)]
pub(in crate::server) enum ConnectionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("{0}")]
    Config(String),
    #[error("Invalid or missing credentials")]
    InvalidCredentials,
    #[error(
        "protocol mismatch (server protocol v{server_version}, client protocol v{client_version})"
    )]
    ProtocolMismatch {
        server_version: u32,
        client_version: u32,
    },
    #[error("amux upgrade required (minimum v{minimum_version}, you have v{client_version})")]
    UpgradeRequired {
        minimum_version: String,
        client_version: String,
    },
    #[error("heartbeat timed out")]
    HeartbeatTimeout,
}

/// Context for connection handlers.
pub(crate) struct ConnectionContext {
    pub(crate) state: Arc<RwLock<ServerState>>,
    pub(crate) user_state: Arc<RwLock<ServerUserState>>,
    pub(crate) user_id: Uuid,
    pub(crate) event_tx: mpsc::Sender<SessionEvent>,
    pub(crate) link: Link,
    pub(crate) is_local: bool,
    /// Negotiated heartbeat setup for this connection. `None` disables
    /// heartbeats entirely (used for local Unix-socket connections).
    pub(crate) heartbeat: Option<HeartbeatSetup>,
    pub(crate) next_request_id: Arc<AtomicU64>,
    /// Client implementation name (from Connect handshake, e.g. "amux-cli").
    pub(crate) client_name: Option<String>,
    /// Semantic version of the connecting client (from Connect handshake).
    pub(crate) client_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeartbeatRole {
    Dialer,
    Acceptor,
}

impl HeartbeatRole {
    pub(in crate::server) fn as_str(self) -> &'static str {
        match self {
            Self::Dialer => "dialer",
            Self::Acceptor => "acceptor",
        }
    }
}

/// Negotiated heartbeat configuration for a connection. Both peers drop the
/// connection after `idle_timeout` seconds without inbound traffic. Only the
/// dialer initiates heartbeats; the acceptor replies via `HeartbeatAck`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeartbeatSetup {
    pub(crate) role: HeartbeatRole,
    pub(crate) idle_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MessageMetadata {
    pub(super) is_heartbeat: bool,
}

impl MessageMetadata {
    pub(super) fn from_message(msg: &Message) -> Self {
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
    TransportErr(TransportError),
    Eof,
}
