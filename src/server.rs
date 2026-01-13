use crate::buffer::MultiplexReader;
use crate::config::Config;
use crate::connection::ConnectionId;
use crate::error::{AmuxError, Result};
use crate::message::Message;
use crate::session::{AgentId, LocalAgentSession, SessionEvent};
use crate::transport::UnixTransport;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};

/// Server state shared across connection handlers
struct ServerState {
    config: Config,
    agents: HashMap<String, Arc<LocalAgentSession>>,
    next_connection_id: u64,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            agents: HashMap::new(),
            next_connection_id: 0,
        }
    }

    fn next_conn_id(&mut self) -> ConnectionId {
        let id = ConnectionId(self.next_connection_id);
        self.next_connection_id += 1;
        id
    }
}

/// The amux server
pub struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
}

impl Server {
    /// Create a new server with default config
    pub fn new() -> Self {
        Self::with_config(Config::new())
    }

    /// Create a new server with custom config
    pub fn with_config(config: Config) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);
        Self {
            state: Arc::new(RwLock::new(ServerState::new(config))),
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// Run the server
    pub async fn run(&mut self) -> Result<()> {
        let socket_path = {
            let state = self.state.read().await;
            state.config.socket_path.clone()
        };

        // Clean up old socket
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)?;
        log!("server: listening on {:?}", socket_path);

        // Take the event receiver
        let mut event_rx = self.event_rx.take().expect("run() called twice");

        // Task: Handle session lifecycle events
        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    SessionEvent::Ended(agent_id) => {
                        log!("server: session {} ended, removing", agent_id.agent_id);
                        let mut state = state.write().await;
                        state.agents.remove(&agent_id.agent_id);
                    }
                }
            }
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    log!("server: client connected");
                    let state = self.state.clone();
                    let event_tx = self.event_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, state, event_tx).await {
                            log!("server: connection error: {}", e);
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

/// Handle a single connection
async fn handle_connection(
    stream: UnixStream,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let conn_id = {
        let mut state = state.write().await;
        state.next_conn_id()
    };

    log!("server: {} connected", conn_id);

    let mut transport = UnixTransport::new(stream);

    // Message handling loop
    loop {
        let msg = match transport.read_message().await {
            Ok(msg) => msg,
            Err(AmuxError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log!("server: {} disconnected", conn_id);
                break;
            }
            Err(e) => {
                log!("server: {} read error: {}", conn_id, e);
                break;
            }
        };

        log!("server: {} received {:?}", conn_id, msg);

        match msg {
            Message::ListAgents => {
                let agents = {
                    let state = state.read().await;
                    state
                        .agents
                        .values()
                        .map(|s| s.to_agent_info())
                        .collect::<Vec<_>>()
                };
                transport
                    .write_message(&Message::ListAgentsResult { agents })
                    .await?;
            }

            Message::CreateAgent {
                agent_id,
                command,
                working_dir,
                rows,
                cols,
            } => {
                let result = create_agent(
                    &state,
                    &event_tx,
                    &agent_id,
                    &command,
                    working_dir,
                    rows,
                    cols,
                )
                .await;

                let response = match result {
                    Ok(()) => Message::CreateAgentResult {
                        success: true,
                        error: None,
                    },
                    Err(e) => Message::CreateAgentResult {
                        success: false,
                        error: Some(e.to_string()),
                    },
                };
                transport.write_message(&response).await?;
            }

            Message::Subscribe {
                agent_id,
                rows,
                cols,
            } => {
                let result = handle_subscribe(&state, &agent_id, rows, cols).await;

                match result {
                    Ok((mut buffer_reader, input_tx)) => {
                        // Send success response
                        transport
                            .write_message(&Message::SubscribeResult {
                                success: true,
                                error: None,
                            })
                            .await?;

                        log!("server: {} entering subscribed mode", conn_id);

                        // Enter subscribed mode - single loop with select!
                        loop {
                            tokio::select! {
                                // PTY output ready → send to client
                                output = buffer_reader.read() => {
                                    match output {
                                        Some(data) => {
                                            transport.write_message(&Message::Output { data }).await?;
                                        }
                                        None => {
                                            log!("server: {} session ended", conn_id);
                                            break;
                                        }
                                    }
                                }
                                // Client message
                                msg = transport.read_message() => {
                                    match msg {
                                        Ok(Message::Input { data }) => {
                                            let _ = input_tx.send(data).await;
                                        }
                                        Err(_) => {
                                            log!("server: {} client disconnected", conn_id);
                                            break;
                                        }
                                        _ => {} // Ignore unexpected messages
                                    }
                                }
                            }
                        }

                        return Ok(());
                    }
                    Err(e) => {
                        transport
                            .write_message(&Message::SubscribeResult {
                                success: false,
                                error: Some(e.to_string()),
                            })
                            .await?;
                    }
                }
            }

            Message::Shutdown => {
                log!("server: {} requested shutdown", conn_id);
                shutdown_server(&state).await;
                transport
                    .write_message(&Message::Error {
                        code: 0,
                        message: "Server shutting down".to_string(),
                    })
                    .await?;

                // Remove socket and exit
                let socket_path = {
                    let state = state.read().await;
                    state.config.socket_path.clone()
                };
                let _ = std::fs::remove_file(socket_path);
                log!("server: exiting");
                std::process::exit(0);
            }

            _ => {
                transport
                    .write_message(&Message::Error {
                        code: 1,
                        message: "Unexpected message".to_string(),
                    })
                    .await?;
            }
        }
    }

    Ok(())
}

/// Create a new agent
async fn create_agent(
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    agent_id: &str,
    command: &str,
    working_dir: PathBuf,
    rows: u16,
    cols: u16,
) -> Result<()> {
    let mut state = state.write().await;

    // Check if agent already exists
    if state.agents.contains_key(agent_id) {
        return Err(AmuxError::AgentAlreadyExists(agent_id.to_string()));
    }

    // Create agent ID
    let id = AgentId::local(&state.config, agent_id);

    // Create session
    let session = LocalAgentSession::new(id, command, working_dir, rows, cols, event_tx.clone())?;

    state.agents.insert(agent_id.to_string(), Arc::new(session));

    log!("server: created agent {}", agent_id);
    Ok(())
}

/// Handle subscribe request
async fn handle_subscribe(
    state: &Arc<RwLock<ServerState>>,
    agent_id: &str,
    rows: u16,
    cols: u16,
) -> Result<(MultiplexReader, mpsc::Sender<Vec<u8>>)> {
    let state_guard = state.read().await;

    let session = state_guard
        .agents
        .get(agent_id)
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))?
        .clone();

    // Resize PTY if needed
    session.resize(rows, cols).await?;

    // Subscribe to the session - this atomically gives us a reader with
    // all existing output plus a stream of future output, with no gaps or duplicates
    session
        .subscribe()
        .await
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))
}

/// Shutdown the server
async fn shutdown_server(state: &Arc<RwLock<ServerState>>) {
    let mut state = state.write().await;
    for (name, session) in state.agents.iter() {
        log!("server: shutting down agent {}", name);
        session.shutdown().await;
    }
    state.agents.clear();
}
