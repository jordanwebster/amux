use std::fs;
use std::path::{Path, PathBuf};

use super::{InstallationError, ProfileId};

/// Filesystem locations owned by one profile runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilePaths {
    pub config_path: Option<PathBuf>,
    pub socket_path: PathBuf,
    pub state_path: PathBuf,
    pub data_dir: PathBuf,
    pub reports_dir: PathBuf,
}

impl ProfilePaths {
    pub(crate) fn allocated(root: &Path, id: ProfileId) -> Self {
        let profiles = root.join("profiles");
        let directory = profiles.join(id.to_string());
        Self {
            config_path: Some(directory.join("config.yaml")),
            socket_path: profiles.join(format!("{id}.sock")),
            state_path: directory.join("state/state.yaml"),
            data_dir: directory.join("data"),
            reports_dir: directory.join("data/reports"),
        }
    }

    /// Allocate a UUID namespace beneath an already-open installation root.
    /// No name or account subject participates in path construction.
    pub fn for_id(root: &Path, id: ProfileId) -> Result<Self, InstallationError> {
        let root = fs::canonicalize(root)?;
        let paths = Self::allocated(&root, id);
        let profiles = root.join("profiles");
        let directory = profiles.join(id.to_string());
        validate_socket_path(&paths.socket_path)?;
        // Keep the agent host's Codex socket out of the shared /tmp fallback.
        #[cfg(unix)]
        {
            let codex = adjacent_codex_socket_path(&paths.socket_path);
            use std::os::unix::ffi::OsStrExt;
            validate_socket_path(&codex)?;
            if codex.as_os_str().as_bytes().len() > MAX_CODEX_SOCKET_PATH_BYTES {
                return Err(InstallationError::SocketPathTooLong(codex));
            }
        }
        for path in [
            &profiles,
            &directory,
            &directory.join("state"),
            &paths.data_dir,
            &paths.data_dir.join("agents"),
            &paths.data_dir.join("cache"),
            &paths.data_dir.join("cache/artifacts"),
            &paths.reports_dir,
        ] {
            private_directory(path)?;
        }
        // Existing files must not redirect runtime writes out of this namespace.
        for path in [
            directory.join("config.yaml"),
            directory.join("credentials.yaml"),
            paths.state_path.clone(),
            directory.join("state/suspended.yaml"),
            paths.data_dir.join("device.key"),
            paths.data_dir.join("host_id"),
            paths.data_dir.join("trust.json"),
            paths.socket_path.clone(),
        ] {
            reject_symlink(&path)?;
        }
        #[cfg(unix)]
        reject_symlink(&adjacent_codex_socket_path(&paths.socket_path))?;
        Ok(paths)
    }

    /// The profile credential lives beside its configuration, not in shared preferences.
    pub fn credentials_path(&self) -> Option<PathBuf> {
        self.config_path
            .as_ref()
            .map(|path| path.with_file_name("credentials.yaml"))
    }
}

// Codex imposes the macOS pathname cap even on other Unix targets.
#[cfg(unix)]
pub(crate) const MAX_CODEX_SOCKET_PATH_BYTES: usize = 103;

#[cfg(unix)]
pub(crate) fn adjacent_codex_socket_path(server_socket_path: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    // Stable FNV-1a keeps servers with different configured socket paths
    // isolated without copying a potentially long filename into sun_path.
    let hash = server_socket_path
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    server_socket_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("c{hash:016x}.sock"))
}

pub(super) fn private_directory(path: &Path) -> Result<(), InstallationError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(InstallationError::InvalidPath(path.to_owned()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn reject_symlink(path: &Path) -> Result<(), InstallationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(InstallationError::InvalidPath(path.to_owned()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Check bytes, including room for the terminating NUL, against this target's sockaddr_un.
pub fn validate_socket_path(path: &Path) -> Result<(), InstallationError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = path.as_os_str().as_bytes();
        if bytes.contains(&0) {
            return Err(InstallationError::InvalidPath(path.to_owned()));
        }
        if bytes.len() >= address.sun_path.len() {
            return Err(InstallationError::SocketPathTooLong(path.to_owned()));
        }
    }
    Ok(())
}
