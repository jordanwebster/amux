use crate::session::{AgentSession, SessionEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};

pub const SOCKET_PATH: &str = "/tmp/amux.sock";

// Protocol commands
pub const CMD_ATTACH: u8 = 0x01;
pub const CMD_LIST: u8 = 0x02;
pub const CMD_KILL: u8 = 0x03;

pub struct Server {
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
}

impl Server {
    pub fn new() -> Self {
        Server {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> std::io::Result<()> {
        // Clean up old socket
        let _ = std::fs::remove_file(SOCKET_PATH);

        let listener = UnixListener::bind(SOCKET_PATH)?;
        log!("server: listening on {}", SOCKET_PATH);

        // Create event channel for session lifecycle events
        let (event_tx, mut event_rx) = mpsc::channel::<SessionEvent>(256);

        // Task: Handle session lifecycle events
        let sessions = self.sessions.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    SessionEvent::Ended(name) => {
                        log!("server: session {} ended, removing", name);
                        let mut sessions = sessions.write().await;
                        sessions.remove(&name);
                    }
                }
            }
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    log!("server: client connected");
                    let sessions = self.sessions.clone();
                    let event_tx = event_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, sessions, event_tx).await {
                            log!("server: client error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    log!("server: accept error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }
}

async fn handle_client(
    mut stream: UnixStream,
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> std::io::Result<()> {
    // Read command byte
    let cmd = stream.read_u8().await?;

    match cmd {
        CMD_ATTACH => handle_attach(stream, sessions, event_tx).await,
        CMD_LIST => handle_list(stream, sessions).await,
        CMD_KILL => handle_kill(stream, sessions).await,
        _ => {
            log!("server: unknown command: {}", cmd);
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unknown command",
            ))
        }
    }
}

async fn handle_attach(
    mut stream: UnixStream,
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> std::io::Result<()> {
    // Read null-terminated session name
    let mut name_buf = Vec::new();
    let mut reader = BufReader::new(&mut stream);
    reader.read_until(0, &mut name_buf).await?;
    name_buf.pop(); // Remove null terminator
    let session_name = String::from_utf8_lossy(&name_buf).to_string();

    // Read terminal size (4 bytes: rows u16 BE, cols u16 BE)
    let mut size_buf = [0u8; 4];
    reader.read_exact(&mut size_buf).await?;
    let rows = u16::from_be_bytes([size_buf[0], size_buf[1]]);
    let cols = u16::from_be_bytes([size_buf[2], size_buf[3]]);

    log!("server: ATTACH {} ({}x{})", session_name, cols, rows);

    // Get underlying stream back from BufReader
    drop(reader);

    // Get or create session
    let session = {
        let mut sessions_write = sessions.write().await;
        if let Some(session) = sessions_write.get(&session_name) {
            if session.is_alive().await {
                session.clone()
            } else {
                // Session is dead, create a new one
                log!("server: session {} is dead, creating new one", session_name);
                sessions_write.remove(&session_name);
                let session = Arc::new(AgentSession::new(
                    session_name.clone(),
                    rows,
                    cols,
                    event_tx.clone(),
                )?);
                sessions_write.insert(session_name.clone(), session.clone());
                session
            }
        } else {
            // Create new session
            let session = Arc::new(AgentSession::new(
                session_name.clone(),
                rows,
                cols,
                event_tx.clone(),
            )?);
            sessions_write.insert(session_name.clone(), session.clone());
            session
        }
    };

    // Split stream and attach
    let (reader, writer) = stream.into_split();
    let client_initiated = session.attach(reader, writer, rows, cols).await;

    // Note: We can't send response code here because the stream is already split
    // The client will detect session end via connection close
    // client_initiated tells us if the client detached vs session ended

    if !client_initiated {
        log!("server: session {} ended, client will be notified via disconnect", session_name);
    }

    Ok(())
}

async fn handle_list(
    mut stream: UnixStream,
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
) -> std::io::Result<()> {
    log!("server: LIST");

    let sessions_guard = sessions.read().await;
    let mut names: Vec<String> = Vec::new();
    for (name, session) in sessions_guard.iter() {
        if session.is_alive().await {
            names.push(name.clone());
        }
    }
    names.sort();

    // Simple format: one session name per line
    let response = if names.is_empty() {
        "No agents running.\n".to_string()
    } else {
        let mut resp = "Running agents:\n".to_string();
        for name in names {
            resp.push_str(&format!("  {}\n", name));
        }
        resp
    };

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;

    Ok(())
}

async fn handle_kill(
    mut stream: UnixStream,
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
) -> std::io::Result<()> {
    log!("server: KILL");

    // Shutdown all sessions
    {
        let sessions = sessions.read().await;
        for (name, session) in sessions.iter() {
            log!("server: killing session {}", name);
            session.shutdown().await;
        }
    }

    // Clear sessions
    {
        let mut sessions = sessions.write().await;
        sessions.clear();
    }

    // Send confirmation
    stream.write_all(b"Server shutting down.\n").await?;
    stream.flush().await?;

    // Remove socket and exit
    let _ = std::fs::remove_file(SOCKET_PATH);

    // Exit the process
    log!("server: exiting");
    std::process::exit(0);
}
