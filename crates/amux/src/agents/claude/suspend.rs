use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agents::{AgentParent, ClaudeDriver, LocalAgentNameSource, TerminalSize};
use crate::suspend::SuspendedAgent;

pub(super) struct ClaudeSuspendRecord {
    pub driver: ClaudeDriver,
    pub agent_id: Uuid,
    pub name: Option<String>,
    pub name_source: LocalAgentNameSource,
    pub working_dir: PathBuf,
    pub terminal_size: Option<TerminalSize>,
    pub created_at: DateTime<Utc>,
    pub args: Vec<String>,
    pub session_id: Uuid,
    pub parent: Option<AgentParent>,
}

impl From<ClaudeSuspendRecord> for SuspendedAgent {
    fn from(record: ClaudeSuspendRecord) -> Self {
        Self::Claude {
            driver: record.driver,
            agent_id: record.agent_id,
            name: record.name,
            name_source: record.name_source.into(),
            working_dir: record.working_dir,
            terminal_size: record.terminal_size,
            created_at: record.created_at,
            args: record.args,
            session_id: record.session_id,
            parent: record.parent,
            working_on: None,
        }
    }
}

pub(super) fn sanitize_resume_args(args: Vec<String>) -> Vec<String> {
    fn skip_value(args: &[String], index: &mut usize) {
        *index += 1;
        if *index < args.len() && !args[*index].starts_with('-') {
            *index += 1;
        }
    }

    let mut retained = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--fork-session" | "-c" | "--continue" => index += 1,
            "-r" | "--resume" | "--from-pr" | "--session-id" | "-w" | "--worktree" | "--tmux" => {
                skip_value(&args, &mut index)
            }
            arg if arg.starts_with("--resume=")
                || arg.starts_with("--from-pr=")
                || arg.starts_with("--session-id=")
                || arg.starts_with("--worktree=")
                || arg.starts_with("--tmux=")
                || (arg.starts_with("-r") && arg.len() > 2)
                || (arg.starts_with("-w") && arg.len() > 2) =>
            {
                index += 1;
            }
            _ => {
                retained.push(args[index].clone());
                index += 1;
            }
        }
    }
    retained
}
