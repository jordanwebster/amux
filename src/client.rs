use crate::config::Config;
use crate::error::AmuxError::AgentNotFound;
use crate::error::{AmuxError, Result};
use crate::message::{
    AgentType, Command, CreateAgentRequest, DirectMessage, Message, PROTOCOL_VERSION,
    ProtocolError, RoutableMessage, ServerDebugInfo, ShutdownReason, TerminalSize,
};
use crate::route::{Route, generate_terminal_link};
use crate::transport::{Transport, UnixTransport};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
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

    // Retry up to 5 times on same connection in case of link name collision
    for attempt in 0..5 {
        // Generate terminal link name: "term-{rand}"
        let link_name = generate_terminal_link();
        transport
            .write_message(&Message::Direct(DirectMessage::Connect {
                link_name: link_name.clone(),
                token: None,
                version: PROTOCOL_VERSION,
            }))
            .await?;

        // Receive ConnectResult
        let response = transport.read_message().await?;
        match response {
            Message::Direct(DirectMessage::ConnectResult { error: None }) => {
                tracing::info!(link = %link_name, "connected");
                return Ok((transport, link_name));
            }
            Message::Direct(DirectMessage::ConnectResult {
                error: Some(ProtocolError::LinkNameTaken),
            }) => {
                tracing::debug!(link = %link_name, attempt = attempt + 1, "link name taken, retrying");
                continue;
            }
            Message::Direct(DirectMessage::ConnectResult {
                error: Some(ProtocolError::InvalidCredentials),
            }) => {
                tracing::error!("invalid credentials");
                return Err(AmuxError::InvalidCredentials);
            }
            Message::Direct(DirectMessage::ConnectResult {
                error: Some(ProtocolError::InvalidLinkName),
            }) => {
                return Err(AmuxError::Config(
                    ProtocolError::InvalidLinkName.to_string(),
                ));
            }
            Message::Direct(DirectMessage::ConnectResult {
                error:
                    Some(ProtocolError::VersionMismatch {
                        server_version,
                        client_version,
                    }),
            }) => {
                return Err(AmuxError::VersionMismatch(format!(
                    "protocol v{}, client v{}",
                    server_version, client_version
                )));
            }
            Message::Direct(DirectMessage::ConnectResult { error }) => {
                let msg = error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection rejected".to_string());
                return Err(AmuxError::Config(msg));
            }
            _ => return Err(AmuxError::InvalidMessage),
        }
    }

    Err(AmuxError::Config(
        "Failed to connect after 5 attempts".to_string(),
    ))
}

/// Get terminal size, falling back to 24x80 if the ioctl fails
pub fn get_terminal_size() -> TerminalSize {
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

    // Send CreateAgent
    transport
        .write_message(&Message::Routable {
            src,
            dst,
            message: RoutableMessage::CreateAgent(CreateAgentRequest {
                agent_id,
                name: name.map(|s| s.to_string()),
                agent_type,
                working_dir: working_dir.clone(),
                terminal_size: Some(terminal_size),
            }),
        })
        .await?;

    match transport.read_message().await? {
        Message::Routable {
            message: RoutableMessage::CreateAgentResult { error: None, .. },
            ..
        } => subscribe_and_stream(transport, agent_id, full_route, Some(terminal_size)).await,
        Message::Routable {
            message: RoutableMessage::CreateAgentResult { error: Some(e), .. },
            ..
        } => Err(AmuxError::Pty(e.to_string())),
        _ => Err(AmuxError::InvalidMessage),
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
                _ => {
                    return Err(AmuxError::InvalidMessage);
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
                _ => {
                    return Err(AmuxError::InvalidMessage);
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

    subscribe_and_stream(transport, agent_id, full_route, Some(terminal_size)).await
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    mut transport: UnixTransport,
    agent_id: Uuid,
    full_route: Route,
    terminal_size: Option<TerminalSize>,
) -> Result<()> {
    // Prepare to send: pop first hop, create src for return path
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    // Send Subscribe
    transport
        .write_message(&Message::Routable {
            src,
            dst,
            message: RoutableMessage::SubscribeRaw {
                agent_id,
                terminal_size,
            },
        })
        .await?;

    // Read SubscribeResult
    let response = transport.read_message().await?;
    match response {
        Message::Routable {
            message: RoutableMessage::SubscribeRawResult { error: None, .. },
            ..
        } => {
            tracing::info!(agent_id = %agent_id, "subscribed");
        }
        Message::Routable {
            message: RoutableMessage::SubscribeRawResult { error: Some(e), .. },
            ..
        } => {
            eprintln!("Failed to subscribe: {}", e);
            return Ok(());
        }
        _ => {
            return Err(AmuxError::InvalidMessage);
        }
    }

    // Now enter raw mode and stream - pass full route so InputBytes can do pop/push
    run_attached(transport, agent_id, full_route).await
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
        _ => {
            return Err(AmuxError::InvalidMessage);
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
        Message::Command(Command::ConnectToServerResult { error: Some(e) }) => {
            Err(AmuxError::Config(e.to_string()))
        }
        _ => Err(AmuxError::InvalidMessage),
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
        _ => Err(AmuxError::InvalidMessage),
    }
}

/// Run the attached session (streaming mode with Ctrl-b handling)
async fn run_attached(
    mut transport: UnixTransport,
    agent_id: Uuid,
    full_route: Route,
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

    let mut detached = false;
    let mut error: Option<AmuxError> = None;
    let mut shutdown_reason: Option<ShutdownReason> = None;

    // Main loop: select on stdin channel and server messages
    loop {
        tokio::select! {
            // Event from stdin
            event = input_rx.recv() => {
                match event {
                    Some(StdinEvent::Data(data)) => {
                        let (src, dst) = Route::send(full_route.clone())
                            .expect("full_route should have at least one link");
                        if transport.write_message(&Message::Routable {
                            src,
                            dst,
                            message: RoutableMessage::RawInput {
                                agent_id,
                                data,
                            },
                        }).await.is_err() {
                            break;
                        }
                    }
                    Some(StdinEvent::Detach) => {
                        detached = true;
                        break;
                    }
                    None => {
                        // Channel closed (stdin task exited unexpectedly)
                        break;
                    }
                }
            }
            // Message from server
            msg = transport.read_message() => {
                match msg {
                    Ok(Message::Routable { message: RoutableMessage::RawOutput { data, .. }, .. }) => {
                        io::stdout().write_all(&data).ok();
                        io::stdout().flush().ok();
                    }
                    Ok(Message::Routable { message: RoutableMessage::AgentEnded { .. }, .. }) => {
                        tracing::info!("agent ended");
                        break;
                    }
                    Ok(Message::Command(Command::ShutdownNotification(reason))) => {
                        tracing::info!(reason = %reason, "server shutdown");
                        shutdown_reason = Some(reason);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "read error");
                        error = Some(e);
                        break;
                    }
                    _ => {} // Ignore unexpected messages
                }
            }
        }
    }

    // Drop raw mode guard before printing message
    drop(_raw_guard);

    if let Some(e) = error {
        return Err(e);
    }

    if let Some(reason) = shutdown_reason {
        println!("\n[{}]", reason);
        std::process::exit(1);
    }

    if detached {
        println!("\n[detached from session]");
    } else {
        println!("\n[session ended]");
        // Force exit because stdin blocking read in spawn_blocking won't terminate
        std::process::exit(0);
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
