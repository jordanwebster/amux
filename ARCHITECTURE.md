# amux Architecture

## Overview

amux is a terminal multiplexer for Claude that enables multiple clients to connect to Claude agent sessions. It supports two classes of clients:

1. **Terminal clients** - Traditional terminals consuming raw PTY output with ANSI rendering
2. **Rich clients** - Mobile apps, web dashboards consuming structured log data

The architecture provides a unified session lifecycle model while supporting different wire protocols and data formats for each client type.

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              amux server                                     │
│                                                                              │
│  ┌─────────────────────────┐              ┌─────────────────────────────┐   │
│  │   Unix Socket           │              │   TCP/WebSocket             │   │
│  │   /tmp/amux.sock        │              │   0.0.0.0:7890              │   │
│  │                         │              │                             │   │
│  │   - Terminal clients    │              │   - Mobile apps             │   │
│  │   - Local CLI tools     │              │   - Web dashboards          │   │
│  │   - Raw PTY protocol    │              │   - Framed JSON protocol    │   │
│  └───────────┬─────────────┘              └──────────────┬──────────────┘   │
│              │                                           │                   │
│              └─────────────────────┬─────────────────────┘                   │
│                                    │                                         │
│                                    ▼                                         │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                         Session Manager                                │  │
│  │                                                                        │  │
│  │   agent1 → Running { pty, buffers, broadcasts }                       │  │
│  │   agent2 → Ended { exit_code, replay_buffer, log_lines }              │  │
│  │   agent3 → Running { ... }                                             │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                    │                                         │
│              ┌─────────────────────┼─────────────────────┐                   │
│              ▼                     ▼                     ▼                   │
│  ┌───────────────────┐ ┌───────────────────┐ ┌───────────────────┐          │
│  │   AgentSession    │ │   AgentSession    │ │   AgentSession    │          │
│  │                   │ │                   │ │                   │          │
│  │ ┌───────────────┐ │ │ ┌───────────────┐ │ │ ┌───────────────┐ │          │
│  │ │ PTY + Claude  │ │ │ │ PTY + Claude  │ │ │ │ PTY + Claude  │ │          │
│  │ └───────────────┘ │ │ └───────────────┘ │ │ └───────────────┘ │          │
│  │                   │ │                   │ │                   │          │
│  │ ┌───────────────┐ │ │ ┌───────────────┐ │ │ ┌───────────────┐ │          │
│  │ │ PTY Buffer    │ │ │ │ PTY Buffer    │ │ │ │ PTY Buffer    │ │          │
│  │ │ (raw bytes)   │ │ │ │ (raw bytes)   │ │ │ │ (raw bytes)   │ │          │
│  │ └───────────────┘ │ │ └───────────────┘ │ │ └───────────────┘ │          │
│  │                   │ │                   │ │                   │          │
│  │ ┌───────────────┐ │ │ ┌───────────────┐ │ │ ┌───────────────┐ │          │
│  │ │ Log Buffer    │ │ │ │ Log Buffer    │ │ │ │ Log Buffer    │ │          │
│  │ │ (JSON lines)  │ │ │ │ (JSON lines)  │ │ │ │ (JSON lines)  │ │          │
│  │ └───────────────┘ │ │ └───────────────┘ │ │ └───────────────┘ │          │
│  └───────────────────┘ └───────────────────┘ └───────────────────┘          │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Dual Data Streams

Each session maintains two parallel data streams for different client types:

| Aspect | PTY Stream | Log Stream |
|--------|------------|------------|
| **Content** | Raw terminal bytes with ANSI codes | Structured JSON log lines |
| **Source** | PTY master output | Claude Code hooks / log files |
| **Consumers** | Terminal clients | Rich clients (apps, web) |
| **Rendering** | Terminal emulator (ANSI) | Native UI components |
| **Buffer** | `replay_buffer: Vec<u8>` | `log_lines: Vec<LogLine>` |
| **Broadcast** | `broadcast::Sender<Vec<u8>>` | `broadcast::Sender<LogLine>` |

### Log Line Types

```rust
enum LogLine {
    // Conversation
    UserMessage { content: String, timestamp: u64 },
    AssistantMessage { content: String, timestamp: u64 },

    // Tool usage
    ToolCall { tool: String, params: Value, timestamp: u64 },
    ToolResult { tool: String, result: Value, success: bool, timestamp: u64 },

    // Session lifecycle
    SessionStart { cwd: String, model: String, timestamp: u64 },
    SessionEnd { reason: String, exit_code: Option<i32>, timestamp: u64 },

    // Errors
    Error { message: String, recoverable: bool, timestamp: u64 },
}
```

---

## Unified Session Lifecycle

Both terminal and rich clients share the same session lifecycle model. The difference is in how they **respond** to lifecycle events (a UI decision).

### Session States

```
┌──────────┐      ┌──────────┐      ┌──────────┐      ┌──────────┐
│ Creating │ ───> │ Running  │ ───> │  Ended   │ ───> │ Deleted  │
└──────────┘      └──────────┘      └──────────┘      └──────────┘
                        │                 │
                  [event:            [event:
                   output]            ended]
```

```rust
enum SessionState {
    Running {
        pty: MasterPty,
        pty_broadcast: broadcast::Sender<Vec<u8>>,
        log_broadcast: broadcast::Sender<LogLine>,
        replay_buffer: Vec<u8>,
        log_lines: Vec<LogLine>,
        started_at: Instant,
        current_size: (u16, u16),
    },
    Ended {
        exit_code: Option<i32>,
        replay_buffer: Vec<u8>,     // Preserved for terminal replay
        log_lines: Vec<LogLine>,    // Preserved for rich client viewing
        started_at: Instant,
        ended_at: Instant,
    },
}
```

### Event Model

```rust
enum SessionEvent {
    Created { name: String },
    Output { name: String },           // New data available
    LogLine { name: String, line: LogLine },
    Ended { name: String, exit_code: Option<i32> },
    Deleted { name: String },
}
```

### Client Strategies

The same events, different UI responses:

| Client Type | on_session_ended Response |
|-------------|---------------------------|
| Terminal | Print `[session ended]`, exit process |
| Mobile app | Show interstitial, offer "View History" / "Close" |
| Web dashboard | Update status badge, keep session visible |
| Monitoring tool | Log event, alert if unexpected |
| CI/automation | Check exit code, report pass/fail |

---

## Protocol v1: Terminal Clients (Current)

### Transport
Unix socket at `/tmp/amux.sock`

### Wire Protocol
Simple binary protocol optimized for terminal use:

| Command | Byte | Payload | Response |
|---------|------|---------|----------|
| ATTACH | 0x01 | session_name (null-terminated) + size (4 bytes) | Switches to PTY streaming |
| LIST | 0x02 | none | Text list of sessions |
| KILL | 0x03 | none | Kills all agents, server exits |

### Terminal Size Format
```
[rows: u16 BE][cols: u16 BE]  // 4 bytes total
```

### Attach Flow
```
Client                                 Server
  │                                      │
  │──── ATTACH (0x01) ──────────────────>│
  │     "agent1\0"                       │
  │     [rows:cols]                      │
  │                                      │
  │<──── replay_buffer ──────────────────│
  │<──── live PTY stream ────────────────│
  │────> stdin ──────────────────────────│
  │                                      │
  │      ... bidirectional streaming ... │
  │                                      │
  │<──── [connection close] ─────────────│  (session ended)
  │                                      │
```

### Control Sequences
- `Ctrl-b d` - Detach from session (session continues)
- `Ctrl-b Ctrl-b` - Send literal Ctrl-b

### Session End Signaling
Connection close indicates session ended. Terminal client prints `[session ended]` and exits.

---

## Protocol v2: Rich Clients (Future)

### Transport
- TCP socket (e.g., `0.0.0.0:7890`)
- WebSocket for browser clients
- Future: HTTPS with authentication

### Wire Protocol
Framed messages with JSON payloads:

```
┌─────────────┬─────────────┬─────────────────────────┐
│ type (1B)   │ length (4B) │ payload (variable)      │
└─────────────┴─────────────┴─────────────────────────┘

Types:
  0x10 = REQUEST       (JSON payload)
  0x11 = RESPONSE      (JSON payload)
  0x12 = EVENT         (JSON payload)
```

### Commands

```json
// List all sessions (including ended)
→ {"cmd": "list_sessions"}
← {"sessions": [
    {"name": "agent1", "state": "running", "started_at": 1704700000},
    {"name": "agent2", "state": "ended", "exit_code": 0, "ended_at": 1704699000}
  ]}

// Get session details
→ {"cmd": "get_session", "name": "agent1"}
← {"name": "agent1", "state": "running", "size": [24, 80], "log_count": 847}

// Fetch log lines (paginated)
→ {"cmd": "get_logs", "session": "agent1", "from": 0, "to": 100}
← {"lines": [...], "total": 847}

// Get latest sequence number (for cache sync)
→ {"cmd": "get_log_head", "session": "agent1"}
← {"seq": 847}

// Subscribe to events
→ {"cmd": "subscribe", "events": ["session_created", "session_ended", "log_line"]}
← {"ok": true}

// Create new session
→ {"cmd": "create_session", "name": "agent3"}
← {"ok": true, "name": "agent3"}

// Send input to session
→ {"cmd": "send_input", "session": "agent1", "content": "Help me refactor..."}
← {"ok": true}

// Kill session
→ {"cmd": "kill_session", "name": "agent1"}
← {"ok": true}

// Delete ended session
→ {"cmd": "delete_session", "name": "agent2"}
← {"ok": true}
```

### Events

```json
// Session lifecycle
{"event": "session_created", "name": "agent3", "timestamp": 1704700100}
{"event": "session_ended", "name": "agent1", "exit_code": 0, "timestamp": 1704700200}

// New log line available
{"event": "log_line", "session": "agent1", "seq": 848, "line": {...}}
```

### Client Caching Pattern

```
Mobile App                              Server
    │                                      │
    │── get_log_head("agent1") ───────────>│
    │<─ {seq: 500} ────────────────────────│
    │                                      │
    │   [local cache has seq 0-400]        │
    │                                      │
    │── get_logs("agent1", 401, 500) ─────>│
    │<─ {lines: [...]} ────────────────────│
    │                                      │
    │   [cache now 0-500, subscribe live]  │
    │                                      │
    │── subscribe(["log_line"]) ──────────>│
    │                                      │
    │<─ {event: log_line, seq: 501} ───────│
    │<─ {event: log_line, seq: 502} ───────│
    │                                      │
```

---

## Streaming Strategy

### Decision: Chunked Updates First, Hybrid Later

**Phase 1 (Initial Implementation):**
Rich clients receive log lines as they're written to Claude's log files. Updates arrive in chunks when Claude flushes output, not token-by-token.

**Rationale:**
- Simpler implementation
- No PTY parsing complexity
- Acceptable UX for initial version
- Log files provide authoritative structured data

**Phase 2 (Future Optimization):**
Implement hybrid streaming for better UX:

```
PTY Stream ────> Parser ────> Streaming chunks (ephemeral, real-time)
                                      │
                                      ▼
                              ┌──────────────────┐
                              │   Rich Client    │
                              │                  │
                              │ [streaming...█]  │ ← Live tokens from PTY parse
                              │                  │
Log File ─────> Watcher ─────>│ [Reconcile]      │ ← When log lands, replace
                              │                  │   with authoritative version
                              └──────────────────┘
```

**Hybrid approach benefits:**
- Real-time streaming feel (like ChatGPT/Claude apps)
- Eventually consistent with structured logs
- Best of both worlds

**Hybrid approach challenges:**
- PTY parsing is fragile (depends on Claude Code UI format)
- Brief reconciliation moment when logs land
- Additional complexity

**Alternative for Phase 2:**
If Claude Code adds streaming hooks (e.g., `on_assistant_chunk`), use those instead of PTY parsing. Cleaner solution, but requires Claude Code changes.

---

## Current Implementation (v1)

### Files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI parsing, server auto-start |
| `src/server.rs` | Unix socket listener, command dispatch |
| `src/session.rs` | AgentSession struct, PTY management |
| `src/client.rs` | Terminal client, raw mode, Ctrl-b handling |
| `src/log.rs` | File-based logging to `/tmp/amux.log` |

### Key Structures

```rust
// src/session.rs
pub struct AgentSession {
    pub name: String,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    replay_buffer: Arc<RwLock<Vec<u8>>>,
    broadcast_tx: Arc<RwLock<Option<broadcast::Sender<Vec<u8>>>>>,
    pty_input_tx: mpsc::Sender<Vec<u8>>,
    current_size: Arc<Mutex<(u16, u16)>>,
}

// src/server.rs
pub struct Server {
    sessions: Arc<RwLock<HashMap<String, Arc<AgentSession>>>>,
}
```

### Session Cleanup

When Claude exits:
1. `child.wait()` returns in background task
2. PTY master is dropped (sends SIGHUP to shell)
3. `broadcast_tx.take()` closes broadcast channel
4. All attached clients receive `Closed` error
5. Server removes session from HashMap
6. Terminal clients print `[session ended]` and exit

### CLI Usage

```bash
amux                      # Attach to agent1 (creates if needed)
amux attach -t agent2     # Attach to specific session
amux list-agents          # List running sessions
amux kill-server          # Kill all sessions, stop server
```

---

## Future Vision

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ Mobile App  │────>│ Public      │────>│ Local       │────>│ Claude      │
│ (rich)      │     │ Server      │     │ Server      │     │ Agent       │
└─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
                    (internet)          (home network)       (local PTY)

┌─────────────┐
│ Terminal    │─────────────────────────────────────────────>│
│ (local)     │                                              │
└─────────────┘                    Unix socket               │
```

### Planned Features

- [ ] TCP listener for rich clients
- [ ] Log buffer with sequence numbers
- [ ] Event subscription system
- [ ] Persistent ended sessions
- [ ] Chunked log streaming
- [ ] WebSocket transport
- [ ] Authentication
- [ ] Hybrid PTY-parse streaming (Phase 2)
- [ ] Session persistence across server restart
- [ ] Daisy-chaining servers for remote access
