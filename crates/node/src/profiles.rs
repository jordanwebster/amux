//! `<data_dir>/registry`: which profiles the installation hosts. Each
//! profile is a complete amux with its own host id, store and agents.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::install::{AGENTS, HOST_ID, PROFILES, REGISTRY, private_dir, write_durably};

pub type ProfileId = Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub profiles: Vec<ProfileId>,
}

impl Registry {
    pub fn read(data_dir: &Path) -> io::Result<Self> {
        match std::fs::read(data_dir.join(REGISTRY)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    fn write(&self, data_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_durably(&data_dir.join(REGISTRY), &bytes)
    }
}

/// Where one profile's files live.
pub fn profile_dir(data_dir: &Path, profile: ProfileId) -> PathBuf {
    data_dir.join(PROFILES).join(profile.to_string())
}

/// Creates a profile: its directory, a fresh host id, and its registry
/// entry, written last so a crash leaves no half-made profile listed.
pub fn create_profile(data_dir: &Path) -> io::Result<ProfileId> {
    let profile = Uuid::new_v4();
    let dir = profile_dir(data_dir, profile);
    private_dir(&dir.join(AGENTS))?;
    write_durably(&dir.join(HOST_ID), Uuid::new_v4().to_string().as_bytes())?;
    let mut registry = Registry::read(data_dir)?;
    registry.profiles.push(profile);
    registry.write(data_dir)?;
    Ok(profile)
}

/// The profile's host id, as written when it was created.
pub fn host_id(profile_dir: &Path) -> io::Result<Uuid> {
    let text = std::fs::read_to_string(profile_dir.join(HOST_ID))?;
    Uuid::parse_str(text.trim()).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
