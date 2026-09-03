use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::index::{Index, atomic_write, blob_path};
use crate::{ARTIFACT_SIZE_CAP, ArtifactId, ArtifactKind, ArtifactMeta, Clock, StoreError, id_of};

/// The authoritative artifact store for one agent.
pub struct Owner {
    root: PathBuf,
    clock: Arc<dyn Clock>,
    index: Mutex<Index>,
}

impl Owner {
    /// Opens an owner and loads its index into memory.
    pub fn open(root: PathBuf, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        let index = Index::open(&root)?;
        let root = fs::canonicalize(root)?;
        Ok(Self {
            root,
            clock,
            index: Mutex::new(index),
        })
    }

    /// Stores an ephemeral artifact, retaining the first metadata recorded for its bytes.
    pub fn put(
        &self,
        kind: ArtifactKind,
        name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactMeta, StoreError> {
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if size > ARTIFACT_SIZE_CAP {
            return Err(StoreError::TooLarge {
                size,
                max: ARTIFACT_SIZE_CAP,
            });
        }

        let id = id_of(bytes);
        let mut index = self.index();
        if let Some(meta) = index.get(&id) {
            return Ok(meta.clone());
        }

        let meta = ArtifactMeta {
            id: id.clone(),
            kind,
            name: name.to_owned(),
            mime: mime.to_owned(),
            size,
            created_at: self.clock.now(),
            pinned_at: None,
        };
        atomic_write(&blob_path(&self.root, &id), bytes)?;

        let mut updated = index.clone();
        updated.insert(meta.clone());
        updated.write(&self.root)?;
        *index = updated;
        Ok(meta)
    }

    /// Returns metadata from the in-memory index without touching disk.
    pub fn meta(&self, id: &ArtifactId) -> Result<ArtifactMeta, StoreError> {
        self.index()
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::Missing { id: id.clone() })
    }

    /// Reads and verifies an indexed artifact.
    pub fn get(&self, id: &ArtifactId) -> Result<(ArtifactMeta, Vec<u8>), StoreError> {
        let index = self.index();
        let meta = index
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::Missing { id: id.clone() })?;
        let bytes = match fs::read(blob_path(&self.root, id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::Corrupt { id: id.clone() });
            }
            Err(error) => return Err(error.into()),
        };
        if id_of(&bytes) != *id {
            return Err(StoreError::Corrupt { id: id.clone() });
        }
        Ok((meta, bytes))
    }

    /// Pins every listed artifact atomically as of the current clock time.
    pub fn pin(&self, ids: &[ArtifactId]) -> Result<Vec<ArtifactMeta>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut index = self.index();
        for id in ids {
            if index.get(id).is_none() {
                return Err(StoreError::Missing { id: id.clone() });
            }
        }

        let pinned_at = self.clock.now();
        let mut updated = index.clone();
        for id in ids {
            updated
                .get_mut(id)
                .expect("all artifact ids were checked")
                .pinned_at = Some(pinned_at);
        }
        updated.write(&self.root)?;
        let pinned = ids
            .iter()
            .map(|id| {
                updated
                    .get(id)
                    .expect("all artifact ids were checked")
                    .clone()
            })
            .collect();
        *index = updated;
        Ok(pinned)
    }

    /// Returns the absolute path where an artifact's bytes belong.
    pub fn path_of(&self, id: &ArtifactId) -> PathBuf {
        blob_path(&self.root, id)
    }

    /// Removes expired ephemeral artifacts from the loaded index and from disk.
    pub fn sweep(&self, ttl: Duration) -> Result<Vec<ArtifactId>, StoreError> {
        let now = self.clock.now();
        let mut index = self.index();
        let expired: Vec<_> = index
            .ordered()
            .filter(|meta| {
                meta.pinned_at.is_none()
                    && now
                        .signed_duration_since(meta.created_at)
                        .to_std()
                        .is_ok_and(|age| age > ttl)
            })
            .map(|meta| meta.id.clone())
            .collect();
        if expired.is_empty() {
            return Ok(expired);
        }

        let mut updated = index.clone();
        for id in &expired {
            updated.remove(id);
        }
        updated.write(&self.root)?;
        *index = updated;
        for id in &expired {
            match fs::remove_file(blob_path(&self.root, id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(expired)
    }

    /// Returns pinned artifacts in their original creation order.
    pub fn pinned(&self) -> Vec<ArtifactMeta> {
        self.index()
            .ordered()
            .filter(|meta| meta.pinned_at.is_some())
            .cloned()
            .collect()
    }

    /// Deletes this owner's complete artifact directory.
    pub fn delete_all(self) -> Result<(), StoreError> {
        match fs::remove_dir_all(self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn index(&self) -> MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::{EPHEMERAL_TTL, SystemClock};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "amux-artifacts-owner-test-{}-{}",
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

    struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self(Mutex::new(now))
        }

        fn set(&self, now: DateTime<Utc>) {
            *self.0.lock().unwrap() = now;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn open(root: &TestDir, clock: Arc<dyn Clock>) -> Owner {
        Owner::open(root.path().join("store"), clock).unwrap()
    }

    #[test]
    fn put_is_idempotent_and_keeps_the_first_metadata_and_lifetime() {
        let root = TestDir::new();
        let clock = Arc::new(TestClock::new(at("2026-09-03T08:00:00Z")));
        let owner = open(&root, clock.clone());
        let first = owner
            .put(ArtifactKind::Image, "first.png", "image/png", b"same")
            .unwrap();
        clock.set(at("2026-09-03T09:00:00Z"));
        let pinned = owner
            .pin(std::slice::from_ref(&first.id))
            .unwrap()
            .remove(0);
        clock.set(at("2026-09-03T10:00:00Z"));

        let repeated = owner
            .put(ArtifactKind::File, "later.txt", "text/plain", b"same")
            .unwrap();

        assert_eq!(repeated, pinned);
        assert_eq!(repeated.name, "first.png");
        assert_eq!(repeated.created_at, first.created_at);
        assert_eq!(repeated.pinned_at, Some(at("2026-09-03T09:00:00Z")));
    }

    #[test]
    fn oversized_put_is_rejected_before_a_blob_is_written() {
        let root = TestDir::new();
        let owner = open(&root, Arc::new(SystemClock));
        let bytes = vec![0; usize::try_from(ARTIFACT_SIZE_CAP + 1).unwrap()];

        let error = owner
            .put(
                ArtifactKind::File,
                "large",
                "application/octet-stream",
                &bytes,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            StoreError::TooLarge {
                size,
                max: ARTIFACT_SIZE_CAP
            } if size == ARTIFACT_SIZE_CAP + 1
        ));
        assert_eq!(fs::read_dir(owner.root.join("blobs")).unwrap().count(), 0);
    }

    #[test]
    fn unknown_artifact_is_missing_without_a_disk_lookup() {
        let root = TestDir::new();
        let owner = open(&root, Arc::new(SystemClock));
        let unknown = id_of(b"unknown");
        fs::remove_dir_all(owner.root.join("blobs")).unwrap();

        assert!(matches!(
            owner.meta(&unknown),
            Err(StoreError::Missing { id }) if id == unknown
        ));
        assert!(matches!(
            owner.get(&unknown),
            Err(StoreError::Missing { id }) if id == unknown
        ));
    }

    #[test]
    fn sweep_uses_loaded_index_and_preserves_pinned_and_recent_artifacts() {
        let root = TestDir::new();
        let clock = Arc::new(TestClock::new(at("2026-09-03T08:00:00Z")));
        let owner = open(&root, clock.clone());
        let expired = owner
            .put(ArtifactKind::Diff, "old.diff", "text/x-diff", b"expired")
            .unwrap();
        let pinned = owner
            .put(ArtifactKind::Image, "kept.png", "image/png", b"pinned")
            .unwrap();
        owner.pin(std::slice::from_ref(&pinned.id)).unwrap();
        clock.set(at("2026-09-03T09:00:01Z"));
        let recent = owner
            .put(ArtifactKind::File, "recent", "text/plain", b"recent")
            .unwrap();
        let unindexed = id_of(b"unindexed");
        fs::write(owner.path_of(&unindexed), b"unindexed").unwrap();

        let swept = owner.sweep(EPHEMERAL_TTL).unwrap();

        assert_eq!(swept, vec![expired.id.clone()]);
        assert!(matches!(
            owner.meta(&expired.id),
            Err(StoreError::Missing { .. })
        ));
        assert!(!owner.path_of(&expired.id).exists());
        assert_eq!(owner.get(&pinned.id).unwrap().1, b"pinned");
        assert_eq!(owner.get(&recent.id).unwrap().1, b"recent");
        assert!(owner.path_of(&unindexed).exists());
        assert_eq!(owner.pinned(), vec![owner.meta(&pinned.id).unwrap()]);
    }

    #[test]
    fn get_reports_tampered_or_missing_indexed_bytes_as_corrupt() {
        let root = TestDir::new();
        let owner = open(&root, Arc::new(SystemClock));
        let meta = owner
            .put(
                ArtifactKind::File,
                "data",
                "application/octet-stream",
                b"original",
            )
            .unwrap();

        fs::write(owner.path_of(&meta.id), b"tampered").unwrap();
        assert!(matches!(
            owner.get(&meta.id),
            Err(StoreError::Corrupt { id }) if id == meta.id
        ));

        fs::remove_file(owner.path_of(&meta.id)).unwrap();
        assert!(matches!(
            owner.get(&meta.id),
            Err(StoreError::Corrupt { id }) if id == meta.id
        ));
    }

    #[test]
    fn pin_with_a_missing_id_changes_nothing() {
        let root = TestDir::new();
        let clock = Arc::new(TestClock::new(at("2026-09-03T08:00:00Z")));
        let owner = open(&root, clock.clone());
        let stored = owner
            .put(ArtifactKind::File, "stored", "text/plain", b"stored")
            .unwrap();
        let missing = id_of(b"missing");
        clock.set(at("2026-09-03T09:00:00Z"));

        assert!(matches!(
            owner.pin(&[stored.id.clone(), missing.clone()]),
            Err(StoreError::Missing { id }) if id == missing
        ));
        assert!(owner.meta(&stored.id).unwrap().pinned_at.is_none());
        drop(owner);

        let reopened = open(&root, clock);
        assert!(reopened.meta(&stored.id).unwrap().pinned_at.is_none());
    }

    #[test]
    fn open_loads_once_and_path_is_absolute() {
        let root = TestDir::new();
        let owner = open(&root, Arc::new(SystemClock));
        let meta = owner
            .put(ArtifactKind::File, "data", "text/plain", b"memory")
            .unwrap();
        fs::write(owner.root.join("index.json"), b"corrupt after open").unwrap();

        assert_eq!(owner.meta(&meta.id).unwrap(), meta);
        assert!(owner.path_of(&meta.id).is_absolute());
    }

    #[test]
    fn pinned_records_retain_creation_order() {
        let root = TestDir::new();
        let clock = Arc::new(TestClock::new(at("2026-09-03T08:00:00Z")));
        let owner = open(&root, clock.clone());
        let first = owner
            .put(ArtifactKind::File, "first", "text/plain", b"z-first")
            .unwrap();
        clock.set(at("2026-09-03T08:00:01Z"));
        let second = owner
            .put(ArtifactKind::File, "second", "text/plain", b"a-second")
            .unwrap();
        owner.pin(&[second.id.clone(), first.id.clone()]).unwrap();

        assert_eq!(
            owner
                .pinned()
                .into_iter()
                .map(|meta| meta.id)
                .collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
    }

    #[test]
    fn delete_all_removes_the_owner_root() {
        let root = TestDir::new();
        let owner = open(&root, Arc::new(SystemClock));
        let owner_root = owner.root.clone();
        owner
            .put(ArtifactKind::File, "data", "text/plain", b"delete")
            .unwrap();

        owner.delete_all().unwrap();

        assert!(!owner_root.exists());
    }
}
