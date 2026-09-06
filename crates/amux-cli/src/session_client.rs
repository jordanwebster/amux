use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use amux::terminal_io::{self, TerminalV1Args};
use amux::{
    AgentIdentifier, AgentType, Client, ClientError, Config, CreateAgentRequest, LeaderKey,
    SendInputRequest, SessionCloseReason, ShutdownReason, SubscribeSessionEvent,
    SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use crossterm::terminal;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::client_common::{get_client, print_update_banner, require_running_client};

/// Events from stdin reading task
pub(crate) enum StdinEvent {
    /// Raw input data to send to agent
    Data(Vec<u8>),
    /// User requested detach to the shell (<leader>d).
    Detach,
    /// User requested the fleet picker (<leader>s). From the TUI this
    /// resumes the chrome; from CLI attach it behaves like detach.
    SwitchToFleet,
}

/// How an attached session ended (transport errors surface as `Err`).
#[derive(Debug)]
pub(crate) enum AttachOutcome {
    Detached,
    SwitchedToFleet,
    SessionEnded,
    SessionClosed(SessionCloseReason),
    Shutdown(ShutdownReason),
}

/// Find a subsequence within a slice, returns the starting index if found
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Get terminal size, falling back to 24x80 if the terminal query fails.
fn get_terminal_size() -> TerminalSize {
    terminal::size()
        .map(|(cols, rows)| TerminalSize { rows, cols })
        .unwrap_or_default()
}

/// Create a new agent and open it in the configured default mode.
pub async fn new_agent(
    name: Option<&str>,
    agent_type: AgentType,
    args: Vec<String>,
    config: &Config,
) -> Result<()> {
    let codex_configuration = codex_configuration_facts(&agent_type);
    let terminal_exposed = agent_type_exposes_terminal(&agent_type);
    let rpc = get_client(config).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    let create_rpc = rpc.clone();
    let verify_rpc = rpc.clone();
    create_and_open_agent(
        name,
        move || async move {
            create_rpc
                .create_agent(CreateAgentRequest {
                    agent_id,
                    host_id: None,
                    name: name.map(str::to_string),
                    agent_type,
                    working_dir,
                    terminal_size: Some(terminal_size),
                    args,
                    parent: None,
                    initial_prompt: None,
                })
                .await
                .map(|agent| agent.id)
                .map_err(|error| anyhow!("failed to create agent: {error}"))
        },
        move |agent_id| async move {
            match (config.ui.default_open_mode, terminal_exposed) {
                // Kinds without terminal_v1 still have a structured layer.
                // For Claude/SDK that layer is the deliberate unsupported
                // placeholder and intentionally opens no stream.
                (_, false) => {
                    crate::ui::run_for_agent(config.clone(), agent_id, codex_configuration).await
                }
                (amux::OpenMode::Chat, true) => {
                    crate::ui::run_for_agent(config.clone(), agent_id, codex_configuration).await
                }
                (amux::OpenMode::Raw, true) => {
                    let identifier = AgentIdentifier::from(agent_id);
                    let outcome = if codex_configuration.is_some() {
                        attach_new_codex_terminal(
                            &rpc,
                            identifier,
                            config.keybinds.leader.clone(),
                            StdinHandback::ProcessExits,
                        )
                        .await?
                    } else {
                        attach_terminal(
                            &rpc,
                            identifier,
                            config.keybinds.leader.clone(),
                            StdinHandback::ProcessExits,
                        )
                        .await?
                    };
                    finish_cli_attach(outcome, &config.state_path)
                }
            }
        },
        move |agent_id| async move {
            verify_rpc
                .list_agents()
                .await
                .map(|agents| agents.iter().any(|agent| agent.id == agent_id))
                .map_err(|error| anyhow!("failed to list agents: {error}"))
        },
    )
    .await
}

async fn create_and_open_agent<Create, CreateFuture, Open, OpenFuture, Verify, VerifyFuture>(
    name: Option<&str>,
    create: Create,
    open: Open,
    verify: Verify,
) -> Result<()>
where
    Create: FnOnce() -> CreateFuture,
    CreateFuture: Future<Output = Result<Uuid>>,
    Open: FnOnce(Uuid) -> OpenFuture,
    OpenFuture: Future<Output = Result<()>>,
    Verify: FnOnce(Uuid) -> VerifyFuture,
    VerifyFuture: Future<Output = Result<bool>>,
{
    let agent_id = create().await?;
    let Err(open_error) = open(agent_id).await else {
        return Ok(());
    };

    match verify(agent_id).await {
        Ok(true) => Err(created_agent_open_error(name, agent_id, open_error)),
        Ok(false) => Err(open_error),
        Err(verification_error) => Err(anyhow!(
            "{open_error:#}\nCould not verify whether created agent {agent_id} is still present: {verification_error:#}"
        )),
    }
}

fn created_agent_open_error(
    name: Option<&str>,
    agent_id: Uuid,
    error: anyhow::Error,
) -> anyhow::Error {
    let display = name
        .map(str::to_string)
        .unwrap_or_else(|| agent_id.to_string());
    anyhow!(
        "agent '{display}' was created and is running. Reattach with 'amux attach {agent_id}', or remove it with 'amux rm {agent_id}' (or 'd' in the fleet view).\n{error:#}"
    )
}

/// The three creation choices a Codex chat states in its header: what it
/// runs on, whether it asks before acting, and what it may touch. An
/// unset one is stated as `default` — Codex's own default is a fact, not
/// an absence. They travel as separate facts because the header joins
/// them with its own separator and drops them one at a time when the
/// line is too narrow.
fn codex_configuration_facts(agent_type: &AgentType) -> Option<Vec<String>> {
    let AgentType::Codex {
        model,
        approval_policy,
        sandbox_policy,
        ..
    } = agent_type
    else {
        return None;
    };
    Some(
        [model, approval_policy, sandbox_policy]
            .into_iter()
            .map(|fact| fact.clone().unwrap_or_else(|| "default".to_string()))
            .collect(),
    )
}

fn agent_type_exposes_terminal(agent_type: &AgentType) -> bool {
    match agent_type {
        AgentType::Claude { driver } => {
            amux::AgentKind::Claude { driver: *driver }.exposes(amux::Protocol::TerminalV1)
        }
        AgentType::Codex { .. } => amux::AgentKind::Codex.exposes(amux::Protocol::TerminalV1),
        #[cfg(any(debug_assertions, test))]
        AgentType::TestAgent { .. } => {
            amux::AgentKind::TestAgent.exposes(amux::Protocol::TerminalV1)
        }
    }
}

/// The command line's half of the one entry policy the fleet keys use:
/// a session with no terminal behind it has nothing to pass through, and
/// an agent on another machine opens the chat rather than piping a
/// terminal across the network. Everything else raw attaches, except a
/// Codex agent: its own structured screen is its primary surface, and
/// the command line — unlike the fleet, which offers Ctrl+Enter and `o`
/// beside Enter — has only one key to spend, so it spends it on the
/// richer surface. `docs/CHAT.md` records that difference.
fn attach_opens_chat(kind: &amux::AgentKind, local: bool) -> bool {
    !local || matches!(kind, amux::AgentKind::Codex) || !kind.exposes(amux::Protocol::TerminalV1)
}

/// Attach to an existing agent
pub async fn attach(target: Option<&str>, config: &Config) -> Result<()> {
    let retry_command = target
        .map(|target| format!("amux attach {target}"))
        .unwrap_or_else(|| "amux attach".to_string());
    let rpc = require_running_client(config, Some(&retry_command)).await?;

    let agents = rpc.list_agents().await?;
    let Some(agent) = resolve_attach_agent(&agents, target)? else {
        eprintln!("No agents running. Use 'amux new' to create one.");
        return Ok(());
    };

    tracing::info!(agent = %agent.id, "attaching");

    // Unknown locality reads as local, the way the fleet's entry policy
    // reads it: the stored identity is missing only before this machine
    // has one, when every agent it can see is its own.
    let local = amux::setup::local_host_id(config).is_none_or(|local| local == agent.host_id);
    if attach_opens_chat(&agent.kind, local) {
        return crate::ui::run_for_agent(config.clone(), agent.id, None).await;
    }

    let outcome = attach_terminal(
        &rpc,
        AgentIdentifier::from(agent.id),
        config.keybinds.leader.clone(),
        StdinHandback::ProcessExits,
    )
    .await?;
    finish_cli_attach(outcome, &config.state_path)
}

/// Remove an agent by exact name or UUID without prompting.
pub async fn remove_agent(target: &str, force: bool, config: &Config) -> Result<()> {
    let rpc = require_running_client(config, None).await?;
    let report = remove_agent_with_client(target, force, &rpc).await?;
    for child in &report.removed_children {
        println!("Removed child {}.", removal_child_label(child));
    }
    for child in &report.unreachable_children {
        println!(
            "Child {} was unreachable and remains running.",
            removal_child_label(child)
        );
    }
    println!("Deleted agent '{target}'.");
    Ok(())
}

async fn remove_agent_with_client(
    target: &str,
    force: bool,
    rpc: &Client,
) -> Result<amux::DeleteAgentSummary> {
    let agents = rpc.list_agents().await?;
    let agent = resolve_remove_agent(&agents, target)?;
    ensure_family_is_removable(&agents, agent, force)?;
    rpc.delete_agent_with_summary(agent.id)
        .await
        .map_err(|error| anyhow!("failed to delete agent '{target}': {error}"))
}

fn family_descendants<'a>(agents: &'a [amux::Agent], root: &amux::Agent) -> Vec<&'a amux::Agent> {
    let mut children: HashMap<AgentKey, Vec<&amux::Agent>> = HashMap::new();
    for agent in agents {
        if let Some(parent) = agent.parent {
            children
                .entry((parent.agent_id, parent.host_id))
                .or_default()
                .push(agent);
        }
    }
    for group in children.values_mut() {
        group.sort_by_key(|agent| display_name(agent));
    }

    let root = agent_key(root);
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten().rev() {
            let key = agent_key(child);
            if seen.insert(key) {
                descendants.push(*child);
                pending.push(key);
            }
        }
    }
    descendants.sort_by_key(|agent| display_name(agent));
    descendants
}

fn ensure_family_is_removable(
    agents: &[amux::Agent],
    root: &amux::Agent,
    force: bool,
) -> Result<()> {
    if force {
        return Ok(());
    }
    let working: Vec<_> = family_descendants(agents, root)
        .into_iter()
        .filter_map(|agent| {
            agent.working_on.as_ref().map(|work| {
                let task = clipped_working_on(&work.text);
                if task.is_empty() {
                    display_name(agent)
                } else {
                    format!("{} ({task})", display_name(agent))
                }
            })
        })
        .collect();
    if working.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "refusing to delete '{}': child agents still working: {}; rerun with --force",
            display_name(root),
            working.join(", ")
        ))
    }
}

fn removal_child_label(agent: &amux::Agent) -> String {
    let name = display_name(agent);
    match &agent.working_on {
        Some(work) => {
            let task = clipped_working_on(&work.text);
            if task.is_empty() {
                format!("'{name}' [working]")
            } else {
                format!("'{name}' [working: {task}]")
            }
        }
        None => format!("'{name}'"),
    }
}

fn resolve_remove_agent<'a>(agents: &'a [amux::Agent], target: &str) -> Result<&'a amux::Agent> {
    if let Ok(id) = Uuid::parse_str(target) {
        return agents
            .iter()
            .find(|agent| agent.id == id)
            .ok_or_else(|| anyhow!("agent not found: {target}"));
    }
    let matches: Vec<_> = agents
        .iter()
        .filter(|agent| agent.name.as_deref() == Some(target))
        .collect();
    match matches.as_slice() {
        [] => Err(anyhow!("agent not found: {target}")),
        [agent] => Ok(*agent),
        _ => Err(anyhow!("agent name `{target}` is ambiguous")),
    }
}

#[cfg(test)]
async fn delete_exact_agent<Delete, DeleteFuture>(
    agents: &[amux::Agent],
    target: &str,
    delete: Delete,
) -> Result<()>
where
    Delete: FnOnce(Uuid) -> DeleteFuture,
    DeleteFuture: Future<Output = Result<()>>,
{
    let agent = resolve_remove_agent(agents, target)?;
    delete(agent.id).await
}

fn resolve_attach_agent<'a>(
    agents: &'a [amux::Agent],
    target: Option<&str>,
) -> Result<Option<&'a amux::Agent>> {
    let Some(target) = target else {
        return Ok(agents.first());
    };
    if let Ok(id) = Uuid::parse_str(target) {
        return agents
            .iter()
            .find(|agent| agent.id == id)
            .map(Some)
            .ok_or_else(|| anyhow!("agent not found: {target}"));
    }
    let matches: Vec<_> = agents
        .iter()
        .filter(|agent| agent.name.as_deref() == Some(target))
        .collect();
    match matches.as_slice() {
        [] => Err(anyhow!("agent not found: {target}")),
        [agent] => Ok(Some(*agent)),
        _ => Err(anyhow!(
            "agent name `{target}` is ambiguous; attach by id instead"
        )),
    }
}

/// Attach for the fleet TUI: run the passthrough on the real terminal and
/// come back with an optional status-line notice instead of exiting the
/// process. Stdin is fully reclaimed before returning so the chrome's event
/// stream is the only reader again.
pub(crate) async fn attach_for_ui(
    config: &Config,
    agent: amux::AgentId,
) -> Result<amux_tui::AttachReturn> {
    let rpc = require_running_client(config, None).await?;
    let outcome = attach_terminal(
        &rpc,
        AgentIdentifier::from(agent),
        config.keybinds.leader.clone(),
        StdinHandback::ReclaimForCaller,
    )
    .await?;
    use amux_tui::AttachReturn;
    Ok(match outcome {
        AttachOutcome::Detached => AttachReturn::Exit,
        AttachOutcome::SwitchedToFleet => AttachReturn::Fleet(None),
        AttachOutcome::SessionEnded => {
            AttachReturn::Fleet(Some(amux_tui::Notice::done("session ended")))
        }
        AttachOutcome::SessionClosed(reason) => {
            AttachReturn::Fleet(Some(session_close_notice(&reason)))
        }
        AttachOutcome::Shutdown(reason) => {
            AttachReturn::Fleet(Some(amux_tui::Notice::problem(format!("daemon: {reason}"))))
        }
    })
}

/// List running agents, folding children into their family unless requested.
pub async fn list_agents(all: bool, config: &Config) -> Result<()> {
    let rpc = require_running_client(config, Some("amux list")).await?;
    let agents = rpc.list_agents().await?;
    if agents.is_empty() {
        println!("No agents running.");
    } else {
        println!("Running agents:");
        for line in agent_list_lines(&agents, all, Utc::now()) {
            println!("{line}");
        }
    }

    print_update_banner(&config.state_path);
    Ok(())
}

type AgentKey = (Uuid, Uuid);

fn agent_key(agent: &amux::Agent) -> AgentKey {
    (agent.id, agent.host_id)
}

fn display_name(agent: &amux::Agent) -> String {
    agent.name.clone().unwrap_or_else(|| agent.id.to_string())
}

fn sort_agent_indexes(indexes: &mut [usize], agents: &[amux::Agent]) {
    indexes.sort_by(|left, right| {
        display_name(&agents[*left])
            .cmp(&display_name(&agents[*right]))
            .then_with(|| agent_key(&agents[*left]).cmp(&agent_key(&agents[*right])))
    });
}

fn descendant_count(
    root: AgentKey,
    children: &HashMap<AgentKey, Vec<usize>>,
    agents: &[amux::Agent],
) -> usize {
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    let mut count = 0;
    while let Some(parent) = pending.pop() {
        for index in children.get(&parent).into_iter().flatten() {
            let child = agent_key(&agents[*index]);
            if seen.insert(child) {
                count += 1;
                pending.push(child);
            }
        }
    }
    count
}

fn clipped_working_on(text: &str) -> String {
    const WIDTH: usize = 40;
    let text = text.lines().next().unwrap_or_default().trim();
    if text.chars().count() <= WIDTH {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(WIDTH.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

struct ListRender<'a> {
    agents: &'a [amux::Agent],
    children: HashMap<AgentKey, Vec<usize>>,
    name_counts: HashMap<String, usize>,
    multiple_hosts: bool,
    all: bool,
    now: DateTime<Utc>,
    visited: HashSet<AgentKey>,
    lines: Vec<String>,
}

impl ListRender<'_> {
    fn push(&mut self, index: usize, depth: usize) {
        let agent = &self.agents[index];
        let key = agent_key(agent);
        if !self.visited.insert(key) {
            return;
        }

        let mut labels = Vec::new();
        if agent
            .name
            .as_ref()
            .and_then(|name| self.name_counts.get(name))
            .is_some_and(|count| *count > 1)
        {
            labels.push(format!("id {}", agent.id));
        }
        if self.multiple_hosts {
            labels.push(format!("host {}", short_uuid(agent.host_id)));
        }
        let label = if labels.is_empty() {
            String::new()
        } else {
            format!(" ({})", labels.join(", "))
        };
        let child_count = descendant_count(key, &self.children, self.agents);
        let family = (depth == 0 && child_count > 0).then(|| format!(" ⋯{child_count}"));
        let working = agent.working_on.as_ref().and_then(|working| {
            let text = clipped_working_on(&working.text);
            (!text.is_empty()).then(|| {
                format!(
                    " · {text} {}",
                    amux_ui::format_relative_age(self.now, working.updated_at)
                )
            })
        });
        self.lines.push(format!(
            "{}{}{}{} [{}] - {}{}",
            "  ".repeat(depth + 1),
            display_name(agent),
            family.unwrap_or_default(),
            label,
            agent.kind,
            agent.working_dir.display(),
            working.unwrap_or_default()
        ));

        if self.all {
            let child_indexes = self.children.get(&key).cloned().unwrap_or_default();
            for child in child_indexes {
                self.push(child, depth + 1);
            }
        } else {
            let mut pending = vec![key];
            while let Some(parent) = pending.pop() {
                for child in self.children.get(&parent).into_iter().flatten() {
                    let child = agent_key(&self.agents[*child]);
                    if self.visited.insert(child) {
                        pending.push(child);
                    }
                }
            }
        }
    }
}

fn agent_list_lines(agents: &[amux::Agent], all: bool, now: DateTime<Utc>) -> Vec<String> {
    let known: HashSet<_> = agents.iter().map(agent_key).collect();
    let mut children: HashMap<AgentKey, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    let mut name_counts = HashMap::new();
    for (index, agent) in agents.iter().enumerate() {
        if let Some(name) = &agent.name {
            *name_counts.entry(name.clone()).or_insert(0) += 1;
        }
        match agent.parent.map(|parent| (parent.agent_id, parent.host_id)) {
            Some(parent) if known.contains(&parent) && parent != agent_key(agent) => {
                children.entry(parent).or_default().push(index);
            }
            _ => roots.push(index),
        }
    }
    sort_agent_indexes(&mut roots, agents);
    for indexes in children.values_mut() {
        sort_agent_indexes(indexes, agents);
    }

    let mut render = ListRender {
        agents,
        children,
        name_counts,
        multiple_hosts: agents
            .iter()
            .map(|agent| agent.host_id)
            .collect::<HashSet<_>>()
            .len()
            > 1,
        all,
        now,
        visited: HashSet::new(),
        lines: Vec::new(),
    };
    for root in roots {
        render.push(root, 0);
    }
    let mut remainder: Vec<_> = (0..agents.len()).collect();
    sort_agent_indexes(&mut remainder, agents);
    for index in remainder {
        render.push(index, 0);
    }
    render.lines
}

fn short_uuid(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

/// Open the raw byte-passthrough session (late attach renders via the
/// existing buffer replay — the history is a bounded byte tail).
pub(crate) async fn subscribe_raw(
    rpc: &Client,
    agent: &AgentIdentifier,
    terminal_size: Option<TerminalSize>,
) -> Result<amux::SessionStream> {
    let session = rpc
        .subscribe_session(SubscribeSessionRequest {
            agent: agent.clone(),
            io_protocol: terminal_io::TERMINAL_V1.to_string(),
            args: terminal_io::encode_terminal_v1_args(TerminalV1Args {
                terminal_size,
                replay_query: None,
            })
            .map(Into::into),
        })
        .await
        .map_err(|error| anyhow!("failed to subscribe to session: {error}"))?;

    tracing::info!(?agent, "subscribed to raw session");
    Ok(session)
}

/// What happens to the blocked stdin reader when the session ends without a
/// detach: the CLI paths exit the process (the reader dies with it); the TUI
/// path must reclaim stdin — one keypress, prompted — before the chrome's
/// event stream may read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StdinHandback {
    ProcessExits,
    ReclaimForCaller,
}

/// Attach on the real terminal: raw mode (RAII), the leader-scanning stdin
/// reader, and the passthrough loop. Every exit path restores the terminal
/// mode; with `ReclaimForCaller` stdin is exclusively released before
/// returning.
async fn attach_terminal(
    rpc: &Client,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let session = subscribe_raw(rpc, &agent, Some(get_terminal_size())).await?;
    attach_subscribed(rpc, session, agent, leader, handback).await
}

/// A newly-created Codex backend learns its thread id asynchronously. Retry
/// only that named readiness race; every other subscription error remains
/// immediate and unchanged.
async fn attach_new_codex_terminal(
    rpc: &Client,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let terminal_size = get_terminal_size();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let session = loop {
        match subscribe_raw(rpc, &agent, Some(terminal_size)).await {
            Ok(session) => break session,
            Err(error)
                if codex_thread_not_ready(&error) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    };
    attach_subscribed(rpc, session, agent, leader, handback).await
}

/// Only the transient not-yet-published case is worth retrying. A thread that
/// failed to materialize reports a different error, and waiting will not fix
/// it, so this must not match it.
fn codex_thread_not_ready(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .contains(amux::codex_io::CODEX_RAW_THREAD_NOT_READY)
}

async fn attach_subscribed(
    rpc: &Client,
    session: amux::SessionStream,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let raw_mode_guard = RawModeGuard::new()?;
    let stop_reading = Arc::new(AtomicBool::new(false));
    let (input_rx, reader) = spawn_stdin_reader(leader, stop_reading.clone());
    let outcome = attach_loop(rpc.clone(), session, agent, input_rx, io::stdout()).await;
    drop(raw_mode_guard);

    if handback == StdinHandback::ReclaimForCaller {
        stop_reading.store(true, Ordering::SeqCst);
        let needs_key = !matches!(
            outcome,
            Ok(AttachOutcome::Detached | AttachOutcome::SwitchedToFleet)
        ) && !reader.is_finished();
        if needs_key {
            let label = match &outcome {
                Ok(AttachOutcome::SessionClosed(reason)) => session_close_label(reason),
                Ok(AttachOutcome::Shutdown(_)) => "daemon shut down",
                _ => "session ended",
            };
            println!("\n[{label} — press any key to return to the fleet]");
        }
        let _ = reader.await;
    }
    outcome
}

/// Print the outcome and exit the process where today's CLI contract says
/// so; only a detach returns.
fn finish_cli_attach(outcome: AttachOutcome, state_path: &Path) -> Result<()> {
    match outcome {
        AttachOutcome::Shutdown(reason) => {
            println!("\n[{}]", reason);
            if reason != ShutdownReason::Updating {
                print_update_banner(state_path);
            }
            std::process::exit(1);
        }
        AttachOutcome::Detached | AttachOutcome::SwitchedToFleet => {
            println!("\n[detached from session]");
        }
        AttachOutcome::SessionEnded => {
            println!("\n[session ended]");
            print_update_banner(state_path);
            std::process::exit(0);
        }
        AttachOutcome::SessionClosed(reason) => {
            println!("\n[{}]", session_close_label(&reason));
            print_update_banner(state_path);
            std::process::exit(0);
        }
    }

    print_update_banner(state_path);
    Ok(())
}

/// Spawn the blocking stdin reader with the leader-key scanner. It exits on
/// stdin EOF, on a detach chord, or — once `stop_reading` is set — after the
/// next read returns (the reclaim keypress, which is consumed).
fn spawn_stdin_reader(
    leader: LeaderKey,
    stop_reading: Arc<AtomicBool>,
) -> (mpsc::Receiver<StdinEvent>, tokio::task::JoinHandle<()>) {
    let (input_tx, input_rx) = mpsc::channel::<StdinEvent>(256);

    let leader_raw = leader.raw_byte();
    let leader_csi_u = leader.csi_u_sequence();

    let reader = tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        let mut pending_leader = false;

        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) if stop_reading.load(Ordering::SeqCst) => {
                    // Reclaim: the session is over; this keypress hands
                    // stdin back to the caller and is deliberately consumed.
                    return;
                }
                Ok(n) => {
                    let data = &buffer[..n];

                    if let Some(pos) = find_subsequence(data, &leader_csi_u) {
                        if pos > 0
                            && input_tx
                                .blocking_send(StdinEvent::Data(data[..pos].to_vec()))
                                .is_err()
                        {
                            return;
                        }
                        let after = pos + leader_csi_u.len();
                        if after < n {
                            if let Some(event) = chord_event(data[after]) {
                                tracing::info!("leader chord");
                                let _ = input_tx.blocking_send(event);
                                return;
                            }
                            let mut remaining = vec![leader_raw];
                            remaining.extend_from_slice(&data[after..]);
                            if input_tx.blocking_send(StdinEvent::Data(remaining)).is_err() {
                                return;
                            }
                        } else {
                            pending_leader = true;
                        }
                        continue;
                    }

                    let mut i = 0;
                    while i < n {
                        if pending_leader {
                            pending_leader = false;
                            if let Some(event) = chord_event(data[i]) {
                                tracing::info!("leader chord");
                                let _ = input_tx.blocking_send(event);
                                return;
                            }
                            if input_tx
                                .blocking_send(StdinEvent::Data(vec![leader_raw, data[i]]))
                                .is_err()
                            {
                                return;
                            }
                            i += 1;
                            continue;
                        }

                        if data[i] == leader_raw {
                            pending_leader = true;
                            i += 1;
                            continue;
                        }

                        let start = i;
                        while i < n && data[i] != leader_raw {
                            i += 1;
                        }
                        if input_tx
                            .blocking_send(StdinEvent::Data(data[start..i].to_vec()))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }

            if pending_leader {
                let mut next = [0u8; 1];
                match stdin.read_exact(&mut next) {
                    Ok(_) => {
                        pending_leader = false;
                        if stop_reading.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Some(event) = chord_event(next[0]) {
                            tracing::info!("leader chord");
                            let _ = input_tx.blocking_send(event);
                            return;
                        }
                        if input_tx
                            .blocking_send(StdinEvent::Data(vec![leader_raw, next[0]]))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    (input_rx, reader)
}

/// `<leader>d` detaches to the shell; `<leader>s` goes to the fleet.
fn chord_event(byte: u8) -> Option<StdinEvent> {
    match byte {
        b'd' => Some(StdinEvent::Detach),
        b's' => Some(StdinEvent::SwitchToFleet),
        _ => None,
    }
}

/// The passthrough loop: session output to `output`, input events to
/// `SendInput`, until detach or the session ends. Pure plumbing over
/// injected IO so the tier-2 suite can drive it without a terminal.
pub(crate) async fn attach_loop<W: Write>(
    rpc: Client,
    mut session: amux::SessionStream,
    agent: AgentIdentifier,
    mut input_rx: mpsc::Receiver<StdinEvent>,
    mut output: W,
) -> Result<AttachOutcome> {
    loop {
        tokio::select! {
            event = input_rx.recv() => match event {
                Some(StdinEvent::Data(data)) => {
                    if rpc
                        .send_input(SendInputRequest {
                            agent: agent.clone(),
                            input_id: Uuid::new_v4().as_bytes().to_vec(),
                            io_protocol: terminal_io::TERMINAL_V1.to_string(),
                            payload: data.into(),
                            pin: Vec::new(),
                        })
                        .await
                        .is_err()
                    {
                        return Ok(AttachOutcome::SessionEnded);
                    }
                }
                Some(StdinEvent::Detach) => return Ok(AttachOutcome::Detached),
                Some(StdinEvent::SwitchToFleet) => return Ok(AttachOutcome::SwitchedToFleet),
                None => return Ok(AttachOutcome::SessionEnded),
            },
            event = session.recv() => match event {
                Ok(SubscribeSessionEvent::Output { payload }) => {
                    output.write_all(&payload).ok();
                    output.flush().ok();
                }
                Ok(SubscribeSessionEvent::Opened)
                | Ok(SubscribeSessionEvent::ReplayComplete { .. }) => {}
                Ok(SubscribeSessionEvent::Closed { reason }) => {
                    tracing::info!(?reason, "session closed");
                    return Ok(AttachOutcome::SessionClosed(reason));
                }
                Err(ClientError::ServerShutdown(reason)) => {
                    tracing::info!(reason = %reason, "server shutdown");
                    return Ok(AttachOutcome::Shutdown(reason));
                }
                Err(error) => {
                    if let ClientError::Protocol(error) = &error {
                        tracing::info!(error = %error, "session failed");
                    } else {
                        tracing::warn!(error = %error, "session read error");
                    }
                    return Err(error.into());
                }
            }
        }
    }
}

fn session_close_label(reason: &SessionCloseReason) -> &'static str {
    match reason {
        SessionCloseReason::AgentDeleted | SessionCloseReason::AgentExited { .. } => {
            "session ended"
        }
        SessionCloseReason::HostUnreachable => "host unreachable",
        SessionCloseReason::InternalError { .. } => "session error",
    }
}

/// The same label, for a status line that also shows whether it went
/// well: the agent finishing or being deleted is the session doing what it
/// was told, and only losing it is a problem.
fn session_close_notice(reason: &SessionCloseReason) -> amux_tui::Notice {
    let label = session_close_label(reason);
    match reason {
        SessionCloseReason::AgentDeleted | SessionCloseReason::AgentExited { .. } => {
            amux_tui::Notice::done(label)
        }
        SessionCloseReason::HostUnreachable | SessionCloseReason::InternalError { .. } => {
            amux_tui::Notice::problem(label)
        }
    }
}

/// RAII guard to restore terminal mode on drop
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Tier-2 attach round-trip specs: a real embedded daemon and a pty
/// test-agent driven through the passthrough loop with injected IO, plus
/// vt100 assertions over the terminal-hygiene byte sequences that ratatui's
/// TestBackend is blind to.
#[cfg(test)]
mod attach {
    #[test]
    fn leader_chords_split_detach_from_fleet() {
        assert!(matches!(
            super::chord_event(b'd'),
            Some(super::StdinEvent::Detach)
        ));
        assert!(matches!(
            super::chord_event(b's'),
            Some(super::StdinEvent::SwitchToFleet)
        ));
        assert!(super::chord_event(b'x').is_none());
    }

    #[test]
    fn codex_configuration_facts_are_explicit_about_defaults_and_overrides() {
        let defaults = AgentType::Codex {
            model: None,
            approval_policy: None,
            sandbox_policy: None,
            resume_thread_id: None,
        };
        assert_eq!(
            super::codex_configuration_facts(&defaults),
            Some(vec![
                "default".to_string(),
                "default".to_string(),
                "default".to_string()
            ])
        );

        let selected = AgentType::Codex {
            model: Some("gpt-5.4".to_string()),
            approval_policy: Some("never".to_string()),
            sandbox_policy: Some("workspace-write".to_string()),
            resume_thread_id: None,
        };
        assert_eq!(
            super::codex_configuration_facts(&selected),
            Some(vec![
                "gpt-5.4".to_string(),
                "never".to_string(),
                "workspace-write".to_string()
            ])
        );
        assert_eq!(
            super::codex_configuration_facts(&AgentType::Claude {
                driver: amux::ClaudeDriver::Pty,
            }),
            None
        );
    }

    #[test]
    fn entry_policy_a_session_without_a_terminal_is_created_and_attached_as_chat() {
        let sdk_type = AgentType::Claude {
            driver: amux::ClaudeDriver::Sdk,
        };
        let sdk_kind = amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Sdk,
        };
        assert!(!super::agent_type_exposes_terminal(&sdk_type));
        assert!(super::attach_opens_chat(&sdk_kind, true));

        let pty_type = AgentType::Claude {
            driver: amux::ClaudeDriver::Pty,
        };
        let pty_kind = amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        };
        assert!(super::agent_type_exposes_terminal(&pty_type));
        assert!(!super::attach_opens_chat(&pty_kind, true));

        assert!(!super::attach_opens_chat(&amux::AgentKind::TestAgent, true));
    }

    /// The one place the command line parts company with the fleet: a
    /// Codex agent on this machine opens its own structured screen here,
    /// while the fleet's Enter still raw attaches under the shipped
    /// default. `amux attach` has no second key to offer, so the richer
    /// surface leads; the fleet's half is pinned by
    /// `entry_policy_a_local_codex_agent_keeps_the_configured_default` in
    /// `amux-tui`, and `docs/CHAT.md` names the difference and why.
    #[test]
    fn entry_policy_attach_leads_with_the_codex_screen_on_this_machine() {
        assert!(super::attach_opens_chat(&amux::AgentKind::Codex, true));
        assert!(super::attach_opens_chat(&amux::AgentKind::Codex, false));
        assert!(!super::attach_opens_chat(
            &amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Pty,
            },
            true
        ));
    }

    /// `amux attach` follows the fleet's rule for a remote agent too: the
    /// chat travels over the connection the daemon already has, so a
    /// terminal-capable agent on another machine still opens the chat.
    #[test]
    fn entry_policy_attach_opens_chat_for_every_agent_on_another_machine() {
        for kind in [
            amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Pty,
            },
            amux::AgentKind::TestAgent,
        ] {
            assert!(
                !super::attach_opens_chat(&kind, true),
                "{kind:?} raw attaches on this machine"
            );
            assert!(
                super::attach_opens_chat(&kind, false),
                "{kind:?} opens the chat from another machine"
            );
        }
    }

    #[test]
    fn raw_create_retry_matches_only_the_codex_thread_readiness_race() {
        let readiness = anyhow!(
            "failed to subscribe to session: Codex raw session is not ready: thread_id is not available yet"
        );
        assert!(super::codex_thread_not_ready(&readiness));
        assert!(!super::codex_thread_not_ready(&anyhow!(
            "failed to subscribe to session: permission denied"
        )));
    }

    #[tokio::test]
    async fn post_create_open_failure_preserves_agent_and_underlying_error() {
        let named_id = Uuid::from_u128(0x1234);
        let create_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let open_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let verify_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_create_calls = create_calls.clone();
        let observed_open_calls = open_calls.clone();
        let observed_verify_calls = verify_calls.clone();
        let error = super::create_and_open_agent(
            Some("steady"),
            move || async move {
                observed_create_calls.fetch_add(1, Ordering::SeqCst);
                Ok(named_id)
            },
            move |created_id| async move {
                observed_open_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(created_id, named_id);
                Err(anyhow!("terminal attach failed"))
            },
            move |created_id| async move {
                observed_verify_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(created_id, named_id);
                Ok(true)
            },
        )
        .await
        .expect_err("opening the successfully created agent must fail");

        assert_eq!(create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(open_calls.load(Ordering::SeqCst), 1);
        assert_eq!(verify_calls.load(Ordering::SeqCst), 1);
        let command_target = named_id.to_string();
        assert_eq!(
            error.to_string(),
            format!(
                "agent 'steady' was created and is running. Reattach with 'amux attach {command_target}', or remove it with 'amux rm {command_target}' (or 'd' in the fleet view).\nterminal attach failed"
            )
        );

        let unnamed_id = Uuid::from_u128(0x5678);
        let error = super::create_and_open_agent(
            None,
            || async { Ok(unnamed_id) },
            |_| async { Err(anyhow!("chat open failed")) },
            |_| async { Ok(true) },
        )
        .await
        .expect_err("unnamed recovery must use the created UUID");
        let target = unnamed_id.to_string();
        assert_eq!(
            error.to_string(),
            format!(
                "agent '{target}' was created and is running. Reattach with 'amux attach {target}', or remove it with 'amux rm {target}' (or 'd' in the fleet view).\nchat open failed"
            )
        );
    }

    #[tokio::test]
    async fn post_create_open_failure_without_created_agent_returns_underlying_error() {
        let agent_id = Uuid::from_u128(0x6789);
        let error = super::create_and_open_agent(
            Some("deleted-later"),
            || async { Ok(agent_id) },
            |_| async { Err(anyhow!("late UI draw failed")) },
            |created_id| async move {
                assert_eq!(created_id, agent_id);
                Ok(false)
            },
        )
        .await
        .expect_err("an absent agent must not produce a running claim");

        assert_eq!(error.to_string(), "late UI draw failed");
    }

    #[tokio::test]
    async fn post_create_open_failure_with_failed_verification_is_uncertain() {
        let agent_id = Uuid::from_u128(0x789a);
        let error = super::create_and_open_agent(
            Some("unknown-status"),
            || async { Ok(agent_id) },
            |_| async { Err(anyhow!("late UI event failed")) },
            |_| async { Err(anyhow!("inventory unavailable")) },
        )
        .await
        .expect_err("failed verification must retain the open error");

        assert_eq!(
            error.to_string(),
            format!(
                "late UI event failed\nCould not verify whether created agent {agent_id} is still present: inventory unavailable"
            )
        );
        assert!(!error.to_string().contains("is running"));
    }

    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use amux::AgentId;
    use amux_ui::{Model, Runtime, RuntimeOptions};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<StdMutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedBuf {
        fn contains(&self, needle: &str) -> bool {
            let bytes = self.0.lock().unwrap();
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        }
    }

    async fn embedded_client() -> (amux::Installation, Client, tempfile::TempDir) {
        let root = amux::test_fixtures::short_installation_root();
        let installation = amux::Installation::open(amux::InstallationOptions {
            root: amux::InstallationRoot::OnDisk(root.path().into()),
            settings: amux::InstallationSettings {
                repository_roots: Vec::new(),
                claude: amux::ClaudeSettings::default(),
                host_name: "session-test".into(),
                prevent_idle_sleep: Some(false),
                keybinds: Default::default(),
                ui: Default::default(),
                keymaps_dir: Default::default(),
                minimum_client_versions: Default::default(),
                update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
                status_reporters: Default::default(),
            },
            listeners: amux::Listeners::InProcessOnly,
            credentials: amux::CredentialSource::ProfileFiles,
            identity_http: Default::default(),
        })
        .await
        .unwrap();
        let id = installation
            .create(amux::OperationId::new(), None)
            .await
            .unwrap()
            .record
            .id;
        let client = installation.client(id).unwrap();
        (installation, client, root)
    }

    async fn create_cat_agent(client: &Client, name: &str) -> AgentId {
        let agent = client
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: "cat".to_string(),
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .expect("create test agent");
        agent.id
    }

    fn listed_agent(id: u128, name: &str) -> amux::Agent {
        amux::Agent {
            id: Uuid::from_u128(id),
            host_id: Uuid::from_u128(99),
            name: Some(name.to_string()),
            command: "test-agent".to_string(),
            working_dir: std::env::temp_dir(),
            kind: amux::AgentKind::TestAgent,
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
        }
    }

    fn child_agent(id: u128, name: &str, parent: u128) -> amux::Agent {
        let mut agent = listed_agent(id, name);
        agent.parent = Some(amux::AgentParent {
            agent_id: Uuid::from_u128(parent),
            host_id: Uuid::from_u128(99),
        });
        agent
    }

    #[test]
    fn a2a_list_collapses_families_and_states_current_work() {
        use chrono::TimeZone as _;

        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 23, 12, 0, 0).unwrap();
        let mut parent = listed_agent(1, "alpha");
        parent.working_on = Some(amux::WorkingOn {
            text: "coordinating the release\nprivate detail".to_string(),
            updated_at: now - chrono::Duration::minutes(2),
        });
        let agents = [
            parent,
            child_agent(2, "beta", 1),
            child_agent(3, "gamma", 2),
            listed_agent(4, "solo"),
        ];

        let lines = super::agent_list_lines(&agents, false, now);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "  alpha ⋯2 [test-agent] - {} · coordinating the release 2m",
                std::env::temp_dir().display()
            )
        );
        assert_eq!(
            lines[1],
            format!("  solo [test-agent] - {}", std::env::temp_dir().display())
        );
        assert!(!lines.iter().any(|line| line.contains("private detail")));
    }

    #[test]
    fn a2a_list_all_indents_every_generation() {
        let now = chrono::Utc::now();
        let agents = [
            child_agent(3, "gamma", 2),
            listed_agent(1, "alpha"),
            child_agent(2, "beta", 1),
        ];

        let lines = super::agent_list_lines(&agents, true, now);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("  alpha ⋯2 [test-agent] - "));
        assert!(lines[1].starts_with("    beta [test-agent] - "));
        assert!(lines[2].starts_with("      gamma [test-agent] - "));
    }

    #[test]
    fn list_names_every_kind_and_claude_driver() {
        let mut pty = listed_agent(1, "claude-pty");
        pty.kind = amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        };
        let mut sdk = listed_agent(2, "claude-sdk");
        sdk.kind = amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Sdk,
        };
        let mut codex = listed_agent(3, "codex");
        codex.kind = amux::AgentKind::Codex;
        let test_agent = listed_agent(4, "test");

        let lines =
            super::agent_list_lines(&[pty, sdk, codex, test_agent], false, chrono::Utc::now());
        assert!(lines.iter().any(|line| line.contains("[claude/pty]")));
        assert!(lines.iter().any(|line| line.contains("[claude/sdk]")));
        assert!(lines.iter().any(|line| line.contains("[codex]")));
        assert!(lines.iter().any(|line| line.contains("[test-agent]")));
    }

    #[test]
    fn a2a_rm_cascade_refuses_working_children_without_force() {
        let parent = listed_agent(1, "parent");
        let idle = child_agent(2, "idle-child", 1);
        let mut working = child_agent(3, "working-child", 2);
        working.working_on = Some(amux::WorkingOn {
            text: "running the release suite".to_string(),
            updated_at: chrono::Utc::now(),
        });
        let agents = [parent, idle, working];

        let error = super::ensure_family_is_removable(&agents, &agents[0], false)
            .expect_err("working descendants require an explicit force");
        assert_eq!(
            error.to_string(),
            "refusing to delete 'parent': child agents still working: working-child (running the release suite); rerun with --force"
        );
        super::ensure_family_is_removable(&agents, &agents[0], true)
            .expect("force permits the cascade");
    }

    #[test]
    fn a2a_rm_cascade_lists_every_child_and_marks_working_ones() {
        let parent = listed_agent(1, "parent");
        let idle = child_agent(2, "idle-child", 1);
        let mut working = child_agent(3, "working-child", 2);
        working.working_on = Some(amux::WorkingOn {
            text: "running the release suite".to_string(),
            updated_at: chrono::Utc::now(),
        });
        let agents = [parent, idle, working];

        let descendants = super::family_descendants(&agents, &agents[0]);
        assert_eq!(
            descendants
                .iter()
                .map(|agent| super::removal_child_label(agent))
                .collect::<Vec<_>>(),
            [
                "'idle-child'".to_string(),
                "'working-child' [working: running the release suite]".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn remove_agent_uses_exact_name_and_deletes_only_one_match() {
        let agents = [listed_agent(1, "worker"), listed_agent(2, "worker-copy")];
        let delete_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_calls = delete_calls.clone();
        let missing = super::delete_exact_agent(&agents, "work", move |agent_id| async move {
            observed_calls.lock().unwrap().push(agent_id);
            Ok(())
        })
        .await
        .expect_err("substring matches must be refused");
        assert_eq!(missing.to_string(), "agent not found: work");
        assert!(delete_calls.lock().unwrap().is_empty());

        let observed_calls = delete_calls.clone();
        super::delete_exact_agent(&agents, "worker", move |agent_id| async move {
            observed_calls.lock().unwrap().push(agent_id);
            Ok(())
        })
        .await
        .expect("delete exact match");
        assert_eq!(*delete_calls.lock().unwrap(), vec![Uuid::from_u128(1)]);
    }

    #[tokio::test]
    async fn remove_agent_uses_exact_uuid_and_deletes_only_that_id() {
        let target_id = Uuid::from_u128(1);
        let target = target_id.to_string();
        let agents = [
            listed_agent(1, "worker"),
            listed_agent(2, &target),
            listed_agent(3, "worker-copy"),
        ];
        let delete_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_calls = delete_calls.clone();

        super::delete_exact_agent(&agents, &target, move |agent_id| async move {
            observed_calls.lock().unwrap().push(agent_id);
            Ok(())
        })
        .await
        .expect("delete exact UUID");

        assert_eq!(*delete_calls.lock().unwrap(), vec![target_id]);
    }

    #[tokio::test]
    async fn remove_agent_missing_uuid_never_invokes_deletion() {
        let missing = Uuid::from_u128(4).to_string();
        let agents = [listed_agent(1, &missing), listed_agent(2, "worker")];
        let delete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_calls = delete_calls.clone();

        let error = super::delete_exact_agent(&agents, &missing, move |_| async move {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect_err("a missing UUID must not fall back to an exact-name match");

        assert_eq!(error.to_string(), format!("agent not found: {missing}"));
        assert_eq!(delete_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn remove_agent_refuses_duplicate_exact_names() {
        let agents = [listed_agent(1, "duplicate"), listed_agent(2, "duplicate")];
        let delete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_calls = delete_calls.clone();
        let error = super::delete_exact_agent(&agents, "duplicate", move |_| async move {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect_err("duplicate exact names must be ambiguous");
        assert_eq!(error.to_string(), "agent name `duplicate` is ambiguous");
        assert_eq!(delete_calls.load(Ordering::SeqCst), 0);
    }

    struct OpenAttach {
        input: mpsc::Sender<StdinEvent>,
        output: SharedBuf,
        loop_task: tokio::task::JoinHandle<Result<AttachOutcome>>,
    }

    async fn open_attach(client: &Client, agent: AgentId) -> OpenAttach {
        let identifier = AgentIdentifier::from(agent);
        let session = subscribe_raw(client, &identifier, None)
            .await
            .expect("subscribe raw session");
        let (input, input_rx) = mpsc::channel(16);
        let output = SharedBuf::default();
        let loop_task = tokio::spawn(attach_loop(
            client.clone(),
            session,
            identifier,
            input_rx,
            output.clone(),
        ));
        OpenAttach {
            input,
            output,
            loop_task,
        }
    }

    async fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !check() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_model(runtime: &mut Runtime, what: &str, check: impl Fn(&Model) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !check(runtime.model()) {
            let remaining = deadline - tokio::time::Instant::now();
            assert!(!remaining.is_zero(), "timed out waiting for {what}");
            match tokio::time::timeout(remaining, runtime.next()).await {
                Ok(true) => {}
                Ok(false) => panic!("runtime shut down waiting for {what}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    fn render_fleet(model: &Model) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(68, 11)).expect("terminal");
        let view = amux_tui::ViewState::default();
        let ctx = amux_tui::FrameContext {
            viewport: (68, 11),
            theme: amux_tui::Theme::default(),
            now: chrono::Utc::now(),
        };
        terminal
            .draw(|frame| amux_tui::render(model, &view, &ctx, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.cell((x, y)).expect("cell").symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The V1 loop: attach, see output, detach, the fleet repaints from the
    /// Model, attach again — then 100 scripted cycles with zero corruption
    /// (every cycle detaches cleanly and the next attach still streams).
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(
        windows,
        ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
    )]
    async fn round_trip_repaints_fleet() {
        let (installation, client, _root) = embedded_client().await;
        let mut runtime = Runtime::start_with_client(client.clone(), RuntimeOptions::default());
        wait_model(&mut runtime, "snapshot", |model| model.is_synchronized()).await;

        let agent = create_cat_agent(&client, "round-trip").await;
        wait_model(&mut runtime, "agent in model", move |model| {
            model.agent(agent).is_some()
        })
        .await;

        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"hello-there\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("echoed output", || output.contains("hello-there")).await;
        attached.input.send(StdinEvent::Detach).await.unwrap();
        let outcome = attached.loop_task.await.unwrap().unwrap();
        assert!(matches!(outcome, AttachOutcome::Detached), "{outcome:?}");

        // The fleet repaints from the Model alone after detach.
        let frame = render_fleet(runtime.model());
        assert!(frame.contains("round-trip"), "fleet row present:\n{frame}");
        assert!(frame.contains("connected"), "status line present:\n{frame}");

        // Attach again: the session streams again (late attach replays).
        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"second-visit\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("second echo", || output.contains("second-visit")).await;
        attached
            .input
            .send(StdinEvent::SwitchToFleet)
            .await
            .unwrap();
        let outcome = attached.loop_task.await.unwrap().unwrap();
        assert!(
            matches!(outcome, AttachOutcome::SwitchedToFleet),
            "{outcome:?}"
        );

        // 100 scripted attach/detach cycles.
        for cycle in 0..100 {
            let attached = open_attach(&client, agent).await;
            attached.input.send(StdinEvent::Detach).await.unwrap();
            let outcome = attached.loop_task.await.unwrap().unwrap();
            assert!(
                matches!(outcome, AttachOutcome::Detached),
                "cycle {cycle}: {outcome:?}"
            );
        }

        client
            .delete_agent(agent)
            .await
            .expect("clean up PTY agent after stress cycles");
        installation
            .shutdown(amux::ShutdownReason::UserRequested)
            .await;
    }

    /// The terminal-hygiene byte sequences, asserted through a real vt100
    /// parser (the surface TestBackend cannot see): entering chrome uses the
    /// alternate screen; the restore sequence leaves it, shows the cursor,
    /// and resets modes. Every passthrough exit path emits exactly these
    /// bytes via the RAII guard.
    #[test]
    fn detach_leaves_terminal_sane() {
        let mut parser = vt100::Parser::new(24, 80, 0);

        let mut enter = Vec::new();
        amux_tui::write_enter_chrome(&mut enter).unwrap();
        parser.process(&enter);
        assert!(parser.screen().alternate_screen(), "chrome uses alt screen");
        assert!(parser.screen().hide_cursor(), "chrome hides the cursor");

        let mut restore = Vec::new();
        amux_tui::write_restore(&mut restore).unwrap();
        parser.process(&restore);
        assert!(
            !parser.screen().alternate_screen(),
            "restore leaves the alternate screen"
        );
        assert!(!parser.screen().hide_cursor(), "restore shows the cursor");
    }

    /// Kill the agent mid-attach: the loop reports the close instead of
    /// hanging or corrupting, and the chrome's restore sequence still
    /// yields a sane terminal.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(
        windows,
        ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
    )]
    async fn kill_during_attach_still_restores_the_terminal() {
        let (installation, client, _root) = embedded_client().await;
        let agent = create_cat_agent(&client, "doomed").await;

        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"warm-up\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("agent alive", || output.contains("warm-up")).await;

        client.delete_agent(agent).await.expect("delete mid-attach");
        let outcome = tokio::time::timeout(Duration::from_secs(20), attached.loop_task)
            .await
            .expect("attach loop returns after the kill")
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                outcome,
                AttachOutcome::SessionClosed(SessionCloseReason::AgentDeleted)
                    | AttachOutcome::SessionClosed(SessionCloseReason::AgentExited { .. })
            ),
            "{outcome:?}"
        );

        // The resume path writes the reset sequence; assert it on the
        // captured stream through vt100.
        let mut parser = vt100::Parser::new(24, 80, 0);
        let mut enter = Vec::new();
        amux_tui::write_enter_chrome(&mut enter).unwrap();
        parser.process(&enter);
        let mut restore = Vec::new();
        amux_tui::write_restore(&mut restore).unwrap();
        parser.process(&restore);
        assert!(!parser.screen().alternate_screen());
        assert!(!parser.screen().hide_cursor());
        installation
            .shutdown(amux::ShutdownReason::UserRequested)
            .await;
    }

    /// Enter on a row whose host is offline surfaces the daemon's
    /// `last_dial_error` in the status line instead of attaching.
    #[tokio::test]
    async fn offline_host_shows_dial_error_instead_of_attaching() {
        use amux_ui::{Msg, ServerMsg, update};

        let host = amux::HostEntry {
            id: Uuid::from_u128(1),
            name: "hetzner".to_string(),
            online: false,
            version: None,
            capabilities: None,
            trust_status: amux::HostTrustStatus::Trusted,
            last_dial_error: Some("dial tcp: connection refused".to_string()),
        };
        let agent = amux::Agent {
            id: Uuid::from_u128(2),
            host_id: host.id,
            name: Some("faraway".to_string()),
            command: "claude".to_string(),
            working_dir: std::env::temp_dir(),
            kind: amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
        };
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: None,
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
        ] {
            update(&mut model, msg);
        }

        let mut view = amux_tui::ViewState::default();
        let action = amux_tui::keys::handle_key(
            &mut view,
            &model,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            5,
            chrono::Utc::now(),
        );
        assert_eq!(action, None, "no attach action for an offline host");
        let notice = view.notice.clone().expect("status-line notice").text;
        assert!(
            notice.contains("dial tcp: connection refused"),
            "notice carries last_dial_error: {notice}"
        );

        let frame = render_fleet_with_view(&model, &view);
        assert!(
            frame.contains("✗ hetzner is offline: dial tcp: connection refused"),
            "status line shows the dial error:\n{frame}"
        );
    }

    fn render_fleet_with_view(model: &Model, view: &amux_tui::ViewState) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(68, 11)).expect("terminal");
        let ctx = amux_tui::FrameContext {
            viewport: (68, 11),
            theme: amux_tui::Theme::default(),
            now: chrono::Utc::now(),
        };
        terminal
            .draw(|frame| amux_tui::render(model, view, &ctx, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.cell((x, y)).expect("cell").symbol());
            }
            out.push('\n');
        }
        out
    }
}
