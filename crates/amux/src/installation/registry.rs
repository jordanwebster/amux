//! Durable profile intent. Runtime observations and credentials are stored elsewhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::InstallationError;
pub use super::binding::{AccountId, Binding};
use super::paths::{private_directory, reject_symlink};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(pub Uuid);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileLabel {
    pub account_name: Option<String>,
    pub email: Option<String>,
    pub override_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRecord {
    pub id: ProfileId,
    pub label: ProfileLabel,
    pub binding: Option<Binding>,
    pub paused: bool,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub enum InstallationRoot {
    OnDisk(PathBuf),
    InMemory,
}

/// Owns the lock descriptor; never unlink the lock file, which would let a new
/// opener lock a different inode while an earlier supervisor still owns this one.
#[derive(Debug)]
struct LockedRoot {
    path: PathBuf,
    _lock: File,
}

impl LockedRoot {
    fn open(path: &Path) -> Result<Self, InstallationError> {
        // Resolve aliases before locking, so relative paths and symlinked parents
        // cannot give the same installation a second owner.
        if !path.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
        }
        let path = fs::canonicalize(path)?;
        private_directory(&path)?;
        let lock_path = path.join("lock");
        reject_symlink(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lock = options.open(&lock_path)?;
        if !lock.metadata()?.is_file() {
            return Err(InstallationError::InvalidPath(lock_path));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor stays open and owned by this guard.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return Err(InstallationError::RootBusy(path));
                }
                return Err(error.into());
            }
            use std::os::unix::fs::PermissionsExt;
            lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(InstallationError::RootBusy(path));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        Ok(Self { path, _lock: lock })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    profiles: Vec<ProfileRecord>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    deleting: BTreeSet<ProfileId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    credentials: BTreeMap<ProfileId, Uuid>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    logged_out: BTreeSet<ProfileId>,
}

/// A single writer under the installation root lock. Mutations persist a full
/// replacement before publishing a new in-memory snapshot.
#[derive(Debug)]
pub struct Registry {
    root: Option<LockedRoot>,
    records: BTreeMap<ProfileId, ProfileRecord>,
    deleting: BTreeSet<ProfileId>,
    credentials: BTreeMap<ProfileId, Uuid>,
    logged_out: BTreeSet<ProfileId>,
}

impl Registry {
    pub fn open(root: InstallationRoot) -> Result<Self, InstallationError> {
        let InstallationRoot::OnDisk(path) = root else {
            return Ok(Self {
                root: None,
                records: BTreeMap::new(),
                deleting: BTreeSet::new(),
                credentials: BTreeMap::new(),
                logged_out: BTreeSet::new(),
            });
        };
        let root = LockedRoot::open(&path)?;
        let path = root.path.join("registry.yaml");
        reject_symlink(&path)?;
        let data = match fs::read(&path) {
            Ok(bytes) => serde_yaml::from_slice::<RegistryFile>(&bytes)
                .map_err(|error| InstallationError::Registry(error.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => RegistryFile::default(),
            Err(error) => return Err(error.into()),
        };
        let mut records = BTreeMap::new();
        for record in data.profiles {
            if record.revision == 0 {
                return Err(InstallationError::Registry(
                    "revision must be positive".into(),
                ));
            }
            if records.insert(record.id, record).is_some() {
                return Err(InstallationError::Registry("duplicate profile id".into()));
            }
        }
        if data.deleting.iter().any(|id| !records.contains_key(id)) {
            return Err(InstallationError::Registry(
                "deletion intent names an unknown profile".into(),
            ));
        }
        validate_accounts(&records, &data.credentials)?;
        Ok(Self {
            root: Some(root),
            records,
            deleting: data.deleting,
            credentials: data.credentials,
            logged_out: data.logged_out,
        })
    }

    /// Canonical disk root, or None for an entirely in-memory registry.
    pub fn path(&self) -> Option<&Path> {
        self.root.as_ref().map(|root| root.path.as_path())
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ProfileRecord> {
        self.records.values()
    }

    pub fn get(&self, id: ProfileId) -> Result<&ProfileRecord, InstallationError> {
        self.records
            .get(&id)
            .ok_or(InstallationError::UnknownProfile(id))
    }

    pub fn create(
        &mut self,
        id: ProfileId,
        label: ProfileLabel,
    ) -> Result<ProfileRecord, InstallationError> {
        if self.records.contains_key(&id) {
            return Err(InstallationError::Registry(format!(
                "profile {id} already exists"
            )));
        }
        let record = ProfileRecord {
            id,
            label,
            binding: None,
            paused: false,
            revision: 1,
        };
        let mut candidate = self.records.clone();
        candidate.insert(id, record.clone());
        self.commit(candidate, self.deleting.clone())?;
        Ok(record)
    }

    /// Replace only the revision the caller read. Other profiles keep their revisions.
    pub fn replace(
        &mut self,
        mut record: ProfileRecord,
    ) -> Result<ProfileRecord, InstallationError> {
        self.check_revision(record.id, record.revision)?;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| InstallationError::Registry("profile revision exhausted".into()))?;
        let mut candidate = self.records.clone();
        candidate.insert(record.id, record.clone());
        self.commit(candidate, self.deleting.clone())?;
        Ok(record)
    }

    pub fn remove(
        &mut self,
        id: ProfileId,
        expected_revision: u64,
    ) -> Result<(), InstallationError> {
        self.check_revision(id, expected_revision)?;
        let mut candidate = self.records.clone();
        candidate.remove(&id);
        let mut deleting = self.deleting.clone();
        deleting.remove(&id);
        self.commit(candidate, deleting)
    }

    pub(crate) fn is_deleting(&self, id: ProfileId) -> bool {
        self.deleting.contains(&id)
    }

    /// Persist unavailability before stopping services or removing any files.
    /// An interrupted cleanup must never restart as a fresh device identity.
    pub(crate) fn mark_deleting(
        &mut self,
        id: ProfileId,
        revision: u64,
    ) -> Result<(), InstallationError> {
        self.check_revision(id, revision)?;
        let mut deleting = self.deleting.clone();
        deleting.insert(id);
        self.commit(self.records.clone(), deleting)
    }

    fn check_revision(&self, id: ProfileId, expected: u64) -> Result<(), InstallationError> {
        let actual = self.get(id)?.revision;
        if expected != actual {
            return Err(InstallationError::RevisionMismatch { expected, actual });
        }
        Ok(())
    }

    fn commit(
        &mut self,
        records: BTreeMap<ProfileId, ProfileRecord>,
        deleting: BTreeSet<ProfileId>,
    ) -> Result<(), InstallationError> {
        let mut credentials = self.credentials.clone();
        credentials.retain(|id, _| records.contains_key(id));
        let mut logged_out = self.logged_out.clone();
        logged_out.retain(|id| records.contains_key(id));
        self.commit_with_credentials(records, deleting, credentials, logged_out)
    }

    pub(crate) fn is_logged_out(&self, id: ProfileId) -> bool {
        self.logged_out.contains(&id)
    }

    pub(crate) fn credential_version(&self, id: ProfileId) -> Option<Uuid> {
        self.credentials.get(&id).copied()
    }

    pub(crate) fn commit_binding(
        &mut self,
        mut record: ProfileRecord,
        version: Option<Uuid>,
    ) -> Result<ProfileRecord, InstallationError> {
        self.check_revision(record.id, record.revision)?;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or_else(|| InstallationError::Registry("profile revision exhausted".into()))?;
        let mut records = self.records.clone();
        records.insert(record.id, record.clone());
        let mut credentials = self.credentials.clone();
        match version {
            Some(version) => {
                credentials.insert(record.id, version);
            }
            None => {
                credentials.remove(&record.id);
            }
        }
        let mut logged_out = self.logged_out.clone();
        if version.is_some() {
            logged_out.remove(&record.id);
        } else {
            logged_out.insert(record.id);
        }
        self.commit_with_credentials(records, self.deleting.clone(), credentials, logged_out)?;
        Ok(record)
    }

    fn commit_with_credentials(
        &mut self,
        records: BTreeMap<ProfileId, ProfileRecord>,
        deleting: BTreeSet<ProfileId>,
        credentials: BTreeMap<ProfileId, Uuid>,
        logged_out: BTreeSet<ProfileId>,
    ) -> Result<(), InstallationError> {
        validate_accounts(&records, &credentials)?;
        if let Some(root) = &self.root {
            let bytes = serde_yaml::to_string(&RegistryFile {
                profiles: records.values().cloned().collect(),
                deleting: deleting.clone(),
                credentials: credentials.clone(),
                logged_out: logged_out.clone(),
            })
            .map_err(|error| InstallationError::Registry(error.to_string()))?;
            let path = root.path.join("registry.yaml");
            reject_symlink(&path)?;
            // NamedTempFile creates a private, unique file on the same filesystem
            // and removes it on any failure before the atomic rename.
            let mut staged = tempfile::NamedTempFile::new_in(&root.path)?;
            staged.write_all(bytes.as_bytes())?;
            staged.as_file().sync_all()?;
            staged.persist(&path).map_err(|error| error.error)?;
            self.records = records;
            self.deleting = deleting;
            self.credentials = credentials;
            self.logged_out = logged_out;
            // A directory-sync failure follows a committed rename. Keep memory in
            // agreement with the visible file even when durability cannot be confirmed.
            #[cfg(unix)]
            File::open(&root.path)?.sync_all()?;
        } else {
            self.records = records;
            self.deleting = deleting;
            self.credentials = credentials;
            self.logged_out = logged_out;
        }
        Ok(())
    }
}

fn validate_accounts(
    records: &BTreeMap<ProfileId, ProfileRecord>,
    credentials: &BTreeMap<ProfileId, Uuid>,
) -> Result<(), InstallationError> {
    let mut accounts = BTreeSet::new();
    for record in records.values() {
        if let Some(binding) = &record.binding
            && (binding.account.subject.trim().is_empty()
                || !accounts.insert((
                    binding.account.service.clone(),
                    binding.account.subject.clone(),
                )))
        {
            return Err(InstallationError::Registry(
                "missing or duplicate account subject".into(),
            ));
        }
    }
    if credentials
        .keys()
        .any(|id| records.get(id).is_none_or(|r| r.binding.is_none()))
    {
        return Err(InstallationError::Registry(
            "credential version has no bound profile".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
