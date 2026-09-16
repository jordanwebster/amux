use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedRevision {
    host_id: Uuid,
    through_revision: u64,
}

pub(super) struct InventoryRevisions {
    host_id: Uuid,
    path: PathBuf,
    through: u64,
}

impl InventoryRevisions {
    pub(super) fn open(state_path: &Path, host_id: Uuid) -> io::Result<Self> {
        let path = state_path.with_file_name(format!("inventory-revision-{host_id}.yaml"));
        let through = match fs::read_to_string(&path) {
            Ok(contents) => {
                let persisted: PersistedRevision =
                    serde_yaml::from_str(&contents).map_err(io::Error::other)?;
                if persisted.host_id == host_id {
                    persisted.through_revision
                } else {
                    0
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        Ok(Self {
            host_id,
            path,
            through,
        })
    }

    pub(super) fn through(&self) -> u64 {
        self.through
    }

    /// Persist the next value before returning it to an authoritative event.
    pub(super) fn reserve(&mut self) -> io::Result<u64> {
        let next = self
            .through
            .checked_add(1)
            .ok_or_else(|| io::Error::other("inventory revision exhausted"))?;
        let persisted = PersistedRevision {
            host_id: self.host_id,
            through_revision: next,
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
        self.through = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_protocol_revision_restart_reserves_after_durable_value() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.yaml");
        let host_id = Uuid::new_v4();
        let mut first = InventoryRevisions::open(&state_path, host_id).unwrap();
        assert_eq!(first.reserve().unwrap(), 1);
        drop(first);

        let mut restarted = InventoryRevisions::open(&state_path, host_id).unwrap();
        assert_eq!(restarted.through(), 1);
        assert_eq!(restarted.reserve().unwrap(), 2);
    }
}
