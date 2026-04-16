use crate::protocol::message::TerminalSize;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// All suspended agent sessions, serialized to disk across server restarts.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct SuspendedServerState {
    pub(crate) agents: Vec<SuspendedAgent>,
}

/// Persisted source for a Claude agent's display name.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SuspendedLocalAgentNameSource {
    Unset,
    Amux,
    ProviderName,
    ProviderSlug,
}

/// Serializable representation of a suspended agent session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) enum SuspendedAgent {
    Claude {
        agent_id: Uuid,
        name: Option<String>,
        name_source: SuspendedLocalAgentNameSource,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
        args: Vec<String>,
        session_id: Uuid,
        created_at: DateTime<Utc>,
    },
    #[cfg(any(debug_assertions, test))]
    TestAgent {
        agent_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
        created_at: DateTime<Utc>,
    },
}

impl SuspendedAgent {
    pub(crate) fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude { agent_id, .. } => *agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { agent_id, .. } => *agent_id,
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Claude { name, .. } => name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { name, .. } => name.as_deref(),
        }
    }
}

/// Save suspended server state to `<state_dir>/suspended.yaml` (sibling of state.yaml).
pub(crate) fn save_suspended(
    state_path: &Path,
    state: &SuspendedServerState,
) -> Result<(), std::io::Error> {
    let suspended_path = suspended_path(state_path);
    if let Some(parent) = suspended_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(state).map_err(std::io::Error::other)?;

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(&suspended_path)?;
    file.write_all(yaml.as_bytes())?;

    tracing::info!(
        path = %suspended_path.display(),
        count = state.agents.len(),
        "saved suspended agents"
    );
    Ok(())
}

/// Load suspended server state from `<state_dir>/suspended.yaml` and delete the file.
pub(crate) fn load_and_remove_suspended(
    state_path: &Path,
) -> Result<SuspendedServerState, Box<dyn std::error::Error + Send + Sync>> {
    let suspended_path = suspended_path(state_path);
    if !suspended_path.exists() {
        return Ok(SuspendedServerState { agents: Vec::new() });
    }
    let yaml = fs::read_to_string(&suspended_path)?;
    let state: SuspendedServerState = serde_yaml::from_str(&yaml)?;
    fs::remove_file(&suspended_path)?;
    tracing::info!(
        path = %suspended_path.display(),
        count = state.agents.len(),
        "loaded and removed suspended agents"
    );
    Ok(state)
}

fn suspended_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("suspended.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_suspended_agent_roundtrip() {
        let state = SuspendedServerState {
            agents: vec![
                SuspendedAgent::Claude {
                    agent_id: Uuid::new_v4(),
                    name: Some("test-claude".to_string()),
                    name_source: SuspendedLocalAgentNameSource::ProviderName,
                    working_dir: PathBuf::from("/home/user/project"),
                    terminal_size: Some(TerminalSize {
                        rows: 40,
                        cols: 120,
                    }),
                    args: vec!["--dangerously-skip-permissions".to_string()],
                    session_id: Uuid::new_v4(),
                    created_at: Utc::now(),
                },
                #[cfg(any(debug_assertions, test))]
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

        assert_eq!(loaded.agents.len(), state.agents.len());
        assert!(matches!(
            &loaded.agents[0],
            SuspendedAgent::Claude { name, .. } if name.as_deref() == Some("test-claude")
        ));

        let suspended = suspended_path(&state_path);
        assert!(!suspended.exists());
    }
}
