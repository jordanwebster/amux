use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The maximum size accepted for one artifact.
pub const ARTIFACT_SIZE_CAP: u64 = 32 * 1024 * 1024;

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

    /// Returns the digest portion of the canonical identity.
    pub fn hex(&self) -> &str {
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
}
