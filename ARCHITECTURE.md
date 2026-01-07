# Agent Multiplexer (amux) Architecture

## Overview

amux is a terminal multiplexer for Claude that allows multiple terminals to connect to Claude sessions. It provides a transparent wrapper that preserves Claude's full interactive TUI while enabling session sharing and management.

## System Architecture

```
Terminal A                    Terminal B                   Terminal C
    │                             │                            │
    ▼                             ▼                            ▼
┌───────┐                    ┌───────┐                    ┌───────┐
│ amux  │                    │ amux  │                    │ amux  │
│client │                    │client │                    │client │
└───┬───┘                    └───┬───┘                    └───┬───┘
    │                             │                            │
    └─────────────┬───────────────┴────────────┬───────────────┘
                  │        Unix Socket         │
                  │      /tmp/amux.sock        │
                  ▼                            ▼
           ┌─────────────────────────────────────────┐
           │              amux server                │
           │                                         │
           │  ┌─────────────────────────────────┐   │
           │  │     Session Manager (HashMap)   │   │
           │  │  agent1 → AgentSession          │   │
           │  │  agent2 → AgentSession          │   │
           │  │  ...                            │   │
           │  └─────────────────────────────────┘   │
           │                  │                      │
           │    ┌─────────────┴─────────────┐       │
           │    ▼                           ▼       │
           │  ┌───────────────┐  ┌───────────────┐  │
           │  │ AgentSession  │  │ AgentSession  │  │
           │  │ - PTY+Claude  │  │ - PTY+Claude  │  │
           │  │ - Replay buf  │  │ - Replay buf  │  │
           │  │ - Broadcast   │  │ - Broadcast   │  │
           │  └───────────────┘  └───────────────┘  │
           └─────────────────────────────────────────┘
```

## Wire Protocol

Single socket at `/tmp/amux.sock`. Client sends command byte followed by command-specific data:

| Command | Byte | Payload | Response |
|---------|------|---------|----------|
| ATTACH  | 0x01 | session_name (null-terminated) + terminal size (4 bytes) | Switches to PTY streaming mode |
| LIST    | 0x02 | none | Text list of sessions, then close |
| KILL    | 0x03 | none | Kills all agents, server exits |

### Terminal Size Format
4 bytes: rows (u16 BE) + cols (u16 BE)

## CLI Usage

```bash
amux                      # Attach to agent1 (creates if needed)
amux attach -t agent2     # Attach to/create specific session
amux list-agents          # List running sessions
amux kill-server          # Kill all sessions, stop server

# While attached:
Ctrl-b d                  # Detach (return to shell, session continues)
Ctrl-b Ctrl-b             # Send literal Ctrl-b
```

## Key Components

### AgentSession (`src/session.rs`)

Encapsulates all resources for one Claude session:

```rust
struct AgentSession {
    name: String,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    replay_buffer: Arc<RwLock<Vec<u8>>>,           // Up to 10MB
    broadcast_tx: Arc<RwLock<Option<Sender>>>,     // None when session dead
    pty_input_tx: mpsc::Sender<Vec<u8>>,
    current_size: Arc<Mutex<(u16, u16)>>,
    alive: Arc<AtomicBool>,
}
```

**Lifecycle:**
1. `new()` - Spawns Claude in PTY, starts reader/writer tasks
2. `attach()` - Connects client to session (resize, replay, stream)
3. When Claude exits → `broadcast_tx.take()` → clients get `Closed` → `[session ended]`

### Server (`src/server.rs`)

Single server process managing all sessions:

```rust
struct Server {
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
}
```

**Responsibilities:**
- Listen on `/tmp/amux.sock`
- Parse protocol commands and dispatch
- Create/destroy sessions on demand
- Remove dead sessions from map

### Client (`src/client.rs`)

**Functions:**
- `attach(session_name)` - Send ATTACH, enter streaming mode
- `list_agents()` - Send LIST, print response
- `kill_server()` - Send KILL

**Streaming mode:**
- Terminal in raw mode (`RawModeGuard` with RAII cleanup)
- Forward stdin → socket (with Ctrl-b prefix handling)
- Forward socket → stdout
- On disconnect: print `[detached from session]` or `[session ended]`

### Logging (`src/log.rs`)

- All logs to `/tmp/amux.log` (never to terminal)
- Simple `[timestamp] message` format

## Session Lifecycle

### Creation
1. Client sends `ATTACH session_name`
2. Server checks HashMap for existing session
3. If not found (or dead): create new `AgentSession`
4. Session spawns Claude in PTY

### Client Attach
1. Server resizes PTY to client's terminal size
2. Send replay buffer (up to 10MB of history)
3. Subscribe client to broadcast channel
4. Bridge: client input → PTY, PTY output → client

### Session End
1. Claude exits (normal exit, Ctrl-c, /exit, crash)
2. `child.wait()` returns in background task
3. `alive` flag set to `false`
4. `broadcast_tx.take()` drops sender
5. All clients' `broadcast_rx.recv()` returns `Closed`
6. Clients print `[session ended]` and exit cleanly
7. Server removes session from HashMap

### Detach vs Disconnect
- **Detach** (`Ctrl-b d`): Client exits, session continues running
- **Disconnect** (session ends): Claude exited, all clients notified

## Design Decisions

### Single Socket vs Multi-Socket

**Chose: Single socket** (`/tmp/amux.sock`)

*Considered:* One socket per session (`/tmp/amux/agent1.sock`, etc.)

*Why single socket:*
- Cleaner protocol with command dispatch
- Server has full knowledge of all sessions
- Foundation for future API (e.g., "list messages since X")
- Proper process management (kill-server actually kills processes)
- More like tmux architecture

### Prefix Key

**Current: Ctrl-b** (tmux default)

*Rationale:* Avoid conflict with user's tmux config (commonly remapped to Ctrl-a). Will be configurable in future.

### Session Cleanup

**Approach:** Close broadcast channel when Claude exits

*Problem:* Clients were getting stuck when Claude exited because `broadcast_tx` was held by `Arc` in session struct.

*Solution:* Wrap sender in `Option`, call `.take()` on exit to drop it. Clients receive `Closed` error and exit gracefully.

### Terminal Mode

**Raw mode with RAII guard**

- `RawModeGuard::new()` saves original termios, sets raw mode
- `Drop` implementation restores original mode
- Ensures terminal is restored even on panic/early return

---

## Development Log

### Phase 1: JSON REPL Approach (Abandoned)

**Goal:** Run Claude in headless JSON mode, parse structured output, render to terminal.

**Why abandoned:** Required reimplementing too much of Claude's UX.

### Phase 2: Transparent PTY Wrapper

**Goal:** Wrap Claude transparently, identical UX to running `claude` directly.

**Result:** Perfect transparency, but no multiplexing.

### Phase 3: Multi-Socket Multiplexer

**Goal:** Multiple terminals sharing one Claude session.

**Implementation:** One socket per session, simple but limited.

### Phase 4: Single-Socket Architecture (Current)

**Goal:** Single server managing multiple named sessions with proper lifecycle.

**Key changes:**
- Protocol-based command dispatch (ATTACH/LIST/KILL)
- `AgentSession` abstraction encapsulating all resources
- Proper session cleanup when Claude exits
- Graceful client notification on session end

---

## Future Vision

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ Mobile App  │────>│ Public      │────>│ Local       │────>│ Claude      │
│ (client)    │     │ Server      │     │ Server      │     │ Agent       │
└─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
                    (internet)          (home network)       (local shell)
```

**Planned features:**
- Configurable prefix key
- Session persistence/resume after server restart
- API for structured data: "fetch all messages since X"
- Claude hooks integration for structured event capture
- TCP/WebSocket transport for remote access
- Daisy-chaining servers
- Mobile client prototype
