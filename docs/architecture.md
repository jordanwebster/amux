# Architecture

A detailed design for the amux server internals covering data structures, message flow, routing, and the task model.

> **Protocol note:** This document still contains historical pre-protobuf
> protocol sections, including MessagePack and `Routable`/`Direct`/`Command`
> examples. The current wire protocol is defined by
> `crates/amux/proto/amux/v1/amux.proto`, generated via `crates/amux/build.rs`,
> and summarized in `notes/PROTO_REFACTOR.md`. Treat the old wire-shape sections
> below as design history until this architecture document is fully rewritten.

## Quick Overview

**What is amux?** A multiplexer for AI agent sessions (Claude, Codex, etc.) that enables:
- Multiple terminals attaching to the same agent
- Remote access via cloud relay
- Rich clients (mobile/web) receiving structured logs via WebSocket

**Core concepts:**
- **Server** - manages connections, agents, and routing
- **Connection** - Unix socket (or Windows named pipe), TCP, or WebSocket; all use the same framed message protocol
- **AgentSession** - a running agent; wraps a provider-specific session (e.g. `ClaudeSession`) with its PTY, replay buffer, and structured-log pipeline
- **Routing Table** - maps links to per-connection handles for forwarding messages

**Key design choices:**
- All connections (local and remote) use framed messages with length-prefixed encoding (or native WebSocket binary frames)
- Connections are identified by validated `Link`s (e.g. `"term-abc1"`, `"myhost-xyz2"`)
- Messages are `Routable` (carry src/dst routes + opaque payload, forwarded across hops), `Direct` (peer-to-peer, handled by directly connected server), or `Command` (CLI-only, rejected from remote peers)
- Serialization uses MessagePack (named / map format) for all transports (Unix, TCP, WebSocket)
- Subscriptions are explicit and lease-renewed; Structured I/O payloads are opaque `serde_json::Value`

---

## Glossary

| Term | Description |
|------|-------------|
| **agent_id** | UUID identifying an agent session. Optional human-readable name can be set via `--name` flag. |
| **Link** | Validated connection name (newtype over `String`, rejects `.`). Used as keys in the routing table. |
| **Route** | A stack of `Link`s (`VecDeque<Link>`) representing a multi-hop path. Serializes as `"AB.BC.CD"` (dot-separated). |
| **RoutableMessage** | A message that carries `src` and `dst` routes and can be forwarded across hops. |
| **DirectMessage** | A message handled only by the directly connected server (no routing). |
| **SubscriptionId** | Transparent UUID newtype identifying one subscribe call. Lease-renewed by the client; owns the output-stream task. |
| **host_id** | Stable UUID for an amux server instance, persisted in `state.yaml` and propagated via `AnnounceHost`. |
| **PTY** | Pseudo-terminal — the interface used to run interactive CLI agents like Claude. |

---

## Core Identity Types

```rust
// Agents are identified by UUID, with an optional human-readable name
agent_id: Uuid           // e.g. 550e8400-e29b-41d4-a716-446655440000
name: Option<String>     // e.g. "my-session" (set via --name flag)

// Connections are identified by validated link names
struct Link(String);     // e.g. "term-abc1", "myhost-xyz2" (rejects names containing '.')
```

`Link` is a newtype wrapper that rejects names containing `.` (the route
separator) at construction, so downstream code can treat a `Link` as a
well-formed name without re-validating. Handshake frames still carry the raw
`String` so the server can respond with `InvalidLinkName` rather than drop the
connection.

### Route

A route is a stack of links representing a path through the network. The top of the stack (front of deque) is the next hop.

```rust
/// Serializes as "AB.BC.CD" where AB is the first hop.
struct Route {
    links: VecDeque<Link>,
}

impl Route {
    fn empty() -> Self;
    fn is_empty(&self) -> bool;
    fn len(&self) -> usize;
    fn from_link(link: Link) -> Self;            // Single-hop route
    fn push(&mut self, link: Link);              // Push link to front (new next hop)
    fn pop(&mut self) -> Option<Link>;           // Pop next hop
    fn peek(&self) -> Option<&Link>;             // Peek without consuming
    fn contains_link(&self, link: &str) -> bool;
    fn starts_with_route(&self, prefix: &Route) -> bool;
    fn replace_prefix(&mut self, old: &Route, new: &Route) -> bool;

    /// Prepare to send: pops from dst, creates src from the popped link.
    /// Returns (src, dst) for the message.
    fn send(dst: Route) -> Option<(Route, Route)>;

    /// Prepare a reply: literally just send(src). The src route accumulated
    /// hop links on the way in, so sending through it reverses the path.
    fn reply(src: Route) -> Option<(Route, Route)>;
}
```

Link names are generated with random suffixes for uniqueness:
- Terminal connections: `"term-{rand}"` (4 lowercase alphanumeric chars)
- Server connections: `"{hostname}-{rand}"` or just `"{hostname}"` if `randomise_link_name` is false (periods in the hostname are replaced with hyphens)

Hook invocations do not open their own connections; the client-side hook
handler reuses the existing CLI Unix-socket connection (see
[Hooks System](#hooks-system)).

See `crates/amux/src/protocol/route.rs`, `crates/amux/src/protocol/link.rs`.

---

## Transport Abstraction

Each transport exposes raw frame I/O plus the same message-level read/write,
with split support for the reader/writer task architecture:

```rust
trait Transport: Send + Sync {
    fn read_frame(&mut self)  -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn write_frame(&mut self, data: &[u8]) -> impl Future<Output = Result<()>> + Send;
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

`read_frame`/`write_frame` are the low-level escape hatch the handshake uses
(before there is a valid `Message` to exchange). Once the handshake succeeds,
the reader/writer tasks only ever call the message-level methods.

Implementations:

| Transport | Stream | Serialization | Framing | Flush |
|-----------|--------|---------------|---------|-------|
| `LocalTransport` | Unix socket (unix) or named pipe (windows), split into read/write halves | MessagePack | Length-prefixed | No |
| `TcpTransport<S>` | Generic over `AsyncRead + AsyncWrite` (plain TCP or TLS) | MessagePack | Length-prefixed | Yes |
| `WebSocketTransport` | `WebSocketStream<TcpStream>` | MessagePack | WebSocket native (binary frames) | N/A |

`TcpTransport` is generic over the stream type, allowing it to wrap both plain TCP and TLS streams (e.g. `TcpTransport<ClientTlsStream<TcpStream>>`).

See `crates/amux/src/transport.rs` and `crates/amux/src/transport/`.

---

## Message Types

The protocol uses a tagged top-level enum (serde tag `kind`, snake_case):

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
    Direct  { message: DirectMessage },
    /// CLI-only commands. MUST be rejected if received over TCP or WebSocket.
    Command { command: Command },
    /// Forward-compatibility fallback: any unknown `kind` tag decodes here.
    #[serde(other)]
    Unknown,
}
```

`Message::routable(src, dst, request_id, &routable_message)` encodes the
`RoutableMessage` into the opaque payload. It panics on encode failure; use
`Message::try_routable` at sites that carry user-supplied `serde_json::Value`
payloads (e.g. `StructuredInput`/`StructuredOutput`).

### RoutableMessage

Messages that carry routing information and can be forwarded across server hops. Deserialized from `payload` at the final destination only. Serde tag `type`, snake_case:

```rust
enum RoutableMessage {
    // Subscribing to agent output (lease-based; see Subscriptions)
    SubscribeRaw         { agent_id: Uuid, terminal_size: Option<TerminalSize> },
    SubscribeRawResult   { subscription_id: SubscriptionId, lease_ms: u64, error: Option<ProtocolError> },
    SubscribeStructured  { agent_id: Uuid, query: Option<SubscribeQuery> },
    SubscribeStructuredResult {
        subscription_id: SubscriptionId,
        seq: u64,                               // Current seq at subscribe time
        structured_protocol: Option<String>,    // Agent-declared protocol tag, if any
        lease_ms: u64,
        error: Option<ProtocolError>,
    },
    ExtendSubscription       { subscription_id: SubscriptionId },
    ExtendSubscriptionResult { subscription_id: SubscriptionId, lease_ms: u64, error: Option<ProtocolError> },
    Unsubscribe              { subscription_id: SubscriptionId },

    // Agent lifecycle
    CreateAgent(CreateAgentRequest),
    CreateAgentResult   { agent_id: Uuid, error: Option<ProtocolError> },
    RenameAgent(RenameAgentRequest),
    RenameAgentResult   { agent_id: Uuid, error: Option<ProtocolError> },
    DeleteAgent         { agent_id: Uuid },
    DeleteAgentResult   { agent_id: Uuid, error: Option<ProtocolError> },

    // Input
    RawInput            { agent_id: Uuid, data: Vec<u8> },
    StructuredInput     { agent_id: Uuid, seq: u64, payload: serde_json::Value },
    StructuredInputResult { agent_id: Uuid, error: Option<ProtocolError> },

    // Output
    RawOutput           { subscription_id: SubscriptionId, data: Vec<u8> },
    StructuredOutput    { subscription_id: SubscriptionId, seq: u64, payload: serde_json::Value },

    // Subscription lifecycle
    SubscriptionClosed  { subscription_id: SubscriptionId, reason: SubscriptionCloseReason },

    /// Sent by an intermediate hop when it cannot forward a message. The
    /// original sender matches on `request_id` to fail the pending request.
    Unreachable { request_id: u64 },

    UnsupportedMessage,   // Parsed payload with a known-but-unsupported tag
    InvalidMessage,       // Malformed / undecodable payload bytes
    #[serde(other)]
    Unknown,              // Forward-compat fallback
}

enum SubscribeQuery {
    Since { seq: u64 },      // Replay entries with `seq >= seq`
    Tail  { count: u64 },    // Replay only the last `count` entries
}

enum SubscriptionCloseReason {
    SourceClosed,            // The underlying buffer / agent ended
    Unsubscribed,            // The client sent Unsubscribe
    LeaseExpired,            // No ExtendSubscription arrived before the lease
}

struct SubscriptionId(Uuid);    // Transparent newtype; identifies one subscribe call
```

Structured I/O payloads are opaque `serde_json::Value`. Agent-specific schemas
(e.g. Claude's `UserMessage` / `AssistantMessage` / `PermissionRequest` /
permission-tool inputs, or the Claude `PermissionResponse` / `SubmitMessage`
reply shapes) live on each side of the wire and are exchanged as JSON. The
`structured_protocol` field on `SubscribeStructuredResult` identifies the
schema (e.g. `"claude_pty_v1"`) so clients can version their parsers.

`RoutableMessage` has its own `encode()`/`decode()` methods for the two-step serialization used with opaque payloads.

### DirectMessage

Peer-to-peer session messages between directly connected servers (no routing):

```rust
enum DirectMessage {
    // In-session authentication refresh (cloud links)
    Reauth { token: String },
    ReauthResult { error: Option<ProtocolError> },

    // Heartbeats (negotiated idle timeout; see Heartbeats)
    Heartbeat,
    HeartbeatAck,

    /// Marks the end of the initial host/agent discovery snapshot for a
    /// connection. Lets a peer know it has seen the full existing state and
    /// can distinguish new announcements from replayed ones.
    InitialSyncComplete,

    // Agent discovery (pure registry, no routing side effects).
    AnnounceAgent {
        agent_id: Uuid,
        host_id: Uuid,                    // Stable ID of the host that owns the agent
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        agent_type: String,
        structured_protocol: Option<String>,
        readonly: bool,                   // True for externally-started (transcript-only) sessions
        args: Vec<String>,                // Extra args passed to the agent command
        created_at: DateTime<Utc>,
    },
    WithdrawAgent { agent_id: Uuid },

    // Host/route management (single source of routing truth)
    AnnounceHost { id: Uuid, name: String, route: Route, version: String },
    WithdrawHost { id: Uuid, route: Route },

    #[serde(other)]
    Unknown,
}
```

`AnnounceAgent` does not itself carry a route — routes are built up implicitly
as the message traverses peer links (each hop prepends its own link to the
advertised `host_id`'s known route via `AnnounceHost`).

Heartbeats are governed by a per-connection **idle timeout** negotiated during
the handshake. The acceptor publishes `idle_timeout_secs` in `ConnectResult`
(pulled from server config, default 180s; `None` for local Unix sockets).
After the handshake, both peers apply the same rule symmetrically: if no
inbound message has been seen for `idle_timeout` seconds, the connection is
closed and the normal `WithdrawHost` cleanup path runs.

Only the dialer initiates heartbeats — it sends `Heartbeat` whenever its own
outbound link has been idle, at its own cadence (currently `idle_timeout / 3`).
The cadence is not part of the wire protocol; the dialer is free to pick any
rate as long as something is sent within the idle window. The acceptor replies
to each `Heartbeat` with `HeartbeatAck`, which counts as inbound traffic and
resets the dialer's kill deadline.

### Handshake

Connection bootstrap is a separate wire protocol (not part of `Message`):

```rust
const PROTOCOL_VERSION: u32 = 1;

struct Connect {
    link_name: String,                  // Wire-typed as String so malformed names
                                        // reach the server and get InvalidLinkName
    token: Option<String>,              // JWT for cloud; None for local/LAN
    version: u32,                       // Must equal PROTOCOL_VERSION
    client_name: Option<String>,        // e.g. "amux-cli", "amux-app-ios"
    client_version: Option<String>,     // Semver; checked against minimum_client_versions
}

struct ConnectResult {
    error: Option<ProtocolError>,
    // None disables heartbeats (Unix); Some(t) negotiates a t-second idle timeout.
    idle_timeout_secs: Option<u32>,
}
```

Handshake frames use MessagePack map encoding and are exchanged (as raw
frames, via `read_frame`/`write_frame`) before the connection enters the
normal session message loop.

### Command

CLI-only messages sent over Unix socket. Servers reject these if received over TCP or WebSocket:

```rust
enum Command {
    ListAgents,
    ListAgentsResult   { agents: Vec<Agent> },
    ResolveAgent       { identifier: String },
    ResolveAgentResult { agent: Option<Agent> },

    Shutdown,
    ShutdownNotification { reason: ShutdownReason },

    Debug              { verbose: bool, format: DebugFormat }, // Yaml | Json
    DebugResult        { dump: String },

    ConnectToServer       { address: String },
    ConnectToServerResult { error: Option<ProtocolError> },

    /// Deliver an agent-provider hook payload to a specific agent.
    /// `payload` is opaque bytes (e.g. Claude Code's hook JSON) that the
    /// receiving session parses based on `provider`.
    HandleHook {
        agent_id: Uuid,
        provider: HookProvider,     // Claude | Unknown
        payload: Vec<u8>,
        external: bool,             // True for hooks from externally-started sessions
    },
    HandleHookResult { error: Option<ProtocolError> },

    // Suspend / resume owned agents (persist / restore across server restarts)
    Suspend,
    SuspendResult { suspended_count: u64, error: Option<ProtocolError> },
    Resume,
    ResumeResult  { resumed_count: u64, failed_count: u64, error: Option<ProtocolError> },

    #[serde(other)]
    Unknown,
}

enum ShutdownReason {
    ProtocolMismatch,  // Server received version mismatch from cloud
    UserRequested,     // User ran `amux shutdown`
    Updating,          // Server is restarting to apply an update
}
```

### Supporting Types

```rust
enum ProtocolError {
    // Routable-delivery failures
    NoAgentFound,
    UnknownSubscription,
    UnsupportedSubscribeQuery,
    SequenceNumberMismatch { client_seq: u64, current_seq: u64 },

    // Generic / handshake
    ServerError { message: String },
    LinkNameTaken,
    InvalidCredentials,
    InvalidLinkName,
    ProtocolMismatch { server_version: u32, client_version: u32 },
    UpgradeRequired  { minimum_version: String, client_version: String },

    #[serde(other)]
    Unknown,
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

See `crates/amux/src/protocol/message/` (`envelope.rs`, `routable.rs`, `direct.rs`, `command.rs`, `common.rs`).

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
    state:       Arc<RwLock<ServerState>>,      // Global server state
    user_state:  Arc<RwLock<ServerUserState>>,  // Per-user state (agents, routes, registry)
    user_id:     Uuid,                          // Authenticated user ID (LOCAL_USER_ID for Unix)
    event_tx:    mpsc::Sender<SessionEvent>,    // Channel to notify server of session events
    link:        Link,                          // This connection's link name
    is_local:    bool,                          // True for Unix socket connections
    heartbeat:   Option<HeartbeatSetup>,        // Negotiated idle-timeout/role; None disables heartbeats
    next_request_id: Arc<AtomicU64>,            // Per-connection counter for outgoing messages
    client_name:    Option<String>,             // From Connect handshake
    client_version: Option<String>,             // From Connect handshake
}

struct HeartbeatSetup {
    role: HeartbeatRole,            // Dialer | Acceptor
    idle_timeout: Duration,
}
```

Each connection has a `ConnectionHandle` stored in the routes table, bundling
an `mpsc::Sender<Message>` with the `Arc<AtomicU64>` request-id counter. Other
tasks send messages to a connection by looking up its `Link` in the routes
table and sending through the handle.

For cloud connections, the loop extends with token refresh support: a `select!` branch fires when the JWT token is nearing expiry, triggering in-band re-authentication via `DirectMessage::Reauth`.

See `crates/amux/src/server/connection/` (`context.rs`, `driver.rs`, `heartbeat.rs`, `reauth.rs`, `subscription.rs`).

---

## Server

```rust
/// Global server state
struct ServerState {
    config: Config,
    host_id: Uuid,                                       // Stable ID persisted in state.yaml
    is_cloud_server: bool,
    jwt_validator: Option<Arc<JwtValidator>>,
    users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,  // Per-user isolation
    shutdown_tx: mpsc::Sender<ShutdownRequest>,          // Shutdown / Suspend requests from handlers
}

/// Per-user state. Each authenticated user gets isolated agents, routes,
/// registry, peer links, hosts, and subscriptions. LOCAL_USER_ID
/// (`Uuid::nil()`) is used for non-authenticated Unix-socket connections.
/// User isolation is enforced on cloud servers via JWT authentication.
struct ServerUserState {
    agents:     HashMap<Uuid, AgentSession>,            // Local sessions only
    routes:     HashMap<Link, ConnectionHandle>,         // One entry per live connection
    registry:   AgentRegistry,                           // Local + remote agents, name mapping
    peer_links: HashSet<Link>,                           // Links that receive announcements
    hosts:      HashMap<Uuid, Host>,                     // Known remote hosts by host_id
    active_subscriptions: HashMap<SubscriptionId, SubscriptionEntry>,
}

struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    next_request_id: Arc<AtomicU64>,
}

struct SubscriptionEntry {
    subscription_id: SubscriptionId,
    agent_id: Uuid,
    mode: SubscriptionMode,         // Raw | Structured
    cancel: oneshot::Sender<()>,    // Dropping cancels the stream task
    dst: Route,                     // Reply route (= original src reversed)
    lease_deadline: Instant,        // Extended via ExtendSubscription
}
```

The server's `run()` method:
1. Binds the local listener (Unix socket or named pipe), TCP, and WebSocket listeners
2. Optionally sets up TLS (cloud mode, using `AMUX_TLS_CERT`/`AMUX_TLS_KEY` env vars)
3. Optionally establishes cloud connection (local mode with cloud enabled)
4. Enters main `select!` loop handling: listener accepts, session events, shutdown signal

**Routes table:** `HashMap<Link, ConnectionHandle>` keyed by link name, per-user. When a connection disconnects, its route is removed and its request-id counter goes with it.

**Subscriptions:** Explicit, lease-based. Each successful `SubscribeRaw` /
`SubscribeStructured` inserts a `SubscriptionEntry` into
`active_subscriptions`, keyed by a fresh `SubscriptionId`. A paired output
stream task reads from the agent's buffer and sends `RawOutput` /
`StructuredOutput` messages back along the stored `dst` route. Clients keep
the subscription alive with `ExtendSubscription`; if the lease expires (default
`SUBSCRIPTION_LEASE_DURATION = 300s`), the server sends `SubscriptionClosed {
reason: LeaseExpired }` and drops the entry. `Unsubscribe` and host withdrawal
also tear down the entry; in all cases the `cancel` oneshot signals the stream
task to stop.

**Agent Registry:** `AgentRegistry` provides centralized tracking of both local and remote agents with bidirectional name-to-UUID mapping. Used for `ListAgents`, `ResolveAgent`, and agent discovery propagation.

**Hosts:** `HashMap<Uuid, Host>` tracks known remote hosts announced via `AnnounceHost`. When a host is withdrawn, all agents reachable via that host's route are bulk-removed from the registry and all matching subscriptions are cancelled.

See `crates/amux/src/server/` (`state.rs`, `runtime.rs`, `accept.rs`, `dispatch/`, `routing/`).

---

## Agent Session

`AgentSession` is the top-level enum the server holds for each locally-owned
agent. Each variant wraps a concrete, provider-specific session type; PTY I/O
is encapsulated in a shared `PtyHandle` set up by `spawn_pty_agent`.

```rust
enum AgentSession {
    Claude(ClaudeSession),
    #[cfg(any(debug_assertions, test))]
    TestAgent(TestAgentSession),
}

struct ClaudeSession {
    agent_id:      Uuid,
    name:          Option<String>,
    command:       String,                         // Always "claude"
    working_dir:   PathBuf,
    pty:           Option<PtyHandle>,              // None until start()
    log_source:    Option<StructuredLogSource>,    // Structured entry pipeline
    terminal_size: Option<TerminalSize>,
    session_id:    Option<Uuid>,                   // Claude session ID (--resume / SessionStart)
    readonly:      bool,                           // True for externally-started (transcript-only)
    args:          Vec<String>,                    // Extra args for the claude command
    name_source:   LocalAgentNameSource,
    name_sniffer_abort: Option<AbortHandle>,
    created_at:    DateTime<Utc>,
}

struct PtyHandle {
    // Owns the PTY master, the input channel, and the reader/writer/exit tasks.
    // Exposes: subscribe() -> (MultiplexByteReader, mpsc::Sender<Vec<u8>>),
    //          resize(rows, cols), stop(StopPolicy), ...
}
```

`AgentSession::try_new(req)` dispatches on `req.agent_type` to construct the
right variant; `ClaudeSession::start()` is the second phase that actually
spawns the `claude` process. Hook delivery (`HandleHook`) is dispatched to the
session, which parses the opaque payload into a provider-specific `ClaudeHook`
and either links a transcript or appends a structured entry via
`StructuredLogSource`.

### CreateAgentRequest

```rust
struct CreateAgentRequest {
    agent_id:      Uuid,
    name:          Option<String>,
    agent_type:    AgentType,
    working_dir:   PathBuf,
    terminal_size: Option<TerminalSize>,   // None means use defaults
    args:          Vec<String>,            // Extra args (e.g. --fork-session, --resume <id>)
}

struct TerminalSize { rows: u16, cols: u16 }   // Defaults to 24×80

enum AgentType {
    Claude,                                // Spawns `claude` with --session-id / --resume
    #[cfg(any(debug_assertions, test))]
    TestAgent { command: String },         // Dev/test only
    #[serde(other)]
    Unknown,
}
```

See `crates/amux/src/agent/` (`session.rs`, `pty.rs`, `claude/session/*.rs`, `test_agent.rs`).

---

## Config

```rust
struct Config {
    host_name: String,                  // Hostname for generating link names (default: system hostname)
    cloud_url: String,                  // Cloud API URL (default: "https://amux.sh")
    socket_path: PathBuf,               // Unix socket / named pipe path (default: per-user runtime dir)
    tcp_port: Option<u16>,              // TCP port for server-to-server (None = don't listen)
    websocket_port: Option<u16>,        // WebSocket port for rich clients (None = don't listen)
    randomise_link_name: bool,          // Add random suffix to link names (default: true; test-only override)
    state_path: PathBuf,                // Path to persistent state file
    enforce_tls_in_cloud_mode: bool,    // Whether cloud server handles TLS itself (default: true)

    enable_cloud_mode: Option<bool>,    // User preference; None until `amux init` prompts
    prevent_idle_sleep: Option<bool>,   // User preference; None until `amux init` prompts
    minimum_client_versions: HashMap<String, String>,  // e.g. {"amux-cli": "0.2.0"}
    idle_timeout_secs: u32,             // Heartbeat idle timeout (default: 180)
    keybinds: Keybinds,                 // Leader key etc.
    path: Option<PathBuf>,              // Source file (runtime-only)
}
```

Cloud servers must have both `tcp_port` and `websocket_port` set; `validate(is_cloud: bool)` enforces this.

Config is loaded from a YAML file at `$XDG_CONFIG_HOME/amux/config.yaml`
(falling back to `~/.config/amux/config.yaml`) or via `--config` flag. All
fields have serde defaults.

See `crates/amux/src/config.rs`.

---

## Structured I/O

`StructuredInput` and `StructuredOutput` on the wire carry an opaque
`serde_json::Value` plus an `agent_id`/`subscription_id` and a `seq` number.
The protocol deliberately does not know about agent-specific message shapes:
the sending side serializes them as JSON and the receiving side parses them
based on the `structured_protocol` identifier advertised in
`SubscribeStructuredResult` (e.g. `"claude_pty_v1"`).

For Claude, the current payload shapes include:

- Output entries such as `UserMessage`, `AssistantMessage`, and
  `PermissionRequest` (with a tool-specific `tool_input`, e.g. `Edit`,
  `Bash`, `WebFetch`, `Skill`, `ExitPlanMode`, …).
- Input entries such as `PermissionResponse` (the user's reply to a
  permission request, translated to a keystroke) and `SubmitMessage` (rich
  client text input, delivered to the PTY with a short delay and a trailing
  carriage return).

On the agent side, a `TranscriptTailer` watches Claude Code's transcript
JSONL file and pushes parsed entries into a `StructuredLogSource`, which
drives the `MultiplexStructuredBuffer` that feeds active structured
subscriptions. Permission-request hook events are appended to the same log
source when received via `HandleHook`.

See `crates/amux/src/agent/claude/transcript.rs`, `crates/amux/src/agent/claude/session/*.rs`.

---

## BroadcastBuffer

The core abstraction for agent output replay and broadcast, parameterized by a `BufferPolicy` that controls storage, truncation, and replay semantics. Supports multiple concurrent readers with atomic subscribe (no data loss or duplication between replay and live output).

Two concrete instantiations via type aliases:
- `MultiplexByteBuffer` (`BroadcastBuffer<BytePolicy>`) — contiguous byte stream for PTY output
- `MultiplexStructuredBuffer` (`BroadcastBuffer<StructuredPolicy>`) — discrete structured entries for Claude I/O

```rust
struct BroadcastBuffer<P: BufferPolicy> {
    inner: Arc<BroadcastInner<P>>,
}

struct BroadcastInner<P: BufferPolicy> {
    storage: RwLock<P::Storage>,
    subscribers: RwLock<Vec<mpsc::Sender<P::Item>>>,   // Bounded channels
    capacity: usize,
    closed: RwLock<bool>,
}

impl<P: BufferPolicy> BroadcastBuffer<P> {
    /// Publish an input into the buffer and broadcast the resulting item.
    /// Holds the storage write lock during both publication and broadcast,
    /// ensuring atomicity with subscribe(). Slow / dead subscribers are
    /// cleaned up (bounded backpressure).
    async fn write(&self, input: P::Input) -> Option<P::Item>;

    /// Subscribe: returns a reader that receives all existing items
    /// then live updates. Holds the read lock during subscribe for atomicity
    /// with write(). Returns None if the buffer is closed.
    async fn subscribe(&self) -> Option<BroadcastReader<P>>;

    /// Close: drops all subscriber channels, prevents new subscriptions.
    async fn close(&self);
}
```

The key invariant: `write()` and `subscribe()` are mutually exclusive via the storage lock. This ensures a new subscriber sees exactly all items written before it subscribed, with no gaps and no duplicates in the transition to live data.

See `crates/amux/src/buffer.rs`.

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
   - On parsed-but-unknown routable tag: send `RoutableMessage::UnsupportedMessage`
   - On malformed / undecodable payload bytes: send `RoutableMessage::InvalidMessage`

### Reply Routing

`Route::reply(src)` is literally `Route::send(src)` — the same pop-and-push operation applied to the incoming `src` route instead of `dst`. This works because `src` accumulated each hop's link name as the message traveled inward, so "sending" through `src` naturally reverses the path. This symmetry is the entire routing algorithm: there is no separate reply mechanism.

### Routing Failure Handling

When a routable message can't be forwarded (next hop not in routes table or
channel closed), an intermediate hop sends a `RoutableMessage::Unreachable {
request_id }` back along the accumulated src route so the original sender can
fail the pending request. Route cleanup itself flows from `WithdrawHost`,
which remains the single source of routing truth.

### Route Lifecycle

`AnnounceHost`/`WithdrawHost` are the single source of routing truth:

- **Connection loss:** Server identifies hosts reachable via the dead link, broadcasts `WithdrawHost` per host. Upon receiving `WithdrawHost`, each server removes the host, bulk-removes agents whose route passes through that host, cancels matching subscriptions, and propagates the withdrawal.
- **Agent death:** Server sends `SubscriptionClosed { reason: SourceClosed }` to matching subscribers, sends `WithdrawAgent` per peer (discovery cleanup only), removes the agent from the registry. No route changes.
- `AnnounceAgent`/`WithdrawAgent` are pure discovery — they update the agent registry for `ListAgents`/`ResolveAgent` but have zero routing side effects.

See `crates/amux/src/protocol/route.rs`, `crates/amux/src/server/dispatch/`, `crates/amux/src/server/routing/`.

---

## Agent Lifecycle

### Spawning

`AgentSession::try_new(req)` constructs the right variant; `ClaudeSession::start()` (or `TestAgentSession::start()`) actually spawns the process. Both delegate to `spawn_pty_agent`, which creates the PTY and spawns three background tasks:

```
Task 1: PTY Reader (spawn_blocking)
  - Reads PTY stdout in a blocking loop
  - Writes bytes to the MultiplexByteBuffer owned by PtyHandle (broadcasts to all subscribers)

Task 2: Input Forwarder (spawn)
  - Reads from the input_rx channel
  - Writes to PTY stdin

Task 3: Exit Monitor (spawn_blocking)
  - Waits for child process to exit
  - Drops PTY master
  - Closes the MultiplexByteBuffer (disconnects all subscribers)
  - Sends SessionEvent::Ended to the server
```

The agent type determines the command and arguments:
- `AgentType::Claude` → runs `claude` with `--session-id={agent_id}` (or `--resume <id>` when resuming); any extra `args` are appended.
- `AgentType::TestAgent { command }` → runs the given command (dev/test only)

### Termination

1. Child process exits
2. Exit monitor detects exit, drops PTY master, closes buffers
3. `SessionEvent::Ended { agent_id, user_id }` sent to the server via the event channel
4. Server removes the agent from the registry and broadcasts `WithdrawAgent` to peers
5. Output streaming tasks detect buffer closure and send `RoutableMessage::SubscriptionClosed { reason: SourceClosed }` to their subscribers

See `crates/amux/src/agent/session.rs`, `crates/amux/src/agent/pty.rs`.

---

## Connection Lifecycle

### Accepting Connections (Server-Side)

`accept_handshake()` handles the initial handshake:

1. Read first frame, decode `Connect { link_name, token, version, client_name?, client_version? }`
2. Check protocol version (`version` must match `PROTOCOL_VERSION`), replying with `ProtocolMismatch` otherwise
3. If `client_name` matches an entry in `minimum_client_versions`, check `client_version`, replying with `UpgradeRequired` if below
4. If cloud mode: validate JWT token via JWKS, determine `user_id` from claims
5. Reserve the link via `user_state.try_reserve_link(link)` — this validates the name, checks uniqueness, and atomically inserts the `ConnectionHandle` into the routes table (returns the receive half of the outgoing channel)
6. Send `ConnectResult { error: None, idle_timeout_secs: <negotiated> }`
7. Return `(link, outgoing_rx)` for use in `connection_loop`

On link name collision, the server responds with `ConnectResult { error: Some(LinkNameTaken) }`. The client retries with a new random suffix (up to 5 attempts).

After handshake, the driver splits the transport into reader/writer halves, spawns the reader and writer tasks, runs `connection_loop()` until disconnection, then removes the route from `user_state.routes`.

### Connecting to Peers (Client-Side)

`connect_handshake()` sends `Connect { link_name, token, version: PROTOCOL_VERSION, client_name, client_version }` and waits for `ConnectResult`. On `LinkNameTaken`, it regenerates the link name and retries (up to 5 attempts).

### Cleanup

When a connection drops:
1. The `ConnectionHandle` is removed from `user_state.routes`
2. If this link was a peer link, `WithdrawHost` is broadcast for every host reachable via it
3. Any subscriptions whose `dst` passed through this link are cancelled
4. The output stream tasks detect the dropped `cancel` oneshot and stop

See `crates/amux/src/server/accept.rs`, `crates/amux/src/server/connection/driver.rs`.

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

See `crates/amux/src/transport/framing.rs`.

---

## Input Forwarding

Two input paths, both routable:

- **`RawInput { agent_id, data }`** — Raw keystroke bytes from terminals, delivered directly to PTY stdin.
- **`StructuredInput { agent_id, seq, payload }`** — A JSON payload from a rich client interpreted by the receiving session based on its advertised `structured_protocol`. For Claude, the payload is one of:
  - `SubmitMessage` — rich-client text input; the session writes it to the PTY with a short delay and a trailing carriage return (`\r`).
  - `PermissionResponse` — the user's reply to a permission request; translated to a keystroke and written to the PTY.

Structured inputs carry `seq`, which the server checks against the current
output seq to reject stale submissions with `SequenceNumberMismatch`; the
result is reported via `StructuredInputResult`.

Input messages are forwarded via generic `handle_routable` routing. When delivered locally, the agent is resolved by UUID and the session's input channel is written.

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
│  │ Reads MultiplexByteBuffer / MultiplexStructuredBuffer → sends     │  │
│  │ RawOutput / StructuredOutput routable messages along dst route.   │  │
│  │ Cancelled via oneshot when subscription is dropped/expired.       │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  Per Local Agent (via spawn_pty_agent):                                │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐ │
│  │ PTY Reader       │  │ Input Forwarder  │  │ Exit Monitor         │ │
│  │ (spawn_blocking) │  │ (spawn)          │  │ (spawn_blocking)     │ │
│  │                  │  │                  │  │                      │ │
│  │ PTY stdout →     │  │ input_rx →       │  │ Waits for exit →     │ │
│  │ MultiplexByteBuffer│ │ PTY stdin        │  │ SessionEvent::Ended  │ │
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
│  │ Watches JSONL file → parses → pushes to StructuredLogSource →     │  │
│  │ MultiplexStructuredBuffer                                         │  │
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
                                ├─> PtyHandle.subscribe() → MultiplexByteReader
                                ├─> Register SubscriptionEntry + send SubscribeRawResult
                                └─> Spawn output stream task (paired with cancel oneshot)
                                      │
                                      └─> MultiplexByteReader.read() → RawOutput msg → reply-routed to subscriber

Terminal ──RawInput──> Connection Handler → PtyHandle input channel → PTY stdin
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

amux integrates with Claude Code via hooks — shell commands that Claude Code calls on specific events.

### Wire shape

Hooks are delivered via `Command::HandleHook`, which carries an opaque byte
payload tagged by `HookProvider`. The protocol intentionally does not know the
hook shape:

```rust
enum HookProvider { Claude, Unknown }

// Command variant:
HandleHook {
    agent_id: Uuid,          // Pre-resolved by the client (via AMUX_AGENT_ID or session_id)
    provider: HookProvider,
    payload: Vec<u8>,        // Provider-specific bytes (e.g. Claude Code's hook JSON)
    external: bool,          // True for hooks from externally-started Claude sessions
}
```

For the `Claude` provider, the session parses the payload into a provider-internal
`ClaudeHook` (e.g. `SessionStart`, `PermissionRequest`, etc.) and acts on it.

### Hook Connection Flow

1. Claude Code invokes `amux hooks claude <event>` (reads hook JSON from stdin).
2. The CLI resolves the agent ID — either from the `AMUX_AGENT_ID` env var (amux-managed sessions) or from the hook JSON's `session_id` (external sessions).
3. The CLI opens a connection to the local server via `ConnectPolicy::ExistingOnly` (it does not start a server). The link name follows the normal terminal-style `term-{rand}` form — there is no special "hook-" prefix.
4. The CLI sends `Command::HandleHook { agent_id, provider: Claude, payload, external }`.
5. The server dispatches to the matching `ClaudeSession`, which parses the payload and either links a transcript file or appends a structured entry for subscribers.

### Permission Request Flow

```
Claude Code → `amux hooks claude permission-request` → Command::HandleHook → server
    → ClaudeSession parses payload → StructuredLogSource push (JSON permission-request entry)
    → MultiplexStructuredBuffer → rich client via StructuredOutput → user sees UI
    → StructuredInput (PermissionResponse payload) routable → server → writes keystroke to PTY
```

See `crates/amux-cli/src/hooks.rs`, `crates/amux/src/agent/claude/hooks.rs`, `crates/amux/src/agent/claude/session/hooks.rs`.

---

## Key Design Decisions

### 1. Unified connection_loop for all transports

Rather than separate `LocalConnection` and `RemoteConnection` types with different behavior, all connections use the same `connection_loop`. The `Transport` trait abstracts away the underlying stream. This eliminates code duplication and ensures protocol consistency across transports.

### 2. BroadcastBuffer atomic subscribe

The original design had a race condition window between getting the replay buffer and starting to receive live data. `BroadcastBuffer` (instantiated as `MultiplexByteBuffer` and `MultiplexStructuredBuffer`) solves this by holding a lock during both the snapshot and subscriber registration, ensuring zero gaps or duplicates. This is the core correctness guarantee for late-joining terminals and structured subscribers.

### 3. MessagePack serialization

MessagePack (rmp-serde with named/map format) replaced bincode for all transports. Named format provides forward/backward compatibility when fields are added, unlike bincode's positional encoding. Binary frames handle byte blobs cleanly without base64 encoding.

### 4. Handshake-based connection establishment

Connections start with standalone `Connect` / `ConnectResult` handshake frames (outside the `Message` enum). This:
- Assigns link names at connect time (used for routing)
- Carries JWT tokens for cloud authentication
- Supports link name collision retry

### 5. Lease-based subscriptions

Subscriptions are tracked explicitly in `active_subscriptions: HashMap<SubscriptionId, SubscriptionEntry>`, keyed by a fresh `SubscriptionId` returned to the client. Each entry owns a cancellation oneshot, the reply `dst` route, and a `lease_deadline`. A paired output stream task reads from the buffer and sends along the stored `dst`; the client keeps the subscription alive with `ExtendSubscription`. Subscriptions end via `Unsubscribe`, lease expiry, host withdrawal, or buffer closure, always emitting `SubscriptionClosed { reason, ... }` to the subscriber.

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
