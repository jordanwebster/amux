use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::message::{AgentType, Message};
use crate::transport::{Transport, UnixTransport};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Control key prefix (Ctrl-b = 0x02)
const CTRL_B: u8 = 0x02;

/// Connect to server and perform handshake, returns (transport, server_host_id)
async fn connect_and_handshake(config: &Config) -> Result<(UnixTransport, String)> {
    let stream = UnixStream::connect(&config.socket_path).await?;
    let mut transport = UnixTransport::new(stream);

    // Generate unique client ID and send Connect
    let client_id = Uuid::new_v4().to_string();
    transport
        .write_message(&Message::Connect { host_id: client_id })
        .await?;

    // Receive ConnectResponse with server's host_id
    let response = transport.read_message().await?;
    match response {
        Message::ConnectResponse {
            success: true,
            host_id: server_host_id,
            ..
        } => {
            log!("client: connected to server {}", server_host_id);
            Ok((transport, server_host_id))
        }
        Message::ConnectResponse {
            success: false,
            error,
            ..
        } => Err(AmuxError::Config(
            error.unwrap_or_else(|| "Connection rejected".to_string()),
        )),
        _ => Err(AmuxError::InvalidMessage),
    }
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
pub async fn new_agent(agent_id: &str, agent_type: AgentType, config: &Config) -> Result<()> {
    let (mut transport, server_host_id) = connect_and_handshake(config).await?;
    let (rows, cols) = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    log!(
        "client: CREATE {} type={:?} dir={:?} ({}x{}) on server {}",
        agent_id,
        agent_type,
        working_dir,
        cols,
        rows,
        server_host_id
    );

    // Send CreateAgent
    transport
        .write_message(&Message::CreateAgent {
            agent_id: agent_id.to_string(),
            agent_type,
            working_dir: working_dir.clone(),
            rows,
            cols,
        })
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
            return Err(AmuxError::Pty(
                error.unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }
        _ => {
            return Err(AmuxError::InvalidMessage);
        }
    }

    // Now subscribe (local agent, so dst_host is this server)
    subscribe_and_stream(transport, agent_id, rows, cols, &server_host_id).await
}

/// Attach to an existing agent
pub async fn attach(agent_id: Option<&str>, config: &Config) -> Result<()> {
    let (mut transport, server_host_id) = connect_and_handshake(config).await?;
    let (rows, cols) = get_terminal_size();

    // If no agent_id specified, list agents and pick the first one
    let agent_id = match agent_id {
        Some(id) => id.to_string(),
        None => {
            transport.write_message(&Message::ListAgents).await?;

            let response = transport.read_message().await?;
            match response {
                Message::ListAgentsResult { agents } if !agents.is_empty() => {
                    agents[0].agent_id.clone()
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

    log!("client: ATTACH {} ({}x{})", agent_id, cols, rows);

    subscribe_and_stream(transport, &agent_id, rows, cols, &server_host_id).await
}

/// Parse agent specifier which can be "agent" or "host/agent"
fn parse_agent_specifier(specifier: &str) -> (Option<&str>, &str) {
    if let Some(pos) = specifier.find('/') {
        (Some(&specifier[..pos]), &specifier[pos + 1..])
    } else {
        (None, specifier)
    }
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    mut transport: UnixTransport,
    agent_specifier: &str,
    rows: u16,
    cols: u16,
    server_host_id: &str,
) -> Result<()> {
    // Parse agent specifier to extract optional host and agent_id
    let (target_host, agent_id) = parse_agent_specifier(agent_specifier);

    // dst_host is target (or local server if not specified)
    // src_host will be set by the server (it knows our full hierarchical ID)
    let src_host = String::new(); // Server will rewrite this
    let dst_host = target_host.unwrap_or(server_host_id).to_string();

    // Send Subscribe
    transport
        .write_message(&Message::Subscribe {
            src_host: src_host.clone(),
            dst_host: dst_host.clone(),
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
            eprintln!(
                "Failed to subscribe: {}",
                error.unwrap_or_else(|| "Unknown error".to_string())
            );
            return Ok(());
        }
        _ => {
            return Err(AmuxError::InvalidMessage);
        }
    }

    // Now enter raw mode and stream
    // Note: src_host for Input messages is set by server, we just pass dst_host and agent_id
    run_attached(transport, &dst_host, agent_id).await
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let (mut transport, _server_host_id) = match connect_and_handshake(config).await {
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
                agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
                println!("Running agents:");
                for agent in agents {
                    println!("  {} - {}", agent.agent_id, agent.working_dir.display());
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
    let (mut transport, _server_host_id) = match connect_and_handshake(config).await {
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

    println!("Server shutting down.");
    Ok(())
}

/// Connect to a remote amux server
pub async fn connect(address: &str, config: &Config) -> Result<()> {
    let (mut transport, _server_host_id) = connect_and_handshake(config).await?;

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
        } => Err(AmuxError::Config(
            error.unwrap_or_else(|| "Connection failed".to_string()),
        )),
        _ => Err(AmuxError::InvalidMessage),
    }
}

/// Run the attached session (streaming mode with Ctrl-b handling)
async fn run_attached(mut transport: UnixTransport, dst_host: &str, agent_id: &str) -> Result<()> {
    let dst_host = dst_host.to_string();
    let agent_id = agent_id.to_string();
    // Put terminal in raw mode
    let _raw_guard = RawModeGuard::new()?;

    // Channel to bridge blocking stdin to async loop
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);

    // Flag to signal detach
    let detach_flag = Arc::new(AtomicBool::new(false));
    let detach_flag_clone = detach_flag.clone();

    // Task: Read stdin, handle Ctrl-b, send to channel
    tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];

        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let mut i = 0;
                    while i < n {
                        if buffer[i] == CTRL_B {
                            let next_byte = if i + 1 < n {
                                i += 1;
                                buffer[i]
                            } else {
                                let mut next = [0u8; 1];
                                match stdin.read_exact(&mut next) {
                                    Ok(_) => next[0],
                                    Err(_) => break,
                                }
                            };

                            match next_byte {
                                b'd' => {
                                    log!("client: detaching (Ctrl-b d)");
                                    detach_flag_clone.store(true, Ordering::SeqCst);
                                    return;
                                }
                                CTRL_B => {
                                    // Send literal Ctrl-b
                                    if input_tx.blocking_send(vec![CTRL_B]).is_err() {
                                        return;
                                    }
                                }
                                _ => {
                                    // Send Ctrl-b + next byte
                                    if input_tx.blocking_send(vec![CTRL_B, next_byte]).is_err() {
                                        return;
                                    }
                                }
                            }
                            i += 1;
                        } else {
                            let start = i;
                            while i < n && buffer[i] != CTRL_B {
                                i += 1;
                            }
                            if input_tx.blocking_send(buffer[start..i].to_vec()).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Main loop: select on stdin channel and server messages
    loop {
        if detach_flag.load(Ordering::SeqCst) {
            break;
        }

        tokio::select! {
            // Input from stdin ready to send
            // Note: src_host is set by server, we send empty and server rewrites
            Some(data) = input_rx.recv() => {
                if transport.write_message(&Message::Input {
                    src_host: String::new(),
                    dst_host: dst_host.clone(),
                    agent_id: agent_id.clone(),
                    data,
                }).await.is_err() {
                    break;
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
                    Err(_) => break,
                    _ => {} // Ignore unexpected messages
                }
            }
        }
    }

    let detached = detach_flag.load(Ordering::SeqCst);

    // Drop raw mode guard before printing message
    drop(_raw_guard);

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
