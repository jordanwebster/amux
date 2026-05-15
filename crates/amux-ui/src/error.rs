use thiserror::Error;

use super::notification::{DisconnectReason, SessionFailureReason};

#[derive(Clone, Debug, Error)]
pub enum AmuxError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("server shutdown: {0}")]
    ServerShutdown(String),
    #[error("{0}")]
    Other(String),
}

pub(crate) fn disconnect_reason(error: amux::ClientError) -> DisconnectReason {
    match error {
        amux::ClientError::ServerShutdown(_) => DisconnectReason::ServerShutdown,
        amux::ClientError::Transport(error) => DisconnectReason::TransportError(error.to_string()),
        other => DisconnectReason::TransportError(other.to_string()),
    }
}

pub(crate) fn session_failure_reason(error: &amux::ClientError) -> SessionFailureReason {
    match error {
        amux::ClientError::Protocol(error) => protocol_failure_reason(error),
        amux::ClientError::Transport(error) => SessionFailureReason::Transport(error.to_string()),
        other => SessionFailureReason::Other(other.to_string()),
    }
}

pub(crate) fn protocol_failure_reason(
    error: &amux::protocol::ProtocolError,
) -> SessionFailureReason {
    match error {
        amux::protocol::ProtocolError::NoAgentFound => SessionFailureReason::NotFound,
        amux::protocol::ProtocolError::Unimplemented { .. } => SessionFailureReason::Unsupported,
        other => SessionFailureReason::Other(other.to_string()),
    }
}

impl From<amux::ClientError> for AmuxError {
    fn from(error: amux::ClientError) -> Self {
        match error {
            amux::ClientError::Transport(error) => Self::Transport(error.to_string()),
            amux::ClientError::Protocol(error) => Self::Protocol(error.to_string()),
            amux::ClientError::ServerShutdown(reason) => Self::ServerShutdown(reason.to_string()),
            other => Self::Other(other.to_string()),
        }
    }
}
