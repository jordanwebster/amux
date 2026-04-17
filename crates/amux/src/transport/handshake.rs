use std::time::Duration;

use thiserror::Error;

use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::protocol::link::Link;
use crate::protocol::message::ProtocolError;
use crate::transport::{Transport, TransportError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) type Result<T> = std::result::Result<T, HandshakeError>;

#[derive(Debug, Error)]
pub(crate) enum HandshakeError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("handshake timed out")]
    Timeout,
    #[error("invalid handshake message: {0}")]
    InvalidMessage(String),
    #[error("server rejected connection: {0}")]
    Protocol(ProtocolError),
    #[error(
        "handshake failed after 5 link-name collisions — this is usually transient, retry the command"
    )]
    TooManyAttempts,
}

/// Outcome of a successful client-initiated handshake.
pub(crate) struct HandshakeOutcome {
    pub(crate) link: Link,
    /// Negotiated idle timeout. `None` means heartbeats are disabled on this
    /// connection.
    pub(crate) idle_timeout_secs: Option<u32>,
}

pub(crate) async fn connect_handshake<T, F>(
    transport: &mut T,
    mut generate_link: F,
) -> Result<HandshakeOutcome>
where
    T: Transport,
    F: FnMut() -> Link,
{
    for attempt in 0..5 {
        let proposed_link = generate_link();

        let connect = Connect {
            link_name: proposed_link.as_str().to_string(),
            token: None,
            version: PROTOCOL_VERSION,
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        let payload = connect
            .encode()
            .map_err(TransportError::SerializationEncode)?;
        transport.write_frame(&payload).await?;

        let payload = tokio::time::timeout(HANDSHAKE_TIMEOUT, transport.read_frame())
            .await
            .map_err(|_| HandshakeError::Timeout)??;
        let response = ConnectResult::decode(&payload).map_err(|e| {
            HandshakeError::InvalidMessage(format!("expected ConnectResult during handshake: {e}"))
        })?;

        match response.error {
            None => {
                return Ok(HandshakeOutcome {
                    link: proposed_link,
                    idle_timeout_secs: response.idle_timeout_secs,
                });
            }
            Some(ProtocolError::LinkNameTaken) => {
                tracing::debug!(link = %proposed_link, attempt = attempt + 1, "link name taken, retrying");
                continue;
            }
            Some(other) => return Err(HandshakeError::Protocol(other)),
        }
    }

    Err(HandshakeError::TooManyAttempts)
}
