use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::Path;

use amux::claude_io::{self, ClaudeRawV1Args};
use amux::{
    AgentIdentifier, AgentType, Client, ClientError, Config, CreateAgentRequest, LeaderKey,
    SendInputRequest, SessionCloseReason, ShutdownReason, SubscribeSessionEvent,
    SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Result, anyhow};
use crossterm::terminal;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::client_common::{get_client, print_update_banner};

/// Events from stdin reading task
enum StdinEvent {
    /// Raw input data to send to agent
    Data(Vec<u8>),
    /// User requested detach (<leader>d)
    Detach,
}

/// Why the attached session's main loop exited.
enum ExitReason {
    Detached,
    SessionEnded,
    SessionClosed(SessionCloseReason),
    Shutdown(ShutdownReason),
    Error(anyhow::Error),
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

/// Create a new agent and attach to it
pub async fn new_agent(
    name: Option<&str>,
    agent_type: AgentType,
    args: Vec<String>,
    config: &Config,
) -> Result<()> {
    let rpc = get_client(config).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    let agent = rpc
        .create_agent(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: name.map(|s| s.to_string()),
            agent_type,
            working_dir: working_dir.clone(),
            terminal_size: Some(terminal_size),
            args,
        })
        .await
        .map_err(|error| anyhow!("failed to create agent: {error}"))?;

    subscribe_and_stream(
        rpc,
        AgentIdentifier::from(agent.id),
        Some(terminal_size),
        config.keybinds.leader.clone(),
        &config.state_path,
    )
    .await
}

/// Attach to an existing agent
pub async fn attach(target: Option<&str>, config: &Config) -> Result<()> {
    let rpc = get_client(config).await?;
    let terminal_size = get_terminal_size();

    let agent = match target {
        Some(identifier) => AgentIdentifier::from(identifier),
        None => {
            let agents = rpc.list_agents().await?;
            if let Some(agent) = agents.first() {
                AgentIdentifier::from(agent.id)
            } else {
                eprintln!("No agents running. Use 'amux new' to create one.");
                return Ok(());
            }
        }
    };

    tracing::info!(?agent, "attaching");

    subscribe_and_stream(
        rpc,
        agent,
        Some(terminal_size),
        config.keybinds.leader.clone(),
        &config.state_path,
    )
    .await
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let rpc = get_client(config).await?;
    let mut agents = rpc.list_agents().await?;
    if agents.is_empty() {
        println!("No agents running.");
    } else {
        agents.sort_by(|a, b| {
            let a_id = a.id.to_string();
            let b_id = b.id.to_string();
            let a_name = a.name.as_deref().unwrap_or(&a_id);
            let b_name = b.name.as_deref().unwrap_or(&b_id);
            a_name.cmp(b_name)
        });
        let multiple_hosts = agents
            .iter()
            .map(|agent| agent.host_id)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let mut name_counts = HashMap::new();
        for agent in &agents {
            if let Some(name) = &agent.name {
                *name_counts.entry(name.clone()).or_insert(0usize) += 1;
            }
        }
        println!("Running agents:");
        for agent in agents {
            let agent_id_str = agent.id.to_string();
            let display_name = agent.name.as_deref().unwrap_or(&agent_id_str);
            let name_is_ambiguous = agent
                .name
                .as_ref()
                .and_then(|name| name_counts.get(name))
                .is_some_and(|count| *count > 1);
            let mut labels = Vec::new();
            if name_is_ambiguous {
                labels.push(format!("id {}", agent.id));
            }
            if multiple_hosts {
                labels.push(format!("host {}", short_uuid(agent.host_id)));
            }
            if labels.is_empty() {
                println!("  {} - {}", display_name, agent.working_dir.display());
            } else {
                println!(
                    "  {} ({}) - {}",
                    display_name,
                    labels.join(", "),
                    agent.working_dir.display()
                );
            }
        }
    }

    print_update_banner(&config.state_path);
    Ok(())
}

fn short_uuid(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

async fn subscribe_and_stream(
    rpc: Client,
    agent: AgentIdentifier,
    terminal_size: Option<TerminalSize>,
    leader: LeaderKey,
    state_path: &Path,
) -> Result<()> {
    let session = rpc
        .subscribe_session(SubscribeSessionRequest {
            agent: agent.clone(),
            io_protocol: claude_io::RAW_V1.to_string(),
            args: claude_io::encode_raw_v1_args(ClaudeRawV1Args {
                terminal_size,
                replay_query: None,
            })
            .map(Into::into),
        })
        .await
        .map_err(|error| anyhow!("failed to subscribe to session: {error}"))?;

    tracing::info!(?agent, "subscribed to raw session");

    run_attached(rpc, session, agent, leader, state_path).await
}

fn handle_subscribe_session_event(event: SubscribeSessionEvent) {
    match event {
        SubscribeSessionEvent::Output { payload } => {
            io::stdout().write_all(&payload).ok();
            io::stdout().flush().ok();
        }
        SubscribeSessionEvent::Opened | SubscribeSessionEvent::ReplayComplete { .. } => {}
        SubscribeSessionEvent::Closed { reason } => {
            tracing::info!(?reason, "session closed");
        }
    }
}

async fn run_attached(
    rpc: Client,
    mut session: amux::SessionStream,
    agent: AgentIdentifier,
    leader: LeaderKey,
    state_path: &Path,
) -> Result<()> {
    let raw_mode_guard = RawModeGuard::new()?;

    let (input_tx, mut input_rx) = mpsc::channel::<StdinEvent>(256);

    let leader_raw = leader.raw_byte();
    let leader_csi_u = leader.csi_u_sequence();

    tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        let mut pending_leader = false;

        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
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
                            if data[after] == b'd' {
                                tracing::info!("detaching");
                                let _ = input_tx.blocking_send(StdinEvent::Detach);
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
                            if data[i] == b'd' {
                                tracing::info!("detaching");
                                let _ = input_tx.blocking_send(StdinEvent::Detach);
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
                        if next[0] == b'd' {
                            tracing::info!("detaching");
                            let _ = input_tx.blocking_send(StdinEvent::Detach);
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

    let exit_reason: ExitReason = loop {
        tokio::select! {
            event = input_rx.recv() => match event {
                Some(StdinEvent::Data(data)) => {
                    if rpc
                        .send_input(SendInputRequest {
                            agent: agent.clone(),
                            io_protocol: claude_io::RAW_V1.to_string(),
                            payload: data.into(),
                        })
                        .await
                        .is_err()
                    {
                        break ExitReason::SessionEnded;
                    }
                }
                Some(StdinEvent::Detach) => break ExitReason::Detached,
                None => break ExitReason::SessionEnded,
            },
            event = session.recv() => match event {
                Ok(event) => {
                    if let SubscribeSessionEvent::Closed { reason } = event {
                        tracing::info!(?reason, "session closed");
                        break ExitReason::SessionClosed(reason);
                    }
                    handle_subscribe_session_event(event);
                }
                Err(ClientError::ServerShutdown(reason)) => {
                    tracing::info!(reason = %reason, "server shutdown");
                    break ExitReason::Shutdown(reason);
                }
                Err(error) => {
                    if let ClientError::Protocol(error) = &error {
                        tracing::info!(error = %error, "session failed");
                    } else {
                        tracing::warn!(error = %error, "session read error");
                    }
                    break ExitReason::Error(error.into());
                }
            }
        }
    };

    drop(raw_mode_guard);

    match exit_reason {
        ExitReason::Error(e) => return Err(e),
        ExitReason::Shutdown(reason) => {
            println!("\n[{}]", reason);
            if reason != ShutdownReason::Updating {
                print_update_banner(state_path);
            }
            std::process::exit(1);
        }
        ExitReason::Detached => {
            println!("\n[detached from session]");
        }
        ExitReason::SessionEnded => {
            println!("\n[session ended]");
            print_update_banner(state_path);
            std::process::exit(0);
        }
        ExitReason::SessionClosed(reason) => {
            println!("\n[{}]", session_close_label(&reason));
            print_update_banner(state_path);
            std::process::exit(0);
        }
    }

    print_update_banner(state_path);
    Ok(())
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
