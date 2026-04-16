//! State management for amux.
//!
//! State is stored in `~/.local/state/amux/state.yaml` and persists across sessions.
//! Includes cloud authentication state and other persistent preferences.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse state file: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Persistent state for amux
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    /// Stable host identifier for this amux state directory.
    #[serde(default)]
    pub host_id: Option<Uuid>,
    #[serde(default)]
    pub cloud: CloudState,
    #[serde(default)]
    pub claude: ClaudeState,
}

/// Cloud authentication state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudState {
    /// Whether cloud mode is enabled (None = not yet configured)
    pub use_cloud_mode: Option<bool>,
    /// OAuth refresh token for cloud authentication
    pub refresh_token: Option<String>,
}

/// Claude-specific state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeState {
    /// Plugin version last successfully applied to Claude Code.
    pub applied_plugin_version: Option<String>,
    /// Marketplace source path last successfully applied to Claude Code.
    pub applied_marketplace_path: Option<PathBuf>,
}

impl State {
    /// Default state path: `$XDG_STATE_HOME/amux/state.yaml`,
    /// falling back to `~/.local/state/amux/state.yaml`.
    pub fn default_path() -> PathBuf {
        crate::config::amux_xdg_dir("XDG_STATE_HOME", ".local/state").join("state.yaml")
    }

    /// Load state with shared lock (allows concurrent reads)
    pub fn load(path: &Path) -> Result<Self, StateError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let file = File::open(path)?;
        file.lock_shared()?;
        let mut contents = String::new();
        (&file).read_to_string(&mut contents)?;
        file.unlock()?;

        if contents.trim().is_empty() {
            return Ok(Self::default());
        }

        serde_yaml::from_str(&contents).map_err(StateError::Parse)
    }

    /// Atomic load-modify-save with exclusive lock held throughout
    pub fn update<F, T>(path: &Path, f: F) -> Result<T, StateError>
    where
        F: FnOnce(&mut State) -> T,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        opts.mode(0o600);
        let file = opts.open(path)?;
        file.lock_exclusive()?;

        let mut contents = String::new();
        (&file).read_to_string(&mut contents)?;

        let mut state: State = if contents.trim().is_empty() {
            State::default()
        } else {
            serde_yaml::from_str(&contents)?
        };

        let result = f(&mut state);

        let yaml = serde_yaml::to_string(&state)?;
        // Truncate and rewrite
        file.set_len(0)?;
        (&file).seek(SeekFrom::Start(0))?;
        (&file).write_all(yaml.as_bytes())?;
        file.unlock()?;

        Ok(result)
    }
}

/// Load the persisted host ID, creating and saving one if this is the first run.
pub fn load_or_create_host_id(path: &Path) -> Result<Uuid, StateError> {
    if let Some(host_id) = State::load(path)?.host_id {
        return Ok(host_id);
    }

    State::update(path, |state| match state.host_id {
        Some(host_id) => host_id,
        None => {
            let host_id = Uuid::new_v4();
            state.host_id = Some(host_id);
            host_id
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_state_default() {
        let state = State::default();
        assert!(state.host_id.is_none());
        assert!(state.cloud.use_cloud_mode.is_none());
        assert!(state.cloud.refresh_token.is_none());
    }

    #[test]
    fn test_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");
        let state = State::load(&path).unwrap();
        assert!(state.host_id.is_none());
        assert!(state.cloud.use_cloud_mode.is_none());
    }

    #[test]
    fn test_state_save_and_load() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        State::update(&path, |s| {
            s.cloud.use_cloud_mode = Some(true);
            s.cloud.refresh_token = Some("test_token".to_string());
        })
        .unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert_eq!(loaded.cloud.use_cloud_mode, Some(true));
        assert_eq!(loaded.cloud.refresh_token, Some("test_token".to_string()));
    }

    #[test]
    fn test_state_update() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        State::update(&path, |s| {
            s.cloud.use_cloud_mode = Some(false);
        })
        .unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert_eq!(loaded.cloud.use_cloud_mode, Some(false));
    }

    #[test]
    fn test_state_partial_yaml() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        // Write partial YAML (only cloud section)
        fs::write(&path, "cloud:\n  use_cloud_mode: true\n").unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert_eq!(loaded.cloud.use_cloud_mode, Some(true));
        // Claude section should be default
        assert!(loaded.claude.applied_plugin_version.is_none());
        assert!(loaded.claude.applied_marketplace_path.is_none());
    }

    #[test]
    fn test_load_or_create_host_id_persists_once() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        let first = load_or_create_host_id(&path).unwrap();
        let second = load_or_create_host_id(&path).unwrap();

        assert_eq!(first, second);
        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.host_id, Some(first));
    }

    #[cfg(unix)]
    #[test]
    fn test_load_or_create_host_id_reads_existing_readonly_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        let host_id = load_or_create_host_id(&path).unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o400);
        fs::set_permissions(&path, perms).unwrap();

        let loaded = load_or_create_host_id(&path).unwrap();
        assert_eq!(loaded, host_id);
    }
}
