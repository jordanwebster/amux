# Architecture Refactor Progress

Objective: implement `docs/NEW_ARCHITECTURE.md` and keep this file as the
working ledger for decisions, evidence, and remaining work.

## Format

Each checkpoint uses:

- `Status`: `todo`, `in_progress`, `blocked`, or `done`.
- `Spec`: exact section or invariant references from `docs/NEW_ARCHITECTURE.md`.
- `Artifacts`: files/modules that carry the implementation.
- `Evidence`: commands, inspections, or tests that prove the checkpoint.
- `Notes`: important decisions or follow-up risks.

## Checkpoints

| ID | Status | Spec | Deliverable | Artifacts | Evidence |
| --- | --- | --- | --- | --- | --- |
| P1 | done | 4.2, 4.6.7, 5.1-5.7 | Replace the old custom RPC proto with gRPC services, split first-party io protocol protos, and generate tonic server/client code. | `crates/amux/proto/amux/v1/*.proto`, `crates/amux/build.rs`, `crates/amux/src/protocol/` | `amux.proto` now generates only `RoutingService`, `AgentService`, and `ClientService`; first-party `claude.proto`/`test_agent.proto` are compiled by tonic-build; stale custom-RPC and stale AgentService inventory/resolve/admin/hook service schema scans are clean; full verification passed. |
| S1 | done | 4.5, S-1-S-6 | Introduce service-owned state and event sources; make gRPC impls thin shims over service methods. | `crates/amux/src/services/`, `crates/amux/src/routing/events.rs`, `crates/amux/src/services/startup/mod.rs` | Generated gRPC impls delegate to service methods; startup attachments use deltas-only subscriptions; snapshot subscriptions remain at gRPC/routing peer boundaries; EventSource-backed network streams return `RESOURCE_EXHAUSTED` when the subscriber queue closes; full verification passed. |
| R1 | done | 5.1-5.7, R-1-R-4, I-5-I-12 | Implement `RoutingService.Connect`, handshake state rules, first-route routing core, raw/logical event streams, reauth/goaway. | `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/routing/`, `crates/amux/src/services/startup/mod.rs` | First-route storage, route-matched down, raw/logical event flavours, hop-local origin filtering, Connect handshake rules, connector/acceptor loops, cloud auth interceptor with claims in request extensions, same-user reauth, auth-expiry GoAway, dynamic link assignment, host-id collision rejection, and generated TCP/cloud routing are covered by routing/startup tests; full verification passed. |
| T1 | done | 6.1-6.8, T-1-T-10 | Implement lazy tunnel registry, `TunnelTransport`, tunnel pool, forwarding, teardown on host removal. | `crates/amux/src/tunnel/`, `crates/amux/src/transport/`, `crates/amux/src/services/startup/mod.rs` | Lazy initiator/target tunnel creation, `TunnelTransport` IO/connect-info, full `TunnelId` registry keys, target mismatch rejection, route teardown, EOF/task cleanup, no-wait `NOT_FOUND`, per-link HostUp-before-TunnelFrame ordering, tonic HTTP/2 keepalive, and target-side `serve_with_incoming` are covered by tunnel/startup tests; full verification passed. |
| A1 | done | 4.6, A-1-A-12 | Refactor local agent management into per-host `AgentService`, snapshot-then-deltas, `AgentUpdated`, `SessionClosed`. | `crates/amux/src/services/agent/mod.rs`, `crates/amux/src/services/agent/session_rpc.rs`, `crates/amux/src/services/agent/lifecycle.rs`, `crates/amux/src/agents/` | Per-host AgentService state, snapshot-then-deltas, full `AgentUpdated`, lifecycle event-before-response ordering, per-host name uniqueness, hook-only readonly agents, opaque io-protocol payload handling, terminal `SessionClosed`, no AgentService `HostUnreachable`, retained replay buffers under `agents/buffer.rs`, and session subscriber `RESOURCE_EXHAUSTED` are covered by agent/buffer/client tests; full verification passed. |
| C1 | done | 4.4, C-1-C-11, I-4 | Implement first-class `ClientService` as the only client-facing surface with aggregated host/agent model and dispatch. | `crates/amux/src/services/client.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/server.rs` | Generated local/in-process client endpoints serve only `ClientService`; public clients use generated `ClientServiceClient`; local/remote dispatch, model snapshots, remote inventory, host teardown, remote session unreachable mapping, downstream-cancel ownership, and concurrent remote session subscribers covered by `cargo test -p amux services::client::tests::`; full verification passed. |
| U1 | done | 6.7, 7.1, 10 | Rework daemon startup/listeners to tonic servers and target file layout. | `crates/amux/src/server.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/services/startup/cloud.rs`, `crates/amux/src/services/agent/lifecycle.rs`, `crates/amux/src/routing/host.rs`, `crates/amux/src/user_state.rs`, `crates/amux/src/transport/` | Daemon/embedded startup calls `start_user_services`, attaches ClientService to routing/local-agent streams before serving, starts host-service `serve_with_incoming`, exposes local ClientService over Unix/in-process tonic, serves direct/cloud RoutingService over generated tonic TCP/TLS listeners, opens cloud routing in the background, removes the old `server/` namespace, and stale generated-service dead-code/comment scans are clean; full verification passed. |
| D1 | done | 10.2 | Remove obsolete custom RPC/framing/dispatch/role code. | `crates/amux/src/server/`, `crates/amux/src/transport/`, `crates/amux/src/protocol/`, `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/auth/cloud.rs` | WebSocket transport, runtime TCP accept, legacy outbound cloud dialer, legacy connection-loop token refresher, local framed IPC wrappers, normal-build public legacy `ClientRuntime`, normal-build public legacy protocol frame/handshake re-exports, normal-build public legacy transport error variants, normal-build legacy TCP accept/dial path, normal-build legacy dispatch/connection loop, custom method registry, length-prefixed transport stack, legacy accept-handshake module, legacy `TransportMessage` codec/envelope types, legacy `agent_lifecycle` and session frame wrappers, legacy session-subscription lifecycle/map, legacy frame-reply shutdown/suspend path, custom RPC state harness, legacy custom-protocol integration harness, legacy public-client custom runtime files, legacy `server::accept`, transport connect-handshake, framed `TcpTransport`, custom RPC state module, server connection/dispatch/runtime stream stack, test-only length-prefixed framing/memory transport, legacy session-subscription adapters, stale server route/remote-host state, obsolete proto `TransportMessage`/`Frame`/`ConnectRequest`/`RoutingRole` definitions, test-only protocol wrappers, old `server/routing` local-agent namespace, and stale source comments/allowances removed. Obsolete custom-RPC/source/proto scan is clean. |
| V1 | done | all | Completion audit maps every explicit objective requirement to current artifacts and verified evidence. | `notes/REFACTOR_PROGRESS.md`, command output | Final simplification, correctness, and spec audit passes completed. The forbidden `RoutingService.SubscribeRoutingEvents` RPC path is gone; direct `server connect` remains supported for non-cloud links; remote-flow and suspend e2e regressions are fixed; cloud routing auth uses the interceptor/extension path; old `server/`, `client/rpc.rs`, and fat `protocol/wire/` structure are gone; startup composition lives in `services/startup/`; routing link overflow now closes through the live `Connect` cleanup path; EventSource-backed network streams close with `RESOURCE_EXHAUSTED`. Final verification: `cargo check --workspace --all-targets`, `cargo test --workspace --all-targets`, `cargo run -p e2e-runner -- run`, and `git diff --check` passed. Final subagent audits found no blocking spec deviations. |

## Work Log

### 2026-05-16

- Created this progress ledger.
- Adjusted `.gitignore` from `notes/` to `notes/*` plus
  `!notes/REFACTOR_PROGRESS.md` so this required ledger is visible to git
  while other local notes remain ignored.
- Current audit snapshot before refactor:
  - `notes/REFACTOR_PROGRESS.md` was absent.
  - `crates/amux/proto/amux/v1/amux.proto` still used the old custom RPC
    layer (`Frame`, `FrameBody`, `call_id`, `Ping`, `Pong`, `RoutingRole`) and
    did not define `ClientService`.
  - `crates/amux/proto/amux/v1/claude.proto` and
    `crates/amux/proto/amux/v1/test_agent.proto` were absent.
  - `crates/amux/build.rs` used `prost_build` only, so tonic service stubs were
    not generated.
- P1 slice landed:
  - Added `tonic` / `tonic-build` workspace plumbing.
  - Split Claude-specific payloads into `crates/amux/proto/amux/v1/claude.proto`.
  - Split `TestAgentCreateConfig` into
    `crates/amux/proto/amux/v1/test_agent.proto`.
  - Added new-architecture wire envelopes to `amux.proto`: `Message`, `Hello`,
    `HelloAck`, `RoutingEvent`, `TunnelId`, `TunnelFrame`, `Reauth`, and
    `ReauthAck`.
  - Added `RoutingService.Connect` and a transitional `ClientService` surface.
  - Added `AgentUpdated` and `SessionClosed` proto messages.
  - Updated descriptor tests to account for multiple `amux.v1` proto files.
  - Tonic client generation was later re-enabled with `build_transport(false)`;
    this keeps generated service clients while omitting tonic's inherent
    `RoutingServiceClient::connect(endpoint)` transport constructor that
    collides with the `RoutingService.Connect` RPC method.
  - Transitional note: legacy custom RPC messages and services remain so the
    existing runtime keeps compiling while the implementation migrates.
  - Verification: `cargo check -p amux` passed; `cargo test -p amux` passed
    (331 unit tests, 8 embedded tests, 0 doc tests).
- R1 partial slice landed:
  - Added top-level `crates/amux/src/routing/` as the target home for new
    routing primitives.
  - Added `RoutingCore` with first-route-only `HostUp` storage, route-matched
    `HostDown`, raw `RoutingEvent` stream, logical `HostReachabilityEvent`
    stream, and snapshot subscription methods.
  - Added a bounded `EventSource` that disconnects full/closed subscribers,
    matching the backpressure direction in S-6.
  - Tests cover R-1, R-2, R-3 snapshot registration, and subscriber drop on
    backpressure.
  - Transitional note: the old runtime still uses `server::routing`; the new
    `routing` module is additive until `RoutingService.Connect` is wired over
    it.
  - Verification: `cargo test -p amux routing::` passed; `cargo test -p amux`
    passed (335 unit tests, 8 embedded tests, 0 doc tests).
- R1 route-prefix teardown slice landed:
  - Added `RoutingCore::remove_route_prefix(...)` and
    `remove_link_routes(...)` for link/relay loss cascades.
  - Prefix removal emits raw `HostDown` and logical `HostRemoved` for every
    stored host whose first-route path starts with the failed prefix, preserving
    sorted deterministic event order.
  - Verification: `cargo test -p amux routing::core` passed (4 tests);
    `cargo test -p amux` passed (355 unit tests, 8 embedded tests,
    0 doc tests).
- T1 partial slice landed:
  - Added top-level `crates/amux/src/tunnel/` as the target home for new tunnel
    primitives.
  - Added `TunnelId` with generated-proto conversions and validation.
  - Added `TunnelTransport` implementing `AsyncRead`, `AsyncWrite`, and tonic
    server `Connected` connect info.
  - Added `create_tunnel(...)` helper that links tonic-facing duplex bytes to
    routed `TunnelFrame` messages and supports inbound payload delivery.
  - Tests cover tunnel id conversion, invalid id decoding, transport I/O,
    connect info, outbound frame wrapping, and inbound delivery.
  - Transitional note: tunnel registry, tunnel pool, route teardown, and tonic
    Channel/Server integration are still pending.
  - Verification: `cargo test -p amux tunnel::` passed; `cargo test -p amux`
    passed (339 unit tests, 8 embedded tests, 0 doc tests).
- T1 pool slice landed:
  - Added `crates/amux/src/tunnel/pool.rs`.
  - Added `TunnelPool.channel_to(peer)` with cached tonic `Channel`s, route
    lookup through `RoutingCore`, first-hop link writer lookup, and T-10
    `NotFound` behavior when no `HostEntry` exists.
  - Added target-side lazy tunnel creation from inbound `TunnelFrame`s and
    delivery of first/next payloads into the same `TunnelTransport`.
  - Added `remove_host(host_id)` teardown for cached channels and every tunnel
    whose initiator or target is the removed host.
  - Added direct `tower` / `hyper-util` dependencies for the tonic custom
    connector path (`TokioIo<TunnelTransport>`).
  - Transitional note: the pool is still not wired into daemon `ServerUserState`
    or `RoutingService.Connect`; link registration is explicit for now.
  - Verification: `cargo test -p amux tunnel::` passed; `cargo test -p amux`
    passed (344 unit tests, 8 embedded tests, 0 doc tests).
- T1 forwarding slice landed:
  - Extended `TunnelPool::handle_inbound_frame` to handle intermediate-hop
    forwarding: pop the next destination link, rewrite the remaining `dst`,
    and enqueue the opaque frame to that link's writer.
  - Missing forwarding links are dropped silently, matching §5.5.
  - Endpoint frames still validate `TunnelId.target == my_host_id` and create
    target-side tunnels lazily.
  - Verification: `cargo test -p amux tunnel::` passed (11 tests);
    `cargo test -p amux` passed (354 unit tests, 8 embedded tests,
    0 doc tests).
- A1 AgentUpdated slice landed:
  - Added `AgentEvent::AgentUpdated` to the Rust protocol event enum.
  - Added wire encode/decode support for the `AgentUpdated` proto variant.
  - Changed local metadata updates (`rename`, automatic name improvements) to
    broadcast `AgentUpdated` instead of reusing `AgentUp`.
  - Updated peer handling to accept `AgentUpdated` as a full replacement event
    for the subscribed host.
  - Updated `amux-ui` inventory subscription handling to treat `AgentUpdated`
    as an upsert, preserving its existing notification behavior.
  - Added protocol coverage:
    `agent_subscription_streams_agent_updated_on_rename`.
  - Transitional note: `AgentService` still uses the legacy
    `ServerUserState`/custom-RPC machinery and does not yet emit
    `SessionClosed`.
  - Verification:
    `cargo test -p amux agent_subscription_streams_agent_updated_on_rename`
    passed; `cargo test -p amux` passed (345 unit tests, 8 embedded tests,
    0 doc tests); `cargo check -p amux-ui` passed; `cargo check -p amux-cli`
    passed.
- A1 SessionClosed slice landed:
  - Added Rust wire/domain decoding for `SubscribeSessionResponse.closed` and
    all current `SessionClosed` reasons.
  - Added a server stream helper that sends a final stream item and terminal
    response under one send gate.
  - Changed local agent deletion to end matching `SubscribeSession` streams
    with `SessionClosed { agent_deleted }` followed by an OK terminal response.
  - Left route loss and explicit client cancellation on their existing
    cancellation/unreachable paths until `ClientService` routing is wired.
  - Added protocol coverage:
    `deleting_agent_closes_subscribe_session_with_agent_deleted`.
  - Verification:
    `cargo test -p amux deleting_agent_closes_subscribe_session_with_agent_deleted`
    passed; `cargo test -p amux` passed (347 unit tests, 8 embedded tests,
    0 doc tests); `cargo check -p amux-cli` passed; `cargo check -p amux-ui`
    passed.
- A1 SessionClosed/agent-exit slice landed:
  - Changed `SubscribeSession` output-source completion to emit
    `SessionClosed { agent_exited }` followed by an OK terminal response.
  - Changed output-source encode/internal failures to emit
    `SessionClosed { internal_error }` followed by OK, matching A-10's
    in-band terminal cause model.
  - Preserved existing explicit cancellation/stale-subscription behavior.
  - Added protocol coverage:
    `withdrawn_agent_closes_subscribe_session_with_agent_exited`.
  - Verification:
    `cargo test -p amux withdrawn_agent_closes_subscribe_session_with_agent_exited`
    passed; `cargo test -p amux` passed (352 unit tests, 8 embedded tests,
    0 doc tests).
- C1 aggregation-model slice landed:
  - Added `crates/amux/src/services/client.rs` as the future
    `ClientService` state core.
  - Added host aggregation that filters relay hosts out of client-facing
    host snapshots/events while keeping that filtering outside
    `RoutingCore`.
  - Added agent aggregation for local/remote `AgentService` events,
    including bulk `AgentDown` emission when a reachable host is removed.
  - Added `AgentRef` resolution against the aggregated model with typed
    `AmbiguousAgentName` errors.
  - Added typed wire encode/decode for `ProtocolError::AmbiguousAgentName`
    using the existing `amux.v1.AmbiguousAgentName` error detail.
  - Transitional note: this model is not yet wired to daemon startup,
    tunnel-backed remote subscriptions, or generated `ClientService` tonic
    handlers.
  - Verification:
    `cargo test -p amux services::client` passed;
    `cargo test -p amux ambiguous_agent_name_uses_typed_detail` passed;
    `cargo test -p amux` passed (351 unit tests, 8 embedded tests,
    0 doc tests); `cargo check -p amux-cli` passed; `cargo check -p amux-ui`
    passed.
- C1/S1 tonic-shim slice landed:
  - Added a generated-protobuf alias module at `protocol::wire::pb` so code can
    refer to generated request types even when transitional domain adapters use
    the same Rust names.
  - Added client-model to generated-protobuf response conversion helpers for
    `SubscribeHosts` and `SubscribeAgents`, including snapshot-complete
    framing.
  - Implemented the generated `client_service_server::ClientService` trait for
    the additive `services::client::ClientService` model.
  - The tonic shim now serves model-backed `ListHosts`, transitional
    `ListAgents`, `SubscribeHosts`, and `SubscribeAgents`; lifecycle, session,
    admin, and hook methods return explicit `UNIMPLEMENTED` until their
    dependencies are wired.
  - Verification: `cargo test -p amux services::client` passed (7 tests);
    `cargo test -p amux` passed (359 unit tests, 8 embedded tests,
    0 doc tests); latest post-handshake-slice verification also passed
    `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `git diff --check`.
- R1 connect-handshake state slice landed:
  - Added `crates/amux/src/routing/link.rs` with a pure
    `RoutingService.Connect` handshake state machine for connector/acceptor
    pre-handshake and established-stream message legality.
  - Protocol violations close the state machine: pre-handshake failures map to
    `HelloAck { error }`, while post-handshake handshake messages map to
    `GoAway(PROTOCOL_ERROR, drain_timeout_ms: 0)`.
  - Covered accepted/rejected/malformed `HelloAck`, acceptor first-message
    enforcement, connector silence-until-ack enforcement, missing message
    bodies, and legal post-handshake variants (`RoutingEvent`, `TunnelFrame`,
    `Reauth`, `ReauthAck`, `GoAway`).
  - Verification: `cargo test -p amux routing::link` passed (11 tests);
    `cargo test -p amux` passed (370 unit tests, 8 embedded tests,
    0 doc tests); `cargo check -p amux-cli` passed; `cargo check -p amux-ui`
    passed; `git diff --check` passed.
- R1 routing-event wire-codec slice landed:
  - Added `crates/amux/src/routing/wire.rs` for new `RoutingService.Connect`
    `RoutingEvent` conversion.
  - Outbound conversion strips hop-local `origin_link`, filters events learned
    from the same link, filters a peer's own `HostUp`, and appends
    `SnapshotComplete` for snapshots.
  - Inbound conversion validates generated protobuf events and prepends the
    incoming link to wire routes before storage, satisfying I-9's hop-relative
    route rule.
  - Verification: `cargo test -p amux routing::wire` passed (7 tests);
    latest full verification passed `cargo test -p amux` (379 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- T1 host-removal teardown hook landed:
  - Added `TunnelPool::handle_host_event(...)` as the logical
    `HostRemoved` entry point for consumers of `RoutingCore.subscribe_hosts()`.
  - `HostRemoved(host_id)` drops cached tonic channels and tunnels whose
    `TunnelId` names that host; `HostAdded` is ignored.
  - Verification: `cargo test -p amux tunnel::pool` passed (8 tests);
    latest full verification passed `cargo test -p amux` (379 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- P1 tonic-client generation slice landed:
  - Switched `crates/amux/build.rs` from `build_client(false)` to
    `build_transport(false)`.
  - Generated clients now exist for `RoutingService`, `AgentService`, and
    `ClientService`; the transport convenience constructors remain omitted to
    avoid the `RoutingServiceClient::connect(...)` name collision.
  - Verification: `cargo check -p amux` passed; generated output contains
    `routing_service_client`, `agent_service_client`, and
    `client_service_client`; `cargo test -p amux protocol::wire::generated`
    passed (2 tests); latest full verification passed `cargo test -p amux`
    (379 unit tests, 8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- A1/S1 AgentService tonic-shim slice landed:
  - Implemented the generated `agent_service_server::AgentService` trait for
    `AgentServiceCtx` for unary methods: `ListAgents`, `ResolveAgent`,
    `CreateAgent`, `RenameAgent`, `DeleteAgent`, and `SendInput`.
  - The shim reuses existing protobuf/domain converters and service methods;
    `SubscribeAgentEvents` and `SubscribeSession` return explicit
    `UNIMPLEMENTED` until target-side tonic streaming is wired.
  - Verification: `cargo test -p amux services::agent` passed (4 tests);
    latest full verification passed `cargo test -p amux` (388 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- S1 HookService/AdminService tonic-shim slice landed:
  - Added a shared `services::status::protocol_status` helper for consistent
    `ProtocolError` to `tonic::Status` mapping across generated shims.
  - Implemented the generated `hook_service_server::HookService` trait for
    `HookServiceCtx`, backed by existing hook handling and request validation.
  - Implemented generated `admin_service_server::AdminService::Debug` for
    `AdminServiceCtx`; state-changing admin calls return explicit
    `UNIMPLEMENTED` until daemon tonic lifecycle plumbing lands.
  - Verification: `cargo test -p amux services::` passed (15 tests);
    latest full verification passed `cargo test -p amux` (388 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- T1/S1 target-side AgentService tonic server slice landed:
  - Re-exported `TunnelTransport` from `tunnel` and added an additive
    `spawn_agent_tonic_server(...)` helper that serves generated
    `AgentServiceServer<AgentServiceCtx>` over an incoming
    `mpsc::Receiver<TunnelTransport>`.
  - Added a loopback test that connects a generated `AgentServiceClient` over
    an in-memory `TunnelTransport` and successfully calls `ListAgents`.
  - Transitional note: this is not yet wired into daemon startup or
    `ServerUserState`; it proves the §6.7 target-side tonic server path for
    the host-facing agent service.
  - Verification: `cargo test -p amux services::` passed (16 tests);
    latest full verification passed `cargo test -p amux` (388 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1/S1 RoutingService.Connect acceptor-loop slice landed:
  - Added an additive generated `routing_service_server::RoutingService`
    implementation for `RoutingConnectCtx`.
  - The acceptor stream now enforces the `Hello`/`HelloAck` handshake,
    registers the assigned link as a tunnel writer, stores the direct peer as a
    first-route `HostUp`, sends filtered snapshot-then-live routing events, and
    removes link-prefixed routes/tunnels when the stream closes.
  - Post-handshake inbound `RoutingEvent`s are imported through the new
    hop-relative wire codec, `TunnelFrame`s are dispatched through
    `TunnelPool`, `Reauth` receives an in-band accepted `ReauthAck`, and
    protocol errors send handshake-appropriate `HelloAck`/`GoAway` messages.
  - Transitional note: this slice covers the acceptor side only. Connector-side
    dialing, authentication/authorization, reauth timers, dynamic link-name
    assignment, daemon listener wiring, and legacy custom-RPC removal remain
    pending.
  - Verification: `cargo test -p amux services::routing` passed (6 tests);
    latest full verification passed `cargo test -p amux` (394 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1 RoutingService.Connect connector-loop slice landed:
  - Added additive connector-side stream machinery with `RoutingConnectorCtx`.
    It sends the initial `Hello`, validates `HelloAck.accepted`, adopts the
    acceptor-assigned link name, and stores the acceptor as a direct peer route.
  - Refactored acceptor and connector streams to share one established-link
    loop for link writer registration, routing snapshot/live forwarding,
    inbound routing-event import, tunnel-frame dispatch, reauth acking, and
    route/tunnel cleanup.
  - Added connector coverage for initial `Hello`, accepted `HelloAck`, inbound
    hop-relative routes with the assigned link, bad first acceptor message
    `GoAway(PROTOCOL_ERROR)`, tunnel-frame dispatch, and link-route cleanup.
  - Transitional note: generated-client dialing over real TCP/TLS/tonic
    channels, auth metadata/interceptors, reauth timers, dynamic acceptor
    link-name assignment, daemon startup wiring, and legacy custom-RPC removal
    remain pending.
  - Verification: `cargo test -p amux services::routing` passed (11 tests);
    latest full verification passed `cargo test -p amux` (399 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- A1/S1 AgentService SubscribeAgentEvents tonic-stream slice landed:
  - Added a local `EventSource<AgentEvent>` to `ServerUserState` and feeds it
    from the existing topology broadcast path for `AgentUp`, `AgentUpdated`,
    and `AgentDown`.
  - Implemented generated `AgentService.SubscribeAgentEvents` on
    `AgentServiceCtx` as snapshot-then-live streaming over the local agent
    event source, while keeping the legacy custom-RPC subscription path
    intact during migration.
  - Re-exported the protobuf `agent_event_to_wire` conversion for generated
    streaming shims, and kept `SubscribeSession` explicitly `UNIMPLEMENTED`
    until session streaming is moved off the custom RPC runtime.
  - Verification: `cargo test -p amux services::agent` passed (6 tests);
    latest full verification passed `cargo test -p amux` (400 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- C1/S1 ClientService local lifecycle dispatch slice landed:
  - `ClientService` can now be constructed with a local `AgentServiceCtx`,
    giving the generated client-facing service a direct path to the local
    per-host agent service during the migration.
  - Implemented generated `CreateAgent`, `RenameAgent`, and `DeleteAgent` for
    local agents, including protobuf lifecycle request decoding, local-host
    validation, `AgentService` dispatch, and model updates via the returned
    `AgentUp`, `AgentUpdated`, and `AgentDown` events.
  - Remote lifecycle dispatch is still explicitly `UNIMPLEMENTED`, and
    `SubscribeSession`, `SendInput`, admin, and hook calls remain on the
    pending side of the generated `ClientService` migration.
  - Verification: `cargo test -p amux services::client` passed (9 tests);
    latest full verification passed `cargo test -p amux` (402 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- C1/S1 ClientService local `SendInput` dispatch slice landed:
  - Implemented generated `ClientService.SendInput` for local agents by
    decoding the protobuf request through the existing AgentService wire
    decoder and forwarding the typed request to `AgentService::send_input`.
  - Known remote agents now receive the same explicit remote-dispatch
    `UNIMPLEMENTED` status as lifecycle methods; clients without a local
    `AgentServiceCtx` still report `ClientService.SendInput` as not wired.
  - Transitional note: generated `SubscribeSession` and remote `SendInput`
    routing over tunnel-backed AgentService clients remain pending.
  - Verification: `cargo test -p amux services::client` passed (9 tests);
    latest full verification passed `cargo test -p amux` (402 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- C1/S1 ClientService local admin/hook dispatch slice landed:
  - Added optional local `AdminServiceCtx` and `HookServiceCtx` dependencies
    to `ClientService`, alongside the existing local `AgentServiceCtx`.
  - Implemented generated `ClientService.Debug` by delegating to
    `AdminService::debug`, preserving the existing debug format mapping.
  - Implemented generated `ClientService.HandleHook` by decoding the client
    request and delegating to `HookService::handle`, preserving local
    `ProtocolError` to tonic status mapping.
  - Transitional note: mutating admin RPCs (`Shutdown`, `Suspend`, `Resume`,
    `ConnectToServer`) still return explicit `UNIMPLEMENTED` until daemon
    lifecycle plumbing is moved behind the generated ClientService surface.
  - Verification: `cargo test -p amux services::client` passed (10 tests);
    latest full verification passed `cargo test -p amux` (403 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- A1/C1 generated local `SubscribeSession` slice landed:
  - Implemented generated `AgentService.SubscribeSession` for local sessions
    using the existing session preparation/read path and direct tonic
    response streaming. The stream emits `Opened`, replay/live output events,
    and an in-band `Closed` event when the output source ends or fails.
  - Added local generated-session close events to `ServerUserState`; local
    agent deletion now emits `SessionClosed { agent_deleted }` to generated
    AgentService/ClientService session streams before the PTY output source is
    closed.
  - Re-exported the protobuf `session_output_event_to_wire` conversion so
    generated streaming shims can write typed `SubscribeSessionResponse`
    values without round-tripping through custom-RPC payload bytes.
  - Implemented generated `ClientService.SubscribeSession` for local agents
    by decoding the client request, rejecting known remote agents with the
    existing explicit remote-dispatch `UNIMPLEMENTED` status, and delegating to
    the local `AgentService` stream.
  - Transitional note: remote `SubscribeSession` over tunnel-backed
    `AgentServiceClient` is still pending. Generated local streams report
    output-source closure as `agent_exited` and delete-triggered closure as
    `agent_deleted`.
  - Verification: `cargo test -p amux services::` passed (32 tests);
    latest full verification passed `cargo test -p amux` (404 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1/S1 RoutingService `SubscribeRoutingEvents` tonic-stream slice landed:
  - Implemented generated `RoutingService.SubscribeRoutingEvents` over
    `RoutingCore::subscribe_routing_events_with_snapshot`.
  - The stream now emits current `HostUp` snapshot events, a
    `SnapshotComplete` marker, and live routing events using generated
    `SubscribeRoutingEventsResponse` messages.
  - Added generated-stream coverage for snapshot ordering and live `HostUp`
    forwarding through the service shim.
  - Verification: `cargo test -p amux services::routing` passed (12 tests);
    latest full verification passed `cargo test -p amux` (405 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- T1 tunnel initiator response-path slice landed:
  - Updated `TunnelPool::handle_inbound_frame` to deliver endpoint frames to
    an existing tunnel before applying target-side lazy-creation validation.
  - This enables response bytes for initiator-side cached channels
    (`TunnelId { initiator: local, target: remote }`) while preserving the
    protocol violation behavior for unknown endpoint frames whose target is
    not the local host.
  - Added coverage for inbound response frames reaching an existing
    initiator-side `TunnelTransport`.
  - Verification: `cargo fmt && cargo test -p amux tunnel::pool` passed
    (9 tests); latest full verification passed `cargo test -p amux`
    (406 unit tests, 8 embedded tests, 0 doc tests),
    `cargo check -p amux-cli`, and `cargo check -p amux-ui`.
- C1/T1 remote AgentService dispatch slice landed:
  - Added optional tunnel-backed remote `AgentServiceClient` plumbing to
    generated `ClientService`.
  - Implemented remote dispatch for generated `CreateAgent`, `RenameAgent`,
    `DeleteAgent`, `SubscribeSession`, and `SendInput` when the target host is
    known to be non-local.
  - Remote lifecycle responses now update the aggregated client agent model
    and emit the same `AgentUp`, `AgentUpdated`, and `AgentDown` events as the
    local path.
  - Added an end-to-end generated-gRPC test over bridged `TunnelPool`s and a
    target-side `AgentService` tonic server; the test covers remote create,
    rename, subscribe, input echo, delete, model events, and session close.
  - Transitional note: remote admin/hook dispatch remains intentionally absent;
    local admin/hook delegation is still the only generated ClientService path
    for those methods.
  - Verification: `cargo test -p amux services::client` passed (12 tests);
    latest full verification passed `cargo test -p amux` (407 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- C1 remote inventory subscription slice landed:
  - Made `ClientService` state shareable with background subscription tasks.
  - `HostAdded` for a non-relay, non-local host now starts a tunnel-backed
    `AgentService.SubscribeAgentEvents` task; `HostRemoved` aborts that task
    and removes the host's agents from the aggregated model.
  - Remote `AgentUp`, `AgentUpdated`, and `AgentDown` stream events now feed
    the same ClientService agent model/event source as local updates.
  - If the remote agent event stream fails before the host is removed,
    ClientService marks that host's agents down and retries the subscription.
  - Added generated-gRPC coverage where the remote `AgentService` is mutated
    directly and ClientService learns create/rename/delete solely through the
    remote inventory subscription.
  - Verification: `cargo test -p amux services::client` passed (13 tests);
    latest full verification passed `cargo test -p amux` (408 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- C1 remote `SubscribeSession` host-unreachable slice landed:
  - Wrapped remote generated `SubscribeSession` streams in ClientService so an
    upstream `UNAVAILABLE` becomes an in-band
    `SessionClosed { host_unreachable }` response followed by a clean stream
    end.
  - Non-route-failure upstream statuses are still forwarded as gRPC errors.
  - Added focused stream-mapping coverage for pass-through responses,
    `UNAVAILABLE` conversion, stream termination after the synthetic close,
    and non-`UNAVAILABLE` error forwarding.
  - Verification: `cargo test -p amux services::client` passed (14 tests);
    latest full verification passed `cargo test -p amux` (409 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- A1/C1 generated admin lifecycle slice landed:
  - Added typed `ShutdownRequest` variants with `oneshot` replies for
    generated gRPC callers, leaving the legacy frame-reply variants in place
    for the current custom RPC runtime.
  - Implemented generated `AdminService.Shutdown` and `AdminService.Suspend`
    through those typed shutdown-channel requests; the server runtime and
    embedded shutdown loop now handle both typed and legacy request shapes.
  - Implemented generated `AdminService.Resume` and `ConnectToServer` through
    local user-context helpers. Context-free service instances return explicit
    `UNIMPLEMENTED` for those two methods until daemon startup provides the
    local user state and event sender.
  - Generated `ClientService` now delegates local `Shutdown`, `Suspend`,
    `Resume`, and `ConnectToServer` to the local `AdminService` instead of
    returning hard-coded stubs.
  - Added service coverage for typed shutdown/suspend requests, resume/connect
    context validation, resume count responses, and ClientService lifecycle
    delegation.
  - Transitional note: daemon startup still needs to construct generated
    AdminService/ClientService contexts with user state and serve them through
    tonic. The legacy custom RPC admin path remains in place until U1/D1.
  - Verification: `cargo test -p amux services::` passed (41 tests); latest
    full verification passed `cargo test -p amux` (414 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- C1/S1 ClientService startup event-attachment slice landed:
  - Added ClientService helpers to attach to `RoutingCore` logical host events
    and local `AgentService` inventory events using snapshot-then-deltas.
  - Routing attachment applies the current host snapshot, then continuously
    applies live `HostAdded`/`HostRemoved` events to the client host model.
  - Local agent attachment applies the current local-agent snapshot, then
    continuously applies live `AgentUp`/`AgentUpdated`/`AgentDown` events to
    the client agent model.
  - Added coverage that proves pre-existing snapshot state and live updates
    both populate and remove ClientService model entries without manual test
    calls to `apply_host_event` or `apply_agent_event`.
  - Transitional note: daemon startup still needs to call these helpers while
    constructing the generated service graph before binding tonic listeners.
  - Verification: `cargo test -p amux services::client` passed (17 tests);
    latest full verification passed `cargo test -p amux` (416 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- P1/C1 AgentRef client-boundary slice landed:
  - Changed generated `ClientService` request types for `RenameAgent`,
    `DeleteAgent`, `SubscribeSession`, and `SendInput` to client-specific
    messages carrying `AgentRef`.
  - Kept host-facing `AgentService` request types agent-id keyed, matching the
    architecture split where ClientService resolves names and AgentService
    receives concrete `agent_id`s.
  - ClientService now resolves generated `AgentRef` values at the boundary,
    maps zero matches to `NOT_FOUND`, preserves ambiguity handling through the
    existing aggregated model resolver, and dispatches the resolved id locally
    or remotely.
  - Added generated ClientService coverage for name-based local rename/delete
    while preserving id-based remote dispatch and session/input behavior.
  - Verification: `cargo test -p amux services::client` passed (17 tests);
    latest full verification passed `cargo test -p amux` (416 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`, and
    `cargo check -p amux-ui`.
- P1/A1/C1 generated inventory-surface cleanup slice landed:
  - Removed generated `AgentService.ListAgents` and
    `AgentService.ResolveAgent` from the proto and tonic service impl. Agent
    inventory for host-facing generated gRPC now flows through
    `AgentService.SubscribeAgentEvents`.
  - Changed generated `ClientService.ListAgents` to return plain aggregated
    `Agent` records through `ClientListAgentsResponse`, matching the
    client-facing model boundary and avoiding route-bearing `AgentEntry`
    leakage.
  - Updated the target-side tonic loopback test to call
    `AgentService.SubscribeAgentEvents` over `TunnelTransport` instead of the
    removed generated list RPC.
  - Kept the legacy custom-dispatch `ListAgents` and `ResolveAgent` method
    names/messages in place for the old runtime until D1. The method registry
    descriptor test now treats those two as explicitly legacy-custom-only while
    continuing to require parity for generated legacy-dispatched proto RPCs.
  - Verification: `cargo test -p amux services::` passed (42 tests); latest
    full verification passed `cargo test -p amux` (415 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- P1/A1/C1 client-create boundary cleanup slice landed:
  - Split generated `ClientService.CreateAgent` onto
    `ClientCreateAgentRequest`, keeping `host_id` as a client-boundary target
    selector instead of a host-facing `AgentService.CreateAgent` field.
  - `ClientService` now converts `ClientCreateAgentRequest` into the
    host-local `CreateAgentRequest` before dispatching locally or remotely.
  - Generated `AgentService.SubscribeAgentEvents` no longer validates or
    depends on the legacy `host_id` field. The field remains in the proto for
    the custom dispatcher until D1 because the old runtime still uses it to
    track server-origin agent subscriptions.
  - Updated tonic service and remote-dispatch coverage to use the split create
    request and empty generated agent-event subscriptions.
  - Verification: `cargo test -p amux services::` passed (42 tests); latest
    full verification passed `cargo test -p amux` (415 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1/S1 generated service-graph slice landed:
  - Added `services::graph::GeneratedServiceGraph` as the startup bundle for
    generated services. It constructs local `AgentService`, `AdminService`,
    `HookService`, `ClientService`, `RoutingCore`, and `TunnelPool` contexts in
    dependency order.
  - The graph seeds the ClientService host model with the local non-relay host,
    attaches ClientService to RoutingCore host events, attaches ClientService
    to local AgentService inventory events before listeners are exposed, and
    attaches TunnelPool cleanup to host-removal events.
  - The graph starts the target-side host-service tonic server over incoming
    `TunnelTransport`s, currently registering generated `AgentService`.
  - Added coverage proving startup attachments populate ClientService from
    local AgentService and RoutingCore events, plus loopback coverage proving
    the graph-served AgentService accepts a generated client over an incoming
    tunnel.
  - Transitional note: the graph is not yet called from daemon/embedded
    runtime listener setup; U1 still needs to swap the local client listener
    and cloud connection path onto the generated tonic services.
  - Verification: `cargo test -p amux services::` passed (44 tests); latest
    full verification passed `cargo test -p amux` (417 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1 generated daemon-startup integration slice landed:
  - `Server::run` now starts the session-event handler, constructs
    `GeneratedServiceGraph`, and holds it for the daemon lifetime before
    binding the existing local/TCP legacy listeners.
  - This gives generated `ClientService`/`AgentService` startup subscriptions
    and incoming-tunnel serving the documented ordering without yet replacing
    the legacy local client transport used by the current CLI/UI.
  - Transitional note: `EmbeddedBuilder::open` still uses the legacy in-memory
    custom RPC path, and cloud dialing still uses the legacy framed transport.
    The next U1 slices need generated local/embedded ClientService channels
    and generated `RoutingService.Connect` cloud dialing/listening.
  - Verification: `cargo test -p amux services::graph::tests` passed (2
    tests), `cargo test -p amux server::runtime::tests` passed (1 test);
    latest full verification passed `cargo test -p amux` (417 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1 generated in-process ClientService channel slice landed:
  - Added an in-process HTTP/2 transport wrapper and
    `GeneratedServiceGraph::open_in_process_client_channel()` for embedded
    generated `ClientService` clients.
  - The helper serves a cloned generated ClientService over a single in-process
    tonic connection and returns a `Channel` suitable for generated
    `ClientServiceClient`.
  - Added coverage that creates an agent and lists it through the generated
    ClientService client over that in-process channel.
  - Transitional note: this is the generated embedded transport primitive; the
    public `EmbeddedBuilder::open` API still returns the legacy custom-RPC
    `Client` until the CLI/UI client migration lands.
  - Verification: `cargo test -p amux services::graph::tests` passed (3
    tests); latest full verification passed `cargo test -p amux` (418 unit
    tests, 8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1 generated public embedded client slice landed:
  - The public `Client` can now be backed by either the legacy custom-RPC
    runtime or a generated `ClientServiceClient`, allowing callers to keep the
    existing API shape while embedded mode moves onto the generated service
    surface.
  - `EmbeddedBuilder::open` now constructs and retains
    `GeneratedServiceGraph`, opens an in-process generated ClientService
    channel, and returns a generated-backed public `Client`.
  - Generated-backed public methods now cover create, rename, delete,
    send-input, list, resolve, shutdown, suspend, resume, connect, debug dump,
    and hook handling.
  - Transitional note: at this point the public generated client still needed
    stream wrappers for session, routing, and agent event subscriptions.
    Daemon/local socket clients and cloud dialing still used the legacy
    custom-RPC transport.
  - Verification: `cargo test -p amux --test embedded` passed (8 tests);
    latest full verification passed `cargo test -p amux` (419 unit tests,
    8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1/C1 generated public stream-wrapper slice landed:
  - The public generated-backed `Client` now supports
    `subscribe_session`, `subscribe_routing_events`, and
    `subscribe_agent_events` without falling back to legacy custom-RPC frames.
  - Generated session streams map `ClientService.SubscribeSession` responses
    into the existing public `SubscribeSessionFrame` shape, including a final
    `Response(Ok(()))` when the tonic stream ends.
  - Generated routing streams map `ClientService.SubscribeHosts` into the
    existing public `RoutingEvent` shape with empty routes at the client
    boundary.
  - Generated agent streams map and filter `ClientService.SubscribeAgents` by
    requested host id, preserving the existing host-scoped public API while
    tracking seen agent ids so host removals/deletes surface as `AgentDown`.
  - Suppressed exact duplicate agent upserts in ClientService so the generated
    graph does not rebroadcast the same local inventory event once from the
    unary lifecycle result and again from the local AgentService subscription.
  - Added public-client coverage through the generated in-process
    ClientService channel for routing snapshots, agent snapshot/live
    up/update/down, session open/output, and delete-triggered session close.
  - Transitional note: daemon/local socket clients and cloud dialing still use
    the legacy custom-RPC transport; generated local listener and
    `RoutingService.Connect` cloud wiring remain pending.
  - Verification:
    `cargo test -p amux services::graph::tests::generated_graph_public_client_wrapper_uses_in_process_channel`
    passed; `cargo test -p amux services::` passed (47 tests); latest full
    verification passed `cargo test -p amux` (420 unit
    tests, 8 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1 generated Unix local ClientService listener slice landed:
  - Unix daemon startup now binds the configured local socket as a raw tonic
    HTTP/2 listener for generated `ClientService` instead of accepting the
    legacy framed local custom-RPC transport on that socket.
  - `DaemonBuilder::open` now connects to the daemon through a UnixStream
    tonic `Channel` and returns a generated-backed public `Client` without an
    embedded ownership guard.
  - The generated service graph owns the Unix ClientService listener task, so
    it shuts down with the graph and the existing server shutdown path still
    removes the socket path.
  - Windows named-pipe local clients remain on the legacy framed transport
    until tonic named-pipe support is wired or D1 removes that path.
  - Marked now-Unix-dead legacy local-client helpers as transitional
    dead-code allowances so `cargo check -p amux` stays warning-clean while
    Windows and D1 still reference the old local framing code.
  - Added embedded/daemon coverage that starts `Server::run`, opens through
    `DaemonBuilder::open`, lists agents over generated ClientService, and
    shuts the daemon down through the generated admin path.
  - Transitional note: cloud TCP/TLS connections still use the legacy framed
    transport; generated `RoutingService.Connect` cloud dialing and listener
    wiring remain pending.
  - Verification:
    `cargo test -p amux --test embedded daemon_open_uses_generated_local_client_service`
    passed; latest full verification passed `cargo test -p amux` (420 unit
    tests, 9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1/U1 generated RoutingService network primitive slice landed:
  - Added link-name reservation to `RoutingCore`, including exact reservation,
    dynamic suffixing when a proposed link is already in use, and explicit
    release on link cleanup.
  - Changed generated `RoutingService.Connect` acceptor setup to assign link
    names from the peer's proposed `Hello.proposed_link_name` instead of
    relying only on a fixed context link, while preserving fixed-link test
    contexts for existing coverage.
  - Connector-side generated `HelloAck` handling now reserves the accepted
    assigned link locally and rejects collisions before storing the direct
    peer route.
  - Added `spawn_connector_to_channel(...)`, a generated `RoutingService`
    client-side primitive that drives the existing connector state machine
    over a tonic `Channel`.
  - Added tonic-in-process coverage proving two RoutingService instances
    establish a real generated gRPC `Connect` stream and both store direct
    peer routes through the generated client/server path.
  - Transitional note: this is the reusable generated network primitive.
    Daemon TCP/TLS cloud listener and cloud reconnection code still need to
    switch from the legacy framed transport to tonic channels with auth
    metadata/interceptors.
  - Verification: `cargo test -p amux routing::` passed (44 tests); latest
    full verification passed `cargo test -p amux` (423 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1 generated non-cloud TCP RoutingService listener slice landed:
  - `GeneratedServiceGraph` now owns the local host identity needed by
    generated network listeners and exposes
    `serve_routing_service_on_tcp_listener(...)`.
  - The generated TCP listener wraps accepted `TcpStream`s in a tonic server
    transport and serves generated `RoutingService.Connect` through the shared
    routing/tunnel/client graph.
  - Added `routing_connector_ctx(...)` and graph coverage proving a connector
    can dial a real TCP listener over a tonic `Channel` and establish direct
    peer routes through generated `RoutingService.Connect`.
  - Changed `Server::run` so non-cloud `tcp_port` listeners are handed to the
    generated service graph and are no longer also passed to the legacy TCP
    accept loop.
  - Transitional note: cloud relay TCP/TLS and cloud reconnection still use
    the legacy framed transport until generated tonic channel/interceptor
    wiring lands for those paths.
  - Verification:
    `cargo fmt && cargo test -p amux services::graph::tests::generated_graph_serves_routing_service_on_tcp_listener`
    passed; latest full verification passed `cargo test -p amux` (424 unit
    tests, 9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- D1/U1 WebSocket removal slice landed:
  - Removed `tokio-tungstenite` from the workspace and `amux` crate
    dependencies.
  - Deleted `crates/amux/src/transport/websocket.rs` and removed
    `WebSocketTransport` from the transport module.
  - Removed the legacy WebSocket upgrade/accept path from `server::accept`.
  - Removed `websocket_port` from `Config`; cloud relay validation now
    requires only `tcp_port`, matching the new TCP/TLS-only routing-service
    startup path.
  - Removed the WebSocket listener branch from `Server::run`; the remaining
    network listener branch is TCP/TLS, with non-cloud TCP already handed to
    generated `RoutingService`.
  - Updated `e2e-runner` so generated test configs no longer allocate or write
    `websocket_port`.
  - Transitional note: legacy custom framing still exists for cloud relay
    TCP/TLS and outbound cloud reconnection until generated authenticated
    `RoutingService.Connect` replaces those paths.
  - Verification: `cargo test -p amux config::` passed (22 config tests);
    `cargo check -p amux` passed; `cargo check -p e2e-runner` passed; latest
    full verification passed `cargo test -p amux` (423 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1/U1 generated RoutingService auth-metadata connector slice landed:
  - Added `spawn_connector_to_channel_with_bearer_token(...)`, which opens a
    generated `RoutingService.Connect` stream over an existing tonic `Channel`
    and attaches `authorization: Bearer ...` metadata to the request.
  - Kept the existing unauthenticated `spawn_connector_to_channel(...)`
    primitive as the default for in-process/non-cloud callers.
  - Added tonic coverage with a test RoutingService wrapper that rejects
    missing/mismatched authorization metadata before delegating to the normal
    generated Connect handler.
  - Transitional note: this prepares generated outbound cloud dialing, but
    runtime cloud reconnection still needs to build the TLS tonic `Channel`
    from `/api/connect` details and cloud relay TCP/TLS listeners still need
    generated auth validation/per-user graph selection.
  - Verification:
    `cargo test -p amux services::routing::tests::connector_to_channel_can_attach_bearer_metadata`
    passed; `cargo test -p amux services::routing` passed (15 tests); latest
    full verification passed `cargo test -p amux` (424 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- R1/U1 generated cloud RoutingService per-user graph slice landed:
  - Added `GeneratedCloudRoutingService`, a generated `RoutingService` wrapper
    that authenticates request metadata before handing `Connect` or
    `SubscribeRoutingEvents` to a generated service graph.
  - Added `CloudRoutingAuthenticator` and `JwtCloudRoutingAuthenticator` so
    runtime cloud relay mode can validate `authorization: Bearer ...`
    metadata with the existing JWKS-backed `JwtValidator`.
  - The wrapper lazily creates and retains one `GeneratedServiceGraph` per
    authenticated JWT subject, preserving the cloud relay's per-user isolation
    boundary while using generated routing/tunnel/client services.
  - Made `ensure_user_state` available at crate scope and added narrow
    `ServerState` accessors for `tcp_port` and `jwt_validator` so generated
    service setup does not reach through server internals.
  - Added in-process tonic coverage for missing authorization rejection and
    successful bearer-token selection of the correct per-user generated graph.
  - Transitional note: runtime cloud relay listener selection is not switched
    yet; TLS acceptor integration and outbound cloud reconnection still need to
    build/use generated tonic channels.
  - Verification:
    `cargo fmt && cargo test -p amux services::graph::tests::generated_cloud_routing_service`
    passed (2 tests); `cargo test -p amux services::graph::tests` passed
    (7 tests); latest full verification passed `cargo test -p amux`
    (426 unit tests, 9 embedded tests, 0 doc tests),
    `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `git diff --check`.
- U1 generated cloud external-TLS listener slice landed:
  - Added `GeneratedCloudRoutingService::serve_on_tcp_listener(...)`, which
    serves the authenticated per-user generated cloud RoutingService over a TCP
    listener.
  - Changed `Server::run` so cloud relay mode with
    `enforce_tls_in_cloud_mode = false` hands its TCP listener to generated
    cloud `RoutingService` instead of the legacy framed `tcp_accept` path.
  - The generated cloud listener task is retained and aborted during server
    shutdown alongside the existing listener teardown.
  - Added real-TCP tonic coverage proving a bearer-authenticated connector can
    establish `RoutingService.Connect` through the generated cloud listener.
  - Transitional note: cloud relay mode with in-process TLS termination
    (`enforce_tls_in_cloud_mode = true`) still uses the legacy framed accept
    loop until a generated TLS incoming transport is added. Outbound cloud
    reconnection still needs to build and use generated tonic channels.
  - Verification:
    `cargo fmt && cargo test -p amux services::graph::tests::generated_cloud_routing_service`
    passed (3 tests); `cargo test -p amux services::graph::tests` passed
    (8 tests); latest full verification passed `cargo test -p amux`
    (427 unit tests, 9 embedded tests, 0 doc tests),
    `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `git diff --check`.
- U1/D1 generated cloud TLS listener and runtime TCP cleanup slice landed:
  - Generalized the graph's TCP server transport wrapper so generated tonic
    servers can run over either raw `TcpStream` or accepted TLS streams.
  - Added `GeneratedCloudRoutingService::serve_on_tls_tcp_listener(...)`,
    including TLS handshake timeout handling and TCP keepalive/nodelay setup
    before handing accepted TLS streams to tonic.
  - Changed `Server::run` so cloud relay mode with
    `enforce_tls_in_cloud_mode = true` also serves generated cloud
    `RoutingService`; all daemon TCP listener branches are now generated.
  - Removed the runtime TCP select arm, the network connection limiter tied to
    that legacy arm, and the now-unused `server::accept::tcp_accept` entry
    point.
  - Transitional note: outbound cloud reconnection still uses the legacy
    framed `CloudConnection` path. `server::accept::tcp_connect` and the
    legacy connection loop remain until that dialer is replaced.
  - Verification: `cargo test -p amux services::graph::tests` passed
    (8 tests); `cargo check -p amux` passed warning-clean; latest full
    verification passed `cargo test -p amux` (427 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- U1/D1 generated outbound cloud dialer slice landed:
  - Split `/api/connect` consumption into `CloudRoutingConnectionDetails` so
    the cloud API exchange can feed generated routing without constructing the
    legacy framed `CloudConnection`.
  - Added a TLS-backed tonic `Channel` helper for outbound cloud dialing and
    wired daemon plus embedded cloud auto-connect to
    `RoutingService.Connect` with bearer metadata.
  - Removed the legacy outbound `CloudConnection` handshake path and the
    frame-based cloud reconnection loop from `server::cloud`.
  - Transitional note at this slice: generated cloud token refresh/expiry
    enforcement was still pending and was addressed by the following R1/U1
    reauth slice. The old custom `Reauth` machinery and legacy local
    connection loop remain until the remaining custom-RPC and Windows
    named-pipe paths are replaced.
  - Verification: `cargo check -p amux` passed warning-clean;
    `cargo test -p amux auth::cloud::` passed (9 tests);
    `cargo test -p amux server::cloud::` passed (6 tests);
    `cargo test -p amux services::routing::tests::connector_to_channel_can_attach_bearer_metadata`
    passed (1 test). Latest full verification passed
    `cargo test -p amux` (427 unit tests, 9 embedded tests, 0 doc tests),
    `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `git diff --check`.
- R1/U1 generated cloud reauth and auth-expiry slice landed:
  - Added generated routing auth-session state for cloud `RoutingService`
    links, including authenticated `user_id`, `client_id`, token expiry, and
    minimum client-version enforcement during `Hello` acceptance.
  - Generated acceptor-side `Reauth` now validates replacement tokens through
    the cloud authenticator, requires the same `user_id`, sends
    `ReauthAck` errors for invalid or cross-user tokens, and sends
    `GoAway { AUTH_EXPIRED }` when the active token expires without a
    successful reauth.
  - Generated connector-side cloud links now keep `/api/connect` expiry
    details, proactively refresh through `CredentialProvider`, and send
    generated `Reauth` over the live `RoutingService.Connect` stream.
  - `ConnectionClaims` now carries JWT `exp`, allowing generated cloud routing
    to own the link auth lifetime from validated metadata as required by I-11.
  - Transitional note: the old custom `Reauth` machinery remains only for the
    legacy local/custom-RPC connection loop; remaining D1 work is still to
    remove that loop and the custom RPC framing once all clients use generated
    services.
  - Verification: `cargo test -p amux services::routing::tests::` passed
    (20 tests); `cargo test -p amux services::graph::tests::generated_cloud_routing_service`
    passed (3 tests); `cargo check -p amux` passed warning-clean. Latest full
    verification passed `cargo test -p amux` (432 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, and `git diff --check`.
- D1 legacy connection-loop token refresher cleanup slice landed:
  - Removed the now-dead legacy `RunConnection.token_refresh` plumbing,
    connection-loop token refresh select arms, refresh timeout state, and
    legacy refresh-priority heartbeat handling.
  - Deleted `server::connection::reauth`; generated cloud reauth now lives in
    `services::routing`, while the remaining inbound custom-loop `Reauth`
    handler stays in place for the legacy local/custom-RPC path.
  - Simplified `auth::cloud` to the `/api/connect` detail fetcher and removed
    legacy framed-cloud protocol/update-required error handling that no longer
    has callers after generated outbound cloud dialing.
  - Removed stale generated cloud dialer error variants and the dead
    `ConnectionError::ProtocolMismatch` variant.
  - Verification: `cargo check -p amux` passed warning-clean;
    `cargo test -p amux auth::cloud::` passed (1 test);
    `cargo test -p amux server::connection::driver::tests` passed (4 tests);
    `cargo test -p amux server::protocol_tests::reauth` passed (2 tests).
    Latest full verification passed `cargo test -p amux` (424 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, `cargo check -p e2e-runner`, and
    `git diff --check`.
- U1/C1/D1 generated `ConnectToServer` migration slice landed:
  - Added an establishment signal to generated `RoutingService` connector
    tasks so callers can distinguish accepted `HelloAck` from rejection or
    pre-handshake stream closure instead of treating those cases as clean task
    exits.
  - Wired generated `AdminService.ConnectToServer` through a generated
    `RoutingService.Connect` TCP channel using the service graph's
    `RoutingCore`/`TunnelPool`, and retained the resulting connector task for
    graph teardown.
  - Generated `ClientService.ConnectToServer` now reaches the generated
    connector through its local admin delegate; the old `server::connect_to_server`
    shim was removed. The legacy custom local-dispatch handler still calls
    `tcp_connect` until the remaining custom-RPC path is removed.
  - Added graph coverage proving generated admin `ConnectToServer` establishes
    direct peer routes through a real generated TCP `RoutingService` listener.
  - Verification:
    `cargo test -p amux services::graph::tests::generated_admin_connect_to_server_uses_routing_service_connector`
    passed; `cargo test -p amux services::graph::tests::generated_graph_serves_routing_service_on_tcp_listener`
    passed; `cargo test -p amux services::admin::tests::` passed (6 tests);
    `cargo test -p amux services::client::tests::tonic_client_service_delegates_local_admin_lifecycle_methods`
    passed; `cargo test -p amux services::routing::tests::connector_`
    passed (8 tests); `cargo check -p amux` passed warning-clean. Latest full
    verification passed `cargo test -p amux` (425 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, `cargo check -p e2e-runner`, and
    `git diff --check`.
- D1 local framed IPC removal slice landed:
  - Removed the old framed Unix and Windows named-pipe local transport modules
    plus the platform abstraction that accepted legacy local custom-RPC
    connections.
  - Removed the daemon runtime's Windows/local legacy accept branch; daemon
    clients now use generated Unix `ClientService` on Unix or embedded
    in-process channels. Non-Unix daemon `open` now reports that generated
    local `ClientService` is not implemented for that platform instead of
    falling back to framed named pipes.
  - Simplified the public client connection shim so the remaining legacy
    `ClientRuntime` is memory-backed for protocol tests only. The generated
    Unix connector now pre-opens the Unix stream before building the tonic
    channel, preserving CLI stale-socket detection via `ConnectError::Transport`.
  - Deleted `transport/local.rs`, `transport/named_pipe.rs`, and
    `transport/unix.rs`; the transport module is now explicitly marked as a
    transitional length-prefixed layer for the remaining custom protocol
    harness and legacy dispatch cleanup.
  - Added transitional dead-code allowances to the now-production-dead custom
    protocol modules so `cargo check -p amux` remains warning-clean while the
    test harness still covers them.
  - Verification:
    `cargo test -p amux --test embedded daemon_open_uses_generated_local_client_service`
    passed; `cargo test -p amux server::protocol_tests::local_list_agents_runs_through_real_connection_loop`
    passed; `cargo test -p amux transport::` passed (16 tests);
    `cargo check -p amux` passed warning-clean. Latest full verification
    passed `cargo test -p amux` (422 unit tests, 9 embedded tests,
    0 doc tests), `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1/C1 public client legacy-runtime gating slice landed:
  - Made `client::connection`, `client::rpc::runtime`,
    `Client::new(Connection)`, the `client::Connection` re-export, legacy
    stream variants, and all custom-RPC public-client fallback bodies
    `#[cfg(test)]`.
  - Normal builds of the public `Client` now use the generated
    `ClientServiceClient` surface only. A malformed internal client with no
    generated channel returns an explicit unexpected-client error instead of
    falling into custom RPC.
  - Kept the protocol harness on the memory-backed legacy client path so the
    remaining custom framing/dispatch cleanup still has test coverage.
  - Verification:
    `cargo test -p amux server::protocol_tests::local_list_agents_runs_through_real_connection_loop`
    passed; `cargo test -p amux services::client::tests::tonic_client_service_delegates_local_admin_lifecycle_methods`
    passed; `cargo test -p amux services::graph::tests::generated_admin_connect_to_server_uses_routing_service_connector`
    passed; `cargo test -p amux --test embedded daemon_open_uses_generated_local_client_service`
    passed; `cargo test -p amux client::rpc::runtime::tests`
    passed. Latest full verification passed `cargo test -p amux` (423 unit
    tests, 9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, `cargo check -p e2e-runner`, and
    `git diff --check`.
- D1 legacy TCP accept/dialer isolation slice landed:
  - Made `server::accept` test-only. The old protobuf handshake acceptor and
    legacy `tcp_connect` helper still compile for protocol tests, but are no
    longer part of normal builds.
  - Removed the legacy custom-RPC local `Admin.ConnectToServer` dispatcher's
    call into `tcp_connect`; it now returns `Unimplemented` and points callers
    at the generated `ClientService` path.
  - Gated accept-only connect-error encoders and old transport re-exports to
    tests, then marked the remaining transitional RPC/state scaffolding as
    dead-code-tolerant for the next D1 cleanup slice.
  - Verification:
    `cargo test -p amux server::dispatch::local::tests::legacy_connect_to_server_returns_unimplemented`
    passed; `cargo test -p amux server::dispatch::local::tests::` passed
    (7 tests); `cargo test -p amux server::accept::tests::` passed
    (12 tests);
    `cargo test -p amux services::graph::tests::generated_admin_connect_to_server_uses_routing_service_connector`
    passed; `cargo check -p amux` passed warning-clean. Latest full
    verification passed `cargo test -p amux` (423 unit tests,
    9 embedded tests, 0 doc tests), `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, `cargo check -p e2e-runner`, and
    `git diff --check`.
- D1 legacy dispatch/connection isolation slice landed:
  - Made `server::dispatch` and the old framed `server::connection` loop
    test-only. Normal daemon and embedded builds now enter through generated
    tonic service graph paths, while the protocol harness still compiles the
    custom dispatcher for regression coverage.
  - Gated legacy service adapters that took custom-RPC stream/runtime types:
    `RoutingServiceCtx`, custom `RoutingService::subscribe_routing_events`,
    `AgentService::list`, `AgentService::resolve`,
    `AgentService::subscribe_agent_events`, and
    `SubscribeSessionCall`/legacy `AgentService::subscribe_session`.
  - Trimmed normal-build re-exports for old dispatcher-only RPC resources,
    accept validation, legacy routing snapshot helpers, and custom event
    decoders. Remaining old RPC output/session lifecycle modules are marked
    dead-code-tolerant until the custom RPC state harness is deleted.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 length-prefixed transport isolation slice landed:
  - Made `transport::framing`, `transport::handshake`, `transport::memory`,
    the `Transport`/`MessageReader`/`MessageWriter`/`TransportSplit` traits,
    and framed `TcpTransport` test-only. Normal builds now retain only
    `TransportError`, TLS channel/listener helpers, and TCP keepalive
    configuration.
  - Gated the old top-level custom runtime `Message::encode/decode` helpers
    and their protobuf re-exports to tests. Generated services continue to use
    typed tonic/protobuf conversions directly.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 legacy session-subscription lifecycle isolation slice landed:
  - Made `server::session_subscription_lifecycle` test-only and gated its
    re-exports, along with the legacy `ServerUserState.session_subscriptions`
    map and `SessionSubscriptionState` record.
  - Changed normal `AgentService::delete` to use only the generated
    `local_session_close_events` path for `SessionClosed { agent_deleted }`;
    the custom-RPC subscriber closing path remains compiled for protocol tests.
  - Gated the old session-output payload encoder and dispatcher-only inbound
    cleanup re-exports to tests so `cargo check -p amux` stays warning-clean.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 legacy frame-reply shutdown/suspend isolation slice landed:
  - Made the old `ShutdownRequest::Shutdown` and `ShutdownRequest::Suspend`
    variants test-only. Normal daemon and embedded lifecycle handling now
    accepts only generated `GeneratedShutdown`/`GeneratedSuspend` requests.
  - Gated the legacy frame-response branches, deferred reply plumbing, and
    `notify_other_clients` helper to tests. Normal `notify_local_clients` is
    now an explicit no-op because generated local clients are not tracked in
    the old framed connection map.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 custom RPC state-harness isolation slice landed:
  - Trimmed normal `ServerUserState` down to local agents plus generated event
    sources. The old connection handles, route/host maps, remote name owners,
    route subscription state, and RPC-dispatch lookup helpers now compile only
    for protocol tests.
  - Made `server::rpc_dispatcher`, `server::rpc_output`, and the top-level
    custom `rpc` module test-only. Normal builds no longer compile the custom
    RPC state machine or server-stream sink helpers.
  - Changed normal topology broadcasting to emit only generated local agent
    events. Legacy custom-RPC routing/agent subscriber fanout remains
    test-only.
  - Simplified normal server debug output so it no longer reads stale legacy
    route/host/peer maps; verbose route/host detail remains test-only with the
    old protocol harness.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 public protocol re-export cleanup slice landed:
  - Stopped normal public builds from re-exporting the legacy custom-RPC
    handshake and frame envelope types from `amux::protocol`. Generated-era
    public payload types remain exported for CLI/UI and service clients.
  - Gated unused legacy envelope convenience helpers to tests so the normal
    build remains warning-clean while old protocol harness coverage remains
    available.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 transport error surface cleanup slice landed:
  - Made the legacy custom-frame encode/decode and length-prefixed validation
    `TransportError` variants test-only; normal public builds now expose only
    I/O and config transport failures used by generated local IPC/TLS paths.
  - Removed the transitional dead-code allowance from `transport.rs`.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy handshake module isolation slice landed:
  - Moved `PROTOCOL_VERSION` to the generated-era public protocol surface and
    made the old `protocol::handshake` module test-only.
  - Removed the legacy handshake module's dead-code allowance; custom
    `Connect`/`ConnectResult`/`RoutingRole` remain available only to the old
    protocol harness.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy `TransportMessage` codec isolation slice landed:
  - Gated the old `protocol::wire::runtime` `TransportMessage` encoder/decoder,
    routing-event payload codec, route helpers, and GoAway reason conversions
    to tests. Normal builds keep only the host and agent event conversions used
    by generated gRPC services.
  - Made `CallId` and the top-level legacy envelope types (`Message`, `Frame`,
    `GoAway`, `Reauth*`) test-only while leaving `FrameBody`, `RequestFrame`,
    and `ResponseFrame` normal for current transitional session/lifecycle
    converters.
  - Removed the runtime wire module's dead-code allowance.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy lifecycle/session frame-wrapper isolation slice landed:
  - Made `protocol::agent_lifecycle` test-only and moved legacy agent
    lifecycle response-frame codecs behind test cfg.
  - Hid `FrameBody`/`ResponseFrame` and the legacy subscribe-session frame-body
    decoder from normal builds. `RequestFrame` remains normal only as a narrow
    temporary adapter for generated protobuf requests.
  - Removed the `protocol/wire/agent_rpc.rs` dead-code allowance after gating
    legacy request/response payload helpers and route adapters to tests.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 custom method-registry isolation slice landed:
  - Removed the `protocol/method.rs` dead-code allowance.
  - Kept generated service method-name constants available to normal code, but
    made the custom dispatcher `MethodSpec` registry, access/kind model, and
    lookup helpers test-only.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- C1/U1 generated service dead-code cleanup slice landed:
  - Removed file-wide dead-code allowances from `services/client.rs` and
    `services/graph.rs`.
  - Made test-only `ClientService` constructors/subscription helpers explicit,
    deleted two unused client helper paths, and kept graph internals that tests
    inspect behind `#[cfg(test)]`.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 generated-service helper allowance cleanup slice landed:
  - Removed normal-build dead-code allowances from generated service helpers
    that are used by the tonic service graph/admin/cloud connector paths.
  - Made test-only helper entry points explicit for the agent tonic test server,
    unauthenticated/bearer routing connector test spawners, the direct
    connector stream harness, and terminal-link generator.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (423 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy custom-protocol integration harness deletion slice landed:
  - Deleted `server/protocol_harness.rs` and `server/protocol_tests.rs`, removing
    the old end-to-end custom frame/RPC harness from the test build.
  - Removed the now-unused legacy `Client::new(Connection)` constructor and the
    obsolete aggregate inbound-call helper that only the harness used.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (389 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 public-client custom runtime deletion slice landed:
  - Deleted `client/connection.rs` and `client/rpc/runtime.rs`.
  - Simplified `Client` to require the generated `ClientService` channel for
    every public operation; test-only legacy stream variants and custom-RPC
    fallback bodies are gone.
  - Removed the now-unused test-only `agent_entry_to_domain` re-export from the
    wire module.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (386 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy accept-handshake source deletion slice landed:
  - Deleted `server/accept.rs` and `transport/handshake.rs`.
  - Removed framed `TcpTransport` and accept-only connect error response
    encoders; `transport/tcp.rs` now only carries TCP socket helpers.
  - Removed now-unused accept/handshake re-exports and role helper methods.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (372 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`, and
    `cargo check -p e2e-runner`.
- D1 legacy custom RPC server-runtime source deletion slice landed:
  - Deleted the remaining custom RPC state module plus the test-only server
    connection, dispatch, RPC dispatcher/output, session-subscription lifecycle,
    route-forwarding, memory transport, and length-prefixed framing files.
  - Simplified `ServerUserState` to generated-service local-agent state and
    removed legacy route/remote-host maps, frame-reply shutdown variants, and
    custom session-subscription bookkeeping.
  - Removed legacy custom-RPC service adapters from `services/agent.rs`,
    `services/agent/session_rpc.rs`, and `services/routing.rs`; generated tonic
    streams remain the only service path.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (291 unit tests, 9 embedded tests, 0 doc tests). Latest full verification
    passed `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check`.
- D1 obsolete protocol/proto cleanup slice landed:
  - Deleted `protocol/agent_lifecycle.rs`, `protocol/handshake.rs`, and the
    legacy `protocol/message/envelope.rs` custom-RPC domain wrappers.
  - Trimmed `amux.proto` to generated services plus the `RoutingService.Connect`
    `Message` envelope; removed obsolete `TransportMessage`, `Frame`,
    `FrameBody`, `Request`, `Response`, `StreamItem`, `Cancel`, `call_id`,
    `ConnectRequest`, `ConnectResponse`, heartbeat, and `RoutingRole` schema.
  - Replaced transitional `RequestFrame` decoders with direct `(method,
    payload)` generated-service payload decoders and renamed agent mutation
    helpers away from the deleted lifecycle wrapper terminology.
  - Removed test-only method registry, stale routing/tunnel module-wide
    dead-code allowances, and stale migration comments.
  - Verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (258 unit tests, 9 embedded tests, 0 doc tests); `cargo check -p amux-cli`,
    `cargo check -p amux-ui`, `cargo check -p e2e-runner`, and
    `git diff --check` passed. `rg` for obsolete custom-RPC names under
    `crates/amux/src` and `crates/amux/proto` is clean.
- C1 completion audit slice landed:
  - Marked C1 done after mapping C-1 through C-11 and I-4 to the generated
    ClientService implementation and client/server graph:
    local Unix/in-process client endpoints serve only `ClientService`; host
    services serve `AgentService` only over tunnels; routing listeners serve
    `RoutingService` for host links; public clients wrap a generated
    `ClientServiceClient`.
  - Existing coverage proves AgentRef name/id resolution, relay filtering,
    local lifecycle/session/input/admin/hook dispatch, remote lifecycle/session
    and input dispatch over `TunnelPool`, remote inventory subscriptions on
    `HostAdded`, and agent teardown on `HostRemoved`.
  - Added targeted C-6/C-7 coverage: concurrent remote `SubscribeSession`
    calls to the same agent receive independent output streams, dropping one
    subscriber leaves the other live, and dropping the downstream wrapper drops
    its owned upstream stream.
  - Verification:
    `cargo test -p amux services::client::tests::` passed (20 client-service
    tests); `cargo check -p amux` passed warning-clean; `cargo test -p amux`
    passed (260 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- P1 protocol-surface completion slice landed:
  - Removed stale AgentService inventory/resolve schema from `amux.proto`:
    `AgentEntry`, route-bearing `ListAgentsResponse`, and `ResolveAgent*`.
    ClientService now uses the spec-shaped `ListAgentsResponse { repeated
    Agent agents }`.
  - Removed generated `HookService` and `AdminService` from the protobuf
    surface; their Rust service structs remain in-process helpers delegated
    through generated `ClientService` only.
  - Updated generated-client method labels to `ClientService` method names and
    tightened the descriptor test to assert the generated service set is
    exactly `RoutingService`, `AgentService`, and `ClientService`.
  - Verification:
    `cargo test -p amux services::admin::tests::` passed (6 tests);
    `cargo test -p amux services::hook::tests::` passed (1 test);
    `cargo test -p amux protocol::wire::generated::tests::descriptor_set_contains_core_protocol_messages_and_services`
    passed; `cargo check -p amux` passed warning-clean; `cargo test -p amux`
    passed (259 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- S1 startup delta-subscription slice landed:
  - Added `AgentService::subscribe_agent_events(...)` as the deltas-only
    in-process subscription primitive.
  - Changed ClientService startup attachment to consume deltas-only
    `RoutingCore::subscribe_hosts()` and `AgentService::subscribe_agent_events()`;
    snapshot subscriptions remain on gRPC/network-facing boundaries.
  - Made `RoutingCore::subscribe_hosts_with_snapshot()` test-only now that
    normal startup no longer uses it.
  - Verification:
    `cargo test -p amux services::client::tests::attach_` passed (2 tests);
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (259 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- S1 network-subscriber backpressure/completion slice landed:
  - Event-source-backed generated streams now return `RESOURCE_EXHAUSTED` once
    their live event receiver closes after the subscriber queue is dropped:
    `AgentService.SubscribeAgentEvents`, `RoutingService.SubscribeRoutingEvents`,
    `ClientService.SubscribeHosts`, and `ClientService.SubscribeAgents`.
  - ClientService's in-process startup subscription tasks now log an error if
    their delta stream ends, making subscriber loss visible.
  - Marked S1 done after auditing S-1 through S-6 against generated service
    shims, direct in-process service-method calls, startup delta subscriptions,
    atomic snapshot helpers, and subscriber backpressure behavior.
  - Verification:
    `cargo test -p amux resource_exhausted_when` passed (3 tests);
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (262 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- R1 completion audit slice landed:
  - Added explicit generated `RoutingService.Connect` coverage for same-user
    host-id collision: an acceptor returns `ALREADY_EXISTS` and preserves the
    first established route.
  - Marked R1 done after auditing 5.1-5.7, R-1-R-4, and I-5-I-12 against
    the routing core, handshake state machine, wire codecs, generated
    connector/acceptor loops, metadata auth/cloud routing wrapper, reauth
    timers, GoAway handling, route teardown, and loop-prevention filters.
  - Verification:
    `cargo test -p amux services::routing::tests::acceptor_rejects_host_id_collision_without_displacing_existing_route`
    passed; `cargo check -p amux` passed warning-clean; `cargo test -p amux`
    passed (263 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- T1 completion audit slice landed:
  - Added shared tonic HTTP/2 keepalive configuration for generated gRPC
    servers plus outbound routing/tunnel channels.
  - Tightened tunnel forwarding so a routed `TunnelFrame` with an initiator
    host id emits that initiator's `HostUp` on the same outgoing link before
    the first frame, while avoiding echoing the initiator back toward its own
    origin route.
  - Added tunnel-pool coverage for endpoint target mismatch, HostUp-before-frame
    ordering, and origin-route non-echo behavior; marked T1 done after auditing
    6.1-6.8 and T-1-T-10 against tunnel construction, registry keys, forwarding,
    target-side incoming transports, route teardown, no-idle-GC semantics, and
    no-wait `NOT_FOUND`.
  - Verification:
    `cargo test -p amux tunnel::` passed (16 tunnel tests);
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (266 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- A1 completion audit slice landed:
  - Added explicit lifecycle coverage showing `CreateAgent`, `RenameAgent`,
    and `DeleteAgent` emit `AgentUp`, full `AgentUpdated`, and `AgentDown`
    before returning, keep emitted agents host-local, preserve advisory
    `readonly = false` for public creation, and enforce per-host name
    uniqueness with `ALREADY_EXISTS`.
  - Added an explicit lag signal to retained session buffers and mapped lagged
    `SubscribeSession` readers to gRPC `RESOURCE_EXHAUSTED` instead of an
    in-band `SessionClosed`, satisfying A-12 without changing normal EOF /
    delete semantics.
  - Cleaned a stale AgentService proto comment that still referenced the
    removed host-id request filter.
  - Marked A1 done after auditing 4.6 and A-1-A-12 against AgentService,
    session I/O, hook-created readonly sessions, first-party io-protocol
    codecs, retained buffers, and ClientService-only `HostUnreachable`
    synthesis.
  - Verification:
    `cargo test -p amux services::agent::` passed (8 agent-service tests);
    `cargo test -p amux buffer::tests::test_full_subscriber_reports_lagged`
    passed; `cargo check -p amux` passed warning-clean; `cargo test -p amux`
    passed (269 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- U1 completion audit slice landed:
  - Routed ClientService's synthesized host-unreachable session close through
    the shared session conversion, removing the remaining normal-path
    generated-service dead-code allowance.
  - Cleaned stale server-runtime comments that still described custom accept
    tasks and websocket-era cloud validation.
  - Marked U1 done after auditing 6.7, 7.1, and 10 against daemon and embedded
    startup: `GeneratedServiceGraph` construction, startup event attachments,
    host-service `serve_with_incoming`, local Unix and in-process ClientService
    serving, generated TCP/TLS cloud RoutingService serving, background cloud
    connector startup, and generated public client attachment.
  - Verification:
    `cargo test -p amux services::graph::tests::` passed (9 graph tests);
    `cargo test -p amux --test embedded` passed (9 embedded tests);
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (269 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- V1 final completion audit landed:
  - Confirmed every implementation checkpoint row is `done`: P1, S1, R1, T1,
    A1, C1, U1, and D1.
  - Re-ran stale source/proto scans for obsolete custom-RPC/framing/service
    surface names including `TransportMessage`, `FrameBody`, `call_id`,
    `RoutingRole`, old connect request/response schema, stale AgentService
    inventory/resolve schema, generated Admin/Hook services, websocket/framed
    transport paths, server accept/connection/dispatch paths, and client
    connection/runtime paths; the scan returned no matches.
  - Re-ran generated-service cleanup scan; only the expected V1 ledger row and
    test helper names matched, plus the unrelated platform sleep-inhibitor
    drop-cleanup allowance outside this refactor surface.
  - Final verification:
    `cargo check -p amux` passed warning-clean; `cargo test -p amux` passed
    (269 unit tests, 9 embedded tests, 0 doc tests);
    `cargo check -p amux-cli`, `cargo check -p amux-ui`,
    `cargo check -p e2e-runner`, and `git diff --check` passed.
- External review reopened the completion audit:
  - `amux.proto` was updated externally to remove
    `RoutingService.SubscribeRoutingEvents`, `SubscribeRoutingEventsRequest`,
    and `SubscribeRoutingEventsResponse`; this intentionally broke generated
    service code still exposing the forbidden RPC path.
  - Current compile break is concentrated in
    `crates/amux/src/services/routing.rs`,
    `crates/amux/src/services/graph.rs`,
    `crates/amux/src/client/rpc.rs`, and
    `crates/amux-ui/src/inventory.rs`.
  - E2E status from the review: `cargo run -p e2e-runner -- run` had 8 passes
    and 5 failures; all remote-flow specs failed because clients could not see
    remote agents, and `server_suspend_notification` received ordinary session
    ending instead of the suspend notification.
  - Remaining spec/cleanup gaps to close before marking V1 done again:
    remove the forbidden routing-event RPC surface completely; migrate client
    inventory semantics to `ClientService.SubscribeHosts`; debug and fix remote
    agent visibility end to end; fix suspend notifications; audit auth against
    I-11's interceptor/claims-extension structure; remove or rename old
    file-layout leftovers that still leak the previous architecture.
  - Required review sequence before final close: implementation spec-review
    subagents, simplification subagents, correctness subagents, and a final
    spec-review subagent round, with findings recorded here.
- Resumed implementation fixes landed:
  - Removed the generated `RoutingService.SubscribeRoutingEvents` RPC shims,
    response helpers, and tests from `services/routing.rs` and
    `services/graph.rs`; routing events now leave a host only through the
    in-band `Message.routing_event` path after `RoutingService.Connect`
    handshake.
  - Renamed the public generated client host stream from
    `subscribe_routing_events` / `RoutingEventStream` to
    `subscribe_hosts` / `HostEventStream`, and migrated `amux-ui` inventory to
    `ClientService.SubscribeHosts` semantics.
  - Fixed remote agent visibility by popping the first hop before encoding
    tunnel frame destinations. `TunnelPool::channel_to` and target-side tunnel
    creation now pass the already-consumed destination route into
    `create_tunnel`; added generated-graph regression coverage for remote
    agent subscription after routing connect.
  - Reinstated generated-session shutdown notification: `notify_local_clients`
    emits a local shutdown event, direct session streams return
    `UNAVAILABLE` with the shutdown reason, and the public client maps that
    status to `ClientError::ServerShutdown`.
  - Updated stale route-prefix e2e expectations. ClientService now hides route
    details from remote attach/list flows; `remote_connection` attaches by
    agent name and `remote_list_agents` no longer expects `(via host-b)`.
  - Verification:
    `cargo check -p amux`, `cargo check -p amux-ui`,
    `cargo check -p amux-cli`, and `cargo check -p e2e-runner` passed after
    the proto cleanup; targeted tests for remote graph subscription and
    generated session shutdown passed; `cargo run -p e2e-runner -- run` passed
    all 13 tests.
- Implementation spec-review round 1 findings to address:
  - Public API still leaked old client-route semantics:
    `AgentEntry.route`, `SubscribeSessionRequest.route`, and
    `SendInputRequest.route` were still present even though generated
    ClientService ignores routes and resolves by `AgentRef`.
  - Public `Client::subscribe_agent_events(host_id)` and `amux-ui` inventory
    still projected a host-scoped subscription model over the aggregate
    `ClientService.SubscribeAgents` stream.
  - Public `CreateAgentRequest` lacked the optional ClientService `host_id`
    target.
  - Remote inventory subscription had an unconditional retry loop and removed
    agents on any upstream subscription error; C-8/C-9 say `HostAdded` starts
    subscription and `HostRemoved` is authoritative cleanup.
  - Routing emitted `HostAdded` before the link writer was registered, making
    that retry loop load-bearing for direct peers.
  - CLI/UI ignored structured `SessionClosed` reasons, and typed generated
    shutdown currently crosses the client boundary via an exact status-message
    match.
  - E2E comments still referenced route-prefix/ResolveAgent language.
- Implementation spec-review round 1 fixes landed:
  - Removed public route-bearing client API state: deleted exported
    `AgentEntry`, removed `route` from public `SubscribeSessionRequest` and
    `SendInputRequest`, and made public `list_agents`, `resolve_agent`,
    `create_agent`, and `rename_agent` return plain `Agent` values.
  - Added optional `host_id` to public `CreateAgentRequest` and encode it into
    `ClientCreateAgentRequest.host_id`, so remote create targeting is explicit
    and aligned with ClientService.
  - Replaced the public per-host `subscribe_agent_events(host_id)` façade with
    aggregate `subscribe_agents()`. `amux-ui` now starts exactly one
    `SubscribeHosts` task and one `SubscribeAgents` task and relies on
    ClientService `AgentDown` events for cleanup.
  - Changed remote agent subscription failure handling to stop/log without
    deleting cached agents; only `HostRemoved` clears remote agents. Added a
    regression test that subscription errors leave cached agents untouched.
  - Registered tunnel link writers before storing direct peers / emitting
    reachability from `RoutingService.Connect`, removing the startup race that
    made remote inventory retry behavior mask a missing tunnel.
  - CLI and UI now surface structured `SessionClosed` reasons instead of
    treating every close as an ordinary ended session, and stale route-prefix
    e2e comments were updated.
  - Verification so far: `cargo check --workspace --all-targets` is clean after
    this API cleanup.
  - Test verification after the fixes: `cargo test --workspace --lib` passed
    (`270` amux lib tests + `1` amux-ui lib test), and
    `cargo run -p e2e-runner -- run` passed all `13` e2e tests.
- Implementation spec-review round 2 findings to address:
  - No protobuf/API-boundary blockers: reviewers confirmed
    `RoutingService` now exposes only `Connect`, routing events/tunnel frames
    are in the `Message` envelope, and the normal public client path no longer
    exposes route-bearing agent entries.
  - Tunnel frames still accepted missing `dst` and forwarded frames without a
    `tunnel_id`, preserving route-only semantics from the old protocol.
  - Public session streams still exposed old terminal
    `SubscribeSessionFrame::Response(...)` semantics even though V1
    `SubscribeSessionResponse` is event-only.
  - Remote `SubscribeSession` mapped upstream `UNAVAILABLE` to
    `host_unreachable` only after the upstream stream was created; pre-stream
    `UNAVAILABLE` still escaped as a gRPC error.
  - Client host inventory handling was rechecked against C-3; relays with empty
    `supported_agent_types` must stay excluded from client-facing host
    inventory.
  - Shutdown/suspend reasons still crossed the gRPC boundary by exact status
    message matching.
  - UI session notifications collapsed structured close reasons into
    unstructured strings.
  - Routing handshake errors for protocol-version mismatch and invalid link
    names should use the structured error details already defined in the proto.
  - Lower-priority residue: cloud routing auth still authenticates in the
    service wrapper rather than a literal tonic interceptor/claims-extension
    shape; file structure still differs from the spec; public Rust still
    exports internal `Route`/`Link` types and some client methods are id-first
    despite generated `AgentRef` support.
- Implementation spec-review round 2 fixes landed:
  - `TunnelPool` now requires both `TunnelFrame.dst` and
    `TunnelFrame.tunnel_id` on every inbound frame, including forwarded frames;
    tests cover missing-field rejection and forwarding preserving the tunnel
    id.
  - Removed public `SubscribeSessionFrame::Response(...)`; public
    `SessionStream` now yields only V1 `SubscribeSessionEvent` values, with
    `Closed` as the terminal event and premature EOF as an unexpected stream
    error.
  - `ClientService` maps pre-stream and post-stream remote `UNAVAILABLE` to
    in-band `SessionClosed { host_unreachable }`.
  - `ClientService` host inventory remains aligned with C-3 by filtering relays
    with empty `supported_agent_types`; the routing core still tracks them.
  - Shutdown/suspend reasons now cross generated gRPC as typed status metadata
    (`amux-shutdown-reason`) instead of exact message matching.
  - `amux-ui` preserves structured session close reasons in
    `SessionFailureReason`.
  - Routing handshake failures for protocol-version mismatch and invalid
    proposed link names now use structured proto `ErrorDetail` values, with
    regression tests.
  - Public Rust client rename/delete/session/input calls now accept
    `AgentIdentifier` and encode generated `AgentRef`, including name-based
    subscribe/send coverage in the generated graph test.
  - Verification so far: `cargo check --workspace --all-targets` passed after
    these changes; targeted tests for tunnel frame validation/forwarding,
    host inventory, remote session `UNAVAILABLE`, shutdown metadata,
    structured handshake errors, and name-based public client calls passed.
  - Full verification after fixing stale routing test assumptions:
    `cargo test --workspace --lib` passed (`273` amux lib tests + `1`
    amux-ui lib test), and `cargo run -p e2e-runner -- run` passed all `13`
    e2e tests.
- Implementation spec-review round 3 findings to address:
  - Public `Client::subscribe_session` still eagerly consumed the first stream
    event and rejected anything except `SessionOpened`, so a valid first
    `SessionClosed { host_unreachable }` from ClientService was converted into
    `ClientError::Unexpected`.
  - Connector-side `HelloAccepted` validation still returned unstructured
    strings for protocol-version mismatch and invalid assigned link names.
  - Typed `AmbiguousAgentName` details were encoded for the in-band protocol
    but flattened through generated tonic statuses.
  - Notes and the ignored amux-ui runtime-test comment retained stale wording
    from the temporary host-inventory interpretation.
  - Remaining low-priority residue: cloud routing auth still uses wrapper
    validation rather than a literal tonic interceptor/claims-extension path,
    and internal `Route`/`Link` remain public exports.
- Implementation spec-review round 3 fixes landed:
  - Public `Client::subscribe_session` now returns the generated stream without
    consuming the first event; first-event `SessionClosed` is preserved for the
    caller.
  - Client host inventory is back to spec C-3: relay-only hosts are filtered
    from public `ListHosts`/`SubscribeHosts`, while their routing events still
    drive agent subscriptions internally.
  - Connector-side `HelloAccepted` validation now sends structured `GoAway`
    protocol errors for protocol-version mismatch and invalid assigned link
    names.
  - Generated tonic statuses now carry encoded `amux.v1.Error` details for
    protocol errors, and the public client decodes those details back into
    typed `ProtocolError` variants. The generated graph public-client test now
    covers typed `AmbiguousAgentName` preservation through that path.
  - Stale host-inventory notes and the ignored amux-ui runtime-test comment
    were corrected to match the current public API shape.
  - Verification after round 3 fixes:
    `cargo check --workspace --all-targets` passed; focused regressions for the
    public first-session-close stream, public typed ambiguous-name decoding,
    C-3 host filtering, and connector structured `GoAway` passed;
    `cargo test --workspace --lib` passed (`276` amux lib tests + `1` amux-ui
    lib test); `cargo test --workspace --all-targets` passed; `cargo run -p
    e2e-runner -- run` passed all `13` e2e tests; `git diff --check` passed.
- Follow-up public API cleanup while implementation spec-review round 4 runs:
  - `protocol` is no longer a public crate module. The crate root now exports
    only client-facing domain types (`Agent`, `Host`, `AgentEvent`,
    `HostEvent`, `ProtocolError`, `SubscribeSessionEvent`,
    `SessionCloseReason`, etc.).
  - `Route` is no longer re-exported from the crate root, and CLI/UI callers no
    longer reach through `amux::protocol`.
  - Removed unused old route-stack helpers (`Route::send`, `Route::reply`,
    route prefix rewriting) plus the unused test-only send-control encoder.
  - Verification: `cargo check --workspace --all-targets` passed with no
    warnings after this cleanup.
- Implementation spec-review round 4 findings and fixes:
  - `ClientService.CreateAgent(host_id=...)` now rejects targets that are not
    in the public agent-capable host model. Relay-only hosts remain filtered by
    C-3 and cannot be targeted by guessing a host id.
  - Remote `SubscribeSession` maps plain tunnel `UNAVAILABLE` to in-band
    `SessionClosed { host_unreachable }`, but preserves `UNAVAILABLE` statuses
    carrying `amux-shutdown-reason` metadata so shutdown/suspend is not
    mislabeled as host loss.
  - Remote `AgentService.SubscribeAgentEvents` failures now mark cached agents
    for that host down and retry while the host remains reachable.
  - Direct unauthenticated local-daemon `RoutingService` TCP links were removed
    from production startup. Non-cloud `tcp_port` is ignored for routing
    listeners, and `ConnectToServer` now rejects manual direct links instead of
    dialing a plain channel.
  - Authenticated remote inventory is covered through
    `GeneratedCloudRoutingService` with bearer metadata; the old direct TCP e2e
    tests were removed and replaced with `direct_connect_reject`.
  - `AgentService.CreateAgentRequest.agent_id` is now required at the wire
    boundary, and `RenameAgentRequest.name` / `ClientRenameAgentRequest.name`
    reject empty strings.
  - The generated descriptor test now asserts exact method sets, including that
    `RoutingService` contains only `Connect`.
  - Verification after round 4 fixes:
    `cargo check --workspace --all-targets` passed with no warnings; targeted
    client/graph/agent/wire tests passed; `cargo test --workspace --lib`
    passed (`275` amux lib tests + `1` amux-ui lib test);
    `cargo test --workspace --all-targets` passed; `cargo run -p e2e-runner
    -- run` passed all `10` current e2e tests; `git diff --check` passed.
- Simplification review round 1 findings and fixes:
  - Removed the dead direct `ConnectToServer` gRPC/admin/client path from the
    proto, public Rust client, server services, and docs. `amux server connect`
    now rejects locally with a cloud-connector migration hint instead of
    contacting the daemon through a removed RPC.
  - Local daemons no longer bind `tcp_port` before ignoring it; only cloud
    relays bind the authenticated `RoutingService` listener.
  - Removed e2e runner `tcp_port` allocation/substitution and deleted the
    direct-connect rejection e2e. The scripted suite now reflects the current
    cloud-connector model.
  - Generated gRPC handlers no longer re-encode typed protobuf requests into
    bytes and decode them by old method-name strings. Production paths now use
    direct typed wire converters; legacy payload decoders remain test-only for
    codec coverage.
  - Flattened the public `Client` wrapper to store `GeneratedClient` directly
    and removed single-variant generated stream enums plus the impossible
    "generated ClientService unavailable" branch.
  - Removed stale `idle_timeout_secs` config and validation, dropped stale
    e2e `check_for_updates` YAML output, and replaced outdated architecture
    docs with canonical pointers to `docs/NEW_ARCHITECTURE.md`.
  - Verification after simplification round 1:
    `cargo check --workspace --all-targets` passed with no warnings;
    `cargo test --workspace --lib` passed (`271` amux lib tests + `1`
    amux-ui lib test); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `9` current e2e tests;
    `git diff --check` passed.
- Simplification review round 2 findings and fixes:
  - Config, state, and auth YAML parsing now rejects unknown fields. This
    removes compatibility-style silent acceptance of stale keys while the
    schema is still free to break.
  - Daemon host IDs are generated fresh at startup and are no longer persisted
    in amux state. `State` now stores only amux-owned integration state.
  - Removed the visible `amux server connect` command, its client wrapper, and
    the last user-facing direct-link surface instead of keeping a command that
    always rejects locally.
  - Removed old full-session custom-RPC payload helpers:
    `encode_raw_v1_subscribe`, `SessionCodecError`, legacy
    subscribe/input request encoders, old method-name constants, and
    method-name mutation payload decoding. Remaining tests exercise typed
    protobuf conversion directly.
  - Made the internal `agent` runtime module private at the crate root and
    exposed only a narrow `claude_io` module for the CLI/UI data it still
    needs.
  - Removed amux-owned `Unknown` variants from protocol/domain enums and their
    silent ignore branches. External provider payloads that genuinely need
    tolerance, such as Claude hook events, still keep provider-local unknown
    handling.
  - Replaced stale `CLAUDE.md` architecture guidance with a short pointer to
    `AGENTS.md`, `docs/NEW_ARCHITECTURE.md`, and this ledger.
  - Deferred broader simplification findings for a later slice, but not for
    goal completion:
    moving local agent state fully out of `ServerUserState` into an
    AgentService-owned state object, making `ClientService` impossible to
    partially wire, collapsing duplicate `AgentRecord`/`Agent` conversion
    layers, and renaming transitional `Generated*` identifiers. Those are
    cross-cutting ownership/mechanical refactors rather than stale-surface
    removals, and must be revisited before `V1` is marked `done`.
  - Verification after simplification round 2:
    `cargo fmt` ran with the existing stable-rustfmt warnings for nightly-only
    import options; `cargo check --workspace --all-targets` passed; focused
    tests for agent RPC wire conversion, Claude IO args, session event decode,
    state parsing, and auth parsing passed; `cargo test --workspace --lib`
    passed (`269` amux lib tests + `1` amux-ui lib test);
    `cargo test --workspace --all-targets` passed; `cargo run -p e2e-runner
    -- run` passed all `9` current e2e tests; `git diff --check` passed.
- Correctness review round 1 findings and fixes:
  - Protobuf string paths now reject non-UTF8 paths instead of using lossy
    conversion. This was fixed for public ClientService create requests and
    AgentService/ClientService agent inventory events, with regressions for
    both paths.
  - `ClientService.Debug` and `ClientService.Suspend` now reject missing or
    unknown generated enum values instead of treating proto3 defaults as YAML
    or user-requested suspend. Public clients already send explicit values.
  - Release daemon startup via `--config-from-stdin` no longer rejects the
    parent process's serialized `state_path` / `randomise_link_name`. These
    config fields are now deserializable in release, and release-mode config
    round-trip was verified with a lib test.
  - Non-embedded `Server::run` now owns local background cloud/update tasks
    and aborts them during shutdown instead of detaching reconnection/update
    loops past the server lifecycle.
  - Inbound post-handshake `HostUp` for the local host id is now a protocol
    error, preventing transitive routing events from inserting the local host
    into the remote route table.
  - Connector-side `HelloAccepted` validation now decodes and checks the
    accepted host before reserving the assigned link, so malformed accepted
    hosts cannot leak link reservations.
  - `ClientService` remote inventory now inserts the host into `hosts_model`
    before spawning the remote `SubscribeAgentEvents` task, removing the
    spawn-before-registration race where the task could exit immediately.
  - `amux-ui` session registry entries are removed when session streams end
    naturally or with errors, and attach ignores already-finished stale tasks.
    The ignored embedded UI runtime test now passes when included; notification
    ordering in that test was made race-tolerant for attach/send flows.
  - `amux list` keeps the existing local single-host output, but shows short
    agent ids for ambiguous names and short host ids when inventory spans
    multiple hosts, so aggregate remote inventory is distinguishable.
  - Local deployment notes were checked against removed config keys
    (`user_id`, `max_replay_buffer`, `websocket_port`).
  - Still required before `V1` is marked `done`: add or explicitly replace
    remote CLI/e2e coverage for cloud/routing remote list/attach/session-ended
    flows. Current service/graph tests cover the generated
    `RoutingService.Connect`/tunnel path, but the scripted e2e suite remains
    local-only after deleting the old direct-TCP `server connect` tests.
  - Verification after correctness round 1:
    `cargo check --workspace --all-targets` passed; focused routing,
    ClientService, config, protocol wire, public client encode, amux-ui, and
    e2e-runner tests passed; `cargo test -p amux-ui -- --include-ignored`
    passed; `cargo test --release -p amux --lib
    config::tests::yaml_windows_path_roundtrip -- --nocapture` passed;
    `cargo test --workspace --lib` passed (`273` amux lib tests + `1`
    amux-ui lib test); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `9` current e2e tests;
    `git diff --check` passed.
- Correctness review round 2 findings and fixes:
  - Direct-peer host-id collision handling now treats `RoutingCore::apply_host_up`
    as the authoritative insertion boundary. A collision detected at insert
    time returns a protocol error instead of letting the second link establish,
    and the regression exercises that insertion path directly.
  - Routing handshakes now apply production semantic validation to remote
    `Host` values after protobuf decoding, including the supported-agent-type
    cap. Malformed but structurally decodable hosts are rejected before route
    storage or link ownership can leak.
  - Cloud connector tasks are now owned by their parent cloud loop while a
    connection attempt is active; aborting the server/cloud task aborts the
    in-flight connector instead of detaching it.
  - `amux list` now prints full agent UUIDs for ambiguous agent names so the
    identifier shown by the command is accepted by `amux attach`. Multi-host
    inventory still labels host ids separately.
  - `amux-ui` now stages remote agent inventory events whose host snapshot has
    not arrived yet, and emits the initial connected/snapshot state only after
    the host and agent streams are coherent.
  - Remote agent subscription stream errors no longer synthesize `AgentDown`
    or purge cached agents. `HostRemoved` is the authoritative C-9 cleanup
    signal; stream errors log and retry while the host remains reachable.
  - Local deployment notes were checked for the current cloud relay shape:
    explicit `enforce_tls_in_cloud_mode: false` in the local relay sample,
    no stale WebSocket/nginx guidance, and current Linux/macOS/Windows release
    artifact examples.
  - Still required before the goal is marked `complete`: complete the deferred
    simplification slice recorded in simplification round 2, and add or
    explicitly replace remote CLI/e2e coverage for cloud/routing remote
    list/attach/session-ended flows. Passing unit/graph tests are not enough to
    call that end-to-end surface done.
  - Verification after correctness round 2:
    `cargo check --workspace --all-targets` passed; focused routing,
    ClientService, amux-ui inventory/runtime, amux-cli, and release `amux`
    checks passed; `cargo test --workspace --lib` passed (`275` amux lib tests
    + `2` amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `9` current e2e tests;
    `git diff --check` passed.
- Simplification review round 3 findings and fixes:
  - Completed the deferred simplification slice from round 2 before treating
    `V1` as finishable.
  - Local agent state moved out of `ServerUserState` and into
    `AgentServiceState`. Server/user state now owns service state instead of
    duplicating agent/runtime maps; admin, hook, resume, notify, and legacy
    server-routing helpers work through that service-owned state.
  - `ClientService` is now fully wired by construction. Its constructor
    requires local agent/admin/hook contexts and the remote tunnel pool; the
    old optional-dependency branches and "not wired" errors are gone.
  - Removed the duplicate `AgentRecord` layer from wire conversion. Canonical
    `protocol::Agent` is now the domain type used by agent/client/event wire
    adapters.
  - Renamed transitional generated-service wrapper names now that generated
    tonic is the production path: `ServiceGraph`, `CloudRoutingService`,
    `ClientServiceRpc`, `ClientServiceResponseStream`, and
    `Client::from_client_service_channel` replaced `Generated*`/`new_generated`
    names in production and tests.
  - Added cloud-routed remote CLI/e2e coverage to replace the deleted direct
    TCP remote tests. The e2e runner can now create a local fake cloud API,
    issue refresh/connect responses, serve JWKS, allocate the relay TCP port,
    and inject cloud-enabled config/auth files. `remote_cloud_agent_flow`
    starts a cloud relay plus two local daemons and verifies remote `amux list`,
    remote `amux attach`, remote input, and `[session ended]`.
  - Debug/test builds can connect to a plaintext cloud-routing listener when
    the configured cloud API URL is `http://...`; release builds still require
    HTTPS config and use the TLS channel. This keeps the scripted local cloud
    fixture lightweight without reintroducing production unauthenticated
    routing listeners.
  - Verification after simplification round 3:
    `cargo fmt` ran with the existing stable-rustfmt warnings for nightly-only
    import options; `cargo check --workspace --all-targets` passed;
    `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `10` current e2e tests;
    `git diff --check` passed.
- Direct-connect correction and startup simplification slice landed:
  - Restored the direct `amux server connect <host:port>` primitive. A
    non-cloud daemon with `tcp_port` configured now serves a plain generated
    `RoutingService.Connect` listener again, and
    `ClientService.ConnectToServer` opens an operator-requested direct host
    link through the local daemon. Cloud relay listeners remain authenticated;
    cloud relay daemons reject manual direct links.
  - Restored the four direct remote e2e tests that existed before the cloud-only
    consolidation: `remote_connection`, `remote_list_agents`,
    `remote_attach_by_alias`, and `remote_agent_ended`. The scripted e2e suite
    is back to 13 tests, including `server_suspend_notification`.
  - Collapsed the concrete `ServiceGraph` concept into startup wiring:
    `services/startup.rs` exposes `start_user_services(...)` and the returned
    `StartedUserServices` lifetime handle. This keeps the mundane wiring in one
    startup function: build RoutingService/AgentService/ClientService, build the
    tunnel pool, attach subscriptions before serving, serve AgentService over
    incoming tunnels, expose ClientService locally, and provide routing
    connector context for cloud/direct links.
  - Kept `EmbeddedServerGuard` because it is a small RAII lifetime owner for
    embedded server tasks and the started-services handle, not a domain layer.
  - Removed docs that implied direct host links were forbidden. The current
    architecture notes now distinguish direct non-cloud host links from
    authenticated cloud relay links.
  - Source scans after the slice show no production `ServiceGraph` or
    `RoutingService.SubscribeRoutingEvents` RPC surface; remaining hits for
    older names are historical ledger/protocol notes.
  - Verification after this slice:
    `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests.
- Cloud routing auth interceptor slice landed:
  - Moved initial cloud-routing JWT validation out of the
    `CloudRoutingService` business handler. Cloud routing listeners now wrap the
    generated tonic `RoutingServiceServer` with a `RoutingAuthInterceptor`
    tower service that runs before tonic dispatch reads the request stream.
  - The interceptor validates gRPC metadata through the existing cloud routing
    authenticator, stores `AuthenticatedRoutingUser` in request extensions, and
    turns auth failures into `UNAUTHENTICATED` gRPC responses. The
    `CloudRoutingService.Connect` handler now rejects requests without
    extension claims and uses those claims to select per-user services and
    configure reauth.
  - Direct non-cloud `RoutingService.Connect` listeners are deliberately not
    wrapped by this cloud auth interceptor.
  - Source scan evidence: no `authenticate(request.metadata())` call remains in
    the cloud routing handler path.
  - Verification after this slice:
    `cargo test -p amux services::startup::tests::cloud_routing_service_`
    passed; `cargo check --workspace --all-targets` passed;
    `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
- Old `server/routing` namespace cleanup landed:
  - Removed the misleading `server/routing` module. The remaining live code
    there was local agent lifecycle/name bookkeeping, not routing, so it now
    lives in `services/agent/lifecycle.rs` alongside `AgentService` state and
    event projection.
  - Moved `TopologyEvent`, local lifecycle create/delete/withdraw/resume,
    shutdown/suspend helpers, hook/name-change event broadcasting, and local
    rename candidate handling out of `server::routing`. Runtime/session-event
    handling and hook/admin service code now call the service-owned helpers.
  - Removed the now-empty `server/routing.rs`,
    `server/routing/{agents,naming,peers,topology}.rs` files. Source scans show
    no `server::routing`, `crate::server::routing`, or `super::routing`
    references remain; remaining `server/routing` hits are historical notes.
  - Remaining §10 layout work before completion audit at this point:
    `server/` still contains daemon implementation files, `protocol/wire/` is
    still a central conversion area, and the agent implementation directory is
    still singular `agent/`.
  - Verification after this slice:
    focused `services::agent::lifecycle`, `services::agent::tests`, and
    `services::hook` tests passed; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
- Agent implementation directory rename landed:
  - Renamed the root runtime-agent implementation module from singular
    `agent` to plural `agents`, matching §10 and the updated Rust layout
    guidance: `crates/amux/src/agent.rs` is now
    `crates/amux/src/agents/mod.rs`, and the old `crates/amux/src/agent/`
    tree is now `crates/amux/src/agents/`.
  - Updated root-module imports from `crate::agent` to `crate::agents` while
    preserving the distinct `services::agent` and `protocol::agent` modules.
    Public re-exports such as `amux::Agent` and `amux::claude_io::*` are
    unchanged.
  - Remaining §10 layout work before completion audit: `server/` still contains
    daemon implementation files and `protocol/wire/` is still a central
    conversion area.
  - Verification after this slice:
    `cargo check -p amux --lib` passed; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
- Auth file-structure slice landed:
  - Moved the root auth module to `auth/mod.rs` and split focused auth helpers
    into `auth/credentials.rs`, `auth/claims.rs`, and `auth/oauth.rs`.
    `ConnectionClaims` now lives with claims, the credential-provider trait and
    token/error types live with credentials, and OAuth device/refresh mechanics
    live in the library rather than in the CLI binary.
  - Kept CLI-specific auth.yaml persistence, terminal prompting, and
    `DeviceFlowProvider` wiring in `crates/amux-cli/src/auth.rs`. The CLI now
    depends on the library OAuth helpers and no longer needs direct `oauth2` or
    `chrono` dependencies.
  - Remaining §10 layout work before completion audit: `server/` still contains
    daemon implementation files and `protocol/wire/` is still a central
    conversion area.
  - Verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options.
- Client module simplification slice landed:
  - Removed `crates/amux/src/client/rpc.rs`. The code there was no longer a
    custom RPC layer; it was the public generated-`ClientService` wrapper, so it
    now lives directly in `crates/amux/src/client.rs` as the library client API.
  - Kept `crates/amux/src/client/connect.rs` as the focused local
    channel-opening helper. Source scans show no `client::rpc`, `mod rpc`, or
    `pub use rpc` references remain; historical notes still mention the old
    file name.
  - Verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux client:: -- --nocapture` passed (`23` tests);
    `cargo test -p amux started_services_public_client_wrapper_uses_in_process_channel -- --nocapture`
    passed; `cargo test -p amux public_client_preserves_first_session_closed_event -- --nocapture`
    passed; `cargo fmt --all` ran with the existing stable-rustfmt warnings
    for nightly-only import options; `cargo test --workspace --lib` passed
    (`274` amux lib tests + `2` amux-ui lib tests);
    `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
- Server namespace cleanup slice landed:
  - Removed the remaining live `crates/amux/src/server/` directory. The daemon
    entrypoint/runtime is now the single top-level `server.rs` module again.
    The two tiny runtime submodules for session-event handling and local-client
    shutdown notification were inlined into `server.rs`.
  - Moved local host construction and remote-host validation from
    `server/host.rs` to `routing/host.rs`; routing and services now call the
    routing-owned helpers directly.
  - Moved shared daemon/user state from `server/state.rs` to top-level
    `user_state.rs`, matching the spec's per-user container direction and
    removing the need for server-private state re-exports.
  - Moved server debug rendering from `server/debug.rs` to `debug/server.rs`
    under the existing debug module, and moved cloud reconnect/backoff from
    `server/cloud.rs` to `services/startup/cloud.rs` with the rest of startup
    wiring.
  - Source scans show no current `server::cloud`, `server::debug`,
    `server::host`, `server::state`, `server::runtime`, or
    `crate::server::{ServerState, ShutdownRequest, ...}` references. Historical
    ledger notes still mention the old paths.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux routing::host -- --nocapture` passed (`3` tests);
    `cargo test -p amux services::startup::cloud::tests -- --nocapture`
    passed (`6` tests);
    `cargo test -p amux server::tests -- --nocapture` passed (`1` test);
    `cargo test -p amux admin_service_debug_returns_dump -- --nocapture`
    passed; `cargo test -p amux started_services_seeds_client_and_attaches_startup_events -- --nocapture`
    passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
- Protocol wire cleanup slice landed:
  - Removed the remaining `crates/amux/src/protocol/wire/` directory. The
    generated protobuf include now lives in `protocol/proto.rs`, and central
    protocol-error encode/decode lives in `protocol/error.rs`.
  - Moved AgentService/session request and response conversions from
    `protocol/wire/agent_rpc.rs` to `protocol/agent/wire.rs`, next to the
    `Agent` domain DTO and agent RPC request shapes.
  - Moved host and agent-event conversions from `protocol/wire/runtime.rs` to
    `protocol/message/wire.rs`, next to `Host`, `AgentEvent`, and `HostEvent`.
  - Kept `protocol::wire` as a small compatibility facade over generated proto
    types and colocated conversion functions so service call sites did not
    churn in the same slice. Source scans show no current
    `protocol/wire/agent_rpc.rs`, `protocol/wire/runtime.rs`, or
    `protocol/wire/` directory; remaining hits are historical ledger/spec text.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux protocol::proto::tests -- --nocapture` passed (`2`
    tests); `cargo test -p amux protocol::error::tests -- --nocapture` passed
    (`2` tests);
    `cargo test -p amux protocol::agent::wire::tests -- --nocapture` passed
    (`11` tests);
    `cargo test -p amux protocol::message::wire::tests -- --nocapture` passed
    (`1` test); `cargo test -p amux routing::wire::tests -- --nocapture`
    passed (`7` tests);
    `cargo test -p amux services::agent::tests -- --nocapture` passed (`7`
    tests); `cargo test -p amux protocol::session::tests -- --nocapture`
    passed; `cargo test -p amux services::client::tests::tonic_client_service_dispatches_remote_agent_methods_over_tunnel -- --nocapture`
    passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Routing and agent domain layout cleanup slice landed:
  - Moved `Link`, `InvalidLinkName`, `Route`, and `generate_server_link` out of
    `protocol/` and into `routing/{types,route}.rs`. Routing link/route types
    are now owned by the routing domain instead of by the protocol facade.
  - Moved the replay buffer implementation from top-level `buffer.rs` into
    `agents/buffer.rs` and re-exported only the agent-runtime types needed by
    session and log-source code.
  - Removed the old `protocol::link`, `protocol::route`, and `crate::buffer`
    imports from production code. Source now imports routing primitives from
    `crate::routing::{Link, Route, generate_server_link}` and replay-buffer
    primitives from `crate::agents`.
  - Focused verification before the full pass:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux routing::types -- --nocapture` passed (`6` tests);
    `cargo test -p amux routing::route -- --nocapture` passed (`26` tests);
    `cargo test -p amux routing::core::tests -- --nocapture` passed (`5`
    tests); `cargo test -p amux routing::wire::tests -- --nocapture` passed
    (`7` tests); `cargo test -p amux tunnel:: -- --nocapture` passed (`17`
    tests); `cargo test -p amux services::routing::tests -- --nocapture`
    passed (`28` tests); `cargo test -p amux agents::buffer::tests -- --nocapture`
    passed (`28` tests);
    `cargo test -p amux services::agent::session_rpc::tests -- --nocapture`
    passed (`2` tests); `cargo test -p amux agents::log_source::tests -- --nocapture`
    passed (`5` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Routing host domain layout cleanup slice landed:
  - Moved `Host`, `Capabilities`, `SupportedAgentType`, and `HostEvent` out of
    `protocol::message` and into `routing::{types,events}`. Public API
    re-exports now come from `routing` via `lib.rs` rather than from
    `protocol`.
  - Moved `AGENT_TYPE_CLAUDE` and the debug/test `AGENT_TYPE_TEST_AGENT`
    constants out of `protocol::message` and into `agents/types.rs`, since the
    protocol layer should not own agent-domain vocabulary.
  - Moved host/capabilities protobuf conversions next to the routing host types
    and updated services, tunnels, routing wire conversion, and the public
    client host stream decoder to call the routing-owned helpers directly.
  - Source scans show no current `protocol::message::Host`,
    `protocol::message::Capabilities`, `protocol::message::SupportedAgentType`,
    `protocol::message::AGENT_TYPE_*`, `crate::protocol::Host`, or
    `crate::protocol::HostEvent` source references.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux routing:: -- --nocapture` passed (`87` tests);
    `cargo test -p amux services::client::tests -- --nocapture` passed (`22`
    tests); `cargo test -p amux tunnel::pool::tests -- --nocapture` passed
    (`13` tests); `cargo test -p amux client::tests -- --nocapture` passed
    (`23` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Agent event domain layout cleanup slice landed:
  - Moved `AgentEvent` and AgentService event protobuf conversions from
    `protocol/message/{routing,wire}.rs` into `agents/events.rs`, next to the
    agent-domain event type.
  - Deleted `protocol/message/routing.rs` and `protocol/message/wire.rs`.
    `protocol/message.rs` now only fronts the remaining common request/error
    DTOs that still need a later domain pass.
  - Updated AgentService, ClientService, public client streams, startup tests,
    and the public crate re-export to use `agents::AgentEvent` directly.
  - Source scans show no current `protocol::AgentEvent`,
    `protocol::message::AgentEvent`, `protocol::message::Host`,
    `protocol::message::Capabilities`, `protocol::message::SupportedAgentType`,
    or `protocol::message::AGENT_TYPE_*` source references.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux agents::events -- --nocapture` passed (`1` test);
    `cargo test -p amux services::agent::tests -- --nocapture` passed (`7`
    tests); `cargo test -p amux services::client::tests -- --nocapture`
    passed (`22` tests); `cargo test -p amux client::tests -- --nocapture`
    passed (`23` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Public session/client-method layout cleanup slice landed:
  - Moved public `SubscribeSessionEvent` and public `SessionCloseReason` from
    `protocol/session.rs` into `agents/session_events.rs`, and updated the
    public crate re-export plus client/startup code to use the agent-owned
    types.
  - Moved generated ClientService method-name constants out of
    `protocol/method.rs` and into a private `client.rs` module. These are
    public-client decode/error labels, not protocol-domain model types.
  - Deleted `protocol/session.rs` and `protocol/method.rs`; source scans show
    no current `protocol::session` or `protocol::method` references.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux agents::session_events -- --nocapture` passed (`1`
    test); `cargo test -p amux client::tests -- --nocapture` passed (`23`
    tests);
    `cargo test -p amux services::startup::tests::public_client_preserves_first_session_closed_event -- --nocapture`
    passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Agent DTO/wire ownership cleanup slice landed:
  - Moved the public client-visible `Agent` DTO from `protocol/agent.rs` into
    `agents/types.rs`, next to the other agent-domain public types.
  - Moved AgentService protobuf conversion helpers from
    `protocol/agent/wire.rs` into `agents/wire.rs`; `protocol::wire` now only
    fronts generated proto/error plumbing and no longer re-exports agent
    conversion helpers.
  - Renamed runtime/session-local agent metadata from `Agent` to
    `AgentRecord` so code paths that need route/local-session details do not
    masquerade as the public DTO.
  - Deleted `protocol/agent.rs` and `protocol/agent/wire.rs`. At this point
    the `protocol/` tree still contained the `message.rs` common-type bag;
    that was removed in the following common DTO cleanup slice.
  - Focused verification after this slice:
    `cargo test -p amux agents::wire -- --nocapture` passed (`11` tests);
    `cargo test -p amux services::agent::tests -- --nocapture` passed (`7`
    tests); `cargo test -p amux services::client::tests -- --nocapture`
    passed (`22` tests);
    `cargo test -p amux admin_service_debug_returns_dump -- --nocapture`
    passed.
  - Full verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Protocol common DTO cleanup slice landed:
  - Deleted the remaining `protocol/message.rs` and
    `protocol/message/common.rs` catch-all. At this point `protocol/`
    contained `error.rs`, `proto.rs`, and `wire.rs`; the next slice collapsed
    that filesystem layout to `mod.rs` plus `error.rs`.
  - Moved `AgentType`, `CreateAgentRequest`, `RenameAgentRequest`, and
    `TerminalSize` into `agents/types.rs`; moved `SequencedReplayQuery` into
    `agents/buffer.rs`.
  - Moved `DebugFormat` into `debug.rs` and shutdown notification metadata
    (`ShutdownReason` plus `SHUTDOWN_REASON_METADATA_KEY`) into `server.rs`.
  - Moved the `ProtocolError` enum into `protocol/error.rs`, next to its
    generated-protobuf encode/decode helpers. This is the only remaining
    public type exported from `protocol`.
  - Source scans show no current `protocol::message` source references and no
    domain DTOs re-exported through `protocol`.
  - Verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Protocol two-file layout cleanup slice landed:
  - Moved `crates/amux/src/protocol.rs` to `protocol/mod.rs`, inlined the
    generated `tonic::include_proto!` facade there, and deleted
    `protocol/proto.rs` and `protocol/wire.rs`.
  - Kept `protocol::wire` as a small inline facade for generated protobuf
    types and central error encode/decode helpers. The filesystem layout now
    matches §10's two-file `protocol/` target: `mod.rs` plus `error.rs`.
  - Source scans show no current `protocol::proto`, `super::proto`,
    `protocol/proto`, or `protocol/wire.rs` source references.
  - Verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Module `mod.rs` layout cleanup slice landed:
  - Normalized modules that also own submodules to the updated Rust guide:
    `client/mod.rs`, `debug/mod.rs`, `services/mod.rs`,
    `services/agent/mod.rs`, `services/startup/mod.rs`,
    `transport/mod.rs`, `sleep_inhibitor/mod.rs`, `agents/claude/mod.rs`,
    and `agents/claude/session/mod.rs`.
  - Left top-level `server.rs` in place because §10 names it as the daemon
    entrypoint and no `server/` directory remains.
  - Source scans show no remaining `foo.rs` plus sibling `foo/` module pairs
    under `crates/amux/src`.
  - Verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`274` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Post-review correctness slice landed:
  - Removed the tunnel-layer synthetic `HostUp` behavior. Routing events now
    leave through the routing service mutation paths, using registered
    per-link peer host IDs plus `should_send_routing_event_to_link(...)` for
    hop-local filtering. This preserves T-9 ordering without leaking routing
    semantics into `TunnelPool`.
  - Added coverage that a real `HostUp` is sent before a later peer tunnel
    frame and that forwarded tunnel frames do not synthesize routing events.
  - Changed `TunnelPool::handle_inbound_frame(...)` to clone senders under its
    lock and perform channel sends after dropping the lock, avoiding lock-held
    awaits under backpressure.
  - Hardened remote input validation: inbound `HostUp` must pass semantic
    host validation, and remote `AgentService` events whose host IDs do not
    match the subscribed host are ignored.
  - Split suspend into prepare/save/commit. Suspended state is built before
    agents are stopped, persistence happens before withdrawal, and failed
    resume records are written back instead of being deleted before failures
    are known.
  - Moved daemon shutdown/suspend success replies until after cloud/background
    tasks are stopped and the local socket path is removed. Embedded shutdown
    still replies before delayed task abort so the in-process RPC response can
    be delivered.
  - Removed the unused public `Client::shutdown(reason)` parameter; shutdown
    currently carries no protocol-level reason and means user-requested
    shutdown.
  - Focused verification after this slice:
    `cargo test -p amux services::routing::tests -- --nocapture` passed (`29`
    tests); `cargo test -p amux tunnel:: -- --nocapture` passed (`17` tests);
    `cargo test -p amux services::client::tests -- --nocapture` passed (`22`
    tests); lifecycle/admin/suspend focused tests passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`280` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.

- Simplification and second review correction slice landed:
  - Removed stale route semantics from host-local `AgentRecord`; route state is
    now only in routing/tunnel code. Debug serialization now reports local
    agent metadata without fake remote route fields.
  - Collapsed internal `SessionOutputEvent` into the public
    `SubscribeSessionEvent`, removing a duplicate session-output enum and the
    conversion shim between identical event shapes.
  - Collapsed duplicated `AgentEvent::AgentUp` / `AgentEvent::AgentUpdated`
    field sets into `AgentEvent::{AgentUp, AgentUpdated} { agent: Agent }`.
    AgentService, ClientService, the public client stream decoder, and
    amux-ui inventory now all carry the single public `Agent` DTO instead of
    reconstructing identical shapes at each boundary.
  - Removed the zero-sized `AgentService`, `AdminService`, and `HookService`
    namespace structs. The behavior now lives directly on `AgentServiceCtx`,
    `AdminServiceCtx`, and `HookServiceCtx`, which are the actual stateful
    service objects used by the tonic shims and local ClientService dispatch.
  - Centralized route-to-protobuf conversion under `routing::wire` and reused
    it from tunnel frame code.
  - Fixed T-10 cached-channel behavior: `TunnelPool::channel_to(...)` now
    checks the current `RoutingCore` host entry before returning a cached
    channel, so removed hosts synchronously return `NOT_FOUND` even before the
    async tunnel cleanup task runs.
  - Fixed snapshot/delta ordering on newly established links. Link writers now
    start in a snapshotting state; live routing deltas buffer until the initial
    snapshot plus `SnapshotComplete` have been sent, then buffered deltas drain
    before the link becomes live. Duplicate `HostUp`s already covered by the
    snapshot are suppressed while preserving later `HostDown`s.
  - Hardened cloud routing JWT validation by requiring the `aud` claim as well
    as validating it against `amux_token`.
  - Added typed `GoAway` sends to established routing links during
    shutdown/suspend with a server drain budget.
  - Implemented receiver-side `GoAway` drain posture. Inbound `GoAway` now
    marks the link draining, stops new outbound `TunnelPool::channel_to(...)`
    creation through that link, keeps inbound/in-flight tunnel frames flowing
    until the drain deadline, then runs the normal route cleanup and host
    removal fan-out.
  - Focused verification after this slice:
    `cargo test -p amux auth::jwt::tests -- --nocapture` passed;
    `cargo test -p amux tunnel::pool::tests -- --nocapture` passed (`15`
    tests); `cargo test -p amux agents::session_events -- --nocapture`
    passed; `cargo test -p amux agents::wire::tests::session_output_events_roundtrip -- --nocapture`
    passed; `cargo test -p amux services::routing::tests -- --nocapture`
    passed (`30` tests); typed-GoAway focused tests passed;
    `cargo test -p amux draining -- --nocapture` passed (`3` focused tests);
    `cargo test -p amux inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing -- --nocapture`
    passed; `cargo test -p amux services::client::tests -- --nocapture`
    passed (`24` tests); `cargo test -p amux services::agent::tests -- --nocapture`
    passed (`7` tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests); `cargo test -p amux-ui inventory::tests -- --nocapture`
    passed; after the service-context collapse,
    `cargo test -p amux services::agent -- --nocapture` passed (`11` tests);
    `cargo test -p amux services::admin -- --nocapture` passed (`6` tests);
    `cargo test -p amux services::hook -- --nocapture` passed;
    `cargo test -p amux services::client::tests -- --nocapture` passed
    (`24` tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --lib` passed (`289` amux lib tests + `2`
    amux-ui lib tests); `cargo test --workspace --all-targets` passed;
    `cargo run -p e2e-runner -- run` passed all `13` current e2e tests;
    `git diff --check` passed.
  - Still pending from the latest spec/correctness review: moving the routing
    link runtime out of the service shim.
- Cloud-relay e2e coverage slice landed:
  - Added `e2e-tests/cloud_relay_connection.test`. The test starts a cloud
    relay daemon, starts two cloud-enabled local daemons backed by the e2e
    runner's fake cloud API/JWKS/token fixture, waits for routing convergence,
    verifies remote `amux list`, attaches to the remote agent through the
    relay, and sends input from the remote terminal back to the original
    session.
  - This coverage is additive. The restored direct-connect e2e tests remain in
    place, so the scripted suite now has 14 tests: the original local/direct
    coverage plus one cloud-relay remote flow.
  - Simplification review at this logical boundary identified the next cleanup
    queue before finish: collapse the private Admin/Hook context indirection
    into concrete client/startup runtime wiring, remove the `TopologyEvent`
    mirror in favor of direct `AgentEvent` emission, collapse the remaining
    public `ClientServiceRpc` delegation layer into `Client`, consider using
    public host/agent event shapes inside `ClientService`, and remove
    transitional "generated/new architecture" wording from live surfaces.
  - Verification after this slice:
    `cargo run -p e2e-runner -- run cloud_relay_connection` passed;
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `cargo check --workspace --all-targets` passed;
    `cargo test --workspace --all-targets` passed (`289` amux lib tests, `9`
    embedded tests, `26` amux-cli tests, `2` amux-ui lib tests, `9`
    e2e-runner tests, ignored runtime integration unchanged);
    `git diff --check` passed.
- Simplification slice after cloud e2e landed:
  - Removed the `TopologyEvent` mirror and the stale
    `broadcast_topology_event(..., _exclude_link)` helper. Local agent
    lifecycle, resume, suspend, rename, and external-hook bootstrap now emit
    concrete `AgentEvent`s directly while committing local state changes.
  - Collapsed the public-client `ClientServiceRpc` wrapper into `Client`.
    `Client` now owns the generated tonic `ClientServiceClient` channel and
    the closed-state flag directly instead of forwarding every public method
    through a private delegation layer.
  - Replaced duplicate `ClientHostEvent` / `ClientAgentEvent` model events
    inside `ClientService` with the public `HostEvent` / `AgentEvent` shapes.
    Snapshot markers remain stream-boundary concerns.
  - Cleaned transitional "generated" wording from live comments, logs, and
    user-facing errors where it referred to the migration rather than actual
    generated protobuf code.
  - Remaining simplification/correctness work before completion: collapse the
    private Admin/Hook context indirection into concrete client/startup runtime
    wiring if it still carries no real boundary, and move the routing link
    runtime out of the service shim.
  - Focused verification after this slice:
    `cargo test -p amux services::agent -- --nocapture` passed (`11` tests);
    `cargo test -p amux services::client::tests -- --nocapture` passed (`24`
    tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests);
    `cargo test -p amux connector_sends_reauth_before_token_expiry -- --nocapture`
    passed; `cargo test -p amux-ui inventory::tests -- --nocapture` passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`289` amux lib
    tests, `9` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests, `9`
    e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Admin/Hook context collapse landed:
  - Deleted `services/admin.rs` and `services/hook.rs`. These were private
    pseudo-service contexts with no generated service boundary; their behavior
    now lives directly on `ClientService`, which is the actual client-facing
    service.
  - `ClientService` now receives startup wiring explicitly: local
    `AgentServiceCtx`, server state, routing core, tunnel pool, and connector
    task registry. Debug, shutdown, suspend, resume, direct connect, and hook
    handling use those concrete dependencies directly.
  - Preserved the failed-resume persistence coverage by moving that assertion
    to a ClientService test. The amux lib test count dropped by six because the
    removed Admin/Hook modules and their standalone unit tests are gone.
  - Focused verification after this slice:
    `cargo check --workspace --all-targets` passed;
    `cargo test -p amux services::client::tests -- --nocapture` passed (`25`
    tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests);
    `cargo run -p e2e-runner -- run remote_connection` passed;
    `cargo run -p e2e-runner -- run cloud_relay_connection` passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`283` amux lib
    tests, `9` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests, `9`
    e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Routing link runtime simplification landed:
  - Moved the `RoutingService.Connect` acceptor/connector runtime out of the
    `services` namespace and into `routing/connect/mod.rs`. The services
    namespace now only contains the client/agent/startup service wiring; routing
    link establishment, route import/export, tunnel-frame dispatch, reauth, and
    cleanup live with routing.
  - Moved `protocol_status` into `protocol/error.rs`, so routing runtime no
    longer depends back on service helpers for protocol-to-tonic error mapping.
  - Split routing link writer/fanout state out of `TunnelPool` into
    `routing/link_registry.rs`. `TunnelPool` now owns tunnel objects, cached
    host channels, and inbound tunnel delivery; the link registry owns outgoing
    `RoutingService.Connect` writers, snapshot buffering, draining state,
    routing-event fanout, and shutdown `GoAway` sends.
  - Removed the cloud routing token-auth adapter. The cloud routing
    authenticator now implements the routing token-auth trait directly.
  - Replaced the fixed sleep in `cloud_relay_connection` with a bounded
    e2e-runner retry directive that reruns the last one-shot command until the
    expected output appears. This addresses the cloud relay readiness race
    without weakening the assertion.
  - Focused verification after this slice:
    `cargo check -p amux --all-targets` passed;
    `cargo test -p e2e-runner parser::tests::test_parse_retry_next_expect -- --nocapture`
    passed; `cargo test -p amux routing::connect::tests -- --nocapture`
    passed (`31` tests); `cargo test -p amux tunnel::pool::tests -- --nocapture`
    passed (`19` tests); `cargo test -p amux services::client::tests -- --nocapture`
    passed (`25` tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests); `cargo run -p e2e-runner -- run cloud_relay_connection`
    passed; `cargo check --workspace --all-targets` passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo test --workspace --all-targets` passed
    (`283` amux lib tests, `9` embedded tests, `26` amux-cli tests, `2`
    amux-ui lib tests, `10` e2e-runner tests, ignored runtime integration
    unchanged); `cargo run -p e2e-runner -- run` passed all `14` current e2e
    tests; `git diff --check` passed.
- Post-review alignment cleanup landed:
  - Collapsed the former concrete service-graph concept into startup wiring.
    `start_user_services` is the composition point for RoutingService,
    AgentService, ClientService, tunnels, remote-agent subscriptions, and
    local/in-process client channels; `EmbeddedServerGuard` remains only as the
    RAII owner for embedded server lifetime.
  - Routing-event propagation now comes from `RoutingCore` subscriptions.
    `spawn_routing_event_fanout` bridges core routing events into the link
    registry, so the Connect runtime no longer manually broadcasts at individual
    mutation sites.
  - Session lifecycle events are now owned by `AgentService`. `Server` no
    longer carries a process-wide session event bus, and `SessionEvent` no
    longer contains user identity metadata; per-user AgentService instances
    consume their own session events.
  - Removed the public `Client::resolve_agent` API and changed CLI attach/input
    to pass `AgentRef` through `ClientService`, keeping name/id resolution on
    the client service boundary rather than leaking the old resolve-then-call
    flow back into callers.
  - Fixed the test-only echo agent lifetime so its synthetic exit handle does
    not immediately trigger AgentService's session-ended cleanup.
  - Focused verification after this slice:
    `cargo check -p amux --all-targets` passed;
    `cargo test -p amux routing::connect::tests -- --nocapture` passed (`31`
    tests); `cargo test -p amux services::agent -- --nocapture` passed (`11`
    tests); `cargo test -p amux services::client::tests -- --nocapture` passed
    (`25` tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests); `cargo test -p amux-ui --lib` passed (`2` tests);
    `cargo test -p amux-cli --bin amux` passed (`26` tests);
    `cargo run -p e2e-runner -- run remote_attach_by_alias` passed;
    `cargo run -p e2e-runner -- run cloud_relay_connection` passed.
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`283` amux lib
    tests, `9` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests,
    `10` e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Post-simplification cleanup landed:
  - Removed the public stream `cancel()` methods from `SessionStream`,
    `HostEventStream`, and `AgentEventStream`. Detach/cancel callers now drop
    the stream owner directly, matching tonic stream ownership instead of
    exposing a no-op async cancellation API.
  - Local `ClientService` lifecycle/session dispatch no longer round-trips
    through AgentService protobuf request decoding. Local create, subscribe,
    and send-input paths build domain request structs directly; remote paths
    still encode AgentService wire requests because they cross a tunnel.
  - Removed the mirrored `AgentRecord` from `LocalAgentContext`. Local agent
    metadata now has one source of truth: the session. Snapshots, debug output,
    rename/name-candidate events, and registration events derive records from
    `AgentSession::to_agent(host_id)`.
  - Collapsed duplicated daemon/embedded lifecycle transitions in
    `server.rs`. Cloud/update background task startup now uses a shared helper,
    and shutdown/suspend uses one `process_shutdown_request` path while keeping
    daemon socket/listener lifetime and embedded `EmbeddedServerGuard` lifetime
    distinct.
  - Focused verification after this slice:
    `cargo check -p amux --all-targets` passed;
    `cargo test -p amux services::agent -- --nocapture` passed (`11` tests);
    `cargo test -p amux services::client::tests -- --nocapture` passed (`25`
    tests); `cargo test -p amux-cli --bin amux` passed (`26` tests);
    `cargo test -p amux-ui --lib` passed (`2` tests);
    `cargo test -p amux server::tests -- --nocapture` passed (`2` tests);
    `cargo test -p amux services::startup::tests -- --nocapture` passed (`9`
    tests); `cargo test -p amux --test embedded -- --nocapture` passed (`9`
    tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`283` amux lib
    tests, `9` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests,
    `10` e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Final simplification review slice landed:
  - Removed the unused structured-input cancellation token and the
    `send_structured_input_cancellable` path. Structured input now validates
    sequence and executes bytes/delays directly; real cancellation remains
    stream/task ownership rather than a never-triggered token.
  - Split cloud-authenticated per-user relay state from full user services.
    `CloudRoutingService` now stores routing/tunnel runtime only for each
    authenticated user instead of starting `ClientService`, `AgentService`, and
    local agent subscriptions that a relay user cannot consume.
  - Kept a small incoming-tunnel drain task for routing-only cloud users so an
    accidental tunnel targeted at the relay is discarded without closing the
    routing link. Host users still serve `AgentService` on incoming tunnels.
  - Explicit review decisions:
    embedded clients still go through generated `ClientService` over an
    in-process channel because that preserves one client service contract; a
    direct embedded backend would create a second client execution path. The
    `LinkRegistry` pending/snapshot buffer remains because it is carrying the
    Connect snapshot/live ordering invariant and is covered by unit tests.
  - Focused verification after this slice:
    `cargo check -p amux --all-targets` passed;
    `cargo test -p amux agents::claude::session::tests -- --nocapture` passed
    (`9` tests); `cargo test -p amux services::agent::session_rpc -- --nocapture`
    passed (`2` tests); `cargo test -p amux services::startup::tests -- --nocapture`
    passed (`9` tests); `cargo test -p amux routing::connect::tests -- --nocapture`
    passed (`31` tests); `cargo test -p amux services::client::tests -- --nocapture`
    passed (`25` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`283` amux lib
    tests, `9` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests,
    `10` e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Final review fixes landed:
  - `EmbeddedBuilder::open()` now rejects `as_cloud_relay()` explicitly. Cloud
    relay mode needs a daemon listener; embedded open still returns a local
    `ClientService` client and does not expose a relay RoutingService listener.
  - Direct server-connect task lifetime is owned by `ClientService` through its
    own shared task registry. Startup now owns only startup-spawned runtime
    tasks, not tasks created by client-facing admin methods.
  - Removed the duplicate `CloudRoutingAuthenticator` trait. The cloud
    interceptor extracts bearer metadata and calls the existing
    `RoutingTokenAuthenticator` directly; JWT and test authenticators implement
    one auth trait.
  - Reauth protocol messages are cloud-auth state only. Direct unauthenticated
    links now treat unexpected `Reauth` and `ReauthAck` frames as protocol
    errors instead of accepting/ignoring them.
  - Focused verification after this slice:
    `cargo check -p amux --all-targets` passed;
    `cargo test -p amux --test embedded -- --nocapture` passed (`10` tests);
    `cargo test -p amux services::startup::tests -- --nocapture` passed (`9`
    tests); `cargo test -p amux routing::connect::tests -- --nocapture` passed
    (`33` tests); `cargo test -p amux services::client::tests -- --nocapture`
    passed (`25` tests).
  - Full verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`285` amux lib
    tests, `10` embedded tests, `26` amux-cli tests, `2` amux-ui lib tests,
    `10` e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` current e2e tests;
    `git diff --check` passed.
- Follow-up simplification slice landed:
  - Extracted Unix, TCP, TLS, and in-process tonic transport helpers into
    `transport/`, with a shared `GrpcIo<T>` wrapper for transports that do not
    need connect metadata and a shared one-shot IO-to-Channel connector. Direct
    TCP accepts now apply the same `TCP_NODELAY`/keepalive setup as the cloud
    listener path.
  - Collapsed local `ServerState` user storage down to the local
    `SharedAgentServiceState`. Cloud relay per-user routing/tunnel state
    remains separate in `CloudRoutingService`.
  - Moved foreground server execution to `ServerBuilder::run()`. `EmbeddedBuilder`
    is now only the embedded `open()` path that owns an `EmbeddedServerGuard`.
  - Removed the unimplemented Claude SDK create runtime from proto/domain
    dispatch, then flattened the one-arm Claude create `oneof`; Claude create
    requests now carry the implemented PTY terminal-size field directly.
  - Simplified hooks to the only implemented provider: Claude. The public
    `Client::handle_hook` now takes just the payload and returns `()`;
    `HandleHookRequest` no longer carries a provider enum; Claude's plugin uses
    one `amux hooks claude` command for every hook event; CLI hook target
    parsing is owned by the client/service path instead of duplicated in the
    CLI.
  - Narrowed public `claude_io` exports to the client-facing constants/types and
    encode/decode helpers, made server-side codec helpers `pub(crate)`, removed
    public agent-type constant re-exports, and narrowed Claude submodule
    visibility.
  - Updated `docs/NEW_ARCHITECTURE.md` to distinguish cloud-authenticated
    routing from direct unauthenticated links, reflect the current
    `services/startup/`, `routing/connect/`, and `transport/` layout, and remove
    stale `ServerUserState` routing/tunnel ownership language.
  - Focused verification after this slice:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets` passed;
    `cargo test -p amux services::startup::tests -- --nocapture` passed (`9`
    tests); `cargo test -p amux services::agent -- --nocapture` passed (`11`
    tests); `cargo test -p amux agents:: -- --nocapture` passed (`74` tests);
    `cargo test -p amux services::client::tests -- --nocapture` passed (`25`
    tests); `cargo test -p amux --test embedded -- --nocapture` passed (`10`
    tests); `cargo test -p amux routing::connect::tests -- --nocapture` passed
    (`33` tests); `cargo test -p amux-cli --bin amux` passed (`23` tests).
- Final completion review slice landed:
  - Implemented the deferred simplifications from the final review boundary:
    extracted transport helpers into `transport/{unix,tcp,tls,memory,io,single_io}.rs`,
    collapsed the local user-state abstraction to the single local
    `SharedAgentServiceState`, narrowed the public Claude/io surface, removed
    the unimplemented Claude SDK create runtime, collapsed hook handling to the
    implemented Claude provider, made routing-core snapshot helpers test-only,
    and simplified public client stream wrappers to own tonic streams directly
    with `&mut self` receive semantics.
  - Fixed correctness issues found by review: cloud relay shutdown/suspend
    now sends `GoAway` through per-user routing runtimes and gives it drain
    time; embedded shutdown receivers continue after recoverable suspend
    failures; cloud update-required status is reported for both initial
    handshake rejection and post-handshake `GoAway`; stale update-required CLI
    markers clear once the current binary satisfies the minimum version; remote
    lifecycle responses validate host/agent identity; missing remote route
    targets map to `UNAVAILABLE`; direct listener readiness is established
    before the local client socket; simultaneous direct connect keeps duplicate
    links without replacing the first route.
  - Closed the final backpressure findings: `EventSource` now distinguishes
    critical in-process subscribers from drop-on-overflow network
    subscribers; AgentService/ClientService network event streams return
    `RESOURCE_EXHAUSTED` on subscriber queue closure; session close/shutdown
    side-channel streams use drop-on-overflow; routing link writer overflow
    sends a `LinkCloseReason` to the live `RoutingService.Connect` task so the
    normal route/tunnel/link cleanup path runs; established-link outbound
    error/reauth sends use nonblocking `try_send_outbound` so cleanup cannot
    park behind a full outbound queue.
  - Final subagent review results:
    simplification review found no blocking cleanup after the stream/helper
    cleanup; bug review findings were fixed and rechecked as resolved; final
    spec audit reported no blocking spec deviations.
  - Final verification:
    `cargo fmt --all` ran with the existing stable-rustfmt warnings for
    nightly-only import options; `cargo check --workspace --all-targets`
    passed; `cargo test --workspace --all-targets` passed (`290` amux lib
    tests, `10` embedded tests, `25` amux-cli tests, `2` amux-ui lib tests,
    `10` e2e-runner tests, ignored runtime integration unchanged);
    `cargo run -p e2e-runner -- run` passed all `14` e2e tests;
    `git diff --check` passed.

## Verification Commands

Run and record results here as slices land:

```sh
cargo test -p amux
cargo test --workspace
```
