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

/// Save suspended server state to `<state_dir>/suspended.yaml` (sibling of state.yaml).
pub fn save_suspended(
    state_path: &Path,
    state: &crate::agents::SuspendedServerState,
) -> Result<(), StateError> {
    let suspended_path = suspended_path(state_path);
    if let Some(parent) = suspended_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(state)?;
    fs::write(&suspended_path, yaml)?;
    tracing::info!(path = %suspended_path.display(), count = state.agents.len(), "saved suspended agents");
    Ok(())
}

/// Load suspended server state from `<state_dir>/suspended.yaml` and delete the file.
/// Returns an empty state if the file does not exist.
pub fn load_and_remove_suspended(
    state_path: &Path,
) -> Result<crate::agents::SuspendedServerState, StateError> {
    let suspended_path = suspended_path(state_path);
    if !suspended_path.exists() {
        return Ok(crate::agents::SuspendedServerState { agents: Vec::new() });
    }
    let yaml = fs::read_to_string(&suspended_path)?;
    let state: crate::agents::SuspendedServerState = serde_yaml::from_str(&yaml)?;
    fs::remove_file(&suspended_path)?;
    tracing::info!(path = %suspended_path.display(), count = state.agents.len(), "loaded and removed suspended agents");
    Ok(state)
}

/// Derive the suspended agents file path from the state file path.
fn suspended_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("suspended.yaml")
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

    #[test]
    fn test_suspended_agent_roundtrip() {
        use crate::agents::{LocalAgentNameSource, SuspendedAgent, SuspendedServerState};
        use crate::message::TerminalSize;
        use chrono::Utc;
        use uuid::Uuid;

        let state = SuspendedServerState {
            agents: vec![
                SuspendedAgent::Claude {
                    agent_id: Uuid::new_v4(),
                    name: Some("test-claude".to_string()),
                    name_source: LocalAgentNameSource::ProviderName,
                    working_dir: PathBuf::from("/home/user/project"),
                    terminal_size: Some(TerminalSize {
                        rows: 40,
                        cols: 120,
                    }),
                    session_id: Uuid::new_v4(),
                    created_at: Utc::now(),
                },
                SuspendedAgent::TestAgent {
                    agent_id: Uuid::new_v4(),
                    name: None,
                    command: "test-agent".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    terminal_size: None,
                    created_at: Utc::now(),
                },
            ],
        };

        let temp = TempDir::new().unwrap();
        let state_path = temp.path().join("state.yaml");

        save_suspended(&state_path, &state).unwrap();
        let loaded = load_and_remove_suspended(&state_path).unwrap();

        assert_eq!(loaded.agents.len(), 2);
        assert!(
            matches!(&loaded.agents[0], SuspendedAgent::Claude { name, .. } if name.as_deref() == Some("test-claude"))
        );
        assert!(
            matches!(&loaded.agents[1], SuspendedAgent::TestAgent { command, .. } if command == "test-agent")
        );

        // File should be deleted after loading
        let suspended = suspended_path(&state_path);
        assert!(!suspended.exists());
    }

    #[test]
    fn test_load_suspended_nonexistent_returns_empty() {
        let temp = TempDir::new().unwrap();
        let state_path = temp.path().join("state.yaml");

        let loaded = load_and_remove_suspended(&state_path).unwrap();
        assert!(loaded.agents.is_empty());
    }

    #[test]
    fn test_suspended_agent_into_session_roundtrip() {
        use crate::agents::{LocalAgentNameSource, SuspendedAgent};
        use chrono::Utc;
        use uuid::Uuid;

        let agent_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let sa = SuspendedAgent::Claude {
            agent_id,
            name: Some("my-claude".to_string()),
            name_source: LocalAgentNameSource::ProviderSlug,
            working_dir: PathBuf::from("/home/user"),
            terminal_size: Some(crate::message::TerminalSize {
                rows: 30,
                cols: 100,
            }),
            session_id,
            created_at: Utc::now(),
        };

        // Convert to session (don't start — no PTY needed for metadata check)
        let session = sa.into_session();
        assert_eq!(session.agent_id(), agent_id);
        assert_eq!(session.name(), Some("my-claude"));
        assert_eq!(
            session.local_name_source(),
            Some(LocalAgentNameSource::ProviderSlug)
        );
        assert_eq!(session.working_dir(), PathBuf::from("/home/user"));
    }
}
