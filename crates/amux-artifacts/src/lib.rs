//! Content-addressed artifact storage shared by owning and viewing hosts.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod index;
mod owner;

pub use owner::Owner;

/// The maximum size accepted for one artifact.
pub const ARTIFACT_SIZE_CAP: u64 = 10 * 1024 * 1024;

/// How long an unpinned artifact remains eligible for storage.
pub const EPHEMERAL_TTL: Duration = Duration::from_secs(60 * 60);

/// The SHA-256 identity of an artifact.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactId(String);

impl ArtifactId {
    const PREFIX: &'static str = "sha256:";
    const HEX_LEN: usize = 64;

    /// Returns the canonical `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn hex(&self) -> &str {
        &self.0[Self::PREFIX.len()..]
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ArtifactId {
    type Err = InvalidArtifactId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix(Self::PREFIX) else {
            return Err(InvalidArtifactId(value.to_owned()));
        };
        if hex.len() != Self::HEX_LEN
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidArtifactId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

impl Serialize for ArtifactId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// A string that is not a canonical artifact identity.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid artifact id: {0}")]
pub struct InvalidArtifactId(String);

/// Computes the canonical identity for `bytes`.
pub fn id_of(bytes: &[u8]) -> ArtifactId {
    let digest = Sha256::digest(bytes);
    ArtifactId(format!("{}{:x}", ArtifactId::PREFIX, digest))
}

/// The closed set of artifact payload kinds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Image,
    File,
    Diff,
}

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
