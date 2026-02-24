use crate::config::Config;
use crate::error::AmuxError::AgentNotFound;
use crate::error::{AmuxError, Result};
use crate::message::{
    AgentType, Command, CreateAgentRequest, Message, RoutableMessage, ServerDebugInfo,
    ShutdownReason, TerminalSize,
};
use crate::route::{Route, generate_terminal_link};
use crate::server::connect_handshake;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Control key prefix (Ctrl-b = 0x02)
const CTRL_B: u8 = 0x02;

/// CSI u sequence for Ctrl-b: ESC[98;5u
/// Modern terminals (iTerm2, kitty, WezTerm) use this instead of raw 0x02
const CSI_U_CTRL_B: &[u8] = &[27, b'[', b'9', b'8', b';', b'5', b'u'];

/// Events from stdin reading task
enum StdinEvent {
    /// Raw input data to send to agent
    Data(Vec<u8>),
    /// User requested detach (Ctrl-b d)
    Detach,
}

/// Why the attached session's main loop exited.
/// Replaces three separate mutable state variables (detached, error,
/// shutdown_reason) with a single sum type — impossible states become
/// unrepresentable and the post-loop handling is an exhaustive match.
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

/// Connect to server and perform handshake, returns (transport, link_name)
async fn connect_and_handshake(config: &Config) -> Result<(UnixTransport, String)> {
    let stream = UnixStream::connect(&config.socket_path).await?;
    let mut transport = UnixTransport::new(stream);
    let link_name = connect_handshake(&mut transport, generate_terminal_link).await?;
    tracing::info!(link = %link_name, "connected");
    Ok((transport, link_name))
}

/// Get terminal size, falling back to 24x80 if the ioctl fails
fn get_terminal_size() -> TerminalSize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = io::stdout().as_raw_fd();

    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0 {
        TerminalSize {
            rows: size.ws_row,
            cols: size.ws_col,
        }
    } else {
        TerminalSize::default()
    }
}

/// Create a new agent and attach to it
pub async fn new_agent(name: Option<&str>, agent_type: AgentType, config: &Config) -> Result<()> {
    let (mut transport, link_name) = connect_and_handshake(config).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    // Generate UUID for agent_id
    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    // Create route with just our link (local agent)
    let full_route = Route::from_link(&link_name);
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    let request_counter = AtomicU64::new(1);

    // Send CreateAgent
    transport
        .write_message(&Message::routable(
            src,
            dst,
            request_counter.fetch_add(1, Ordering::Relaxed),
            &RoutableMessage::CreateAgent(CreateAgentRequest {
                agent_id,
                name: name.map(|s| s.to_string()),
                agent_type,
                working_dir: working_dir.clone(),
                terminal_size: Some(terminal_size),
            }),
        ))
        .await?;

    match transport.read_message().await? {
        Message::Routable { payload, .. } => match RoutableMessage::decode(&payload)? {
            RoutableMessage::CreateAgentResult { error: None, .. } => {
                subscribe_and_stream(
                    transport,
                    agent_id,
                    full_route,
                    Some(terminal_size),
                    request_counter,
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
    let (mut transport, link_name) = connect_and_handshake(config).await?;
    let terminal_size = get_terminal_size();

    // Resolve the target to (route, agent_id)
    let (mut route_suffix, agent_id) = match target {
        Some(identifier) => {
            // Use ResolveAgent to resolve the identifier server-side
            transport
                .write_message(&Message::Command(Command::ResolveAgent {
                    identifier: identifier.to_string(),
                }))
                .await?;

            let response = transport.read_message().await?;
            match response {
                Message::Command(Command::ResolveAgentResult {
                    agent: Some(info), ..
                }) => (info.route, info.id),
                Message::Command(Command::ResolveAgentResult { agent: None }) => {
                    return Err(AgentNotFound(identifier.to_string()));
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
            // No target — list agents and pick the first one
            transport
                .write_message(&Message::Command(Command::ListAgents))
                .await?;

            let response = transport.read_message().await?;
            match response {
                Message::Command(Command::ListAgentsResult { agents }) if !agents.is_empty() => {
                    (agents[0].route.clone(), agents[0].id)
                }
                Message::Command(Command::ListAgentsResult { .. }) => {
                    eprintln!("No agents running. Use 'amux new-agent' to create one.");
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
        route_suffix.push(&link_name);
        route_suffix
    };
    tracing::info!(agent_id = %agent_id, route = %full_route, "attaching");

    let request_counter = AtomicU64::new(1);
    subscribe_and_stream(
        transport,
        agent_id,
        full_route,
        Some(terminal_size),
        request_counter,
    )
    .await
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    mut transport: UnixTransport,
    agent_id: Uuid,
    full_route: Route,
    terminal_size: Option<TerminalSize>,
    request_counter: AtomicU64,
) -> Result<()> {
    // Prepare to send: pop first hop, create src for return path
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    // Send Subscribe
    transport
        .write_message(&Message::routable(
            src,
            dst,
            request_counter.fetch_add(1, Ordering::Relaxed),
            &RoutableMessage::SubscribeRaw {
                agent_id,
                terminal_size,
            },
        ))
        .await?;

    // Read SubscribeResult
    let response = transport.read_message().await?;
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

    // Now enter raw mode and stream - pass full route so InputBytes can do pop/push
    run_attached(transport, agent_id, full_route, request_counter).await
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let (mut transport, _link_name) = match connect_and_handshake(config).await {
        Ok(info) => info,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No agents running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    transport
        .write_message(&Message::Command(Command::ListAgents))
        .await?;

    let response = transport.read_message().await?;
    match response {
        Message::Command(Command::ListAgentsResult { mut agents }) => {
            if agents.is_empty() {
                println!("No agents running.");
            } else {
                // Sort by name if present, else by id
                agents.sort_by(|a, b| {
                    let a_id = a.id.to_string();
                    let b_id = b.id.to_string();
                    let a_name = a.name.as_deref().unwrap_or(&a_id);
                    let b_name = b.name.as_deref().unwrap_or(&b_id);
                    a_name.cmp(b_name)
                });
                println!("Running agents:");
                for agent in agents {
                    // Display name if present, else UUID
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
    let (mut transport, _link_name) = match connect_and_handshake(config).await {
        Ok(info) => info,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No server running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    transport
        .write_message(&Message::Command(Command::Shutdown))
        .await?;

    // Wait for server acknowledgment before closing connection
    match transport.read_message().await {
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
pub async fn connect(address: &str, config: &Config) -> Result<()> {
    let (mut transport, _link_name) = connect_and_handshake(config).await?;

    // Send ConnectToServer message to local server
    transport
        .write_message(&Message::Command(Command::ConnectToServer {
            address: address.to_string(),
        }))
        .await?;

    // Wait for result
    let response = transport.read_message().await?;
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
    let (mut transport, _link_name) = match connect_and_handshake(config).await {
        Ok(info) => info,
        Err(AmuxError::Io(e))
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            return Err(AmuxError::ServerError("No server running".to_string()));
        }
        Err(e) => return Err(e),
    };

    transport
        .write_message(&Message::Command(Command::Debug))
        .await?;

    let response = transport.read_message().await?;
    match response {
        Message::Command(Command::DebugResult { info }) => Ok(info),
        other => Err(AmuxError::InvalidMessage(format!(
            "expected DebugResult, got {}",
            other.type_label()
        ))),
    }
}

/// Run the attached session (streaming mode with Ctrl-b handling)
async fn run_attached(
    mut transport: UnixTransport,
    agent_id: Uuid,
    full_route: Route,
    request_counter: AtomicU64,
) -> Result<()> {
    // Put terminal in raw mode
    let _raw_guard = RawModeGuard::new()?;

    // Channel to bridge blocking stdin to async loop
    let (input_tx, mut input_rx) = mpsc::channel::<StdinEvent>(256);

    // Task: Read stdin, handle Ctrl-b (both raw 0x02 and CSI u format), send events to channel
    tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        let mut pending_ctrl_b = false; // True if we saw Ctrl-b and are waiting for next key

        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let data = &buffer[..n];

                    // Check if this read contains CSI u Ctrl-b sequence
                    if let Some(pos) = find_subsequence(data, CSI_U_CTRL_B) {
                        // Send data before the sequence
                        if pos > 0
                            && input_tx
                                .blocking_send(StdinEvent::Data(data[..pos].to_vec()))
                                .is_err()
                        {
                            return;
                        }
                        // Now we're waiting for 'd'
                        let after = pos + CSI_U_CTRL_B.len();
                        if after < n {
                            // There's data after the Ctrl-b sequence
                            if data[after] == b'd' {
                                tracing::info!("detaching");
                                let _ = input_tx.blocking_send(StdinEvent::Detach);
                                return;
                            }
                            // Not 'd' - send Ctrl-b + remaining data
                            let mut remaining = vec![CTRL_B];
                            remaining.extend_from_slice(&data[after..]);
                            if input_tx.blocking_send(StdinEvent::Data(remaining)).is_err() {
                                return;
                            }
                        } else {
                            // Ctrl-b was at end, wait for next read
                            pending_ctrl_b = true;
                        }
                        continue;
                    }

                    // Process byte by byte for raw Ctrl-b
                    let mut i = 0;
                    while i < n {
                        if pending_ctrl_b {
                            pending_ctrl_b = false;
                            if data[i] == b'd' {
                                tracing::info!("detaching");
                                let _ = input_tx.blocking_send(StdinEvent::Detach);
                                return;
                            }
                            // Not 'd' - send Ctrl-b + this byte
                            if input_tx
                                .blocking_send(StdinEvent::Data(vec![CTRL_B, data[i]]))
                                .is_err()
                            {
                                return;
                            }
                            i += 1;
                            continue;
                        }

                        if data[i] == CTRL_B {
                            pending_ctrl_b = true;
                            i += 1;
                            continue;
                        }

                        // Regular data - collect until we hit raw Ctrl-b
                        let start = i;
                        while i < n && data[i] != CTRL_B {
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

            // If we ended with pending Ctrl-b, wait for next byte
            if pending_ctrl_b {
                let mut next = [0u8; 1];
                match stdin.read_exact(&mut next) {
                    Ok(_) => {
                        pending_ctrl_b = false;
                        if next[0] == b'd' {
                            tracing::info!("detaching");
                            let _ = input_tx.blocking_send(StdinEvent::Detach);
                            return;
                        }
                        if input_tx
                            .blocking_send(StdinEvent::Data(vec![CTRL_B, next[0]]))
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

    // Main loop: select on stdin channel and server messages.
    // The loop produces an ExitReason via break-with-value, which the
    // post-loop match handles exhaustively.
    let exit_reason: ExitReason = loop {
        tokio::select! {
            event = input_rx.recv() => {
                match event {
                    Some(StdinEvent::Data(data)) => {
                        let (src, dst) = Route::send(full_route.clone())
                            .expect("full_route should have at least one link");
                        if transport.write_message(&Message::routable(
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
            msg = transport.read_message() => {
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
                            _ => {} // Ignore unexpected routable messages
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
                    _ => {} // Ignore unexpected messages
                }
            }
        }
    };

    // Drop raw mode guard before printing message
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
            // Force exit because stdin blocking read in spawn_blocking won't terminate
            std::process::exit(0);
        }
    }

    Ok(())
}

/// RAII guard to restore terminal mode on drop
struct RawModeGuard {
    original: libc::termios,
}

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        let mut original: libc::termios = unsafe { std::mem::zeroed() };

        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }

        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(RawModeGuard { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let fd = io::stdin().as_raw_fd();
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &self.original);
        }
    }
}
