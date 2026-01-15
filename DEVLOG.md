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
