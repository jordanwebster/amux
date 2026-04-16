#[cfg(any(debug_assertions, test))]
use crate::agent::TestAgentSession;
use crate::agent::{AgentSession, ClaudeSession, LocalAgentNameSource};
use crate::protocol::message::{AgentType, CreateAgentRequest, TerminalSize};
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
pub struct SuspendedServerState {
    pub agents: Vec<SuspendedAgent>,
}

/// Serializable representation of a suspended agent session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SuspendedAgent {
    Claude {
        agent_id: Uuid,
        name: Option<String>,
        name_source: LocalAgentNameSource,
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

fn sanitize_claude_resume_args(args: Vec<String>) -> Vec<String> {
    fn skip_flag_with_optional_value(args: &[String], i: &mut usize) {
        *i += 1;
        if *i < args.len() && !args[*i].starts_with('-') {
            *i += 1;
        }
    }

    let mut sanitized = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fork-session" | "-c" | "--continue" => i += 1,
            "-r" | "--resume" | "--from-pr" | "--session-id" | "-w" | "--worktree" | "--tmux" => {
                skip_flag_with_optional_value(&args, &mut i)
            }
            arg if arg.starts_with("--resume=")
                || arg.starts_with("--from-pr=")
                || arg.starts_with("--session-id=")
                || arg.starts_with("--worktree=")
                || arg.starts_with("--tmux=") =>
            {
                let _ = arg;
                i += 1;
            }
            arg if arg.starts_with("-r") && arg.len() > 2 => i += 1,
            arg if arg.starts_with("-w") && arg.len() > 2 => i += 1,
            _ => {
                sanitized.push(args[i].clone());
                i += 1;
            }
        }
    }
    sanitized
}

impl SuspendedAgent {
    pub fn agent_id(&self) -> Uuid {
        match self {
            Self::Claude { agent_id, .. } => *agent_id,
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { agent_id, .. } => *agent_id,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Claude { name, .. } => name.as_deref(),
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent { name, .. } => name.as_deref(),
        }
    }

    pub fn into_session(self) -> AgentSession {
        match self {
            Self::Claude {
                agent_id,
                name,
                name_source,
                working_dir,
                terminal_size,
                args,
                session_id,
                created_at,
            } => {
                let args = sanitize_claude_resume_args(args);
                let req = CreateAgentRequest {
                    agent_id,
                    name,
                    agent_type: AgentType::Claude,
                    working_dir,
                    terminal_size,
                    args,
                };
                let mut session = ClaudeSession::new(&req);
                session.session_id = Some(session_id);
                session.created_at = created_at;
                session.set_name_and_source(req.name, name_source);
                AgentSession::Claude(session)
            }
            #[cfg(any(debug_assertions, test))]
            Self::TestAgent {
                agent_id,
                name,
                command,
                working_dir,
                terminal_size,
                created_at,
            } => {
                let req = CreateAgentRequest {
                    agent_id,
                    name,
                    agent_type: AgentType::TestAgent {
                        command: command.clone(),
                    },
                    working_dir,
                    terminal_size,
                    args: vec![],
                };
                let mut session = TestAgentSession::new(&req, command);
                session.created_at = created_at;
                AgentSession::TestAgent(session)
            }
        }
    }
}

/// Save suspended server state to `<state_dir>/suspended.yaml` (sibling of state.yaml).
pub fn save_suspended(
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
pub fn load_and_remove_suspended(
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
    fn suspended_claude_into_session_filters_resume_unsafe_args() {
        let sa = SuspendedAgent::Claude {
            agent_id: Uuid::new_v4(),
            name: Some("claude".to_string()),
            name_source: LocalAgentNameSource::ProviderName,
            working_dir: PathBuf::from("/tmp"),
            terminal_size: None,
            args: vec![
                "--dangerously-skip-permissions".to_string(),
                "--resume".to_string(),
                Uuid::new_v4().to_string(),
                "--fork-session".to_string(),
                "--continue".to_string(),
                "--from-pr=123".to_string(),
                "--session-id".to_string(),
                Uuid::new_v4().to_string(),
                "--worktree".to_string(),
                "feature-branch".to_string(),
                "--tmux=classic".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ],
            session_id: Uuid::new_v4(),
            created_at: Utc::now(),
        };

        let session = sa.into_session();

        assert_eq!(
            session.to_agent(Uuid::new_v4()).args,
            vec![
                "--dangerously-skip-permissions".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ]
        );
    }

    #[test]
    fn test_suspended_agent_roundtrip() {
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
