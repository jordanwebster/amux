use thiserror::Error;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tonic::transport::{Channel, Endpoint};

use crate::config::{Config, ConfigError};
use crate::transport::TransportError;
#[cfg(unix)]
use crate::transport::connect_single_io;

type Result<T> = std::result::Result<T, ConnectError>;

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("{0}")]
    Start(String),
}

/// Connect to an existing daemon's ClientService over local IPC.
#[cfg(unix)]
pub(crate) async fn connect_existing_client_service(config: &Config) -> Result<Channel> {
    connect_client_service_socket(&config.socket_path).await
}

/// Connect to a ClientService socket named directly rather than through a
/// profile config. A client that was handed a socket path — the switcher is
/// told one per profile by the front door — has no config to load and must
/// not guess at one.
#[cfg(unix)]
pub(crate) async fn connect_client_service_socket(path: &std::path::Path) -> Result<Channel> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(TransportError::from)?;
    connect_single_io(
        Endpoint::from_static("http://local-client-service"),
        "local ClientService stream",
        stream,
    )
    .await
    .map_err(|error| ConnectError::Start(format!("failed to connect ClientService: {error}")))
}
