//! The installation: one data directory, one daemon.
//!
//! ```text
//! <data_dir>/
//!   installation.lock     exclusive; one daemon per data dir
//!   registry              which profiles exist
//!   generation            boot id, clean-shutdown flag, generation counter
//!   reports/              debug bundles
//!   profiles/<profile_id>/
//!     host_id             this profile's host id, written once
//!     store.sqlite        the profile store
//!     agents/<agent_id>/  one directory per agent, the agent's contract
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub const INSTALLATION_LOCK: &str = "installation.lock";
pub const REGISTRY: &str = "registry";
pub const GENERATION: &str = "generation";
pub const REPORTS: &str = "reports";
pub const PROFILES: &str = "profiles";
pub const HOST_ID: &str = "host_id";
pub const STORE: &str = "store.sqlite";
pub const AGENTS: &str = "agents";

/// The installation's exclusive lock, held for the daemon's lifetime. The
/// file is never removed: unlinking it would let a second opener lock a
/// different inode while this one is still held.
#[derive(Debug)]
pub struct InstallationLock {
    file: Option<File>,
    path: PathBuf,
}

impl InstallationLock {
    /// Takes `<data_dir>/installation.lock`, creating the directory as
    /// needed, or reports that another daemon holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        private_dir(data_dir).map_err(LockError::Io)?;
        let path = data_dir.join(INSTALLATION_LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file: Some(file),
                path,
            }),
            Err(fs::TryLockError::WouldBlock) => Err(LockError::Busy(path)),
            Err(fs::TryLockError::Error(error)) => Err(LockError::Io(error)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // Unlock explicitly: a descriptor a concurrently forked child
        // inherited would otherwise keep the lock until that child execs.
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another amux daemon holds {}", .0.display())]
    Busy(PathBuf),
    #[error("taking the installation lock: {0}")]
    Io(io::Error),
}

/// Creates a directory, and its parents, only this user can enter.
pub fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Replaces `path` with `bytes` so that a crash or power cut leaves either
/// the old file or the new one, and the new one is on disk before this
/// returns: a temporary file, fsync, rename, fsync of the directory.
pub fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a durable file needs a parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("a durable file needs a name"))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    sync_dir(dir)
}

/// Makes a rename in `dir` durable.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows makes a rename durable with the file's own flush; a directory
/// cannot be opened for one.
#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
