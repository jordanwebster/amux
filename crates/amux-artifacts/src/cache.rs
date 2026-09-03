use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::index::{BLOBS_DIR, INDEX_FILE, atomic_write, blob_path, recover_artifacts};
use crate::{ArtifactId, ArtifactMeta, Clock, FetchError, StoreError, id_of};

/// A byte-bounded artifact cache shared by every agent on a viewing host.
pub struct Cache {
    root: PathBuf,
    clock: Arc<dyn Clock>,
    bound: u64,
    index: Mutex<CacheIndex>,
}

impl Cache {
    /// Opens a cache, recovering its persisted index and enforcing `bound`.
    pub fn open(root: PathBuf, bound: u64, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        let mut index = CacheIndex::open(&root)?;
        let root = fs::canonicalize(root)?;
        let evicted = index.evict_to(bound);
        if !evicted.is_empty() {
            index.write(&root)?;
            remove_blobs(&root, &evicted)?;
        }
        Ok(Self {
            root,
            clock,
            bound,
            index: Mutex::new(index),
        })
    }

    /// Returns verified cached bytes or fetches, verifies, and stores them.
    pub async fn get<F>(
        &self,
        id: &ArtifactId,
        fetch: F,
    ) -> Result<(ArtifactMeta, Vec<u8>), StoreError>
    where
        F: Future<Output = Result<(ArtifactMeta, Vec<u8>), FetchError>>,
    {
        if let Some(hit) = self.read_hit(id)? {
            return Ok(hit);
        }

        let (meta, bytes) = fetch.await?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if meta.id != *id || meta.size != size || id_of(&bytes) != *id {
            return Err(StoreError::Corrupt { id: id.clone() });
        }

        let mut index = self.index();
        atomic_write(&blob_path(&self.root, id), &bytes)?;
        let mut updated = index.clone();
        updated.insert(meta.clone(), self.clock.now());
        let evicted = updated.evict_to(self.bound);
        updated.write(&self.root)?;
        *index = updated;
        remove_blobs(&self.root, &evicted)?;
        Ok((meta, bytes))
    }

    /// Returns the path of a cached blob, or `Missing` if it is no longer cached.
    pub fn path_of(&self, id: &ArtifactId) -> Result<PathBuf, StoreError> {
        let path = blob_path(&self.root, id);
        if self.index().get(id).is_some() && path.is_file() {
            Ok(path)
        } else {
            Err(StoreError::Missing { id: id.clone() })
        }
    }

    fn read_hit(&self, id: &ArtifactId) -> Result<Option<(ArtifactMeta, Vec<u8>)>, StoreError> {
        let mut index = self.index();
        let Some(record) = index.get(id).cloned() else {
            return Ok(None);
        };
        let bytes = match fs::read(blob_path(&self.root, id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if id_of(&bytes) != *id || record.meta.size != size {
            return Ok(None);
        }

        let mut updated = index.clone();
        updated
            .get_mut(id)
            .expect("the cached record was cloned")
            .last_used = self.clock.now();
        updated.write(&self.root)?;
        *index = updated;
        Ok(Some((record.meta, bytes)))
    }

    fn index(&self) -> MutexGuard<'_, CacheIndex> {
        self.index.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheRecord {
    meta: ArtifactMeta,
    last_used: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CacheIndex {
    artifacts: BTreeMap<ArtifactId, CacheRecord>,
}

impl CacheIndex {
    fn open(root: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(root.join(BLOBS_DIR))?;
        match fs::read(root.join(INDEX_FILE)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(index) if Self::is_valid(&index) => Ok(index),
                Ok(_) | Err(_) => Self::recover(root),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::recover(root),
            Err(error) => Err(error.into()),
        }
    }

    fn recover(root: &Path) -> Result<Self, StoreError> {
        let mut index = Self::default();
        for meta in recover_artifacts(root)? {
            let last_used = meta.created_at;
            index.insert(meta, last_used);
        }
        index.write(root)?;
        Ok(index)
    }

    fn write(&self, root: &Path) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        atomic_write(&root.join(INDEX_FILE), &bytes)?;
        Ok(())
    }

    fn get(&self, id: &ArtifactId) -> Option<&CacheRecord> {
        self.artifacts.get(id)
    }

    fn get_mut(&mut self, id: &ArtifactId) -> Option<&mut CacheRecord> {
        self.artifacts.get_mut(id)
    }

    fn insert(&mut self, meta: ArtifactMeta, last_used: DateTime<Utc>) {
        self.artifacts
            .insert(meta.id.clone(), CacheRecord { meta, last_used });
    }

    fn evict_to(&mut self, bound: u64) -> Vec<ArtifactId> {
        let mut total = self.artifacts.values().fold(0_u64, |total, record| {
            total.saturating_add(record.meta.size)
        });
        let mut evicted = Vec::new();
        while total > bound {
            let Some(id) = self
                .artifacts
                .iter()
                .min_by_key(|(id, record)| (record.last_used, *id))
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            let record = self
                .artifacts
                .remove(&id)
                .expect("the least-recently-used record exists");
            total = total.saturating_sub(record.meta.size);
            evicted.push(id);
        }
        evicted
    }

    fn is_valid(index: &Self) -> bool {
        index
            .artifacts
            .iter()
            .all(|(id, record)| id == &record.meta.id)
    }
}

fn remove_blobs(root: &Path, ids: &[ArtifactId]) -> Result<(), StoreError> {
    for id in ids {
        match fs::remove_file(blob_path(root, id)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use chrono::DateTime;

    use super::*;
    use crate::{ArtifactKind, SystemClock};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "amux-artifacts-cache-test-{}-{}",
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

    fn meta(bytes: &[u8], name: &str) -> ArtifactMeta {
        ArtifactMeta {
            id: id_of(bytes),
            kind: ArtifactKind::File,
            name: name.to_owned(),
            mime: "application/octet-stream".to_owned(),
            size: bytes.len() as u64,
            created_at: at("2026-09-03T08:00:00Z"),
            pinned_at: Some(at("2026-09-03T08:00:01Z")),
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
        let mut context = Context::from_waker(Waker::noop());
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn miss_fetches_once_and_hit_does_not_poll_fetch() {
        let root = TestDir::new();
        let cache = Cache::open(root.path().join("cache"), 1024, Arc::new(SystemClock)).unwrap();
        let bytes = b"cached".to_vec();
        let meta = meta(&bytes, "cached");
        let fetches = AtomicUsize::new(0);

        let first = block_on(cache.get(&meta.id, async {
            fetches.fetch_add(1, Ordering::SeqCst);
            Ok((meta.clone(), bytes.clone()))
        }))
        .unwrap();
        let second = block_on(cache.get(&meta.id, async {
            fetches.fetch_add(1, Ordering::SeqCst);
            Err(FetchError::new("a cache hit must not fetch"))
        }))
        .unwrap();

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
        assert_eq!(
            cache.path_of(&meta.id).unwrap(),
            cache.root.join("blobs").join(meta.id.hex())
        );
    }

    #[test]
    fn tampered_blob_is_refetched_once() {
        let root = TestDir::new();
        let cache = Cache::open(root.path().join("cache"), 1024, Arc::new(SystemClock)).unwrap();
        let bytes = b"original".to_vec();
        let meta = meta(&bytes, "original");
        block_on(cache.get(&meta.id, async { Ok((meta.clone(), bytes.clone())) })).unwrap();
        fs::write(cache.path_of(&meta.id).unwrap(), b"tampered").unwrap();
        let fetches = AtomicUsize::new(0);

        let (_, restored) = block_on(cache.get(&meta.id, async {
            fetches.fetch_add(1, Ordering::SeqCst);
            Ok((meta.clone(), bytes.clone()))
        }))
        .unwrap();

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(restored, bytes);
        assert_eq!(fs::read(cache.path_of(&meta.id).unwrap()).unwrap(), bytes);
    }

    #[test]
    fn fetched_hash_mismatch_is_corrupt_and_not_stored() {
        let root = TestDir::new();
        let cache = Cache::open(root.path().join("cache"), 1024, Arc::new(SystemClock)).unwrap();
        let expected = meta(b"expected", "expected");

        let error = block_on(cache.get(&expected.id, async {
            Ok((expected.clone(), b"different".to_vec()))
        }))
        .unwrap_err();

        assert!(matches!(
            error,
            StoreError::Corrupt { id } if id == expected.id
        ));
        assert!(matches!(
            cache.path_of(&expected.id),
            Err(StoreError::Missing { .. })
        ));
    }

    #[test]
    fn crossing_bound_evicts_the_least_recently_used_blob() {
        let root = TestDir::new();
        let cache_root = root.path().join("cache");
        let clock = Arc::new(TestClock::new(at("2026-09-03T08:00:00Z")));
        let cache = Cache::open(cache_root.clone(), 6, clock.clone()).unwrap();
        let one = meta(b"one", "one");
        let two = meta(b"two", "two");
        let three = meta(b"six", "three");
        block_on(cache.get(&one.id, async { Ok((one.clone(), b"one".to_vec())) })).unwrap();
        clock.set(at("2026-09-03T08:00:01Z"));
        block_on(cache.get(&two.id, async { Ok((two.clone(), b"two".to_vec())) })).unwrap();
        clock.set(at("2026-09-03T08:00:02Z"));
        block_on(cache.get(&one.id, async {
            Err(FetchError::new("one should be cached"))
        }))
        .unwrap();
        drop(cache);
        clock.set(at("2026-09-03T08:00:03Z"));
        let reopened = Cache::open(cache_root, 6, clock).unwrap();

        block_on(reopened.get(&three.id, async { Ok((three.clone(), b"six".to_vec())) })).unwrap();

        assert!(reopened.path_of(&one.id).is_ok());
        assert!(matches!(
            reopened.path_of(&two.id),
            Err(StoreError::Missing { .. })
        ));
        assert!(reopened.path_of(&three.id).is_ok());
    }

    #[test]
    fn reopened_cache_serves_persisted_blob_without_fetching() {
        let root = TestDir::new();
        let cache_root = root.path().join("cache");
        let bytes = b"persisted".to_vec();
        let meta = meta(&bytes, "persisted");
        let cache = Cache::open(cache_root.clone(), 1024, Arc::new(SystemClock)).unwrap();
        block_on(cache.get(&meta.id, async { Ok((meta.clone(), bytes.clone())) })).unwrap();
        drop(cache);

        let reopened = Cache::open(cache_root, 1024, Arc::new(SystemClock)).unwrap();
        let fetches = AtomicUsize::new(0);
        let actual = block_on(reopened.get(&meta.id, async {
            fetches.fetch_add(1, Ordering::SeqCst);
            Err(FetchError::new("persisted blob should be cached"))
        }))
        .unwrap();

        assert_eq!(fetches.load(Ordering::SeqCst), 0);
        assert_eq!(actual, (meta, bytes));
    }
}
