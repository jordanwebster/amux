//! Catalogues: what an agent offers to pick from, written by the agent
//! process into `agents/<id>/catalogues/<sha256>` before the snapshot that
//! names the hash is journaled. A paired host keeps a copy of each one it
//! read under the replica's directory, so it answers its own clients again
//! with the origin away.

use std::io;

use prost::Message as _;
use sha2::{Digest as _, Sha256};
use store::{AgentRow, StoreError};
use wire::{Catalogue, ErrorCode};

use crate::blobs::{BlobError, hex, write_blob};
use crate::runtime::ProfileRuntime;

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("the agent has not said what it offers yet")]
    NotOffered,
    /// A peer's agent whose catalogue this host has not read yet.
    #[error("this host holds no copy of the agent's catalogue {0}")]
    NotHeld(String),
    #[error("a catalogue for a host is not served yet")]
    HostForm,
    #[error("the request names neither an agent nor a host")]
    NoTarget,
    #[error("reading the catalogue: {0}")]
    Read(io::Error),
    #[error("the catalogue {0} does not decode")]
    Decode(String),
    #[error("the catalogue a peer sent is not {0}")]
    Mismatch(String),
    #[error("writing the catalogue: {0}")]
    Write(io::Error),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl CatalogueError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NotOffered | Self::NotHeld(_) => ErrorCode::NotFound,
            Self::HostForm => ErrorCode::Unimplemented,
            Self::NoTarget => ErrorCode::InvalidArgument,
            Self::Read(_)
            | Self::Decode(_)
            | Self::Mismatch(_)
            | Self::Write(_)
            | Self::Store(_) => ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

impl From<BlobError> for CatalogueError {
    fn from(error: BlobError) -> Self {
        match error {
            BlobError::Store(error) => Self::Store(error),
            _ => Self::NoAgent,
        }
    }
}

/// The hash a catalogue is stored and named by: its encoding with `hash`
/// empty.
pub fn catalogue_hash(catalogue: &Catalogue) -> Vec<u8> {
    Sha256::digest(unhashed(catalogue)).to_vec()
}

fn unhashed(catalogue: &Catalogue) -> Vec<u8> {
    Catalogue {
        hash: Vec::new(),
        ..catalogue.clone()
    }
    .encode_to_vec()
}

impl ProfileRuntime {
    /// The catalogue the agent's newest snapshot names, from its own
    /// directory or this host's copy of a peer's. A peer's not read yet is
    /// [`CatalogueError::NotHeld`]: its origin answers it.
    pub async fn catalogue(&self, agent_id: &[u8]) -> Result<Catalogue, CatalogueError> {
        let row = self.blob_owner(agent_id).await?;
        let own = row.agent.host == self.host().as_bytes();
        let Some(hash) = row.snapshot.as_ref().and_then(|s| s.catalogue.clone()) else {
            return Err(if own {
                CatalogueError::NotOffered
            } else {
                CatalogueError::NotHeld(String::new())
            });
        };
        let path = self.catalogue_path(&row, &hash);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !own => {
                return Err(CatalogueError::NotHeld(hex(&hash)));
            }
            Err(error) => return Err(CatalogueError::Read(error)),
        };
        let mut catalogue =
            Catalogue::decode(bytes.as_slice()).map_err(|_| CatalogueError::Decode(hex(&hash)))?;
        catalogue.hash = hash;
        Ok(catalogue)
    }

    /// Keeps a peer's agent's catalogue, read from its origin, under that
    /// replica's directory, once it proves to be what its hash names.
    pub async fn keep_replica_catalogue(
        &self,
        agent_id: &[u8],
        catalogue: &Catalogue,
    ) -> Result<(), CatalogueError> {
        if catalogue_hash(catalogue) != catalogue.hash {
            return Err(CatalogueError::Mismatch(hex(&catalogue.hash)));
        }
        let row = self.blob_owner(agent_id).await?;
        if row.agent.host == self.host().as_bytes() {
            return Ok(());
        }
        let dir = self.files_of(&row).join(agent_dir::CATALOGUES);
        let (hash, bytes) = (catalogue.hash.clone(), unhashed(catalogue));
        tokio::task::spawn_blocking(move || write_blob(&dir, &hash, &bytes))
            .await
            .map_err(|error| CatalogueError::Write(io::Error::other(error)))?
            .map_err(CatalogueError::Write)
    }

    fn catalogue_path(&self, row: &AgentRow, hash: &[u8]) -> std::path::PathBuf {
        self.files_of(row)
            .join(agent_dir::CATALOGUES)
            .join(hex(hash))
    }
}
