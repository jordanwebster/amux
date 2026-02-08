# amux Development Log

This file tracks significant development work, decisions made, and current state. Update this file after completing a chunk of work.

---

## How to Maintain This Log

1. **Add new entries at the top** (reverse chronological order)
2. **Use the standard entry format** shown below
3. **Be concise but complete** - future you will thank present you
4. **Include verification results** - what was tested and how
5. **Document decisions and rationale** - not just what, but why

### Entry Template

```markdown
## YYYY-MM-DD: Brief Title

### Summary
One paragraph describing what was done.

### Changes
- List of files created/modified
- Key structural changes

### Decisions Made
- Decision 1: rationale
- Decision 2: rationale

### Verification
- What was tested
- Results

### Next Steps
- What remains to be done
```

---

## 2026-02-08: Architecture docs rewrite + config improvements

### Summary

Rewrote both architecture documents from scratch — the previous versions documented the original Milestone 1 design (flat `Message` enum, `ConnectionId(u64)`, bincode, raw mode) which no longer exists. Also added default config file loading from `~/.config/amux/config.yaml` and an `enforce_tls_in_cloud_mode` config parameter for reverse-proxy deployments.

### Decisions Made

- **Full rewrite over incremental patches**: Documents were so far from current codebase that patching would have been harder to review than starting fresh.
- **Default config failure is a warning (not fatal)**: The file may be partially written; falling back to defaults is safer. Explicit `--config` failure remains fatal.
- **`verify_token` decoupled from TLS**: Previously `verify_token` was `true` only when `tls_acceptor` was `Some`. Now derived from `is_cloud_server`, so cloud mode behind a reverse proxy still validates JWT tokens.

---

## 2026-02-06: Routable/Local message split + generic error forwarding

### Summary

Two major protocol changes. First, restructured the flat `Message` enum into `Message::Routable { src, dst, message }` and `Message::Local(LocalMessage)`. This collapses six separate forwarding arms into one generic forwarding path and encodes routing capability in the type system. Second, added `RoutableMessage::Error(ProtocolError)` so any routable message that can't be forwarded gets a typed error sent back via normal routing, with stale route cleanup.

### Decisions Made

- **Two-variant top-level enum**: `Routable`/`Local` cleanly captures the routing distinction while keeping the wire format simple. Breaking wire format is intentional — the old format mixed routing fields into individual variants.
- **AgentEnded stays Local**: Each server decides how to propagate end-of-session semantics to its own subscribers.
- **Amplification prevention**: If a `RoutableMessage::Error` itself fails to forward, it's logged and dropped rather than generating another error.
- **Stream message error suppression**: Output/StructuredOutput forwarding failures don't send routable errors back — high-frequency stream messages would cause churn without triggering teardown.
- **Conditional stale route cleanup**: When a channel send fails, check `is_closed()` before removing — a new connection may have already replaced the route.
- **Handshake link-name uniqueness**: Moved route insertion into `accept_handshake` so uniqueness check and insert happen atomically under one write lock.
- **Route leak prevention**: If ConnectResponse write fails after route insertion, the stale route is cleaned up before returning the error.
- **Lock hygiene**: Restructured to avoid holding write locks across `.await` points — use scoped read locks for checks, drop before I/O, re-acquire write lock only for mutations.

### Cleanup (same session)

Flattened `LocalControl` wrapper back into top-level `Message` variants. Removed `ConnectionKind` gating (all directly connected clients are equally trusted). Added missing forwarding arms for multi-hop response routing. Extracted then later inlined `forward_to_next_hop` helper as generic forwarding collapsed to one site.

- **Kept `block_in_place` for `ConnectToServer`**: The async type recursion cycle (handle_message → tcp_connect → connection_loop → handle_message) requires breaking the cycle at the type level. `block_in_place` + `block_on` is simplest without boxing.

### Future

- **Agent propagation + route-based cleanup**: `list-agents` only returns local agents. An `AdvertiseAgent` message for peers would need agent→route tracking and purge on route death.
- **WebSocket token validation in cloud mode**: WebSocket connections currently bypass authentication.

---

## 2026-02-02 → 2026-02-04: Cloud mode infrastructure

### Summary

Implemented the full cloud mode stack: OAuth 2.0 device flow authentication, JWT validation with JWKS caching, TLS transport, persistent state management (`~/.local/state/amux/state.yaml`), cloud connection manager with exponential backoff, and server-side cloud mode support (`amux serve --cloud`). Integrated outbound cloud connections into the server using the unified `tcp_peer_loop` pattern with optional token refresh.

### Decisions Made

- **Unix socket always available**: Even cloud servers need Unix socket for local management commands (`list-agents`, `kill-server`). Created unconditionally.
- **No polling for cloud mode**: Instead of polling every 60 seconds waiting for cloud mode to be enabled, `establish_cloud_connection` checks once at startup. Users must restart after `amux init`.
- **Retriable vs non-retriable errors**: Auth failures (`NotAuthenticated`, `Auth`, `CloudDisabled`, `InvalidCredentials`) stop reconnection immediately. Connection errors trigger exponential backoff (1s → 5min max).
- **Generic TcpTransport**: `TcpTransport<S>` generic over stream type — TLS is an implementation detail of connection setup, not the transport layer. Eliminated `TlsTcpClientTransport`/`TlsTcpServerTransport`.
- **verify_token parameter**: Token validation decoupled from TLS. `accept_handshake()` takes `verify_token: bool` — cloud servers pass `true`, local servers pass `false`.
- **Unified peer loop**: Single generic `tcp_peer_loop<T: Transport>` with `Option<TokenRefreshState>`. Uses `std::future::pending()` when None so token refresh branch never fires for non-cloud connections.
- **HostChanged triggers reconnection**: When token refresh indicates a different cloud server, the peer loop returns an error which triggers full reconnection via the auto-connect task.
- **State file with file locking**: `fs2::FileExt` for concurrent access from multiple processes (hook handlers and server).
- **Two separate cloud fields**: `is_cloud_server` (running as cloud relay with TLS+auth) vs `use_cloud_mode` (cloud enabled in state.yaml) — different concepts that were confusing when combined.
- **Serde defaults for Config**: `#[serde(default)]` at struct level allows partial YAML configs while ensuring all fields have values at runtime.
- **Test-only field semantics**: Fields like `randomise_link_name` use `#[cfg_attr(not(any(debug_assertions, test)), serde(skip_deserializing))]` — readable in all builds but only settable via config in debug/test.

---

## 2026-01-21: Link-based stack routing

### Summary

Converted from hierarchical host_id routing (using "/" separator) to link-based stack routing. Routes are `VecDeque<String>` stacks that get popped/pushed at each hop. Before sending through link X: pop X from dst, push X to src. On receive: if dst is empty, process locally; otherwise route to next hop. Replies reverse automatically by swapping src↔dst.

This replaced the earlier hierarchical routing which had prefix-based resolution and a NAT-like scheme where each server prefixed `src_host` when forwarding upstream and stripped its prefix when routing downstream.

### Decisions Made

- **VecDeque for stack**: `push_front`/`pop_front` for efficient stack operations. Top of stack is the front (first element).
- **Dot-separated serialization**: Routes serialize as "AB.BC.CD" with top on left. Compact and readable in logs.
- **Link name generation**: nanoid with lowercase alphanumeric (36 chars). Terminal links `term-{4}`, hook links `hook-{4}`, server links `{hostname}-{4}`.
- **Collision detection with retry**: Clients retry up to 5 times with new random names. With 36^4 = 1.6M possible suffixes, collisions are rare.
- **ProtocolError enum**: Typed errors (`ServerError(String)`, `LinkNameTaken`, `NoRouteFound`) instead of `Option<String>`.

### Protocol rules

1. Before sending through link X: pop X from dst, push X to src
2. src must never be empty — it's the return path for replies
3. For responses: use `Route::reply(incoming_src)` to prepare reply routes
4. For forwarding: manipulate the incoming src/dst, push the outgoing link

### Stack routing example

```
A creates:      dst=[AB,BC,CD]  src=[]
A sends via AB: dst=[BC,CD]    src=[AB]      → B
B sends via BC: dst=[CD]       src=[BC,AB]   → C
C sends via CD: dst=[]         src=[CD,BC,AB]→ D
D receives:     dst=[]         → process locally (src has full return path)
Reply: swap src↔dst, route automatically reversed.
```

---

## 2026-01-15 → 2026-01-18: Hooks, structured logs, and dashboard

### Summary

Built the Claude Code integration layer: hooks system for session start and permission requests, structured log parsing from Claude's transcript files, WebSocket transport with JSON serialization for the React dashboard, and dashboard input via `SubmitInput`. Migrated from bincode to MessagePack (rmp-serde) for binary serialization.

### Key decisions and lessons

- **Bincode → msgpack migration**: Bincode fails with `DeserializeAnyNotSupported` on `#[serde(tag = "...")]` tagged enums. MessagePack with `to_vec_named` (named map format) handles tagged enums and provides forward/backward compatibility across protocol versions.
- **Serde's full power for Claude JSON parsing**: Claude sends `tool_name` + `tool_input` as separate fields. Instead of manual `serde_json::Value` parsing, use `#[serde(tag = "tool_name", content = "tool_input")]` (adjacently-tagged) with `#[serde(flatten)]` to deserialize directly into typed structs.
- **Two input message types**: Raw terminal clients use `InputBytes` for direct byte passthrough. Dashboard uses `SubmitInput` which adds a 20ms delay between text and Enter to ensure Claude Code interprets them as separate events (PTY read boundary semantics).
- **Connection type determines subscription mode**: WebSocket subscribes to structured logs, Unix/TCP subscribes to raw bytes. No new Subscribe variants needed.
- **Separate `MultiplexLogBuffer`**: Logs need entry-count limits, not byte limits, so a separate buffer type was created.
- **Runtime nesting fix**: Hook commands run through `#[tokio::main]`, so creating a nested runtime panicked. Fixed with `tokio::task::block_in_place` + `Handle::current().block_on()`.
- **Hooks fail silently**: Errors logged to `/tmp/amux.log` but exit code 0. Hooks should not block Claude Code workflow.
- **CSI u keyboard protocol**: Modern terminals (iTerm2, kitty, WezTerm) send `ESC[98;5u` for Ctrl-b instead of raw `0x02`. Code detects both for detach.
- **StdinEvent enum over AtomicBool**: Using an enum through the channel lets the main loop react immediately to detach, rather than polling a flag that was never checked because the loop was blocked in `select!`.
- **Keystroke-based permission response**: Claude Code's TUI accepts 1/2/3 for Yes/Yes(all)/No — single character responses.
- **Subscriber leak fix**: Dead subscribers accumulated in `MultiplexBuffer`. Fixed with `subs.retain(|tx| tx.send(...).is_ok())` — combines broadcast and cleanup in a single pass.

---

## 2026-01-13 → 2026-01-15: TCP transport and remote subscriptions

### Summary

Added TCP transport for server-to-server connections, implemented remote agent subscriptions, and evolved the connection handler architecture to its current symmetric form. A client on Server B can attach to an agent on Server A. Fixed a critical mutex deadlock by switching from shared transport access to channel-based message passing.

### Key decisions and lessons

- **Channel-based routing (deadlock fix)**: The original design used `Arc<Mutex<Box<dyn Transport>>>` in the routes table. This caused deadlock: TCP handler holds mutex while blocked on `read_message().await`, Unix client handler tries to acquire mutex to write → blocked forever. Solution: store `mpsc::Sender<Message>` in routes. Each handler owns its transport and uses `select!` to read from transport OR receive from channel.
- **Raw mode removed (premature optimization)**: The raw byte mode optimization for local Unix sockets was removed. Message framing overhead is negligible for local sockets, and consistent message-based protocol simplifies debugging. Can be added back if profiling shows it matters.
- **SubscriptionHandle removed**: Introduced as an abstraction, then removed — it added complexity without clear benefit. Session now exposes `MultiplexReader` and input sender directly.
- **Connect goes through local server**: `amux connect` sends `ConnectToServer` to the local server via Unix socket — the server makes the outbound TCP connection. Keeps connection state managed by the server.
- **Symmetric handler naming**: `unix_accept`/`tcp_accept`, `unix_client_loop`/`tcp_peer_loop`, `unix_handle_message`/`tcp_handle_message`. Handlers kept separate because Unix (local client) and TCP (peer server) serve different roles.
- **Subscribe spawns output task**: When Subscribe succeeds, spawn a task that reads from buffer_reader and sends Output messages via the client's route channel. Main loop continues handling all messages — allows commands while attached.

---

## 2026-01-10: Milestone 1 complete + E2E testing framework

### Summary

Converted the early prototype to the production architecture: message-based protocol with serde/bincode serialization, length-prefixed framing, raw byte streaming after subscribe, multi-client support with replay buffers. Built a declarative E2E regression testing framework with explicit output matching and variable substitution.

### Key decisions

- **CLI design (tmux-style)**: `new-agent -t <name> <command>` and `attach [-t <name>]`. Command is positional to new-agent, not attach.
- **Separate CreateAgent and Subscribe**: Creating an agent and subscribing are separate messages — allows creating without attaching.
- **MultiplexBuffer atomic subscribe (race condition fix)**: Replaced separate `replay_buffer` + `broadcast_tx` with unified `MultiplexBuffer`. The old architecture had a race: data could be lost between getting replay and subscribing to broadcast. Fix: `write()` holds lock during append AND broadcast; `subscribe()` holds lock during snapshot AND registration. Either new data is in the snapshot, OR the subscriber is registered before it's broadcast.
- **AgentType enum**: Type safety ensures only known agent types. `TestAgent(String)` variant excluded from release builds via `#[cfg(any(debug_assertions, test))]`.
- **session_id = agent_id for hook linking**: Pass agent's target name as Claude's `--session-id`, then look up `agents.get(session_id)` when the hook arrives. Replaces fragile `agents.iter().last()` hack.
- **UUID-based agent IDs with alias support**: `agent_id` is auto-generated UUID; `-t` flag sets optional human-readable alias. `resolve_agent()` tries UUID first, falls back to alias scan.
- **E2E explicit output**: Tests show exactly what the terminal shows — PTY echo followed by agent response. More verbose but completely transparent.
- **E2E auto-injection**: Test files use simple `amux` and `test-agent` names; executor injects absolute paths and `--config` flag automatically.

---

## 2026-01-XX: Initial Prototype (Pre-architecture)

### Summary
Initial prototype demonstrating basic PTY multiplexing. Used raw command bytes (0x01=ATTACH, 0x02=LIST, 0x03=KILL) instead of structured messages. Proved out the core concepts but needed restructuring.

### Key Learnings Carried Forward
- `portable-pty` works well for PTY management
- `spawn_blocking` needed for PTY reads (blocking I/O)
- `broadcast::channel` works well for multi-client fan-out
- Child waiter task pattern for clean process lifecycle
- `RawModeGuard` RAII pattern for terminal state restoration

---
