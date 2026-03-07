//! State management for amux.
//!
//! State is stored in `~/.local/state/amux/state.yaml` and persists across sessions.
//! Includes cloud authentication state and other persistent preferences.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

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
    /// Installed plugin version (None = not installed, triggers install)
    pub plugin_version: Option<u32>,
}

impl State {
    /// Default state path: `$XDG_STATE_HOME/amux/state.yaml`,
    /// falling back to `~/.local/state/amux/state.yaml`.
    pub fn default_path() -> PathBuf {
        crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("amux/state.yaml")
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

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_state_default() {
        let state = State::default();
        assert!(state.cloud.use_cloud_mode.is_none());
        assert!(state.cloud.refresh_token.is_none());
    }

    #[test]
    fn test_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");
        let state = State::load(&path).unwrap();
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
        assert_eq!(loaded.cloud.use_cloud_mode, Some(false));
    }

    #[test]
    fn test_state_partial_yaml() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        // Write partial YAML (only cloud section)
        fs::write(&path, "cloud:\n  use_cloud_mode: true\n").unwrap();

        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.cloud.use_cloud_mode, Some(true));
        // Claude section should be default
        assert!(loaded.claude.plugin_version.is_none());
    }
}
