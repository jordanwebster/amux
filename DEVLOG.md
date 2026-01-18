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

## 2025-01-18: Migrate from bincode to msgpack (rmp-serde)

### Summary

Replaced bincode with rmp-serde (MessagePack) for binary serialization over Unix/TCP transports. This provides protocol robustness (msgpack handles field additions/removals gracefully across versions), tagged enum support (bincode fails with `DeserializeAnyNotSupported` on `#[serde(tag = "...")]`), and allows type unification by merging the duplicate `ClaudeHook` types that existed only as a bincode workaround.

### Changes

**Modified files:**
- `Cargo.toml` - Replaced `bincode = "1"` with `rmp-serde = "1"`
- `src/error.rs` - Split `Serialization` error variant into `SerializationEncode` and `SerializationDecode` (rmp-serde has separate encode/decode error types)
- `src/message.rs` - Updated `encode()`/`decode()` to use `rmp_serde::to_vec_named`/`from_slice` (named map format for forward/backward compatibility); merged `ClaudeHook` types with tagged serialization format; added `ClaudeSessionStart`, `ClaudePermissionRequest`, `ClaudePermissionTool` structs
- `src/transport.rs` - Updated error mappings from `AmuxError::Serialization` to `AmuxError::SerializationEncode`/`SerializationDecode`
- `src/hooks.rs` - Removed duplicate type definitions (moved to message.rs); removed `TryFrom` conversion (no longer needed); simplified to direct use of unified `ClaudeHook` type
- `src/server.rs` - Updated `ClaudeHook` pattern matching to use tuple variants with structs; added UUID parsing for session_id String→Uuid conversion

### Decisions Made

1. **Named map format:** Using `to_vec_named` instead of `to_vec` serializes structs as maps with field names rather than positional arrays. This makes the protocol robust to field additions, removals, and reordering - servers with different versions can communicate as long as they share common fields.

2. **Unified ClaudeHook type:** Previously had two separate `ClaudeHook` enums - one in `hooks.rs` with `#[serde(tag = "hook_event_name")]` for parsing Claude's JSON, and one in `message.rs` untagged for bincode. Msgpack supports tagged enums, so we can use a single type with Claude's tagged format.

3. **Uuid session_id with serde:** Using `Uuid` type directly in `ClaudeSessionStart` and `ClaudePermissionRequest` - serde handles string↔UUID conversion automatically during serialization. Invalid UUIDs fail at deserialization time rather than requiring manual parsing in server.rs.

4. **Drop unused fields:** Removed `cwd` and `source` fields from `ClaudeSessionStart` - they were never used. Serde ignores unknown fields by default, so Claude's JSON with those fields still deserializes correctly.

5. **Split error variants:** rmp-serde has separate `encode::Error` and `decode::Error` types, so we split the error variant to get proper `From` impls.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6 E2E tests pass
```

---

## 2025-01-18: Fix agent detach (Ctrl-b d)

### Summary

Fixed the broken Ctrl-b d detach functionality. Two issues were addressed:

1. The client would get stuck because the stdin task set an `AtomicBool` flag and exited, but the main `select!` loop was blocked waiting for server messages and never checked the flag.

2. Modern terminal emulators (iTerm2, kitty, WezTerm) use the CSI u / kitty keyboard protocol which sends `ESC[98;5u` for Ctrl-b instead of the raw byte `0x02`. The code only checked for raw `0x02`, so Ctrl-b was never detected outside of tmux.

### Changes

**Modified files:**
- `src/client.rs` - Added `StdinEvent` enum (`Data(Vec<u8>)` or `Detach`); stdin task detects both raw Ctrl-b (`0x02`) and CSI u Ctrl-b (`ESC[98;5u`); sends `StdinEvent::Detach` through channel; main loop handles detach by breaking and closing connection

### Decisions Made

1. **StdinEvent enum over AtomicBool flag**: Using an enum through the channel lets the main loop react immediately when it receives the detach event, rather than polling a flag at the start of each iteration (which never happened because the loop was blocked in `select!`).

2. **Support both keyboard protocols**: Traditional terminals and tmux send raw `0x02` for Ctrl-b. Modern terminals use CSI u format (`ESC[98;5u`). The code now handles both.

3. **Connection close is sufficient**: For local terminal clients, simply closing the connection triggers automatic cleanup - the server's streaming task detects the closed channel and exits, dead subscribers are removed on next buffer write. No explicit protocol handshake needed.

### Verification

- `cargo check && cargo fmt && cargo clippy && cargo test` - 31 tests pass
- `cargo run -p e2e-runner -- run` - 6 E2E tests pass
- Manual testing: Ctrl-b d works both inside tmux and in native terminal (iTerm2)

---

## 2025-01-18: Add permission request handling for Claude Code

### Summary

Implemented permission request handling allowing the dashboard to display Edit permission requests from Claude Code and send approve/deny responses back to the agent via keystroke emulation. When Claude Code wants to edit a file, the PermissionRequest hook sends the request to the server, which forwards it to WebSocket subscribers. The dashboard displays a card with a diff view and Yes/Yes(all)/No buttons. User responses are converted to keystrokes (1/2/3) and sent to the agent's PTY.

### Changes

**Rust (amux core):**
- `src/structured_log.rs` - Added `PermissionRequest` variant to `StructuredLog` and `PermissionTool` enum with `Edit` variant
- `src/message.rs` - Added `PermissionResponse` enum (Yes/YesAll/No), `ClaudeHook::PermissionRequest` variant, and `Message::PermissionRequestResponse`
- `src/hooks.rs` - Added `ClaudePermissionTool` enum with `#[serde(tag = "tool_name", content = "tool_input")]` for direct deserialization of Claude's JSON format; `ClaudePermissionRequest` uses `#[serde(flatten)]` to pull tool fields from parent object; simple `From` conversion to wire protocol types
- `src/main.rs` - Added `PermissionRequest` CLI subcommand under hooks
- `src/server.rs` - Handle `PermissionRequest` hook (writes to log buffer) and `PermissionRequestResponse` message (sends keystroke to agent)
- `src/session.rs` - Added `write_log()` method for direct log entry injection

**Dashboard (React):**
- `src/types/protocol.ts` - Added `PermissionTool`, `PermissionRequest`, `PermissionResponse` types and type guard
- `src/components/DiffView.tsx` - New component showing old vs new text with red/green styling
- `src/components/PermissionRequestCard.tsx` - New component with file path, diff view, and response buttons
- `src/components/Message.tsx` - Handle `PermissionRequest` rendering
- `src/components/ChatWindow.tsx` - Pass permission response callback to Message
- `src/hooks/useWebSocket.ts` - Added `sendPermissionResponse()` function
- `src/contexts/WebSocketContext.tsx` - Exposed `sendPermissionResponse` in context

### Decisions Made

- **Use serde's full power for Claude JSON parsing**: Claude sends `tool_name` + `tool_input` as separate fields. Instead of manual parsing with `serde_json::Value`, we use `#[serde(tag = "tool_name", content = "tool_input")]` (adjacently-tagged) with `#[serde(flatten)]` to deserialize directly into typed structs. No custom parsing code needed.
- **Keystroke-based response**: Claude Code's TUI accepts 1/2/3 for Yes/Yes(all)/No - simple single character responses, no complex sequences needed.
- **Permission request in log buffer**: Permission requests are written to the structured log buffer alongside messages, allowing WebSocket clients to receive them in the message stream.

### Verification

- `cargo check` - No errors
- `cargo fmt` - Clean
- `cargo clippy` - No warnings
- `cargo test` - 31 tests pass
- `cargo run -p e2e-runner -- run` - 6 e2e tests pass
- `npm run build` (dashboard) - TypeScript compiles successfully
- **Playwright MCP e2e test** - Verified full flow: send message → Claude responds → Edit permission request displays with diff → click "No" → response sent

### Next Steps

- Consider adding support for other permission tool types beyond Edit

---

## 2025-01-17: Fix subscriber leak in MultiplexBuffer and MultiplexLogBuffer

### Summary

Fixed a resource leak where dead subscribers (disconnected clients) accumulated in the `subscribers` list of `MultiplexBuffer` and `MultiplexLogBuffer`. When a `MultiplexReader` was dropped, the corresponding `mpsc::UnboundedSender` remained in the list indefinitely, causing unbounded growth for long-running agents with many attach/detach cycles.

### Changes

**Modified files:**
- `src/buffer.rs` - Changed `write()` to use `subs.retain(|tx| tx.send(...).is_ok())`
- `src/multiplex_log_buffer.rs` - Same pattern applied to `write()`

### Decisions Made

1. **Use send error instead of `is_closed()`**: `UnboundedSender::send()` returns `SendError` only when the receiver has dropped. Using `retain(|tx| tx.send(...).is_ok())` combines broadcast and cleanup in a single pass, and respects the error rather than discarding it with `let _ = ...`.

2. **Write lock instead of read lock**: Changed `subscribers.read()` to `subscribers.write()` to enable `retain()`. Lock ordering analysis confirmed no deadlock risk - both `write()` and `subscribe()` acquire locks in the same order (buffer → subscribers).

### Verification

- `cargo check && cargo fmt && cargo clippy && cargo test` - all 31 tests pass
- `cargo run -p e2e-runner -- run` - all 6 E2E tests pass

### Next Steps

- None - fix is complete

---

## 2025-01-17: Enable dashboard input with SubmitInput message

### Summary

Added the ability to send input from the React dashboard to Claude agents. This required refactoring WebSocket handling to match the Unix/TCP patterns (using `tokio::select!` with outgoing channels) and creating a new `SubmitInput` message type that handles the timing requirements for Claude Code to interpret Enter as "submit" rather than "newline".

### Changes

**Backend (src/):**
- `message.rs` - Renamed `Input` to `InputBytes` (raw bytes, no auto-enter) and added `SubmitInput` (writes data, waits 20ms, sends `\r`)
- `server.rs` - Refactored `websocket_accept` and `websocket_client_loop` to use outgoing channels and `tokio::select!` (matching Unix/TCP pattern); Subscribe handler now spawns streaming task instead of blocking; Added `SubmitInput` handling for WebSocket/TCP
- `client.rs` - Updated to use `InputBytes`
- `transport.rs` - Updated tests to use `InputBytes`

**Frontend (dashboard/src/):**
- `types/protocol.ts` - Added `SubmitInput` message type
- `contexts/WebSocketContext.tsx` - New file: context provider for sharing WebSocket connection
- `hooks/useWebSocket.ts` - Added `sendInput` callback using `SubmitInput`
- `components/InputArea.tsx` - Enabled input with state management and submit handlers
- `components/AgentSidebar.tsx` - Updated to use context
- `App.tsx` - Wrapped with `WebSocketProvider`

### Decisions Made

1. **Two message types (InputBytes vs SubmitInput)**: Raw terminal clients use `InputBytes` for direct byte passthrough. The dashboard uses `SubmitInput` which adds a 20ms delay between text and Enter to ensure Claude Code interprets them as separate events.

2. **20ms delay with `\r`**: Claude Code distinguishes submit vs newline based on PTY read boundaries. The delay ensures text and Enter arrive as separate reads. Started with 100ms, reduced to 20ms after testing.

3. **WebSocket only supports SubmitInput**: Removed `InputBytes` from WebSocket handler since rich clients should use the higher-level submit semantics.

### Verification

- Backend: `cargo check && cargo fmt && cargo clippy && cargo test` - all pass
- Frontend: `npm run build` - builds successfully
- Manual test with Playwright: sent message from dashboard, Claude responded correctly

### Next Steps

- Consider if 20ms delay is optimal or if there's a better approach
- Add reconnection logic to dashboard WebSocket

---

## 2025-01-17: Use Uuid type for agent identifiers

### Summary

Changed `agent_id` from `String` to `uuid::Uuid` type for type safety. This ensures agent IDs are always valid UUIDs at compile time. The change affects `CreateAgentRequest`, `AgentInfo`, `LocalAgentSession`, `SessionEvent`, and the agents `HashMap` key.

Routable protocol messages (`Subscribe`, `Input`, `Output`, `SubscribeResult`, `StructuredOutput`) keep `agent_id` as `String` to support both UUID and alias lookups.

### Changes

**Modified files:**
- `Cargo.toml` - Added `serde` feature to uuid dependency
- `src/message.rs` - `CreateAgentRequest.agent_id` and `AgentInfo.agent_id` now `Uuid`; `ClaudeHook::SessionStart.session_id` now `Uuid`
- `src/session.rs` - `LocalAgentSession.agent_id` and `SessionEvent::Ended` now use `Uuid`
- `src/server.rs` - `ServerState.agents` HashMap keyed by `Uuid`; updated lookups
- `src/client.rs` - Generate `Uuid` directly, convert to string for display
- `src/hooks.rs` - `TryFrom` conversion to parse session_id as UUID
- `src/transport.rs` - Updated tests to use `Uuid::new_v4()`

### Decisions Made

1. **Uuid for creation/storage, String for lookups**: `CreateAgentRequest` and internal storage use `Uuid` for type safety. Protocol messages for subscribe/input use `String` to support alias-based lookups.

2. **Future consideration**: Could move alias resolution to the client side, making all protocol messages use `Uuid` exclusively. This would require adding alias resolution to the protocol (e.g., a ResolveAlias message).

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6 E2E tests pass
```

---

## 2025-01-17: Remove AgentId struct, simplify to UUID

### Summary

Removed the `AgentId` struct (which contained `host_id`, `user_id`, `agent_id`) and replaced it with a plain UUID string. The struct's `host_id`/`user_id` fields were vestigial - routing uses `src_host`/`dst_host` message fields, and agents are keyed by UUID only. Also simplified `LocalAgentSession::new()` to take `&CreateAgentRequest` directly.

### Changes

**Modified files:**
- `src/session.rs` - Removed `AgentId` struct; changed `LocalAgentSession.id` to `agent_id: String`; simplified `new()` to take `&CreateAgentRequest`
- `src/server.rs` - Updated `create_agent()` to use new API; updated `SessionEvent::Ended` handling
- `ARCHITECTURE.md` - Added implementation note explaining the simplification
- `CLAUDE.md` - Updated core types section

### Decisions Made

1. **AgentId struct removed**: The tuple was designed for global uniqueness, but routing evolved to use `src_host`/`dst_host` message fields. Keeping the struct added complexity without benefit.

2. **LocalAgentSession::new() takes &CreateAgentRequest**: Reduces parameter count and keeps request data together.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6 E2E tests pass
```

---

## 2025-01-17: UUID-based Agent IDs with Alias Support

### Summary

Changed `agent_id` from a user-provided string to an auto-generated UUID. The `-t` flag now sets an optional human-readable alias. This fixes Claude integration which requires `--session-id` to be a valid UUID.

### Changes

**Modified files:**
- `src/message.rs` - Added `CreateAgentRequest` struct, changed `CreateAgent` to tuple variant; added `alias: Option<String>` to `AgentInfo`
- `src/session.rs` - Added `alias: Option<String>` field to `LocalAgentSession`, updated `new()` and `to_agent_info()`
- `src/client.rs` - Changed `new_agent()` to generate UUID for `agent_id` and accept alias; updated `list_agents()` to prefer alias when displaying
- `src/server.rs` - Added `resolve_agent()` helper for UUID/alias lookup; refactored `create_agent()` to accept `CreateAgentRequest`; updated Subscribe/Input handlers to use alias lookup
- `src/main.rs` - Changed `-t` flag from required with default to optional
- `src/transport.rs` - Updated test to use `CreateAgentRequest`

### Decisions Made

1. **UUID generated by client**: The client generates the UUID before sending `CreateAgent`. This keeps UUID generation close to the user interaction.

2. **Alias is optional**: Users can create agents without an alias (`amux new-agent claude`). The UUID will be used for display and lookup in that case.

3. **Prefer alias in display**: `list-agents` shows alias when available, falling back to UUID. This keeps E2E tests working (they use aliases) and is human-friendly.

4. **Resolve by either**: `resolve_agent()` helper first tries UUID lookup, then falls back to alias scan. This allows users to attach/input using either identifier.

5. **Alias uniqueness enforced**: Creating an agent with a duplicate alias returns an error, just like duplicate UUIDs.

6. **CreateAgentRequest struct**: Extracted fields from `Message::CreateAgent` into a reusable struct. This reduces `create_agent()` from 8 parameters to 3, fixing the clippy `too_many_arguments` warning.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK (no warnings)
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6 E2E tests pass
```

### Next Steps

- Manual test with Claude to verify hook linking works with UUIDs

---

## 2025-01-16: Robust Agent Session ID Linking

### Summary

Made agent creation robust by passing `--session-id=<agent_id>` to Claude when spawning, then using that session_id to look up the correct agent when the SessionStart hook arrives. This replaces the fragile `agents.iter().last()` hack with proper session-based lookup.

### Changes

**Modified files:**
- `src/message.rs` - Added `AgentType` enum (`Claude`, `TestAgent(String)`), updated `CreateAgent` message to use `agent_type` instead of `command`, added `session_id` field to `ClaudeHook::SessionStart`
- `src/hooks.rs` - Updated `From<ClaudeHook>` conversion to preserve `session_id` in the wire protocol
- `src/session.rs` - Updated `LocalAgentSession::new` to accept `AgentType`, builds command/args based on type (Claude gets `--session-id=<agent_id>`)
- `src/server.rs` - Updated `create_agent` to accept `AgentType`, changed HookEvent handler to look up agents by `session_id` instead of `agents.iter().last()`
- `src/client.rs` - Updated `new_agent` to accept `AgentType`
- `src/main.rs` - Added `parse_agent_type` function with TODO for future ValueEnum migration
- `src/transport.rs` - Updated test to use `AgentType`

### Decisions Made

- **AgentType enum over command string**: Type safety ensures only known agent types can be created. `TestAgent(String)` variant holds the command/path for E2E test flexibility.
- **TestAgent only in debug builds**: Uses `#[cfg(any(debug_assertions, test))]` to exclude test-agent from release builds.
- **Flexible parse_agent_type**: Accepts both "test-agent" and full paths ending in "test-agent" to support E2E executor's path substitution. Added TODO to switch to Clap's ValueEnum once E2E executor can call binaries directly.
- **session_id = agent_id**: We pass the amux agent target name (e.g., "myagent") as Claude's `--session-id`, so when the hook arrives we can directly look up `agents.get(session_id)`.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run            # 6 E2E tests pass
```

### Next Steps

- Once E2E executor can call binaries directly, switch to Clap's ValueEnum for cleaner CLI parsing

---

## 2025-01-16: Comment Cleanup and Commenting Guidelines

### Summary

Cleaned up comments across the codebase to follow a consistent commenting philosophy: comments should add value, not restate the obvious. Added commenting guidelines to CLAUDE.md to establish standards for future development.

### Changes

**Modified files:**
- `CLAUDE.md` - Added "Commenting Guidelines" section with good/bad examples
- `src/config.rs` - Removed redundant field doc comments that just echoed field names
- `src/transport.rs` - Added module-level doc comment explaining frame format; removed duplicate frame format comments from individual methods; removed obvious inline comments
- `src/server.rs` - Removed obvious inline comments while keeping important invariants and Task labels

### Commenting Philosophy Established

**Good comments (keep):**
- `///` doc comments on public APIs explaining behavior, invariants, return values
- `//!` module-level doc comments explaining purpose and guarantees
- WHY comments explaining non-obvious decisions or constraints
- `// Task:` labels on spawned async blocks
- Important invariants (e.g., "routes table uses single-layer keys only")

**Bad comments (remove):**
- Comments restating what the next line obviously does
- Comments echoing the variable/function name
- Duplicate documentation (e.g., frame format repeated 4 times)

### Files Reviewed But Not Changed

- `src/buffer.rs` and `src/multiplex_log_buffer.rs` - Already excellent documentation with invariants
- `src/session.rs` - Good Task labels and public API docs
- `src/client.rs` - Good Task labels, flow comments help navigation
- `src/hooks.rs` - Good WHY comment explaining enum duplication

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6/6 E2E tests pass
```

---

## 2025-01-16: Fix E2E remote_connection Test WebSocket Port Conflict

### Summary

Fixed a CI failure in the `remote_connection` E2E test. When running tests with multiple server configs, each server was getting a unique TCP port but defaulting to the same WebSocket port (9002). This caused the second server to fail to start with "Server failed to start" because port 9002 was already bound by the first server.

### Changes

**Modified files:**
- `e2e-runner/src/parser.rs` - Added `websocket_port: Option<u16>` field to `TestConfig`
- `e2e-runner/src/executor.rs` - Added auto-assignment of unique WebSocket port for each config (matching tcp_port pattern), added `websocket_port` to generated YAML config

### Root Cause

The executor was generating YAML configs with only `tcp_port` specified:
```yaml
host_id: "host-b"
user_id: "test"
socket_path: "/tmp/amux-test-remote_connection-server_b.sock"
tcp_port: 49188
```

Without `websocket_port`, the server used the default (9002). When two servers started, both tried to bind port 9002, causing the second to fail.

### Fix

Added `websocket_port` to `TestConfig` (matching how `tcp_port` is handled):
```rust
pub struct TestConfig {
    // ...
    pub websocket_port: Option<u16>,
}
```

And auto-assignment in the environment setup (matching tcp_port pattern):
```rust
let ws_port = match cfg.websocket_port {
    Some(p) => p,
    None => {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    }
};
```

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 31 tests pass
cargo run -p e2e-runner -- run             # 6/6 E2E tests pass (including remote_connection)
```

---

## 2025-01-15: WebSocket Subscription to Structured Logs

### Summary

Added WebSocket connections that subscribe to structured conversation logs (user/assistant messages) parsed from Claude's transcript file. When a WebSocket client subscribes to an agent, it receives structured log entries instead of raw PTY bytes. The hook handler now sends a `HookEvent` to the server to link transcripts to sessions.

### Changes

**New files:**
- `src/structured_log.rs` - `StructuredLog` enum (UserMessage, AssistantMessage)
- `src/multiplex_log_buffer.rs` - `MultiplexLogBuffer` and `MultiplexLogReader` for log broadcast
- `src/transcript.rs` - `TranscriptTailer` that parses Claude's JSONL transcript

**Modified files:**
- `src/message.rs` - Added `HookEvent`, `HookEventResult`, `StructuredOutput` message types
- `src/session.rs` - Added `log_buffer`, `link_transcript()`, `subscribe_logs()` to `LocalAgentSession`
- `src/hooks.rs` - Updated to send `HookEvent` to server instead of just logging
- `src/transport.rs` - Added `WebSocketTransport` with JSON serialization
- `src/config.rs` - Added `websocket_port` field (default 9002)
- `src/server.rs` - Added WebSocket listener, `websocket_accept()`, `websocket_client_loop()`, HookEvent handling
- `src/main.rs` - Added module declarations for new files
- `Cargo.toml` - Added `tokio-tungstenite` and `futures-util` dependencies

### Decisions Made

1. **Separate `MultiplexLogBuffer`:** Logs need entry-count limits, not byte limits, so a separate buffer type was created (not generic MultiplexBuffer).

2. **Connection-type determines subscription:** WebSocket subscribes to logs, Unix/TCP subscribes to bytes. No new Subscribe message variants needed - same protocol, different behavior.

3. **JSON serialization for WebSocket:** Human-readable and web-friendly, vs bincode for Unix/TCP.

4. **Session linking via most-recent:** HookEvent links transcript to the most recently created session (simplest approach for single-user local mode).

5. **Transcript parsing extracts text only:** Assistant messages with thinking blocks are parsed to extract just the text content, ignoring tool use and thinking.

6. **Runtime nesting fix:** The hook command runs through `#[tokio::main]`, so creating a nested runtime panicked. Fixed by using `tokio::task::block_in_place` with `Handle::current().block_on()`.

7. **Separate Hook enums for JSON vs wire protocol:** Bincode doesn't support internally tagged enums (`#[serde(tag = "...")]` fails with `DeserializeAnyNotSupported`). We maintain `hooks::ClaudeHook` with serde tag for parsing Claude's JSON input, and `message::ClaudeHook` untagged for bincode wire protocol, with a `From` impl to convert between them.

8. **Proper transcript parsing with serde:** Refactored `transcript.rs` to use serde tagged enums (`TranscriptEntry`, `ContentBlock`) instead of manual string matching. Uses `#[serde(other)]` for catch-all variants and railway-oriented `.ok().and_then()` for clean parsing flow.

### Verification

```
cargo check                        # OK
cargo fmt                          # OK
cargo clippy                       # OK
cargo test                         # 31 tests pass
```

Tests added: 7 for MultiplexLogBuffer, 6 for TranscriptTailer parsing, 1 for HookEvent bincode roundtrip.

### Manual Testing

```bash
# Terminal 1: Start Claude agent via amux
cargo run -- new-agent -t test1 claude

# Terminal 2: Connect via WebSocket
websocat ws://localhost:9002
{"Connect":{"host_id":"ws-client"}}
# Response: {"ConnectResponse":{"success":true,"error":null,"host_id":"<server-host-id>"}}

{"Subscribe":{"src_host":"","dst_host":"<server-host-id>","agent_id":"test1","rows":24,"cols":80}}
# Response: {"SubscribeResult":{...,"success":true,...}}

# As conversation happens in Terminal 1, structured logs stream to Terminal 2:
{"StructuredOutput":{...,"entry":{"type":"UserMessage","content":"Hi Claude...","timestamp":"...","uuid":"..."}}}
{"StructuredOutput":{...,"entry":{"type":"AssistantMessage","content":"Let me check...","timestamp":"...","uuid":"..."}}}
```

User and assistant messages stream in real-time as the conversation progresses.

### Next Steps

- Add E2E tests for WebSocket subscription flow
- Consider environment variable for session linking (AMUX_SESSION_ID)

---

## 2025-01-15: Claude Code Hooks Integration (SessionStart)

### Summary

Added initial support for Claude Code hooks integration. The new `amux hooks claude session-start` command receives hook data from Claude Code via stdin and logs it to `claude_hooks.jsonl` in the current working directory. Hook data is parsed into typed structs (`ClaudeHook` enum) for type safety.

### Changes

**New files:**
- `src/hooks.rs` - Hook handling with `ClaudeHook` enum and `ClaudeSessionStart` struct

**Modified files:**
- `src/main.rs` - Added `Hooks` command with nested `HooksProvider` and `ClaudeHookEvent` enums
- `Cargo.toml` - Added `serde_json` dependency

**New types:**
```rust
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
}

pub struct ClaudeSessionStart {
    pub cwd: String,
    pub session_id: String,
    pub source: String,  // "startup", "resume", "clear"
    pub transcript_path: String,
}
```

### Decisions Made

1. **Hidden command:** The `hooks` command is hidden from `--help` using `#[command(hide = true)]`. Like the `--server` flag, it's internal infrastructure.

2. **Nested subcommand structure:** `amux hooks <provider> <event>` allows future expansion to other providers (opencode, codex) and events (PreToolUse, PostToolUse).

3. **Typed parsing:** Hook data is parsed into `ClaudeHook` enum using serde's internally tagged enum (`#[serde(tag = "hook_event_name")]`). This gives type safety and easy access to fields like `transcript_path`.

4. **Fail silently:** Errors are logged to `/tmp/amux.log` but the command exits with code 0. Hooks should not block Claude Code workflow.

5. **Log to cwd:** `claude_hooks.jsonl` is created in the current working directory (the project root when invoked by Claude Code).

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 17 tests pass
cargo run -p e2e-runner -- run             # 6/6 E2E tests pass
```

Manual testing:
```bash
# Basic invocation with real Claude Code format
echo '{"cwd":"/Users/jlw","hook_event_name":"SessionStart","session_id":"test","source":"startup","transcript_path":"~/.claude/test.jsonl"}' | cargo run -- hooks claude session-start
cat claude_hooks.jsonl  # Shows typed entry

# Hidden from help
cargo run -- --help  # Does not show "hooks"
```

### Next Steps

- Add more Claude Code hook events (PreToolUse, PostToolUse, Stop)
- Read transcript file to capture conversation messages
- Consider per-agent abstractions for different AI tools

---

## 2025-01-15: Remove unix_subscribed_mode

### Summary

Removed `unix_subscribed_mode` and the `UnixAction` enum to achieve symmetry with the TCP flow. The subscribed mode was a leftover from the removed "raw mode" optimization (2025-01-13). Now Subscribe spawns an output streaming task and the main loop continues handling all messages.

### Changes

**Removed:**
- `UnixAction` enum (Continue, EnterSubscribed, Shutdown variants)
- `unix_subscribed_mode` function (~60 lines)

**Modified in `src/server.rs`:**
- `unix_handle_message` now returns `Result<()>`
- Subscribe handling spawns output task via route channel
- Input handling checks if dst_host is local before routing
- Shutdown handling inlines process::exit
- `unix_client_loop` simplified to a select! loop

### Decisions Made

1. **Spawn output task on Subscribe:** When Subscribe succeeds, spawn a task that reads from buffer_reader and sends Output messages via the client's route channel. Main loop continues handling all messages.

2. **Don't store input_tx:** On Input messages, look up the agent via `agents.get(&agent_id)` and call `send_input` directly. No need to store subscription state.

3. **Inline shutdown:** The handler calls `process::exit(0)` directly instead of returning an action.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 17 tests pass
cargo run -p e2e-runner -- run             # 6/6 E2E tests pass
```

### Benefits

1. **Commands while attached:** Since the main loop continues, clients can send other messages while subscribed
2. **Simpler code:** Removed ~80 lines (enum + function + action matching)
3. **No special types:** No UnixAction enum, no state machine transitions

---

## 2025-01-15: Symmetric Connection Handler Refactoring

### Summary
Refactored Unix and TCP connection handlers to have symmetric naming and structure. The protocol was designed for transport-agnostic routing, and this refactor makes the code reflect that symmetry while keeping Unix and TCP handlers separate (since they serve different roles: local clients vs peer servers).

### Changes

**Renamed functions for clarity:**
- `handle_connection` → `unix_accept` (Unix bootstrap)
- `handle_unix_client_loop` → `unix_client_loop` (Unix message loop)
- `handle_subscribed_mode` → `unix_subscribed_mode` (Unix subscribed streaming)
- `handle_inbound_tcp` → `tcp_accept` (TCP bootstrap)
- `handle_tcp_connection` → `tcp_peer_loop` (TCP message loop)
- `handle_tcp_message` → `tcp_handle_message` (TCP message handler)
- `handle_connect_to_server` → `tcp_connect` (TCP outbound connection)

**Added new types:**
- `UnixClientContext` - bundles state, event_tx, client_host_id, our_host
- `UnixAction` enum - Continue, EnterSubscribed, Shutdown

**Extracted message handler:**
- `unix_handle_message` - extracted from inline code in unix_client_loop

### Decisions Made

1. **Keep handlers separate:** Unix and TCP serve different roles (local client vs peer server). Merging would add conditional complexity without meaningful simplification.

2. **Symmetric naming pattern:**
   - Bootstrap: `unix_accept`, `tcp_accept`
   - Loop: `unix_client_loop`, `tcp_peer_loop`
   - Handler: `unix_handle_message`, `tcp_handle_message`

3. **UnixAction enum for state transitions:** Unix can transition to subscribed mode, which requires an action enum. TCP doesn't have this need, so it just returns Result<()>.

4. **Context struct only for Unix:** UnixClientContext bundles parameters cleanly. TCP's tcp_handle_message only needs state, so no context struct was added.

### Verification

```
cargo check && cargo fmt && cargo clippy  # OK
cargo test                                 # 17 tests pass
cargo run -p e2e-runner -- run             # 6/6 E2E tests pass
```

### Next Steps (Phase 2)

Consider removing `unix_subscribed_mode` in favor of spawned output tasks (like TCP does). This would:
- Allow commands while subscribed (like tmux's Ctrl-b + s)
- Enable clean Unsubscribe message for detaching
- Make Unix and TCP even more symmetric

---

## 2025-01-15: Dead code cleanup

### Summary
Aggressive cleanup of unused code from the initial architecture plan. Removed unused structs, methods, error variants, and function parameters. Re-enabled dead_code warnings in CI.

### Changes

**Removed files:**
- `src/connection.rs` - Only contained unused `ConnectionId` struct

**Modified files:**
- `src/server.rs`:
  - Removed `next_connection_id` field and `next_conn_id()` method
  - Removed `Server::new()` (only `with_config` is used)
  - Removed unused `local_client_id` parameter from `handle_unix_client_loop`
  - Removed unused `transport` parameter from `handle_tcp_message`
  - Removed unused `event_tx` parameter from `handle_tcp_connection`, `handle_inbound_tcp`, `handle_connect_to_server`
  - Updated log messages to use meaningful identifiers (`client_host_id`, `agent_id`) instead of opaque connection IDs
- `src/session.rs` - Removed unused `is_alive()` method
- `src/buffer.rs` - Removed unused `is_closed()` method and its test
- `src/error.rs` - Removed unused `NotSubscribed` and `ConnectionClosed` variants
- `src/main.rs` - Removed `mod connection` declaration
- `e2e-runner/src/parser.rs` - Removed unused `description` field from `TestCase`
- `.github/workflows/ci.yml` - Removed `-A dead_code` exception from clippy
- `CLAUDE.md` - Updated file list and structure, added `cargo check` to workflow

### Decisions Made
- Be aggressive about removing dead code (YAGNI) - can always add back when needed
- Remove code only used in tests if the underlying feature is unused
- Re-enable dead_code lint in CI to catch future issues

### Verification
- `cargo check && cargo fmt && cargo clippy` - no warnings
- `cargo test` - 17 tests pass
- `cargo run -p e2e-runner -- run` - 6 e2e tests pass
- `cargo clippy --all-targets -- -D warnings` - passes (CI command)

---

## 2025-01-15: Remote Subscriptions with Hierarchical Routing

### Summary
Implemented remote agent subscriptions allowing a client on Server B to attach to an agent running on Server A. This required implementing a hierarchical routing protocol where each server prefixes `src_host` when forwarding upstream and strips its prefix when routing responses downstream. Fixed a critical mutex deadlock by switching from shared transport access to channel-based message passing.

### Changes

**Modified files:**
- `src/server.rs` - Major refactor:
  - Changed `routes` from `HashMap<String, Arc<Mutex<Box<dyn Transport>>>>` to `HashMap<String, mpsc::Sender<Message>>` (channel-based)
  - Added `resolve_route()` function for hierarchical routing (strip prefix, extract next hop)
  - Rewrote `handle_unix_client_loop()` to use `select!` loop with transport reads and channel receives
  - Added `handle_subscribed_mode()` for local subscription streaming
  - Rewrote `handle_tcp_connection()` and added `handle_tcp_message()` with channel-based outgoing messages
  - Added `Message::Input` handling in Unix client loop for forwarding to remote agents
- `src/client.rs` - Added Connect handshake with UUID-based client ID, updated all client functions to use `connect_and_handshake()`
- `src/message.rs` - Updated Connect/ConnectResponse messages with host_id field (done in previous session)
- `src/transport.rs` - Added Transport trait (done in previous session)
- `src/session.rs` - Added `send_input()` method (done in previous session)

### Decisions Made

1. **Hierarchical host IDs with "/" separator:** Client IDs are prefixed by their server's host_id (e.g., "host-b/client-uuid"). This creates a NAT-like routing scheme where each server only knows its immediate neighbors.

2. **Routes table uses single-layer keys:** No nested host_ids in route keys. When routing to "host-b/client-uuid", extract "host-b" as the next hop. This keeps routing logic simple and stateless.

3. **Upstream prefixing / downstream stripping:**
   - Forwarding upstream: prefix `src_host` with our host_id
   - Routing downstream: strip our prefix from `dst_host`, route to first segment of remainder

4. **Channel-based message passing instead of shared transport:** The original design used `Arc<Mutex<Box<dyn Transport>>>` in the routes table, but this caused deadlock:
   - TCP handler holds mutex while blocked on `read_message().await`
   - Unix client handler tries to acquire mutex to write → blocked forever
   - Solution: Store `mpsc::Sender<Message>` in routes. TCP handler owns transport and uses `select!` to read from transport OR receive from channel.

### Verification

```
cargo fmt                           # OK
cargo clippy                        # OK (only dead_code warnings)
cargo test                          # 18 tests pass
cargo run -p e2e-runner -- run      # 6/6 E2E tests pass (including remote_connection)
```

### Message Flow (Remote Subscribe)

```
Client (Server B)                Server B                    Server A
       |                            |                            |
       |-- Subscribe -------------> |                            |
       |   dst=host-a               |-- Subscribe --------------> |
       |   src=""                   |   dst=host-a                |
       |                            |   src=host-b/client-uuid    |
       |                            |                             |
       |                            |<-- SubscribeResult -------- |
       |<-- SubscribeResult ------- |    dst=host-b/client-uuid   |
       |    dst=client-uuid         |                             |
       |                            |<-- Output ----------------- |
       |<-- Output ---------------- |    dst=host-b/client-uuid   |
       |                            |                             |
       |-- Input -----------------> |                             |
       |   dst=host-a               |-- Input ------------------> |
       |                            |   dst=host-a                |
       |                            |   src=host-b/client-uuid    |
```

### Next Steps

- Add agent discovery (AddAgents message) for listing remote agents
- Add cloud mode with WebSocket transport
- Add token-based authentication

---

## 2025-01-13: Add TCP Listener and Server-to-Server Connection Foundation

### Summary
Added the foundation for server-to-server communication. The server now listens on both Unix socket (for local clients) and TCP (for remote servers). Added `amux connect <host:port>` command that tells the local server to connect to a remote server. Connections use a simple handshake protocol before entering the (currently stubbed) server-to-server handler.

### Changes

**Modified files:**
- `src/config.rs` - Added `tcp_port: Option<u16>` field (defaults to 9001), `DEFAULT_TCP_PORT` constant
- `src/message.rs` - Added `ConnectToServer`, `ConnectToServerResult`, `ServerConnect`, `ServerConnectResponse` messages
- `src/transport.rs` - Added `TcpTransport` struct mirroring `UnixTransport` with same framing protocol
- `src/main.rs` - Added `Connect { address }` CLI command
- `src/client.rs` - Added `connect()` function that sends `ConnectToServer` to local server
- `src/server.rs` - Added TCP listener in `run()` using `tokio::select!`, added `handle_connect_to_server()`, `handle_inbound_tcp()`, and stubbed `handle_tcp_connection()`

### Decisions Made

1. **TCP port in config is optional:** The `tcp_port` field is `Option<u16>` so it's optional in YAML config files. Server always creates TCP listener using `config.tcp_port.unwrap_or(DEFAULT_TCP_PORT)`.

2. **Connect goes through local server:** The `amux connect` command doesn't make a direct connection. It sends `ConnectToServer` to the local server via Unix socket, and the local server makes the outbound TCP connection. This keeps connection state managed by the server.

3. **Simple handshake protocol:** Initiator sends `ServerConnect` (empty), receiver responds with `ServerConnectResponse { success, error }`. Any unexpected message before handshake completes closes the connection.

4. **Stubbed handler:** `handle_tcp_connection()` is called after handshake succeeds on both sides. Currently just logs and returns - protocol implementation deferred to next milestone.

### Verification

```
cargo fmt                           # OK
cargo clippy                        # OK (only dead_code warnings)
cargo test                          # 18 tests pass
cargo run -p e2e-runner -- run      # 5/5 E2E tests pass
```

### Next Steps

- Implement server-to-server protocol in `handle_tcp_connection`:
  - AddAgents for agent discovery
  - Subscribe/Output forwarding
  - Routing table updates

---

## 2025-01-13: Remove Raw Mode, Simplify to Message-Based Streaming

### Summary
Removed the raw byte mode optimization for local Unix domain sockets. After subscribe, bytes now flow via `Output` and `Input` messages instead of switching to unframed raw bytes. Also removed the `SubscriptionHandle` abstraction and `Transport` trait, simplifying to just inherent methods on `UnixTransport`.

### Changes

**Modified files:**
- `src/message.rs` - Added `Output { data: Vec<u8> }` and `Input { data: Vec<u8> }` message variants
- `src/session.rs` - Removed `SubscriptionHandle` and `SubscriptionSender`; `subscribe()` now returns `Option<(MultiplexReader, mpsc::Sender<Vec<u8>>)>` directly
- `src/server.rs` - Removed `handle_raw_mode()` function; subscribe handler now uses `tokio::select!` loop with `Output`/`Input` messages
- `src/client.rs` - Rewrote `run_attached()` to use message-based I/O with mpsc channel bridging blocking stdin
- `src/transport.rs` - Removed `Transport` trait; made `read_frame()`/`write_frame()` private; removed `read_raw()`, `write_raw()`, `flush()`, `into_split()` methods
- `Cargo.toml` - Removed `async-trait` dependency (using native async traits in Rust 1.75+)

### Decisions Made

1. **No raw mode:** The optimization was premature - message framing overhead is negligible for local sockets. Consistent message-based protocol simplifies the codebase and makes debugging easier.

2. **Remove SubscriptionHandle:** The abstraction added complexity without clear benefit. Session now exposes `MultiplexReader` and input sender directly.

3. **Remove Transport trait:** With only one transport type (UnixTransport) for now, the trait was unnecessary indirection. Methods are now inherent to `UnixTransport`. Can add trait back when implementing TCP/WebSocket transports.

4. **Native async traits:** Rust 1.75+ supports async functions in traits natively. Removed `async-trait` crate dependency.

5. **Channel bridge for stdin:** Client still needs `spawn_blocking` for stdin (no async stdin in std), but uses an mpsc channel to bridge to the async select! loop instead of raw socket writes.

### Verification

```
cargo fmt                           # OK
cargo clippy                        # OK (only dead_code warnings)
cargo test                          # 18 tests pass
cargo run -p e2e-runner -- run      # 5/5 E2E tests pass
```

### Message Flow (Updated)

```
Client                              Server
  |                                    |
  |-- CreateAgent -------------------> |
  |<-- CreateAgentResult ------------- |
  |-- Subscribe ---------------------> |
  |<-- SubscribeResult --------------- |
  |                                    |
  |<-- Output { data } --------------- |  (PTY output as messages)
  |-- Input { data } ----------------> |  (user input as messages)
  |<-- Output { data } --------------- |
  |...                                 |
```

---

## 2025-01-11: SubscriptionHandle - Simplify Session/Server Interface

### Summary
Introduced `SubscriptionHandle` as a unified abstraction for interacting with agent sessions. The handle bundles output reading and input sending into a single object returned by `subscribe()`. This simplifies the server code by removing the need to track session references and connection state separately.

### Changes

**Modified files:**
- `src/session.rs` - Added `SubscriptionHandle` and `SubscriptionSender` structs; `subscribe()` now returns `SubscriptionHandle` instead of `MultiplexReader`; removed `send_input()` method (now encapsulated in handle)
- `src/server.rs` - Simplified `handle_subscribe()` to return just `SubscriptionHandle`; simplified `handle_raw_mode()` to take just `SubscriptionHandle`; removed `LocalConnectionState` usage
- `src/connection.rs` - Removed `LocalConnectionState` struct (no longer needed)

### Decisions Made

1. **SubscriptionHandle naming:** Named it "subscription" rather than "session" because you're subscribing to a session - the handle represents your subscription, not the session itself. Multiple clients can have their own handles to the same session.

2. **`send()` instead of `write()`:** The method is named `send()` to make the direction clear - you're sending input TO the agent, not "writing" to the subscription.

3. **`split()` method:** Added `split()` to decompose the handle into `(MultiplexReader, SubscriptionSender)` for use in separate async tasks. This matches the pattern of `transport.into_split()`.

4. **Remove LocalConnectionState:** The `subscribed_agent` and `raw_mode` fields were written but never read - pure bookkeeping with no consumers. Removed entirely.

### Verification

```
cargo fmt                           # OK
cargo clippy --workspace            # OK (existing dead_code warnings only)
cargo test --workspace              # 26 tests pass (20 amux, 6 e2e-runner)
cargo run -p e2e-runner -- run      # 5/5 E2E tests pass
```

### API Summary

```rust
pub struct SubscriptionHandle { ... }

impl SubscriptionHandle {
    pub async fn read(&mut self) -> Option<Vec<u8>>;
    pub async fn send(&self, data: Vec<u8>) -> Result<()>;
    pub fn split(self) -> (MultiplexReader, SubscriptionSender);
}

pub struct SubscriptionSender { ... }

impl SubscriptionSender {
    pub async fn send(&self, data: Vec<u8>) -> Result<()>;
}
```

The flow is now:
```
session.subscribe() -> SubscriptionHandle
handle.read()       -> bytes from agent
handle.send(bytes)  -> bytes to agent
```

---

## 2025-01-11: MultiplexBuffer - Fix Replay/Broadcast Race Condition

### Summary
Replaced the separate `replay_buffer` + `broadcast_tx` architecture with a unified `MultiplexBuffer` abstraction. This eliminates a race condition where data could be lost between getting the replay buffer and subscribing to the broadcast channel. The new design provides atomic subscribe semantics - subscribers receive all existing bytes plus all future bytes with no gaps or duplicates.

### Changes

**New files:**
- `src/buffer.rs` - MultiplexBuffer and MultiplexReader implementations with 11 unit tests

**Modified files:**
- `src/main.rs` - Added `mod buffer`
- `src/session.rs` - Replaced `replay_buffer` + `broadcast_tx` fields with single `MultiplexBuffer`; simplified PTY reader task; updated `subscribe()` to return `MultiplexReader`
- `src/server.rs` - Simplified `handle_subscribe()` (no separate replay + subscribe); updated `handle_raw_mode()` to use `MultiplexReader`
- `src/client.rs` - Removed `ReplayBytes` message handling; replay now comes through raw stream
- `src/message.rs` - Removed `ReplayBytes` variant and its test

### Decisions Made

1. **Single buffer abstraction:** Instead of separate replay buffer and broadcast channel, use one `MultiplexBuffer` that handles both concerns. Writers append to the buffer and broadcast to subscribers. New subscribers get the current buffer contents plus future writes.

2. **Per-subscriber channels:** Each subscriber gets their own `mpsc::unbounded_channel`. When they subscribe, we copy the current buffer to their channel and register them for future broadcasts. This avoids cursor arithmetic and works naturally with async.

3. **Mutual exclusion via buffer lock:** `write()` holds the buffer write lock during both append and broadcast. `subscribe()` holds the buffer read lock during both snapshot and registration. These are mutually exclusive, ensuring no data is lost or duplicated.

4. **Remove ReplayBytes message:** Since replay bytes now flow through the raw stream automatically (subscriber's first read contains the replay), there's no need for a separate protocol message. This simplifies the client/server handshake.

### Verification

```
cargo fmt                           # OK
cargo clippy --workspace            # OK (existing dead_code warnings only)
cargo test --workspace              # 26 tests pass (20 amux, 6 e2e-runner)
cargo run -p e2e-runner -- run      # 5/5 E2E tests pass
```

### Technical Details

The race condition in the old code:
```
t0: get_replay_buffer() -> gets bytes [A]
t1: PTY outputs B, appends to replay, broadcasts B
t2: subscribe() -> receiver created (too late for B)
Result: client gets [A] then [C, D, ...] - B is lost
```

The fix ensures atomicity:
- `write()` holds buffer lock during append AND broadcast
- `subscribe()` holds buffer lock during snapshot AND registration
- Either B is in the snapshot, OR the subscriber is registered before B is broadcast

---

## 2025-01-10: E2E Testing Framework - Explicit Output & Variables

### Summary
Refactored the e2e testing framework to use explicit output matching and added variable substitution for dynamic values. Tests now include all terminal output (PTY echo + agent response), making them transparent and easy to debug. Added directory entity for unique temp paths per test, enabling parallel execution and path variable assertions.

### Changes

**test-agent/src/main.rs:**
- Changed output from `{message}` to `echo: {message}` to distinguish from PTY echo

**e2e-runner/src/executor.rs:**
- Added `VariableContext` for `$name.path` and `$name.socket_path` substitution
- Added auto-injection of default directory ("cwd") and config ("local") when not specified
- Canonicalize directory paths to resolve symlinks (e.g., `/var` → `/private/var` on macOS)
- Reduced timeout from 2s to 200ms (bytes transfer rapidly)

**e2e-runner/src/terminal.rs:**
- Added `read_expected()` with `\r\n` → `\n` normalization
- Added `stty raw` wrapper to disable outer PTY processing
- Added `shell_quote()` for safe command argument handling

**e2e-runner/src/parser.rs:**
- Added `Directory` struct with name and optional path
- Made terminal `config` and `cwd` fields optional (use defaults)
- Output lines are grouped until next input/terminal-switch

**src/client.rs:**
- Simplified `list-agents` output to `{agent_id} - {working_dir}` (display format, not debug)

**New test files:**
- `e2e-tests/list_agents.test` - Tests list-agents with directory variables
- `e2e-tests/replay_buffer.test` - Tests replay buffer and broadcast

### Test File Format

Tests are explicit about all terminal output:

```
# test: replay_buffer

## Environment

terminal:
  name: T1

terminal:
  name: T2

## Test

@T1
> amux new-agent -t myagent test-agent
> hello
hello
echo: hello

@T2
> amux attach -t myagent
hello
echo: hello
> world
world
echo: world
```

For dynamic paths, use directory entities and variables:

```
## Environment

directory:
  name: mydir

terminal:
  name: T1
  cwd: mydir

## Test

@T1
> amux list-agents
  myagent - $mydir.path
```

### Decisions Made

1. **Explicit output:** Tests show exactly what the terminal shows - PTY echo followed by agent response. No hidden magic or stripping. More verbose but completely transparent.

2. **"echo:" prefix:** test-agent prefixes responses with "echo:" to distinguish from PTY echo. This makes test output unambiguous.

3. **Minimal environment:** Tests only specify what they need. Default config and directory are auto-injected. Only declare entities when you need to name them for variables.

4. **200ms timeout:** Bytes transfer rapidly over Unix sockets. 200ms is generous - could potentially go lower.

### Verification

```
cargo fmt && cargo clippy --workspace  # OK
cargo test --workspace                 # 16 tests pass
cargo run -p e2e-runner -- run         # 5/5 E2E tests pass
```

### Current E2E Tests

| Test | Coverage |
|------|----------|
| `new_agent` | Create agent, send input, verify response |
| `attach` | Second terminal attaches and interacts |
| `multiple_agents` | Two agents on same server |
| `list_agents` | List agents with directory path variables |
| `replay_buffer` | Late joiner sees history, broadcast works |

---

## 2025-01-10: E2E Config-Based Testing Refactor

### Summary
Refactored E2E testing infrastructure from environment-variable-based configuration to config-file-based approach. Tests now generate real YAML config files in a temp directory and pass them to amux via `--config` flag. This eliminates the need for the `test-support` feature flag and enables future parallel test execution with isolated socket paths.

### Changes

**Modified in amux:**
- `Cargo.toml` - Added `serde_yaml` dependency, removed `test-support` feature
- `src/config.rs` - Added `from_file()`, `Serialize`/`Deserialize` traits, removed env var handling
- `src/error.rs` - Added `Config` error variant
- `src/main.rs` - Added global `--config <FILE>` flag
- `src/client.rs` - All functions now accept `&Config` parameter
- `src/server.rs` - Removed unused `SOCKET_PATH` export

**Modified in e2e-runner:**
- `src/parser.rs` - Renamed `Server` → `TestConfig`, `server` → `config`
- `src/executor.rs` - Generates YAML config files, auto-injects `--config` and `test-agent` paths

**Modified test files:**
- `e2e-tests/new_agent.test` - Renamed from `echo.test`, updated to new format with `config:` entity

### Test File Format (New)

```
# test: new_agent
# description: Create a new agent and verify it responds to input

## Environment

config:
  name: local

terminal:
  name: T1
  config: local

## Test

@T1
> amux new-agent -t test1 test-agent
> hello world
hello world
```

The executor transparently transforms `amux new-agent -t test1 test-agent` into:
```
/abs/path/amux --config /tmp/.../local.yaml new-agent -t test1 /abs/path/test-agent
```

### Decisions Made

1. **YAML config format:** Chose YAML over TOML for consistency with the test file format and easier human editing.

2. **Auto-inject paths:** Test files use simple `amux` and `test-agent` names. The executor injects absolute paths and `--config` flag automatically. This keeps test files clean and portable.

3. **Removed test-support feature:** No more conditional compilation. Config is always loaded from file or defaults - no env var overrides needed.

4. **Each test gets unique socket:** Socket paths are auto-generated as `/tmp/amux-test-{testname}-{configname}.sock`. This enables future parallel test execution.

### Verification

```
cargo fmt && cargo clippy --workspace  # OK (only dead_code warnings for future infrastructure)
cargo test --workspace                 # All 13 tests pass
cargo run -p e2e-runner -- run         # 1/1 E2E tests pass
```

### Next Steps

- Add more E2E tests (list-agents, attach/detach, multi-terminal)
- Enable parallel test execution (each test has isolated sockets)
- Add cloud mode testing with TCP transport

---

## 2025-01-10: E2E Testing Infrastructure

### Summary
Created a declarative E2E regression testing framework for amux. The framework uses a simple test file format with Server/Terminal environment entities and Input/ExpectOutput test steps. A test-agent binary echoes input with a NUL byte synchronization signal, allowing the test runner to reliably detect when output is complete.

### Changes

**New crates:**
- `test-agent/` - Minimal echo agent that responds with "received: {input}" + NUL byte
- `e2e-runner/` - Test harness with parser, PTY wrapper, executor, and CLI

**Files in e2e-runner:**
- `src/main.rs` - CLI with `run [filter]` and `update [filter]` commands
- `src/parser.rs` - Parses `.test` files into `TestCase` structs
- `src/executor.rs` - Runs tests, manages PTY terminals, compares output
- `src/terminal.rs` - PTY wrapper using `portable-pty`

**New test files:**
- `e2e-tests/echo.test` - Basic test-agent echo functionality

**Modified files:**
- `Cargo.toml` - Added workspace members and `test-support` feature
- `src/config.rs` - Added env var overrides (AMUX_SOCKET, AMUX_HOST_ID) with `#[cfg(feature = "test-support")]`
- `src/main.rs` - Fixed to use Config for socket path
- `src/client.rs` - Fixed to use Config for socket path

### Test File Format

```
# test: echo
# description: Basic test-agent echo functionality

## Environment

server:
  name: local
  host_id: test-host

terminal:
  name: T1
  server: local

## Test

@T1
> amux new-agent -t test1 /path/to/test-agent
> hello world
received: hello world
```

### Decisions Made

1. **test-support feature flag:** Environment variable overrides (AMUX_SOCKET, AMUX_HOST_ID) are only available when amux is built with `--features test-support`. This prevents production builds from accidentally picking up test configuration.

2. **NUL byte synchronization:** The test-agent sends a NUL byte (0x00) after each response. The test runner reads until NUL to know when output is complete. This eliminates timing-based flakiness.

3. **"received:" prefix:** Test-agent responds with "received: {input}" instead of just echoing. This distinguishes agent output from PTY local echo.

4. **PTY echo stripping:** The executor tracks input sent and strips the corresponding PTY echo from the beginning of output before comparison.

5. **Flat test directory:** All tests live in `e2e-tests/*.test` (flat structure). This keeps things simple while allowing easy filtering by name.

### Verification

```
cargo build --workspace --features test-support  # OK
cargo fmt && cargo clippy --workspace            # OK (only warnings for future infrastructure)
cargo test --workspace                           # 13/13 passed
cargo run -p e2e-runner -- run                   # 1/1 passed
```

### Next Steps

- Add more test scenarios: `list-agents`, `attach/detach`, `two-terminals`, `replay-buffer`
- Investigate the potential race condition user mentioned: replay buffer sent before raw mode transition
- Consider adding `update` mode to auto-update expected output in test files

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
