//! Installation-owned registry and isolated profile storage.

mod paths;
pub mod registry;

use std::path::PathBuf;

pub use paths::ProfilePaths;
#[cfg(unix)]
pub(crate) use paths::{MAX_CODEX_SOCKET_PATH_BYTES, adjacent_codex_socket_path};
pub use registry::{Binding, InstallationRoot, ProfileId, ProfileLabel, ProfileRecord, Registry};

#[derive(Debug, thiserror::Error)]
pub enum InstallationError {
    #[error("unknown profile {0}")]
    UnknownProfile(ProfileId),
    #[error("profile {0} has been deleted")]
    Deleted(ProfileId),
    #[error("profile revision mismatch: expected {expected}, actual {actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("installation root is already in use: {}", .0.display())]
    RootBusy(PathBuf),
    #[error("socket path is too long: {}", .0.display())]
    SocketPathTooLong(PathBuf),
    #[error("socket path is occupied: {}", .0.display())]
    SocketOccupied(PathBuf),
    #[error("invalid installation path: {}", .0.display())]
    InvalidPath(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid profile registry: {0}")]
    Registry(String),
}
