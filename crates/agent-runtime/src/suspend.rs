use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::{AgentParent, ClaudeDriver, SealedAt, TerminalSize, WorkingOn};

/// All suspended agent sessions, serialized to disk across server restarts.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct SuspendedServerState {
    pub(crate) agents: Vec<SuspendedAgent>,
}

#[derive(Default, Serialize, Deserialize)]
struct ConsumedSeals {
    ids: BTreeSet<Uuid>,
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
        driver: ClaudeDriver,
        agent_id: Uuid,
        name: Option<String>,
        name_source: SuspendedLocalAgentNameSource,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
        args: Vec<String>,
        session_id: Uuid,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<AgentParent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_on: Option<WorkingOn>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seal: Option<SealedAt>,
    },
    #[cfg(unix)]
    Codex {
        agent_id: Uuid,
        name: Option<String>,
        working_dir: PathBuf,
        model: Option<String>,
        approval_policy: Option<String>,
        sandbox_policy: Option<String>,
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        daemon_mode: Option<String>,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<AgentParent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_on: Option<WorkingOn>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seal: Option<SealedAt>,
    },
    #[cfg(any(debug_assertions, test))]
    TestAgent {
        agent_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<AgentParent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_on: Option<WorkingOn>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seal: Option<SealedAt>,
    },
}

impl SuspendedAgent {
    pub(crate) fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude { agent_id, .. } => *agent_id,
            #[cfg(unix)]
            Self::Codex { agent_id, .. } => *agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { agent_id, .. } => *agent_id,
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Claude { name, .. } => name.as_deref(),
            #[cfg(unix)]
            Self::Codex { name, .. } => name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { name, .. } => name.as_deref(),
        }
    }

    pub(crate) fn working_on(&self) -> Option<&WorkingOn> {
        match self {
            Self::Claude { working_on, .. } => working_on.as_ref(),
            #[cfg(unix)]
            Self::Codex { working_on, .. } => working_on.as_ref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { working_on, .. } => working_on.as_ref(),
        }
    }

    pub(crate) fn set_working_on(&mut self, value: Option<WorkingOn>) {
        match self {
            Self::Claude { working_on, .. } => *working_on = value,
            #[cfg(unix)]
            Self::Codex { working_on, .. } => *working_on = value,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { working_on, .. } => *working_on = value,
        }
    }

    pub(crate) fn seal(&self) -> Option<SealedAt> {
        match self {
            Self::Claude { seal, .. } => *seal,
            #[cfg(unix)]
            Self::Codex { seal, .. } => *seal,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { seal, .. } => *seal,
        }
    }

    pub(crate) fn set_seal(&mut self, value: Option<SealedAt>) {
        match self {
            Self::Claude { seal, .. } => *seal = value,
            #[cfg(unix)]
            Self::Codex { seal, .. } => *seal = value,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { seal, .. } => *seal = value,
        }
    }

    pub(crate) fn recreate_under(&mut self, agent_id: Uuid) {
        match self {
            Self::Claude {
                agent_id: current, ..
            } => *current = agent_id,
            #[cfg(unix)]
            Self::Codex {
                agent_id: current, ..
            } => *current = agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent {
                agent_id: current, ..
            } => *current = agent_id,
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
    let temp_path = suspended_path.with_extension("yaml.tmp");

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(&temp_path)?;
    file.write_all(yaml.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, &suspended_path)?;
    // The rename must survive a crash before the prepared agents are stopped.
    #[cfg(unix)]
    fs::File::open(
        suspended_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;

    tracing::info!(
        path = %suspended_path.display(),
        count = state.agents.len(),
        "saved suspended agents"
    );
    Ok(())
}

/// Load suspended server state from `<state_dir>/suspended.yaml`.
pub(crate) fn load_suspended(
    state_path: &Path,
) -> Result<SuspendedServerState, Box<dyn std::error::Error + Send + Sync>> {
    let suspended_path = suspended_path(state_path);
    if !suspended_path.exists() {
        return Ok(SuspendedServerState { agents: Vec::new() });
    }
    let yaml = fs::read_to_string(&suspended_path)?;
    let state: SuspendedServerState = serde_yaml::from_str(&yaml)?;
    tracing::info!(
        path = %suspended_path.display(),
        count = state.agents.len(),
        "loaded suspended agents"
    );
    Ok(state)
}

/// Delete the suspended server state file if it exists.
pub(crate) fn remove_suspended(state_path: &Path) -> Result<(), std::io::Error> {
    let suspended_path = suspended_path(state_path);
    match fs::remove_file(&suspended_path) {
        Ok(()) => {
            #[cfg(unix)]
            fs::File::open(
                suspended_path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
            tracing::info!(path = %suspended_path.display(), "removed suspended agents");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Durably consume every supplied seal before any resumed publisher starts.
///
/// The returned ids were already consumed by an earlier attempt and therefore
/// must be recreated under a new agent identity without continuing their old
/// sequence.
pub(crate) fn consume_seals(
    state_path: &Path,
    agents: &[SuspendedAgent],
) -> Result<HashSet<Uuid>, std::io::Error> {
    let path = consumed_seals_path(state_path);
    let mut consumed = match fs::read_to_string(&path) {
        Ok(yaml) => serde_yaml::from_str::<ConsumedSeals>(&yaml).map_err(std::io::Error::other)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConsumedSeals::default(),
        Err(error) => return Err(error),
    };
    let mut already = HashSet::new();
    let mut changed = false;
    for agent in agents {
        let Some(seal) = agent.seal() else {
            already.insert(agent.agent_id());
            continue;
        };
        if !consumed.ids.insert(seal.id) {
            already.insert(agent.agent_id());
        } else {
            changed = true;
        }
    }
    if changed {
        save_consumed_seals(&path, &consumed)?;
    }
    Ok(already)
}

/// Permanently invalidate prepared seals before their live sources unpark.
pub(crate) fn invalidate_seals(
    state_path: &Path,
    agents: &[SuspendedAgent],
) -> Result<(), std::io::Error> {
    consume_seals(state_path, agents).map(|_| ())
}

/// Remove only the prepared records named by these seals, preserving older
/// failed resumes that share the same state file.
pub(crate) fn remove_prepared(
    state_path: &Path,
    agents: &[SuspendedAgent],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let seals = agents
        .iter()
        .filter_map(SuspendedAgent::seal)
        .map(|seal| seal.id)
        .collect::<HashSet<_>>();
    let mut retained = load_suspended(state_path)?;
    let before = retained.agents.len();
    retained
        .agents
        .retain(|agent| agent.seal().is_none_or(|seal| !seals.contains(&seal.id)));
    if retained.agents.len() == before {
        return Ok(());
    }
    if retained.agents.is_empty() {
        remove_suspended(state_path)?;
    } else {
        save_suspended(state_path, &retained)?;
    }
    Ok(())
}

fn save_consumed_seals(path: &Path, state: &ConsumedSeals) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(state).map_err(std::io::Error::other)?;
    let temp_path = path.with_extension("yaml.tmp");
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(&temp_path)?;
    file.write_all(yaml.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, path)?;
    #[cfg(unix)]
    fs::File::open(
        path.parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

fn suspended_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("suspended.yaml")
}

fn consumed_seals_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("consumed-seals.yaml")
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_suspended_agent_roundtrip() {
        let state = SuspendedServerState {
            agents: vec![
                SuspendedAgent::Claude {
                    driver: ClaudeDriver::Pty,
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
                    parent: Some(AgentParent {
                        agent_id: Uuid::new_v4(),
                        host_id: Uuid::new_v4(),
                    }),
                    working_on: Some(WorkingOn {
                        text: "reviewing protocol".to_string(),
                        updated_at: Utc::now(),
                    }),
                    seal: None,
                },
                #[cfg(unix)]
                SuspendedAgent::Codex {
                    agent_id: Uuid::new_v4(),
                    name: Some("test-codex".to_string()),
                    working_dir: PathBuf::from("/home/user/project"),
                    model: Some("test-model".to_string()),
                    approval_policy: Some("on-request".to_string()),
                    sandbox_policy: Some("workspace-write".to_string()),
                    thread_id: "thread-1".to_string(),
                    daemon_mode: Some("spawned-well-known".to_string()),
                    created_at: Utc::now(),
                    parent: None,
                    working_on: None,
                    seal: None,
                },
                #[cfg(any(debug_assertions, test))]
                SuspendedAgent::TestAgent {
                    agent_id: Uuid::new_v4(),
                    name: None,
                    command: "test-agent".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    terminal_size: None,
                    created_at: Utc::now(),
                    parent: None,
                    working_on: None,
                    seal: None,
                },
            ],
        };

        let temp = TempDir::new().unwrap();
        let state_path = temp.path().join("state.yaml");

        save_suspended(&state_path, &state).unwrap();
        let loaded = load_suspended(&state_path).unwrap();

        assert_eq!(loaded.agents.len(), state.agents.len());
        assert!(matches!(
            &loaded.agents[0],
            SuspendedAgent::Claude {
                driver: ClaudeDriver::Pty,
                name,
                parent: Some(parent),
                working_on: Some(working_on),
                ..
            } if name.as_deref() == Some("test-claude")
                && parent.agent_id != Uuid::nil()
                && parent.host_id != Uuid::nil()
                && working_on.text == "reviewing protocol"
        ));

        let suspended = suspended_path(&state_path);
        assert!(suspended.exists());
        remove_suspended(&state_path).unwrap();
        assert!(!suspended.exists());
    }

    #[cfg(unix)]
    #[test]
    fn codex_daemon_mode_defaults_when_omitted() {
        let state = SuspendedServerState {
            agents: vec![SuspendedAgent::Codex {
                agent_id: Uuid::new_v4(),
                name: Some("pending-resume".to_string()),
                working_dir: PathBuf::from("/tmp"),
                model: None,
                approval_policy: None,
                sandbox_policy: None,
                thread_id: "thread-known".to_string(),
                daemon_mode: None,
                created_at: Utc::now(),
                parent: None,
                working_on: None,
                seal: None,
            }],
        };
        let yaml = serde_yaml::to_string(&state).unwrap();
        assert!(!yaml.contains("daemon_mode"));

        let loaded: SuspendedServerState = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(
            &loaded.agents[0],
            SuspendedAgent::Codex {
                thread_id,
                daemon_mode: None,
                ..
            } if thread_id == "thread-known"
        ));
    }
}
