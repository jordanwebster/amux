# Claude Code Instructions

This file provides guidance for AI assistants working on the amux codebase.

AMUX IS IN ACTIVE DEVELOPMENT AND IS NOT CURRENTLY RELEASED. DO NOT CONCERN YOURSELF WITH BACKWARDS COMPATIBILITY.

## First Steps

1. **Read DEVLOG.md** - See recent work, decisions made, and current state
2. **Read this file** - Understand code style and project structure
3. **Skim docs/architecture.md** - Canonical design for the system

## After Completing Work

1. Run `cargo check && cargo +nightly fmt --all && cargo +nightly clippy --workspace --all-targets -- -D warnings && cargo test`
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
- MessagePack serialization for all transports (Unix, TCP, WebSocket)
- OAuth 2.0 device flow + JWT authentication for cloud
- TLS for server-to-server connections

**Source of truth:** docs/architecture.md is the canonical design document for server internals. docs/cloud_architecture.md covers the cloud deployment model.

## Getting Oriented

1. **Start with the README** - Understand what amux does at a product level
2. **Read docs/architecture.md** - This is the detailed internal design document covering data structures, message flow, and the task model
3. **Skim docs/cloud_architecture.md** - Understand the cloud deployment model (stateless servers, token auth, TTL-based routing)
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
- Use `serde` with `rmp-serde` / MessagePack for all transports (Unix, TCP, WebSocket)
- Prefer `Arc<Mutex<T>>` or `Arc<RwLock<T>>` for shared state
- Use channels (`mpsc`, `broadcast`) for task communication

### Naming Conventions

- Types: `PascalCase` (e.g., `AgentSession`, `ConnectionContext`)
- Functions/methods: `snake_case` (e.g., `handle_subscribe`, `broadcast_to_peers`)
- Constants: `SCREAMING_SNAKE_CASE`
- Modules: `snake_case`

### Structure

The project is a Cargo workspace with crates under `crates/`:

```
crates/
├── amux/                       # Library crate — public API + protocol/server/transport
│   └── src/
│       ├── lib.rs              # Public API: connect(), Connection, ConnectPolicy
│       ├── connect.rs          # connect() function, ConnectPolicy enum
│       ├── connection.rs       # Connection struct (wraps split transport)
│       ├── message.rs          # Protocol messages (Message, RoutableMessage, DirectMessage, Command)
│       ├── route.rs            # Route type (stack-based multi-hop routing)
│       ├── config.rs           # Config struct
│       ├── agents/
│       │   ├── mod.rs          # AgentSession enum, PtyHandle, StopPolicy, spawn_pty_agent
│       │   ├── claude.rs       # ClaudeSession (hook handling, structured input)
│       │   └── testagent.rs    # TestAgentSession (#[cfg(any(debug_assertions, test))])
│       ├── buffer.rs           # BroadcastBuffer<P> (generic byte + entry buffers)
│       ├── agent_registry.rs   # AgentRegistry (local + remote agent tracking)
│       ├── error.rs            # Error types with thiserror
│       ├── cloud.rs            # Cloud connection management, token refresh
│       ├── oauth.rs            # OAuth 2.0 device flow
│       ├── jwt.rs              # JWT validation (JWKS)
│       ├── handshake.rs        # Connect/ConnectResult handshake types
│       ├── state.rs            # Persistent state (refresh tokens, etc.)
│       ├── claude/
│       │   ├── mod.rs          # Claude Code integration module root
│       │   ├── types.rs        # Hook events, permissions, tool inputs, structured I/O
│       │   ├── structured_log_source.rs  # Structured log source for agents
│       │   └── transcript.rs   # TranscriptTailer for JSONL transcript files
│       ├── server/
│       │   ├── mod.rs          # Server struct, ServerState, ServerUserState
│       │   ├── accept.rs       # Connection acceptance, handshake
│       │   ├── connection.rs   # Connection loop, reader/writer tasks, stream management
│       │   ├── handlers.rs     # Message dispatch (handle_routable, handle_command, handle_direct)
│       │   ├── routing.rs      # Route management, peer disconnect, agent creation
│       │   └── cloud.rs        # Cloud connection establishment
│       └── transport/
│           ├── mod.rs          # Transport trait, MessageReader/Writer, TransportSplit
│           ├── framing.rs      # Length-prefixed framing
│           ├── unix.rs         # UnixTransport
│           ├── tcp.rs          # TcpTransport (generic over stream type)
│           ├── tls.rs          # TLS support (rustls)
│           └── websocket.rs    # WebSocketTransport (MessagePack over binary frames)
├── amux-cli/                   # Binary crate → produces `amux` binary
│   └── src/
│       ├── main.rs             # CLI parsing, server startup
│       ├── client.rs           # Client-side protocol (new, attach, list, etc.)
│       ├── init.rs             # `amux init` command
│       ├── hooks.rs            # Client-side hook handler (`amux hooks claude <event>`)
│       └── plugin.rs           # Plugin installation and update management
├── test-agent/                 # Simple echo agent for E2E testing
└── e2e-runner/                 # E2E test runner
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

1. Add variant to `RoutableMessage` enum in `crates/amux/src/message.rs`
2. Handle in `handle_routable()` in `crates/amux/src/server/handlers.rs`
3. Update `crates/amux-cli/src/client.rs` if the client needs to send/receive it

### Adding a new command

1. Add variant to `Command` enum in `crates/amux/src/message.rs`
2. Handle in `handle_command()` in `crates/amux/src/server/handlers.rs`
3. Update `crates/amux-cli/src/client.rs` for the CLI side
4. Update `Command::type_label()` and `Message::type_label()` in `crates/amux/src/message.rs`

### Adding a new transport

1. Implement `Transport` + `TransportSplit` traits (`read_message`, `write_message`, `into_split`)
2. Add listener in server startup (`crates/amux/src/server/mod.rs`)
3. Add accept handler in `crates/amux/src/server/accept.rs`

### Modifying routing behavior

1. Update `crates/amux/src/server/routing.rs` for route management
2. Update `handle_routable()` in `crates/amux/src/server/handlers.rs` for forwarding logic
3. Update `WithdrawHost` handler for cleanup behavior

## Building and Testing

**After making any code changes, always run:**

```bash
cargo check             # Fast type-check
cargo +nightly fmt --all   # Format code (nightly required for unstable rustfmt options in rustfmt.toml)
cargo +nightly clippy --workspace --all-targets -- -D warnings   # Lint (warnings are errors — zero tolerance; nightly for consistency with CI)
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
cargo run -- new claude --name test1     # Create agent
cargo run -- attach --name test1         # Attach (in another terminal)
cargo run -- list                        # List running agents
cargo run -- shutdown                    # Clean shutdown
# Use Ctrl-a d to detach without killing agent
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
> amux new test-agent --name myagent
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
   > amux list
     agent1 - $mydir.path
   ```

4. **Multi-terminal:** Test attach, broadcast, replay buffer:
   ```
   @T1
   > amux new test-agent --name shared
   > message one
   message one
   echo: message one

   @T2
   > amux attach --name shared
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
- Output format changes (list, error messages)

## Questions?

If the design documents don't answer your question, ask the user for clarification rather than guessing. The architecture is intentionally constrained to keep things simple.
