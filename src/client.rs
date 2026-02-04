use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::message::{AgentType, CreateAgentRequest, Message, ProtocolError};
use crate::route::{generate_terminal_link, Route};
use crate::transport::{Transport, UnixTransport};
use serde::Deserialize;
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Parse target string: "route:agent_id" or just "agent_id" for local.
/// Route is serialized as dot-separated link names (e.g., "linkA.linkB").
/// Returns (Some(route), agent_id) for remote targets, (None, agent_id) for local.
fn parse_target(target: &str) -> (Option<Route>, String) {
    match target.rsplit_once(':') {
        Some((route_str, agent_id)) => {
            let deserializer =
                serde::de::value::StrDeserializer::<serde::de::value::Error>::new(route_str);
            let route: Route =
                Route::deserialize(deserializer).expect("Route deserialization cannot fail");
            (Some(route), agent_id.to_string())
        }
        None => (None, target.to_string()),
    }
}

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
            .write_message(&Message::Connect {
                link_name: link_name.clone(),
                token: None,
            })
            .await?;

        // Receive ConnectResponse
        let response = transport.read_message().await?;
        match response {
            Message::ConnectResponse { success: true, .. } => {
                log!("client: connected with link {}", link_name);
                return Ok((transport, link_name));
            }
            Message::ConnectResponse {
                success: false,
                error: Some(ProtocolError::LinkNameTaken),
            } => {
                log!(
                    "client: link name {} taken, retrying (attempt {})",
                    link_name,
                    attempt + 1
                );
                continue;
            }
            Message::ConnectResponse {
                success: false,
                error: Some(ProtocolError::InvalidCredentials),
            } => {
                log!("client: invalid credentials - authentication failed");
                return Err(AmuxError::InvalidCredentials);
            }
            Message::ConnectResponse {
                success: false,
                error,
            } => {
                let msg = error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection rejected".to_string());
                return Err(AmuxError::Config(msg));
            }
            Message::Error { message } => {
                return Err(AmuxError::ServerError(message));
            }
            _ => return Err(AmuxError::InvalidMessage),
        }
    }

    Err(AmuxError::Config(
        "Failed to connect after 5 attempts".to_string(),
    ))
}

/// Get terminal size (rows, cols)
pub fn get_terminal_size() -> (u16, u16) {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = io::stdout().as_raw_fd();

    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0 {
        (size.ws_row, size.ws_col)
    } else {
        (24, 80) // fallback
    }
}

/// Create a new agent and attach to it
pub async fn new_agent(alias: Option<&str>, agent_type: AgentType, config: &Config) -> Result<()> {
    let (mut transport, link_name) = connect_and_handshake(config).await?;
    let (rows, cols) = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    // Generate UUID for agent_id
    let agent_id = Uuid::new_v4();

    log!(
        "client: CREATE {} (alias={:?}) type={:?} dir={:?} ({}x{}) via {}",
        agent_id,
        alias,
        agent_type,
        working_dir,
        cols,
        rows,
        link_name
    );

    // Send CreateAgent
    transport
        .write_message(&Message::CreateAgent(CreateAgentRequest {
            agent_id,
            alias: alias.map(|s| s.to_string()),
            agent_type,
            working_dir: working_dir.clone(),
            rows,
            cols,
        }))
        .await?;

    // Read response
    let response = transport.read_message().await?;
    match response {
        Message::CreateAgentResult { success: true, .. } => {
            log!("client: agent created successfully");
        }
        Message::CreateAgentResult {
            success: false,
            error,
        } => {
            let msg = error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Unknown error".to_string());
            return Err(AmuxError::Pty(msg));
        }
        _ => {
            return Err(AmuxError::InvalidMessage);
        }
    }

    // Now subscribe using alias if provided, else UUID
    // (server supports lookup by either)
    let agent_id_str = agent_id.to_string();
    let subscribe_id = alias.unwrap_or(&agent_id_str);

    // Create route with just our link (local agent)
    let full_route = Route::from_link(&link_name);

    subscribe_and_stream(transport, subscribe_id, full_route, rows, cols).await
}

/// Attach to an existing agent
pub async fn attach(target: Option<&str>, config: &Config) -> Result<()> {
    let (mut transport, link_name) = connect_and_handshake(config).await?;
    let (rows, cols) = get_terminal_size();

    // If no target specified, list agents and pick the first one (local only)
    let (route_suffix, agent_id) = match target {
        Some(t) => parse_target(t),
        None => {
            transport.write_message(&Message::ListAgents).await?;

            let response = transport.read_message().await?;
            match response {
                Message::ListAgentsResult { agents } if !agents.is_empty() => {
                    (None, agents[0].agent_id.to_string())
                }
                Message::ListAgentsResult { .. } => {
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
    let full_route = match route_suffix {
        Some(mut suffix) => {
            suffix.push(&link_name);
            suffix
        }
        None => Route::from_link(&link_name),
    };

    log!(
        "client: ATTACH {} route={:?} ({}x{})",
        agent_id,
        full_route,
        cols,
        rows
    );

    subscribe_and_stream(transport, &agent_id, full_route, rows, cols).await
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    mut transport: UnixTransport,
    agent_id: &str,
    full_route: Route,
    rows: u16,
    cols: u16,
) -> Result<()> {
    // Prepare to send: pop first hop, create src for return path
    let (src, dst) =
        Route::send(full_route.clone()).expect("full_route should have at least one link");

    // Send Subscribe
    transport
        .write_message(&Message::Subscribe {
            src,
            dst,
            agent_id: agent_id.to_string(),
            rows,
            cols,
        })
        .await?;

    // Read SubscribeResult
    let response = transport.read_message().await?;
    match response {
        Message::SubscribeResult { success: true, .. } => {
            log!("client: subscribed successfully");
        }
        Message::SubscribeResult {
            success: false,
            error,
            ..
        } => {
            let msg = error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Unknown error".to_string());
            eprintln!("Failed to subscribe: {}", msg);
            return Ok(());
        }
        Message::Error { message } => {
            return Err(AmuxError::ServerError(message));
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

    transport.write_message(&Message::ListAgents).await?;

    let response = transport.read_message().await?;
    match response {
        Message::ListAgentsResult { mut agents } => {
            if agents.is_empty() {
                println!("No agents running.");
            } else {
                // Sort by alias if present, else by agent_id
                agents.sort_by(|a, b| {
                    let a_id = a.agent_id.to_string();
                    let b_id = b.agent_id.to_string();
                    let a_name = a.alias.as_deref().unwrap_or(&a_id);
                    let b_name = b.alias.as_deref().unwrap_or(&b_id);
                    a_name.cmp(b_name)
                });
                println!("Running agents:");
                for agent in agents {
                    // Display alias if present, else UUID
                    let agent_id_str = agent.agent_id.to_string();
                    let display_name = agent.alias.as_deref().unwrap_or(&agent_id_str);
                    println!("  {} - {}", display_name, agent.working_dir.display());
                }
            }
        }
        Message::Error { message } => {
            return Err(AmuxError::ServerError(message));
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

    transport.write_message(&Message::Shutdown).await?;

    // Wait for server acknowledgment before closing connection
    // TODO: Server should gracefully end agent sessions (send Ctrl+C) before exiting
    let _ = transport.read_message().await;

    println!("Server shutting down.");
    Ok(())
}

/// Connect to a remote amux server
pub async fn connect(address: &str, config: &Config) -> Result<()> {
    let (mut transport, _link_name) = connect_and_handshake(config).await?;

    // Send ConnectToServer message to local server
    transport
        .write_message(&Message::ConnectToServer {
            address: address.to_string(),
        })
        .await?;

    // Wait for result
    let response = transport.read_message().await?;
    match response {
        Message::ConnectToServerResult { success: true, .. } => {
            println!("Connected to {}", address);
            Ok(())
        }
        Message::ConnectToServerResult {
            success: false,
            error,
        } => {
            let msg = error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Connection failed".to_string());
            Err(AmuxError::Config(msg))
        }
        _ => Err(AmuxError::InvalidMessage),
    }
}

/// Run the attached session (streaming mode with Ctrl-b handling)
async fn run_attached(
    mut transport: UnixTransport,
    agent_id: &str,
    full_route: Route,
) -> Result<()> {
    let agent_id = agent_id.to_string();
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
                                log!("client: detaching (Ctrl-b d)");
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
                                log!("client: detaching (Ctrl-b d)");
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
                            log!("client: detaching (Ctrl-b d)");
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

    // Main loop: select on stdin channel and server messages
    loop {
        tokio::select! {
            // Event from stdin
            event = input_rx.recv() => {
                match event {
                    Some(StdinEvent::Data(data)) => {
                        let (src, dst) = Route::send(full_route.clone())
                            .expect("full_route should have at least one link");
                        if transport.write_message(&Message::InputBytes {
                            src,
                            dst,
                            agent_id: agent_id.clone(),
                            data,
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
                    Ok(Message::Output { data, .. }) => {
                        io::stdout().write_all(&data).ok();
                        io::stdout().flush().ok();
                    }
                    Ok(Message::AgentEnded) => {
                        log!("client: agent ended");
                        break;
                    }
                    Ok(Message::Error { message }) => {
                        log!("client: server error: {}", message);
                        error = Some(AmuxError::ServerError(message));
                        break;
                    }
                    Err(e) => {
                        log!("client: read error: {}", e);
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

    if detached {
        log!("client: detached from session");
        println!("\n[detached from session]");
    } else {
        log!("client: session ended");
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
