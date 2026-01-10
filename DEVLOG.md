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

## 2025-01-10: Milestone 1 Complete - Local Terminal Architecture

### Summary
Converted the early prototype to the production architecture defined in ARCHITECTURE.md. The system now uses a message-based protocol with serde/bincode serialization, length-prefixed framing, and raw byte streaming after subscribe. Multiple terminals can attach to the same agent session with full replay buffer support.

### Changes

**New files created:**
- `src/error.rs` - Error types using `thiserror` crate
- `src/message.rs` - Protocol messages (`Message` enum) with serde serialization
- `src/config.rs` - Server configuration (`Config` struct)
- `src/transport.rs` - `Transport` trait and `UnixTransport` with length-prefixed framing
- `src/connection.rs` - `ConnectionId` and `LocalConnectionState` types

**Files refactored:**
- `src/main.rs` - New CLI structure (new-agent, attach, list-agents, kill-server)
- `src/server.rs` - Message-based protocol, subscription management, raw mode streaming
- `src/client.rs` - New protocol implementation with framed handshake then raw mode
- `src/session.rs` - Added `AgentId`, kept proven PTY patterns from prototype

**Dependencies added:**
- `serde` + `bincode` - Message serialization
- `thiserror` - Error handling
- `uuid` - Host ID generation
- `async-trait` - Async trait support
- `tempfile` (dev) - Test utilities

### Decisions Made

1. **CLI design (tmux-style):** Commands are `new-agent -t <name> <command>` and `attach [-t <name>]`. This matches tmux conventions users already know. The command (claude, codex, etc.) is a positional argument to new-agent, not attach, because you're creating an agent of that type.

2. **Working directory propagation:** The client sends its current working directory in `CreateAgent` message so agents spawn in the right place. This is stored in `LocalAgentSession` and shown in `list-agents`.

3. **Separate CreateAgent and Subscribe:** Creating an agent and subscribing to it are separate protocol messages. This allows future features like creating agents without immediately attaching.

4. **Raw mode after subscribe:** Following ARCHITECTURE.md, local Unix sockets switch to raw byte streaming after the Subscribe handshake completes. This eliminates framing overhead for the high-frequency PTY I/O path.

5. **Keep prototype PTY patterns:** The prototype's PTY handling (spawn_blocking for reads, broadcast channel for fan-out, child waiter task) was battle-tested and carried forward unchanged.

6. **Length-prefixed framing:** Messages use 4-byte big-endian length prefix + bincode payload. Maximum frame size is 16MB to prevent DoS.

### Verification

All tests passed and manual verification completed successfully:

| Test | Result |
|------|--------|
| `cargo build` | OK |
| `cargo clippy` | OK (only dead code warnings for future infrastructure) |
| `cargo test` | 10/10 passed |
| `amux new-agent -t test1 claude` | Creates agent, shows Claude UI |
| `amux attach -t test1` (2nd terminal) | Both terminals show same output |
| Type in either terminal | Input reaches agent correctly |
| Ctrl-b d | Detaches cleanly, agent continues |
| Reattach after detach | Replay buffer displayed correctly |
| `amux list-agents` | Shows running agents with command and working dir |
| `amux kill-server` | Cleans up all agents and exits |
| Ctrl-C handling | Works correctly - one Ctrl-C escapes input, another kills agent, all attached terminals return to shell cleanly |

### Current State

**Working:**
- Local Unix socket server with auto-start
- Message-based protocol (CreateAgent, Subscribe, ListAgents, Shutdown)
- Multiple clients attaching to same agent
- Replay buffer for late joiners
- Ctrl-b d to detach
- Clean Ctrl-C handling across all attached terminals
- Terminal resize propagation

**Not yet implemented (future milestones):**
- TCP transport for server-to-server connections
- WebSocket transport for rich clients
- Token-based authentication for cloud mode
- Routing table for multi-server agent discovery
- Structured logs for mobile/web clients

### Next Steps

Milestone 2 (from ARCHITECTURE.md):
1. Add `TcpTransport` implementation
2. Add `RemoteConnection` type
3. Implement routing table
4. Add `AddAgents` message for agent discovery
5. Test two servers connecting and routing

---

## 2025-01-XX: Initial Prototype (Pre-architecture)

### Summary
Initial prototype demonstrating basic PTY multiplexing. Used raw command bytes (0x01=ATTACH, 0x02=LIST, 0x03=KILL) instead of structured messages. Proved out the core concepts but needed restructuring.

### Key Learnings Carried Forward
- `portable-pty` works well for PTY management
- `spawn_blocking` needed for PTY reads (blocking I/O)
- `broadcast::channel` works well for multi-client fan-out
- Child waiter task pattern for clean process lifecycle
- `RawModeGuard` RAII pattern for terminal state restoration

---
