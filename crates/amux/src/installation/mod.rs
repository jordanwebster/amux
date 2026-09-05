//! Installation ownership, isolated profile storage, and supervised lifecycles.

pub use crate::services::front_door::{FrontDoor, FrontDoorListener};

pub mod binding;
mod credentials;
mod operation;
pub use binding::{
    AccountId, BindError, BindRequest, BindTarget, CloudServiceId, NonPristine, UserInfo,
};
mod paths;
pub mod supervisor;

pub(crate) use operation::OperationGate;
pub use supervisor::{
    CredentialSource, Installation, InstallationOptions, Intent, OperationId, ProfileEvent,
    ProfileStatus, ProfileWatch,
};

pub use crate::profile::runtime::{InstallationSettings, Listeners};
pub use crate::profile::status::Observed;
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
    #[error("profile is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid profile registry: {0}")]
    Registry(String),
}

// Results in the retry ledger own their error. Preserve OS error codes where
// available, and preserve the kind and diagnostic for constructed I/O errors.
impl Clone for InstallationError {
    fn clone(&self) -> Self {
        match self {
            Self::UnknownProfile(id) => Self::UnknownProfile(*id),
            Self::Deleted(id) => Self::Deleted(*id),
            Self::RevisionMismatch { expected, actual } => Self::RevisionMismatch {
                expected: *expected,
                actual: *actual,
            },
            Self::RootBusy(path) => Self::RootBusy(path.clone()),
            Self::SocketPathTooLong(path) => Self::SocketPathTooLong(path.clone()),
            Self::SocketOccupied(path) => Self::SocketOccupied(path.clone()),
            Self::InvalidPath(path) => Self::InvalidPath(path.clone()),
            Self::Io(error) => Self::Io(match error.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::new(error.kind(), error.to_string()),
            }),
            Self::Registry(error) => Self::Registry(error.clone()),
            Self::Unavailable(error) => Self::Unavailable(error.clone()),
        }
    }
}
