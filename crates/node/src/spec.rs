//! `agents/<id>/spec.<n>`: one immutable file per incarnation, written by
//! the daemon before it spawns that incarnation and never rewritten.
//!
//! A spec holds everything the agent process needs to start, so any later
//! daemon can start it identically and a person can read why it was
//! started the way it was. A resume writes the next file from the current
//! configuration; config is frozen per incarnation.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use prost::Message as _;
use wire::{AgentParent, AgentSpec, CreateAgentRequest, EffectiveConfig, Input, Kind};

use crate::runtime::Launch;

/// The kind's name in a spec and on the agents row.
pub fn kind_name(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::ClaudePty => Some("claude_pty"),
        Kind::ClaudeSdk => Some("claude_sdk"),
        Kind::Codex => Some("codex"),
        Kind::Unspecified => None,
    }
}

pub fn kind_from_name(name: &str) -> Kind {
    match name {
        "claude_pty" => Kind::ClaudePty,
        "claude_sdk" => Kind::ClaudeSdk,
        "codex" => Kind::Codex,
        _ => Kind::Unspecified,
    }
}

/// What a creation asked for, resolved into provider arguments and the
/// agent's configuration. Carried from one incarnation's spec to the next.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub provider_args: Vec<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
}

/// Resolves a creation's per-provider settings. Claude takes its model and
/// permission mode as arguments; Codex takes the model in its thread start
/// and the rest as configuration overrides.
pub fn resolve(request: &CreateAgentRequest) -> Resolved {
    use wire::create_agent_request::Config;
    match &request.config {
        Some(Config::Claude(claude)) => {
            let mut args = claude.args.clone();
            if let Some(model) = &claude.model {
                args.extend(["--model".to_owned(), model.clone()]);
            }
            if let Some(mode) = &claude.permission_mode {
                args.extend(["--permission-mode".to_owned(), mode.clone()]);
            }
            if let Some(effort) = &claude.effort {
                args.extend(["--effort".to_owned(), effort.clone()]);
            }
            Resolved {
                provider_args: args,
                model: claude.model.clone(),
                permission_mode: claude.permission_mode.clone(),
            }
        }
        Some(Config::Codex(codex)) => {
            let mut args = Vec::new();
            for (key, value) in [
                ("approval_policy", &codex.approval_policy),
                ("sandbox_mode", &codex.sandbox_policy),
                ("model_reasoning_effort", &codex.effort),
            ] {
                if let Some(value) = value {
                    args.extend([
                        "--config".to_owned(),
                        format!("{key}={}", serde_json::json!(value)),
                    ]);
                }
            }
            Resolved {
                provider_args: args,
                model: codex.model.clone(),
                permission_mode: None,
            }
        }
        None => Resolved::default(),
    }
}

/// The provider command as configured for a kind.
pub fn provider_command(launch: &Launch, kind: Kind) -> String {
    match kind {
        Kind::Codex => launch.codex_command.clone(),
        _ => launch.claude_command.clone(),
    }
}

/// Where a command resolves on the PATH, the way the agent process will
/// launch it; the command itself when it names a path or is not found.
pub fn resolve_binary(command: &str) -> String {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return command.to_owned();
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return command.to_owned();
    };
    for dir in std::env::split_paths(&dirs) {
        let candidate = dir.join(format!("{command}{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    command.to_owned()
}

/// Everything one incarnation's spec is built from.
pub struct Incarnation<'a> {
    pub agent_id: &'a [u8],
    pub profile_id: &'a [u8],
    pub kind: Kind,
    pub cwd: &'a str,
    pub name: &'a str,
    pub parent: Option<AgentParent>,
    pub resolved: &'a Resolved,
    pub created_at_ms: i64,
    pub incarnation: u32,
    pub initial_prompt: Option<Input>,
}

pub fn build(launch: &Launch, at: Incarnation<'_>) -> AgentSpec {
    let command = provider_command(launch, at.kind);
    let agent = &launch.agent;
    AgentSpec {
        agent_id: at.agent_id.to_vec(),
        profile_id: at.profile_id.to_vec(),
        kind: kind_name(at.kind).unwrap_or_default().to_owned(),
        cwd: at.cwd.to_owned(),
        name: at.name.to_owned(),
        parent: at.parent,
        provider_args: at.resolved.provider_args.clone(),
        provider_env: launch
            .provider_env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<_, _>>(),
        provider_binary: resolve_binary(&command),
        provider_command: command,
        // The agent observes and records the version it actually runs, on
        // its boundary item; probing it here would start the provider.
        provider_version: String::new(),
        config: Some(EffectiveConfig {
            hooks: Vec::new(),
            grace_ms: millis(agent.grace_secs),
            drain_ms: millis(agent.drain_secs),
            facts_ring_bytes: agent.facts_ring_mib.saturating_mul(1024 * 1024),
            journal_segment_bytes: launch.journal_segment_bytes,
            model: at.resolved.model.clone(),
            permission_mode: at.resolved.permission_mode.clone(),
            env: HashMap::new(),
            install_path: launch.install_path.to_string_lossy().into_owned(),
        }),
        daemon_version: crate::version().to_owned(),
        created_at_ms: at.created_at_ms,
        incarnation: at.incarnation,
        initial_prompt: at.initial_prompt,
    }
}

fn millis(secs: u64) -> u32 {
    u32::try_from(secs.saturating_mul(1000)).unwrap_or(u32::MAX)
}

/// The path of incarnation `n`'s spec.
pub fn path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("spec.{n}"))
}

/// Writes `spec.<n>`, refusing to replace one that exists: a spec is
/// never rewritten. Written through a temporary file so a reader never
/// sees half a spec.
pub fn write(dir: &Path, spec: &AgentSpec) -> io::Result<()> {
    let target = path(dir, spec.incarnation);
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is never rewritten", target.display()),
        ));
    }
    let temp = dir.join(format!(".spec.{}.tmp", spec.incarnation));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)?;
    file.write_all(&spec.encode_to_vec())?;
    drop(file);
    fs::rename(&temp, &target)
}

/// The newest spec in a directory and its number.
pub fn newest(dir: &Path) -> io::Result<Option<AgentSpec>> {
    let mut newest: Option<u32> = None;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(n) = name
            .to_str()
            .and_then(|name| name.strip_prefix("spec."))
            .and_then(|n| n.parse::<u32>().ok())
        {
            newest = newest.max(Some(n));
        }
    }
    let Some(n) = newest else { return Ok(None) };
    let bytes = fs::read(path(dir, n))?;
    AgentSpec::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
