# amux: New Architecture

Status: **draft, in active design.** Reference sketch at `~/new_architecture.png`.
Sections marked _(TODO)_ are placeholders to be filled in as we walk through
the remaining user journeys.

The intent is for this document to be implementable as-written: it should be
specific enough that an engineer (or agent) can build the system from these
contents alone, with implementation choices spelled out where they matter and
left abstract where they don't.

---

## 1. Motivation

The current architecture grew out of a single layer that handles routing,
multiplexing, RPC framing, and connection lifecycle all together. It has
served the prototype well, but several pressures are pushing against it:

- **Clients are second-class.** A "client" is just an amux server with a
  special `Observer` routing role. As `amux` moves toward being usable as a
  library (e.g. `amux-ui`, embedded servers, third-party apps), that model
  is leaking. Clients want an ergonomic API, not a routing role.
- **We've reimplemented gRPC.** `Frame` / `FrameBody` / `Request` /
  `Response` / `StreamItem` / `Cancel` / `call_id` are an in-house RPC
  layer. gRPC over HTTP/2 already solves this — multiplexing, streaming,
  cancellation, deadlines, keep-alive.
- **Routing and transport are entangled.** There's no first-class object
  you can hand a frame to and say "deliver this to host B." Today's
  "tunnel" between two hosts exists only implicitly through routes.

The redesign extracts a small number of cleanly-separated concerns:

1. **Tunnels** — a first-class bidirectional byte channel between two hosts,
   established implicitly, used as the transport for everything host↔host.
2. **gRPC everywhere** — drop the custom RPC layer. Use gRPC over Unix /
   TCP+TLS / in-memory / tunnel transports.
3. **First-class clients** — clients no longer wear a routing role. They
   talk to `ClientService`, an ergonomic per-host gRPC service that owns
   an aggregated view of hosts + agents.
4. **Services own state + emit events.** Each daemon-internal service is a
   Rust struct that owns its state, exposes methods (called by both gRPC
   handlers and other in-process services), and emits events through
   subscribe primitives. No central event bus; each service has its own.

---

## 2. Glossary

- **Host** — an amux process that owns agents and/or relays traffic for
  other hosts. Identified by a `host_id` (UUID) generated at startup and
  stable for the daemon's lifetime. A "relay" in the cloud sense is just
  a host whose capabilities advertise no agent types.
- **Connection** — a generic transport connection (Unix socket, TCP+TLS,
  in-memory). Carries a gRPC channel. Every link rides on a connection,
  but not every connection is a link.
- **Link** — a `RoutingService.Connect` bidi stream that has completed a
  successful `Hello` / `HelloAck` handshake. Always between two hosts.
  Identified by its `assigned_link_name`. A tunnel rides on one or more
  links. **Client↔server connections are NOT links** — they're just gRPC
  channels to `ClientService`.
- **Link name** — a string negotiated at handshake. Connector proposes
  (e.g. `"jlw-laptop"`), acceptor assigns the final value. Both ends of
  a single link share the same name.
- **Route** — `Vec<link_name>`. Each forwarding hop pops the front and uses
  it to find its outgoing link. Empty `dst` means the receiver is the
  endpoint.
- **Tunnel** — a logical bidirectional byte channel between two hosts,
  identified by `TunnelId { initiator, target }`. Up to **two** tunnels
  exist between any host pair (one per initiator direction). Established
  implicitly on first use.
- **TunnelId** — `(initiator_host_id, target_host_id)` pair. Set by the
  initiator at tunnel creation; never modified at intermediate hops.
- **Client** — a user of a host. Talks to its host's `ClientService` over a
  local gRPC connection (not a link). Has no `host_id` and is not in the
  routing topology.
- **Service** — an in-process Rust struct owning state + methods + event
  source(s). Three top-level services live on each daemon:
  `RoutingService`, `AgentService`, `ClientService`. See §4.5.
- **Agent** — a running process (PTY or SDK) owned by exactly one host.
- **Session** — a subscription to an agent's I/O stream.

---

## 3. High-level topology

```
            ┌──────────────────────────────────────────┐
            │              Relay R                     │
            │    ─ RoutingService                      │
            │    ─ ClientService (admin only)          │
            │    no agents (supported_agent_types = ∅) │
            └──────┬───────────────────────────┬───────┘
                   │                           │
   Connect stream  │                           │  Connect stream
   (TCP+TLS,       │                           │  (TCP+TLS,
    JWT in initial │                           │   JWT in initial
    metadata)      │                           │   metadata)
                   │                           │
          ┌────────┴────────┐         ┌────────┴────────┐
          │     Host A      │◀ ─ ─ ─ ▶│     Host B      │
          │                 │  tunnel │                 │
          │ RoutingService  │ A ↔ B   │ RoutingService  │
          │ AgentService    │(logical;│ AgentService    │
          │ ClientService   │ rides as│ ClientService   │
          │                 │ Tunnel- │                 │
          │                 │ Frames) │                 │
          └────────┬────────┘         └────────┬────────┘
                   │                           │
       Unix socket │                           │ Unix socket
       (or in-process duplex                   │
        for embedded clients)                  │
                   ▼                           ▼
              ┌────────┐                  ┌────────┐
              │ Client │                  │ Client │
              └────────┘                  └────────┘
```

One user's view at steady state. Solid lines are physical connections:
the gRPC `RoutingService.Connect` streams between routing parties, and
the Unix-socket gRPC streams from clients to their host's
`ClientService`. The dashed line is the **logical** tunnel A↔B: it has
no separate transport — its frames ride inside the two physical
Connect streams as `TunnelFrame` messages and are forwarded opaquely
by R (T-1/T-2).

Key constraints reflected here:

- Relays enforce per-user isolation (I-3) — every entity shown belongs
  to one user's `ServerUserState`.
- Clients connect only to `ClientService` (C-1, C-3); they never speak
  the routing protocol and never reach `AgentService` or
  `RoutingService` directly.
- The A↔B tunnel is identified by `TunnelId { A, B }` (T-1, T-3); R
  neither terminates nor inspects its payload (T-2).

The topology generalises: a host can connect to multiple relays; a
tunnel can traverse multiple relay hops (each intermediate relay still
forwards opaquely). The two-host / one-relay case shown here is the
minimum that exercises every layer.

---

## 4. Concepts

### 4.1 Tunnels

A tunnel is a bidirectional byte channel between two hosts. The byte
channel is fed at one end by `tonic` (the gRPC stack), and at the other
by `tonic` on the peer. Internally, the routing layer wraps outbound
bytes in `TunnelFrame`s and unwraps inbound ones — invisibly to gRPC.

Because HTTP/2 has fixed client/server roles per connection, a single
tunnel carries one gRPC client→server flow. When **both** hosts want to
make calls on each other (e.g. each calling `SubscribeAgentEvents` on the
other), two tunnels coexist between them — one per "client direction" —
distinguished by the `initiator` field of the `TunnelId`.

Tunnels are **implicit**: no explicit handshake, no negotiation. The
first byte either side wants to send creates the local half. The first
inbound frame for an unknown `TunnelId` creates the peer half. See §6
for the in-process tunnel design.

### 4.2 Protocol structure

A single `amux.proto` containing three services:

- **`RoutingService`** — host↔host: handshake, routing events, tunnel
  frames. See §5.
- **`ClientService`** — client↔host: ergonomic client API. See §4.4.
- **`AgentService`** — host↔host application service, riding over a
  tunnel: agent inventory, create/delete, sessions, input.

Per-`io_protocol` payload types (`ClaudeRawV1*`, `ClaudePtyTranscriptV1*`,
etc.) may eventually move to their own proto files as that catalog grows.

Authentication is **always** in gRPC metadata (`authorization: Bearer
<jwt>`), validated by a `tonic` interceptor. Auth never appears in
protobuf message bodies.

### 4.3 Transports and gRPC

gRPC over HTTP/2 is the universal RPC layer. Transports below HTTP/2:

| Today's transport             | New world                                                |
| ---                           | ---                                                      |
| `Transport` trait + framing   | **Removed.** tonic owns framing.                         |
| `MessageReader` / `MessageWriter` / `TransportSplit` | **Removed.**                      |
| `framing.rs` (length-prefix)  | **Removed.** HTTP/2 framing replaces it.                 |
| `UnixTransport`               | **Reduced.** Plain `tokio::net::UnixStream` + listener helper for tonic's `serve_with_incoming`. |
| `TcpTransport`                | **Reduced.** Plain `tokio::net::TcpStream` + listener helper. |
| `tls.rs`                      | **Kept.** rustls config + accept helper.                 |
| `WebSocketTransport`          | **Removed.** No consumer.                                |
| `MemoryTransport`             | **Reduced.** `tokio::io::duplex` (no wrapper).           |
| —                             | **`TunnelTransport` (new).** Lives under `routing/`, not `transport/`. See §6. |

After the cleanup, `crates/amux/src/transport/` shrinks to listener-setup
helpers for Unix and TCP+TLS (each producing a `Stream<Item = Result<IO, _>>`
suitable for `tonic::transport::Server::serve_with_incoming`), plus the
rustls bits. Everything else is plain `tokio::io::*` and tonic types.

Also dropped from today's protocol stack:

- Custom RPC layer (`Frame` / `call_id` / `FrameBody` / `Request` /
  `Response` / `StreamItem` / `Cancel`). gRPC owns these.
- `Ping` / `Pong`. HTTP/2 PING via `tonic` keep-alive replaces them.
- `endpoint_type` / `routing_role` enum. Service shape conveys the
  distinction.

### 4.4 ClientService

`ClientService` is the gRPC service the daemon exposes to clients over a
local gRPC connection (Unix socket; in-memory for embedded clients).
It is the only client-facing surface; `RoutingService` and `AgentService`
are host↔host services and are never reachable by a client.

#### 4.4.1 Responsibilities

- Maintain an aggregated, filtered model of hosts and agents across the
  network.
- Resolve `AgentRef` lookups (id-or-name).
- Dispatch agent-targeted method calls either to the local `AgentService`
  (in-process) or to the remote host's `AgentService` (via tunnel).
- Expose subscribe streams for the model (`SubscribeHosts`,
  `SubscribeAgents`) and for agent sessions (`SubscribeSession`).
- Handle local-only admin and hooks methods.

#### 4.4.2 API

```proto
service ClientService {
  // One-shot inventory
  rpc ListHosts(ListHostsRequest)   returns (ListHostsResponse);
  rpc ListAgents(ListAgentsRequest) returns (ListAgentsResponse);

  // Streaming model — snapshot then deltas
  rpc SubscribeHosts(SubscribeHostsRequest)   returns (stream SubscribeHostsResponse);
  rpc SubscribeAgents(SubscribeAgentsRequest) returns (stream SubscribeAgentsResponse);

  // Lifecycle
  rpc CreateAgent(CreateAgentRequest) returns (CreateAgentResponse);
  rpc RenameAgent(RenameAgentRequest) returns (RenameAgentResponse);
  rpc DeleteAgent(DeleteAgentRequest) returns (DeleteAgentResponse);

  // Session
  rpc SubscribeSession(SubscribeSessionRequest) returns (stream SubscribeSessionResponse);
  rpc SendInput(SendInputRequest)               returns (SendInputResponse);

  // Admin (local-only by nature)
  rpc Debug(DebugRequest)                       returns (DebugResponse);
  rpc Shutdown(ShutdownRequest)                 returns (ShutdownResponse);
  rpc Suspend(SuspendRequest)                   returns (SuspendResponse);
  rpc Resume(ResumeRequest)                     returns (ResumeResponse);
  rpc ConnectToServer(ConnectToServerRequest)   returns (ConnectToServerResponse);

  // Hooks
  rpc HandleHook(HandleHookRequest) returns (HandleHookResponse);
}

message AgentRef {
  oneof identifier {
    bytes  agent_id = 1;
    string name     = 2;
  }
}
```

Message bodies:

```proto
message ListHostsRequest {}
message ListHostsResponse  { repeated Host hosts = 1; }

message ListAgentsRequest  {}
message ListAgentsResponse { repeated Agent agents = 1; }

message SubscribeHostsRequest {}
message SubscribeHostsResponse {
  oneof event {
    HostAdded        host_added        = 1;
    HostRemoved      host_removed      = 2;
    SnapshotComplete snapshot_complete = 100;
  }
}
message HostAdded   { Host host = 1; }
message HostRemoved { bytes host_id = 1; }

message SubscribeAgentsRequest  {}
message SubscribeAgentsResponse {
  oneof event {
    AgentUp          agent_up          = 1;
    AgentDown        agent_down        = 2;
    AgentUpdated     agent_updated     = 3;
    SnapshotComplete snapshot_complete = 100;
  }
}
message AgentUp      { Agent agent = 1; }
message AgentDown    { bytes agent_id = 1; }
message AgentUpdated { Agent agent = 1; }

message CreateAgentRequest {
  optional bytes  host_id = 1;   // absent ⇒ local host
  optional string name    = 2;
  oneof config {
    ClaudeCreateConfig    claude     = 10;
    TestAgentCreateConfig test_agent = 100;
  }
}
message CreateAgentResponse { Agent agent = 1; }

message RenameAgentRequest  { AgentRef agent = 1; string new_name = 2; }
message RenameAgentResponse { Agent agent = 1; }

message DeleteAgentRequest  { AgentRef agent = 1; }
message DeleteAgentResponse {}

message SubscribeSessionRequest {
  AgentRef agent       = 1;
  string   io_protocol = 2;
  optional bytes args  = 3;   // io_protocol-defined; opaque to ClientService
}
message SubscribeSessionResponse {
  oneof event {
    SessionOpened  opened          = 1;
    SessionOutput  output          = 2;
    ReplayComplete replay_complete = 3;
  }
}

message SendInputRequest {
  AgentRef agent       = 1;
  string   io_protocol = 2;
  oneof event {
    SessionInput   input   = 10;
    SessionControl control = 11;
  }
}
message SendInputResponse {}
```

Admin and hook messages keep today's shapes (`Debug`, `Shutdown`,
`Suspend`, `Resume`, `ConnectToServer`, `HandleHook`).

No `GetServerInfo` / `GetLocalHost` method. If a client needs to know
which host is "local," it discovers via `SubscribeHosts` (the local host
appears in that stream like any other).

#### 4.4.3 Internal model

```rust
struct ClientService {
    user_state: Arc<ServerUserState>,
    routing:    Arc<RoutingService>,
    agents:     Arc<AgentService>,

    hosts_model:  RwLock<HashMap<HostId, Host>>,    // relays excluded
    agents_model: RwLock<HashMap<AgentId, Agent>>,  // local + remote merged

    host_event_source:  EventSource<HostReachabilityEvent>,
    agent_event_source: EventSource<AgentChangeEvent>,

    remote_agent_subs: RwLock<HashMap<HostId, AbortHandle>>,
}

enum HostReachabilityEvent {
    HostAdded(Host),
    HostRemoved(HostId),
}

enum AgentChangeEvent {
    AgentUp(Agent),
    AgentDown(AgentId),
    AgentUpdated(Agent),
}
```

#### 4.4.4 Population

At construction (before the daemon binds the client-facing listener):

1. Subscribe to `routing.subscribe_hosts()` (logical reachability stream;
   see §5.4). Spawn a handler task.
2. Subscribe to `agents.subscribe_agents()` (in-process). Spawn a handler
   task.

The handler task for **`routing.subscribe_hosts()`**:

- On `HostAdded(host)`:
  - Filter: if `host.capabilities.supported_agent_types` is empty
    (relay), drop the event entirely.
  - Otherwise: insert into `hosts_model`; emit
    `HostReachabilityEvent::HostAdded(host)` to `host_event_source`;
    spawn a remote-agent subscription task for this host (see below);
    store its `AbortHandle` in `remote_agent_subs[host_id]`.
- On `HostRemoved(host_id)`:
  - If `hosts_model` doesn't contain it (was a relay), do nothing.
  - Otherwise: abort the entry in `remote_agent_subs[host_id]`; remove
    from `hosts_model`; for every agent in `agents_model` with
    `host_id == host_id`, remove it and emit
    `AgentChangeEvent::AgentDown(agent_id)`; emit
    `HostReachabilityEvent::HostRemoved(host_id)`.

The handler task for **`agents.subscribe_agents()`** (local agents):

- On `AgentUp(agent)`: insert into `agents_model`; emit
  `AgentChangeEvent::AgentUp(agent)`.
- On `AgentDown(agent_id)`: remove from `agents_model`; emit
  `AgentChangeEvent::AgentDown(agent_id)`.
- On `AgentUpdated(agent)`: replace entry; emit
  `AgentChangeEvent::AgentUpdated(agent)`.

A **remote-agent subscription task** (one per non-relay remote host):

1. Acquire a tonic `Channel` to host `X` via the tunnel pool.
2. Call `AgentService.SubscribeAgentEvents` on host `X`.
3. Consume the stream:
   - `AgentUp` / `AgentDown` / `AgentUpdated`: apply to `agents_model`
     and emit corresponding downstream events. Same shape as the local
     handler.
   - `SnapshotComplete`: ignored (or used for diagnostics).
4. If the stream errors before the task is aborted: mark every agent
   with `host_id == X` as `AgentDown` (in `agents_model` and downstream
   events); re-open the subscription. Don't terminate the task — the
   accompanying `HostRemoved` would have aborted it explicitly.

#### 4.4.5 Method dispatch

For every agent-targeted method (`RenameAgent`, `DeleteAgent`,
`SubscribeSession`, `SendInput`), the impl first resolves `AgentRef`:

- `AgentRef.agent_id`: look up by id in `agents_model`. Not found ⇒
  `NOT_FOUND`.
- `AgentRef.name`: scan `agents_model` for matching names. Zero ⇒
  `NOT_FOUND`. Two or more ⇒ `AmbiguousAgentName`.

After resolution, the agent's `host_id` determines dispatch:

```rust
if agent.host_id == self.my_host_id {
    // In-process call into local AgentService.
    self.agents.<method>(...).await
} else {
    // Remote call via tonic Channel over tunnel.
    let channel = self.user_state.tunnel_pool.channel_to(agent.host_id).await?;
    AgentServiceClient::new(channel).<method>(...).await
}
```

For `CreateAgent`, no `AgentRef` resolution — but `host_id` defaulting
applies (`req.host_id.unwrap_or(self.my_host_id)`), and dispatch follows
the same local/remote split.

#### 4.4.6 `SubscribeSession` and `SendInput` — no multiplexing layer

Each `SubscribeSession` call from a client corresponds 1:1 to an upstream
subscription against the agent's host:

- **Local agent**: direct in-process subscription against
  `AgentService`'s local agent session machinery. Each subscriber gets
  its own `MultiplexBuffer` reader.
- **Remote agent**: a fresh `AgentService.SubscribeSession` gRPC call
  on the agent's host, over a tunnel. Each subscriber gets a separate
  upstream call. All upstream calls to the same remote host share the
  same tonic Channel / tunnel / HTTP/2 connection (gRPC multiplexes
  the streams).

There is no application-level multiplexing across client subscribers.
The agent's owning host is where `MultiplexBuffer` fan-out happens; this
host stays the same whether subscribers are local or remote.

`args`, `SessionInput.payload`, and `SessionControl.payload` are opaque
to ClientService. For local dispatch, they pass through to the local
`AgentService` method (which dispatches per-io_protocol). For remote
dispatch, they ride opaque through the gRPC call to the remote
`AgentService`. The agent's owning host is the sole authority on
interpretation, including resolution policies for things like terminal
size when multiple subscribers send different sizes (the PTY-owning
host's existing logic applies, unchanged).

#### 4.4.7 List* / Subscribe* implementations

`ListHosts` reads `hosts_model.values()`. `ListAgents` reads
`agents_model.values()`. Both return what's currently in the model;
no I/O.

`SubscribeHosts` and `SubscribeAgents` are mid-life subscribers (the
client connected after the model was populated), so they use the
snapshot-then-deltas pattern against the ClientService's own event
sources:

```rust
async fn subscribe_hosts(&self, _req) -> Stream<SubscribeHostsResponse> {
    let (snapshot, rx) = self.host_event_source.subscribe_with_snapshot(
        || self.hosts_model.read().values().cloned().collect()
    );
    // Emit HostAdded(host) for each entry in snapshot
    //   → SnapshotComplete
    //   → live events from rx
}
```

#### 4.4.8 Subscription lifecycle

The gRPC stream IS the subscription's lifetime:

- Client cancels the downstream stream (e.g. CLI exits) → tonic on the
  daemon propagates cancellation to any upstream `AgentService.
  SubscribeSession` call → upstream handler exits → upstream
  `MultiplexBuffer` reader is dropped.
- Tunnel breaks (`HostRemoved` on the agent's route) → upstream gRPC
  stream errors with `UNAVAILABLE` → ClientService closes the
  downstream stream with the same status.

No application-level "subscription closed" message or ack. No
per-`(agent_id, io_protocol)` dedup on the subscription side.

### 4.5 Internal service architecture

The daemon is structured as a small set of in-process "services" — Rust
structs that own state and expose methods + event subscriptions. The
gRPC service traits (from the generated proto code) are implemented on
these structs as thin shims; in-process callers invoke the same methods
directly.

#### 4.5.1 The pattern

Every service is a struct of the form:

```rust
struct SomeService {
    state: RwLock<State>,
    events: EventSource<SomeEvent>,
}

impl SomeService {
    // Methods that do work. gRPC handlers call these; in-process
    // callers (other services on this host) also call these.
    pub async fn some_operation(&self, ...) -> Result<...> { ... }

    // Subscribe primitive: deltas only, for in-process subscribers
    // registered at startup.
    pub fn subscribe_events(&self) -> EventReceiver<SomeEvent>;

    // Subscribe with snapshot: atomic capture of state + subscription,
    // for subscribers arriving mid-life (typically gRPC handlers
    // serving subscribe RPCs to remote peers).
    pub fn subscribe_events_with_snapshot(
        &self,
    ) -> (Snapshot, EventReceiver<SomeEvent>);
}
```

`ServerUserState` holds `Arc<SomeService>` for each top-level service.
Services that need to call into each other hold `Arc` references to
those they depend on.

#### 4.5.2 Snapshot atomicity

When `subscribe_events_with_snapshot()` is called:

1. Acquire the relevant lock on `state`.
2. Build the snapshot from the current state.
3. Register the new subscriber's sender into the event source.
4. Release the lock.

This ensures the snapshot read and the start of the delta stream are
atomic: no event can fire between them. Events that arrive after step
4 are queued behind the snapshot in the subscriber's receive buffer.

In-process subscribers that registered at startup don't need this —
they called `subscribe_events()` before any event could fire, so the
state was empty at the moment of registration.

#### 4.5.3 Backpressure

Each subscriber's receiver is a bounded mpsc. If the producer cannot
send because the receiver's queue is full:

- **In-process subscriber**: this is a programming error (model would
  become inconsistent). Log loudly and disconnect the subscriber.
- **Network subscriber** (gRPC handler forwarding to a remote peer):
  close the underlying gRPC stream with `RESOURCE_EXHAUSTED`. The peer
  will reconnect and re-snapshot.

#### 4.5.4 Startup ordering

The daemon constructs top-level services in topological order: a service
must be constructed after the services it depends on. ClientService
depends on RoutingService and AgentService.

After construction, ClientService **immediately** subscribes to
RoutingService and AgentService — before the daemon binds any external
listener (Unix socket for clients, TCP for cloud handshake). This
guarantees ClientService's subscriptions are in place before any event
can fire from those services (RoutingService events require a peer
connection; AgentService events require a local `CreateAgent` call).

#### 4.5.5 What's NOT a service

- gRPC client-side stubs (e.g. `AgentServiceClient` for calling remote
  hosts): per-call objects built over tonic Channels.
- Tunnels, the tunnel pool, link writer tasks: routing machinery, not
  services. State changes flow through `RoutingService` events.
- The CLI: a separate process that talks to ClientService over gRPC.

### 4.6 AgentService

The host-local agent service. Manages the lifecycle and I/O of agents
running on a single host. Reachable from other hosts via tunnels and
from `ClientService` in-process.

`AgentService` has no awareness of agents on other hosts — cross-host
inventory aggregation is `ClientService`'s job (§4.4). An
`AgentService` instance only ever emits `Agent` messages with its own
host's `host_id`.

#### 4.6.1 Service shape

```proto
service AgentService {
  rpc SubscribeAgentEvents(SubscribeAgentEventsRequest)
      returns (stream SubscribeAgentEventsResponse);

  rpc CreateAgent(CreateAgentRequest) returns (CreateAgentResponse);
  rpc RenameAgent(RenameAgentRequest) returns (RenameAgentResponse);
  rpc DeleteAgent(DeleteAgentRequest) returns (DeleteAgentResponse);

  rpc SubscribeSession(SubscribeSessionRequest)
      returns (stream SubscribeSessionResponse);
  rpc SendInput(SendInputRequest) returns (SendInputResponse);
}
```

Diffs from today's surface:

- `ListAgents` removed. `SubscribeAgentEvents` covers all callers via
  the snapshot-then-deltas pattern (initial `AgentUp` burst followed by
  `SnapshotComplete`). Add back as unary if a future caller needs it.
- `ResolveAgent` removed. Name resolution happens in `ClientService`
  against the aggregated inventory (§4.4) — only it knows the full
  cross-host name table. All `AgentService` methods are `agent_id`-keyed.
- `AgentUpdated` added to the event stream so rename / mutable-field
  changes propagate without an `AgentDown`+`AgentUp` pair.
- `host_id` filter removed from `SubscribeAgentEventsRequest`:
  AgentService is per-host by construction; the tunnel determines which
  host you're talking to.
- `AgentEntry { Agent, Route }` is gone. Routes don't appear in the
  agent-service surface anymore; the route to a host is `RoutingService`
  state.
- `SessionClosed` event added to `SubscribeSessionResponse` so
  attached clients learn that the agent was deleted (or otherwise
  ended) before the stream closes.

Everything else — the `Agent` shape, `CreateAgentRequest`'s
agent-type oneof, the opaque `Session*` envelopes, `SessionInput` /
`SessionControl` split, the first-party io_protocol payloads — is
preserved.

#### 4.6.2 Agent and inventory events

```proto
message Agent {
  bytes  agent_id           = 1;
  bytes  host_id            = 2;
  optional string name      = 3;
  string command            = 4;
  string working_dir        = 5;
  string agent_type         = 6;    // "claude" | "test_agent" | ...
  repeated string io_protocols = 7; // io_protocols this agent supports
  bool   readonly           = 8;    // advisory; see §4.6.6
  repeated string args      = 9;
  int64  created_at_unix_ms = 10;
}

message SubscribeAgentEventsRequest {}

message SubscribeAgentEventsResponse {
  oneof event {
    AgentUp           agent_up          = 10;
    AgentUpdated      agent_updated     = 11;
    AgentDown         agent_down        = 12;

    SnapshotComplete  snapshot_complete = 100;
  }
}

message AgentUp      { Agent agent = 1; }
message AgentUpdated { Agent agent = 1; }            // full replacement
message AgentDown {
  bytes agent_id = 1;
  optional string reason = 2;
}

message SnapshotComplete {}
```

The stream is snapshot-then-deltas: on `SubscribeAgentEvents`, the
service emits one `AgentUp` per currently-known agent, then
`SnapshotComplete`, then deltas. Subscribers MUST NOT assume the model
is complete until `SnapshotComplete` arrives.

`AgentUpdated` carries the full new `Agent`; subscribers replace by
`agent_id`.

#### 4.6.3 Lifecycle

```proto
message CreateAgentRequest {
  bytes agent_id = 1;             // client-generated UUID
  optional string name = 2;

  oneof agent {
    claude.ClaudeCreateConfig         claude     = 10;
    test_agent.TestAgentCreateConfig  test_agent = 100;
  }
}

message CreateAgentResponse { Agent agent = 1; }

message RenameAgentRequest  { bytes agent_id = 1; string name = 2; }
message RenameAgentResponse { Agent agent = 1; }

message DeleteAgentRequest  { bytes agent_id = 1; }
message DeleteAgentResponse {}
```

Configs for first-party agent types live in their own proto files
(§4.6.7). The `oneof` in `CreateAgentRequest` references them. A host
that doesn't support an agent type rejects the corresponding variant
with `UNIMPLEMENTED`.

`RenameAgentRequest.name` is non-optional and non-empty. Once an agent
has a name it keeps a name; clearing is not supported.

**Operation ↔ event ordering.** Lifecycle operations MUST push the
corresponding inventory event (`AgentUp` / `AgentUpdated` /
`AgentDown`) to all current subscribers' event queues before the unary
response returns. There is no cross-stream synchronization: a
subscriber MAY observe the event before or after it observes the
unary response on its other RPC. This matches today's behaviour
(`broadcast_topology_event` is called inside the write lock that
guards the lifecycle change, before the unary returns).

This means an in-process caller like `ClientService` cannot assume its
aggregated model has been updated by the time `create_agent` returns;
it should splice the returned `Agent` into its own model directly, and
the eventual `AgentUp` from the subscription path is a no-op merge.

#### 4.6.4 Name uniqueness

Names are unique **per host**. `CreateAgent` and `RenameAgent` reject
with `ALREADY_EXISTS` if the chosen name is already in use by another
agent on the same host. Two hosts can each have an agent named
`review` — `ClientService` is responsible for surfacing the ambiguity
to clients (e.g. via `AmbiguousAgentName` when an unqualified name
matches in two places).

#### 4.6.5 Sessions

The session I/O surface is preserved verbatim from today's protocol,
with one addition (`SessionClosed`):

```proto
message SubscribeSessionRequest {
  bytes  agent_id    = 1;
  string io_protocol = 2;
  optional bytes args = 3;             // opaque; io_protocol-defined
}

message SubscribeSessionResponse {
  oneof event {
    SessionOpened   opened          = 1;
    SessionOutput   output          = 2;
    ReplayComplete  replay_complete = 3;
    SessionClosed   closed          = 4;     // [NEW]
  }
}

message SendInputRequest {
  bytes  agent_id    = 1;
  string io_protocol = 2;
  oneof event {
    SessionInput   input   = 10;
    SessionControl control = 11;
  }
}
message SendInputResponse {}

message SessionOpened  {}
message SessionOutput  { bytes payload = 1; }
message SessionInput   {
  bytes input_id = 1;        // client correlation id
  bytes payload  = 2;        // io_protocol-defined
}
message SessionControl { bytes payload = 1; }  // resize/focus/etc.
message ReplayComplete {}

message SessionClosed {
  oneof reason {
    AgentDeleted    agent_deleted    = 1;
    AgentExited     agent_exited     = 2;
    HostUnreachable host_unreachable = 3;
    InternalError   internal_error   = 4;
  }
}
message AgentDeleted    {}
message AgentExited     { optional int32 exit_code = 1; }
message HostUnreachable {}
message InternalError   { string detail = 1; }
```

`AgentService` treats `args`, `SessionInput.payload`, and
`SessionControl.payload` as opaque. Each value of `io_protocol` defines
their schema (§4.6.7).

`SessionClosed` is emitted as the last event before the stream ends
cleanly with `OK`. The `reason` oneof carries a structured cause so
subscribers can branch on it without string-matching:

- **`AgentDeleted`** — `DeleteAgent` was called while a subscriber was
  attached.
- **`AgentExited`** — the underlying process exited on its own. Carries
  `exit_code` when known.
- **`HostUnreachable`** — emitted *only* by `ClientService`, never by
  `AgentService`. Synthesized when ClientService's upstream
  `AgentService.SubscribeSession` stream errors with `UNAVAILABLE` due
  to tunnel teardown (see C-10). Distinguishes "remote host vanished"
  from "graceful agent end" and tells clients that retrying later is
  reasonable.
- **`InternalError`** — server-side bug or unexpected state. The
  `detail` string is for human consumption (logs, error messages).

`AgentService` therefore emits at most three of the four reasons
(`AgentDeleted`, `AgentExited`, `InternalError`). `ClientService` may
emit any of the four — it passes upstream reasons through verbatim and
synthesizes `HostUnreachable` itself.

A `SessionClosed` event always means the stream ends with gRPC status
`OK`. Streams that close with a non-`OK` status (network failure
between client and ClientService, ClientService crash) don't carry a
`SessionClosed`; clients distinguish those from the in-band reasons by
the status code.

Each `SubscribeSession` is an independent upstream subscription — there
is no application-level multiplexing across subscribers. ClientService
preserves this 1:1 mapping (C-5).

#### 4.6.6 Readonly agents

`readonly: bool` on `Agent` is **advisory metadata** for clients. It
indicates the agent cannot accept input (e.g. for rendering input UI
as disabled). `AgentService` does not introspect it — the actual
rejection of input happens inside the io_protocol-specific handler
when it tries to process the payload.

Readonly agents arise from the hook bootstrap path: Claude's
`SessionStart` hook can fire for a Claude process that was started
outside amux. The amux daemon has no PTY handle for that process, only
the transcript file to tail. The hook handler constructs a readonly
`Agent`, registers it in `AgentService`, and emits the corresponding
`AgentUp`. There is no public API to create a readonly agent;
`CreateAgentRequest` has no `readonly` field.

For Claude io_protocols, attempting to send input to a readonly agent
produces an io_protocol-level error (today: `ProtocolError::ServerError
{ message: "session is readonly" }`; in gRPC terms this surfaces as
`FAILED_PRECONDITION`). io_protocols that don't have a readonly
concept (e.g. `test_echo_v1`) never see readonly agents.

When the external Claude session ends (`SessionEnd` hook), the readonly
agent is withdrawn — `AgentDown` is emitted and any attached
`SubscribeSession` streams receive `SessionClosed` before terminating.

#### 4.6.7 Proto file layout

```
crates/amux/proto/amux/v1/
├── amux.proto         # core protocol (services, Agent, Session* envelopes)
├── claude.proto       # claude io_protocol — create config + session payloads
└── test_agent.proto   # test_agent — create config only
```

**`amux.proto`** is io_protocol-agnostic. It defines `RoutingService`,
`AgentService`, `ClientService`, the `Agent` message, the inventory
events, the `Session*` envelopes (opaque `bytes` for payloads). It
imports `claude.proto` and `test_agent.proto` to reference their
create configs in `CreateAgentRequest`'s oneof.

**`claude.proto`** holds Claude-specific shapes — both the creation
config (`ClaudeCreateConfig`, `ClaudePtyRuntime`, `ClaudeSdkRuntime`,
`TerminalSize`) and the session-layer payloads:

- `ClaudeRawV1Args`, `ClaudeRawV1Control`, `ClaudeRawV1ReplayQuery` —
  opaque PTY bytes; resize via control.
- `ClaudePtyTranscriptV1Args`, `…ReplayQuery`, `…Cursor`, `…Input`,
  `…Action`, `…Output` — structured transcript stream with sequence
  ids.

`claude.proto` does not import `amux.proto`. Its types are the *inner*
shapes that get serialized into `AgentService`'s opaque `bytes` fields
(`args`, `SessionInput.payload`, etc.); the dependency direction is
strictly `amux.proto → claude.proto`.

**`test_agent.proto`** contains `TestAgentCreateConfig`. `test_echo_v1`
has no args/control payload structure, so no message types are needed
for it.

This layout makes the extension boundary visible in the file system:
a future third-party io_protocol ships its own `.proto` alongside the
existing ones and is referenced from `CreateAgentRequest`'s oneof. The
core protocol file does not change.

#### 4.6.8 Error codes

| Operation | Condition | Status |
|---|---|---|
| any (`agent_id`-keyed) | unknown `agent_id` | `NOT_FOUND` |
| `CreateAgent` | duplicate `agent_id` | `ALREADY_EXISTS` |
| `CreateAgent` / `RenameAgent` | name in use on this host | `ALREADY_EXISTS` |
| `CreateAgent` | unsupported agent-type variant (e.g. `test_agent` in production) | `UNIMPLEMENTED` |
| `SubscribeSession` / `SendInput` | `io_protocol` not supported by this agent | `INVALID_ARGUMENT` |

io_protocol-level failures (e.g. readonly-rejected input) surface
through the same gRPC status mechanism but their mapping is defined
by the io_protocol, not `AgentService`.

---

## 5. Routing service

The protocol spoken between adjacent routing parties (host↔host or
host↔relay). Establishes the link, propagates routing events, and carries
tunnel frames. Clients never speak this protocol.

### 5.1 Service shape

```proto
service RoutingService {
  rpc Connect(stream Message) returns (stream Message);
}
```

A single bidirectional streaming RPC. **The stream is the link.** Opening
it (after successful auth + handshake) brings the link up; closing it
tears the link down. After the handshake completes, both sides freely
send and receive `Message`s in either direction.

Auth: JWT in gRPC metadata. A `tonic` interceptor validates it before the
handler runs; failure terminates the stream with `UNAUTHENTICATED` before
any application bytes flow. The interceptor extracts `user_id` from the
JWT and attaches it to the request extensions; the handler reads it to
look up or create `ServerUserState`.

### 5.2 Message envelope

```proto
message Message {
  oneof body {
    Hello         hello         = 1;
    HelloAck      hello_ack     = 2;
    RoutingEvent  routing_event = 3;
    TunnelFrame   tunnel_frame  = 4;
    Reauth        reauth        = 5;
    ReauthAck     reauth_ack    = 6;
    GoAway        goaway        = 7;
  }
}
```

State rules:

- The first `Message` in each direction MUST be `hello` (connector) or
  `hello_ack` (acceptor).
- `hello` / `hello_ack` are illegal post-handshake.
- All other variants are illegal pre-handshake.
- Receiving any illegal variant is a protocol violation (see §5.7).

### 5.3 Handshake

```proto
message Hello {
  repeated uint32 supported_protocol_versions = 1;
  string proposed_link_name = 2;
  Host host = 3;
}

message HelloAck {
  oneof outcome {
    HelloAccepted accepted = 1;
    Error         error    = 2;
  }
}

message HelloAccepted {
  uint32 protocol_version   = 1;
  string assigned_link_name = 2;
  Host   host               = 3;
}

message Host {
  bytes host_id = 1;
  string name = 2;
  string version = 3;
  Capabilities capabilities = 4;
}
```

Flow:

1. Connector opens the gRPC stream (auth in metadata).
2. Server interceptor validates JWT. On failure, stream ends with
   `UNAUTHENTICATED` before any `Message` is read.
3. Connector sends `Message { hello: Hello { ... } }`. Sends nothing
   further until it has read `HelloAck`.
4. Acceptor validates the `Hello` (protocol-version overlap, link-name
   uniqueness, etc.). On success, sends `Message { hello_ack: HelloAck
   { accepted: ... } }`. On failure, sends `Message { hello_ack:
   HelloAck { error: ... } }` and closes its send half.
5. On `accepted`, the link is up. On `error` or any stream-close before
   `accepted`, the link never establishes; the connector MAY retry with
   backoff.

After the handshake, each side knows the other's `Host` (id, name,
version, capabilities). The acceptor does NOT subsequently announce the
connector back to the connector via `HostUp`, nor itself; identity
exchange is the job of the handshake, and `HostUp` is the job of
propagating *other* hosts' identities.

Rejection shape: every pre-handshake failure (wrong first message,
version mismatch, bad link name, malformed payload, ...) is uniformly
delivered as `HelloAck { error: Error { code, message, ... } }` followed
by stream close.

### 5.4 Routing events

#### 5.4.1 Storage policy: first-route only

The routing core stores **at most one route per host_id** — the first
announced one. Subsequent `HostUp` events for an already-known host are
dropped: no storage update, no event emitted, no propagation. A
`HostDown` only takes effect if it matches the currently-stored route.

This is a simplification over today's behavior (which stores all routes
and supports fallback). It means: every host has at most one current
route; when that route's `HostDown` arrives, the host becomes
unreachable with no fallback. Adding multi-route storage later is
additive — more entries in the table, and a `HostRouteChanged` event
variant in the logical event stream.

#### 5.4.2 Two event flavours

The routing core exposes two subscribe primitives:

```rust
impl RoutingService {
    /// Raw protocol events. Each accepted HostUp/HostDown (one per
    /// stored host transition) fires here. Consumed by Connect handlers
    /// to forward to peers.
    pub fn subscribe_routing_events(&self)
        -> EventReceiver<RoutingEvent>;

    pub fn subscribe_routing_events_with_snapshot(&self)
        -> (Vec<RoutingEvent>, EventReceiver<RoutingEvent>);

    /// Logical reachability transitions. Consumed by in-process
    /// model holders (ClientService).
    pub fn subscribe_hosts(&self)
        -> EventReceiver<HostReachabilityEvent>;

    pub fn subscribe_hosts_with_snapshot(&self)
        -> (Vec<Host>, EventReceiver<HostReachabilityEvent>);
}

enum RoutingEvent {
    HostUp   { host: Host, route: Route, origin_link: Option<LinkId> },
    HostDown { host_id: HostId, route: Route, origin_link: Option<LinkId> },
}

enum HostReachabilityEvent {
    HostAdded   { host: Host },
    HostRemoved { host_id: HostId },
    // Future: HostRouteChanged when multi-route is added.
}
```

`origin_link` is the link the event was learned from (`None` if learned
from a direct-peer handshake). Connect handlers use it for loop
prevention — when forwarding to peer P, skip events where
`origin_link == link(P)`. **`origin_link` is hop-local and NEVER appears
on the wire.** The Connect handler strips it when translating
`RoutingEvent` into wire-level `HostUp` / `HostDown`.

With first-route-only storage, the two streams fire 1:1 (a storage
change is always a reachability transition). The names differ because
the *vocabulary* differs: the raw stream matches protocol vocabulary
(matches wire HostUp/HostDown); the logical stream matches model
vocabulary. Future extensions (multi-route fallback, `HostRouteChanged`)
would diverge the two streams; the API distinction prepares for that.

#### 5.4.3 Wire shape

After handshake, each side begins streaming `RoutingEvent`s to the
other: a snapshot of every host currently in this party's routing core
(`HostUp` events, one per stored host), then `SnapshotComplete`, then
deltas (`HostUp` / `HostDown`).

```proto
message RoutingEvent {
  oneof event {
    HostUp           host_up           = 1;
    HostDown         host_down         = 2;
    SnapshotComplete snapshot_complete = 3;
  }
}

message HostUp {
  Host host = 1;
  Route route = 2;
}

message HostDown {
  bytes host_id = 1;
  Route route = 2;
  optional string reason = 3;
}

message SnapshotComplete {}

message Route {
  repeated string links = 1;
}
```

Routes on the wire are **hop-relative**: the announcer's own route to
the host being announced. The receiver MUST prepend the link the event
arrived on before storing, producing the receiver's local route to that
host. When re-announcing to other peers, the announcer uses its own
stored route (still hop-relative, but from its perspective).

Loop prevention: per the `origin_link` mechanism above. Receivers
MUST NOT echo a `HostUp` back along the link it was learned from, nor
announce a host's own `Host` to that host.

#### 5.4.4 Relay write ordering

Each routing party maintains a single ordered writer per outgoing link
(a per-link mpsc plus writer task). `HostUp(X)` is enqueued onto that
writer at the moment the routing core stores X. Any `TunnelFrame` with
`initiator = X` arriving for forwarding is enqueued strictly later
(it can only arrive after X has connected and learned of the target).
This guarantees by construction that on a given link, `HostUp(X)`
precedes any `TunnelFrame` with `initiator = X` — no per-frame check
needed at any hop.

### 5.5 Tunnel frames

```proto
message TunnelFrame {
  Route    dst       = 1;
  TunnelId tunnel_id = 2;
  bytes    payload   = 3;
}

message TunnelId {
  bytes initiator = 1;
  bytes target    = 2;
}
```

Forwarding:

- Receive a `TunnelFrame` on link L.
- If `dst` is non-empty: pop `dst.links[0]` — it must name one of this
  host's outgoing links. Send the frame (with the popped `dst`) out
  that link. Drop silently if the link is gone.
- If `dst` is empty: this host is the endpoint. Validate
  `tunnel_id.target == my_host_id` (else: protocol violation). Look up
  the local tunnel keyed by the full `TunnelId`; deliver `payload`.
  If no such tunnel exists and `target == my_host_id`, create a
  target-side tunnel (see §6.2).

`TunnelId` is set by the initiator at tunnel creation and is NEVER
modified at intermediate hops. `payload` is opaque to the routing layer
— it is inner-HTTP/2 bytes of the gRPC channel running over the tunnel.

No `src` route is carried: responses are routed using the receiver's
own routing table (looking up the peer `host_id` to find a current
route). No `call_id`, no RPC framing — that lives inside the inner gRPC
stream.

Tunnel disruption is signalled via `HostRemoved`: when the route the
tunnel is using is torn down (matching `HostDown`), the tunnel is torn
down at each endpoint. The inner gRPC channel sees its transport break
and errors all in-flight streams. No separate `RoutingError` mechanism.

### 5.6 Reauth

Long-lived links outlive single JWTs. `Reauth` extends a live link
without reconnecting, preserving tunnels and inner-RPC state.

```proto
message Reauth {
  string auth_token = 1;
}

message ReauthAck {
  oneof outcome {
    Empty accepted = 1;
    Error error    = 2;
  }
}
```

Connector behavior: the `CredentialProvider` refreshes the JWT proactively
(when within a small grace window of expiry). On obtaining a new token,
the connector sends `Reauth` over the live stream.

Acceptor behavior: maintain an expiry timer scheduled slightly before
the current token's `exp`. On `Reauth` arrival, validate the new token.
It MUST resolve to the same `user_id` as the original; if not, respond
`ReauthAck { error: { code: UNAUTHENTICATED } }` and let the timer fire.
On success, respond `ReauthAck { accepted }` and reset the timer to the
new token's `exp`.

If the timer fires without successful `Reauth`, the acceptor sends
`GoAway { reason: AUTH_EXPIRED, drain_timeout_ms }` and closes the link
after the drain window.

### 5.7 GoAway

Graceful link shutdown with a typed reason and a drain budget.

```proto
message GoAway {
  GoAwayReason reason = 1;
  optional Error error = 2;
  uint32 drain_timeout_ms = 3;
}

enum GoAwayReason {
  GO_AWAY_REASON_UNSPECIFIED     = 0;
  GO_AWAY_REASON_USER_SHUTDOWN   = 1;
  GO_AWAY_REASON_UPDATING        = 2;
  GO_AWAY_REASON_SUSPENDING      = 3;
  GO_AWAY_REASON_RESTARTING      = 4;
  GO_AWAY_REASON_AUTH_EXPIRED    = 5;
  GO_AWAY_REASON_PROTOCOL_ERROR  = 6;
  GO_AWAY_REASON_UPDATE_REQUIRED = 7;
}
```

Sender of `GoAway`: stops initiating new things on the link, continues
servicing in-flight inner-tunnel traffic, closes the stream after
`drain_timeout_ms` (or sooner if the receiver closes first).

Receiver of `GoAway`: same posture — stops opening new tunnels via this
link, lets in-flight inner-RPC drain, signals downstream consumers (the
Client Server fans out "host going away" notifications via its hosts
model so attached clients can react).

`drain_timeout_ms = 0` + `reason = PROTOCOL_ERROR` is the "abort, no
drain" case for protocol violations.

---

## 6. Tunnel object

The wire-level `TunnelFrame` (§5.5) is the protocol. This section
specifies the in-process tunnel object that sits between the routing
core and gRPC: what types exist, how they're created, how they're torn
down, and how they're integrated with `tonic`.

### 6.1 Roles

A tunnel has two endpoints:

- **Initiator** — the host whose `host_id == TunnelId.initiator`. This
  side is the **gRPC client** on the inner channel.
- **Target** — the host whose `host_id == TunnelId.target`. This side is
  the **gRPC server** on the inner channel.

Role determination is purely from `TunnelId`. No handshake. No
configuration step.

### 6.2 Lazy creation

**Initiator-side creation.** When some component on host A wants to
call an RPC on peer B's service, it asks the tunnel pool:

```rust
async fn channel_to(&self, peer: HostId) -> Result<tonic::Channel, Error>;
```

The implementation:

1. If a cached `tonic::Channel` exists for `(initiator=me, target=peer)`,
   return it.
2. Else, look up `peer` in `ServerUserState.hosts`. Use its `route`.
3. Mint a fresh `TunnelId { initiator: my_host_id, target: peer }`.
4. Construct a `Tunnel` (the routing-side record) and a `TunnelTransport`
   (the gRPC-side handle).
5. Insert `Tunnel` into `ServerUserState.tunnels` keyed by `TunnelId`.
6. Build a `tonic::Channel` over the `TunnelTransport` (see §6.6). Cache
   it.
7. Return the cached `tonic::Channel`.

**Target-side creation.** When the routing core receives a `TunnelFrame`
addressed to this host (`dst` empty, `tunnel_id.target == my_host_id`)
and no tunnel exists in the registry for that `TunnelId`:

1. Mint a `Tunnel` + `TunnelTransport` pair, with `route` looked up from
   `ServerUserState.hosts[tunnel_id.initiator].route`.
2. Insert `Tunnel` into the registry.
3. Push the `TunnelTransport` onto the tonic Server's incoming-stream
   (an `mpsc::Sender<TunnelTransport>` set up at server start, see §6.7).
4. Deliver the inbound `payload` to the `Tunnel` (which forwards it to
   the `TunnelTransport`'s read side).

### 6.3 In-process API

```rust
/// Handle held by tonic. Implements byte-stream traits + the tonic
/// transport hook. Created by tunnel creation paths in §6.2.
pub struct TunnelTransport {
    inner: tokio::io::DuplexStream,
    peer:  HostId,
}

impl tokio::io::AsyncRead  for TunnelTransport { /* delegate */ }
impl tokio::io::AsyncWrite for TunnelTransport { /* delegate */ }
impl tonic::transport::Connected for TunnelTransport {
    type ConnectInfo = TunnelConnectInfo;
    fn connect_info(&self) -> Self::ConnectInfo {
        TunnelConnectInfo { peer: self.peer }
    }
}

#[derive(Clone)]
pub struct TunnelConnectInfo { pub peer: HostId }

/// Internal record in ServerUserState.tunnels. Not exposed publicly.
struct Tunnel {
    id:           TunnelId,
    route:        Route,
    inbound_tx:   tokio::sync::mpsc::Sender<bytes::Bytes>,
    _reader_task: tokio::task::AbortHandle,
    _writer_task: tokio::task::AbortHandle,
}
```

A tunnel is constructed with a helper:

```rust
/// Create a tunnel pair. `outbound_link_tx` is the mpsc Sender for the
/// outgoing link's writer task (i.e. the first hop of `route`).
fn create_tunnel(
    id: TunnelId,
    route: Route,
    outbound_link_tx: mpsc::Sender<Message>,
) -> (Tunnel, TunnelTransport);
```

This:

1. Allocates `tokio::io::duplex(BUF_SIZE)` → `(grpc_half, routing_half)`.
2. Splits `routing_half` into `(routing_read, routing_write)`.
3. Spawns a **reader task** that loops:
   - Read bytes from `routing_read`.
   - On EOF or error: exit.
   - Else: wrap bytes in `TunnelFrame { dst: route.clone(), tunnel_id:
     id.clone(), payload }` and send to `outbound_link_tx`.
4. Spawns a **writer task** that loops:
   - `recv` from `inbound_rx`.
   - On channel-closed: exit.
   - Else: `routing_write.write_all(&payload)`. On write error: exit.
5. Returns `(Tunnel { id, route, inbound_tx, abort handles },
   TunnelTransport { inner: grpc_half, peer })`.

### 6.4 Buffer sizes

- `tokio::io::duplex` buffer: `BUF_SIZE = 64 KiB` per direction (tunable).
- `inbound_tx` channel depth: `INBOUND_DEPTH = 32` payloads (tunable).
- `outbound_link_tx` channel depth: managed by the link writer task.

Backpressure flows end-to-end naturally.

### 6.5 Routing-core integration

```rust
struct ServerUserState {
    user_id:             UserId,
    routing:             Arc<RoutingService>,
    agents:              Arc<AgentService>,
    client_service:      Arc<ClientService>,
    hosts:               HashMap<HostId, HostEntry>,
    tunnels:             HashMap<TunnelId, Tunnel>,
    client_channels:     HashMap<HostId, tonic::Channel>,
    incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    // ...
}

struct HostEntry {
    host:  Host,
    route: Route,
}
```

On inbound `TunnelFrame` to this host (`dst` empty, `tunnel_id.target
== my_host_id`):

```rust
if let Some(tunnel) = state.tunnels.get(&tunnel_id) {
    tunnel.inbound_tx.send(payload).await.ok();
} else {
    let route = state.hosts[&tunnel_id.initiator].route.clone();
    let outbound_link_tx = link_table[&route.links[0]].outgoing_tx();
    let (tunnel, transport) =
        create_tunnel(tunnel_id.clone(), route, outbound_link_tx);
    state.tunnels.insert(tunnel_id.clone(), tunnel);
    state.incoming_tunnels_tx.send(transport).await.ok();
    state.tunnels[&tunnel_id].inbound_tx.send(payload).await.ok();
}
```

On `HostRemoved(host_id)` from RoutingService's logical event stream:

```rust
state.hosts.remove(&host_id);
state.client_channels.remove(&host_id);
state.tunnels.retain(|tid, _| tid.initiator != host_id && tid.target != host_id);
```

Dropping a `Tunnel` aborts its tasks and drops its half of the duplex;
the `TunnelTransport` held by tonic (whether by a Channel or by the
Server's accepted connection) sees EOF and errors out the inner gRPC.

### 6.6 Initiator-side tonic Channel setup

```rust
use tower::service_fn;
use tonic::transport::Endpoint;

let transport: TunnelTransport = /* from create_tunnel */;
let transport_cell = std::sync::Mutex::new(Some(transport));

let channel = Endpoint::from_static("http://tunnel")
    .connect_with_connector(service_fn(move |_uri| {
        let t = transport_cell.lock().unwrap().take()
            .expect("TunnelTransport already consumed");
        async move { Ok::<_, std::io::Error>(t) }
    }))
    .await?;
```

`"http://tunnel"` is a dummy `:authority` for HTTP/2 headers; the real
transport is the `TunnelTransport`. The connector is called once when
tonic establishes the connection.

### 6.7 Target-side tonic Server setup

Each amux server starts one tonic Server with all host↔host services
(`AgentService`, etc.) registered. Setup:

```rust
let (incoming_tx, incoming_rx) = mpsc::channel::<TunnelTransport>(64);
// Store incoming_tx in ServerUserState.incoming_tunnels_tx for the
// routing core to push new target-side tunnels.

let incoming = tokio_stream::wrappers::ReceiverStream::new(incoming_rx)
    .map(Ok::<_, std::io::Error>);

tokio::spawn(
    tonic::transport::Server::builder()
        .add_service(AgentServiceServer::new(...))
        // ... other host↔host services
        .serve_with_incoming(incoming),
);
```

Whenever the routing core creates a new target-side tunnel, the resulting
`TunnelTransport` is pushed onto `incoming_tx`. tonic accepts it as a new
HTTP/2 connection and dispatches requests as they arrive.

### 6.8 Keepalive

Configure tonic keepalive at both layers:

- **Outer link** (the `RoutingService.Connect` stream between routing
  parties): set keepalive on the tonic Server / Channel used for the
  routing service.
- **Inner tunnel** (the gRPC channel over the tunnel): set keepalive on
  the tonic Server (§6.7) and on each Channel built in §6.6.

Concrete knobs: `http2_keepalive_interval`, `http2_keepalive_timeout`,
`keep_alive_while_idle`. Tunable; not protocol.

---

## 7. User journeys

A small set of worked examples that exercise the locked spec. Each
journey cites the invariants it relies on. The set is intentionally
narrow — most other flows (local client connect, list agents, simple
session attach) follow trivially from §4–§6 and aren't worth
re-narrating; see §7.6 for those.

### 7.1 Server startup (with a cloud relay configured)

1. **CLI checks for a running daemon.** The CLI dials the configured
   local Unix socket. If a daemon is already serving, the CLI proceeds
   as a normal client. Otherwise it spawns one.
2. **Daemon spawn.** The CLI forks the daemon binary, detaches it, and
   waits on a startup channel for readiness.
3. **Daemon initialization (in order):**
   - Load config; generate a fresh `host_id` (UUID).
   - Construct **`AgentService`** (local agent registry, empty).
   - Construct **`RoutingService`** (hosts table, empty).
   - Construct **`ClientService`** (`Arc<RoutingService>`,
     `Arc<AgentService>`).
   - ClientService **immediately subscribes** to
     `routing.subscribe_hosts()` and `agents.subscribe_agents()`, before
     any external listener is bound.
   - Start the host↔host tonic Server with `serve_with_incoming(...)`
     (§6.7) for `AgentService` and any other host↔host services.
   - Bind the local Unix socket tonic Server exposing `ClientService`.
4. **Daemon signals ready.** Clients can use the daemon fully from this
   point — local agent operations, listing, attaching — regardless of
   whether the cloud link is established.
5. **Cloud connection (background task).** Concurrent with step 4, the
   daemon dials the configured cloud relay over TCP + TLS, fetches an
   access token via `CredentialProvider`, and opens a `RoutingService.
   Connect` stream with the JWT in metadata.
6. **Handshake.** Per §5.3. The daemon's `Hello` carries its `Host` (id,
   name, version, capabilities); the cloud's `HelloAck` carries the
   relay's `Host`.
7. **Routing exchange begins.** Both sides start streaming
   `RoutingEvent`s (snapshot then deltas). On A's side, the routing
   core processes incoming `HostUp` events and emits on both
   `subscribe_routing_events` (for the cloud-side Connect handler to
   forward, with origin_link filtering) and `subscribe_hosts` (which
   feeds ClientService's `hosts_model`).
8. **Steady state.** The cloud link is established, the hosts view
   populates over time, and tunnels are constructed lazily on first
   host-to-host traffic.

### 7.2 A tunnel is established between two hosts

Setup: hosts A and B are both connected to relay R. R has already
propagated `HostUp(B)` to A and `HostUp(A)` to B (per §5.4). Neither
side has yet exchanged any tunnel traffic. A's `ClientService` has
just observed `HostAdded(B)` and (per C-8) wants to open
`AgentService.SubscribeAgentEvents` against B.

1. **Initiator-side tunnel lookup.** A's `ClientService` asks
   `tunnel_pool.channel_to(B)`. The pool keys by `TunnelId { A, B }`
   (T-3). No existing tunnel — create one.
2. **HostEntry lookup.** The pool reads `RoutingService`'s
   `HostEntry(B)` to get B's route (e.g., `[link_A_to_R]`). If B
   were not yet known, the pool would return `NOT_FOUND` per T-10,
   but R's `HostUp(B)` precedes any tunnel-frame initiated by A on R's
   inbound link by T-9, so this isn't a race in practice — it only
   matters when B has *gone away* and a stale dispatch arrives.
3. **Tunnel construction.** A constructs a `Tunnel` with
   `id = TunnelId { A, B }`, `route = [link_A_to_R]`, and an internal
   `tokio::io::duplex` pair (§6.2). A `TunnelTransport` wraps one
   half of the duplex and implements
   `AsyncRead + AsyncWrite + Connected`.
4. **tonic Channel.** A builds a tonic `Channel` over the
   `TunnelTransport` (§6.6). All subsequent gRPC calls to B reuse
   this Channel; multiple gRPC streams multiplex over the single
   tunnel as HTTP/2 streams.
5. **First RPC.** A's `AgentServiceClient(channel)
   .SubscribeAgentEvents(...)` begins. tonic writes the HTTP/2
   preface, SETTINGS, and request headers to the duplex.
6. **Outbound framing on A.** A's tunnel writer task reads bytes from
   the duplex and wraps each chunk as
   `TunnelFrame { tunnel_id: TunnelId { A, B }, payload }`. The
   routing core's ordered writer for `link_A_to_R` enqueues the
   frame onto the link. (T-1: `TunnelId` is set once and never
   modified at intermediate hops.)
7. **Relay forwarding.** R receives the `TunnelFrame`.
   `tunnel_id.target == B`, not R — so R does not deserialize the
   payload; it looks up B's route and forwards the frame on
   `link_R_to_B`. R's writer for that link previously emitted
   `HostUp(A)` when storing A (T-9), so B already knows about A by
   the time this frame arrives.
8. **Target-side tunnel lazy creation.** B receives the
   `TunnelFrame`. `tunnel_id.target == B` matches B's `host_id`. B's
   tunnel registry has no entry for `TunnelId { A, B }`. Per T-5, B
   constructs a fresh `Tunnel` (route inferred from the inbound
   link), and pushes its `TunnelTransport` onto the tonic Server's
   `serve_with_incoming(...)` stream (§6.7).
9. **gRPC handshake completes on B.** B's tonic Server picks up the
   new transport, consumes the HTTP/2 preface/SETTINGS/headers
   already buffered in the duplex, dispatches to
   `AgentService.SubscribeAgentEvents`.
10. **Response path.** `AgentService` starts streaming `AgentUp`
    events, then `SnapshotComplete`, then deltas. Each gRPC stream
    frame becomes bytes in B's tunnel duplex, gets wrapped as
    `TunnelFrame { tunnel_id: TunnelId { A, B }, payload }` (same
    `TunnelId` regardless of direction — T-1), and routed back
    through R to A.
11. **Steady state.** The tunnel persists until B's route is
    revoked (T-7 — `HostRemoved(B)` tears it down). Subsequent
    gRPC calls from A to B reuse the same Channel/Tunnel.

### 7.3 A client subscribes to a remote agent

Setup: A's `ClientService` has populated `agents_model` for B's
agents (via the journey in §7.2). A client connected over the local
Unix socket calls `ClientService.SubscribeSession(AgentRef { name:
"reviewer" }, io_protocol: "claude_raw_v1", args: ...)`.

1. **Name resolution.** ClientService resolves `AgentRef` against
   `agents_model` (C-2). One match for "reviewer" on host B. Yields
   `(host_id: B, agent_id: X)`. Zero matches → `NOT_FOUND`; more
   than one → `AmbiguousAgentName`.
2. **Dispatch decision.** `host_id != my_host_id`, so this is a
   remote agent. ClientService asks `tunnel_pool.channel_to(B)`.
   The tunnel from §7.2 already exists; the pool returns the
   existing Channel.
3. **New gRPC stream on the tunnel.** ClientService opens a fresh
   `AgentServiceClient(channel).SubscribeSession(...)` call. tonic
   allocates a new HTTP/2 stream within the existing connection —
   the routing layer and tunnel layer don't need to know; gRPC and
   HTTP/2 multiplex this for free.
4. **Upstream handler runs on B.** B's `AgentService.SubscribeSession`
   finds agent X, opens a session subscription via the io_protocol
   handler. The handler emits `SessionOpened`, replays from the
   bounded retained buffer (per `replay_query` inside opaque
   `args` — A-11), emits `ReplayComplete`, then live `SessionOutput`
   frames.
5. **ClientService proxies.** Each upstream `SubscribeSessionResponse`
   frame is forwarded verbatim to the client's downstream stream.
   ClientService doesn't introspect `payload` (C-4). Per C-5 this is
   a 1:1 pairing — no multiplex layer, no mirror buffer.
6. **Input.** When the client calls `ClientService.SendInput(...)`,
   ClientService dispatches it as a unary
   `AgentServiceClient(channel).SendInput(...)` over the same
   tunnel. Independent gRPC call; runs in parallel with the
   `SubscribeSession` stream.
7. **Liveness.** Per C-7, the gRPC stream IS the subscription. If
   the client cancels, ClientService propagates cancellation
   upstream; B's handler exits.

### 7.4 A remote host goes away mid-session

Setup: a client is mid-session with an agent on host B (continuing
from §7.3). A's link to relay R is healthy. R's link to B drops
(B crashed, network partition, B's process exited).

1. **R observes the drop.** R's `RoutingService` sees the
   `RoutingService.Connect` stream to B error. R's routing core
   removes `HostEntry(B)`. The route matches by R-2; R emits
   `HostRemoved(B)` on its `subscribe_hosts` flavour and raw
   `HostDown(B)` on `subscribe_routing_events`.
2. **R forwards `HostDown(B)` to A.** R's Connect handler for
   `link_R_to_A`, subscribed to raw routing events, picks up
   `HostDown(B)` and writes it onto the link.
3. **A observes `HostDown(B)`.** A's routing core processes the
   event. Route matches stored route (R-2). Remove `HostEntry(B)`.
   Emit `HostRemoved(B)` on `subscribe_hosts`.
4. **Tunnel teardown.** A's `tunnel_pool` is subscribed to
   `subscribe_hosts`. On `HostRemoved(B)` it tears down
   `Tunnel { TunnelId { A, B } }` (T-7). The tunnel's duplex closes;
   tonic's Channel sees its transport break and errors *all*
   in-flight gRPC streams on it with `UNAVAILABLE`.
5. **Two cleanup paths in ClientService.**
   - The upstream `SubscribeAgentEvents` stream for B errors with
     `UNAVAILABLE`. The stream's handler task exits. Per C-9 this
     error is *observed but not acted on* — it doesn't drive
     `agents_model` state.
   - The upstream `SubscribeSession` stream errors with
     `UNAVAILABLE`. Per C-10, ClientService synthesizes
     `SessionClosed { host_unreachable }` as the final event on
     the downstream client-facing stream, then closes that stream
     with status `OK`.
6. **Model cleanup driven by routing.** ClientService's
   `subscribe_hosts` consumer observes `HostRemoved(B)`. Per C-9,
   it bulk-removes all `agents_model` entries with `host_id == B`
   and emits `AgentDown { agent_id }` for each on every active
   `SubscribeAgents` downstream stream. (The two cleanup signals —
   upstream stream error and `HostRemoved` — converge on the same
   state but only `HostRemoved` is authoritative.)
7. **Client observation.** The client sees:
   - On its `SubscribeSession` stream: a final `SessionClosed`
     event with `reason = host_unreachable`, then end-of-stream
     with `OK`.
   - On its `SubscribeAgents` stream (if subscribed): `AgentDown`
     for each previously-known agent on B, then the stream
     continues normally with other hosts' events.
8. **No automatic recovery.** Per C-11, ClientService does not
   preserve session subscriptions across host transitions. If the
   client wants to re-attach when B reappears, that's the client's
   responsibility — they re-issue `SubscribeSession`.

### 7.5 A relay restarts

Setup: host A is connected to relay R. R hosts many other hosts
(B, C, D, …) for the same user. R's process restarts (stateless —
no in-memory state survives).

1. **A's link to R breaks.** A's `RoutingService.Connect` stream
   errors when R's TCP connection drops.
2. **Cascade.** A's routing core walks its `HostEntry` table for
   every host whose route starts with `link_A_to_R` — i.e., every
   remote host A knows. For each, it emits `HostDown` matched by
   R-2 and emits `HostRemoved` on `subscribe_hosts`.
3. **Cascade consequences.** For each removed host B':
   - T-7 tears down `Tunnel { TunnelId { A, B' } }`.
   - C-9 bulk-removes B'`s agents from `agents_model` and emits
     `AgentDown` for each on downstream `SubscribeAgents` streams.
   - C-10 synthesizes `SessionClosed { host_unreachable }` on any
     active `SubscribeSession` stream targeting an agent on B'.
   - Net effect from the client's POV: this looks like §7.4
     happening in parallel for every remote host.
4. **A starts reconnecting.** A's daemon retries the TCP+TLS dial
   to R with exponential backoff. Implementation detail (not
   protocol).
5. **R has restarted.** R accepts the new connection. Interceptor
   validates A's JWT (I-11); claims attached to extensions; Connect
   handler reads them, sets the auth timer.
6. **Handshake.** A sends `Hello`. R assigns a new (possibly
   different) link name and sends `HelloAck { accepted }`. Per
   I-12, if A's `host_id` somehow collided with another live host
   in R's `ServerUserState[user]`, R would reject with
   `HOST_ID_COLLISION` — but A's `host_id` is unique for this
   daemon's lifetime (I-1), so in practice this doesn't fire on
   reconnect.
7. **Snapshot exchange.** A sends a routing-event snapshot
   (currently empty — A knows about no other hosts post-cascade).
   R sends its snapshot to A (empty initially; populates as other
   hosts independently reconnect).
8. **Other hosts reconnect to R.** Each B', C', D' independently
   re-dials R. As R learns about each, R emits `HostUp(B')`,
   `HostUp(C')`, … on the link to A (with origin_link filtering
   per R-4 / I-10).
9. **A repopulates.** A's routing core processes each `HostUp`,
   stores the first-announced route per R-1, emits
   `HostAdded(B')` on `subscribe_hosts`. ClientService observes
   each `HostAdded` and (per C-8) opens
   `AgentService.SubscribeAgentEvents` against B' — a tunnel is
   constructed lazily on that first RPC, as in §7.2. The fresh
   subscription delivers a snapshot, then deltas; `agents_model`
   repopulates with B'`s agents.
10. **Clients observe recovery.** Clients see `HostAdded` events
    arrive, followed by `AgentUp` events as each remote host's
    inventory replays. Existing session streams that ended in step
    3 stay ended — clients re-call `SubscribeSession` if they want
    to re-attach (C-11).
11. **Steady state.** A is back in the routing topology with all
    reachable hosts and their agents.

### 7.6 Trivial cases

These flows don't earn standalone narratives — they follow directly
from the locked spec:

- **Acceptor-side handshake.** Mirror image of §7.1 steps 5–7 from
  the relay's perspective. JWT in metadata is interceptor-validated
  (I-11); on success, the `Connect` handler reads claims, validates
  `Hello`, replies `HelloAck { accepted }`. Failure modes:
  `UNAUTHENTICATED` from the interceptor (no `HelloAck` emitted);
  `HOST_ID_COLLISION` per I-12; protocol-version / link-name
  conflicts per §5.3.
- **Host discovery through a relay.** Pure narration of §5.4 + R-1
  + I-9 + I-10. R receives `HostUp(A)` from A; stores; propagates
  along all other links (not back to A). Other hosts receive,
  prepend their inbound link to the route per I-9, store per R-1
  if first-announced.
- **Local client connects to its host.** Unix socket gRPC dial. No
  handshake (clients aren't hosts). ClientService accepts the
  connection. Per I-4, this works regardless of cloud-link state.
- **A client lists agents across the network.** ClientService
  serves `ListAgents` / `SubscribeAgents` directly from its
  `agents_model`, which is populated by §4.4.4: in-process
  subscription to the local `AgentService` plus per-remote-host
  `AgentService.SubscribeAgentEvents` opened on each `HostAdded`
  (C-8).
- **A client subscribes to a local agent.** §7.3 with the dispatch
  branch taking the in-process path: ClientService calls
  `AgentService.SubscribeSession` directly (S-2) without a tunnel.
- **Multiple clients subscribe to the same agent.** Each call is an
  independent upstream subscription per C-5/C-6. No mirror buffer;
  no application-level dedup. The host's io_protocol handler
  serves each subscriber's `replay_query` independently from the
  same retained buffer (A-11).
- **A client disconnects.** Per C-7, the gRPC stream IS the
  subscription. tonic detects the disconnect; ClientService
  propagates cancellation to any upstream calls; in-process or
  remote `AgentService` handlers exit cleanly. No application-level
  close messages.

---

## 8. Invariants

Each invariant should be implementable and testable.

### Identity and durability

- **I-1.** A daemon's `host_id` is unique and stable for the daemon's
  lifetime, generated at startup. `host_id`s are NOT preserved across
  restarts — a restarted daemon is, from the network's perspective, a
  new host.
- **I-2.** `user_id` is established at handshake from the JWT and is
  bound to the link for its lifetime. A `Reauth` MUST resolve to the
  same `user_id`; if not, the acceptor rejects the `Reauth`.
- **I-3.** The routing layer prevents cross-tenant tunneling: a relay
  never forms tunnels between hosts in different users'
  `ServerUserState`s.
- **I-11.** Initial JWT validation happens in the gRPC interceptor,
  before any `Message` is read; failure closes the stream with
  `UNAUTHENTICATED`. Parsed claims (user_id, exp) are attached to the
  request and consumed by the `Connect` handler, which owns the link's
  auth lifetime: it sets a timer for the token's `exp`, handles
  `Reauth` (with `ReauthAck`) per §5.6, and emits `GoAway` if the
  timer fires without a successful reauth. `HelloAck { error }` is
  reserved for application-level handshake failures (unsupported
  protocol version, link-name conflict, host_id collision).

### Local operation

- **I-4.** ClientService is operational before the cloud link is
  attempted. Local CLI commands MUST succeed against a freshly-spawned
  daemon even if the cloud is unreachable.

### Routing-service framing

- **I-5.** Exactly one `RoutingService.Connect` stream exists per
  established link. The stream is the link.
- **I-6.** The first `Message` from the connector MUST be `hello`; the
  first from the acceptor MUST be `hello_ack`.
- **I-7.** `hello` and `hello_ack` are illegal post-handshake. All other
  variants are illegal pre-handshake. Violations end the link with
  `GoAway(PROTOCOL_ERROR, drain_timeout_ms: 0)` post-handshake, or
  `HelloAck { error }` pre-handshake.
- **I-12.** On accept, if `Hello.host.host_id` matches an
  already-live host within the same `ServerUserState`, the acceptor
  rejects the new connection with
  `HelloAck { error: HOST_ID_COLLISION }` and closes the stream. The
  first established link wins; subsequent collisions cannot displace
  it. Scope is per-`ServerUserState` — different users may legitimately
  hold the same `host_id` without colliding.

### Routing semantics

- **I-8.** Link names are symmetric: both ends of a single link agree
  on the same name (assigned by the acceptor at handshake).
- **I-9.** Routes on the wire are hop-relative to the announcer.
  Receivers MUST prepend the incoming link before storing or
  re-announcing.
- **I-10.** A `HostUp` MUST NOT be echoed back along the link it was
  learned from. A host MUST NOT be announced as `HostUp` along the
  link to the announced host itself.

### Routing storage (R-*)

- **R-1.** The routing core stores at most one route per `host_id`
  (the first-announced). Subsequent `HostUp` for an already-known host
  are ignored — no storage, no event on either flavour, no
  propagation.
- **R-2.** A `HostDown` only takes effect (storage removal, events,
  propagation) if its `route` matches the currently-stored route.
- **R-3.** `RoutingService` exposes two event flavours:
  `subscribe_routing_events` (raw, protocol-shaped, carrying
  `origin_link`) and `subscribe_hosts` (logical reachability
  transitions). With first-route-only storage, both fire 1:1.
- **R-4.** `origin_link` is hop-local metadata and never appears on
  the wire. Connect handlers strip it when translating `RoutingEvent`
  into wire-level `HostUp` / `HostDown`.

### Tunnel framing (T-*)

- **T-1.** A `TunnelId` is `(initiator_host_id, target_host_id)`. Set
  by the initiator at tunnel-creation time and NEVER modified at
  intermediate hops.
- **T-2.** A `TunnelFrame` arriving at an endpoint with
  `tunnel_id.target != my_host_id` is a protocol violation.
- **T-3.** Each endpoint's tunnel registry is keyed by the full
  `TunnelId`. Up to **two** tunnels can exist between any pair of
  hosts (one per `initiator`).
- **T-4.** Role determination is derived from `TunnelId`:
  `my_host_id == initiator` ⇒ this side is the gRPC client;
  `my_host_id == target` ⇒ this side is the gRPC server.

### Tunnel lifecycle

- **T-5.** Tunnels are created lazily.
  - Initiator: on first outbound RPC to a peer.
  - Target: on first inbound `TunnelFrame` with an unknown `TunnelId`
    where `tunnel_id.target == my_host_id`. The new tunnel's
    `TunnelTransport` is pushed onto the tonic Server's
    `serve_with_incoming` stream.
- **T-6.** A tunnel's `route` is set at creation from the peer's
  current `HostEntry.route`. The route is NOT updated mid-tunnel;
  if the route changes, the tunnel must die and (eventually) be
  recreated.
- **T-7.** A tunnel is torn down when its route's `HostRemoved` event
  fires. Inner gRPC sees its transport break and errors all
  in-flight streams. In-flight outbound bytes in the duplex are
  dropped.
- **T-8.** Dropping either half of the tunnel's byte channel closes
  the duplex; the other half sees EOF; routing tasks exit cleanly.
  Tunnels are NOT GC'd on idle.
- **T-10.** `tunnel_pool.channel_to(B)` MUST return `NOT_FOUND`
  synchronously when `RoutingService` has no `HostEntry` for B at call
  time. The pool does NOT wait or retry; callers handle the error.

### Routing-side ordering

- **T-9.** Each routing party maintains a single ordered writer per
  outgoing link. `HostUp(X)` is enqueued onto that writer at the
  moment the routing core stores X. Any `TunnelFrame` with
  `initiator = X` routed onto that link is enqueued strictly later.
  This guarantees by construction that on a given link, `HostUp(X)`
  precedes any `TunnelFrame` with `initiator = X`.

### AgentService (A-*)

- **A-1.** `agent_id` is a globally unique UUID. `(host_id, agent_id)`
  is redundant; either alone identifies the agent.
- **A-2.** An `AgentService` instance manages agents only on its own
  host. Cross-host inventory aggregation is `ClientService`'s
  responsibility; `AgentService` has no awareness of agents on other
  hosts.
- **A-3.** `SubscribeAgentEvents` emits one `AgentUp` per
  currently-known agent, then `SnapshotComplete`, then deltas.
  Subscribers MUST NOT assume the model is complete until
  `SnapshotComplete` arrives.
- **A-4.** `AgentUpdated` carries the full new `Agent`; subscribers
  replace by `agent_id`.
- **A-5.** `CreateAgent` / `RenameAgent` / `DeleteAgent` MUST push
  the corresponding inventory event (`AgentUp` / `AgentUpdated` /
  `AgentDown`) to all current subscribers' event queues before the
  unary response returns. There is no cross-stream ordering guarantee
  between the unary response and other subscriptions on the same
  client.
- **A-6.** Agent names are unique per host. `CreateAgent` and
  `RenameAgent` reject duplicates with `ALREADY_EXISTS`. Two hosts
  MAY each have an agent with the same name; disambiguation is
  `ClientService`'s problem.
- **A-7.** `readonly: bool` on `Agent` is advisory metadata.
  Enforcement is the responsibility of the io_protocol-specific input
  handler on the host; `AgentService` does not introspect it.
- **A-8.** Readonly agents are created only via the internal hook
  bootstrap path (Claude `SessionStart` for an externally-started
  Claude process), never via `AgentService.CreateAgent`.
  `CreateAgentRequest` has no `readonly` field.
- **A-9.** `AgentService` treats `args`, `SessionInput.payload`, and
  `SessionControl.payload` as opaque. The `io_protocol` field selects
  the schema; the io_protocol's handler on the host is the sole
  authority on their interpretation.
- **A-10.** A `SubscribeSession` stream that ends because the agent
  was deleted, exited, or hit an internal error MUST emit a
  `SessionClosed` event with the corresponding `reason` as the last
  frame before the stream closes with `OK`. `AgentService` MUST NOT
  emit `HostUnreachable` — that variant is exclusive to
  `ClientService` (C-10).
- **A-11.** Each `(agent_id, io_protocol)` pair on a host owns a
  bounded retained buffer of session output. The buffer's shape,
  replay-query vocabulary, and bound are defined by the io_protocol;
  `AgentService` MUST NOT introspect any of it. New subscribers'
  `replay_query` is interpreted entirely by the io_protocol handler.
- **A-12.** A `SubscribeSession` subscription that falls behind its
  bounded per-subscriber delivery queue MUST be closed with
  `RESOURCE_EXHAUSTED`. Resumption is the subscriber's responsibility
  via a new `SubscribeSession` call with an appropriate `replay_query`.

### ClientService (C-*)

- **C-1.** `ClientService` is the only gRPC service exposed to
  clients. `AgentService` and `RoutingService` are host↔host services
  and are never reachable by a client connection.
- **C-2.** A `name` in `AgentRef` resolves against `agents_model`.
  Zero matches → `NOT_FOUND`; more than one → `AmbiguousAgentName`.
  Resolution happens at the start of every method that accepts
  `AgentRef`.
- **C-3.** `ClientService.ListHosts` / `SubscribeHosts` exclude hosts
  whose `capabilities.supported_agent_types` is empty (relays). The
  routing core continues to track them.
- **C-4.** `ClientService` treats `args`, `SessionInput.payload`, and
  `SessionControl.payload` as opaque. For remote agents the bytes
  forward verbatim to the agent's host; the agent's host is the sole
  authority on their interpretation.
- **C-5.** Each `ClientService.SubscribeSession` corresponds to
  **exactly one** upstream subscription against the agent's host —
  local in-process for local agents, `AgentService.SubscribeSession`
  over a tunnel for remote agents. No application-level multiplexing
  across client subscribers; no mirror MultiplexBuffer.
- **C-6.** Multiple concurrent `SubscribeSession` calls for the same
  `(agent_id, io_protocol)` are allowed — from the same client
  connection or across clients. Each is independent.
- **C-7.** Session-subscription liveness IS the gRPC stream's
  liveness. Client cancels the downstream stream → ClientService
  propagates cancellation upstream → upstream handler exits. Tunnel
  breaks → upstream stream errors → ClientService closes the
  downstream stream. No application-level "subscription closed"
  message or ack.
- **C-8.** Every `HostAdded(B)` event from `RoutingService` triggers
  ClientService to open (or re-open) `AgentService.SubscribeAgentEvents`
  against B via the tunnel pool. The trigger is unconditional — there
  is no distinction between "first time seeing B" and "B returned after
  a previous departure." A fresh snapshot followed by deltas always
  arrives; ClientService reconciles against `agents_model`.
- **C-9.** Every `HostRemoved(B)` event from `RoutingService` triggers
  ClientService to remove all `agents_model` entries with
  `host_id == B` and emit `AgentDown` for each on its outgoing
  `SubscribeAgents` streams. The upstream `SubscribeAgentEvents`
  handler is NOT responsible for synthesizing per-agent teardown — its
  stream error is observed but does not drive `agents_model` state.
- **C-10.** When ClientService's upstream
  `AgentService.SubscribeSession` stream errors with `UNAVAILABLE` due
  to tunnel teardown, ClientService MUST emit
  `SessionClosed { host_unreachable }` as the last event on the
  downstream stream and close with status `OK`. ClientService is the
  sole source of the `HostUnreachable` reason.
- **C-11.** ClientService does not preserve user-initiated session
  subscriptions across host transitions. Once `SessionClosed` is
  emitted, the subscription is dead. Recovery is the client's
  responsibility — clients re-call `SubscribeSession` if they want to
  re-attach.

### Internal service architecture (S-*)

- **S-1.** Each top-level service struct (`RoutingService`,
  `AgentService`, `ClientService`) owns its state and exposes (a)
  methods for in-process callers and gRPC handlers, (b) subscribe
  primitive(s) for event consumption.
- **S-2.** gRPC handlers are thin shims over service methods.
  In-process callers (e.g. `ClientService.create_agent` calling
  `AgentService.create_agent` for a local agent) invoke the same
  methods directly — no in-process gRPC, no serialization.
- **S-3.** In-process subscribers register at daemon startup,
  immediately after service construction and before any external
  listener is bound. They consume the deltas-only subscribe
  primitive; no snapshot needed.
- **S-4.** Snapshots exist only at network boundaries — gRPC handlers
  serving subscribe RPCs to remote peers use
  `subscribe_*_with_snapshot()`.
- **S-5.** Snapshot-then-subscribe is atomic: state lock held briefly
  to capture snapshot AND register subscriber together; deltas after
  release naturally queue behind the snapshot in the receiver.
- **S-6.** A subscriber whose receive queue fills is disconnected.
  For in-process subscribers, this is a fatal model-corruption
  signal; for network subscribers, the gRPC stream is closed with
  `RESOURCE_EXHAUSTED`.

---

## 9. Deferred / forward-compatibility notes

These were earlier open questions; all have been resolved for v1. The
notes are kept so a future implementor can see *why* a given seam
exists.

- **Remote clients — local-only for v1.** `ClientService` listens on a
  Unix socket plus an in-process duplex transport (for embedded
  clients within the same process — admin UIs, integration tests).
  No TCP+TLS listener for remote `amux-ui`-style clients. If that
  changes later, the host↔host auth model (JWT in initial gRPC
  metadata, validated by an interceptor) is the obvious template;
  `ClientService` would gain a parallel listener, not a new service.
- **`ConnectToServer` stays on `ClientService`.** Useful as an admin
  RPC for runtime add-a-relay flows. Not load-bearing on anything
  else; remove only if a future configuration model fully obsoletes
  it.
- **Multi-route fallback — out of scope for v1.** R-1 (first-route
  only) is the locked policy. The §5.4.2 two-flavour event API is a
  forward-compatibility seam: a future multi-route world would
  diverge the raw `subscribe_routing_events` stream (per-route
  events, including a new `HostRouteChanged` variant) from the
  logical `subscribe_hosts` stream (reachability transitions only).
  Adding multi-route later requires multi-route storage and a
  route-selection policy; nothing in v1 makes that hostile.
- **AgentService re-snapshot on reconnect — resolved by C-8/C-9.** A
  remote `AgentEvents` subscription errors with `UNAVAILABLE` only
  when the tunnel breaks, which is simultaneous with
  `HostRemoved(B)`. C-9 bulk-removes B's agents from `agents_model`.
  A later `HostAdded(B)` triggers a fresh `SubscribeAgentEvents` per
  C-8; ClientService starts from a clean slate for B. No
  reconciliation step required.

---

## 10. Target file structure

The implementation should land at this layout. Domain primitives at top
level; services are thin tonic shims over those primitives.

```
crates/amux/src/
├── lib.rs                  # Public API + re-exports
├── config.rs               # Daemon config (parse + defaults)
├── state.rs                # Persistent state (refresh tokens)
├── client.rs               # Library API: connect() → ClientServiceClient
├── server.rs               # Daemon entrypoint: startup, listener binding
├── user_state.rs           # ServerUserState (per-user container)
│
├── protocol/               # Proto include + central conversion error
│   ├── mod.rs              # pub mod proto { tonic::include_proto!("amux.v1"); }
│   └── error.rs            # ConvertError + impl Into<tonic::Status>
│
├── services/               # The three gRPC service impls (thin tonic shims)
│   ├── mod.rs
│   ├── routing.rs          # RoutingService (Connect handler)
│   ├── agent.rs            # AgentService
│   └── client.rs           # ClientService (aggregation + dispatch)
│
├── routing/                # Routing primitives used by RoutingService
│   ├── mod.rs
│   ├── core.rs             # hosts table, links table, storage policy (R-1, R-2)
│   ├── types.rs            # HostId, LinkName, Route, Host, HostEntry + conversions
│   ├── events.rs           # RoutingEvent, HostEvent + EventSources
│   └── link.rs             # Per-link runtime state (writer mpsc, auth timer)
│
├── tunnel/                 # Tunnel primitives
│   ├── mod.rs              # Tunnel struct, lifecycle
│   ├── types.rs            # TunnelId, TunnelFrame domain types + conversions
│   ├── transport.rs        # TunnelTransport (AsyncRead + AsyncWrite + Connected)
│   └── pool.rs             # TunnelPool
│
├── agents/                 # Agent implementations
│   ├── mod.rs              # AgentSession enum + dispatch
│   ├── types.rs            # AgentId, Agent, AgentRef, SessionCloseReason + conversions
│   ├── events.rs           # AgentEvent + EventSource
│   ├── buffer.rs           # Per-session bounded retained buffer (A-11)
│   ├── pty.rs              # PtyHandle + spawning
│   ├── claude/
│   │   ├── mod.rs
│   │   ├── session.rs      # ClaudeSession (PTY + SDK runtimes)
│   │   ├── io.rs           # claude_raw_v1 + claude_pty_transcript_v1 handlers
│   │   └── hook.rs         # Claude hook processing (SessionStart bootstrap)
│   └── test_agent.rs
│
├── auth/
│   ├── mod.rs
│   ├── jwt.rs              # Tonic interceptor (I-11)
│   ├── claims.rs           # Claims + conversion from validated JWT
│   ├── oauth.rs            # OAuth device flow
│   └── credentials.rs      # CredentialProvider
│
└── transport/              # Listener / dial helpers for tonic
    ├── mod.rs
    ├── unix.rs
    ├── tcp.rs              # TCP + TLS via rustls
    └── memory.rs           # In-process duplex
```

### 10.1 Organising principles

- **Domain primitives at top level.** `routing/`, `tunnel/`, `agents/`,
  `auth/`, `transport/` each own a coherent domain. They expose
  ergonomic Rust types and have no gRPC trait impls themselves.
- **Services are integration shims.** `services/routing.rs` is a small
  tonic-trait impl over `routing::Core`. Same for agent / client.
  This keeps the S-1 separation explicit: state + methods in the
  domain module, wire serving in the service module.
- **Conversions co-locate with domain types.** `routing/types.rs`
  defines `Host` and `impl From<proto::Host> for Host` next to each
  other. No central `protocol/wire/` dump.
- **`protocol/` is two files only.** The proto include and a central
  `ConvertError` enum used by every conversion that can fail.
- **`server/` is gone.** `server.rs` (top-level) covers daemon
  startup; per-link runtime moves into `routing/link.rs` and the
  `RoutingService.Connect` handler.

### 10.2 What goes away from today

| Today | Replaced by |
|---|---|
| `rpc.rs` (custom call-id lifecycle) | tonic |
| `server/dispatch.rs` (frame dispatch) | tonic-generated service traits |
| `server/accept.rs` (handshake-on-accept) | tonic accept + `auth/jwt.rs` interceptor |
| `server/connection.rs` (link reader/writer tasks) | `services/routing.rs` + `routing/link.rs` |
| `server/cloud.rs` (cloud dial) | `server.rs` startup |
| `transport/framing.rs` | HTTP/2 framing |
| `transport/websocket.rs` | dropped entirely |
| `protocol/agent_lifecycle.rs` and similar | prost-generated types |
| Most of `protocol/wire/` | inline `From` impls in domain modules |
| `client/connection.rs` + `client/rpc.rs` | tonic `Channel` + generated client stubs |

### 10.3 What stays (largely unchanged)

- `agents/claude/` and `agents/claude/io.rs` — the io_protocol
  handlers, retained-buffer logic, hook processing.
- `agents/pty.rs` — PTY spawning helpers.
- `auth/oauth.rs`, `auth/credentials.rs` — OAuth device flow and
  credential storage.
- `transport/unix.rs`, `transport/tcp.rs` — listener and dial
  helpers (significantly reduced; no framing logic).
- `config.rs`, `state.rs` — daemon config and persistent state.

The implementation should treat this layout as the destination, not as
a starting hypothesis. New code added during the refactor should land
in its target location even if the surrounding scaffolding hasn't been
migrated yet.
