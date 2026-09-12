use thiserror::Error;
#[cfg(unix)]
use tonic::transport::{Channel, Endpoint};

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("invalid connection configuration: {0}")]
    InvalidConfiguration(String),
    #[error("local sockets are not supported on this platform")]
    Unsupported,
    #[error("failed to connect local socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to establish RPC channel: {0}")]
    Transport(#[from] tonic::transport::Error),
}

impl ConnectError {
    /// Recover the underlying socket error when tonic wrapped it while
    /// establishing the HTTP/2 channel. Callers use this to distinguish a
    /// missing local service from a malformed endpoint or protocol failure.
    pub fn io_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            Self::Io(error) => Some(error.kind()),
            Self::Transport(error) => {
                let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
                while let Some(error) = source {
                    if let Some(error) = error.downcast_ref::<std::io::Error>() {
                        return Some(error.kind());
                    }
                    source = error.source();
                }
                None
            }
            Self::InvalidConfiguration(_) | Self::Unsupported => None,
        }
    }
}

/// Connect to a caller-selected local RPC socket without loading settings or
/// starting a node.
#[cfg(unix)]
pub async fn connect_socket(path: &std::path::Path) -> Result<Channel, ConnectError> {
    let path = path.to_owned();
    Endpoint::from_static("http://local-amux-service")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                tokio::net::UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(ConnectError::from)
}
