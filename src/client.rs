use crate::server::{CMD_ATTACH, CMD_KILL, CMD_LIST, SOCKET_PATH};
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

/// Attach to a session
pub async fn attach(session_name: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(SOCKET_PATH).await?;
    log!("client: connected to server");

    // Send ATTACH command
    stream.write_u8(CMD_ATTACH).await?;

    // Send session name (null-terminated)
    stream.write_all(session_name.as_bytes()).await?;
    stream.write_u8(0).await?;

    // Send terminal size
    let (rows, cols) = get_terminal_size();
    log!("client: ATTACH {} ({}x{})", session_name, cols, rows);
    stream.write_all(&rows.to_be_bytes()).await?;
    stream.write_all(&cols.to_be_bytes()).await?;
    stream.flush().await?;

    // Now enter streaming mode
    run_attached(stream).await
}

/// List all running agents
pub async fn list_agents() -> io::Result<()> {
    let mut stream = match UnixStream::connect(SOCKET_PATH).await {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::ConnectionRefused => {
            println!("No agents running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Send LIST command
    stream.write_u8(CMD_LIST).await?;
    stream.flush().await?;

    // Read response
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{}", response);

    Ok(())
}

/// Kill all agents and shut down the server
pub async fn kill_server() -> io::Result<()> {
    let mut stream = match UnixStream::connect(SOCKET_PATH).await {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::ConnectionRefused => {
            println!("No server running.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Send KILL command
    stream.write_u8(CMD_KILL).await?;
    stream.flush().await?;

    // Read response
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{}", response);

    Ok(())
}

/// Run the attached session (streaming mode with Ctrl-a handling)
async fn run_attached(stream: UnixStream) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    // Put terminal in raw mode
    let _raw_guard = RawModeGuard::new()?;

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

    // Task: Forward local stdin to server (with Ctrl-a handling)
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
                                    if rt.block_on(writer.write_all(&[CTRL_B, next_byte])).is_err() {
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
