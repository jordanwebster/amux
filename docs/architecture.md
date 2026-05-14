# Architecture

This document describes the current amux server architecture: identity,
routing, protobuf framing, scoped services, connection lifecycle, server
state, and agent I/O.

The canonical wire schema is
`crates/amux/proto/amux/v1/amux.proto`. Generated protobuf types live in
`crates/amux/src/protocol/wire/generated.rs`; runtime-facing wrappers and
encoders live under `crates/amux/src/protocol/message/` and
`crates/amux/src/protocol/wire/`.

## Quick Overview

amux is a multiplexer for long-running AI agent sessions. One server owns
local agent processes and exposes them to local terminals, peer servers, cloud
relays, and richer clients.

The main moving parts are:

- `Server` - starts listeners, owns global state, and handles session events.
- `Connection` - a local socket, named pipe, TCP/TLS stream, or WebSocket,
  all using the same protobuf message protocol after handshake.
- `AgentSession` - a local provider-specific agent session, currently Claude
  plus test agents in dev/test builds.
- `ServerUserState` - per-user connections, route contexts, local agents, and
  RPC state.
- `RpcDispatcher` - active inbound/outbound call state for unary and streaming
  RPCs.
- `BroadcastBuffer` - replay plus live fanout for PTY bytes and structured
  transcript entries.

Cloud servers are relay-oriented. They authenticate network connections and
forward route-addressed frames, but they do not host routed agent-service
endpoints.

## Identity And Routes

### Agents

Agents are identified by UUID. A local agent may also have a human-readable
name.

```rust
agent_id: Uuid
name: Option<String>
```

Runtime agent metadata is represented by `agent::Agent` and includes:

```rust
struct Agent {
    id: Uuid,
    host_id: Uuid,
    name: Option<String>,
    command: String,
    working_dir: PathBuf,
    route: Route,
    agent_type: String,
    io_protocols: Vec<String>,
    readonly: bool,
    args: Vec<String>,
    created_at: DateTime<Utc>,
}
```

For local agents, `route` materializes as empty. For remote agents, the route
is derived from one available `RouteContext` for the agent's host.

### Links

A `Link` names one direct connection hop. It is validated at construction:

- non-empty
- at most 128 bytes
- ASCII alphanumeric, hyphen, or underscore only
- no `.`, because `.` is the route separator

Terminal links are generated as `term-{rand}`. Server links are generated from
the configured host name, with unsupported characters converted to `-`, and
usually with a random suffix.

The handshake carries the proposed link as a raw string so the server can
return an `InvalidLinkName` error instead of dropping the connection. The
acceptor is authoritative: it may assign the proposed link or a suffixed
variant if the proposal is already reserved.

See `crates/amux/src/protocol/link.rs` and
`crates/amux/src/protocol/route.rs`.

### Routes

A `Route` is a stack of `Link`s. `links[0]` is the next hop. The display and
serde string form is dot-separated, for example `cloud.local-host`.

Operationally, a route is the identity of a concrete tunnel/path through the
network. Host IDs answer "which host is at the far end?"; routes answer
"which exact path are we using to reach it?" A host may be reachable through
multiple routes, and each route can have independent tunnel/RPC lifetime.

```rust
struct Route {
    links: VecDeque<Link>,
}
```

Forwarding is stack-based:

1. Pop the next hop from `dst`.
2. Push that hop onto `src`.
3. Forward the frame to the connection registered under that hop.
4. If `dst` is empty, the frame has reached its endpoint.

Replies use the same operation on the incoming `src` route:

```rust
Route::reply(src) == Route::send(src)
```

This makes the accumulated source route the return path.

## Wire Protocol

amux uses protobuf for the handshake and for every post-handshake transport
message.

Current protocol version:

```rust
pub const PROTOCOL_VERSION: u32 = 3;
```

### Handshake

The connecting side writes one raw protobuf `ConnectRequest` frame. The
listener replies with one raw protobuf `ConnectResponse` frame. These frames
are exchanged with `read_frame`/`write_frame` before the connection is split
into reader and writer tasks.

```protobuf
message ConnectRequest {
  repeated uint32 supported_protocol_versions = 1;
  string proposed_link_name = 2;
  optional ClientInfo client = 3;
  optional string auth_token = 4;
  Capabilities capabilities = 5;
}

message ConnectResponse {
  oneof outcome {
    ConnectAccepted accepted = 1;
    Error error = 2;
  }
}

message ConnectAccepted {
  uint32 protocol_version = 1;
  string assigned_link_name = 2;
  optional HeartbeatConfig heartbeat = 3;
  Capabilities capabilities = 4;
}
```

The acceptor:

1. Decodes `ConnectRequest`.
2. Checks `PROTOCOL_VERSION` is supported.
3. Checks configured minimum client versions when `client.name` matches.
4. Validates JWT credentials for cloud-mode network connections.
5. Validates and reserves the assigned link in per-user connection state.
6. Returns `ConnectAccepted` with the final link name and optional heartbeat
   config.

Local connections use `LOCAL_USER_ID` and no heartbeat. Cloud-mode network
connections authenticate to a JWT-derived per-user `ServerUserState`; non-cloud
network links use the local user state.

See `crates/amux/src/protocol/handshake.rs`,
`crates/amux/src/transport/handshake.rs`, and
`crates/amux/src/server/accept.rs`.

### TransportMessage

After the handshake, every frame decodes to a runtime `Message` wrapper around
protobuf `TransportMessage`.

```rust
enum Message {
    Frame(Frame),
    Ping,
    Pong,
    Reauth(ReauthRequest),
    ReauthResponse(ReauthResponse),
    GoAway(GoAway),
}
```

All application RPC traffic uses one `Frame` envelope. Local, direct-peer, and
multi-hop routed behavior are enforced from connection provenance, method
access policy, and route shape rather than from distinct frame variants.

Each `Frame` carries `src`, `dst`, and a 16-byte `call_id`. `FrameBody`
provides request/response/stream/cancel semantics:

```rust
struct Frame {
    src: Route,
    dst: Route,
    call_id: CallId,
    body: FrameBody,
}

enum FrameBody {
    Request(RequestFrame),
    Response(ResponseFrame),
    StreamItem(Vec<u8>),
    Cancel,
    RoutingError { failed_route: Route, error: ProtocolError },
}

enum ResponseFrame {
    Payload(Vec<u8>),
    Error(ProtocolError),
}
```

`dst == Route::empty()` means the frame is for this server. Otherwise the
connection loop forwards the frame to the next hop. Relay-generated delivery
failures are ordinary frames with `FrameBody::RoutingError`.

See `crates/amux/src/protocol/message/envelope.rs` and
`crates/amux/src/protocol/wire/runtime.rs`.

### Method Access

Every protobuf RPC method has an explicit access policy in
`crates/amux/src/protocol/method.rs`.

| Access | Methods | Route |
| --- | --- | --- |
| LocalOnly | `AgentService/ListAgents`, `AgentService/ResolveAgent`, `HookService/HandleHook`, `AdminService/*` | local connection only |
| DirectPeerOnly | `RoutingService/SubscribeRoutingEvents` | direct peer connection |
| RoutedEndpoint | `AgentService/SubscribeAgentEvents`, `CreateAgent`, `RenameAgent`, `DeleteAgent`, `SubscribeSession`, `SendInput` | direct or multi-hop route |

Dispatch rejects known methods used with the wrong access path with
`PermissionDenied`, and unknown methods with `Unimplemented`.

### Errors

Wire errors use protobuf `Error { code, message, details }`. The Rust runtime
maps these to `ProtocolError`, including typed details such as
`ProtocolVersionMismatch`, `UpdateRequired`, `InvalidLinkName`, and
`SequenceNumberMismatch`.

See `crates/amux/src/protocol/wire/error.rs`.

## Transports

All transports implement the same `Transport` trait:

```rust
trait Transport {
    async fn read_frame(&mut self) -> Result<Vec<u8>>;
    async fn write_frame(&mut self, data: &[u8]) -> Result<()>;
    async fn read_message(&mut self) -> Result<Message>;
    async fn write_message(&mut self, msg: &Message) -> Result<()>;
}
```

`TransportSplit` produces a reader and writer half so the connection driver
can keep transport reads in a task that is never cancelled by `select!`.

| Transport | Stream | Serialization | Framing |
| --- | --- | --- | --- |
| `LocalTransport` | Unix socket or Windows named pipe | protobuf | 4-byte big-endian length prefix |
| `TcpTransport<S>` | plain TCP or TLS stream | protobuf | 4-byte big-endian length prefix |
| `WebSocketTransport` | WebSocket stream | protobuf | native binary WebSocket frames |

Maximum binary frame size is `MAX_FRAME_SIZE` (16 MiB). TCP sockets are
configured with `TCP_NODELAY` and keepalive.

See `crates/amux/src/transport.rs` and `crates/amux/src/transport/`.

## Connection Lifecycle

Each accepted or outbound connection uses the same driver:

1. Run the handshake on the unsplit transport.
2. Reserve the connection link in `ServerUserState::connections`.
3. Mark non-local links as peer connections and start a peer routing-event
   stream.
4. Split the transport.
5. Spawn `reader_loop` and `writer_loop`.
6. Run `connection_loop` over channels.
7. On exit, remove the connection, drop affected route contexts, cancel
   affected RPC/session state, withdraw affected topology, and let the
   writer drain.

The task split is:

- `reader_loop` reads decoded `Message`s and sends `Incoming::Msg`.
- `writer_loop` drains the per-connection outgoing channel and writes frames.
  It also handles transport background I/O such as WebSocket pongs.
- `connection_loop` handles incoming messages, heartbeat deadlines, token
  refresh deadlines, and explicit close requests.

Top-level protobuf decode errors close the connection. The driver queues a
`GoAway` with reason `ProtocolError` before closing when possible.

### Heartbeats

Non-local connections negotiate an idle timeout. Both peers close the
connection after that timeout without inbound traffic.

Only the dialer sends `Ping`, currently at `idle_timeout / 3` after its last
non-heartbeat outbound write. The acceptor replies with `Pong`. Local
connections disable heartbeats.

### Token Refresh

Cloud connections refresh JWTs before expiry. The local side obtains a new
token from the cloud API and sends in-band `ReauthRequest`; the cloud side
validates it and replies with `ReauthResponse`.

See `crates/amux/src/server/connection/`.

## Server State

Global state is small and mostly configuration:

```rust
struct ServerState {
    config: Config,
    host_id: Uuid,
    is_cloud_server: bool,
    jwt_validator: Option<Arc<JwtValidator>>,
    users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    shutdown_tx: mpsc::Sender<ShutdownRequest>,
}
```

Each authenticated user gets isolated routing, RPC, and agent state. Local
Unix/named-pipe clients use `LOCAL_USER_ID` (`Uuid::nil()`).

```rust
struct ServerUserState {
    connections: HashMap<Link, ConnectionEntry>,
    routes: HashMap<Route, RouteContext>,
    hosts: HashMap<Uuid, HostContext>,
    remote_name_owners: HashMap<String, Uuid>,
    local_agents: HashMap<Uuid, LocalAgentContext>,
}

struct ConnectionEntry {
    handle: ConnectionHandle,
    kind: ConnectionKind,
    rpc: RpcDispatcher,
}

struct RouteContext {
    host_id: Uuid,
    rpc: RpcDispatcher,
}

struct HostContext {
    host: Host,
    agents: HashMap<Uuid, Agent>,
}

struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    close_tx: watch::Sender<Option<String>>,
}
```

`connections` is the canonical transport table. A connection is identified by
the first-hop `Link`, records whether the connection is local or peer, and owns
connection-scoped RPC state for direct local endpoint calls, peer control
streams, and direct routed calls whose counterparty path starts at that
connection but does not have a host route context yet.

`routes` is the canonical tunnel table. A route context identifies the remote
host at that exact route and owns the route-scoped RPC call table. A one-hop
peer host is still represented as `Route::from_link(link)`, but the connection
handle itself is owned by `connections`.

`hosts` owns host metadata and the agents available at each host. Multiple
route contexts may point at the same host context. The first `HostUp` for an
exact route owns that route; a later `HostUp` for the same host at a different
route creates another route context rather than rewriting the existing route.
amux does not currently migrate live route contexts when a shorter or "better"
route appears.

Local agents live in `local_agents`; remote agents live in host contexts.
Agent listing and resolution derive their view from `local_agents` plus host
contexts, then choose a deterministic available route for each remote host.
`remote_name_owners` is a small alias cache for unqualified remote name
resolution: the first remote agent to claim a name keeps that name until it is
withdrawn or re-announced under another name. Route-qualified lookup can still
resolve duplicate remote names through the supplied route.

See `crates/amux/src/server/state.rs`,
`crates/amux/src/server/routing/topology.rs`, and the route dispatch modules.

## RPC State

`RpcDispatcher` tracks active calls within one call table. Route contexts own
the call tables for advertised routed tunnels. Connection entries own call
tables for direct local/peer control traffic and for raw direct-hop routed
traffic before a host route exists. Calls are keyed by `CallId`; route lookup
and hop selection stay in the routing layer.

Endpoint routed RPC resolves the full counterparty return path to the longest
known route-context prefix. For example, a frame arriving from `peer.client`
uses the `peer` route context when `peer` is the advertised host route. If no
route context exists yet, endpoint dispatch can fall back to the current
connection's RPC table when the counterparty path starts with that connection
link.

Inbound calls are endpoint-owned work started by a received request. Outbound
calls are work this node is waiting on. State tracks whether a call is waiting
for a response, has an active stream, or is closing.

The dispatcher also owns method-specific deduplication keys for streams that
must be unique. These keys are opaque to generic RPC state and are chosen by the
application layer, notably:

- one active peer routing-event stream per peer link

When routes close or withdraw, the server uses RPC state to send cancels or
routing errors for locally originated calls and to terminate affected
`SubscribeSession` streams.

See `crates/amux/src/rpc/`.

## Routing And Discovery

Peer topology is propagated through the peer-scoped
`RoutingService/SubscribeRoutingEvents` server-streaming RPC.

Initial snapshot order is:

1. hosts
2. `SnapshotComplete`

Events are:

```rust
enum RoutingEvent {
    HostUp { id, name, route, version },
    HostDown { id, route },
    SnapshotComplete,
}
```

Host routes are hop-relative. When a peer host event arrives, the receiver
prepends the inbound peer link before storing or rebroadcasting it. Agent
inventory is not part of routing; interested non-cloud servers open a routed
`AgentService/SubscribeAgentEvents` stream for hosts whose capabilities include
at least one `supported_agent_type`.
Cloud relays advertise no supported agent types; normal hosts advertise
`claude`, plus `test-agent` in dev/test builds.

The protobuf `Host` message is route-free identity and metadata. `HostUp`
carries the route for that announcement:

```protobuf
message Host {
  bytes host_id = 1;
  string name = 2;
  string version = 3;
}

message HostUp {
  Host host = 1;
  Route route = 2;
}

```

Important topology rules:

- Cloud relays do not announce themselves as hosts in initial snapshots.
- `HostUp` is the discovery point for routed agent subscriptions.
- `AgentUp`/`AgentDown` are host inventory events carried by
  `AgentService/SubscribeAgentEvents`; they do not reserve or remove
  connection links or route contexts.
- `HostDown` and link closure remove route contexts and cancel affected
  session/RPC state. A host context and its agents are removed only after
  the last route to that host disappears.
- Announcements learned from one peer are not echoed back to the same peer.
- Route announcements are exact-route ownership, not migration. Re-announcing
  the same host or agent at another route adds another available route; it
  does not rewrite existing route contexts or move their RPC state.

See `crates/amux/src/services/routing.rs`,
`crates/amux/src/server/routing/peers.rs`, and
`crates/amux/src/server/dispatch/peer/`.

## Routed Forwarding

When a unified `Frame` arrives:

1. Non-local connections must have `src.peek() == ctx.link`; otherwise the
   frame is dropped as spoofed.
2. If `dst` has a next hop, the relay pops it, pushes it onto `src`, and sends
   the unchanged frame body to that connection.
3. If the next hop is missing or the channel is closed, the relay sends a
   `RoutingError` back along the accumulated return path.
4. If `dst` is empty, the frame body is dispatched to the routed endpoint.

Cloud servers reject endpoint routed payloads because cloud relays are not
agent hosts.

Endpoint dispatch uses the accumulated `src` as the counterparty return path.
The RPC call table is selected by exact route context first, then by the
longest advertised route prefix, with a connection-scoped fallback when the
counterparty path starts with the current connection link. This keeps replies
and route-scoped cleanup tied to the advertised tunnel that owns the call table.

Routing failures include a reconstructed `failed_route`, built from the
reverse accumulated source path plus the missing hop and remaining destination
path. Endpoints use the `call_id` and failed route for cleanup.

See `crates/amux/src/server/routing/forwarding.rs` and
`crates/amux/src/server/dispatch/frame.rs`.

## Services

### Local Services

Local services are trusted-control calls accepted only from local connections.

- `AgentService/ListAgents`
- `AgentService/ResolveAgent`
- `HookService/HandleHook`
- `AdminService/Debug`
- `AdminService/Shutdown`
- `AdminService/Suspend`
- `AdminService/Resume`
- `AdminService/ConnectToServer`

Network peers cannot invoke local methods.

### Peer Services

`RoutingService/SubscribeRoutingEvents` runs only on peer links. It is a
server-streaming RPC over a direct peer route. The stream stays open for the
connection lifetime and carries host topology changes.

### Routed Agent Services

Routed agent services are endpoint calls to an agent host:

- `SubscribeAgentEvents`
- `CreateAgent`
- `RenameAgent`
- `DeleteAgent`
- `SubscribeSession`
- `SendInput`

`SubscribeAgentEvents` is a routed server-streaming RPC. The request includes
the target `host_id`; the endpoint rejects mismatches and hosts with no
`supported_agent_types`. The snapshot is current local agents, then
`SnapshotComplete`, followed by live `AgentUp`/`AgentDown` events. `AgentUp` is
an upsert.

`CreateAgent` supports Claude PTY runtime and dev/test agents. The protobuf
schema also contains `ClaudeSdkRuntime`, but the service currently returns
`Unimplemented` for that runtime.

See `crates/amux/src/services/`.

## Agent Sessions

`AgentSession` is the server-owned handle for a local agent:

```rust
enum AgentSession {
    Claude(ClaudeSession),
    #[cfg(any(debug_assertions, test))]
    TestAgent(TestAgentSession),
}
```

`AgentSession::try_new(req)` constructs the provider-specific session.
`start()` spawns the underlying process and returns an exit-monitor handle.

For Claude, the session stores:

```rust
struct ClaudeSession {
    agent_id: Uuid,
    name: Option<String>,
    command: String,              // "claude"
    working_dir: PathBuf,
    pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,
    terminal_size: Option<TerminalSize>,
    session_id: Option<Uuid>,
    readonly: bool,
    args: Vec<String>,
    name_source: LocalAgentNameSource,
    name_sniffer_abort: Option<AbortHandle>,
    created_at: DateTime<Utc>,
}
```

Managed Claude sessions spawn `claude` in a PTY with `AMUX_AGENT_ID` set.
Resumed sessions pass `--resume <session_id>` and sanitized resume args.
Readonly Claude sessions are created from external hooks; they have transcript
tailing but no PTY.

Creating a local agent:

1. Checks per-user local agent limit.
2. Checks UUID and name uniqueness.
3. Constructs and starts the `AgentSession`.
4. Registers the local agent in per-user local agent state.
5. Emits `AgentUp` to active `SubscribeAgentEvents` subscribers.
6. Starts a task that sends `SessionEvent::Ended` when the process exits.

Deleting or process exit withdraws the agent, emits `AgentDown` to active
`SubscribeAgentEvents` subscribers, closes matching session subscriptions, and
stops the session as needed.

See `crates/amux/src/agent/` and
`crates/amux/src/server/routing/agents.rs`.

## PTY And Buffers

`spawn_pty_agent` creates the PTY and starts three background tasks:

- PTY reader: blocking read from PTY stdout, writes to `MultiplexByteBuffer`.
- input forwarder: receives input bytes and writes them to PTY stdin.
- exit monitor: waits for the child, drops the PTY master, closes byte and
  structured buffers.

`PtyHandle` owns the PTY input channel, resize support, current size, and the
byte replay buffer.

`BroadcastBuffer<P>` backs both byte and structured output. Its key invariant
is that `write()` and `subscribe()` are synchronized through the storage lock,
so a subscriber receives a coherent replay snapshot followed by live items
without gaps or duplicates.

Concrete buffers:

- `MultiplexByteBuffer` stores a bounded byte vector and replays as one chunk.
- `MultiplexStructuredBuffer` stores bounded structured entries with
  monotonically increasing sequence numbers.

See `crates/amux/src/agent/pty.rs` and `crates/amux/src/buffer.rs`.

## Session I/O

Agent output is exposed through the routed server-streaming
`AgentService/SubscribeSession` RPC. Input is an independent routed unary
`AgentService/SendInput` RPC keyed by agent id and `io_protocol`.

```protobuf
message SubscribeSessionRequest {
  bytes agent_id = 1;
  string io_protocol = 2;
  optional bytes args = 3;
}

message SubscribeSessionResponse {
  oneof event {
    SessionOpened opened = 1;
    SessionOutput output = 2;
    ReplayComplete replay_complete = 3;
  }
}

message SendInputRequest {
  bytes agent_id = 1;
  string io_protocol = 2;
  oneof event {
    SessionInput input = 10;
    SessionControl control = 11;
  }
}
```

The core protocol treats `args`, input payloads, output payloads, controls,
and replay cursors as opaque bytes. The selected `io_protocol` defines those
payloads.

### `claude_raw_v1`

Raw PTY protocol:

- `SubscribeSessionRequest.args` may contain terminal size and a tail-bytes
  replay query.
- `SendInputRequest.input.payload` is written directly to PTY stdin.
- `SendInputRequest.control.payload` supports terminal resize.
- `SessionOutput.payload` is raw PTY stdout bytes.
- `ReplayComplete.cursor` is absent.

### `claude_pty_transcript_v1`

Structured Claude transcript protocol:

- `SubscribeSessionRequest.args` may contain terminal size plus replay `since` or
  `tail_count`.
- Replay `since` uses the last sequence observed by the client; the server
  resumes after it.
- `SessionOutput.payload` is `ClaudePtyTranscriptV1Output { seq_id, payload }`,
  where `payload` is JSON-encoded Claude transcript or hook output.
- `ReplayComplete.cursor` is a protobuf cursor containing the sequence at the
  replay boundary.
- `SendInputRequest.input.payload` is `ClaudePtyTranscriptV1Input { expected_seq,
  actions }`.
- Structured input actions are translated to PTY writes and bounded delays.
- The server rejects stale structured input with `SequenceNumberMismatch` when
  `expected_seq` differs from the current structured log sequence.
Accepted structured inputs complete the `SendInput` unary response. Rejected
structured inputs return the corresponding `ProtocolError`.

See `crates/amux/src/services/agent/session.rs`,
`crates/amux/src/protocol/session.rs`, and
`crates/amux/src/agent/claude/io.rs`.

## Structured Logs And Hooks

`StructuredLogSource` owns a `MultiplexStructuredBuffer` plus an optional
Claude transcript tailer. Linking a new transcript stops the old tailer,
clears stored entries, and starts tailing the new file while keeping existing
subscribers connected.

Claude hooks are delivered through local
`HookService/HandleHook { agent_id, provider, payload, external }`.

The CLI resolves the target agent as follows:

- managed sessions use `AMUX_AGENT_ID`
- external sessions use the Claude hook `session_id`

For managed sessions, the hook is dispatched to the existing `ClaudeSession`.
For external sessions, a non-session-end hook can bootstrap a readonly Claude
session when the hook contains the required working directory and transcript
path.

Claude hook handling:

- `SessionStart` records `session_id` and links the transcript.
- `PermissionRequest`, `Stop`, and `Notification` are appended to the
  structured log with an injected `type`.
- `SessionEnd` withdraws readonly external sessions.

See `crates/amux/src/services/hook.rs`,
`crates/amux/src/agent/claude/session/hooks.rs`, and
`crates/amux-cli/src/hooks.rs`.

## Config

Config is loaded from `$XDG_CONFIG_HOME/amux/config.yaml`, falling back to
`~/.config/amux/config.yaml`, or from an explicit `--config` path.

Important fields:

```rust
struct Config {
    host_name: String,
    cloud_url: String,
    socket_path: PathBuf,
    tcp_port: Option<u16>,
    websocket_port: Option<u16>,
    randomise_link_name: bool,
    state_path: PathBuf,
    check_for_updates: bool,
    enforce_tls_in_cloud_mode: bool,
    enable_cloud_mode: Option<bool>,
    prevent_idle_sleep: Option<bool>,
    minimum_client_versions: HashMap<String, String>,
    idle_timeout_secs: u32,
    keybinds: Keybinds,
    path: Option<PathBuf>,
}
```

Cloud servers must configure both `tcp_port` and `websocket_port`. Release
builds require HTTPS cloud URLs. `idle_timeout_secs` must fit into protobuf
heartbeat milliseconds.

See `crates/amux/src/config.rs`.

## Runtime Task Model

High-level server runtime:

```text
Server::run
  local listener
  optional TCP listener
  optional WebSocket listener
  optional cloud outbound connection task
  session event task
  update checker task in local mode
  shutdown/suspend control loop

Per connection
  handshake
  reserve link
  optional peer routing-event stream setup
  reader_loop
  writer_loop
  connection_loop
  cleanup topology/RPC/session state

Per local agent
  PTY reader
  PTY input forwarder
  child exit monitor
  optional transcript tailer
  optional name sniffer

Per SubscribeSession
  output stream task

Per SendInput
  apply raw PTY input/control or structured transcript input
```

The architecture deliberately keeps routing reachability, RPC call lifecycle,
and agent process lifecycle separate. Route contexts say what can be reached.
RPC state says which calls are active. Agent sessions own local process and I/O
resources.
