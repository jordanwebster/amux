use crate::client_common::{cli_daemon_policy, print_update_banner};
use amux::protocol::{
    AgentType, Command, CreateAgentRequest, Message, ProtocolError, RoutableMessage,
    ShutdownReason, SubscriptionId, TerminalSize,
};
use amux::{AmuxError, Config, Connection, LeaderKey, Result, Route, connect};
use crossterm::terminal;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
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

struct AttachedSession {
    agent_id: Uuid,
    subscription_id: SubscriptionId,
    lease_ms: u64,
    full_route: Route,
    request_counter: Arc<AtomicU64>,
}

impl AttachedSession {
    fn renewal_interval(&self) -> Duration {
        Duration::from_millis((self.lease_ms / 3).max(1))
    }

    async fn send(&self, conn: &Connection, message: RoutableMessage) -> Result<()> {
        let (src, dst) =
            Route::send(self.full_route.clone()).expect("full_route should have at least one link");
        conn.send(&Message::routable(
            src,
            dst,
            self.request_counter.fetch_add(1, Ordering::Relaxed),
            &message,
        ))
        .await
    }

    async fn send_raw_input(&self, conn: &Connection, data: Vec<u8>) -> Result<()> {
        self.send(
            conn,
            RoutableMessage::RawInput {
                agent_id: self.agent_id,
                data,
            },
        )
        .await
    }
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
    let conn = connect(config, cli_daemon_policy()?).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    let full_route = Route::from_link(conn.link_name());
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    let request_counter = Arc::new(AtomicU64::new(1));

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
                    &config.state_path,
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

    let (mut route_suffix, agent_id) = match target {
        Some(identifier) => {
            conn.send(&Message::Command {
                command: Command::ResolveAgent {
                    identifier: identifier.to_string(),
                },
            })
            .await?;

            match conn.recv().await? {
                Message::Command {
                    command:
                        Command::ResolveAgentResult {
                            agent: Some(info), ..
                        },
                } => (info.route, info.id),
                Message::Command {
                    command: Command::ResolveAgentResult { agent: None },
                } => return Err(AmuxError::AgentNotFound(identifier.to_string())),
                other => {
                    return Err(AmuxError::InvalidMessage(format!(
                        "expected ResolveAgentResult, got {}",
                        other.type_label()
                    )));
                }
            }
        }
        None => {
            conn.send(&Message::Command {
                command: Command::ListAgents,
            })
            .await?;

            match conn.recv().await? {
                Message::Command {
                    command: Command::ListAgentsResult { agents },
                } if !agents.is_empty() => (agents[0].route.clone(), agents[0].id),
                Message::Command {
                    command: Command::ListAgentsResult { .. },
                } => {
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

    route_suffix.push(conn.link_name());
    tracing::info!(agent_id = %agent_id, route = %route_suffix, "attaching");

    subscribe_and_stream(
        conn,
        agent_id,
        route_suffix,
        Some(terminal_size),
        Arc::new(AtomicU64::new(1)),
        config.keybinds.leader.clone(),
        &config.state_path,
    )
    .await
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

    conn.send(&Message::Command {
        command: Command::ListAgents,
    })
    .await?;

    match conn.recv().await? {
        Message::Command {
            command: Command::ListAgentsResult { mut agents },
        } => {
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

    print_update_banner(&config.state_path);
    Ok(())
}

async fn subscribe_and_stream(
    conn: Connection,
    agent_id: Uuid,
    full_route: Route,
    terminal_size: Option<TerminalSize>,
    request_counter: Arc<AtomicU64>,
    leader: LeaderKey,
    state_path: &Path,
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

    let (subscription_id, lease_ms) = match conn.recv().await? {
        Message::Routable { payload, .. } => match RoutableMessage::decode(&payload)? {
            RoutableMessage::SubscribeRawResult {
                subscription_id,
                lease_ms,
                error: None,
            } => {
                tracing::info!(agent_id = %agent_id, subscription_id = %subscription_id, lease_ms, "subscribed");
                (subscription_id, lease_ms)
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
    };

    let session = AttachedSession {
        agent_id,
        subscription_id,
        lease_ms,
        full_route,
        request_counter,
    };

    run_attached(conn, session, leader, state_path).await
}

async fn run_attached(
    conn: Connection,
    mut session: AttachedSession,
    leader: LeaderKey,
    state_path: &Path,
) -> Result<()> {
    let raw_mode_guard = RawModeGuard::new()?;

    let conn = Arc::new(conn);
    let renewal_timer = tokio::time::sleep(session.renewal_interval());
    tokio::pin!(renewal_timer);

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
                    if session.send_raw_input(&conn, data).await.is_err() {
                        break ExitReason::SessionEnded;
                    }
                }
                Some(StdinEvent::Detach) => break ExitReason::Detached,
                None => break ExitReason::SessionEnded,
            },
            _ = &mut renewal_timer => {
                if session
                    .send(
                        &conn,
                        RoutableMessage::ExtendSubscription {
                            subscription_id: session.subscription_id,
                        },
                    )
                    .await
                    .is_err()
                {
                    break ExitReason::SessionEnded;
                }
                renewal_timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + session.renewal_interval());
            }
            msg = conn.recv() => match msg {
                Ok(Message::Routable { payload, .. }) => match RoutableMessage::decode(&payload) {
                    Ok(RoutableMessage::RawOutput { subscription_id: id, data }) if id == session.subscription_id => {
                        io::stdout().write_all(&data).ok();
                        io::stdout().flush().ok();
                    }
                    Ok(RoutableMessage::SubscriptionClosed { subscription_id: id, .. }) if id == session.subscription_id => {
                        tracing::info!("agent ended");
                        break ExitReason::SessionEnded;
                    }
                    Ok(RoutableMessage::ExtendSubscriptionResult {
                        subscription_id: id,
                        lease_ms,
                        error: None,
                    }) if id == session.subscription_id => {
                        session.lease_ms = lease_ms;
                        renewal_timer
                            .as_mut()
                            .reset(tokio::time::Instant::now() + session.renewal_interval());
                    }
                    Ok(RoutableMessage::ExtendSubscriptionResult {
                        subscription_id: id,
                        error: Some(ProtocolError::UnknownSubscription),
                        ..
                    }) if id == session.subscription_id => {
                        tracing::info!(subscription_id = %session.subscription_id, "subscription no longer exists");
                        break ExitReason::SessionEnded;
                    }
                    Ok(RoutableMessage::ExtendSubscriptionResult {
                        subscription_id: id,
                        error: Some(error),
                        ..
                    }) if id == session.subscription_id => {
                        tracing::debug!(subscription_id = %session.subscription_id, error = %error, "subscription renewal failed");
                    }
                    _ => {}
                },
                Ok(Message::Command {
                    command: Command::ShutdownNotification { reason },
                }) => {
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
    };

    if matches!(exit_reason, ExitReason::Detached) {
        let _ = session
            .send(
                &conn,
                RoutableMessage::Unsubscribe {
                    subscription_id: session.subscription_id,
                },
            )
            .await;
    }

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
    }

    print_update_banner(state_path);
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
