# Claude Code Instructions

This file provides guidance for AI assistants working on the amux codebase.

## First Steps

1. **Read DEVLOG.md** - See recent work, decisions made, and current state
2. **Read this file** - Understand code style and project structure
3. **Skim ARCHITECTURE.md** - Canonical design for the system

## After Completing Work

1. Run `cargo fmt && cargo clippy && cargo test`
2. **Update DEVLOG.md** - Add an entry describing what was done (see template in DEVLOG.md)

## Git Commits

- **Do NOT include Co-Authored-By lines** in commit messages
- Keep commit messages concise and descriptive
- Use lowercase for commit message subjects

## Current State (January 2025)

**Milestone 1 is complete.** The codebase implements local terminal connections with the new architecture:

- Message-based protocol with serde/bincode serialization
- Transport trait with length-prefixed framing
- Raw byte mode after subscribe (zero framing overhead)
- Multi-client support via broadcast channels

**Files:**
- `src/main.rs` - CLI (new-agent, attach, list-agents, kill-server)
- `src/message.rs` - Protocol messages with serde
- `src/transport.rs` - Transport trait and UnixTransport
- `src/session.rs` - AgentId and LocalAgentSession with PTY
- `src/server.rs` - Server with connection and subscription management
- `src/client.rs` - Client protocol implementation
- `src/config.rs` - Server configuration
- `src/connection.rs` - ConnectionId and connection state
- `src/error.rs` - Error types with thiserror

**Source of truth:** ARCHITECTURE.md remains the canonical design for future work (TCP, WebSocket, cloud mode).

## Suggested Implementation Order

Start with the core types and local-only flow, then expand:

1. **Message enum + serde** - Define all message types with `Serialize`/`Deserialize`
2. **Transport trait + UnixTransport** - `read_frame`, `write_frame`, `read_raw`, `write_raw`
3. **Config** - Server configuration struct
4. **LocalAgentSession** - PTY spawning, replay buffers, input channel
5. **Server + LocalConnection** - Get local terminal → agent flow working
6. **TcpTransport + RemoteConnection** - Add TCP with `bincode` serialization
7. **Routing table + AddAgents** - Server-to-server agent discovery
8. **Proxy logic** - Subscribe/output forwarding through cloud
9. **WebSocketTransport** - JSON serialization for rich clients
10. **Token validation** - Cloud mode authentication

**Milestone 1:** Local terminal can attach to local agent (steps 1-5)
**Milestone 2:** Two servers can connect and route (steps 6-8)
**Milestone 3:** Full cloud flow with mobile client (steps 9-10)

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
- `AgentId` - tuple of (host_id, user_id, agent_id)
- `Connection` - enum of Local (Unix) or Remote (TCP/WebSocket)
- `LocalAgentSession` - a running agent with PTY and replay buffers
- `Route` - either Local or Remote { via: ConnectionId }

**Key optimization:** Local Unix sockets switch to raw byte mode after subscribe (zero framing overhead).

## Code Style

### Rust Idioms

- Use `thiserror` for error types
- Use `tokio` for async runtime
- Use `serde` with `bincode` (TCP/Unix) and `serde_json` (WebSocket) for serialization
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
├── main.rs           # CLI parsing, server startup
├── server.rs         # Server struct, connection management
├── connection.rs     # Connection types, message handling
├── session.rs        # LocalAgentSession, PTY management
├── transport.rs      # Transport trait, Unix/TCP/WebSocket impls
├── message.rs        # Message enum, serde setup
├── routing.rs        # Routing table, Route enum
└── config.rs         # Config struct
```

### Guidelines

1. **Keep functions small** - Extract helpers when a function grows beyond ~50 lines
2. **Handle errors explicitly** - Use `Result<T, E>`, avoid `.unwrap()` except in tests
3. **Document public APIs** - Add doc comments to public structs and functions
4. **Prefer composition** - Use traits and enums over inheritance patterns
5. **Test the boundaries** - Focus tests on message handling and state transitions

## Common Tasks

### Adding a new message type

1. Add variant to `Message` enum in `message.rs`
2. Handle in `LocalConnection::handle_message` and/or `RemoteConnection::handle_message`
3. Add corresponding server method if needed

### Adding a new transport

1. Implement `Transport` trait (read_frame, write_frame, read_raw, write_raw, close)
2. Add to `RemoteConnection` or create new connection type
3. Add listener in server startup

### Modifying the routing table

1. Update `Route` enum if adding new route types
2. Update `handle_connection_closed` for cleanup
3. Update `handle_add_agents` for population

## Building and Testing

**After making any code changes, always run:**

```bash
cargo fmt               # Format code
cargo clippy            # Lint (fix any warnings)
cargo test              # Run all tests
cargo run -p e2e-runner -- run   # Run E2E tests
```

**After completing a chunk of work:**
- Update DEVLOG.md with a new entry (see template in that file)

Additional commands:
```bash
cargo check             # Fast type-check without full build
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
