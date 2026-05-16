//! State management for amux.
//!
//! State is stored in `~/.local/state/amux/state.yaml` and persists across sessions.
//! Includes amux-owned integration state.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
#[serde(default, deny_unknown_fields)]
pub(crate) struct State {
    #[serde(default)]
    pub(crate) claude: ClaudeState,
}

/// Claude-specific state
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_state_default() {
        let state = State::default();
        assert!(state.claude.applied_plugin_version.is_none());
    }

    #[test]
    fn test_state_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");
        let state = State::load(&path).unwrap();
        assert!(state.claude.applied_plugin_version.is_none());
    }

    #[test]
    fn test_state_save_and_load() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        State::update(&path, |s| {
            s.claude.applied_plugin_version = Some("1.0.0".to_string());
        })
        .unwrap();

        let loaded = State::load(&path).unwrap();
        assert_eq!(
            loaded.claude.applied_plugin_version.as_deref(),
            Some("1.0.0")
        );
    }

    #[test]
    fn state_rejects_unknown_fields() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.yaml");

        fs::write(&path, "legacy:\n  ignored: abc123\n").unwrap();

        let error = State::load(&path).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
