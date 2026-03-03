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
- Messages are `Routable` (carry src/dst routes + opaque payload, forwarded across hops), `Direct` (peer-to-peer, handled by directly connected server), or `Command` (CLI-only, rejected from remote peers)
- Serialization uses MessagePack for all transports (Unix, TCP, WebSocket)

---

## Glossary

| Term | Description |
|------|-------------|
| **agent_id** | UUID identifying an agent session. Optional human-readable name can be set via `-t` flag. |
| **link_name** | String identifying a connection (e.g. `"term-abc1"`, `"myhost"`, `"hook-xy12"`). Used as keys in the routing table. |
| **Route** | A stack of link names (`VecDeque<String>`) representing a multi-hop path. Serializes as `"AB.BC.CD"` (dot-separated). |
| **RoutableMessage** | A message that carries `src` and `dst` routes and can be forwarded across hops. |
| **DirectMessage** | A message handled only by the directly connected server (no routing). |
| **PTY** | Pseudo-terminal - the interface used to run interactive CLI agents like Claude. |

---

## Core Identity Types

```rust
// Agents are identified by UUID, with an optional human-readable name
agent_id: Uuid           // e.g. 550e8400-e29b-41d4-a716-446655440000
name: Option<String>     // e.g. "my-session" (set via -t flag)

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

    /// Prepare a reply: literally just send(src). The src route accumulated
    /// hop links on the way in, so sending through it reverses the path.
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

All transports implement a simple two-method trait, with split support for the reader/writer task architecture:

```rust
trait Transport: Send + Sync {
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;
}

trait MessageReader: Send {
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;
}

trait MessageWriter: Send {
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;
    fn background(&mut self) -> impl Future<Output = ()> + Send { std::future::pending() }
}

trait TransportSplit: Transport {
    type Reader: MessageReader + 'static;
    type Writer: MessageWriter + 'static;
    fn into_split(self) -> (Self::Reader, Self::Writer);
}
```

Three implementations:

| Transport | Stream | Serialization | Framing | Flush |
|-----------|--------|---------------|---------|-------|
| `UnixTransport` | `UnixStream` (split into read/write halves) | MessagePack | Length-prefixed | No |
| `TcpTransport<S>` | Generic over `AsyncRead + AsyncWrite` (plain TCP or TLS) | MessagePack | Length-prefixed | Yes |
| `WebSocketTransport` | `WebSocketStream<TcpStream>` | MessagePack | WebSocket native (binary frames) | N/A |

`TcpTransport` is generic over the stream type, allowing it to wrap both plain TCP and TLS streams (e.g. `TcpTransport<ClientTlsStream<TcpStream>>`).

See `src/transport/`.

---

## Message Types

The protocol uses a three-variant top-level enum:

```rust
enum Message {
    /// Routed messages with opaque payload. Intermediate servers forward
    /// the payload without deserializing it.
    Routable {
        src: Route,          // Return path (built up as message travels)
        dst: Route,          // Forward path (consumed as message travels)
        request_id: u64,     // Monotonically increasing per-connection counter
        payload: Vec<u8>,    // Serialized RoutableMessage (opaque to intermediate hops)
    },
    /// Peer-to-peer messages handled by the directly connected server.
    Direct(DirectMessage),
    /// CLI-only commands. MUST be rejected if received over TCP or WebSocket.
    Command(Command),
}
```

`Message::routable(src, dst, request_id, &routable_message)` is a convenience constructor that encodes the `RoutableMessage` into the opaque payload.

### RoutableMessage

Messages that carry routing information and can be forwarded across server hops. Deserialized from `payload` at the final destination only:

```rust
enum RoutableMessage {
    // Subscribing to agent output
    SubscribeRaw { agent_id: Uuid, terminal_size: Option<TerminalSize> },
    SubscribeStructured { agent_id: Uuid },
    SubscribeRawResult { agent_id: Uuid, error: Option<ProtocolError> },
    SubscribeStructuredResult { agent_id: Uuid, error: Option<ProtocolError> },

    // Input to agents
    RawInput { agent_id: Uuid, data: Vec<u8> },           // Raw keystroke bytes
    StructuredInput { agent_id: Uuid, data: StructuredInput }, // Agent-type-keyed input

    // Output from agents
    RawOutput { agent_id: Uuid, data: Vec<u8> },           // Raw terminal bytes
    StructuredOutput { agent_id: Uuid, data: StructuredOutput }, // Agent-type-keyed output

    // Agent creation
    CreateAgent(CreateAgentRequest),
    CreateAgentResult { agent_id: Uuid, error: Option<ProtocolError> },

    // Subscription lifecycle
    SubscriptionClosed { agent_id: Uuid },  // Subscription EOF (agent ended or buffer closed)

    // Error handling
    UnknownMessage,  // Sent back when the endpoint can't deserialize the payload
}
```

`RoutableMessage` has its own `encode()`/`decode()` methods for the two-step serialization used with opaque payloads.

### DirectMessage

Peer-to-peer session messages between directly connected servers (no routing):

```rust
enum DirectMessage {
    // In-session authentication refresh (cloud links)
    Reauth { token: String },
    ReauthResult { error: Option<ProtocolError> },

    // Agent discovery (pure registry, no routing side effects)
    AnnounceAgent { agent_id: Uuid, name: Option<String>, command: String, working_dir: PathBuf, route: Route },
    WithdrawAgent { agent_id: Uuid },

    // Host/route management (single source of routing truth)
    AnnounceHost { id: Uuid, name: String, route: Route, version: String },
    WithdrawHost { id: Uuid },
}
```

### Handshake

Connection bootstrap is a separate wire protocol (not part of `Message`):

```rust
const PROTOCOL_VERSION: u32 = 1;

struct Connect {
    link_name: String,
    token: Option<String>,
    version: u32, // required
}

struct ConnectResult {
    error: Option<ProtocolError>,
}
```

Handshake frames use MessagePack map encoding and are exchanged before the connection enters the normal session message loop.

### Command

CLI-only messages sent over Unix socket. Servers reject these if received over TCP or WebSocket:

```rust
enum Command {
    ListAgents,
    ListAgentsResult { agents: Vec<Agent> },
    ResolveAgent { identifier: String },
    ResolveAgentResult { agent: Option<Agent> },
    Shutdown,
    ShutdownNotification(ShutdownReason),
    Debug,
    DebugResult { info: ServerDebugInfo },
    ConnectToServer { address: String },
    ConnectToServerResult { error: Option<ProtocolError> },
    HandleHook { hook: Hook },
    HandleHookResult { error: Option<ProtocolError> },
}

enum ShutdownReason {
    ProtocolMismatch,  // Server received version mismatch from cloud
    UserRequested,     // User ran kill-server
}
```

### Structured Wrapper Types

Agent-type-keyed wrappers for structured input/output. The outer enum is externally tagged (serde default), the inner enums are internally tagged:

```rust
// Output wrapper (externally tagged)
enum StructuredOutput {
    Claude(ClaudeStructuredOutput),
}

// Input wrapper (externally tagged)
enum StructuredInput {
    Claude(ClaudeStructuredInput),
}
```

See the [StructuredOutput / StructuredInput](#structuredoutput--structuredinput) section for the Claude-specific inner types.

### Supporting Types

```rust
enum ProtocolError {
    ServerError(String),
    LinkNameTaken,
    InvalidCredentials,
    InvalidLinkName,
    VersionMismatch { server_version: u32, client_version: u32 },
}
```

### Serialization

Messages are serialized using MessagePack (rmp-serde) in named/map format for all transports:

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

All connections (Unix, TCP, WebSocket) use a split transport architecture with three tasks:

1. **Reader task** (`reader_loop`) — reads from transport, sends `Incoming` events to a channel. Never cancelled by `select!`.
2. **Writer task** (`writer_loop`) — drains a message channel, writes to transport. Also handles transport-specific background I/O (e.g., WebSocket pong responses).
3. **Connection loop** (`connection_loop`) — pure channel I/O, cancellation-safe. Uses `tokio::select!` on:
   - `incoming_rx.recv()` — incoming messages from the reader task
   - Token refresh deadline (cloud connections only)
   - Token refresh response timeout

Incoming messages are dispatched by `handle_message`:
- `Message::Routable { src, dst, request_id, payload }` → `handle_routable()` (routing + local delivery)
- `Message::Direct(msg)` → `handle_direct()` (peer-to-peer handling)
- `Message::Command(cmd)` → `handle_command()` (CLI-only, rejected if `!is_local`)

### Per-Connection State

```rust
struct ConnectionContext {
    state: Arc<RwLock<ServerState>>,           // Global server state
    user_state: Arc<RwLock<ServerUserState>>,   // Per-user state (agents, routes, registry)
    user_id: Uuid,                              // Authenticated user ID
    event_tx: mpsc::Sender<SessionEvent>,       // Channel to notify server of session events
    link_name: String,                          // This connection's link name
    is_local: bool,                             // True for Unix socket connections
    next_request_id: Arc<AtomicU64>,            // Monotonically increasing counter for outgoing messages
}
```

Each connection has a dedicated `mpsc::Sender<Message>` stored in the routes table. Other tasks send messages to a connection by looking up its link name in the routes table and sending through the channel.

For cloud connections, the loop extends with token refresh support: a `select!` branch fires when the JWT token is nearing expiry, triggering in-band re-authentication via `DirectMessage::Reauth`.

See `src/server/connection.rs`.

---

## Server

```rust
/// Global server state
struct ServerState {
    config: Config,
    host_id: Uuid,                                          // Ephemeral, generated at startup
    is_cloud_server: bool,
    jwt_validator: Option<Arc<JwtValidator>>,
    users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,     // Per-user isolation
}

/// Per-user state. Each authenticated user gets isolated agents, routes, registry,
/// peer links, and streams. LOCAL_USER_ID (Uuid::nil()) is used for non-authenticated
/// connections. User isolation is enforced on cloud servers via JWT authentication.
struct ServerUserState {
    agents: HashMap<Uuid, Arc<LocalAgentSession>>,
    routes: HashMap<String, mpsc::Sender<Message>>,
    registry: AgentRegistry,                                // Local + remote agents, name mapping
    peer_links: HashSet<String>,                            // Links that receive announcements
    hosts: HashMap<Uuid, Host>,                             // Known remote hosts
    active_streams: HashMap<Uuid, Vec<StreamEntry>>,        // Cancellable output streams
    next_stream_id: u64,
}
```

The server's `run()` method:
1. Binds Unix socket, TCP, and WebSocket listeners
2. Optionally sets up TLS (cloud mode, using `AMUX_TLS_CERT`/`AMUX_TLS_KEY` env vars)
3. Optionally establishes cloud connection (local mode with cloud enabled)
4. Enters main `select!` loop handling: listener accepts, session events, shutdown signal

**Routes table:** `HashMap<String, mpsc::Sender<Message>>` keyed by link name, per-user. Each entry is the send-half of a channel to a connection's writer task. When a connection disconnects, its route is removed.

**Subscriptions:** There is no explicit subscriptions HashMap. When a client subscribes to an agent, a dedicated output-streaming task is spawned that reads from the agent's `MultiplexByteBuffer` and writes `RawOutput` messages to the subscriber's channel. The subscription is implicit in the lifetime of this task. Active streams are tracked in `active_streams` with cancellation tokens for cleanup on host withdrawal.

**Agent Registry:** `AgentRegistry` provides centralized tracking of both local and remote agents with bidirectional name-to-UUID mapping. Used for `ListAgents`, `ResolveAgent`, and agent discovery propagation.

**Hosts:** `HashMap<Uuid, Host>` tracks known remote hosts announced via `AnnounceHost`. When a host is withdrawn, all agents reachable via that host's route are bulk-removed from the registry.

See `src/server/mod.rs`.

---

## Local Agent Session

```rust
struct LocalAgentSession {
    agent_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    pty_master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    buffer: Arc<MultiplexByteBuffer>,                           // PTY output replay + broadcast
    log_buffer: Arc<MultiplexStructuredBuffer>,                 // Structured entry replay + broadcast
    transcript_tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    current_size: Arc<Mutex<(u16, u16)>>,                      // Terminal rows, cols
}
```

### Key Methods

```rust
impl LocalAgentSession {
    fn new(req: &CreateAgentRequest, event_tx: mpsc::Sender<SessionEvent>) -> Result<Self>;

    /// Atomic subscribe: returns (MultiplexByteReader, input_sender).
    /// MultiplexByteReader receives all existing output (replay) then live output.
    async fn subscribe(&self) -> Option<(MultiplexByteReader, mpsc::Sender<Vec<u8>>)>;

    /// Subscribe to structured entries (for rich clients).
    async fn subscribe_logs(&self) -> Option<MultiplexStructuredReader>;

    async fn send_input(&self, data: Vec<u8>) -> Result<()>;
    async fn resize(&self, rows: u16, cols: u16) -> Result<()>;
    async fn shutdown(&self);
    async fn link_transcript(&self, path: PathBuf);   // Connect Claude Code transcript file
    async fn write_log(&self, entry: StructuredOutput);   // Write log entry directly (e.g. permission request)
}
```

### CreateAgentRequest

```rust
struct CreateAgentRequest {
    agent_id: Uuid,
    name: Option<String>,
    agent_type: AgentType,
    working_dir: PathBuf,
    terminal_size: Option<TerminalSize>,  // None means use defaults
}

struct TerminalSize {
    rows: u16,
    cols: u16,
}

enum AgentType {
    Claude,                          // Passes --session-id to claude command
    TestAgent(String),               // Dev/test only (debug builds only)
}
```

See `src/session.rs`.

---

## Config

```rust
struct Config {
    host_name: String,               // Hostname for generating link names (default: system hostname)
    cloud_url: String,               // Cloud API URL (default: "https://amux.sh")
    socket_path: PathBuf,            // Unix socket path (default: per-user runtime dir)
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

## StructuredOutput / StructuredInput

Agent-type-keyed wrapper enums for structured data. The outer enum uses serde's default externally-tagged format; the inner Claude-specific enums use internal tagging.

### StructuredOutput

```rust
// Outer wrapper (externally tagged): {"Claude": {type: "UserMessage", ...}}
enum StructuredOutput {
    Claude(ClaudeStructuredOutput),
}

// Claude-specific output (internally tagged with #[serde(tag = "type")])
enum ClaudeStructuredOutput {
    UserMessage { content: String, timestamp: String, uuid: String },
    AssistantMessage { content: String, timestamp: String, uuid: String },
    PermissionRequest { tool: ClaudePermissionTool },
    #[serde(other)]
    Unknown,     // Forward-compatible: unknown types deserialize to this
}

// Tool data from Claude Code permission requests (internally tagged with #[serde(tag = "tool_name")])
enum ClaudePermissionTool {
    Edit { tool_input: EditToolInput },
    AskUserQuestion { tool_input: AskUserQuestionToolInput },
    Bash { tool_input: BashToolInput },
    Write { tool_input: WriteToolInput },
    WebFetch { tool_input: WebFetchToolInput },
    WebSearch { tool_input: WebSearchToolInput },
    NotebookEdit { tool_input: NotebookEditToolInput },
    Skill { tool_input: SkillToolInput },
    ExitPlanMode { tool_input: ExitPlanModeToolInput },
    #[serde(other)]
    Unknown,     // Forward-compatible: unknown tools deserialize to this
}
```

### StructuredInput

```rust
// Outer wrapper (externally tagged): {"Claude": {"SubmitMessage": {data: [...]}}}
enum StructuredInput {
    Claude(ClaudeStructuredInput),
}

// Claude-specific input
enum ClaudeStructuredInput {
    PermissionResponse(PermissionResponse),   // User's response to a permission request
    SubmitMessage { data: Vec<u8> },          // Rich client text input (adds delay + CR)
}
```

`TranscriptTailer` watches a Claude Code transcript JSONL file and parses new entries into `ClaudeStructuredOutput` variants (wrapped in `StructuredOutput::Claude(...)`), writing them to the session's `MultiplexStructuredBuffer`. Permission requests are also written directly to the log buffer when received as hook events.

See `src/message.rs`, `src/transcript.rs`.

---

## BroadcastBuffer

The core abstraction for agent output replay and broadcast, parameterized by a `BufferPolicy` that controls storage, truncation, and replay semantics. Supports multiple concurrent readers with atomic subscribe (no data loss or duplication between replay and live output).

Two concrete instantiations via type aliases:
- `MultiplexByteBuffer` (`BroadcastBuffer<BytePolicy>`) — contiguous byte stream for PTY output
- `MultiplexStructuredBuffer` (`BroadcastBuffer<StructuredPolicy>`) — discrete structured entries for Claude I/O

```rust
struct BroadcastBuffer<P: BufferPolicy> {
    storage: RwLock<P::Storage>,
    subscribers: RwLock<Vec<mpsc::Sender<P::Item>>>,   // Bounded channels
    capacity: usize,
    closed: RwLock<bool>,
}

impl<P: BufferPolicy> BroadcastBuffer<P> {
    /// Write: appends to storage, broadcasts to all subscribers.
    /// Holds write lock during both operations for atomicity.
    /// Slow subscribers that fall behind are disconnected (bounded backpressure).
    async fn write(&self, item: P::Item);

    /// Subscribe: returns BroadcastReader that receives all existing items
    /// then live updates. Holds read lock during subscribe for atomicity with write.
    /// Returns None if closed.
    async fn subscribe(&self) -> Option<BroadcastReader<P>>;

    /// Close: drops all subscriber channels, prevents new subscriptions.
    async fn close(&self);
}
```

The key invariant: `write()` and `subscribe()` are mutually exclusive via the storage lock. This ensures a new subscriber sees exactly all items written before it subscribed, with no gaps and no duplicates in the transition to live data.

See `src/buffer.rs`.

---

## Routing Table

The routing table is a `HashMap<String, mpsc::Sender<Message>>` keyed by link name. Each entry is the send-half of a per-connection channel.

### Forwarding Algorithm

When a `Routable` message arrives at `handle_routable`:

1. **Pop** the next hop from `dst`
2. **If `Some(next_hop)`:** This message needs forwarding
   - Push `next_hop` onto `src` (building the return path)
   - Look up `next_hop` in `user_state.routes`
   - Forward the `Message::Routable` with opaque `payload` verbatim (no deserialization)
   - On channel send failure: log at debug level and drop silently
3. **If `None`:** This message has arrived at its destination
   - Deserialize `payload` into `RoutableMessage` (two-step deserialization)
   - Deliver locally (subscribe, input, output, etc.)
   - On payload deserialization failure: send `RoutableMessage::UnknownMessage` response

### Reply Routing

`Route::reply(src)` is literally `Route::send(src)` — the same pop-and-push operation applied to the incoming `src` route instead of `dst`. This works because `src` accumulated each hop's link name as the message traveled inward, so "sending" through `src` naturally reverses the path. This symmetry is the entire routing algorithm: there is no separate reply mechanism.

### Routing Failure Handling

Routing failures are silent drops. When a message can't be forwarded (next hop not in routes table or channel closed), the message is logged at debug level and dropped. There are no error messages sent back. Route cleanup flows from `WithdrawHost`, which is the single source of routing truth.

### Route Lifecycle

`AnnounceHost`/`WithdrawHost` are the single source of routing truth:

- **Connection loss:** Server identifies hosts reachable via the dead link, broadcasts `WithdrawHost` per host. Upon receiving `WithdrawHost`, each server removes the host, bulk-removes agents whose route passes through that host, cancels active streams for those agents, and propagates the withdrawal.
- **Agent death:** Server sends `SubscriptionClosed` to subscribers (subscription EOF), sends `WithdrawAgent` per peer (discovery cleanup only), removes agent from registry. No route changes.
- `AnnounceAgent`/`WithdrawAgent` are pure discovery — they update the agent registry for `ListAgents`/`ResolveAgent` but have zero routing side effects.

See `src/route.rs`, `src/server/handlers.rs`, `src/server/routing.rs`.

---

## Agent Lifecycle

### Spawning

`LocalAgentSession::new()` creates a PTY and spawns three background tasks:

```
Task 1: PTY Reader (spawn_blocking)
  - Reads PTY stdout in a blocking loop
  - Writes bytes to MultiplexByteBuffer (which broadcasts to all subscribers)

Task 2: Input Forwarder (spawn)
  - Reads from input_rx channel
  - Writes to PTY stdin

Task 3: Child Waiter (spawn_blocking)
  - Waits for child process to exit
  - Drops PTY master
  - Closes MultiplexByteBuffer (disconnects all subscribers)
  - Sends SessionEvent::Ended to server
```

The agent type determines the command and arguments:
- `AgentType::Claude` → runs `claude --session-id={agent_id}`
- `AgentType::TestAgent(cmd)` → runs the given command (dev/test only)

### Termination

1. Child process exits
2. Child waiter task detects exit, drops PTY master, closes buffers
3. `SessionEvent::Ended(agent_id)` sent to server via event channel
4. Server removes agent from registry, broadcasts `WithdrawAgent` to peers
5. Output streaming tasks detect buffer closure and send `RoutableMessage::SubscriptionClosed` to their subscribers

See `src/session.rs`.

---

## Connection Lifecycle

### Accepting Connections (Server-Side)

`accept_handshake()` handles the initial handshake:

1. Read first frame, decode `Connect { link_name, token, version }`
2. Check protocol version (`version` must match `PROTOCOL_VERSION`)
3. If cloud mode: validate JWT token via JWKS, determine `user_id` from claims
4. Check link name uniqueness in `user_state.routes` (read lock fast path, write lock for insert)
5. Create `mpsc::channel` for the connection, insert sender into `user_state.routes`
6. Send `ConnectResult { error: None }`
7. Return `(link_name, outgoing_rx)` for use in `connection_loop`

On link name collision, the server responds with `ConnectResult { error: Some(LinkNameTaken) }`. The client retries with a new random suffix (up to 5 attempts).

After handshake, `accept_connection()` splits the transport into reader/writer halves, spawns the reader and writer tasks, runs `connection_loop()` until disconnection, then removes the route from `user_state.routes`.

### Connecting to Peers (Client-Side)

`connect_handshake()` sends `Connect { link_name, token: None, version: PROTOCOL_VERSION }` and waits for `ConnectResult`. On `LinkNameTaken`, it regenerates the link name and retries (up to 5 attempts).

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

WebSocket handles framing natively; `WebSocketTransport` reads/writes binary MessagePack frames directly.

See `src/transport/framing.rs`.

---

## Input Forwarding

Two input paths, both routable:

- **`RawInput`** - Raw keystroke bytes from terminals, delivered directly to PTY stdin
- **`StructuredInput`** - Agent-type-keyed input from rich clients. For Claude, this includes:
  - `ClaudeStructuredInput::SubmitMessage` - Text input from rich clients. Adds a 20ms delay then appends carriage return (`\r`)
  - `ClaudeStructuredInput::PermissionResponse` - User's response to a permission request. Translated to a keystroke and written to PTY

Input messages are forwarded via generic `handle_routable` routing. When delivered locally, the agent is resolved by UUID or name and `send_input()` is called.

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
│  │ Reads MultiplexByteBuffer/MultiplexStructuredBuffer → sends RawOutput/       │  │
│  │ StructuredOutput routable messages to subscriber's channel        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  Per Local Agent:                                                      │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐ │
│  │ PTY Reader       │  │ Input Forwarder  │  │ Child Waiter         │ │
│  │ (spawn_blocking) │  │ (spawn)          │  │ (spawn_blocking)     │ │
│  │                  │  │                  │  │                      │ │
│  │ PTY stdout →     │  │ input_rx →       │  │ Waits for exit →    │ │
│  │ MultiplexByteBuffer  │  │ PTY stdin        │  │ SessionEvent::Ended  │ │
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
│  │ Watches JSONL file → parses → writes to MultiplexStructuredBuffer        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
└────────────────────────────────────────────────────────────────────────┘
```

**Data flow for local terminal subscription:**
```
Terminal ──SubscribeRaw──> Connection Handler
                                │
                                ▼
                         handle_routable → local delivery
                                │
                                ├─> agent.subscribe() → MultiplexByteReader
                                ├─> Send SubscribeRawResult
                                └─> Spawn output stream task
                                      │
                                      └─> MultiplexByteReader.read() → RawOutput msg → subscriber channel

Terminal ──RawInput──> Connection Handler → agent.send_input() → PTY stdin
```

**Data flow for proxied subscription:**
```
App ──SubscribeRaw──> Cloud Handler ──SubscribeRaw──> Local Handler
                           │                              │
                      (pops dst,                     (dst empty,
                       pushes src,                    local delivery)
                       forwards)                          │
                                                     Subscribe agent
                                                     Spawn output task

Local PTY → MultiplexByteBuffer → Output Stream Task → RawOutput msg
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
3. Sends `Command::HandleHook { hook }` message
4. Server handles: for `SessionStart`, links the transcript file to the agent session
5. For `PermissionRequest`, writes to the agent's log buffer for structured subscribers

### Permission Request Flow

```
Claude Code → amux hooks → HookEvent(PermissionRequest) → server
    → agent.write_log(StructuredOutput::Claude(PermissionRequest)) → MultiplexStructuredBuffer
    → rich client (via StructuredOutput) → user sees permission UI
    → StructuredInput(Claude(PermissionResponse)) (routable) → server → writes keystroke to PTY
```

See `src/hooks.rs`, `src/message.rs`.

---

## Key Design Decisions

### 1. Unified connection_loop for all transports

Rather than separate `LocalConnection` and `RemoteConnection` types with different behavior, all connections use the same `connection_loop`. The `Transport` trait abstracts away the underlying stream. This eliminates code duplication and ensures protocol consistency across transports.

### 2. MultiplexByteBuffer atomic subscribe

The original design had a race condition window between getting the replay buffer and starting to receive live data. `MultiplexByteBuffer` solves this by holding a lock during both the snapshot and subscriber registration, ensuring zero gaps or duplicates. This is the core correctness guarantee for late-joining terminals.

### 3. MessagePack serialization

MessagePack (rmp-serde with named/map format) replaced bincode for all transports. Named format provides forward/backward compatibility when fields are added, unlike bincode's positional encoding. Binary frames handle byte blobs cleanly without base64 encoding.

### 4. Handshake-based connection establishment

Connections start with standalone `Connect` / `ConnectResult` handshake frames (outside the `Message` enum). This:
- Assigns link names at connect time (used for routing)
- Carries JWT tokens for cloud authentication
- Supports link name collision retry

### 5. Implicit subscriptions via spawned tasks

Instead of a centralized `subscriptions: HashMap<AgentId, Vec<ConnectionId>>`, subscriptions are implicit in spawned output-streaming tasks. When a client subscribes, a task is spawned that reads from `MultiplexByteBuffer` and sends to the subscriber's channel. The subscription dies when either the buffer closes or the subscriber disconnects.

### 6. Routable/Direct/Command message split

The `Message` enum has three variants that encode routing capability and trust level in the type system:
- `Routable { src, dst, request_id, payload }` - Can be forwarded across hops. Payload is opaque `Vec<u8>` that intermediate servers copy verbatim. Generic forwarding logic handles all routable message types uniformly.
- `Direct(message)` - Peer-to-peer session messages handled by the directly connected server. Cannot be forwarded. Used for discovery and in-session control (for example, `Reauth`).
- `Command(command)` - CLI-only messages from local Unix socket clients. Rejected if received over TCP or WebSocket. Prevents remote peers from sending privileged commands (Shutdown, Debug, etc.).

This collapses forwarding into one generic path in `handle_routable` and provides security boundaries via the Command/Direct split.

### 7. Stack-based routing

Routes are stacks (VecDeque) of link names rather than flat `Route::Remote { via }` entries. At each hop, the next link is popped from `dst` and pushed to `src`. This naturally:
- Builds the return path as the message travels
- Supports multi-hop forwarding without each intermediate server needing full topology knowledge
- Enables reply routing by simply swapping src/dst

---

## Proxying / Multi-hop Subscriptions

When a client subscribes to an agent on a remote server, messages are forwarded through intermediate servers:

```
App ──SubscribeRaw──> Cloud Server ──SubscribeRaw──> Local Server
                           │                              │
                           │                         (owns agent)
                           │                              │
App <──RawOutput──────── Cloud <────RawOutput──────────── ┘ (ongoing)
App ──RawInput─────────> Cloud ────RawInput──────────────>│
```

All forwarding is handled by generic `handle_routable`:
1. Pop next hop from `dst`
2. Push it to `src`
3. Look up in routes table
4. Send through channel

The same code forwards all routable messages by forwarding the opaque `payload` verbatim. No message-type-specific forwarding logic needed — intermediate hops never deserialize the payload.

---

## What's NOT Here (intentionally deferred)

- **Multi-user access** - Cross-user collaboration (user A accessing user B's agents)
- **Local network discovery** - Automatic discovery of amux servers on LAN
- **Reconnection logic** - Client-side reconnect after disconnect
- **Rate limiting** - Beyond token quotas

---
