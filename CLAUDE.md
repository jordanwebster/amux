# Claude Code Instructions

This file provides guidance for AI assistants working on the amux codebase.

## Current State (January 2025)

The existing code in `src/` is an **early prototype** that predates the current architecture. It demonstrates basic PTY multiplexing but does not follow the designs in ARCHITECTURE.md.

**Source of truth:** Use ARCHITECTURE.md for the canonical design, not the prototype code. The prototype may be useful for reference on PTY handling (`portable-pty` usage), but the overall structure needs to be rewritten.

**Files to note:**
- `src/main.rs` - CLI parsing (can be adapted)
- `src/session.rs` - PTY spawning patterns (useful reference)
- `src/server.rs`, `src/client.rs` - Will be restructured
- `prototyping.rs` - Design sketches, not real code

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

## Testing

```bash
cargo test              # Run all tests
cargo test -- --nocapture  # See println output
cargo clippy            # Lint
cargo fmt               # Format
```

## Questions?

If the design documents don't answer your question, ask the user for clarification rather than guessing. The architecture is intentionally constrained to keep things simple.
