# amux Cloud Architecture

This document describes the cloud deployment architecture for amux. For internal server design, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Overall Architecture

![Global Architecture](images/global_architecture.png)

amux is a federated network of servers that enable remote access to AI agent sessions. While the initial implementation targets Claude, the system is designed to support any terminal-based AI agent (Codex, OpenCode, AMP, etc.).

---

## Cloud Design

- Cloud runs a pool of vanilla amux servers (no special cloud-specific logic in amux itself)
- amux servers are **stateless** and run in **token-required mode**
- A separate **application server** handles:
  - User login/authentication
  - The `/connect` endpoint (not part of amux)
  - Token signing
  - Push notifications
- Server allocation is a **hash of user information modulo number of servers** (ensures consistent routing without state)
- Allocation responses include a **TTL**; when expired, clients MUST re-query and reconnect if the target host changed
- TTL-based reallocation enables **gradual rebalancing** when servers are added/removed, avoiding thundering herd

### Token-based Authentication

The application server issues signed tokens that:
- Prove the user is authenticated (identity)
- Encode `user_id`, `expiry`, and resource quotas (e.g., `max_agents`)
- Are verified by amux servers (signature check, no shared state needed)

This provides DOS protection: no valid token = connection rejected.

## Connection Protocol

When connecting via cloud, the client first authenticates with the application server and calls `/connect`. The response contains:
- Target host and port
- TTL
- Signed token (encodes user_id, expiry, quotas)
- Encryption mode

On TTL expiry, the client MUST re-query. If the host has changed, the client MUST drop the existing connection and connect to the new host.

## Transport Layers

| Client Type | Transport | Characteristics |
|-------------|-----------|-----------------|
| Terminal clients | Unix socket | Framed messages, then raw bytes after subscribe |
| amux server → amux server | TCP with `TCP_NODELAY` | Framed messages (multiplexed) |
| Rich clients (mobile, web) | WebSocket | JSON messages (structured logs) |

**Serialization:** TCP/Unix use `bincode` (binary, compact). WebSocket uses `serde_json` (human readable).

## Local Network (Future)

V1 focuses on single-user local mode and cloud connectivity. The following are deferred:

- **Multi-user local networks**: Security considerations for multiple users on the same LAN
- **Broadcast discovery**: Automatic discovery of amux servers on the local network
- **Server-to-server LAN connections**: Direct connections between local servers without cloud relay

When implemented, security will likely use mutual HMAC challenge-response for key verification:

```
A (connector)                           B (connectee)
     |                                       |
     |────────── connect ───────────────────>|
     |                                       |
     |<─────── challenge: nonce_B ───────────|
     |                                       |
     |─── HMAC(key, "client" || nonce_B) ───>|
     |─── challenge: nonce_A ───────────────>|
     |                                       |
     |       [B verifies A's HMAC]           |
     |                                       |
     |<── HMAC(key, "server" || nonce_A) ────|
     |                                       |
     | [A verifies B's HMAC]                 |
     |                                       |
     |══════ mutually authenticated ═════════|
```

## Session Identity

Agent sessions are uniquely identified by a tuple:

```
(host_id, user_id, agent_id)
```

- **host_id**: UUID identifying the amux server instance (generated on first run, persisted)
- **user_id**: In cloud mode, extracted from token. In local mode, hardcoded (e.g., `"local"`)
- **agent_id**: Name of the agent session (e.g., `"claude-1"`)

## Session Propagation

- amux servers propagate all `(host_id, user_id, agent_id)` tuples they know about to connected amux servers
- Each amux server maintains a **routing table**: how to reach any given `(host_id, user_id, agent_id)` on the network

---

## API

### Server Modes

amux servers can run in two modes:

- **Token mode** (cloud): Requires a valid signed token. The `user_id` is extracted from the token.
- **Local mode**: No token required. A hardcoded `user_id` (e.g., `"local"`) is used for all connections.

Internal data models always include `user_id` for uniformity; local mode simply uses a constant value.

All agent data is multiplexed over a single connection (TCP or WebSocket).

### Connection Lifecycle

Connections must be **established** before any other API calls are allowed. The `establish_connection` call is an application-level message (the first message on the wire), uniform across all transports.

```
1. Open socket (Unix, TCP, or WebSocket)
2. Send establish_connection(token?) as first message
   - Token provided → validate signature, extract user_id
   - No token + local mode → use hardcoded user_id
   - No token + token mode → reject, close connection
3. Connection is now "established"
4. Other API calls are now permitted
5. Any API call before establish → close connection immediately
```

### Routing Table

Each amux server maintains an internal lookup table:

```
(host_id, user_id, agent_id) -> Route

where Route is:
  - Local                    // We own this agent's PTY
  - Remote { via: ConnectionId }  // Forward through this connection
```

This determines how to forward data for any given agent. See [ARCHITECTURE.md](ARCHITECTURE.md) for implementation details.

### Core Methods

#### Connection establishment (must be first)

##### `establish_connection(token?) -> ok | error`

Must be the first message on any connection. Establishes the connection's `user_id`:
- Token provided: validate and extract `user_id`
- No token (local mode): use hardcoded `user_id`

#### Server-level (no agent_id required)

These methods query or update the server's routing knowledge. The `user_id` is implicit from the established connection.

##### `list_agents() -> list[Agent]`

Query the server for all agents it knows about (scoped to connection's `user_id`). Returns a single payload.

```
Agent:
  host_id
  user_id
  agent_id
```

##### `add_agents(agents: list[Agent])`

Client notifies the server of agents it knows about (e.g., local agents, or agents learned from another server).

##### `disconnect()`

Signals the client is disconnecting. Server removes any agents associated with this connection from its routing table.

#### Agent-level (requires agent_id)

These methods operate on specific agents. The `user_id` is implicit from the established connection.

##### `subscribe(host_id, agent_id) -> History`

Subscribe to an agent's output stream. Returns history then streams live updates.

**History format differs by transport:**
- **WebSocket**: Past N structured log entries, then live logs
- **TCP**: Full replay buffer (for terminal UI correctness), then live bytes

##### `unsubscribe(host_id, agent_id)`

Stop receiving data for this agent. Conceptually, "detaching" is just an unsubscribe.

##### `send_message(host_id, agent_id, message)`

Send input to an agent. For terminal clients, `message` is raw bytes (keystrokes). For rich clients, `message` could be structured (e.g., user prompt text).

### Terminal Clients

Local terminal clients use the same API over Unix socket:
- Call `establish_connection()` with no token (local mode)
- Assign themselves a unique `host_id` on startup (UUID)
- Use `subscribe`/`unsubscribe`/`send_message` to interact with agents
- Skip `add_agents` (human uses CLI to specify which agent to attach to)

**Optimization:** After a successful `subscribe`, local Unix socket connections switch to "raw mode" - both sides exchange raw bytes without message framing. This minimizes latency for terminal I/O. See [ARCHITECTURE.md](ARCHITECTURE.md) for details.

---

## Design Notes

### Why This Is Simpler Than It Looks

The architecture avoids distributed systems complexity through deliberate constraints:

**Star topology through cloud:**
- Local servers connect TO cloud, not to each other (initially)
- Cloud server's "routing table" is simply "which connected client owns which agents"
- No server-to-server gossip, no mesh, no complex propagation protocols

**Clients are the source of truth:**
- Cloud servers are stateless routers, not databases
- Agent ownership lives on the local server running the agent
- On reconnect, clients re-advertise their agents - state is rebuilt, not recovered

**Hash routing contains blast radius:**
- All of a user's traffic (local servers, terminals, mobile) routes to the same cloud server
- A user's "world" is self-contained on one cloud server
- No cross-cloud routing needed for single-user access

**Failure handling is straightforward:**
1. Cloud server dies
2. Application server health-checks, stops routing to it
3. Clients reconnect (TTL expiry or connection drop)
4. Application server directs them to healthy server
5. Clients re-advertise agents, re-subscribe
6. Done - no complex recovery

**Deferred complexity:**
- Cross-user collaboration (user A accessing user B's agents) - not yet needed
- Server-to-server LAN connections without cloud - optimization for later
- Stale entry cleanup - simple heartbeat + periodic cleanup is sufficient

---

