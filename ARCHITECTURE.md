# Architecture

A detailed design for the amux server internals covering data structures, message flow, routing, and the task model.

## Quick Overview

**What is amux?** A multiplexer for AI agent sessions (Claude, Codex, etc.) that enables:
- Multiple terminals attaching to the same agent
- Remote access via cloud relay
- Rich clients (mobile/web) receiving structured logs via WebSocket

**Core concepts:**
- **Server** - manages connections, agents, and routing
- **Connection** - Unix socket, TCP, or WebSocket; all use the same framed message protocol
- **LocalAgentSession** - a running agent with PTY, replay buffers, and structured log buffers
- **Routing Table** - maps link names to channels for forwarding messages

**Key design choices:**
- All connections (local and remote) use framed messages with length-prefixed encoding
- Connections are identified by link names (e.g. `"term-abc1"`, `"myhost-xyz2"`)
- Messages are either `Routable` (carry src/dst routes, can be forwarded) or `Local` (handled on the receiving server only)
- Serialization uses MessagePack for binary transports (Unix/TCP) and JSON for WebSocket

---

## Glossary

| Term | Description |
|------|-------------|
| **agent_id** | UUID identifying an agent session. Optional human-readable alias can be set via `-t` flag. |
| **link_name** | String identifying a connection (e.g. `"term-abc1"`, `"myhost"`, `"hook-xy12"`). Used as keys in the routing table. |
| **Route** | A stack of link names (`VecDeque<String>`) representing a multi-hop path. Serializes as `"AB.BC.CD"` (dot-separated). |
| **RoutableMessage** | A message that carries `src` and `dst` routes and can be forwarded across hops. |
| **LocalMessage** | A message handled only by the directly connected server (no routing). |
| **PTY** | Pseudo-terminal - the interface used to run interactive CLI agents like Claude. |

---

## Core Identity Types

```rust
// Agents are identified by UUID, with an optional human-readable alias
agent_id: Uuid           // e.g. 550e8400-e29b-41d4-a716-446655440000
alias: Option<String>    // e.g. "my-session" (set via -t flag)

// Connections are identified by link name strings
link_name: String        // e.g. "term-abc1", "myhost-xyz2", "hook-ab12"
```

### Route

A route is a stack of link names representing a path through the network. The top of the stack (front of deque) is the next hop.

```rust
/// Serializes as "AB.BC.CD" where AB is the first hop.
struct Route {
    links: VecDeque<String>,
}

impl Route {
    fn from_link(link: impl Into<String>) -> Self;  // Single-hop route
    fn push(&mut self, link: impl Into<String>);     // Push link to front (new next hop)
    fn pop(&mut self) -> Option<String>;             // Pop next hop

    /// Prepare to send: pops from dst, creates src from the popped link.
    /// Returns (src, dst) for the message.
    fn send(dst: Route) -> Option<(Route, Route)>;

    /// Prepare a reply: sends back through the src path.
    /// Returns (reply_src, reply_dst).
    fn reply(src: Route) -> Option<(Route, Route)>;
}
```

Link names are generated with random suffixes for uniqueness:
- Terminal connections: `"term-{rand}"` (4 alphanumeric chars)
- Server connections: `"{hostname}-{rand}"` or just `"{hostname}"` if `randomise_link_name` is false
- Hook connections: `"hook-{rand}"`

See `src/route.rs`.

---

## Transport Abstraction

All transports implement a simple two-method trait:

```rust
#[async_trait]
trait Transport: Send + Sync {
    async fn read_message(&mut self) -> Result<Message>;
    async fn write_message(&mut self, msg: &Message) -> Result<()>;
}
```

Three implementations:

| Transport | Stream | Serialization | Framing | Flush |
|-----------|--------|---------------|---------|-------|
| `UnixTransport` | `UnixStream` (split into read/write halves) | MessagePack | Length-prefixed | No |
| `TcpTransport<S>` | Generic over `AsyncRead + AsyncWrite` (plain TCP or TLS) | MessagePack | Length-prefixed | Yes |
| `WebSocketTransport` | `WebSocketStream<TcpStream>` | JSON | WebSocket native | N/A |

`TcpTransport` is generic over the stream type, allowing it to wrap both plain TCP and TLS streams (e.g. `TcpTransport<ClientTlsStream<TcpStream>>`).

See `src/transport/`.

---

## Message Types

The protocol uses a two-variant top-level enum that separates routable messages (forwarded across hops) from local messages (handled by the directly connected server):

```rust
enum Message {
    Routable {
        src: Route,      // Return path (built up as message travels)
        dst: Route,      // Forward path (consumed as message travels)
        message: RoutableMessage,
    },
    Local(LocalMessage),
}
```

### RoutableMessage

Messages that carry routing information and can be forwarded across server hops:

```rust
enum RoutableMessage {
    // Subscribing to agent output
    Subscribe { agent_id: String, rows: u16, cols: u16, mode: SubscribeMode },
    SubscribeResult { agent_id: String, success: bool, error: Option<ProtocolError> },

    // Input to agents
    InputBytes { agent_id: String, data: Vec<u8> },     // Raw keystroke bytes
    SubmitInput { agent_id: String, data: Vec<u8> },     // Structured input (adds delay + CR)

    // Output from agents
    Output { agent_id: String, data: Vec<u8> },          // Raw terminal bytes
    StructuredOutput { agent_id: String, entry: StructuredLog },

    // Permission handling (Claude Code integration)
    PermissionRequestResponse { agent_id: String, response: PermissionResponse },

    // Routing errors
    Error(ProtocolError),
}
```

### LocalMessage

Messages handled only by the directly connected server:

```rust
enum LocalMessage {
    // Agent management
    ListAgents,
    ListAgentsResult { agents: Vec<AgentInfo> },
    CreateAgent(CreateAgentRequest),
    CreateAgentResult { success: bool, error: Option<ProtocolError> },
    AgentEnded,

    // Connection handshake
    Connect { link_name: String, token: Option<String> },
    ConnectResponse { success: bool, error: Option<ProtocolError> },

    // Server operations
    Shutdown,
    Debug,
    DebugResult { info: ServerDebugInfo },
    ConnectToServer { address: String },
    ConnectToServerResult { success: bool, error: Option<ProtocolError> },

    // Hooks (Claude Code integration)
    HookEvent { hook: Hook },
    HookEventResult { success: bool, error: Option<ProtocolError> },

    // Errors
    Error { message: String },
}
```

### Supporting Types

```rust
enum ProtocolError {
    ServerError(String),
    LinkNameTaken,
    NoRouteFound(Route),       // Includes the path traversed before failure
    InvalidCredentials,
}

enum SubscribeMode {
    Raw,         // Stream raw terminal bytes as Output messages (default)
    Structured,  // Stream structured logs as StructuredOutput messages
}
```

### Serialization

Messages are serialized using MessagePack (rmp-serde) in named/map format for binary transports, and JSON for WebSocket:

```rust
impl Message {
    fn encode(&self) -> Result<Vec<u8>> {
        rmp_serde::to_vec_named(self)
    }
    fn decode(data: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(data)
    }
}
```

See `src/message.rs`.

---

## Connection Handling

All connections (Unix, TCP, WebSocket) use the same unified `connection_loop`:

```rust
async fn connection_loop<T: Transport>(
    transport: &mut T,
    outgoing_rx: mpsc::Receiver<Message>,  // Messages to send to this connection
    ctx: ConnectionContext,                 // Shared server state + link name
)
```

The loop uses `tokio::select!` on two sources:
1. **`transport.read_message()`** - Incoming messages from the connection
2. **`outgoing_rx.recv()`** - Messages queued by other parts of the server to send to this connection

Incoming messages are dispatched by `handle_message`:
- `Message::Routable { src, dst, message }` → `handle_routable()` (routing + local delivery)
- `Message::Local(msg)` → `handle_local()` (direct handling)

### Per-Connection State

```rust
struct ConnectionContext {
    state: Arc<RwLock<ServerState>>,       // Shared server state
    event_tx: mpsc::Sender<SessionEvent>,  // Channel to notify server of session events
    link_name: String,                     // This connection's link name
}
```

Each connection has a dedicated `mpsc::Sender<Message>` stored in the routes table. Other tasks send messages to a connection by looking up its link name in the routes table and sending through the channel.

For cloud connections, the loop extends with token refresh support: a third `select!` branch fires when the JWT token is nearing expiry, triggering in-band re-authentication via `LocalMessage::Connect`.

See `src/server/connection.rs`.

---

## Server

```rust
struct ServerState {
    config: Config,
    cloud_mode: bool,
    agents: HashMap<Uuid, Arc<LocalAgentSession>>,
    routes: HashMap<String, mpsc::Sender<Message>>,
    jwt_validator: Option<Arc<JwtValidator>>,
}

struct Server {
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
}
```

The server's `run()` method:
1. Binds Unix socket, TCP, and WebSocket listeners
2. Optionally sets up TLS (cloud mode, using `AMUX_TLS_CERT`/`AMUX_TLS_KEY` env vars)
3. Optionally establishes cloud connection (local mode with cloud enabled)
4. Enters main `select!` loop handling: listener accepts, session events, shutdown signal

**Routes table:** `HashMap<String, mpsc::Sender<Message>>` keyed by link name. Each entry is the send-half of a channel to a connection's `connection_loop`. When a connection disconnects, its route is removed.

**Subscriptions:** Unlike the original design, there is no explicit subscriptions HashMap. When a client subscribes to an agent, a dedicated output-streaming task is spawned that reads from the agent's `MultiplexBuffer` and writes `Output` messages to the subscriber's channel. The subscription is implicit in the lifetime of this task.

See `src/server/mod.rs`.

---

## Local Agent Session

```rust
struct LocalAgentSession {
    agent_id: Uuid,
    alias: Option<String>,
    command: String,
    working_dir: PathBuf,
    pty_master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    buffer: Arc<MultiplexBuffer>,                              // PTY output replay + broadcast
    log_buffer: Arc<MultiplexLogBuffer>,                       // Structured log replay + broadcast
    transcript_tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    current_size: Arc<Mutex<(u16, u16)>>,                      // Terminal rows, cols
}
```

### Key Methods

```rust
impl LocalAgentSession {
    fn new(req: &CreateAgentRequest, event_tx: mpsc::Sender<SessionEvent>) -> Result<Self>;

    /// Atomic subscribe: returns (MultiplexReader, input_sender).
    /// MultiplexReader receives all existing output (replay) then live output.
    async fn subscribe(&self) -> Option<(MultiplexReader, mpsc::Sender<Vec<u8>>)>;

    /// Subscribe to structured logs (for dashboard/rich clients).
    async fn subscribe_logs(&self) -> Option<MultiplexLogReader>;

    async fn send_input(&self, data: Vec<u8>) -> Result<()>;
    async fn resize(&self, rows: u16, cols: u16) -> Result<()>;
    async fn shutdown(&self);
    async fn link_transcript(&self, path: PathBuf);   // Connect Claude Code transcript file
    async fn write_log(&self, entry: StructuredLog);   // Write log entry directly (e.g. permission request)
}
```

### CreateAgentRequest

```rust
struct CreateAgentRequest {
    agent_id: Uuid,
    alias: Option<String>,
    agent_type: AgentType,
    working_dir: PathBuf,
    rows: u16,
    cols: u16,
}

enum AgentType {
    Claude,                          // Passes --session-id to claude command
    TestAgent(String),               // Dev/test only
}
```

See `src/session.rs`.

---

## Config

```rust
struct Config {
    host_name: String,               // Hostname for generating link names (default: system hostname)
    cloud_url: String,               // Cloud API URL (default: "https://amux.sh")
    socket_path: PathBuf,            // Unix socket path (default: /tmp/amux.sock)
    tcp_port: u16,                   // TCP port for server-to-server (default: 9001)
    websocket_port: u16,             // WebSocket port for rich clients (default: 9002)
    randomise_link_name: bool,       // Add random suffix to link names (default: true, test-only override)
    state_path: PathBuf,             // Path to persistent state file
    enforce_tls_in_cloud_mode: bool, // Whether cloud server handles TLS itself (default: true)
}
```

Config is loaded from YAML file at `~/.config/amux/config.yaml` (auto-detected) or via `--config` flag. All fields have serde defaults.

See `src/config.rs`.

---

## StructuredLog

Structured log entries for rich clients (dashboard, mobile). Populated by parsing Claude Code transcript files via `TranscriptTailer`:

```rust
#[serde(tag = "type")]
enum StructuredLog {
    UserMessage { content: String, timestamp: String, uuid: String },
    AssistantMessage { content: String, timestamp: String, uuid: String },
    PermissionRequest { tool: PermissionTool },
}

enum PermissionTool {
    Edit { file_path: String, old_string: String, new_string: String },
}
```

`TranscriptTailer` watches a Claude Code transcript JSONL file and parses new entries into `StructuredLog` variants, writing them to the session's `MultiplexLogBuffer`. Permission requests are also written directly to the log buffer when received as hook events.

See `src/structured_log.rs`, `src/transcript.rs`.

---

## MultiplexBuffer

The core abstraction for agent output replay and broadcast. Supports multiple concurrent readers with atomic subscribe (no data loss or duplication between replay and live output).

```rust
struct MultiplexBuffer {
    buffer: RwLock<Vec<u8>>,                           // All bytes (up to max_size)
    subscribers: RwLock<Vec<mpsc::UnboundedSender<Vec<u8>>>>,
    max_size: usize,                                    // 10MB default
    closed: RwLock<bool>,
}

impl MultiplexBuffer {
    /// Write bytes: appends to buffer, broadcasts to all subscribers.
    /// Holds write lock during both operations for atomicity.
    async fn write(&self, bytes: &[u8]);

    /// Subscribe: returns MultiplexReader that receives all existing bytes
    /// then live updates. Holds read lock during subscribe for atomicity with write.
    /// Returns None if closed.
    async fn subscribe(&self) -> Option<MultiplexReader>;

    /// Close: drops all subscriber channels, prevents new subscriptions.
    async fn close(&self);
}

struct MultiplexReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}
```

The key invariant: `write()` and `subscribe()` are mutually exclusive via the buffer lock. This ensures a new subscriber sees exactly all bytes written before it subscribed, with no gaps and no duplicates in the transition to live data.

`MultiplexLogBuffer` follows the same pattern for `StructuredLog` entries (with entry count limit instead of byte size limit).

See `src/buffer.rs`, `src/multiplex_log_buffer.rs`.

---

## Routing Table

The routing table is a `HashMap<String, mpsc::Sender<Message>>` keyed by link name. Each entry is the send-half of a per-connection channel.

### Forwarding Algorithm

When a `Routable` message arrives at `handle_routable`:

1. **Pop** the next hop from `dst`
2. **If `Some(next_hop)`:** This message needs forwarding
   - Push `next_hop` onto `src` (building the return path)
   - Look up `next_hop` in `state.routes`
   - Send the message through the channel
   - On channel send failure:
     - Remove stale route if channel is closed
     - Send `RoutableMessage::Error(NoRouteFound(...))` back via `Route::reply(src)`
3. **If `None`:** This message has arrived at its destination
   - Deliver locally (subscribe, input, output, etc.)

### Reply Routing

Replies use `Route::reply(src)`, which pops the first link from `src` to determine the next hop and creates the reply's `src` from that link. This naturally reverses the path.

### Error Handling

- **Request messages** (Subscribe, InputBytes, etc.): forwarding failure sends `RoutableMessage::Error(NoRouteFound)` back to the source
- **Stream messages** (Output, StructuredOutput): forwarding failure is logged and dropped silently to prevent churn
- **Error messages**: forwarding failure is logged and dropped to prevent amplification

### Route Cleanup

When a connection disconnects, its route is removed from `state.routes`. Stale routes (where the channel has closed but the route hasn't been cleaned up yet) are detected during forwarding and cleaned up opportunistically.

See `src/route.rs`, `src/server/connection.rs`.

---

## Agent Lifecycle

### Spawning

`LocalAgentSession::new()` creates a PTY and spawns three background tasks:

```
Task 1: PTY Reader (spawn_blocking)
  - Reads PTY stdout in a blocking loop
  - Writes bytes to MultiplexBuffer (which broadcasts to all subscribers)

Task 2: Input Forwarder (spawn)
  - Reads from input_rx channel
  - Writes to PTY stdin

Task 3: Child Waiter (spawn_blocking)
  - Waits for child process to exit
  - Drops PTY master
  - Closes MultiplexBuffer (disconnects all subscribers)
  - Sends SessionEvent::Ended to server
```

The agent type determines the command and arguments:
- `AgentType::Claude` → runs `claude --session-id={agent_id}`
- `AgentType::TestAgent(cmd)` → runs the given command (dev/test only)

### Termination

1. Child process exits
2. Child waiter task detects exit, drops PTY master, closes buffers
3. `SessionEvent::Ended(agent_id)` sent to server via event channel
4. Server removes agent from `state.agents`
5. Output streaming tasks detect buffer closure and send `LocalMessage::AgentEnded` to their subscribers

See `src/session.rs`.

---

## Connection Lifecycle

### Accepting Connections (Server-Side)

`accept_handshake()` handles the initial handshake:

1. Read first message, expect `LocalMessage::Connect { link_name, token }`
2. If `verify_token` (cloud mode): validate JWT token via JWKS
3. Check link name uniqueness in `state.routes` (read lock fast path, write lock for insert)
4. Create `mpsc::channel` for the connection, insert sender into `state.routes`
5. Send `LocalMessage::ConnectResponse { success: true }`
6. Return `(link_name, outgoing_rx)` for use in `connection_loop`

On link name collision, the server responds with `ConnectResponse { error: LinkNameTaken }`. The client retries with a new random suffix (up to 5 attempts).

After handshake, `accept_connection()` runs `connection_loop()` until disconnection, then removes the route from `state.routes`.

### Connecting to Peers (Client-Side)

`connect_handshake()` sends `LocalMessage::Connect { link_name, token: None }` and waits for `ConnectResponse`. On `LinkNameTaken`, it regenerates the link name and retries (up to 5 attempts).

### Cleanup

When a connection drops:
1. Route is removed from `state.routes`
2. Any output streaming tasks for this connection detect the closed channel and stop

See `src/server/accept.rs`.

---

## Framing (TCP/Unix)

Binary transports use length-prefixed framing via the `LengthPrefixed<R, W>` helper:

```
+---------------------------+-------------------+
| length (4 bytes, big-endian) | payload (N bytes) |
+---------------------------+-------------------+
```

- Maximum frame size: 16MB (prevents DoS)
- TCP transports flush after each write (for TCP_NODELAY latency)
- Unix transports do not flush (not applicable)
- Payload is MessagePack-encoded `Message`

WebSocket handles framing natively; `WebSocketTransport` reads/writes JSON text messages directly.

See `src/transport/framing.rs`.

---

## Input Forwarding

Two input message types, both routable:

- **`InputBytes`** - Raw keystroke bytes, delivered directly to PTY stdin
- **`SubmitInput`** - Structured input from rich clients (e.g. dashboard text field). Adds a 20ms delay then appends carriage return (`\r`)

Input messages are forwarded via generic `handle_routable` routing. When delivered locally, the agent is resolved by UUID or alias and `send_input()` is called.

---

## Task Model

```
┌────────────────────────────────────────────────────────────────────────┐
│                              Server                                    │
│                                                                        │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐ │
│  │ Unix Listener    │  │ TCP Listener     │  │ WebSocket Listener   │ │
│  └────────┬─────────┘  └────────┬─────────┘  └──────────┬───────────┘ │
│           │                     │                        │             │
│           └─────────────────────┼────────────────────────┘             │
│                                 │                                      │
│                    spawns per connection                                │
│                                 ▼                                      │
│           ┌─────────────────────────────────────────┐                  │
│           │       Connection Handler Task            │                  │
│           │                                          │                  │
│           │  tokio::select! {                        │                  │
│           │    transport.read_message() => dispatch   │                  │
│           │    outgoing_rx.recv() => write to socket  │                  │
│           │    token_refresh => re-authenticate       │                  │
│           │  }                                        │                  │
│           └─────────────────────────────────────────┘                  │
│                                                                        │
│  Per Subscribe (spawned on subscribe):                                 │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Output Stream Task                                                │  │
│  │ Reads MultiplexBuffer/MultiplexLogBuffer → sends Output/          │  │
│  │ StructuredOutput routable messages to subscriber's channel        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  Per Local Agent:                                                      │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐ │
│  │ PTY Reader       │  │ Input Forwarder  │  │ Child Waiter         │ │
│  │ (spawn_blocking) │  │ (spawn)          │  │ (spawn_blocking)     │ │
│  │                  │  │                  │  │                      │ │
│  │ PTY stdout →     │  │ input_rx →       │  │ Waits for exit →    │ │
│  │ MultiplexBuffer  │  │ PTY stdin        │  │ SessionEvent::Ended  │ │
│  └──────────────────┘  └──────────────────┘  └──────────────────────┘ │
│                                                                        │
│  Optional:                                                             │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Cloud Connection Task (local servers only)                        │  │
│  │ TLS connect → handshake → connection_loop with token refresh      │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Session Event Handler                                             │  │
│  │ Receives SessionEvent::Ended → removes agent from state          │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  Per Agent with Transcript:                                            │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Transcript Tailer                                                 │  │
│  │ Watches JSONL file → parses → writes to MultiplexLogBuffer        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
└────────────────────────────────────────────────────────────────────────┘
```

**Data flow for local terminal subscription:**
```
Terminal ──Subscribe──> Connection Handler
                              │
                              ▼
                       handle_routable → local delivery
                              │
                              ├─> agent.subscribe() → MultiplexReader
                              ├─> Send SubscribeResult
                              └─> Spawn output stream task
                                    │
                                    └─> MultiplexReader.read() → Output msg → subscriber channel

Terminal ──InputBytes──> Connection Handler → agent.send_input() → PTY stdin
```

**Data flow for proxied subscription:**
```
App ──Subscribe──> Cloud Handler ──Subscribe──> Local Handler
                        │                           │
                   (pops dst,                  (dst empty,
                    pushes src,                 local delivery)
                    forwards)                       │
                                               Subscribe agent
                                               Spawn output task

Local PTY → MultiplexBuffer → Output Stream Task → Output msg
    → Cloud routes table → Cloud connection → App
```

---

## Hooks System

amux integrates with Claude Code via hooks - shell commands that Claude Code calls on specific events.

### Hook Events

```rust
enum Hook {
    Claude(ClaudeHook),
}

enum ClaudeHook {
    SessionStart(ClaudeSessionStart),         // Claude Code session started
    PermissionRequest(ClaudePermissionRequest), // Claude Code requesting tool permission
}
```

### Hook Connection Flow

1. Claude Code calls `amux hooks claude session-start` (reads JSON from stdin)
2. `amux` connects to server via Unix socket with a `"hook-{rand}"` link name
3. Sends `HookEvent { hook }` message
4. Server handles: for `SessionStart`, links the transcript file to the agent session
5. For `PermissionRequest`, writes to the agent's log buffer and waits for a `PermissionRequestResponse` from a dashboard client

### Permission Request Flow

```
Claude Code → amux hooks → HookEvent(PermissionRequest) → server
    → agent.write_log(PermissionRequest) → MultiplexLogBuffer
    → dashboard (via StructuredOutput) → user sees permission UI
    → PermissionRequestResponse (routable) → server → writes keystroke to PTY
```

See `src/hooks.rs`, `src/message.rs`.

---

## Key Design Decisions

### 1. Unified connection_loop for all transports

Rather than separate `LocalConnection` and `RemoteConnection` types with different behavior, all connections use the same `connection_loop`. The `Transport` trait abstracts away the underlying stream. This eliminates code duplication and ensures protocol consistency across transports.

### 2. MultiplexBuffer atomic subscribe

The original design had a race condition window between getting the replay buffer and starting to receive live data. `MultiplexBuffer` solves this by holding a lock during both the snapshot and subscriber registration, ensuring zero gaps or duplicates. This is the core correctness guarantee for late-joining terminals.

### 3. MessagePack serialization

MessagePack (rmp-serde with named/map format) replaced bincode for binary transports. Named format provides forward/backward compatibility when fields are added, unlike bincode's positional encoding. JSON is used for WebSocket (human readable, web client friendly).

### 4. Handshake-based connection establishment

Connections start with a `Connect { link_name, token }` / `ConnectResponse` handshake instead of the original `EstablishConnection`. This:
- Assigns link names at connect time (used for routing)
- Carries JWT tokens for cloud authentication
- Supports link name collision retry

### 5. Implicit subscriptions via spawned tasks

Instead of a centralized `subscriptions: HashMap<AgentId, Vec<ConnectionId>>`, subscriptions are implicit in spawned output-streaming tasks. When a client subscribes, a task is spawned that reads from `MultiplexBuffer` and sends to the subscriber's channel. The subscription dies when either the buffer closes or the subscriber disconnects.

### 6. Routable/Local message split

The `Message` enum has two variants that encode routing capability in the type system:
- `Routable { src, dst, message }` - Can be forwarded across hops. Generic forwarding logic handles all routable message types uniformly.
- `Local(message)` - Handled only by the directly connected server. Cannot be forwarded.

This collapses six separate forwarding code paths into one generic forwarding path in `handle_routable`.

### 7. Stack-based routing

Routes are stacks (VecDeque) of link names rather than flat `Route::Remote { via }` entries. At each hop, the next link is popped from `dst` and pushed to `src`. This naturally:
- Builds the return path as the message travels
- Supports multi-hop forwarding without each intermediate server needing full topology knowledge
- Enables reply routing by simply swapping src/dst

---

## Proxying / Multi-hop Subscriptions

When a client subscribes to an agent on a remote server, messages are forwarded through intermediate servers:

```
App ──Subscribe──> Cloud Server ──Subscribe──> Local Server
                        │                          │
                        │                     (owns agent)
                        │                          │
App <──Output──────── Cloud <────Output────────────┘ (ongoing)
App ──InputBytes───> Cloud ────InputBytes──────────>│
```

All forwarding is handled by generic `handle_routable`:
1. Pop next hop from `dst`
2. Push it to `src`
3. Look up in routes table
4. Send through channel

The same code forwards Subscribe, SubscribeResult, InputBytes, Output, StructuredOutput, PermissionRequestResponse, and Error messages. No message-type-specific forwarding logic needed.

---

## What's NOT Here (intentionally deferred)

- **Agent propagation to peers** - Remote servers don't yet advertise their agents to connected peers. `list-agents` only returns local agents.
- **Multi-user access** - Cross-user collaboration (user A accessing user B's agents)
- **Local network discovery** - Automatic discovery of amux servers on LAN
- **Reconnection logic** - Client-side reconnect after disconnect
- **Rate limiting** - Beyond token quotas

---
