#[cfg(windows)]
use std::time::Duration;

use thiserror::Error;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

use super::Connection;
use crate::config::{Config, ConfigError};
use crate::protocol::handshake::RoutingRole;
use crate::protocol::message::ProtocolError;
use crate::protocol::route::generate_terminal_link;
use crate::transport::{HandshakeError, LocalTransport, TransportError, connect_handshake};

type Result<T> = std::result::Result<T, ConnectError>;

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("invalid handshake message: {0}")]
    InvalidHandshake(String),
    #[error("server rejected connection: {0}")]
    Protocol(ProtocolError),
    #[error("{0}")]
    Start(String),
}

impl From<HandshakeError> for ConnectError {
    fn from(error: HandshakeError) -> Self {
        match error {
            HandshakeError::Transport(error) => Self::Transport(error),
            HandshakeError::Timeout => Self::HandshakeTimeout,
            HandshakeError::InvalidMessage(message) => Self::InvalidHandshake(message),
            HandshakeError::Protocol(error) => Self::Protocol(error),
        }
    }
}

#[cfg(unix)]
async fn connect_local_transport(config: &Config) -> std::io::Result<LocalTransport> {
    let stream = tokio::net::UnixStream::connect(&config.socket_path).await?;
    Ok(LocalTransport::new(stream))
}

#[cfg(windows)]
async fn connect_local_transport(config: &Config) -> std::io::Result<LocalTransport> {
    let pipe_name = config.socket_path.to_string_lossy().into_owned();
    for _ in 0..20 {
        match ClientOptions::new().open(&pipe_name) {
            Ok(client) => return Ok(LocalTransport::new(client)),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
    ClientOptions::new()
        .open(&pipe_name)
        .map(LocalTransport::new)
}

/// Connect to an existing daemon via the local control-plane transport.
pub(crate) async fn connect_existing(config: &Config) -> Result<Connection> {
    let mut transport = connect_local_transport(config)
        .await
        .map_err(TransportError::from)?;
    let outcome = connect_handshake(
        &mut transport,
        generate_terminal_link,
        None,
        RoutingRole::Observer,
    )
    .await
    .map_err(ConnectError::from)?;
    tracing::info!(link = %outcome.link, "connected");
    Ok(Connection::new(transport, outcome.link))
}
