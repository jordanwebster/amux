# Interface Sketch

A detailed design for the amux server internals, combining both architectural approaches with preferences for transport abstraction and local socket optimization.

## Quick Overview

**What is amux?** A multiplexer for AI agent sessions (Claude, Codex, etc.) that enables:
- Multiple terminals attaching to the same agent
- Remote access via cloud relay
- Rich clients (mobile/web) receiving structured logs

**Core concepts:**
- **Server** - manages connections, agents, and routing
- **Connection** - Unix socket (local) or TCP/WebSocket (remote)
- **LocalAgentSession** - a running agent with PTY, replay buffers
- **Routing Table** - maps agent IDs to how to reach them (local or via which connection)

**Key optimizations:**
- Local Unix sockets switch to raw byte mode after subscribe (zero framing overhead)
- Remote connections stay framed (multiplexed, need headers)

---

## Glossary

| Term | Description |
|------|-------------|
| **host_id** | Unique identifier for an amux server instance. Generated on first run (UUID) and persisted in config. |
| **user_id** | Identifies the user/owner. In cloud mode, extracted from token. In local mode, hardcoded (e.g., `"local"`). |
| **agent_id** | UUID identifying an agent session. Unique globally. Optional human-readable alias can be set via `-t` flag. |
| **PTY** | Pseudo-terminal - the interface used to run interactive CLI agents like Claude. |
| **Child** | The OS process handle for a running agent (from `std::process::Child` or `portable_pty`). |

> **Implementation Note:** The original design used an `AgentId` tuple `(host_id, user_id, agent_id)` for global uniqueness. The current implementation simplifies this: agents are identified by UUID (`agent_id`), and routing uses `src_host`/`dst_host` fields in protocol messages. The `AgentId` struct has been removed.

---

## Core Identity Types

```rust
/// Agents are identified by UUID string
/// Optional alias provides human-readable name
type AgentId = String;  // UUID

/// Unique connection identifier
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
struct ConnectionId(u64);
```

---

## Transport Abstraction

The key insight: abstract over how bytes get read/written, but let connection types define their own behavior.

```rust
/// Low-level transport - just reads and writes bytes/frames
trait Transport: Send {
    // Framed I/O - for messages during handshake
    /// Read the next frame/message bytes from the wire
    async fn read_frame(&mut self) -> Result<Vec<u8>, TransportError>;
    /// Write a frame/message bytes to the wire
    async fn write_frame(&mut self, data: &[u8]) -> Result<(), TransportError>;

    // Raw I/O - for byte streaming after subscribe (local optimization)
    /// Read raw bytes (no framing) - returns bytes available
    async fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;
    /// Write raw bytes (no framing)
    async fn write_raw(&mut self, data: &[u8]) -> Result<(), TransportError>;

    /// Close the transport
    async fn close(&mut self);
}

/// Implemented by each transport type
struct UnixTransport { socket: UnixStream }
struct TcpTransport { socket: TcpStream }  // with TCP_NODELAY
struct WebSocketTransport { socket: WebSocketStream }

impl Transport for UnixTransport { ... }
impl Transport for TcpTransport { ... }
impl Transport for WebSocketTransport { ... }
```

---

## Message Types

Using serde for serialization - the canonical Rust approach.

```rust
use serde::{Serialize, Deserialize};

/// All API messages - same enum, different serialization formats
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]  // Creates {"type": "ListAgents", ...} in JSON
enum Message {
    // Connection establishment
    EstablishConnection { token: Option<String> },
    EstablishConnectionResult { success: bool, error: Option<String> },

    // Server-level
    ListAgents,
    ListAgentsResult { agents: Vec<AgentId> },
    AddAgents { agents: Vec<AgentId> },
    Disconnect,

    // Agent-level
    Subscribe { host_id: String, agent_id: String },
    SubscribeResult { success: bool, error: Option<String> },
    Unsubscribe { host_id: String, agent_id: String },
    SendMessage { host_id: String, agent_id: String, data: Vec<u8> },

    // Data (output from agents)
    Output { host_id: String, agent_id: String, data: Vec<u8> },
    LogEntry { host_id: String, agent_id: String, entry: StructuredLog },

    // Replay (response to subscribe)
    ReplayBytes { data: Vec<u8> },
    ReplayLogs { entries: Vec<StructuredLog> },

    // Errors
    Error { code: u32, message: String },
    AgentEnded { host_id: String, agent_id: String },
}

/// Format selection - not a trait, just an enum
#[derive(Clone, Copy)]
enum SerdeFormat {
    Binary,  // bincode - for TCP/Unix (compact, fast)
    Json,    // serde_json - for WebSocket (human readable)
}

impl SerdeFormat {
    fn encode(&self, msg: &Message) -> Vec<u8> {
        match self {
            SerdeFormat::Binary => bincode::serialize(msg).unwrap(),
            SerdeFormat::Json => serde_json::to_vec(msg).unwrap(),
        }
    }

    fn decode(&self, data: &[u8]) -> Result<Message, DecodeError> {
        match self {
            SerdeFormat::Binary => bincode::deserialize(data).map_err(Into::into),
            SerdeFormat::Json => serde_json::from_slice(data).map_err(Into::into),
        }
    }
}
```

---

## Connection Types

```rust
/// Connection wraps transport + state + behavior
enum Connection {
    Local(LocalConnection),
    Remote(RemoteConnection),
}

/// Local Unix socket connection - always established, single user
struct LocalConnection {
    id: ConnectionId,
    transport: UnixTransport,
    subscribed_agent: Option<AgentId>,  // At most one
    raw_mode: bool,  // After subscribe, skip framing
}

/// Remote connection (TCP or WebSocket)
struct RemoteConnection {
    id: ConnectionId,
    transport: Box<dyn Transport>,
    format: SerdeFormat,  // Binary for TCP, Json for WebSocket
    state: RemoteConnectionState,
    user_id: Option<String>,  // Set after establish
    subscriptions: HashSet<AgentId>,
}

enum RemoteConnectionState {
    Pending,      // Awaiting establish_connection
    Established,  // Ready for API calls
}
```

---

## Local Connection - Optimized Path

```rust
impl LocalConnection {
    /// Local connections are immediately established with hardcoded user_id
    fn new(id: ConnectionId, socket: UnixStream, user_id: &str) -> Self {
        Self {
            id,
            transport: UnixTransport { socket },
            subscribed_agent: None,
            raw_mode: false,
        }
    }

    async fn handle_message(&mut self, msg: Message, server: &Server) -> Result<()> {
        match msg {
            Message::EstablishConnection { .. } => {
                // Error: local connections don't need this
                self.send(Message::Error {
                    code: 1,
                    message: "Local connections are pre-established".into()
                }).await
            }

            Message::Subscribe { host_id, agent_id } => {
                let agent_id = AgentId { host_id, user_id: server.config.user_id.clone(), agent_id };

                // Subscribe and get replay buffer
                let replay = server.subscribe(self.id, &agent_id).await?;

                self.subscribed_agent = Some(agent_id);
                self.send(Message::SubscribeResult { success: true, error: None }).await?;
                self.send(Message::ReplayBytes { data: replay }).await?;

                // Transition to raw mode - no more message framing
                self.raw_mode = true;
                Ok(())
            }

            Message::SendMessage { data, .. } => {
                if let Some(ref agent) = self.subscribed_agent {
                    server.send_input(agent, data).await
                } else {
                    Err(Error::NotSubscribed)
                }
            }

            _ => Err(Error::InvalidMessage)
        }
    }

    /// In raw mode, just forward bytes directly - no framing overhead
    async fn write_output(&mut self, data: &[u8]) -> Result<()> {
        if self.raw_mode {
            self.transport.write_raw(data).await
        } else {
            let msg = Message::Output {
                host_id: "".into(),  // Not needed, only one subscription
                agent_id: "".into(),
                data: data.to_vec()
            };
            self.send(msg).await
        }
    }

    /// In raw mode, read input bytes directly - no framing overhead
    async fn read_input(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.raw_mode {
            self.transport.read_raw(buf).await
        } else {
            // Would need to parse framed SendMessage, but in practice
            // we switch to raw mode immediately after subscribe
            unimplemented!("framed input not used for local connections")
        }
    }
}
```

---

## Remote Connection - Full Protocol

```rust
impl RemoteConnection {
    fn new(id: ConnectionId, transport: Box<dyn Transport>, format: SerdeFormat) -> Self {
        Self {
            id,
            transport,
            format,
            state: RemoteConnectionState::Pending,
            user_id: None,
            subscriptions: HashSet::new(),
        }
    }

    async fn send(&mut self, msg: Message) -> Result<()> {
        let bytes = self.format.encode(&msg);
        self.transport.write_frame(&bytes).await
    }

    async fn recv(&mut self) -> Result<Message> {
        let bytes = self.transport.read_frame().await?;
        self.format.decode(&bytes)
    }

    async fn handle_message(&mut self, msg: Message, server: &Server) -> Result<()> {
        // Must establish first
        if self.state == RemoteConnectionState::Pending {
            match msg {
                Message::EstablishConnection { token } => {
                    let user_id = server.validate_token(token)?;
                    self.user_id = Some(user_id);
                    self.state = RemoteConnectionState::Established;
                    self.send(Message::EstablishConnectionResult { success: true, error: None }).await
                }
                _ => {
                    self.close().await;
                    Err(Error::NotEstablished)
                }
            }
        } else {
            // Established - handle all messages
            match msg {
                Message::ListAgents => {
                    let agents = server.list_agents(&self.user_id.as_ref().unwrap()).await;
                    self.send(Message::ListAgentsResult { agents }).await
                }

                Message::AddAgents { agents } => {
                    server.add_agents(self.id, agents).await
                }

                Message::Subscribe { host_id, agent_id } => {
                    let agent_id = AgentId {
                        host_id,
                        user_id: self.user_id.clone().unwrap(),
                        agent_id
                    };
                    let result = server.subscribe(self.id, &agent_id).await;
                    // ... handle result, send replay
                }

                // ... other messages
            }
        }
    }

    /// Remote connections always use framed messages
    async fn write_output(&mut self, agent: &AgentId, data: &[u8]) -> Result<()> {
        let msg = Message::Output {
            host_id: agent.host_id.clone(),
            agent_id: agent.agent_id.clone(),
            data: data.to_vec(),
        };
        self.send(msg).await
    }
}
```

---

## Server

```rust
struct Server {
    config: Config,

    // Connections
    connections: HashMap<ConnectionId, Connection>,
    next_connection_id: u64,

    // Agents
    local_agents: HashMap<AgentId, LocalAgentSession>,
    routing_table: HashMap<AgentId, Route>,

    // Subscriptions: who wants data from which agent
    subscriptions: HashMap<AgentId, Vec<ConnectionId>>,
}

enum Route {
    Local,                        // We own this agent
    Remote { via: ConnectionId }, // Forward through this connection
}

impl Server {
    /// Subscribe a connection to an agent's output
    async fn subscribe(&mut self, conn_id: ConnectionId, agent: &AgentId) -> Result<Vec<u8>> {
        match self.routing_table.get(agent) {
            Some(Route::Local) => {
                let session = self.local_agents.get(agent).unwrap();
                let replay = session.get_replay_buffer();
                self.subscriptions.entry(agent.clone()).or_default().push(conn_id);
                Ok(replay)
            }
            Some(Route::Remote { via }) => {
                // Forward subscribe upstream, bridge when data arrives
                self.forward_subscribe(*via, agent).await
            }
            None => Err(Error::AgentNotFound)
        }
    }

    /// Called when agent produces output - fan out to subscribers
    async fn broadcast_output(&self, agent: &AgentId, data: &[u8]) {
        if let Some(subscribers) = self.subscriptions.get(agent) {
            for &conn_id in subscribers {
                if let Some(conn) = self.connections.get_mut(&conn_id) {
                    let _ = conn.write_output(agent, data).await;
                }
            }
        }
    }
}
```

---

## Local Agent Session

```rust
struct LocalAgentSession {
    id: AgentId,

    // PTY
    pty_master: Box<dyn MasterPty + Send>,
    child: Child,

    // Buffers
    byte_replay_buffer: Vec<u8>,        // For terminal clients
    log_replay_buffer: Vec<StructuredLog>,  // For rich clients

    // Input channel
    input_tx: mpsc::Sender<Vec<u8>>,
}

impl LocalAgentSession {
    fn get_replay_buffer(&self) -> Vec<u8> {
        self.byte_replay_buffer.clone()
    }

    fn get_log_replay(&self) -> Vec<StructuredLog> {
        self.log_replay_buffer.clone()
    }

    async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx.send(data).await.map_err(|_| Error::AgentDead)
    }
}
```

---

## Config

```rust
struct Config {
    // Identity
    host_id: String,              // UUID, generated on first run, persisted
    user_id: String,              // Hardcoded for local mode, or from token

    // Server mode
    token_required: bool,         // Cloud mode requires tokens
    token_public_key: Option<String>,  // For verifying signed tokens

    // Listeners (None = disabled)
    unix_socket_path: Option<PathBuf>,    // e.g., /tmp/amux.sock
    tcp_bind_addr: Option<SocketAddr>,    // e.g., 0.0.0.0:9001
    websocket_bind_addr: Option<SocketAddr>,

    // Limits
    max_replay_buffer_bytes: usize,   // e.g., 10MB
    max_log_buffer_entries: usize,    // e.g., 1000
}
```

---

## StructuredLog

For rich clients (mobile, web), we parse agent output into structured entries:

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum StructuredLog {
    /// User sent a message to the agent
    UserMessage { content: String },

    /// Agent is producing text output
    AssistantMessage { content: String },

    /// Agent is calling a tool
    ToolCall { tool: String, args: serde_json::Value },

    /// Tool returned a result
    ToolResult { tool: String, result: String },

    /// Agent is thinking/processing (for streaming indicators)
    Thinking,

    /// Session ended
    SessionEnded { exit_code: Option<i32> },
}
```

The `log_replay_buffer` stores these for rich clients, while `byte_replay_buffer` stores raw PTY output for terminals.

---

## Routing Table

The routing table maps agent identifiers to how to reach them:

```rust
struct Server {
    // ...
    routing_table: HashMap<AgentId, Route>,
}

enum Route {
    Local,                        // We own this agent's PTY
    Remote { via: ConnectionId }, // Forward through this connection
}
```

### Population

**Local agents:** Added when agent is spawned
```rust
fn spawn_agent(&mut self, agent_id: String, command: &str) -> Result<()> {
    let id = AgentId {
        host_id: self.config.host_id.clone(),
        user_id: self.config.user_id.clone(),
        agent_id,
    };

    let session = LocalAgentSession::new(&id, command)?;
    self.local_agents.insert(id.clone(), session);
    self.routing_table.insert(id.clone(), Route::Local);

    // Notify connected servers about new agent
    self.broadcast_add_agents(vec![id]).await;
    Ok(())
}
```

**Remote agents:** Added when `AddAgents` message received
```rust
fn handle_add_agents(&mut self, from_conn: ConnectionId, agents: Vec<AgentId>) {
    for agent in agents {
        // Don't overwrite local agents
        if !self.local_agents.contains_key(&agent) {
            self.routing_table.insert(agent, Route::Remote { via: from_conn });
        }
    }
}
```

### Cleanup

When a connection drops, remove all routes that went through it:
```rust
fn handle_connection_closed(&mut self, conn_id: ConnectionId) {
    // Remove routes via this connection
    self.routing_table.retain(|_, route| {
        !matches!(route, Route::Remote { via } if *via == conn_id)
    });

    // Remove subscriptions from this connection
    for subscribers in self.subscriptions.values_mut() {
        subscribers.retain(|&id| id != conn_id);
    }

    // Remove the connection itself
    self.connections.remove(&conn_id);
}
```

---

## Agent Lifecycle

### Spawning a Local Agent

```rust
impl LocalAgentSession {
    fn new(id: &AgentId, command: &str) -> Result<Self> {
        // Create PTY
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            ..Default::default()
        })?;

        // Spawn the agent process (e.g., "claude", "codex")
        let child = pair.slave.spawn_command(CommandBuilder::new(command))?;

        // Create input channel for sending keystrokes to agent
        let (input_tx, input_rx) = mpsc::channel(256);

        let session = Self {
            id: id.clone(),
            pty_master: pair.master,
            child,
            byte_replay_buffer: Vec::new(),
            log_replay_buffer: Vec::new(),
            input_tx,
        };

        // Start background tasks for PTY I/O (see Task Model)
        session.start_pty_tasks(input_rx);

        Ok(session)
    }
}
```

### Agent Termination

When the child process exits:
1. PTY reader task detects EOF
2. Server is notified via channel
3. Server broadcasts `AgentEnded` to all subscribers
4. Server removes agent from `local_agents` and `routing_table`
5. Server broadcasts `RemoveAgent` to connected servers (so they update their routing tables)

```rust
// Pseudo-code for cleanup
fn handle_agent_ended(&mut self, agent: &AgentId) {
    // Notify subscribers
    if let Some(subscribers) = self.subscriptions.remove(agent) {
        for conn_id in subscribers {
            if let Some(conn) = self.connections.get_mut(&conn_id) {
                let _ = conn.send(Message::AgentEnded {
                    host_id: agent.host_id.clone(),
                    agent_id: agent.agent_id.clone(),
                });
            }
        }
    }

    // Remove from routing
    self.local_agents.remove(agent);
    self.routing_table.remove(agent);

    // Tell connected servers
    self.broadcast_remove_agent(agent).await;
}
```

---

## Connection Lifecycle

### Accepting Connections

```rust
// Unix socket listener
async fn accept_unix_connections(listener: UnixListener, server: Arc<Mutex<Server>>) {
    loop {
        let (socket, _) = listener.accept().await.unwrap();
        let server = server.clone();

        tokio::spawn(async move {
            let conn_id = server.lock().await.register_connection(
                Connection::Local(LocalConnection::new(socket))
            );

            handle_local_connection(conn_id, socket, server).await;

            server.lock().await.handle_connection_closed(conn_id);
        });
    }
}

// TCP listener (similar pattern)
async fn accept_tcp_connections(listener: TcpListener, server: Arc<Mutex<Server>>) {
    loop {
        let (socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();  // TCP_NODELAY for low latency

        let server = server.clone();

        tokio::spawn(async move {
            let conn_id = server.lock().await.register_connection(
                Connection::Remote(RemoteConnection::new(
                    Box::new(TcpTransport { socket }),
                    SerdeFormat::Binary,
                ))
            );

            handle_remote_connection(conn_id, server).await;

            server.lock().await.handle_connection_closed(conn_id);
        });
    }
}
```

### Connection Cleanup

When a connection drops (client disconnects, network error, etc.):
1. Remove all routing table entries that go via this connection
2. Remove this connection from all subscription lists
3. If this was a server connection, its agents become unreachable

See `handle_connection_closed()` above.

---

## Framing (TCP/Unix)

For binary transports, messages are length-prefixed:

```
+----------------+------------------+
| length (4 bytes, big-endian) | payload (N bytes) |
+----------------+------------------+
```

```rust
impl TcpTransport {
    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let len = (data.len() as u32).to_be_bytes();
        self.socket.write_all(&len).await?;
        self.socket.write_all(data).await?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.socket.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        let mut buf = vec![0u8; len];
        self.socket.read_exact(&mut buf).await?;
        Ok(buf)
    }
}
```

WebSocket handles framing natively, so `WebSocketTransport` just reads/writes messages directly.

---

## Input Forwarding (SendMessage)

When a client sends input to a remote agent:

```rust
fn handle_send_message(&mut self, from_conn: ConnectionId, agent: &AgentId, data: Vec<u8>) {
    match self.routing_table.get(agent) {
        Some(Route::Local) => {
            // Deliver to local PTY
            if let Some(session) = self.local_agents.get(agent) {
                let _ = session.input_tx.send(data);
            }
        }

        Some(Route::Remote { via }) => {
            // Forward upstream
            if let Some(conn) = self.connections.get_mut(via) {
                let _ = conn.send(Message::SendMessage {
                    host_id: agent.host_id.clone(),
                    agent_id: agent.agent_id.clone(),
                    data,
                });
            }
        }

        None => {
            // Agent not found - could send error back
        }
    }
}
```

---

## Task Model

How concurrent tasks work together:

```
┌─────────────────────────────────────────────────────────────────────┐
│                           Server                                     │
│                                                                      │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  │
│  │ Unix Listener    │  │ TCP Listener     │  │ WebSocket        │  │
│  │ Task             │  │ Task             │  │ Listener Task    │  │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘  │
│           │                     │                     │             │
│           └─────────────────────┼─────────────────────┘             │
│                                 │                                    │
│                    spawns per connection                             │
│                                 ▼                                    │
│           ┌─────────────────────────────────────────┐               │
│           │         Connection Handler Task          │               │
│           │                                         │               │
│           │  - Reads messages from socket           │               │
│           │  - Dispatches to Server methods         │               │
│           │  - Writes responses back                │               │
│           │  - In raw mode: bridges PTY ↔ socket    │               │
│           └─────────────────────────────────────────┘               │
│                                                                      │
│  Per Local Agent:                                                    │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  │
│  │ PTY Reader Task  │  │ PTY Writer Task  │  │ Child Waiter     │  │
│  │                  │  │                  │  │ Task             │  │
│  │ Reads PTY stdout │  │ Reads input_rx   │  │                  │  │
│  │ → replay buffer  │  │ → PTY stdin      │  │ Waits for exit   │  │
│  │ → broadcast to   │  │                  │  │ → triggers       │  │
│  │   subscribers    │  │                  │  │   cleanup        │  │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘  │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

**Data flow for local terminal subscription:**
```
Terminal ──Subscribe──> Connection Handler
                              │
                              ▼
                        Server.subscribe()
                              │
                              ├─> Add to subscriptions
                              ├─> Send replay buffer
                              └─> Switch to raw mode

PTY Reader ──bytes──> broadcast_output() ──> Connection Handler ──raw──> Terminal
Terminal ──raw──> Connection Handler ──> input_tx ──> PTY Writer ──> PTY stdin
```

**Data flow for proxied subscription:**
```
App ──Subscribe──> Cloud Handler ──Subscribe──> Local Handler
                        │                            │
                        ▼                            ▼
                  Cloud.subscribe()            Local.subscribe()
                        │                            │
                        ├─> Route::Remote            ├─> Route::Local
                        ├─> Forward upstream         ├─> Send replay
                        └─> Add to subscriptions     └─> Add to subscriptions

Local PTY ──Output──> Local Handler ──Output──> Cloud Handler ──Output──> App
```

---

## Key Design Decisions

### 1. Transport trait vs Connection enum

We use **both**:
- `Transport` trait for low-level byte I/O (read_frame, write_frame, write_raw)
- `Connection` enum for high-level behavior differences (Local vs Remote)

This lets us share the byte-shuffling code while having different message handling.

### 2. Local socket optimization

`LocalConnection` has:
- `raw_mode: bool` - after subscribe, skip message framing
- `subscribed_agent: Option<AgentId>` - at most one
- `read_raw()` / `write_raw()` for symmetric byte streaming

After subscribe completes, both sides transition to raw mode:
- Server: `write_raw()` for PTY output, `read_raw()` for client input
- Client: `read_raw()` for PTY output, `write_raw()` for keystrokes

The socket becomes a bidirectional byte pipe with zero framing overhead.

### 3. Serde for serialization

Using serde with format selection via `SerdeFormat` enum:
- TCP/Unix: `bincode` (compact, fast, Rust-native)
- WebSocket: `serde_json` (human readable, rich client friendly)

The `Message` enum is the same; derive macros do the work. Standard ecosystem approach.

### 4. Explicit connection states

`RemoteConnectionState::Pending` vs `Established` makes the state machine clear. Any message before establish → close connection.

### 5. Centralized subscription tracking

`Server.subscriptions: HashMap<AgentId, Vec<ConnectionId>>` keeps fan-out simple. When agent outputs, iterate subscribers and write.

**Why not a Subscription struct?** A first-class `Subscription` object could provide API ergonomics (a "handle" pre-scoped to an agent), but the HashMap is simpler and equally capable. Per-subscriber state (backpressure, sequence numbers) could be added by changing `Vec<ConnectionId>` to `Vec<SubscriberState>` without restructuring.

**Decoupled from multiplexing:** This design is independent of transport strategy. Whether ConnectionIds represent multiplexed connections (one socket, many agents) or dedicated connections (one socket per agent), the subscription logic doesn't change. You could switch transport strategies without touching this code.

---

## Proxying / Multi-hop Subscriptions

When a client subscribes to an agent on a remote server, the intermediate server proxies:

```
App ──Subscribe──> Cloud Server ──Subscribe──> Local Server
                        │                          │
                        │                     (owns agent)
                        │                          │
App <──ReplayBytes─── Cloud <───ReplayBytes────────┘
App <──Output──────── Cloud <───Output─────────────┘ (ongoing)
App ──SendMessage──> Cloud ───SendMessage─────────>│
```

**Key: Raw mode is only for direct terminal connections**

| Connection Type | Raw Mode? | Why |
|-----------------|-----------|-----|
| Terminal → Local (Unix) | Yes | Single agent, direct path, optimize latency |
| App → Cloud (TCP/WS) | No | Multiplexed, needs agent_id in headers |
| Cloud → Local (TCP) | No | Multiplexed, needs agent_id in headers |

**Server behavior on Subscribe:**

```rust
async fn handle_subscribe(&mut self, conn_id: ConnectionId, agent: &AgentId) -> Result<()> {
    match self.routing_table.get(agent) {
        Some(Route::Local) => {
            // We own this agent - send replay, add to subscribers
            let session = self.local_agents.get(agent).unwrap();
            let replay = session.get_replay_buffer();

            self.subscriptions.entry(agent.clone()).or_default().push(conn_id);

            let conn = self.connections.get_mut(&conn_id).unwrap();
            conn.send(Message::SubscribeResult { success: true, error: None }).await?;
            conn.send(Message::ReplayBytes { data: replay }).await?;

            // If local Unix connection, transition to raw mode
            if let Connection::Local(local) = conn {
                local.raw_mode = true;
            }
            Ok(())
        }

        Some(Route::Remote { via }) => {
            // Forward subscribe upstream
            let upstream = self.connections.get_mut(via).unwrap();
            upstream.send(Message::Subscribe {
                host_id: agent.host_id.clone(),
                agent_id: agent.agent_id.clone(),
            }).await?;

            // Track that this connection wants data from this agent
            self.subscriptions.entry(agent.clone()).or_default().push(conn_id);

            // Response will come async via handle_subscribe_result / handle_output
            Ok(())
        }

        None => {
            let conn = self.connections.get_mut(&conn_id).unwrap();
            conn.send(Message::SubscribeResult {
                success: false,
                error: Some("Agent not found".into()),
            }).await
        }
    }
}

/// Called when Output arrives (from local PTY or upstream connection)
async fn handle_output(&mut self, agent: &AgentId, data: &[u8]) {
    if let Some(subscribers) = self.subscriptions.get(agent) {
        for &conn_id in subscribers {
            if let Some(conn) = self.connections.get_mut(&conn_id) {
                let _ = conn.write_output(agent, data).await;
            }
        }
    }
}
```

The `subscriptions` HashMap serves both local and proxy cases:
- Local agent: subscribers are terminals/apps connected to this server
- Remote agent: subscribers are connections that want proxied data

---

## What's NOT here (intentionally deferred)

- **Error types** - flesh out during implementation
- **Graceful shutdown** - drain connections, kill agents cleanly
- **Metrics/logging** - add observability later
- **Terminal resize handling** - PTY window size updates
- **Reconnection logic** - client reconnect after disconnect
- **Rate limiting** - beyond token quotas

---
