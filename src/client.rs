use crate::config::Config;
use crate::error::{AmuxError, Result};
use crate::message::Message;
use crate::transport::{Transport, UnixTransport};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Control key prefix (Ctrl-b = 0x02)
const CTRL_B: u8 = 0x02;

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
pub async fn new_agent(agent_id: &str, command: &str, config: &Config) -> Result<()> {
    let stream = UnixStream::connect(&config.socket_path).await?;
    log!("client: connected to server");

    let mut transport = UnixTransport::new(stream);
    let (rows, cols) = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    log!(
        "client: CREATE {} command='{}' dir={:?} ({}x{})",
        agent_id,
        command,
        working_dir,
        cols,
        rows
    );

    // Send CreateAgent
    transport
        .write_message(&Message::CreateAgent {
            agent_id: agent_id.to_string(),
            command: command.to_string(),
            working_dir: working_dir.clone(),
            rows,
            cols,
        })
        .await?;
    transport.flush().await?;

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

    // Now subscribe
    subscribe_and_stream(transport, agent_id, rows, cols).await
}

/// Attach to an existing agent
pub async fn attach(agent_id: Option<&str>, config: &Config) -> Result<()> {
    let stream = UnixStream::connect(&config.socket_path).await?;
    log!("client: connected to server");

    let mut transport = UnixTransport::new(stream);
    let (rows, cols) = get_terminal_size();

    // If no agent_id specified, list agents and pick the first one
    let agent_id = match agent_id {
        Some(id) => id.to_string(),
        None => {
            transport.write_message(&Message::ListAgents).await?;
            transport.flush().await?;

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

    subscribe_and_stream(transport, &agent_id, rows, cols).await
}

/// Subscribe to an agent and stream I/O
async fn subscribe_and_stream(
    mut transport: UnixTransport,
    agent_id: &str,
    rows: u16,
    cols: u16,
) -> Result<()> {
    // Send Subscribe
    transport
        .write_message(&Message::Subscribe {
            agent_id: agent_id.to_string(),
            rows,
            cols,
        })
        .await?;
    transport.flush().await?;

    // Read SubscribeResult
    let response = transport.read_message().await?;
    match response {
        Message::SubscribeResult { success: true, .. } => {
            log!("client: subscribed successfully");
        }
        Message::SubscribeResult {
            success: false,
            error,
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

    // Read ReplayBytes
    let response = transport.read_message().await?;
    let replay_data = match response {
        Message::ReplayBytes { data } => data,
        _ => {
            return Err(AmuxError::InvalidMessage);
        }
    };

    // Now enter raw mode and stream
    run_attached(transport, &replay_data).await
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let stream = match UnixStream::connect(&config.socket_path).await {
        Ok(s) => s,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No agents running.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let mut transport = UnixTransport::new(stream);

    transport.write_message(&Message::ListAgents).await?;
    transport.flush().await?;

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
    let stream = match UnixStream::connect(&config.socket_path).await {
        Ok(s) => s,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            println!("No server running.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let mut transport = UnixTransport::new(stream);

    transport.write_message(&Message::Shutdown).await?;
    transport.flush().await?;

    println!("Server shutting down.");
    Ok(())
}

/// Run the attached session (streaming mode with Ctrl-b handling)
async fn run_attached(transport: UnixTransport, replay_data: &[u8]) -> Result<()> {
    let (mut reader, mut writer) = transport.into_split();

    // Put terminal in raw mode
    let _raw_guard = RawModeGuard::new()?;

    // Write replay data to stdout
    if !replay_data.is_empty() {
        log!("client: writing {} bytes replay", replay_data.len());
        io::stdout().write_all(replay_data)?;
        io::stdout().flush()?;
    }

    // Flag to signal detach
    let detach_flag = Arc::new(AtomicBool::new(false));
    let detach_flag_clone = detach_flag.clone();

    // Task: Forward server output to local stdout
    let stdout_task = tokio::spawn(async move {
        let mut stdout = io::stdout();
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    stdout.write_all(&buffer[..n]).ok();
                    stdout.flush().ok();
                }
                Err(_) => break,
            }
        }
    });

    // Task: Forward local stdin to server (with Ctrl-b handling)
    let stdin_task = tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        let rt = tokio::runtime::Handle::current();

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
                                    if rt.block_on(writer.write_all(&[CTRL_B])).is_err() {
                                        return;
                                    }
                                }
                                _ => {
                                    if rt.block_on(writer.write_all(&[CTRL_B, next_byte])).is_err()
                                    {
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
                            if rt.block_on(writer.write_all(&buffer[start..i])).is_err() {
                                return;
                            }
                        }
                    }
                    let _ = rt.block_on(writer.flush());
                }
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _ = stdout_task => {}
        _ = stdin_task => {}
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
