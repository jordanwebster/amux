//! State management for amux.
//!
//! State is stored in `~/.local/state/amux/state.yaml` and persists across sessions.
//! Includes cloud authentication state and other persistent preferences.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::paths::default_state_path;

#[derive(Debug, Error)]
pub(crate) enum StateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse state file: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Persistent state for amux
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct State {
    /// Stable host identifier for this amux state directory.
    #[serde(default)]
    pub(crate) host_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) cloud: CloudState,
    #[serde(default)]
    pub(crate) claude: ClaudeState,
}

/// Cloud authentication state. Stored under `cloud:` in `state.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CloudState {
    #[default]
    NotConfigured,
    Disabled,
    Unauthenticated,
    Authenticated {
        refresh_token: String,
    },
}

impl CloudState {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Unauthenticated | Self::Authenticated { .. })
    }

    pub fn needs_init(&self) -> bool {
        matches!(self, Self::NotConfigured | Self::Unauthenticated)
    }

    pub(crate) fn refresh_token(&self) -> Option<&str> {
        match self {
            Self::Authenticated { refresh_token } => Some(refresh_token.as_str()),
            _ => None,
        }
    }
}

/// Claude-specific state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ClaudeState {
    /// Plugin version last successfully applied to Claude Code.
    pub(crate) applied_plugin_version: Option<String>,
    /// Marketplace source path last successfully applied to Claude Code.
    pub(crate) applied_marketplace_path: Option<PathBuf>,
}

impl State {
    /// Default state path: `$XDG_STATE_HOME/amux/state.yaml`,
    /// falling back to `~/.local/state/amux/state.yaml`.
    pub(crate) fn default_path() -> PathBuf {
        default_state_path()
    }

    /// Load state with shared lock (allows concurrent reads)
    pub(crate) fn load(path: &Path) -> Result<Self, StateError> {
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
    pub(crate) fn update<F, T>(path: &Path, f: F) -> Result<T, StateError>
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
pub(crate) fn load_or_create_host_id(path: &Path) -> Result<Uuid, StateError> {
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
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_state_default() {
        let state = State::default();
        assert!(state.host_id.is_none());
        assert!(matches!(state.cloud, CloudState::NotConfigured));
    }

    #[test]
    fn test_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");
        let state = State::load(&path).unwrap();
        assert!(state.host_id.is_none());
        assert!(matches!(state.cloud, CloudState::NotConfigured));
    }

    #[test]
    fn test_state_save_and_load() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        State::update(&path, |s| {
            s.cloud = CloudState::Authenticated {
                refresh_token: "test_token".to_string(),
            };
        })
        .unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert!(
            matches!(&loaded.cloud, CloudState::Authenticated { refresh_token } if refresh_token == "test_token")
        );
    }

    #[test]
    fn test_state_update() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        State::update(&path, |s| {
            s.cloud = CloudState::Disabled;
        })
        .unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert!(matches!(loaded.cloud, CloudState::Disabled));
    }

    #[test]
    fn test_state_partial_yaml() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        // Write partial YAML (only cloud section)
        fs::write(&path, "cloud:\n  status: unauthenticated\n").unwrap();

        let loaded = State::load(&path).unwrap();
        assert!(loaded.host_id.is_none());
        assert!(matches!(loaded.cloud, CloudState::Unauthenticated));
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
