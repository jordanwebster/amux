use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::Read;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

const MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024; // 10MB

/// Message sent when a session ends
#[derive(Clone)]
pub enum SessionEvent {
    /// Session ended normally (Claude exited)
    Ended(String),
}

pub struct AgentSession {
    pub name: String,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    replay_buffer: Arc<RwLock<Vec<u8>>>,
    broadcast_tx: Arc<RwLock<Option<broadcast::Sender<Vec<u8>>>>>,
    pty_input_tx: mpsc::Sender<Vec<u8>>,
    current_size: Arc<Mutex<(u16, u16)>>,
}

impl AgentSession {
    pub fn new(
        name: String,
        rows: u16,
        cols: u16,
        event_tx: mpsc::Sender<SessionEvent>,
    ) -> std::io::Result<Self> {
        log!("session [{}]: creating with size {}x{}", name, cols, rows);

        // Create PTY
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // Spawn Claude
        let mut cmd = CommandBuilder::new("claude");
        cmd.cwd(std::env::current_dir()?);
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        drop(pair.slave);

        let master = pair.master;
        let mut pty_reader = master
            .try_clone_reader()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let mut pty_writer = master
            .take_writer()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(master)));
        let current_size: Arc<Mutex<(u16, u16)>> = Arc::new(Mutex::new((rows, cols)));
        let replay_buffer: Arc<RwLock<Vec<u8>>> = Arc::new(RwLock::new(Vec::new()));
        let (broadcast_tx, _) = broadcast::channel::<Vec<u8>>(256);
        let broadcast_tx: Arc<RwLock<Option<broadcast::Sender<Vec<u8>>>>> =
            Arc::new(RwLock::new(Some(broadcast_tx)));
        let (pty_input_tx, mut pty_input_rx) = mpsc::channel::<Vec<u8>>(256);

        // Task: Read from PTY, append to replay buffer, broadcast to clients
        let broadcast_tx_clone = broadcast_tx.clone();
        let replay_buffer_clone = replay_buffer.clone();
        let session_name = name.clone();
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let mut buffer = [0u8; 4096];
            loop {
                match pty_reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = buffer[..n].to_vec();
                        rt.block_on(async {
                            let mut buf = replay_buffer_clone.write().await;
                            buf.extend_from_slice(&data);
                            if buf.len() > MAX_REPLAY_BUFFER {
                                let excess = buf.len() - MAX_REPLAY_BUFFER;
                                buf.drain(..excess);
                            }
                        });
                        // Send to broadcast channel if still open
                        rt.block_on(async {
                            if let Some(tx) = broadcast_tx_clone.read().await.as_ref() {
                                let _ = tx.send(data);
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
            log!("session [{}]: PTY reader ended", session_name);
        });

        // Task: Forward input to PTY
        let session_name = name.clone();
        tokio::spawn(async move {
            while let Some(data) = pty_input_rx.recv().await {
                if pty_writer.write_all(&data).is_err() {
                    break;
                }
                let _ = pty_writer.flush();
            }
            log!("session [{}]: PTY writer ended", session_name);
        });

        // Task: Wait for child to exit, then clean up and notify server
        let session_name = name.clone();
        let master_clone = master.clone();
        let broadcast_tx_clone = broadcast_tx.clone();
        tokio::task::spawn_blocking(move || {
            let status = child.wait();
            log!("session [{}]: Claude exited: {:?}", session_name, status);

            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                // Drop the PTY master to kill any remaining shell/processes
                {
                    let mut master = master_clone.lock().await;
                    master.take();
                    log!("session [{}]: PTY master dropped", session_name);
                }

                // Close broadcast channel to disconnect all clients
                {
                    let mut tx = broadcast_tx_clone.write().await;
                    tx.take();
                    log!("session [{}]: broadcast channel closed", session_name);
                }

                // Notify server
                let _ = event_tx.send(SessionEvent::Ended(session_name)).await;
            });
        });

        Ok(AgentSession {
            name,
            master,
            replay_buffer,
            broadcast_tx,
            pty_input_tx,
            current_size,
        })
    }

    /// Check if the session is still alive (broadcast channel is open)
    pub async fn is_alive(&self) -> bool {
        self.broadcast_tx.read().await.is_some()
    }

    pub async fn attach(
        &self,
        reader: OwnedReadHalf,
        writer: OwnedWriteHalf,
        rows: u16,
        cols: u16,
    ) -> bool {
        // Returns true if detached normally, false if session ended

        if !self.is_alive().await {
            log!("session [{}]: refusing attach, session is dead", self.name);
            return false;
        }

        log!("session [{}]: client attaching with size {}x{}", self.name, cols, rows);

        // Resize PTY if needed
        {
            let mut current = self.current_size.lock().await;
            if *current != (rows, cols) {
                let master_guard = self.master.lock().await;
                if let Some(master) = master_guard.as_ref() {
                    if let Err(e) = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    }) {
                        log!("session [{}]: resize failed: {}", self.name, e);
                    } else {
                        log!("session [{}]: resized to {}x{}", self.name, cols, rows);
                        *current = (rows, cols);
                    }
                }
            }
        }

        let mut writer = writer;
        let mut reader = reader;

        // Send replay buffer
        {
            let buf = self.replay_buffer.read().await;
            if !buf.is_empty() {
                log!("session [{}]: sending {} bytes replay", self.name, buf.len());
                if writer.write_all(&buf).await.is_err() {
                    return false;
                }
                let _ = writer.flush().await;
            }
        }

        // Subscribe to broadcast channel
        let broadcast_rx = {
            let tx_guard = self.broadcast_tx.read().await;
            match tx_guard.as_ref() {
                Some(tx) => tx.subscribe(),
                None => {
                    log!("session [{}]: broadcast channel already closed", self.name);
                    return false;
                }
            }
        };
        let mut broadcast_rx = broadcast_rx;
        let pty_input_tx = self.pty_input_tx.clone();
        let session_name = self.name.clone();

        // Task: PTY output -> client
        let write_task = tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(data) => {
                        if writer.write_all(&data).await.is_err() {
                            return true; // Client disconnected
                        }
                        let _ = writer.flush().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return false; // Session ended
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        // Task: client input -> PTY
        let read_task = tokio::spawn(async move {
            let mut buffer = [0u8; 1024];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => return true, // Client disconnected
                    Ok(n) => {
                        if pty_input_tx.send(buffer[..n].to_vec()).await.is_err() {
                            return false; // Session ended
                        }
                    }
                    Err(_) => return true, // Client disconnected
                }
            }
        });

        let client_initiated = tokio::select! {
            result = write_task => result.unwrap_or(false),
            result = read_task => result.unwrap_or(false),
        };

        log!("session [{}]: client detached (client_initiated={})", session_name, client_initiated);
        client_initiated
    }

    pub async fn shutdown(&self) {
        log!("session [{}]: shutting down", self.name);
        // Drop the PTY master to kill any remaining processes
        self.master.lock().await.take();
        // Close broadcast channel to disconnect clients
        self.broadcast_tx.write().await.take();
    }
}
