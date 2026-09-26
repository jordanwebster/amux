//! The files beside the binary that record an update: `<binary>.prev`, the
//! binary that was running before the swap, whose presence means an update
//! is not yet activated; `<binary>.rejected`, the version of the last build
//! that was rolled back; and `<binary>.staged`, a download not yet
//! installed.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use super::sync_parent;
use crate::install::write_durably;

/// The supervisor's lock file in the data directory; it holds the pid.
pub const SUPERVISOR_LOCK: &str = "supervisor.lock";

#[derive(Clone, Debug)]
pub struct Files {
    pub prev: PathBuf,
    pub rejected: PathBuf,
    pub staged: PathBuf,
    /// Where a Windows supervisor moves prev when it is its own running
    /// executable, which cannot be deleted; removed at the next start.
    pub aside: PathBuf,
}

fn beside(binary: &Path, suffix: &str) -> PathBuf {
    let mut name = binary
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("amux"));
    name.push(suffix);
    binary.with_file_name(name)
}

impl Files {
    pub fn beside(binary: &Path) -> Self {
        Self {
            prev: beside(binary, ".prev"),
            rejected: beside(binary, ".rejected"),
            staged: beside(binary, ".staged"),
            aside: beside(binary, ".old"),
        }
    }

    pub fn prev_exists(&self) -> bool {
        self.prev.symlink_metadata().is_ok()
    }

    /// Whether prev is the same file as `binary`: a hard link made by a swap
    /// that never renamed over the path.
    pub fn prev_is(&self, binary: &Path) -> bool {
        matches!((FileId::of(&self.prev), FileId::of(binary)), (Ok(prev), Ok(binary)) if prev == binary)
    }

    /// The rolled-back build's version, if one was recorded.
    pub fn rejected(&self) -> Option<Version> {
        let text = std::fs::read_to_string(&self.rejected).ok()?;
        Version::parse(text.trim()).ok()
    }

    /// Records the rolled-back build, durably, before prev moves.
    pub fn write_rejected(&self, version: &Version) -> io::Result<()> {
        write_durably(&self.rejected, version.to_string().as_bytes())
    }

    pub fn remove_prev(&self) -> io::Result<()> {
        std::fs::remove_file(&self.prev)?;
        sync_parent(&self.prev)
    }

    /// Activation's cleanup: prev is no longer needed.
    pub fn retire_prev(&self, mine: &FileId) -> io::Result<()> {
        if cfg!(windows) && FileId::of(&self.prev).is_ok_and(|prev| prev == *mine) {
            std::fs::rename(&self.prev, &self.aside)?;
            return sync_parent(&self.prev);
        }
        self.remove_prev()
    }

    /// Renames prev back over the binary.
    pub fn restore_prev(&self, binary: &Path) -> io::Result<()> {
        std::fs::rename(&self.prev, binary)?;
        sync_parent(binary)
    }

    /// Puts the verified staged binary at the path, keeping the current one
    /// as prev. On Unix the path is never missing: prev is a hard link and
    /// the staged file is renamed over the path. On Windows, where the
    /// running file cannot be replaced, the current one is renamed aside to
    /// prev first.
    pub fn install_staged(&self, binary: &Path) -> io::Result<()> {
        if self.prev_exists() {
            std::fs::remove_file(&self.prev)?;
        }
        #[cfg(unix)]
        std::fs::hard_link(binary, &self.prev)?;
        #[cfg(windows)]
        std::fs::rename(binary, &self.prev)?;
        std::fs::rename(&self.staged, binary)?;
        sync_parent(binary)
    }

    /// What an interrupted download or a Windows update leaves behind.
    pub fn clear_leftovers(&self) {
        let _ = std::fs::remove_file(&self.staged);
        let _ = std::fs::remove_file(&self.aside);
    }
}

pub fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A file's identity: device and inode on Unix, volume and file index on
/// Windows. Two paths with one identity are hard links to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    device: u64,
    file: u64,
}

impl FileId {
    #[cfg(unix)]
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            file: metadata.ino(),
        })
    }

    #[cfg(windows)]
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;

        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let file = File::open(path)?;
        // SAFETY: an all-zero struct is a valid out parameter.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: querying a handle we own into a struct we own.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            device: u64::from(info.dwVolumeSerialNumber),
            file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

/// A descriptor the supervisor that exec'd this one handed over.
#[cfg(unix)]
pub fn inherited_file(fd: u64) -> io::Result<File> {
    use std::os::fd::{FromRawFd as _, RawFd};
    let fd = RawFd::try_from(fd).map_err(io::Error::other)?;
    // SAFETY: fcntl on a descriptor handed over to this process.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor was handed over and nothing else owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Windows supervisors never hand their lock over; the next one takes it.
#[cfg(windows)]
pub fn inherited_file(_fd: u64) -> io::Result<File> {
    Err(io::Error::other(
        "a Windows supervisor takes its lock again",
    ))
}
