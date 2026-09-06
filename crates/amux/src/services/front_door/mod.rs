//! Installation administration, served independently of every profile's client API.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::{StreamExt, stream};
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use crate::installation::Installation;
use crate::protocol::wire;
#[cfg(test)]
use crate::transport::GrpcIo;
use crate::transport::{self, BoxedGrpcIo, ShutdownIo};

/// A plain gRPC connection to the installation's local administration socket.
/// Connecting here does not select a profile or open its client API.
pub struct FrontDoorClient {
    pub profiles: wire::profile_service_client::ProfileServiceClient<Channel>,
    pub installation: wire::installation_service_client::InstallationServiceClient<Channel>,
}

impl FrontDoorClient {
    pub fn admin(&self, id: crate::installation::ProfileId) -> ProfileAdminClient {
        ProfileAdminClient::new(id, self.profiles.clone())
    }

    #[cfg(not(unix))]
    pub async fn connect(_path: &std::path::Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "installation sockets are not supported on this platform",
        ))
    }

    #[cfg(unix)]
    pub async fn connect(path: &std::path::Path) -> std::io::Result<Self> {
        let stream = tokio::net::UnixStream::connect(path).await?;
        let channel = transport::channel_from_single_io(
            tonic::transport::Endpoint::from_static("http://localhost"),
            "front door",
            crate::transport::GrpcIo::new(stream),
        );
        Ok(Self {
            profiles: wire::profile_service_client::ProfileServiceClient::new(channel.clone()),
            installation: wire::installation_service_client::InstallationServiceClient::new(
                channel,
            ),
        })
    }
}

mod client;
pub use client::ProfileAdminClient;

mod handlers;
mod ledger;
pub(crate) use ledger::Ledger as FrontDoorOperations;
mod mapping;
#[cfg(test)]
mod tests;

/// The same service state can serve several local callers without selecting a
/// process-wide current profile. Keep the installation alive independently of it.
#[derive(Clone)]
pub struct FrontDoor {
    installation: Arc<Installation>,
    path: Option<PathBuf>,
    operations: Arc<ledger::Ledger>,
}

impl FrontDoor {
    pub fn new(installation: Arc<Installation>, path: Option<PathBuf>) -> Self {
        let operations = installation.front_door_operations.clone();
        Self {
            installation,
            path,
            operations,
        }
    }

    fn router(&self) -> tonic::transport::server::Router {
        transport::tonic_server_builder()
            .add_service(wire::profile_service_server::ProfileServiceServer::new(
                self.clone(),
            ))
            .add_service(
                wire::installation_service_server::InstallationServiceServer::new(self.clone()),
            )
    }

    /// Open a plain gRPC channel with only ProfileService and InstallationService.
    pub fn channel(&self) -> Channel {
        let (client, server, _connection) = transport::managed_in_process_transport_pair();
        let router = self.router();
        tokio::spawn(async move {
            if let Err(error) = router
                .serve_with_incoming(stream::once(async move {
                    Ok::<_, std::io::Error>(BoxedGrpcIo::local_trusted(server))
                }))
                .await
            {
                tracing::debug!(%error, "front-door channel closed");
            }
        });
        transport::in_process_channel(client)
    }

    /// Bind the configured front door with owner-only socket permissions.
    #[cfg(unix)]
    pub fn listen(&self) -> std::io::Result<FrontDoorListener> {
        let path = self.path.clone().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "front-door socket path is not configured",
            )
        })?;
        let listener = transport::bind_unix_listener(&path)?;
        let ownership = SocketOwnership::capture(path)?;
        let cancellation = CancellationToken::new();
        let connections = CancellationToken::new();
        let closed = connections.clone();
        let incoming = transport::unix_incoming(listener)
            .map(move |io| io.map(|io| ShutdownIo::new(io, closed.clone())));
        let router = self.router();
        let shutdown = cancellation.clone();
        let task = tokio::spawn(async move {
            let _ownership = ownership;
            if let Err(error) = router
                .serve_with_incoming_shutdown(incoming, shutdown.cancelled())
                .await
            {
                tracing::warn!(%error, "front-door listener failed");
            }
        });
        Ok(FrontDoorListener {
            cancellation,
            connections,
            task,
        })
    }
}

/// Owns a local front door. Closing it does not stop any profile runtime.
pub struct FrontDoorListener {
    cancellation: CancellationToken,
    connections: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl FrontDoorListener {
    pub async fn stop(mut self) {
        self.cancellation.cancel();
        // Let the shutdown RPC finish writing its reply before closing existing
        // channels. A stalled caller cannot hold the daemon open indefinitely.
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.task)
            .await
            .is_err()
        {
            self.connections.cancel();
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}
impl Drop for FrontDoorListener {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.connections.cancel();
        self.task.abort();
    }
}

#[cfg(unix)]
struct SocketOwnership {
    path: PathBuf,
    device: u64,
    inode: u64,
}
#[cfg(unix)]
impl SocketOwnership {
    fn capture(path: PathBuf) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}
#[cfg(unix)]
impl Drop for SocketOwnership {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %self.path.display(), "cannot remove front-door socket");
        }
    }
}
