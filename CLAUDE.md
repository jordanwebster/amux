# Claude Code Instructions

This file provides guidance for AI assistants working on the amux codebase.

## First Steps

1. **Read DEVLOG.md** - See recent work, decisions made, and current state
2. **Read this file** - Understand code style and project structure
3. **Skim ARCHITECTURE.md** - Canonical design for the system

## After Completing Work

1. Run `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test`
2. **Update DEVLOG.md** - Add an entry describing what was done (see template in DEVLOG.md)

## Git Commits

- **Do NOT include Co-Authored-By lines** in commit messages
- Keep commit messages concise and descriptive
- Use lowercase for commit message subjects

## Current State

**Milestones 1-3 are complete.** The codebase implements local terminal connections, server-to-server routing, cloud relay, and the v3 protocol:

- Protocol v3 with three-variant Message enum (Routable/Direct/Command)
- Opaque payload routing (intermediate hops don't deserialize)
- Per-connection request_id counters
- Per-user state isolation (for cloud multi-tenancy)
- AnnounceHost/WithdrawHost as single source of routing truth
- Agent discovery propagation via AnnounceAgent/WithdrawAgent
- MessagePack serialization for binary transports, JSON for WebSocket
- OAuth 2.0 device flow + JWT authentication for cloud
- TLS for server-to-server connections

**Source of truth:** ARCHITECTURE.md is the canonical design document for server internals. CLOUD_ARCHITECTURE.md covers the cloud deployment model.

## Getting Oriented

1. **Start with the README** - Understand what amux does at a product level
2. **Read ARCHITECTURE.md** - This is the detailed internal design document covering data structures, message flow, and the task model
3. **Skim CLOUD_ARCHITECTURE.md** - Understand the cloud deployment model (stateless servers, token auth, TTL-based routing)
4. **Explore src/** - The current prototype implementation

## Key Concepts

- **amux** is an agent multiplexer - like tmux but for AI assistant sessions
- Agents run in PTYs (pseudo-terminals) on local machines
- Multiple clients can attach to the same agent session
- Cloud servers act as stateless relays, not databases

## Architecture Summary

```
Terminal ──Unix socket──> Local amux server ──TCP──> Cloud amux server
                               │
                          [owns agents]
                          [routing table]
                          [subscriptions]
```

**Core types:**
- `agent_id` - UUID identifying an agent (optional name for human-friendly references)
- `Route` - stack of link names (`VecDeque<String>`) for multi-hop routing
- `LocalAgentSession` - a running agent with PTY and replay buffers
- `AgentRegistry` - centralized tracking of local + remote agents with name mapping
- `Host` - remote host info propagated via AnnounceHost/WithdrawHost

## Code Style

### Rust Idioms

- Use `thiserror` for error types
- Use `tokio` for async runtime
- Use `serde` with `rmp-serde` / MessagePack (TCP/Unix) and `serde_json` (WebSocket) for serialization
- Prefer `Arc<Mutex<T>>` or `Arc<RwLock<T>>` for shared state
- Use channels (`mpsc`, `broadcast`) for task communication

### Naming Conventions

- Types: `PascalCase` (e.g., `LocalAgentSession`, `ConnectionId`)
- Functions/methods: `snake_case` (e.g., `handle_subscribe`, `broadcast_output`)
- Constants: `SCREAMING_SNAKE_CASE`
- Modules: `snake_case`

### Structure

```
src/
├── main.rs                 # CLI parsing, server startup
├── message.rs              # Protocol messages (Message, RoutableMessage, DirectMessage, Command)
├── route.rs                # Route type (stack-based multi-hop routing)
├── client.rs               # Client-side protocol (new-agent, attach, list-agents, etc.)
├── config.rs               # Config struct
├── session.rs              # LocalAgentSession, PTY management
├── buffer.rs               # MultiplexBuffer for replay/broadcast
├── multiplex_log_buffer.rs # MultiplexLogBuffer for structured output
├── agent_registry.rs       # AgentRegistry (local + remote agent tracking)
├── error.rs                # Error types with thiserror
├── hooks.rs                # Claude Code hook integration
├── transcript.rs           # TranscriptTailer for Claude Code JSONL
├── cloud.rs                # Cloud connection management, token refresh
├── oauth.rs                # OAuth 2.0 device flow
├── jwt.rs                  # JWT validation (JWKS)
├── init.rs                 # `amux init` command
├── state.rs                # Persistent state (refresh tokens, etc.)
├── lib.rs                  # Library root
├── server/
│   ├── mod.rs              # Server struct, ServerState, ServerUserState
│   ├── accept.rs           # Connection acceptance, handshake
│   ├── connection.rs       # Connection loop, message dispatch
│   ├── routing.rs          # Route management, peer disconnect, agent creation
│   └── cloud.rs            # Cloud connection establishment
└── transport/
    ├── mod.rs              # Transport trait, MessageReader/Writer, TransportSplit
    ├── framing.rs          # Length-prefixed framing
    ├── unix.rs             # UnixTransport
    ├── tcp.rs              # TcpTransport (generic over stream type)
    ├── tls.rs              # TLS support (rustls)
    └── websocket.rs        # WebSocketTransport (JSON)
```

### Guidelines

1. **Keep functions small** - Extract helpers when a function grows beyond ~50 lines
2. **Handle errors explicitly** - Use `Result<T, E>`, avoid `.unwrap()` except in tests
3. **Document public APIs** - Add doc comments to public structs and functions
4. **Prefer composition** - Use traits and enums over inheritance patterns
5. **Test the boundaries** - Focus tests on message handling and state transitions

### Commenting Guidelines

Write comments that add value. Prefer clear code over comment clutter.

**Good comments (KEEP):**
- `///` doc comments on public APIs explaining behavior, invariants, return values
- `//!` module-level doc comments explaining purpose and guarantees
- WHY comments explaining non-obvious decisions or constraints
- `// Task:` labels on spawned async blocks (established pattern in this codebase)
- Important invariants (e.g., "routes table uses single-layer keys only")

**Bad comments (REMOVE):**
- Comments restating what the next line obviously does
- Comments echoing the variable/function name
- Outdated or misleading comments

**Examples:**

```rust
// BAD: echoes the field name
/// Path to the Unix socket
pub socket_path: PathBuf,

// GOOD: adds useful context
/// TCP port for server-to-server connections (defaults to 9001)
pub tcp_port: Option<u16>,

// BAD: restates the obvious
// Read length prefix
let mut len_buf = [0u8; 4];

// GOOD: explains a non-obvious decision
// Routes table uses single-layer keys only (no "/" in keys)
let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);

// GOOD: Task label on spawned block
// Task: Read from PTY, write to multiplex buffer
tokio::task::spawn_blocking(move || { ... });
```

## Common Tasks

### Adding a new routable message type

1. Add variant to `RoutableMessage` enum in `message.rs`
2. Handle in `handle_routable()` in `server/connection.rs`
3. Update client.rs if the client needs to send/receive it

### Adding a new command

1. Add variant to `Command` enum in `message.rs`
2. Handle in `handle_command()` in `server/connection.rs`
3. Update client.rs for the CLI side
4. Update `msg_type_label()` in `server/connection.rs`

### Adding a new transport

1. Implement `Transport` + `TransportSplit` traits (`read_message`, `write_message`, `into_split`)
2. Add listener in server startup (`server/mod.rs`)
3. Add accept handler in `server/accept.rs`

### Modifying routing behavior

1. Update `server/routing.rs` for route management
2. Update `handle_routable()` in `server/connection.rs` for forwarding logic
3. Update `WithdrawHost` handler for cleanup behavior

## Building and Testing

**After making any code changes, always run:**

```bash
cargo check             # Fast type-check
cargo fmt               # Format code
cargo clippy --workspace --all-targets -- -D warnings   # Lint (warnings are errors — zero tolerance)
cargo test              # Run all tests
cargo build --workspace && cargo run -p e2e-runner -- run   # Build all binaries then run E2E tests (workspace build avoids stale amux/test-agent binaries)
```

**Clippy policy:** All clippy warnings must be fixed, not suppressed. Do not add `#[allow(clippy::...)]` attributes — fix the underlying issue instead.

**After completing a chunk of work:**
- Update DEVLOG.md with a new entry (see template in that file)

Additional commands:
```bash
cargo test -- --nocapture  # See println output in tests
cargo build --release   # Build optimized binary
cargo run -p e2e-runner -- run <filter>  # Run specific E2E test
```

Manual testing:
```bash
cargo run -- new-agent -t test1 claude   # Create agent
cargo run -- attach -t test1             # Attach (in another terminal)
cargo run -- list-agents                 # List running agents
cargo run -- kill-server                 # Clean shutdown
# Use Ctrl-b d to detach without killing agent
```

## Writing E2E Tests

When adding or changing features, write an E2E test to verify the behavior. Tests live in `e2e-tests/*.test`.

### Test File Format

```
# test: my_feature
# description: What this test verifies

## Environment

# Only declare what you need - defaults are auto-injected
terminal:
  name: T1

## Test

@T1
> amux new-agent -t myagent test-agent
> input here
input here
echo: input here
```

### Key Concepts

1. **Explicit output:** Tests show ALL terminal output - PTY echo + agent response. When you send `> hello`, expect to see `hello` (PTY echo) then `echo: hello` (test-agent response).

2. **Minimal environment:** Only declare entities when needed:
   - `terminal:` - Required, at least one
   - `directory:` - Only if you need `$name.path` variables
   - `config:` - Only for multi-server tests (not yet implemented)

3. **Variables:** Use `$dirname.path` to match dynamic temp directory paths:
   ```
   directory:
     name: mydir

   terminal:
     name: T1
     cwd: mydir

   ## Test

   @T1
   > amux list-agents
     agent1 - $mydir.path
   ```

4. **Multi-terminal:** Test attach, broadcast, replay buffer:
   ```
   @T1
   > amux new-agent -t shared test-agent
   > message one
   message one
   echo: message one

   @T2
   > amux attach -t shared
   message one
   echo: message one
   > message two
   message two
   echo: message two

   @T1
   message two
   echo: message two
   ```

### When to Write E2E Tests

- New CLI commands or flags
- Changes to protocol behavior (attach, subscribe, replay)
- Multi-client interactions (broadcast, late-join replay)
- Output format changes (list-agents, error messages)

## Questions?

If the design documents don't answer your question, ask the user for clarification rather than guessing. The architecture is intentionally constrained to keep things simple.
