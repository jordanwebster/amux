use amux::protocol::{
    AgentType, Command, CreateAgentRequest, Message, RoutableMessage, ServerDebugInfo,
    ShutdownReason, TerminalSize,
};
use amux::{
    AmuxError, Config, ConnectPolicy, Connection, DaemonOptions, LeaderKey, Result, Route, connect,
};
use crossterm::terminal;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use uuid::Uuid;

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
    Shutdown(ShutdownReason),
    Error(AmuxError),
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

fn cli_daemon_policy() -> Result<ConnectPolicy> {
    let executable = std::env::current_exe().map_err(|e| {
        AmuxError::Config(format!("failed to determine current executable path: {e}"))
    })?;
    Ok(ConnectPolicy::SpawnDaemon(DaemonOptions::new(executable)))
}

/// Create a new agent and attach to it
pub async fn new_agent(
    name: Option<&str>,
    agent_type: AgentType,
    args: Vec<String>,
    config: &Config,
) -> Result<()> {
    let conn = connect(config, cli_daemon_policy()?).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    // Create route with just our link (local agent)
    let full_route = Route::from_link(conn.link_name());
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    let request_counter = AtomicU64::new(1);

    // Send CreateAgent
    conn.send(&Message::routable(
        src,
        dst,
        request_counter.fetch_add(1, Ordering::Relaxed),
        &RoutableMessage::CreateAgent(CreateAgentRequest {
            agent_id,
            name: name.map(|s| s.to_string()),
            agent_type,
            working_dir: working_dir.clone(),
            terminal_size: Some(terminal_size),
            args,
        }),
    ))
    .await?;

    match conn.recv().await? {
        Message::Routable { payload, .. } => match RoutableMessage::decode(&payload)? {
            RoutableMessage::CreateAgentResult { error: None, .. } => {
                subscribe_and_stream(
                    conn,
                    agent_id,
                    full_route,
                    Some(terminal_size),
                    request_counter,
                    config.keybinds.leader.clone(),
                )
                .await
            }
            RoutableMessage::CreateAgentResult { error: Some(e), .. } => Err(
                AmuxError::ServerError(format!("failed to create agent: {e}")),
            ),
            other => Err(AmuxError::InvalidMessage(format!(
                "expected CreateAgentResult, got {}",
                other.type_label()
            ))),
        },
        other => Err(AmuxError::InvalidMessage(format!(
            "expected Routable(CreateAgentResult), got {}",
            other.type_label()
        ))),
    }
}

/// Attach to an existing agent
pub async fn attach(target: Option<&str>, config: &Config) -> Result<()> {
    let conn = connect(config, cli_daemon_policy()?).await?;
    let terminal_size = get_terminal_size();

    // Resolve the target to (route, agent_id)
    let (mut route_suffix, agent_id) = match target {
        Some(identifier) => {
            conn.send(&Message::Command(Command::ResolveAgent {
                identifier: identifier.to_string(),
            }))
            .await?;

            let response = conn.recv().await?;
            match response {
                Message::Command(Command::ResolveAgentResult {
                    agent: Some(info), ..
                }) => (info.route, info.id),
                Message::Command(Command::ResolveAgentResult { agent: None }) => {
                    return Err(AmuxError::AgentNotFound(identifier.to_string()));
                }
                other => {
                    return Err(AmuxError::InvalidMessage(format!(
                        "expected ResolveAgentResult, got {}",
                        other.type_label()
                    )));
                }
            }
        }
        None => {
            conn.send(&Message::Command(Command::ListAgents)).await?;

            let response = conn.recv().await?;
            match response {
                Message::Command(Command::ListAgentsResult { agents }) if !agents.is_empty() => {
                    (agents[0].route.clone(), agents[0].id)
                }
                Message::Command(Command::ListAgentsResult { .. }) => {
                    eprintln!("No agents running. Use 'amux new' to create one.");
                    return Ok(());
                }
                other => {
                    return Err(AmuxError::InvalidMessage(format!(
                        "expected ListAgentsResult, got {}",
                        other.type_label()
                    )));
                }
            }
        }
    };

    // Build full route: our link first, then any additional route
    let full_route = {
        route_suffix.push(conn.link_name());
        route_suffix
    };
    tracing::info!(agent_id = %agent_id, route = %full_route, "attaching");

    let request_counter = AtomicU64::new(1);
    subscribe_and_stream(
        conn,
        agent_id,
        full_route,
        Some(terminal_size),
        request_counter,
        config.keybinds.leader.clone(),
    )
    .await
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    conn: Connection,
    agent_id: Uuid,
    full_route: Route,
    terminal_size: Option<TerminalSize>,
    request_counter: AtomicU64,
    leader: LeaderKey,
) -> Result<()> {
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    conn.send(&Message::routable(
        src,
        dst,
        request_counter.fetch_add(1, Ordering::Relaxed),
        &RoutableMessage::SubscribeRaw {
            agent_id,
            terminal_size,
        },
    ))
    .await?;

    let response = conn.recv().await?;
    match response {
        Message::Routable { payload, .. } => match RoutableMessage::decode(&payload)? {
            RoutableMessage::SubscribeRawResult { error: None, .. } => {
                tracing::info!(agent_id = %agent_id, "subscribed");
            }
            RoutableMessage::SubscribeRawResult { error: Some(e), .. } => {
                eprintln!("Failed to subscribe: {}", e);
                return Ok(());
            }
            other => {
                return Err(AmuxError::InvalidMessage(format!(
                    "expected SubscribeRawResult, got {}",
                    other.type_label()
                )));
            }
        },
        other => {
            return Err(AmuxError::InvalidMessage(format!(
                "expected Routable(SubscribeRawResult), got {}",
                other.type_label()
            )));
        }
    }

    run_attached(conn, agent_id, full_route, request_counter, leader).await
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let conn = match connect(config, cli_daemon_policy()?).await {
        Ok(conn) => conn,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No agents running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    conn.send(&Message::Command(Command::ListAgents)).await?;

    let response = conn.recv().await?;
    match response {
        Message::Command(Command::ListAgentsResult { mut agents }) => {
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
                println!("Running agents:");
                for agent in agents {
                    let agent_id_str = agent.id.to_string();
                    let display_name = agent.name.as_deref().unwrap_or(&agent_id_str);
                    if agent.is_remote() {
                        println!(
                            "  {} - {} (via {})",
                            display_name,
                            agent.working_dir.display(),
                            agent.route
                        );
                    } else {
                        println!("  {} - {}", display_name, agent.working_dir.display());
                    }
                }
            }
        }
        other => {
            return Err(AmuxError::InvalidMessage(format!(
                "expected ListAgentsResult, got {}",
                other.type_label()
            )));
        }
    }

    Ok(())
}

/// Kill all agents and shut down the server
pub async fn kill_server(config: &Config) -> Result<()> {
    let conn = match connect(config, ConnectPolicy::ExistingOnly).await {
        Ok(conn) => conn,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No server running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    conn.send(&Message::Command(Command::Shutdown)).await?;

    match conn.recv().await {
        Ok(Message::Command(Command::ShutdownNotification(ShutdownReason::UserRequested))) => {}
        Ok(other) => {
            tracing::warn!(?other, "unexpected shutdown response");
        }
        Err(e) => {
            tracing::warn!(error = %e, "error reading shutdown response");
        }
    }

    println!("Server shutting down.");
    Ok(())
}

/// Connect to a remote amux server
pub async fn connect_remote(address: &str, config: &Config) -> Result<()> {
    let conn = connect(config, cli_daemon_policy()?).await?;

    conn.send(&Message::Command(Command::ConnectToServer {
        address: address.to_string(),
    }))
    .await?;

    let response = conn.recv().await?;
    match response {
        Message::Command(Command::ConnectToServerResult { error: None }) => {
            println!("Connected to {}", address);
            Ok(())
        }
        Message::Command(Command::ConnectToServerResult { error: Some(e) }) => Err(
            AmuxError::ServerError(format!("failed to connect to {address}: {e}")),
        ),
        other => Err(AmuxError::InvalidMessage(format!(
            "expected ConnectToServerResult, got {}",
            other.type_label()
        ))),
    }
}

/// Get server debug information
pub async fn debug(config: &Config) -> Result<ServerDebugInfo> {
    let conn = match connect(config, ConnectPolicy::ExistingOnly).await {
        Ok(conn) => conn,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            return Err(AmuxError::ServerError("No server running".to_string()));
        }
        Err(e) => return Err(e),
    };

    conn.send(&Message::Command(Command::Debug)).await?;

    let response = conn.recv().await?;
    match response {
        Message::Command(Command::DebugResult { info }) => Ok(info),
        other => Err(AmuxError::InvalidMessage(format!(
            "expected DebugResult, got {}",
            other.type_label()
        ))),
    }
}

/// Run the attached session (streaming mode with leader key handling)
async fn run_attached(
    conn: Connection,
    agent_id: Uuid,
    full_route: Route,
    request_counter: AtomicU64,
    leader: LeaderKey,
) -> Result<()> {
    let _raw_guard = RawModeGuard::new()?;

    let conn = Arc::new(conn);

    let (input_tx, mut input_rx) = mpsc::channel::<StdinEvent>(256);

    let leader_raw = leader.raw_byte();
    let leader_csi_u = leader.csi_u_sequence();

    // Task: Read stdin, handle leader key (both raw byte and CSI u format)
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
            event = input_rx.recv() => {
                match event {
                    Some(StdinEvent::Data(data)) => {
                        let (src, dst) = Route::send(full_route.clone())
                            .expect("full_route should have at least one link");
                        if conn.send(&Message::routable(
                            src,
                            dst,
                            request_counter.fetch_add(1, Ordering::Relaxed),
                            &RoutableMessage::RawInput {
                                agent_id,
                                data,
                            },
                        )).await.is_err() {
                            break ExitReason::SessionEnded;
                        }
                    }
                    Some(StdinEvent::Detach) => break ExitReason::Detached,
                    None => break ExitReason::SessionEnded,
                }
            }
            msg = conn.recv() => {
                match msg {
                    Ok(Message::Routable { payload, .. }) => {
                        match RoutableMessage::decode(&payload) {
                            Ok(RoutableMessage::RawOutput { data, .. }) => {
                                io::stdout().write_all(&data).ok();
                                io::stdout().flush().ok();
                            }
                            Ok(RoutableMessage::SubscriptionClosed { .. }) => {
                                tracing::info!("agent ended");
                                break ExitReason::SessionEnded;
                            }
                            _ => {}
                        }
                    }
                    Ok(Message::Command(Command::ShutdownNotification(reason))) => {
                        tracing::info!(reason = %reason, "server shutdown");
                        break ExitReason::Shutdown(reason);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "read error");
                        break ExitReason::Error(e);
                    }
                    _ => {}
                }
            }
        }
    };

    drop(_raw_guard);

    match exit_reason {
        ExitReason::Error(e) => return Err(e),
        ExitReason::Shutdown(reason) => {
            println!("\n[{}]", reason);
            std::process::exit(1);
        }
        ExitReason::Detached => {
            println!("\n[detached from session]");
        }
        ExitReason::SessionEnded => {
            println!("\n[session ended]");
            std::process::exit(0);
        }
    }

    Ok(())
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
