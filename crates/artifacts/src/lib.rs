//! Content-addressed artifact storage shared by owning and viewing hosts.

use std::time::Duration;

use chrono::{DateTime, Utc};
pub use model::ARTIFACT_SIZE_CAP;
use model::{ArtifactId, ArtifactKind, ArtifactRef, id_of};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod cache;
mod index;
mod owner;

pub use cache::Cache;
pub use owner::Owner;

/// How long an unpinned artifact remains eligible for storage.
pub const EPHEMERAL_TTL: Duration = Duration::from_secs(60 * 60);

/// Metadata recorded when an artifact first enters the store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactMeta {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
    pub pinned_at: Option<DateTime<Utc>>,
}

impl ArtifactMeta {
    /// Converts stored metadata into the value exposed through client APIs.
    pub fn into_reference(self) -> ArtifactRef {
        ArtifactRef {
            id: self.id,
            kind: self.kind,
            name: self.name,
            mime: self.mime,
            size: self.size,
        }
    }
}

/// Supplies time to artifact lifetime and recency operations.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// A clock backed by the system's UTC time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A failure returned by a cache's remote fetch operation.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct FetchError {
    message: String,
}

impl FetchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// An artifact store operation failed.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("artifact is {size} bytes; maximum size is {max} bytes")]
    TooLarge { size: u64, max: u64 },
    #[error("artifact is not stored: {id}")]
    Missing { id: ArtifactId },
    #[error("artifact bytes do not match their id: {id}")]
    Corrupt { id: ArtifactId },
    #[error("artifact fetch failed: {0}")]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_sha256_with_canonical_text_and_json() {
        let id = id_of(b"abc");
        assert_eq!(
            id.as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(id.to_string().parse(), Ok(id.clone()));
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<ArtifactId>(&format!("\"{id}\"")).unwrap(),
            id
        );
    }

    #[test]
    fn artifact_id_rejects_noncanonical_text() {
        let uppercase = format!("sha256:{}", "A".repeat(64));
        assert!(uppercase.parse::<ArtifactId>().is_err());
        assert!("sha256:abcd".parse::<ArtifactId>().is_err());
        assert!(
            "md5:00000000000000000000000000000000"
                .parse::<ArtifactId>()
                .is_err()
        );
    }
}
