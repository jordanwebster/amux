use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const REVISION_BLOCK_SIZE: u64 = 1_024;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedRevision {
    host_id: Uuid,
    reserved_through_revision: u64,
}

pub(super) struct InventoryRevisions {
    host_id: Uuid,
    path: PathBuf,
    through: u64,
    reserved_through: u64,
}

impl InventoryRevisions {
    pub(super) fn open(state_path: &Path, host_id: Uuid) -> io::Result<Self> {
        let path = state_path.with_file_name(format!("inventory-revision-{host_id}.yaml"));
        let previous_bound = match fs::read_to_string(&path) {
            Ok(contents) => {
                let persisted: PersistedRevision =
                    serde_yaml::from_str(&contents).map_err(io::Error::other)?;
                if persisted.host_id == host_id {
                    persisted.reserved_through_revision
                } else {
                    0
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        let mut revisions = Self {
            host_id,
            path,
            through: previous_bound,
            reserved_through: previous_bound,
        };
        revisions.reserve_block()?;
        Ok(revisions)
    }

    pub(super) fn through(&self) -> u64 {
        self.through
    }

    /// Return a value already covered by the durable reservation bound.
    pub(super) fn reserve(&mut self) -> io::Result<u64> {
        let next = self
            .through
            .checked_add(1)
            .ok_or_else(|| io::Error::other("inventory revision exhausted"))?;
        if next > self.reserved_through {
            self.reserve_block()?;
        }
        self.through = next;
        Ok(next)
    }

    fn reserve_block(&mut self) -> io::Result<()> {
        let next_bound = self.reserved_through.saturating_add(REVISION_BLOCK_SIZE);
        if next_bound == self.reserved_through {
            return Err(io::Error::other("inventory revision exhausted"));
        }
        let persisted = PersistedRevision {
            host_id: self.host_id,
            reserved_through_revision: next_bound,
        };
        let yaml = serde_yaml::to_string(&persisted).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = self.path.with_extension("yaml.tmp");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(yaml.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, &self.path)?;
        #[cfg(unix)]
        fs::File::open(
            self.path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        self.reserved_through = next_bound;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_protocol_revision_restart_skips_partially_used_block() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.yaml");
        let host_id = Uuid::new_v4();
        let mut first = InventoryRevisions::open(&state_path, host_id).unwrap();
        assert_eq!(first.reserve().unwrap(), 1);
        assert_eq!(first.reserve().unwrap(), 2);
        assert_eq!(first.reserve().unwrap(), 3);
        let persisted: PersistedRevision = serde_yaml::from_str(
            &fs::read_to_string(&first.path).expect("reservation file remains readable"),
        )
        .unwrap();
        assert_eq!(persisted.reserved_through_revision, REVISION_BLOCK_SIZE);
        drop(first);

        let mut restarted = InventoryRevisions::open(&state_path, host_id).unwrap();
        assert_eq!(restarted.through(), REVISION_BLOCK_SIZE);
        assert_eq!(restarted.reserve().unwrap(), REVISION_BLOCK_SIZE + 1);
        let persisted: PersistedRevision = serde_yaml::from_str(
            &fs::read_to_string(&restarted.path).expect("reservation file remains readable"),
        )
        .unwrap();
        assert_eq!(persisted.reserved_through_revision, REVISION_BLOCK_SIZE * 2);
    }

    #[test]
    fn daemon_protocol_revision_one_thousand_reservations_are_memory_only() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.yaml");
        let host_id = Uuid::new_v4();
        let mut revisions = InventoryRevisions::open(&state_path, host_id).unwrap();

        let started = std::time::Instant::now();
        for expected in 1..=1_000 {
            assert_eq!(revisions.reserve().unwrap(), expected);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "1,000 in-block revisions took {elapsed:?}"
        );
    }
}
