//! `<data_dir>/registry`: which profiles the installation hosts. Each
//! profile is a complete amux with its own host id, store and agents.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::install::{AGENTS, HOST_ID, PROFILES, REGISTRY, private_dir, write_durably};

pub type ProfileId = Uuid;

/// The client socket in each profile's directory.
pub const PROFILE_SOCKET: &str = "sock";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub profiles: Vec<ProfileEntry>,
}

/// One profile as the registry lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEntry {
    pub id: ProfileId,
    /// What people call it; unique within the installation.
    pub label: String,
    /// Bumped by every change to the entry, so a rename or delete can say
    /// which version it meant.
    pub revision: u64,
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

    pub(crate) fn write(&self, data_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_durably(&data_dir.join(REGISTRY), &bytes)
    }

    pub fn entry(&self, id: ProfileId) -> Option<&ProfileEntry> {
        self.profiles.iter().find(|entry| entry.id == id)
    }

    /// A label no profile has yet: `wanted`, or `wanted-2`, `wanted-3`…
    pub fn free_label(&self, wanted: &str) -> String {
        let taken = |label: &str| self.profiles.iter().any(|entry| entry.label == label);
        if !taken(wanted) {
            return wanted.to_owned();
        }
        (2..)
            .map(|n| format!("{wanted}-{n}"))
            .find(|label| !taken(label))
            .expect("some suffix is free")
    }
}

/// Where one profile's files live.
pub fn profile_dir(data_dir: &Path, profile: ProfileId) -> PathBuf {
    data_dir.join(PROFILES).join(profile.to_string())
}

/// Creates a profile: its directory, a fresh host id, and its registry
/// entry, written last so a crash leaves no half-made profile listed. The
/// label is made unique.
pub fn create_profile(data_dir: &Path) -> io::Result<ProfileId> {
    create_labelled(data_dir, "default").map(|entry| entry.id)
}

pub(crate) fn create_labelled(data_dir: &Path, label: &str) -> io::Result<ProfileEntry> {
    let profile = Uuid::new_v4();
    let dir = profile_dir(data_dir, profile);
    private_dir(&dir.join(AGENTS))?;
    write_durably(&dir.join(HOST_ID), Uuid::new_v4().to_string().as_bytes())?;
    let mut registry = Registry::read(data_dir)?;
    let entry = ProfileEntry {
        id: profile,
        label: registry.free_label(label),
        revision: 1,
    };
    registry.profiles.push(entry.clone());
    registry.write(data_dir)?;
    Ok(entry)
}

/// Takes back a profile that was created but never came to be hosted: its
/// registry entry and its directory go, so the next start does not host a
/// profile nobody was told about and a retry does not make a second.
pub(crate) fn discard(data_dir: &Path, profile: ProfileId) -> io::Result<()> {
    let mut registry = Registry::read(data_dir)?;
    registry.profiles.retain(|entry| entry.id != profile);
    registry.write(data_dir)?;
    match std::fs::remove_dir_all(profile_dir(data_dir, profile)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The profile's host id, as written when it was created.
pub fn host_id(profile_dir: &Path) -> io::Result<Uuid> {
    let text = std::fs::read_to_string(profile_dir.join(HOST_ID))?;
    Uuid::parse_str(text.trim()).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discarded_profile_leaves_neither_an_entry_nor_a_directory() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let gone = create_labelled(data_dir.path(), "gone").unwrap();

        discard(data_dir.path(), gone.id).unwrap();

        let registry = Registry::read(data_dir.path()).unwrap();
        assert_eq!(registry.profiles, vec![kept.clone()]);
        assert!(!profile_dir(data_dir.path(), gone.id).exists());
        assert!(profile_dir(data_dir.path(), kept.id).exists());
        // Discarding what is already gone is not an error.
        discard(data_dir.path(), gone.id).unwrap();
    }
}
