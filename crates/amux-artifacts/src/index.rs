use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{ArtifactId, ArtifactKind, ArtifactMeta, StoreError, id_of};

pub(crate) const BLOBS_DIR: &str = "blobs";
pub(crate) const INDEX_FILE: &str = "index.json";

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Index {
    artifacts: BTreeMap<ArtifactId, ArtifactMeta>,
}

impl Index {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(root.join(BLOBS_DIR))?;
        match fs::read(root.join(INDEX_FILE)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(index) => Ok(index),
                Err(_) => Self::recover(root),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::recover(root),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn write(&self, root: &Path) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        atomic_write(&root.join(INDEX_FILE), &bytes)?;
        Ok(())
    }

    pub(crate) fn get(&self, id: &ArtifactId) -> Option<&ArtifactMeta> {
        self.artifacts.get(id)
    }

    #[cfg(test)]
    fn insert(&mut self, meta: ArtifactMeta) {
        self.artifacts.insert(meta.id.clone(), meta);
    }

    fn recover(root: &Path) -> Result<Self, StoreError> {
        let mut index = Self::default();
        for entry in fs::read_dir(root.join(BLOBS_DIR))? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }

            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let bytes = fs::read(entry.path())?;
            let id = id_of(&bytes);
            if id.hex() != file_name {
                fs::remove_file(entry.path())?;
                continue;
            }

            let metadata = entry.metadata()?;
            let created_at = metadata
                .modified()
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(|_| DateTime::<Utc>::from(SystemTime::UNIX_EPOCH));
            let meta = ArtifactMeta {
                id: id.clone(),
                kind: ArtifactKind::File,
                name: file_name,
                mime: "application/octet-stream".to_owned(),
                size: bytes.len() as u64,
                created_at,
                pinned_at: None,
            };
            index.artifacts.insert(id, meta);
        }
        index.write(root)?;
        Ok(index)
    }
}

pub(crate) fn blob_path(root: &Path, id: &ArtifactId) -> PathBuf {
    root.join(BLOBS_DIR).join(id.hex())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("index path has no parent"))?;
    fs::create_dir_all(parent)?;

    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{INDEX_FILE}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "amux-artifacts-test-{}-{}",
                std::process::id(),
                sequence
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_blob(root: &Path, bytes: &[u8]) -> ArtifactId {
        let id = id_of(bytes);
        fs::create_dir_all(root.join(BLOBS_DIR)).unwrap();
        fs::write(blob_path(root, &id), bytes).unwrap();
        id
    }

    #[test]
    fn missing_index_is_rebuilt_from_verified_blobs() {
        let root = TestDir::new();
        let id = write_blob(root.path(), b"one blob");

        let index = Index::open(root.path()).unwrap();

        let meta = index.get(&id).unwrap();
        assert_eq!(meta.id, id);
        assert_eq!(meta.size, 8);
        assert_eq!(meta.kind, ArtifactKind::File);
        assert_eq!(meta.mime, "application/octet-stream");
        assert!(meta.pinned_at.is_none());
        assert!(root.path().join(INDEX_FILE).is_file());
    }

    #[test]
    fn corrupt_index_is_rebuilt_and_persisted() {
        let root = TestDir::new();
        let id = write_blob(root.path(), b"recover me");
        fs::write(root.path().join(INDEX_FILE), b"{ definitely not json").unwrap();

        let recovered = Index::open(root.path()).unwrap();
        assert_eq!(recovered.get(&id).unwrap().id, id);

        let reopened = Index::open(root.path()).unwrap();
        assert_eq!(reopened.get(&id).unwrap().id, id);
    }

    #[test]
    fn recovery_drops_blob_whose_bytes_do_not_match_its_name() {
        let root = TestDir::new();
        let expected = write_blob(root.path(), b"original bytes");
        fs::write(blob_path(root.path(), &expected), b"tampered bytes").unwrap();
        fs::write(root.path().join(INDEX_FILE), b"invalid").unwrap();

        let recovered = Index::open(root.path()).unwrap();

        assert!(recovered.get(&expected).is_none());
        assert!(!blob_path(root.path(), &expected).exists());
        let persisted: Index =
            serde_json::from_slice(&fs::read(root.path().join(INDEX_FILE)).unwrap()).unwrap();
        assert!(persisted.get(&expected).is_none());
    }

    #[test]
    fn atomic_write_replaces_the_complete_index() {
        let root = TestDir::new();
        let id = write_blob(root.path(), b"indexed");
        let mut index = Index::default();
        index.insert(ArtifactMeta {
            id: id.clone(),
            kind: ArtifactKind::Image,
            name: "shot.png".to_owned(),
            mime: "image/png".to_owned(),
            size: 7,
            created_at: DateTime::<Utc>::from(SystemTime::UNIX_EPOCH),
            pinned_at: None,
        });

        index.write(root.path()).unwrap();
        let reopened = Index::open(root.path()).unwrap();

        assert_eq!(reopened.get(&id), index.get(&id));
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0
        );
    }
}
