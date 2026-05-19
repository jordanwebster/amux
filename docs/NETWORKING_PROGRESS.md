# Networking Spec Progress

Objective: implement `docs/NETWORKING.md` as the source of truth, accepting
breaking changes while keeping this file as the work ledger.

## Tracking Model

Each checkpoint is intended to be commit-sized. A checkpoint is `done` only
after implementation, focused verification, and two review rounds:

- simplification review: looks for system-level collapses and unnecessary
  compatibility or abstraction
- bug review: looks for correctness and test gaps, noting deliberate breakage

Status values: `todo`, `in_progress`, `reviewing`, `done`, `blocked`.

## Checkpoints

| ID | Status | Spec | Deliverable | Artifacts | Evidence |
| --- | --- | --- | --- | --- | --- |
| N0 | done | objective | Create this systematic progress ledger. | `docs/NETWORKING_PROGRESS.md` | Ledger created with checkpoint statuses, artifacts, evidence, work log, and review tracking. |
| P4 | done | 5.2.1, 6, 6.3, N-TN-2 | Apply protocol v4 schema break: `TunnelId { initiator, nonce }`, add `PairingService` schema, update descriptor expectations. | `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/protocol/mod.rs`, `crates/amux/src/tunnel/` | `cargo test -p amux tunnel::`; `cargo test -p amux protocol::`; `cargo test -p amux routing::connect::`; `cargo test -p amux routing::link`; `cargo check -p amux`; `git diff --check`; two simplification and two bug-review rounds completed. |
| K1 | done | 4.1, 4.2, 10 N-K-* | Persist device identity and trust store under the specified data-dir layout with atomic writes and file modes. | `crates/amux/src/identity.rs`, `crates/amux/src/setup.rs`, `crates/amux/src/server.rs`, `crates/amux-cli/src/init.rs`, `Cargo.toml`, `crates/amux/Cargo.toml` | `cargo test -p amux identity::`; `cargo test -p amux server::tests::`; `cargo test -p amux-cli init::`; `cargo check --workspace --all-targets`; `git diff --check`; two simplification and two bug-review rounds completed. |
| G1 | done | 4.4, 4.7, 8.1, 8.4 | Introduce daemon TLS identity material and dispatcher topology for Trusted Server vs Pairing Server. | `crates/amux/src/identity.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/pairing.rs`, `crates/amux/src/services/`, `crates/amux/src/transport/io.rs`, `crates/amux/src/tunnel/pool.rs` | `cargo check -p amux --tests`; `cargo test -p amux acceptor_rejects_hello_host_id_that_does_not_match_tls_peer`; `cargo test -p amux connector_rejects_hello_accepted_host_id_that_does_not_match_expected_peer`; `cargo test -p amux dropping_inbound_endpoint_transport_removes_target_tunnel`; `cargo test -p amux outbound_tls_timeout_removes_provisional_tunnel`; `cargo test -p amux identity::`; `cargo test -p amux dispatcher::`; `cargo test -p amux services::startup::`; `cargo test -p amux tunnel::pool::`; `cargo test -p amux server::tests::`; `cargo test -p amux`; `cargo check --workspace --all-targets`; `git diff --check`; two simplification and two bug-review rounds completed. |
| CN1 | done | 4.9, 8.5-8.7 | Replace host-keyed tunnel channel cache with route-keyed `ConnectionPool` and route policy `ConnectionManager`. | `crates/amux/src/connection.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/routing/core.rs`, `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/routing/link_registry.rs`, `crates/amux/src/routing/route.rs` | `cargo check -p amux --tests`; `cargo test -p amux routing::core::`; `cargo test -p amux connection::`; `cargo test -p amux routing::connect::`; `cargo test -p amux routing::link_registry::`; `cargo test -p amux tunnel::pool::`; `cargo test -p amux services::startup::`; `cargo test -p amux services::client::`; `cargo test -p amux`; `cargo check --workspace --all-targets`; `git diff --check`; two simplification and two bug-review rounds completed. |
| PAIR1A | done | 4.2, 5, 10 N-P-3, N-P-5, N-P-6 | Add pairing foundations: live mutable trust-store state, pairing trust upsert semantics, and time-bounded one-secret pair-mode lifecycle. | `crates/amux/src/identity.rs`, `crates/amux/src/pairing.rs`, `crates/amux/src/connection.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/routing/link_registry.rs` | `cargo test -p amux identity::`; `cargo test -p amux pairing::`; `cargo test -p amux connection::`; `cargo test -p amux dispatcher::`; `cargo test -p amux tunnel::pool::`; `cargo test -p amux routing::link_registry::`; `cargo test -p amux routing::connect::`; `cargo test -p amux services::startup::`; `cargo test -p amux`; `cargo check --workspace --all-targets`; `git diff --check`; two simplification and two bug-review rounds completed. |
| PAIR1B | done | 5.1, 6.1-6.2, N-P-1..N-P-3, N-P-6, N-P-9 | Implement QR/token responder service and daemon wiring. | `crates/amux/src/pairing.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/connection.rs`, `crates/amux/src/transport/io.rs`, `crates/amux/src/server.rs` | `cargo test -p amux pairing::`; `cargo test -p amux services::pairing::`; `cargo test -p amux services::startup::`; `cargo test -p amux identity::`; `cargo test -p amux connection::`; `cargo test -p amux transport::io::`; `cargo test -p amux dispatcher::`; `cargo check --workspace --all-targets`; `cargo test -p amux`; `git diff --check`; two review rounds completed. |
| PAIR1C | done | 5.2, 5.2.1, 6.1-6.2, N-P-4 | Implement PIN/SPAKE2 responder service cryptography and attempt limits. | `Cargo.toml`, `crates/amux/Cargo.toml`, `Cargo.lock`, `docs/NETWORKING.md`, `crates/amux/src/pairing.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/transport/io.rs`, `crates/amux/src/identity.rs` | `cargo test -p amux services::pairing::`; `cargo test -p amux dispatcher::`; `cargo test -p amux services::startup::`; `cargo test -p amux identity::`; `cargo test -p amux pairing::`; `cargo check --workspace --all-targets`; `cargo test -p amux`; `git diff --check`; two review rounds completed. |
| PAIR1D | done | 5.3, 5.3.1, 7, N-P-8 | Implement SSH pairing and responder/runtime CLI helpers. | `crates/amux/src/ssh_pairing.rs`, `crates/amux/src/transport/ssh.rs`, `crates/amux/src/transport/io.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/server.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/lib.rs`, `crates/amux/src/transport/mod.rs`, `crates/amux/proto/amux/v1/amux.proto`, `crates/amux-cli/src/main.rs`, `docs/NETWORKING.md` | `cargo test -p amux ssh_pairing::`; `cargo test -p amux services::client::tests::tonic_pair_ssh_peer`; `cargo test -p amux services::pairing::`; `cargo test -p amux services::startup::`; `cargo test -p amux protocol::`; `cargo test -p amux-cli`; `cargo check --workspace --all-targets`; `cargo test -p amux`; `git diff --check`; two simplification and two bug-review rounds completed. |
| PAIR1E | done | 7, N-P-* | Implement user-facing `amux pair` initiator/responder CLI flows. | `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/client/mod.rs`, `crates/amux/src/pin_pairing.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/ssh_pairing.rs`, `crates/amux/src/transport/ssh.rs`, `crates/amux/src/transport/tls.rs`, `crates/amux-cli/src/main.rs`, `docs/NETWORKING.md`, `docs/NETWORKING_PROGRESS.md` | Local pair-mode control RPCs, `amux pair` PIN/QR responder start with terminal QR rendering, `amux pair --via-ssh`, `amux pair --connect <ip:port>` direct PIN initiator pairing, and cloud-name/picker PIN initiator pairing are implemented. `cargo test -p amux`; `cargo test -p amux-cli`; `cargo check --workspace --all-targets`; `git diff --check`; two simplification and two bug-review rounds completed. Direct/SSH/cloud runtime Link establishment remains R1. |
| R1 | done | 4.8, 8.2-8.3, 8.8 | Establish paired direct TCP/SSH/cloud Links from trust reachabilities and propagate routing graph events. | `crates/amux/src/services/reachability.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/services/startup/cloud.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/transport/tls.rs`, `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/server.rs` | DirectTcp startup and pair-time runtime Link establishment are implemented. SSH relay runtime Link integration is verified over the same raw Trusted Server ingress used by `ssh <target> amux relay`; OS-level `ssh` command spawning remains covered by transport argument tests. Cloud Links remain established by the cloud attach flow and cloud reachability requires no peer-direct Link. Evidence: `cargo test -p amux services::reachability:: -- --nocapture`; `cargo test -p amux services::startup::tests::direct_ -- --nocapture`; `cargo test -p amux services::startup::tests::direct_tcp_reachabilities_on_both_peers_establish_two_outbound_links -- --nocapture`; `cargo test -p amux services::startup::tests::ssh_relay_runtime_link_establishes_route_over_trusted_ingress -- --nocapture`; `cargo test -p amux services::client::tests::tonic_pair_ -- --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p amux transport::ssh:: -- --nocapture`; `cargo check -p amux --tests`; `git diff --check`. Two simplification and two bug/security review rounds completed; final focused bug re-review found no blocker. |
| CLI1 | done | 7.1-7.4 | Replace legacy `amux connect` surface with spec pairing commands and implicit routing behavior. | `crates/amux-cli/src/main.rs`, `crates/amux-cli/src/server_client.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/protocol/mod.rs`, `crates/amux/proto/amux/v1/amux.proto`, `crates/e2e-runner/src/`, `e2e-tests/remote_*.test`, `e2e-tests/bare_help.test`, `docs/NEW_ARCHITECTURE.md`, `docs/NETWORKING.md`, `docs/cloud_architecture.md`, `docs/deployment.md`, `docs/NETWORKING_PROGRESS.md` | Removed the legacy manual `amux server connect <host:port>` CLI and `ClientService.ConnectToServer` RPC surface; routing now comes from pairing reachabilities and R1 runtime Links. Migrated stale remote e2e tests to `amux pair --listen` + `amux pair --connect`, made `tcp_port: 0` explicit where LAN e2e tests need it, and isolated e2e identities/state/logs per config. Evidence: `cargo check -p amux --tests`; `cargo test -p amux-cli -- --nocapture`; `cargo test -p amux protocol:: -- --nocapture`; `cargo test -p e2e-runner -- --nocapture`; `cargo run -p e2e-runner -- run remote`; `cargo run -p e2e-runner -- run bare_help`; `cargo check --workspace --all-targets`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. The former `cloud_relay_connection` TLS fixture mismatch is resolved by CLOUDTLS1. |
| AUDIT | done | objective | Completion audit maps every explicit spec/objective requirement to concrete implementation and verification evidence. | `docs/NETWORKING_PROGRESS.md`, command output | Audit matrix added below and reviewed in two simplification/bug-security rounds. The initial result was not complete; blockers were split into CLOUDTLS1, WIRELIMITS1, DRAIN1, SWAP1, QRINIT1, RESOURCE1, STATUS1, and OBS1; resolved blocker rows below record later closure. |
| CLOUDTLS1 | done | N-X-3a, N-X-5, implementation defaults | Remove debug/plaintext cloud routing bypass and repair the cloud relay e2e fixture so cloud routing is TLS in tests too. | `crates/amux/src/services/startup/cloud.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/transport/tls.rs`, `crates/amux/src/transport/tcp.rs`, `crates/amux/src/transport/mod.rs`, `crates/e2e-runner/src/executor.rs`, `e2e-tests/cloud_relay_connection.test` | Cloud routing now always uses `tls_channel`; the old debug/test `tcp_channel` bypass was removed. The e2e cloud fixture provisions a localhost TLS leaf cert/key and debug CA for routing TLS, starts the cloud relay in foreground mode, and pairs the cloud-routed daemons before asserting trusted agent sharing. Cloud TLS accepts are bounded and concurrent so one stalled handshake cannot block later sockets. Evidence: `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p e2e-runner -- --nocapture`; `cargo run -p e2e-runner -- run cloud_relay_connection`; `cargo check -p amux --tests`; `cargo check -p e2e-runner --tests`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| WIRELIMITS1 | done | 6.2, N-R-10, N-TN-7, implementation defaults | Enforce wire/protocol limits: routing `Host.name` 256-byte cap, route hop cap, and inbound `TunnelFrame.payload` cap. | `crates/amux/src/config.rs`, `crates/amux/src/routing/host.rs`, `crates/amux/src/routing/mod.rs`, `crates/amux/src/routing/route.rs`, `crates/amux/src/routing/wire.rs`, `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/tunnel/mod.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/services/client.rs` | Host-name cap, route hop cap, and inbound tunnel payload cap implemented. Review fixes added producer-side `Config` validation, cap-before-parse/drop-only over-hop handling, centralized inbound `Host` validation, Connect-level oversized-frame GoAway/cleanup coverage, and post-handshake stream-error GoAway/cleanup coverage. Evidence: `cargo test -p amux oversized -- --nocapture`; `cargo test -p amux inbound_route_over_hop_cap_is_dropped_marker -- --nocapture`; `cargo test -p amux post_handshake_stream_error_sends_protocol_goaway_and_cleans_link -- --nocapture`; `cargo test -p amux oversized_tunnel_frame_sends_protocol_goaway_and_cleans_link -- --nocapture`; `cargo test -p amux routing:: -- --nocapture`; `cargo test -p amux config:: -- --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| DRAIN1 | done | N-TN-8 | Refuse new calls on a draining Link, including cached active-route Channels. | `crates/amux/src/connection.rs`, `crates/amux/src/routing/link_registry.rs`, `crates/amux/src/routing/connect/mod.rs` | `ConnectionManager::materialize` checks the route first hop before returning any cached Channel, including active and pre-active direct or tunnel Channels; inbound and local GoAway paths mark Links draining before the drain window. Evidence: `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux channel_to_rejects_pre_active_cached_channel_on_draining_link -- --nocapture`; `cargo test -p amux send_goaway_marks_links_draining_before_notifying -- --nocapture`; `cargo test -p amux connector_manager_rejects_cached_channel_after_inbound_goaway -- --nocapture`; `cargo test -p amux inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing -- --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| SWAP1 | done | N-CN-5 | Enforce old-route teardown on make-then-break swaps and route removal so old multi-hop tunnels/in-flight streams fail instead of surviving only because the pool entry was removed. | `crates/amux/src/connection.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/routing/connect/mod.rs`, `crates/amux/src/routing/core.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/services/reachability.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/client.rs` | Route runtime cleanup is a required shared dependency for routing Connect contexts and `ConnectionManager`; route removals synchronously unregister route Channels and remove matching tunnels; host replacement purges stale routes from `RoutingCore`; removed `TunnelId`s are bounded tombstones so late frames cannot recreate old endpoint tunnels. Evidence: `cargo test -p amux retired_tunnel_tombstones_are_bounded -- --nocapture`; `cargo test -p amux removed_route_tombstones_tunnel_ids -- --nocapture`; `cargo test -p amux teardown_host_removes_routes_channels_tunnels_and_links -- --nocapture`; `cargo test -p amux swapping_routes_drops_old_route_tunnels -- --nocapture`; `cargo test -p amux host_down_drops_route_tunnels -- --nocapture`; `cargo test -p amux connector_cleans_link_routes_when_input_stream_closes -- --nocapture`; `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux tunnel::pool:: -- --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| QRINIT1 | done | 5.1, N-P-4, N-P-5 | Implement the QR/token initiator path: parse QR payload, open QR-pubkey-pinned pairing channel, call `PairByToken`, and commit initiator-side trust. | `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/protocol/mod.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/qr_pairing.rs`, `crates/amux/src/transport/tls.rs`, `crates/amux/src/transport/mod.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/tunnel/transport.rs`, `crates/amux/src/connection.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/client/mod.rs`, `crates/amux-cli/src/main.rs`, `docs/NETWORKING.md`, `docs/NETWORKING_PROGRESS.md` | QR payload consumption is implemented as `amux pair --qr <payload>` and local `ClientService.PairQrCloudPeer`; the daemon opens a cloud-route tunnel with TLS server verification pinned to the QR pubkey, calls route-proven cloud-only `PairByToken`, commits `Reachability::Cloud` on the initiator, and the responder commits the scanner identity. Review fixes moved QR payload parsing into the library, added daemon-side duplicate pubkey/token preflight before dialing, added SPAKE2 `PairingComplete`, preserved only the active pairing tunnel during key replacement, and tombstone preserved tunnels on drop. Evidence: `cargo check -p amux --tests`; `cargo check -p amux-cli --tests`; `cargo check -p e2e-runner --tests`; `cargo test -p amux -- --nocapture`; `cargo test -p amux descriptor_set_contains_core_protocol_messages_and_services -- --nocapture`; `cargo test -p amux dispatcher:: -- --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`; `cargo test -p amux cloud_pin_pairing_updates_both_trust_stores -- --nocapture`; `cargo test -p amux cloud_qr_pairing_updates_both_trust_stores_and_pins_responder_pubkey -- --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`; `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux tunnel::pool:: -- --nocapture`; `cargo test -p amux qr_pairing_payload -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo test -p e2e-runner -- --nocapture`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| RESOURCE1 | done | N-R-12, implementation defaults | Enforce bounded-resource policy: route/host retention caps, external TCP TLS-handshake rate limit, and cloud inbound tunnel rate cap. This is cross-layer because N-R-12 eviction depends on active-route, trust-store, and client-visible activity. | `crates/amux/src/resource_limits.rs`, `crates/amux/src/routing/`, `crates/amux/src/connection.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/tunnel/pool.rs`, `crates/amux/src/services/startup/`, `docs/NETWORKING.md` | Route/host retention caps, external TCP per-IP + global TLS-handshake caps, and cloud inbound tunnel caps are implemented. Cloud-Link detection is stable Link metadata, not mutable routing state. Evidence: `cargo fmt --check`; `cargo check -p amux --tests`; `cargo test -p amux routing:: -- --nocapture`; `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux dispatcher::tests:: -- --nocapture`; `cargo test -p amux tunnel::pool::tests:: -- --nocapture`; `cargo test -p amux services::client::tests:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux resource_limits:: -- --nocapture`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| STATUS1 | done | 8.12, ClientService host-list surface | Add or deliberately revise the host listing trust/reachability status model. | `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/routing/events.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/connection.rs`, `crates/amux/src/services/reachability.rs`, `crates/amux-cli/src/main.rs`, `crates/amux-ui/src/inventory.rs`, `crates/amux-ui/src/notification.rs`, `crates/amux-ui/src/types.rs`, `docs/NETWORKING.md` | Host inventory uses server-owned `ListHostsRequest.scope`, flattened `HostEntry` rows, `HostUpdated` upserts, trusted reachability status, trust-only offline rows, local-only pairing candidates, remote untrusted filtering, and role-based cloud route classification. Client-visible activity is marked only after the caller/subscriber filter delivers an online host row. UI inventory now consumes and propagates `HostUpdated` rows as `HostEntry` values. Evidence: `cargo check -p amux --tests`; `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p amux remote_subscriber_hidden_untrusted_live_event_does_not_mark_visible_activity -- --nocapture`; `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo test -p amux protocol:: -- --nocapture`; `cargo test -p amux services::reachability:: -- --nocapture`; `cargo check --workspace --all-targets`; `cargo test -p amux-ui -- --nocapture`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed plus final spec-audit follow-up. |
| OBS1 | done | implementation defaults | Add structured audit-log categories matching §10. | `crates/amux/src/audit.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/pairing.rs`, `crates/amux/src/pin_pairing.rs`, `crates/amux/src/ssh_pairing.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/startup/`, `crates/amux/src/routing/link_registry.rs` | Structured tracing categories now cover pairing lifecycle, mTLS/JWT auth failures, trust insert/update/replace, link up/down, and disruptive client-service calls. Review fixes added responder-side JWT failures, helper-side SSH/direct PIN pairing failures, TLS accept failures, pair-mode expiry, cancel-only-when-active behavior, and sanitized peer error details. Evidence: `cargo check -p amux --tests`; `cargo test -p amux audit:: -- --nocapture`; `cargo test -p amux identity:: -- --nocapture`; `cargo test -p amux dispatcher:: -- --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`; `cargo test -p amux routing::link_registry:: -- --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`; `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux ssh_pairing:: -- --nocapture`; `cargo fmt --check`; `git diff --check`. Two simplification and two bug/security review rounds completed. |
| SPECSIM1 | done | 3.1, 4.2, 5.3.1, 8.3-8.4, 8.12, N-C-2..N-C-3, N-X-3a, N-X-6, N-G-1..N-G-7, N-S-2 | Simplify source spec and implementation after review discussion: `HostEntry.online`, SSH relay over the normal local Unix socket, public/non-secret pubkey wording, normal WebPKI/CA cloud TLS wording, and trust-vs-reachability separation. | `docs/NETWORKING.md`, `docs/NETWORKING_PROGRESS.md`, `e2e-tests/remote_list_agents.test`, `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/transport/io.rs`, `crates/amux/src/services/startup/mod.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/routing/`, `crates/amux/src/client/mod.rs`, `crates/amux/src/server.rs`, `crates/amux/src/ssh_pairing.rs`, `crates/amux-cli/src/main.rs`, `crates/amux-ui/src/types.rs` | Host inventory now exposes `HostEntry.online` and `UntrustedButOnline`; `amux relay` connects to `Config.socket_path` and no longer has a sibling socket or `SshRelay` auth class; local-admin pairing/trust RPCs include `PairQrCloudPeer`, fail closed on missing metadata, and are rejected from paired remote mTLS; cloud pubkey/TLS and trust/reachability wording are simplified. Evidence: `cargo check -p amux --tests`; `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`; `cargo test -p amux protocol:: -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo test -p amux-ui -- --nocapture`; `cargo check --workspace --all-targets`; `cargo fmt --check`; `git diff --check`. Simplification/correctness reviews completed; final spec-alignment review issues were resolved. |
| REVOKE1 | done | 5.4, 6.3, 7, N-T-6, N-CN-9, N-S-2, implementation defaults | Implement local trust revocation through `ClientService.Unpair` and `amux unpair`, including durable trust removal, user-revoked GoAway, route/channel/tunnel teardown, and active audit logging. | `docs/NETWORKING.md`, `docs/NETWORKING_PROGRESS.md`, `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/protocol/mod.rs`, `crates/amux/src/identity.rs`, `crates/amux/src/audit.rs`, `crates/amux/src/routing/link_registry.rs`, `crates/amux/src/connection.rs`, `crates/amux/src/services/client.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/lib.rs`, `crates/amux-cli/src/main.rs`, `crates/amux-cli/Cargo.toml` | `GO_AWAY_REASON_USER_REVOKED`, `ListPeers`, `GetPeer`, and `Unpair` are protocol v5. `TrustStore::remove` persists through the existing atomic trust-store save path; Unpair is local-admin-only, sends immediate user-revoked GoAway, reuses SWAP1 host teardown for route/channel/tunnel cleanup and tombstones, emits `trust.remove`, and CLI peer list/info/unpair commands are wired. Evidence: `cargo check --workspace --all-targets`; `cargo test -p amux trust:: -- --nocapture`; `cargo test -p amux services::client::tests::tonic_unpair_ -- --nocapture`; `cargo test -p amux audit:: -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo fmt --check`; `git diff --check`; two simplification and two bug/security review rounds completed. |
| PAIRMOD1 | done | source layout | Consolidate top-level pairing files under `crates/amux/src/pairing/` without behavior changes. | `crates/amux/src/pairing/mod.rs`, `crates/amux/src/pairing/pin.rs`, `crates/amux/src/pairing/qr.rs`, `crates/amux/src/pairing/ssh.rs`, `crates/amux/src/lib.rs`, `crates/amux/src/client/mod.rs`, `crates/amux/src/services/pairing.rs` | `pairing.rs`, `pin_pairing.rs`, `qr_pairing.rs`, and `ssh_pairing.rs` were moved into the `pairing/` module; public re-exports and internal imports now use `pairing::{pin,qr,ssh}` while `services/pairing.rs` remains the gRPC service implementation. Evidence: `cargo check --workspace --all-targets`; `cargo test -p amux pairing:: -- --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`; `cargo test -p amux -- --nocapture`; `cargo fmt --check`; `git diff --check`. One simplification and one bug review round completed because this checkpoint is mechanical. |
| IDTRUST1 | done | source layout | Split peer trust-store domain/storage out of `identity.rs` into `trust.rs` without behavior changes. | `crates/amux/src/identity.rs`, `crates/amux/src/trust.rs`, `crates/amux/src/lib.rs`, `crates/amux/src/audit.rs`, `crates/amux/src/dispatcher.rs`, `crates/amux/src/server.rs`, `crates/amux/src/services/`, `crates/amux/src/routing/core.rs`, `crates/amux/src/transport/tls.rs`, `crates/amux/src/tunnel/pool.rs` | `identity.rs` now holds device keypair, host id, certificate, and TLS verifier builders; `trust.rs` owns `TrustStore`, `TrustEntry`, `Reachability`, `SharedTrustStore`, pairing-update outcomes, trust JSON load/save, and trust-store tests. Evidence: `cargo check --workspace --all-targets`; `cargo test -p amux identity:: -- --nocapture`; `cargo test -p amux trust:: -- --nocapture`; `cargo test -p amux -- --nocapture`; `cargo fmt --check`; `git diff --check`. One simplification and one bug review round completed. |
| CLIPPY1 | done | CI/quality | Fix all `cargo clippy --workspace --all-targets -- -D warnings` failures and confirm the CI guardrail. | `crates/amux/proto/amux/v1/amux.proto`, `crates/amux/src/connection.rs`, `crates/amux/src/routing/`, `crates/amux/src/services/client.rs`, `crates/amux/src/services/pairing.rs`, `crates/amux/src/services/startup/`, `crates/amux/src/transport/ssh.rs`, `crates/amux/src/trust.rs`, `.github/workflows/ci.yml` | Clippy is clean with zero warnings. Fixes include source-level simplifications, the generated-code oneof rename from `target` to `kind`, grouped peer-trust commit arguments, scoped test locks before await, and moving SSH tests after impl items. CI already contained `cargo clippy --workspace --all-targets -- -D warnings`, so no workflow change was needed. Evidence: `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p amux -- --nocapture`; `cargo fmt --check`; `git diff --check`. |
| SPECMAP1 | done | 12 | Reconcile the non-normative reference implementation map with the post-cleanup layout. | `docs/NETWORKING.md`, `docs/NETWORKING_PROGRESS.md` | §12 now maps the actual flat `identity.rs`/`trust.rs` layout, relocated `pairing/` helpers, monolithic `connection.rs` and `routing/connect/mod.rs`, current crate-internal API names, call paths, and every §10 invariant to concrete owning modules. Evidence: stale-layout grep against §12; `git diff --check`. One simplification and one bug review round completed. |

## Completion Audit

Status: `complete` as of 2026-05-19. Result: the known implementation blockers from the completion audit are resolved, and final gpt-5.5 xhigh spec-discrepancy validation found no remaining actionable discrepancies.

Blocking gaps found:

- None currently open.
- Source-spec cleanups applied during SPECSIM1: device pubkeys are public/non-secret while normal v1 cloud routing does not require them; cloud TLS is ordinary public-CA/WebPKI hostname validation; Unix socket topology is the single local socket used by local callers and SSH `amux relay`; trust identity and outbound reachability hints are distinct concerns.
- Resolved by CLOUDTLS1: cloud `RoutingService` no longer has a debug/test plaintext routing shortcut; `cloud_relay_connection` now exercises TLS routing with a localhost test CA, and the local fake HTTP cloud API remains scoped to the e2e OAuth/JWKS/connect metadata control plane rather than the `RoutingService` transport.
- Resolved by WIRELIMITS1: routing/local `Host.name` 256-byte validation, inbound route hop cap, inbound tunnel payload cap, and protocol GoAway handling for oversized or decoder-failed post-handshake tunnel traffic.
- Resolved by DRAIN1: cached direct and tunnel route Channels now refuse new calls while their first hop is draining, for both inbound and locally emitted GoAway.
- Resolved by SWAP1: make-then-break swaps, route removals, link cleanup, and host replacement now actively remove old route Channels/tunnels and tombstone removed tunnel IDs.
- Resolved by QRINIT1: QR/token initiators parse the QR payload through library code, verify the responder TLS certificate against the QR pubkey, call cloud-route-only `PairByToken`, and commit initiator/responder trust with `Reachability::Cloud`.
- Resolved by RESOURCE1: routing route/host caps, external TCP TLS-handshake limits, and cloud inbound tunnel limits are enforced with active-route, trust-store, and client-visible activity protections.
- Resolved by STATUS1: host listing and subscription surfaces now expose stable
  trust/reachability state, trusted offline rows, server-owned pairing-candidate
  filtering, local-only untrusted discovery, and role-based cloud route checks.
- Resolved by OBS1: §10 audit-log categories are emitted as structured tracing
  fields under `amux::audit` for pairing lifecycle, mTLS/JWT authentication
  failures, committed trust updates, Link up/down events, and disruptive
  client-service calls.
- Resolved by REVOKE1: local trust revocation removes trust durably, sends
  user-revoked GoAway, tears down active routes/channels/tunnels, rejects
  remote mTLS callers, exposes peer list/info/unpair CLI and RPC helpers, and
  activates `trust.remove` audit events.

### Audit Matrix

| Requirement(s) | Status | Implementation and evidence |
| --- | --- | --- |
| Objective tracking | implemented | This ledger tracks checkpoints, artifacts, verification, and review rounds. |
| N-K-1, N-K-2, N-K-3 | implemented | Device keypair, `host_id`, persistence, atomic private-file creation, and file modes are in `identity.rs`; K1 evidence covers identity/setup/server tests. |
| N-T-1, N-T-2, N-T-4, N-T-5, N-T-6 | implemented | Local `trust.json`, `TrustEntry`, `Reachability::{Cloud,Ssh,DirectTcp}`, deduplicating pairing upsert, pair-time reachability commits, and local-only revocation are in `trust.rs`, `services/pairing.rs`, `services/client.rs`, and `services/reachability.rs`. |
| N-T-3, N-X-1, N-X-3, N-X-9 | implemented | Pinned device mTLS verifiers bind cert pubkeys to trust-store entries and reject `Hello.host_id` mismatches; covered by G1/R1 routing and identity tests. |
| N-C-1, N-C-4 | implemented | Cloud relay hosts `RoutingService`, not `PairingService`; QR/PIN/SSH pairing authorization is OOB/pair-mode, not cloud-issued. |
| N-C-2, N-C-3 | implemented | Cloud routing metadata exposes host/user/presence/name/capability/routing information. Device pubkeys are public/non-secret, but normal v1 cloud routing does not require or receive them; private keys, PINs, tokens, trust store, pair-mode, and paired gRPC payloads remain outside cloud knowledge; tunnel channels use end-to-end mTLS. |
| N-X-2, N-X-4 | implemented | Direct TCP, cloud, SSH, and tunnel transports are byte-stream carriers; route-length-specific Channels/tunnels are implemented in `connection.rs`, `tunnel/`, and `services/reachability.rs`. |
| N-X-3a, N-X-5 | implemented | Cloud `RoutingService` transport now always uses `tls_channel`, and the cloud relay e2e fixture is TLS-aligned for routing. The local fake HTTP cloud API is only an e2e OAuth/JWKS/connect metadata control plane, not a `RoutingService` transport. PIN no-verify TLS and SSH non-TLS paths match explicit spec exceptions. |
| N-X-6 | implemented | Unix socket mode handling and topology are implemented as a single `Config.socket_path`, mode `600`, used by local callers and SSH `amux relay`; the former sibling SSH relay socket has been removed. |
| N-X-7, N-X-8 | implemented | External TCP listener is opt-in via `tcp_port`; LAN responder flows fail when unset. |
| N-MT-1, N-MT-2, N-MT-3 | implemented | Cloud routing state is keyed by user; device daemon state is single-tenant; pairing commits between device states. |
| N-P-1, N-P-2, N-P-3 | implemented | `PairingService` is pre-trust only; responder pair-mode gates calls, TTLs secrets, rejects overlapping sessions, and consumes success. |
| N-P-4 | implemented | PIN/SPAKE2+AEAD, SSH stdio, QR/token responder service paths, and QR/token initiator consumption are implemented; QRINIT1 closes the QR initiator side with pinned cloud TLS and `PairByToken`. |
| N-P-5, N-P-6, N-P-7, N-P-8, N-P-9 | implemented | Pairing trust updates, token single-use, cloud-opaque pairing, runtime reachability semantics, and self-pairing rejection are implemented for PIN/SSH and QR paths. QRINIT1 adds daemon-side QR self/duplicate preflight, cloud-only token ingress, and replacement response-path coverage. |
| N-G-1, N-G-2, N-G-3, N-G-4, N-G-5, N-G-6, N-G-7 | implemented | Trusted vs Pairing Server topology, dispatcher admission, JWT cloud routing auth, single Unix-socket local/SSH relay ingress, local-admin RPC gating for paired remote mTLS, and accept-once transport auth are implemented. |
| N-R-1, N-R-2, N-R-3, N-R-4, N-R-5, N-R-6, N-R-7, N-R-8, N-R-9, N-R-11 | implemented | Routing semantics, deduplication, route-specific down events, split-horizon, prepend-on-forward, snapshots, and next-hop drop behavior are in `routing/core.rs`, `routing/wire.rs`, `routing/connect/mod.rs`, and `tunnel/pool.rs`. |
| N-R-10 | implemented | Inbound routing events that would exceed `ROUTE_HOP_CAP` after prepending the incoming Link are converted to `RouteOverHopCap` and dropped without closing the Link; covered by `inbound_route_over_hop_cap_is_dropped_marker` and routing tests. |
| N-R-12 | implemented | `RoutingCore` caps retained routes per host at 16 and retained untrusted hosts at 1000, evicting oldest non-active routes/hosts while preserving active-route, trust-store, and recent client-visible hosts. Covered by RESOURCE1 routing/core tests. |
| N-L-1, N-L-2, N-L-3, N-L-4 | implemented | Link naming, registration order, one-hop Channel registration, and no tunnel wrapping for one-hop calls are implemented and covered by R1/CN1 tests. |
| N-CN-1, N-CN-2, N-CN-3, N-CN-4, N-CN-6, N-CN-7, N-CN-8, N-CN-9 | implemented | Route-keyed pool, `ConnectionManager`, shortest-route policy, event-only reevaluation, no automatic fallback, startup DirectTcp/SSH attempts, and revocation teardown of route/channel/tunnel state are implemented. |
| N-CN-5 | implemented | Make-then-break activation materializes the new route before flipping active state; old-route, route-down, link cleanup, and host replacement paths remove stale route Channels and matching tunnels so old multi-hop streams fail. Covered by SWAP1 tests. |
| N-TN-1, N-TN-2, N-TN-3, N-TN-4, N-TN-5, N-TN-6 | implemented | Tunnel IDs, route-carried frames, endpoint dispatch, `TunnelTransport`, and byte-stream close behavior are implemented. |
| N-TN-7 | implemented | Outbound tunnel chunks are capped by the 64 KiB read buffer, and inbound `TunnelFrame.payload` values larger than `TUNNEL_FRAME_PAYLOAD_MAX` are rejected by `TunnelPool`; `routing::connect` turns the error into `GoAway { PROTOCOL_ERROR }` and closes the Link. Covered by `inbound_frame_rejects_oversized_payload` through the `oversized` test filter. |
| N-TN-8 | implemented | Inbound and locally emitted `GoAway` mark Links draining; new tunnel materialization and cached `ConnectionManager` route Channels reject draining first hops with `LinkDraining`; existing in-flight tunnel frames keep flowing during drain. |
| N-S-1 | deferred by scope | No phone client exists in this repo; the implementation has no phone LAN/SSH pairing surface. |
| N-S-2 | implemented | Paired peers and local Unix-socket callers reach the Trusted Server with equivalent runtime authority. Local-only pairing admin/trust mutation RPCs (`StartPairing`, `GetPairingStatus`, `CancelPairing`, `PairPeer`, `PairPinCloudPeer`, `PairQrCloudPeer`, `ListPeers`, `GetPeer`, `Unpair`) are accepted on local Unix/in-process ingress, including SSH `amux relay` because it uses that socket, and rejected on paired remote mTLS ingress. |
| §7 CLI surface | implemented | `amux pair`, `--qr`, `--listen`, `--connect`, `--via-ssh`, `peer list`, `peer info`, and `unpair` are implemented; legacy `amux server connect` and `ClientService.ConnectToServer` are removed. |
| §6.2 wire field bounds | implemented | Pairing and SSH identity names are bounded to 256 bytes, routing `Host.name` inbound validation now enforces the same cap, routing hop count is bounded, and inbound tunnel payloads are capped at 64 KiB. |
| §8.12 host listing status | implemented | `ClientService.ListHosts` and `SubscribeHosts` carry flattened `HostEntry` values with stable id/name, explicit `online`, optional version/capabilities, trust status, and trusted reachability status. Server-owned `ListHostsRequest.scope` exposes normal inventory or local-only pairing candidates; remote callers cannot enumerate untrusted candidates and normal remote snapshots/streams filter untrusted online hosts. Focused service and connection tests cover untrusted online hosts, trusted offline `unknown`, cached trusted `unreachable` errors, non-agent pairing candidates, trust-transition updates, trusted route-loss offline updates, reachability cache status-change events, and spoofed cloud-relay capability rejection. |
| §10 crypto/protocol defaults | implemented | `PROTOCOL_VERSION = 5`, Ed25519 identity/certs, raw 32-byte pubkeys, PKCS#8 DER private keys, TLS 1.3 for device mTLS, SPAKE2/HKDF/ChaCha20-Poly1305, reauth timing, pair-mode TTL, PIN format, PIN attempt cap, cloud routing TLS, pending-routing-event cap, route/host caps, external TCP TLS-handshake limits, cloud inbound tunnel caps, and structured audit categories are implemented. |
| §10 cloud auth defaults | implemented | OAuth endpoint paths, client ID, scopes, and refresh-token storage are implemented in `auth/oauth.rs` and CLI auth state helpers. |
| §10 on-disk paths | implemented | Sensitive file names and modes, data/state/log path resolution, and state-file mode now align with `paths.rs`, `identity.rs`, and `state.rs`. |
| §10 audit log categories | implemented | `audit.rs` centralizes the required category names and emits structured tracing fields for `pairing.start`, `pairing.success`, `pairing.failure`, `pairing.cancel`, `auth.mtls_handshake_failure`, `auth.jwt_failure`, `trust.insert`, `trust.update`, `trust.replace`, `trust.remove`, `link.up`, `link.down`, and `client_service.disruptive_call`. |
| Cross-doc source-of-truth cleanup | implemented | Stale references to `ClientService.ConnectToServer`, `amux server connect`, and old cloud/deployment shortcuts were removed or redirected during CLI1. |

## Work Log

### 2026-05-19

- Created this ledger because the networking spec requires a systematic markdown
  tracker for implementation, evidence, and review rounds.
- Started and completed REVOKE1:
  - Moved revocation into the active networking spec as §5.4, added N-T-6 and
    N-CN-9, listed `Unpair`/peer list/info as local-admin-only
    `ClientService` RPCs, activated `trust.remove`, and bumped
    `PROTOCOL_VERSION` to 5 with `GO_AWAY_REASON_USER_REVOKED`.
  - Added `TrustStore::remove`, peer list/get/unpair RPCs, public client
    helpers, and `amux peer list`, `amux peer info`, and `amux unpair`
    commands.
  - Wired Unpair through the existing durable trust-store save path, per-peer
    user-revoked GoAway, SWAP1 `ConnectionManager::teardown_host` cleanup, and
    `trust.remove` auditing.
  - Verification: `cargo check -p amux --tests`; `cargo test -p amux
    services::client::tests::tonic_unpair_ -- --nocapture`.
  - Applied REVOKE1 review round 1:
    - Kept revocation local-admin-only instead of adding a network revocation
      protocol.
    - Reused existing host teardown/tombstone machinery instead of adding a
      separate revocation cleanup path.
    - Added the `trust::` test namespace while trust-store code still lives in
      `identity.rs`, so the required focused trust-store command is meaningful
      before IDTRUST1.
  - Applied REVOKE1 review round 2:
    - Confirmed TLS pin verifiers read the live trust store after removal, so
      there is no stale acceptance cache to clear.
    - Confirmed paired remote mTLS callers are rejected for list/get/unpair,
      while SSH relay remains local-equivalent through the Unix socket.
  - Final REVOKE1 verification: `cargo check --workspace --all-targets`;
    `cargo test -p amux trust:: -- --nocapture`; `cargo test -p amux
    services::client::tests::tonic_unpair_ -- --nocapture`; `cargo test -p
    amux audit:: -- --nocapture`; `cargo test -p amux-cli -- --nocapture`;
    `cargo fmt --check`; `git diff --check`.
- Started and completed PAIRMOD1:
  - Moved the top-level pairing implementation files into
    `crates/amux/src/pairing/`: `mod.rs`, `pin.rs`, `qr.rs`, and `ssh.rs`.
  - Updated `lib.rs` public re-exports and internal imports to use
    `crate::pairing::{pin,qr,ssh}` while leaving
    `crates/amux/src/services/pairing.rs` as the gRPC service implementation.
  - Applied the single mechanical review round required for this checkpoint:
    no behavior changes, no new abstractions, and no stale module declarations
    or imports under `crates/amux` or `crates/amux-cli`.
  - Verification: `cargo check --workspace --all-targets`; `cargo test -p
    amux pairing:: -- --nocapture`; `cargo test -p amux services::pairing::
    -- --nocapture`; `cargo test -p amux -- --nocapture`; `cargo fmt
    --check`; `git diff --check`.
- Started and completed IDTRUST1:
  - Moved the peer trust-store types and JSON persistence into
    `crates/amux/src/trust.rs`: `TrustStore`, `TrustEntry`, `Reachability`,
    `SharedTrustStore`, and `TrustStorePairingUpdate`.
  - Kept device identity, host-id/key files, certificate generation, and TLS
    pinned-verifier builders in `identity.rs`; imports now use `crate::trust`
    for peer trust concerns.
  - Applied the single split-review round required for this checkpoint:
    preserved read-only readiness checks with `TrustStore::load_in`, left the
    shared file-mode/atomic-write primitives in `identity.rs`, and moved
    trust-store tests under the `trust::` namespace.
  - Verification: `cargo check --workspace --all-targets`; `cargo test -p
    amux identity:: -- --nocapture`; `cargo test -p amux trust:: --
    --nocapture`; `cargo test -p amux -- --nocapture`; `cargo fmt --check`;
    `git diff --check`.
- Started and completed CLIPPY1:
  - Fixed all `cargo clippy --workspace --all-targets -- -D warnings`
    findings without adding new clippy allow annotations.
  - Root fixes included collapsing nested conditionals, removing redundant
    bool conversions, replacing a manual error return with `?`, scoping test
    lock guards before awaits, moving SSH transport tests after impl items, and
    using a slice instead of a temporary `Vec`.
  - Renamed `PeerReachability`'s generated oneof from `target` to `kind` so
    generated Rust no longer trips `enum_variant_names`; this does not change
    field numbers or wire encoding.
  - Grouped peer-trust commit inputs into context/update structs instead of
    suppressing `too_many_arguments`, and confirmed `.github/workflows/ci.yml`
    already runs the required clippy command.
  - Verification: `cargo clippy --workspace --all-targets -- -D warnings`;
    `cargo test -p amux -- --nocapture`; `cargo fmt --check`; `git diff
    --check`.
- Started and completed SPECMAP1:
  - Rewrote §12.1 to match the current reference layout: flat
    `identity.rs`/`trust.rs`, relocated `pairing/` helpers, monolithic
    `connection.rs` and `routing/connect/mod.rs`, flat `audit.rs` and
    `resource_limits.rs`, and the actual CLI file layout.
  - Updated §12.2 through §12.6 with current crate-internal API names,
    local-admin `ClientService` peer/trust RPCs, and direct vs tunneled call
    paths that reference the modules actually on disk.
  - Replaced the wildcard invariant ownership table in §12.7 with one row for
    every §10 invariant, mapped to the primary implementation module or
    intentionally cross-cutting pair of modules.
  - Applied the required SPECMAP1 review round:
    - Simplification: kept §12 non-normative and descriptive instead of
      widening public exports or reintroducing aspirational split modules.
    - Bug/security: grepped for stale removed paths and pre-cleanup API names,
      then fixed the internal API sketches to use the actual crate-internal
      signatures.
  - Final gpt-5.5 xhigh discrepancy review found two §12.2 API-sketch
    mismatches; fixed `PairMode` PIN attempt/commit signatures and
    `PairPinCloudPeer`/`PairQrCloudPeer` response types before final
    acceptance.
  - Verification: stale-layout grep against `docs/NETWORKING.md`; `git diff
    --check`.
- Started RESOURCE1:
  - Added a shared sliding-window limiter and wired the external TCP dispatcher
    to cap TLS handshake attempts per source IP at 10/minute.
  - Added a cloud-origin new-inbound-tunnel limiter in `TunnelPool` at
    30/minute while keeping the cloud Link up and dropping excess frames.
  - Added `RoutingCore` retention caps: 16 routes per host and 1000 retained
    hosts, with route eviction preserving the `ConnectionManager` active route,
    host eviction preserving trust-store peers, active-route peers, and hosts
    with recent client-visible activity.
  - Wired active-route and client-visible activity hints from
    `ConnectionManager` and `ClientService`; clarified the client-visible
    activity grace window in `docs/NETWORKING.md`.
  - Verification so far: `cargo check -p amux --tests`; `cargo test -p amux
    routing::core::tests:: -- --nocapture`; `cargo test -p amux
    dispatcher::tests:: -- --nocapture`; `cargo test -p amux
    tunnel::pool::tests:: -- --nocapture`; `cargo test -p amux
    services::client::tests:: -- --nocapture`; `cargo test -p amux
    resource_limits:: -- --nocapture`.
  - Applied RESOURCE1 review round 1:
    - Centralized resource-limit defaults in `resource_limits.rs` and removed
      duplicate test-only limiter code.
    - Changed the routing host cap to count untrusted hosts only, so
      trust-store hosts are truly exempt from both eviction and cap pressure.
    - Moved cloud inbound tunnel limiting into `PoolState` keyed by
      `TunnelId`, so cloud-origin forwarded frames are capped too and repeated
      frames for one tunnel consume only one rate slot.
    - Restricted client-visible activity marking to actual host-list exposure:
      list/snapshot calls or delivered live host events, not internal model
      insertion alone.
    - Added a global external TCP TLS-handshake concurrency cap of 128 to
      complement the required 10/minute/source-IP limiter.
  - Verification after review round 1: `cargo fmt --check`; `cargo check -p
    amux --tests`; `cargo test -p amux routing::core::tests:: -- --nocapture`;
    `cargo test -p amux dispatcher::tests:: -- --nocapture`; `cargo test -p
    amux tunnel::pool::tests:: -- --nocapture`; `cargo test -p amux
    services::client::tests:: -- --nocapture`; `cargo test -p amux
    resource_limits:: -- --nocapture`.
  - Applied RESOURCE1 review round 2:
    - Split raw host snapshots from client-visible side effects, so
      `ListHosts` marks only the filtered hosts actually returned and
      subscribe marks only its delivered snapshot/live events.
    - Replaced duplicate tunnel-id tombstone/admission queues with a shared
      bounded tunnel-id set and removed the unnecessary limiter `Clone` bound.
    - Moved cloud-Link classification from mutable routing-table lookup to
      stable `LinkRegistry` role metadata. Production cloud attach marks the
      connector side as `CloudRelay`; the test bearer cloud helper does the
      same. Cloud-origin endpoint and forwarded frames remain capped even if
      the cloud host route is removed.
    - Added an external TCP TLS-handshake concurrency cap of 128 alongside the
      required 10/minute/source-IP sliding window.
  - Final RESOURCE1 verification: `cargo fmt --check`; `cargo check -p amux
    --tests`; `cargo test -p amux routing:: -- --nocapture`; `cargo test -p
    amux connection:: -- --nocapture`; `cargo test -p amux dispatcher::tests::
    -- --nocapture`; `cargo test -p amux tunnel::pool::tests:: -- --nocapture`;
    `cargo test -p amux services::client::tests:: -- --nocapture`; `cargo test
    -p amux services::startup:: -- --nocapture`; `cargo test -p amux
    resource_limits:: -- --nocapture`; `git diff --check`.
- Started STATUS1:
  - Replaced the raw-host client inventory surface with a flattened
    `HostEntry`: stable `host_id`/`name`, explicit `online`,
    online-only `version`/`capabilities`, trust status, and optional
    reachability status for trusted/local hosts.
  - Included trust-store-only peers in normal host listing snapshots so
    trusted-but-offline hosts can be shown as `unknown` or cached
    `unreachable` without inventing routing capabilities.
  - Wired lazy reachability error caching from failed direct/SSH reachability
    establishment and failed remote agent dispatch through
    `ConnectionManager`.
  - Verification so far: `cargo check -p amux --tests`; `cargo test -p amux
    services::client:: -- --nocapture`; `cargo test -p amux connection:: --
    --nocapture`; `cargo test -p amux services::reachability:: --
    --nocapture`.
  - Applied STATUS1 review round 1:
    - Flattened `HostEntry` instead of nesting an optional `Host`, making
      online state explicit and avoiding duplicate id/name consistency checks.
    - Renamed client host stream upserts from `HostAdded` to `HostUpdated`;
      trusted route loss now emits an offline update instead of removing the
      host from subscribers.
    - Kept non-agent-capable non-relay hosts visible for cloud pairing while
      still rejecting them as remote `CreateAgent` targets.
    - Published host status updates after pairing trust commits so subscribers
      see untrusted-to-trusted transitions.
    - Tightened §8.12 to define visibility and reachability derivation.
  - Verification after review round 1: `cargo fmt`; `cargo check -p amux
    --tests`; `cargo test -p amux services::client:: -- --nocapture`; `cargo
    test -p amux-cli -- --nocapture`; `cargo test -p amux services::startup::
    -- --nocapture`; `cargo test -p amux connection:: -- --nocapture`; `cargo
    test -p amux services::reachability:: -- --nocapture`.
  - Applied STATUS1 review round 2:
    - Moved pairing candidate selection behind `ListHostsRequest.scope` so the
      server owns cloud-route and local-only untrusted discovery policy.
    - Changed wire reachability status to an explicit `oneof`, and added
      public-client invariant checks for online/offline/trusted rows.
    - Published host status updates when cached reachability errors change.
    - Classified cloud pairing routes from stable `LinkRole::CloudRelay`
      metadata instead of spoofable advertised peer capabilities.
    - Filtered remote `ListHosts` / `SubscribeHosts` inventory so trusted
      remote callers cannot enumerate untrusted online hosts.
  - Verification after STATUS1 review round 2: `cargo fmt`; `cargo check -p
    amux --tests`; `cargo test -p amux services::client:: -- --nocapture`;
    `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux
    services::startup:: -- --nocapture`; `cargo test -p amux-cli --
    --nocapture`; `cargo test -p amux protocol:: -- --nocapture`; `cargo test
    -p amux services::reachability:: -- --nocapture`; `cargo fmt --check`;
    `git diff --check`.
  - Final workspace compile pass found the UI inventory crate still matching the
    old `HostAdded` client event and old raw `Host` type. Updated
    `amux-ui` to consume `HostUpdated` events and expose `HostEntry` rows
    through its notification surface. Verification: `cargo check --workspace
    --all-targets`; `cargo test -p amux-ui -- --nocapture`.
- Started OBS1:
  - Added `audit.rs` as the single source for §10 audit category constants and
    structured tracing emitters under the `amux::audit` target.
  - Wired pairing lifecycle audits across QR/token, SPAKE2/PIN, SSH, direct
    PIN, cloud PIN/QR, local pair-mode start/cancel, pair-mode expiry, and
    peer/protocol abort paths.
  - Wired authentication failure audits for pinned device mTLS verification,
    dispatcher TLS accept failures and timeouts, cloud routing JWT metadata
    failures, reauth failures, and cloud credential rejection.
  - Wired committed trust update audits for pairing insert/update/replace,
    Link up/down audits in the Link registry, and disruptive local/remote
    client-service call audits for delete, shutdown, suspend, and resume.
  - Applied OBS1 review round 1:
    - Removed duplicated trust-audit call sites by emitting from the commit
      guard after durable trust commits.
    - Collapsed repeated category strings behind `audit.rs` constants and a
      spec-matching category test.
    - Moved JWT failure auditing to request/reauth boundaries to avoid
      duplicate internal authenticator logs.
    - Added first-pass missed audit paths for pairing failures and cloud
      credential failures.
  - Applied OBS1 review round 2:
    - Added acceptor-side JWT expiry/reauth failure audits.
    - Added direct PIN and SSH helper start/failure audits before daemon-side
      trust commit RPCs are reached.
    - Added dispatcher TLS accept timeout/protocol/signature failure audits,
      with duplicate suppression when the pinned verifier already emitted the
      host-specific mTLS failure.
    - Added pair-mode expiry as `pairing.failure`, made cancel auditing
      conditional on an active session, and sanitized remote-controlled
      `PairingError` details before audit logging.
  - Verification after OBS1 review round 2: `cargo check -p amux --tests`;
    `cargo test -p amux pairing:: -- --nocapture`; `cargo test -p amux
    services::pairing:: -- --nocapture`; `cargo test -p amux pin_pairing:: --
    --nocapture`; `cargo test -p amux ssh_pairing:: -- --nocapture`; `cargo
    test -p amux dispatcher:: -- --nocapture`; `cargo test -p amux
    routing::connect:: -- --nocapture`; `cargo test -p amux
    services::client:: -- --nocapture`; `cargo test -p amux
    services::startup:: -- --nocapture`.
  - Final gpt-5.5 xhigh spec-discrepancy audit found no OBS1 category
    mismatch, but did find two adjacent source-of-truth issues:
    - Remote host-list subscribers could cause hidden untrusted hosts to be
      marked as client-visible before per-subscriber filtering. Moved
      client-visible marking to filtered `ListHosts` / `SubscribeHosts`
      delivery points and added
      `remote_subscriber_hidden_untrusted_live_event_does_not_mark_visible_activity`.
    - §5.2.1 described the initiator sending `pairing_complete`; aligned the
      spec with the implemented proto/code where the responder commits and
      sends completion before the initiator commits.
  - Verification after final audit fixes: `cargo test -p amux
    remote_subscriber_hidden_untrusted_live_event_does_not_mark_visible_activity
    -- --nocapture`; `cargo test -p amux services::client:: -- --nocapture`;
    `cargo test -p amux services::pairing:: -- --nocapture`; `cargo test -p
    amux routing::core:: -- --nocapture`.
  - Final gpt-5.5 xhigh follow-up audit rechecked the fixed STATUS1/RESOURCE1
    client-visible marking and §5.2.1 SPAKE2 completion-order text and found
    no remaining actionable discrepancies in the touched STATUS1/OBS1 areas.
- Started AUDIT after CLI1:
  - Added a matrix mapping explicit invariants, implementation defaults, and
    CLI/source-of-truth cleanup to code evidence.
  - First audit result is not complete; remaining work is tracked as
    CLOUDTLS1, WIRELIMITS1, DRAIN1, SWAP1, QRINIT1, RESOURCE1,
    STATUS1, and OBS1.
  - Applied AUDIT review round 1 by simplifying contradictory source-spec
    text around cloud pubkey visibility, WebPKI roots, Unix socket topology,
    and on-disk path/mode defaults; added missed gaps for routing host-name
    bounds and GoAway drain behavior.
  - Applied AUDIT review round 2 by correcting the remaining cloud-root-store
    wording and splitting GoAway drain behavior out of wire limits into DRAIN1.
  - Applied AUDIT bug/security review round 2 by adding SWAP1 for old-route
    tunnel teardown, QRINIT1 for the missing QR/token initiator path, the full
    local-only pairing admin RPC surface to N-S-2, and the cloud-auth defaults
    row to the audit matrix.
- Started WIRELIMITS1:
  - Added the routing `Host.name` 256-byte validation cap to inbound host
    validation.
  - Added `ROUTE_HOP_CAP` and drop-only handling for inbound `HostUp` /
    `HostDown` routing events that would exceed the cap after prepending the
    incoming Link.
  - Added `TUNNEL_FRAME_PAYLOAD_MAX` enforcement for inbound tunnel frames;
    oversized payloads return a tunnel-pool protocol error, which the routing
    Connect loop converts to `GoAway { PROTOCOL_ERROR }` and Link closure.
  - Verification: `cargo test -p amux oversized -- --nocapture`; `cargo test
    -p amux inbound_route_over_hop_cap_is_dropped_marker -- --nocapture`;
    `cargo test -p amux routing:: -- --nocapture`; `cargo check -p amux
    --tests`; `cargo fmt --check`; `git diff --check`.
  - Applied WIRELIMITS1 review round 1:
    - Config validation now rejects local `host_name` values over 256 bytes so
      the daemon cannot advertise non-compliant local routing hosts.
    - Inbound routing events drop over-hop routes before parsing downstream
      link names, preserving the spec's drop-only behavior for over-cap
      advertisements.
    - Inbound `HostUp` semantic validation now lives in wire decoding instead
      of being split between decoding and the Connect loop.
    - Added a Connect-level oversized `TunnelFrame.payload` regression test
      that asserts `GoAway { PROTOCOL_ERROR }` and Link route cleanup.
  - Verification after review round 1: `cargo test -p amux oversized --
    --nocapture`; `cargo test -p amux
    inbound_route_over_hop_cap_is_dropped_marker -- --nocapture`; `cargo test
    -p amux oversized_tunnel_frame_sends_protocol_goaway_and_cleans_link --
    --nocapture`; `cargo test -p amux routing:: -- --nocapture`; `cargo test
    -p amux config:: -- --nocapture`; `cargo check -p amux --tests`; `cargo
    fmt --check`; `git diff --check`.
  - Applied WIRELIMITS1 review round 2:
    - `HostUp` over-hop detection now happens before host decoding/semantic
      validation, so over-cap routes are drop-only even when the embedded host
      is invalid.
    - Post-handshake request-stream decode/resource errors now send
      `GoAway { PROTOCOL_ERROR }` before Link cleanup instead of silently
      breaking the Connect loop.
  - Verification after review round 2: `cargo test -p amux oversized --
    --nocapture`; `cargo test -p amux
    inbound_route_over_hop_cap_is_dropped_marker -- --nocapture`; `cargo test
    -p amux post_handshake_stream_error_sends_protocol_goaway_and_cleans_link
    -- --nocapture`; `cargo test -p amux routing:: -- --nocapture`; `cargo
    test -p amux config:: -- --nocapture`; `cargo check -p amux --tests`;
    `cargo fmt --check`; `git diff --check`.
- Started DRAIN1:
  - `ConnectionManager::channel_to` now verifies the active route's first hop
    through `LinkRegistry::outgoing_tx` before returning a cached route
    Channel.
  - Cached multi-hop and cached one-hop routes now return `LinkDraining` while
    their first hop is draining, rather than allowing new call starts on a
    cached `Channel`.
  - Existing direct-channel tests now register the corresponding Link writer,
    matching the real routing Connect path that installs one-hop Channels.
  - Verification: `cargo test -p amux connection:: -- --nocapture`; `cargo
    test -p amux
    inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing --
    --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git
    diff --check`.
  - Applied DRAIN1 review round 1:
    - Removed the duplicate new-materialization first-hop precheck; the drain
      guard is now scoped to returning cached route Channels, while
      `TunnelPool` still owns new tunnel materialization checks.
    - `LinkRegistry::send_goaway_to_all` now marks each writer draining before
      sending local GoAway, so local shutdown/suspend refuses new cached calls
      during the drain window.
    - Added a real routing Connect regression where a shared
      `ConnectionManager` refuses a cached one-hop Channel after receiving
      GoAway over the Connect stream.
  - Verification after DRAIN1 review round 1: `cargo test -p amux connection::
    -- --nocapture`; `cargo test -p amux
    send_goaway_marks_links_draining_before_notifying -- --nocapture`; `cargo
    test -p amux connector_manager_rejects_cached_channel_after_inbound_goaway
    -- --nocapture`; `cargo test -p amux
    inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing --
    --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git
    diff --check`.
  - Applied DRAIN1 review round 2:
    - Moved the cached-channel drain guard to the shared `materialize`
      cache-hit path, covering active and pre-active cached route Channels.
    - Added a pre-active cached direct-channel regression so a route registered
      in `ConnectionPool` before first `ConnectionManager::channel_to` cannot
      bypass drain.
  - Verification after DRAIN1 review round 2: `cargo test -p amux connection::
    -- --nocapture`; `cargo test -p amux
    channel_to_rejects_pre_active_cached_channel_on_draining_link --
    --nocapture`; `cargo test -p amux
    connector_manager_rejects_cached_channel_after_inbound_goaway --
    --nocapture`; `cargo test -p amux
    send_goaway_marks_links_draining_before_notifying -- --nocapture`; `cargo
    test -p amux
    inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing --
    --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git
    diff --check`.
- Started SWAP1:
  - Added a shared `ConnectionManager::remove_route_runtime_state` path that
    unregisters the route-keyed `ConnectionPool` entry and removes matching
    `TunnelPool` tunnels.
  - Route cleanup now uses that path for make-then-break swaps, `HostDown`,
    stale materialization cleanup, and host teardown.
  - Added regressions for shorter-route swaps and route-specific `HostDown`
    tearing down old route tunnels.
  - Verification: `cargo test -p amux swapping_routes_drops_old_route_tunnels
    -- --nocapture`; `cargo test -p amux host_down_drops_route_tunnels --
    --nocapture`; `cargo test -p amux connection:: -- --nocapture`; `cargo
    test -p amux tunnel::pool:: -- --nocapture`; `cargo check -p amux
    --tests`; `cargo fmt --check`; `git diff --check`.
  - Applied SWAP1 review round 1:
    - Centralized route runtime cleanup in `ConnectionManager` by removing the
      duplicate lower-level tunnel cleanup from routing `Connect` paths.
    - Removed stale `active_removed` bookkeeping from the route-down path.
    - Tombstoned removed `TunnelId`s so late frames cannot recreate endpoint
      tunnels that were removed by route swap or route removal.
  - Verification after SWAP1 review round 1: `cargo test -p amux
    removed_route_tombstones_tunnel_ids -- --nocapture`; `cargo test -p amux
    swapping_routes_drops_old_route_tunnels -- --nocapture`; `cargo test -p
    amux host_down_drops_route_tunnels -- --nocapture`; `cargo test -p amux
    connection:: -- --nocapture`; `cargo test -p amux tunnel::pool:: --
    --nocapture`; `cargo test -p amux routing::connect:: -- --nocapture`;
    `cargo check -p amux --tests`; `cargo fmt --check`; `git diff --check`.
  - Applied SWAP1 review round 2:
    - Replaced optional/fallback routing Connect pools with a required shared
      route runtime dependency so direct-channel registration and route cleanup
      cannot silently use different `ConnectionPool`s.
    - Made inbound `HostDown` and Link cleanup synchronously remove route
      runtime state instead of waiting for the async `ConnectionManager`
      routing-event subscriber.
    - Added `RoutingCore::remove_host_routes` and used it during host
      replacement teardown/finish so stale multi-hop routes cannot survive in
      the core routing table while manager state is cleared.
    - Bounded retired `TunnelId` tombstones and exposed the count in tunnel-pool
      tests.
  - Verification after SWAP1 review round 2: `cargo test -p amux
    retired_tunnel_tombstones_are_bounded -- --nocapture`; `cargo test -p
    amux removed_route_tombstones_tunnel_ids -- --nocapture`; `cargo test -p
    amux teardown_host_removes_routes_channels_tunnels_and_links --
    --nocapture`; `cargo test -p amux swapping_routes_drops_old_route_tunnels
    -- --nocapture`; `cargo test -p amux host_down_drops_route_tunnels --
    --nocapture`; `cargo test -p amux
    connector_cleans_link_routes_when_input_stream_closes -- --nocapture`;
    `cargo test -p amux connection:: -- --nocapture`; `cargo test -p amux
    tunnel::pool:: -- --nocapture`; `cargo test -p amux routing::connect:: --
    --nocapture`; `cargo check -p amux --tests`; `cargo fmt --check`; `git
    diff --check`.
- Started CLOUDTLS1:
  - Removed the debug/test `http://` cloud routing bypass; cloud routing
    channels now always go through `tls_channel`.
  - Removed the now-unused plaintext `tcp_channel` helper/export.
  - Added a debug/test `AMUX_CLOUD_TLS_CA` root hook for the cloud TLS client
    path so local e2e can trust a TLS fixture without disabling certificate
    verification.
  - Updated the cloud relay e2e fixture to write a localhost routing TLS
    certificate/key, pass `AMUX_TLS_CERT`/`AMUX_TLS_KEY` to the cloud relay,
    pass the fixture CA to cloud-enabled daemons, run the cloud relay in
    foreground mode, and pair cloud-routed online daemons before exercising trusted
    agent sharing over cloud routing.
  - Verification before review round 1: `cargo test -p amux
    services::startup::cloud:: -- --nocapture`; `cargo test -p e2e-runner --
    --nocapture`; `cargo run -p e2e-runner -- run cloud_relay_connection`.
  - Applied CLOUDTLS1 review round 1:
    - Reconciled stale audit rows so the ledger distinguishes the now-TLS
      `RoutingService` transport from the local fake HTTP cloud API used only
      for OAuth/JWKS/connect metadata in e2e tests.
    - Bug/security review found no issues and independently re-ran the focused
      cloud verification commands.
  - Applied CLOUDTLS1 review round 2:
    - Reconciled the remaining stale CLI1/audit rows so the old
      `cloud_relay_connection` TLS fixture failure is recorded as resolved.
    - Changed cloud TLS listener accept handling from serial handshakes to
      bounded concurrent handshakes, so one slow TCP client cannot block later
      cloud routing connections.
    - Added regression coverage that stalls one cloud TLS handshake and proves
      a second socket can still complete the TLS handshake.
  - Verification after CLOUDTLS1 review round 2: `cargo test -p amux
    services::startup:: -- --nocapture`; `cargo test -p e2e-runner --
    --nocapture`; `cargo run -p e2e-runner -- run cloud_relay_connection`;
    `cargo check -p amux --tests`; `cargo check -p e2e-runner --tests`;
    `cargo fmt --check`; `git diff --check`.
- Started QRINIT1:
  - Added `ClientService.PairQrCloudPeer` so the local daemon, which owns the
    routing graph, can initiate QR pairing over the cloud route.
  - Added a QR-pubkey-pinned pairing TLS client path for tunnel transports;
    PIN pairing keeps its no-server-verification TLS path because SPAKE2 is
    the PIN flow authenticator.
  - Added `pair_by_token_initiator`, which calls `PairByToken`, verifies the
    responder identity matches the QR payload, and returns the responder
    identity for initiator-side trust commit.
  - Added `amux pair --qr <payload>` for QR JSON consumption while keeping
    bare `amux pair --qr` as the responder QR display flow.
  - Updated the networking spec CLI table with the QR initiator command.
  - Verification before QRINIT1 review round 1: `cargo check -p amux
    --tests`; `cargo test -p amux
    descriptor_set_contains_core_protocol_messages_and_services --
    --nocapture`; `cargo test -p amux
    tonic_pairing_admin_rpcs_reject_non_local_callers -- --nocapture`;
    `cargo test -p amux
    tonic_pair_qr_cloud_peer_rejects_self_identity_before_dialing --
    --nocapture`; `cargo test -p amux
    cloud_qr_pairing_updates_both_trust_stores_and_pins_responder_pubkey --
    --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`;
    `cargo test -p amux services::startup:: -- --nocapture`; `cargo test -p
    amux tunnel::pool:: -- --nocapture`; `cargo test -p amux-cli --
    --nocapture`.
  - Applied QRINIT1 review round 1:
    - Moved QR payload parsing and cloud URL validation into the `amux`
      library so CLI and non-CLI callers share one payload contract.
    - Restricted `PairByToken` to cloud pre-trust pairing ingress, matching the
      QR/token flow instead of creating an undocumented direct-token path.
    - Added daemon-side QR preflight for token length and duplicate trusted
      pubkey before opening the pinned QR tunnel.
    - Added a pairing completion message and tunnel-preserving host
      replacement so PIN/QR cloud re-pairing can rotate an existing host key
      without tearing down its own response path.
  - Verification after QRINIT1 review round 1: `cargo check -p amux --tests`;
    `cargo check -p amux-cli --tests`; `cargo check -p e2e-runner --tests`;
    `cargo test -p amux
    descriptor_set_contains_core_protocol_messages_and_services --
    --nocapture`; `cargo test -p amux
    cloud_pin_pairing_updates_both_trust_stores -- --nocapture`; `cargo test
    -p amux
    cloud_qr_pairing_updates_both_trust_stores_and_pins_responder_pubkey --
    --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`;
    `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p
    amux services::startup:: -- --nocapture`; `cargo test -p amux
    tunnel::pool:: -- --nocapture`; `cargo test -p amux qr_pairing_payload
    -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo test
    -p e2e-runner -- --nocapture`.
  - Applied QRINIT1 review round 2:
    - Documented `PairingComplete` in the source networking spec and kept the
      initiator trust commit behind the responder's post-commit completion.
    - Stopped classifying every tunnel as cloud pairing ingress. Inbound
      endpoint tunnels now carry cloud pairing reachability only when the
      origin Link is a cloud-relay route; non-cloud tunneled `PairByToken`
      calls are rejected before token consumption or trust writes.
    - Retired tunnel IDs when endpoint transports drop, including tunnels that
      were preserved long enough to return a key-replacement pairing response.
    - Strengthened the QR cloud replacement regression by seeding responder
      trust with the scanner's old pubkey and asserting replacement to the
      current pubkey/name/reachability.
    - Fixed QR client decode errors to report the QR RPC method name rather
      than the PIN RPC method name.
  - Verification after QRINIT1 review round 2: `cargo check -p amux --tests`;
    `cargo check -p amux-cli --tests`; `cargo check -p e2e-runner --tests`;
    `cargo test -p amux -- --nocapture`; `cargo test -p amux
    descriptor_set_contains_core_protocol_messages_and_services --
    --nocapture`; `cargo test -p amux dispatcher:: -- --nocapture`; `cargo
    test -p amux routing::connect:: -- --nocapture`; `cargo test -p amux
    cloud_pin_pairing_updates_both_trust_stores -- --nocapture`; `cargo test
    -p amux
    cloud_qr_pairing_updates_both_trust_stores_and_pins_responder_pubkey --
    --nocapture`; `cargo test -p amux services::pairing:: -- --nocapture`;
    `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p
    amux services::startup:: -- --nocapture`; `cargo test -p amux
    tunnel::pool:: -- --nocapture`; `cargo test -p amux qr_pairing_payload
    -- --nocapture`; `cargo test -p amux-cli -- --nocapture`; `cargo test
    -p e2e-runner -- --nocapture`; `cargo fmt --check`; `git diff --check`.
- Started PAIR1 by splitting the original broad pairing checkpoint into
  reviewable subcheckpoints: PAIR1A foundation, PAIR1B QR/token service, PAIR1C
  PIN/SPAKE2 service, PAIR1D SSH transport helpers, and PAIR1E user-facing CLI.
- Started PAIR1D SSH helpers:
  - Added a length-prefixed protobuf `PairingIdentity` exchange over generic
    SSH stdin/stdout streams.
  - Initiators persist `Reachability::Ssh { target }`; responders persist the
    trusted peer without outbound reachability because an incoming SSH session
    does not reveal a reusable target for dialing back.
  - Added a `ssh <target> amux relay` stdio transport primitive and a
    Unix-socket stdio relay helper for later CLI/runtime wiring.
  - Added daemon-backed `ClientService.PairPeer` so SSH pairing commits go
    through the live trust store and existing teardown/replacement path instead
    of mutating `trust.json` behind a running daemon's back.
  - Added hidden `amux pair-recv` and `amux relay` commands for the SSH
    responder and runtime helper paths.
  - Added a shared trust-commit lock for QR/PIN/SSH commits so concurrent
    pairings cannot overwrite staged trust snapshots.
  - Initially split SSH relay runtime ingress onto a dedicated Unix socket; this
    was later simplified in SPECSIM1 so `amux relay` uses the normal local Unix
    socket.
  - Removed the test-only direct trust-file SSH commit path; SSH helpers now
    exercise the length-prefixed stdio exchange, while durable trust commits go
    through `ClientService`.
  - Made `ClientService` pairing trust access required for device-local
    service construction, matching the daemon topology instead of carrying an
    optional production path.
  - Documented that SSH pairing is not a distributed atomic transaction:
    a process/host failure after one local commit can leave one-sided trust
    until revocation/trust removal is implemented.
- Started PAIR1E user-facing CLI flows:
  - Added local `ClientService` admin RPCs for starting PIN/QR pair-mode,
    polling pair-mode status, and cancelling pair-mode.
  - Added public client wrappers for starting PIN/QR responder mode and
    checking whether the pairing window is still active.
  - Added visible `amux pair` command handling for bare PIN responder mode,
    `--qr`, `--listen` config validation, and Unix `--via-ssh <target>`.
  - Added an SSH pair initiator helper that spawns
    `ssh -T -o BatchMode=yes -- <target> amux pair-recv`.
  - Left `amux pair --connect [target]` as an explicit not-implemented branch
    for the next PAIR1E slice, because the PIN initiator transport/client flow
    is not wired yet.
  - Applied PAIR1E first-review fixes:
    - `StartPairingResponse` now reuses `PairingIdentity` and returns daemon
      runtime metadata (`tcp_port`, `cloud_url`) so CLI output describes the
      running daemon rather than a possibly stale local config.
    - `StartPairingRequest` can require LAN-direct responder mode; the daemon
      rejects that before arming pair-mode if its runtime config has no
      `tcp_port`.
    - QR responder mode is rejected before arming pair-mode unless cloud mode
      is enabled in the daemon runtime config.
    - Start-pairing validates the local name against the pairing wire bound
      before arming, avoiding PIN/QR windows that cannot complete.
    - `amux pair` cancels active pair-mode on Ctrl-C through the local daemon
      `CancelPairing` RPC.
    - Local-admin RPC rejection messages are generic across Start/Get/Cancel
      pairing and SSH trust commit.
  - Verification for PAIR1E first-review slice: `cargo test -p amux
    tonic_start_pairing`; `cargo test -p amux
    tonic_pairing_admin_rpcs_reject_non_local_callers`; `cargo test -p amux
    pairing_start_response`; `cargo test -p amux ssh_pair_recv_args`; `cargo
    test -p amux protocol::`; `cargo test -p amux-cli`; `cargo check
    --workspace --all-targets`; `git diff --check`.
  - Implemented direct TCP PIN initiator pairing for
    `amux pair --connect <ip:port>`:
    - Added a PIN-pairing TLS client channel that deliberately skips server
      certificate verification, matching N-P-4 because SPAKE2 authenticates the
      peer inside the encrypted transport.
    - Added the SPAKE2 initiator helper using the same transcript, key
      confirmation, and AEAD identity exchange as the responder tests.
    - Added local `ClientService.PairPeer` to commit initiator-side trust
      with `Reachability::DirectTcp { addr }` through the live daemon trust
      path.
    - Wired the CLI direct-target branch to prompt for a PIN, run the SPAKE2
      initiator, and commit the direct reachability. Cloud name lookup and the
      omitted-target picker remain unimplemented.
  - Applied direct TCP PIN review fixes:
    - Direct TCP listener ingress now tags pre-trust pairing with no reusable
      reachability, so responders do not persist the initiator's ephemeral TCP
      source port as `Reachability::DirectTcp`.
    - Narrowed pre-trust pairing ingress metadata to `Cloud` or
      `NoReusableReachability`, removing the ability for tests or miswired
      callers to inject arbitrary trust reachability into `PairingService`.
    - `PairingService` now requires explicit pre-trust pairing transport
      metadata instead of defaulting missing or miswired metadata to cloud
      reachability.
    - SPAKE2 initiators preserve peer-sent `PairingError` reasons instead of
      collapsing every peer abort into `INVALID_PIN`.
    - Collapsed local daemon trust mutation from separate SSH/direct RPCs into
      one local-admin `ClientService.PairPeer` operation with optional
      reachability.
    - Removed the unused cloud reachability branch from `PairPeer`; cloud-name
      initiator pairing is still not implemented in this slice.
    - Split `amux pair --connect` target parsing into direct TCP, cloud-name,
      and omitted-target cases; the latter two remain explicit not-implemented
      branches.
    - Updated the source spec to clarify that direct TCP responders do not
      store accepted peer socket addresses as reusable reachability; only the
      direct dialer stores the listener address it actually dialed.
    - Added an integration-style direct PIN test over a real TCP dispatcher
      listener covering the no-verify TLS channel, SPAKE2 initiator, local
      `PairPeer` commit, initiator direct reachability, and responder empty
      reachability.
    - Kept pair-time runtime Link establishment as R1 work; this PAIR1E slice
      commits trust and direct reachability but does not yet register a live
      `RoutingService.Connect` Link after direct PIN pairing.
  - Verification for direct TCP PIN slice: `cargo test -p amux
    services::pairing::`; `cargo test -p amux
    dispatches_direct_pairing_without_reusable_reachability`; `cargo test -p
    amux services::client::`; `cargo test -p amux protocol::`; `cargo test -p
    amux ssh_pairing::`; `cargo test -p amux
    direct_pin_pairing_over_tcp_updates_both_trust_stores`; `cargo test -p
    amux-cli`; `cargo check --workspace --all-targets`; `cargo test -p amux`;
    `git diff --check`.
  - Implemented terminal QR rendering for `amux pair --qr`:
    - Added the `qrcode` CLI dependency without image/default features.
    - `amux pair --qr` now renders the existing JSON pairing payload as a
      terminal QR code without printing the one-shot token JSON into terminal
      scrollback.
    - QR display failures cancel the active pair-mode before returning the
      display error to the user.
    - The QR payload now matches the source spec fields: `host_id`, `pubkey`,
      `cloud_url`, and `one_shot_token`; the responder name is returned later
      by `PairByTokenResponse`.
    - Added CLI coverage that validates QR payload generation and terminal QR
      rendering.
  - Verification for QR rendering slice: `cargo test -p amux-cli
    qr_pairing_output_renders_terminal_code_for_payload`.
  - Implemented cloud-routed PIN initiator pairing for
    `amux pair --connect [target]`:
    - Added local-admin `ClientService.PairPinCloudPeer` and public client
      wrapper so the local daemon resolves a cloud route, opens a pre-trust
      PIN-pairing tunnel, runs the SPAKE2 initiator, validates the returned
      identity matches the requested `host_id`, and commits
      `Reachability::Cloud`.
    - Added a no-client-auth PIN-pairing TLS channel over `TunnelTransport`,
      routed by `ConnectionManager`/`TunnelPool` without requiring preexisting
      device trust.
    - Wired `amux pair --connect <name>` to exact cloud host lookup (also
      accepting `host_id`) and bare `amux pair --connect` to a numbered
      picker over the daemon's host inventory.
    - Added an integration test with two initially untrusted daemons connected
      through the cloud routing service; pairing over the routed tunnel updates
      both trust stores with `Reachability::Cloud`.
    - Added CLI coverage for exact/ambiguous/missing cloud lookup and picker
      index parsing, plus client-service guardrails for local-admin rejection
      and self-pair rejection on the cloud PIN RPC.
  - Verification for cloud-routed PIN slice so far: `cargo check --workspace
    --all-targets`; `cargo test -p amux
    services::startup::tests::cloud_pin_pairing_updates_both_trust_stores`;
    `cargo test -p amux services::client::tests::tonic_pair`; `cargo test -p
    amux-cli cloud_pairing`.
  - Applied first cloud PIN review fixes:
    - Cloud PIN pairing now selects only routes whose first hop is an
      explicitly advertised cloud relay link before committing
      `Reachability::Cloud`; it no longer uses the generic shortest-route
      selector.
    - Added timeouts for the pre-trust PIN-pairing tunnel TLS handshake and
      the SPAKE2 initiator attempt, with cleanup coverage for provisional
      tunnels.
    - `ListHostsRequest` can exclude the local host; `amux pair --connect`
      cloud-name and picker paths use that self-filtered host inventory.
    - Public pairing identity decoding now validates pubkey/name bounds.
  - Verification for first cloud PIN review fixes: `cargo test -p amux
    connection::`; `cargo test -p amux tunnel::pool::`; `cargo test -p amux
    services::pairing::tests::spake2_initiator_timeout_returns_pairing_timeout`;
    `cargo test -p amux
    services::client::tests::tonic_list_hosts_can_exclude_local_host_for_pairing_selection`;
    `cargo test -p amux
    services::startup::tests::cloud_pin_pairing_updates_both_trust_stores`;
    `cargo test -p amux-cli -- --nocapture`; `cargo test -p amux
    services::client:: -- --nocapture`; `cargo test -p amux
    services::pairing:: -- --nocapture`.
  - Applied second cloud PIN review fixes:
    - The cloud pairing picker/name inventory now uses the same
      cloud-routable route predicate as `PairPinCloudPeer`, so the CLI does
      not show non-cloud-routable remote hosts as cloud pairing targets.
    - The bounded SPAKE2 initiator helper is now the default exported helper;
      call sites no longer choose between bounded and unbounded pairing.
    - SPAKE2 responders now have a timeout too, so idle pre-trust streams
      release in-flight PIN attempt slots before pair-mode TTL expiry.
  - Verification for second cloud PIN review fixes: `cargo check --workspace
    --all-targets`; `cargo test -p amux
    services::client::tests::tonic_list_hosts_cloud_routable_filter_matches_connection_manager`;
    `cargo test -p amux services::pairing::tests:: -- --nocapture`.
  - Final PAIR1E verification: `cargo test -p amux-cli -- --nocapture`;
    `cargo test -p amux services::client:: -- --nocapture`; `cargo test -p
    amux services::startup:: -- --nocapture`; `cargo test -p amux
    tunnel::pool:: -- --nocapture`; `cargo test -p amux -- --nocapture`;
    `cargo check --workspace --all-targets`; `git diff --check`. Two
    simplification and two bug/security review rounds completed.
- Started R1 runtime reachability Links:
  - Added a `ReachabilityLinkConnector` that snapshots trust-store
    `DirectTcp`/`Ssh` reachabilities, ignores `Cloud`, and starts outbound
    `RoutingService.Connect` Links with expected-peer validation.
  - Added a trusted device TCP+mTLS channel helper for `DirectTcp { addr }`
    runtime Links.
  - Device startup now spawns direct reachability Link attempts after the
    cloud attach task is scheduled; failed attempts are logged and non-fatal.
  - `ClientService.PairPeer` now triggers the same Link connector after
    pair-time direct/SSH trust commits, so direct PIN initiators get a runtime
    route without waiting for restart.
  - Direct TCP focused verification covers persisted startup reachability,
    pair-time direct PIN Link establishment, and using the registered 1-hop
    `ConnectionPool` channel for a Trusted Server RPC.
  - Applied first R1 review fixes:
    - Pair-time Link tasks are retained by the reachability connector and
      aborted when the connector is dropped, avoiding detached runtime tasks.
    - Routing Link cleanup no longer unregisters the 1-hop direct channel
      before emitting the route's `HostDown`, avoiding a transient
      route-known/channel-missing window.
    - `RoutingService.Connect` acceptors now time out idle streams before the
      initial `Hello`, and cloud attach waits for bounded routing
      establishment before retrying.
  - Applied second R1 review fixes:
    - Polished the connector-side expected-peer protocol error message.
    - Added DirectTcp teardown coverage that closes the local Link, waits for
      the `HostDown` cleanup, and verifies `channel_to` no longer returns the
      stale one-hop route.
    - Deferred periodic DirectTcp retry/backoff because §8.8 makes retry an
      implementation detail, not a v1 requirement; failed startup attempts
      remain non-fatal and pair-time attempts are retriggered by re-pairing.
    - Confirmed that SSH runtime reachability follows the documented
      SSH-trust model in §3.1/§4.4 rather than amux pubkey pinning, but left
      SSH integration verification for the next R1 slice.
    - Added SSH relay runtime integration coverage using the same raw
      Trusted Server ingress that `ssh <target> amux relay` feeds; the route
      is established through `RoutingService.Connect` and then used for a
      `ClientService` RPC.
    - Added bidirectional DirectTcp coverage proving that peers with mutual
      direct reachabilities each establish their own outbound one-hop Link.
    - Gated cloud attach task creation when cloud mode is disabled and removed
      stale dead-code allowances from now-live R1 helpers.
    - Final focused bug/security re-review found no remaining blocker; SSH
      runtime reachability remains intentionally bound to the documented
      SSH-trust model.
  - Verification so far: `cargo test -p amux services::reachability:: --
    --nocapture`; `cargo test -p amux services::startup::tests::direct_ --
    --nocapture`; `cargo test -p amux
    services::startup::tests::direct_tcp_reachabilities_on_both_peers_establish_two_outbound_links
    -- --nocapture`; `cargo test -p amux
    services::startup::tests::ssh_relay_runtime_link_establishes_route_over_trusted_ingress
    -- --nocapture`; `cargo test -p amux
    services::client::tests::tonic_pair_ -- --nocapture`; `cargo test -p
    amux routing::connect:: -- --nocapture`; `cargo test -p amux
    services::startup:: -- --nocapture`; `cargo test -p amux
    services::client:: -- --nocapture`; `cargo test -p amux transport::ssh::
    -- --nocapture`; `cargo check -p amux --tests`; `git diff --check`.
- Started CLI1:
  - Removed the legacy manual `amux server connect <host:port>` command.
  - Removed public `Client::connect_to_server`, the local `ClientService`
    `ConnectToServer` RPC, its disabled implementation, and the proto messages.
  - Routing is now implicit through trust-store reachabilities established by
    `amux pair` flows and materialized by R1 runtime Links.
  - Verification so far: `cargo check -p amux --tests`; `cargo test -p
    amux-cli -- --nocapture`; `cargo test -p amux protocol:: -- --nocapture`.
- Applied CLI1 round 1 review fixes:
  - Removed stale `server connect` use from remote e2e transcripts by pairing
    test daemons with `amux pair --listen` and `amux pair --connect
    127.0.0.1:$server_a.tcp_port`.
  - Added an e2e `@@capture` directive for generated PINs, shortened Unix
    socket paths on macOS, and gave each e2e config an isolated
    `XDG_DATA_HOME` plus silent pre-init so paired test daemons do not share a
    device identity.
  - Updated top-level help expectations for the now-visible `pair` command and
    simplified the `server` help text to lifecycle-only.
  - Tightened descriptor coverage to assert the exact `ClientService` method
    set, preventing accidental reintroduction of `ConnectToServer`.
  - Updated `docs/NETWORKING.md` wording for LAN listener output and legacy
    `amux server connect`; updated `docs/cloud_architecture.md` to point at the
    networking spec and remove the plaintext bypass wording.
  - Verification: `cargo test -p e2e-runner -- --nocapture`; `cargo test -p
    amux protocol:: -- --nocapture`; `cargo test -p amux-cli -- --nocapture`;
    `cargo run -p e2e-runner -- run remote`; `cargo run -p e2e-runner -- run
    bare_help`; `cargo check --workspace --all-targets`; `cargo fmt --check`;
    `git diff --check`.
  - Full e2e suite note from CLI1: `cloud_relay_connection` was still outside
    CLI1 at that point because the fixture expected a non-TLS local cloud
    relay while current cloud relay startup required TLS cert/key env vars.
    CLOUDTLS1 later resolved that fixture and the focused
    `cloud_relay_connection` run now passes.
- Applied CLI1 round 2 review fixes:
  - Replaced stale `docs/NEW_ARCHITECTURE.md` contents with a redirect to
    `docs/NETWORKING.md` so the removed `ClientService.ConnectToServer` design
    is no longer presented as current.
  - Made remote/cloud e2e `tcp_port` use explicit (`tcp_port: 0`) and stopped
    the runner from silently adding TCP listeners to every generated config.
  - Isolated e2e logs and default state paths by setting per-config
    `XDG_STATE_HOME` and `AMUX_LOG`, alongside the existing per-config
    `XDG_DATA_HOME`.
  - Removed the stale `enforce_tls_in_cloud_mode` example from deployment docs.
  - Verification: `cargo test -p e2e-runner -- --nocapture`; `cargo run -p
    e2e-runner -- run remote`; `cargo run -p e2e-runner -- run bare_help`;
    `cargo test -p amux protocol:: -- --nocapture`; `cargo test -p amux-cli
    -- --nocapture`; `cargo check --workspace --all-targets`; `cargo fmt
    --check`; `git diff --check`.
- Implemented PAIR1A foundation:
  - Replaced the daemon TLS trust snapshot with a shared live trust-store handle
    so runtime verifiers can see trust added by pairing without restart.
  - Added N-P-5 trust-store upsert semantics for new peers, same-key re-pairing
    with deduplicated reachabilities, and key-replacement reachability reset.
  - Replaced boolean pair mode with a TTL-bound one-secret state supporting QR
    tokens, PINs, cancellation, success consumption, expiry, and PIN failed
    attempt cancellation.
  - Verification: `cargo test -p amux identity::`, `cargo test -p amux
    pairing::`, `cargo test -p amux dispatcher::`, `cargo test -p amux
    tunnel::pool::`, `cargo test -p amux services::startup::`, `cargo test -p
    amux`, `cargo check --workspace --all-targets`, and `git diff --check`
    passed.
- Applied PAIR1A round 1 review fixes:
  - `PairingService` now receives the same `Arc<PairMode>` used by the
    dispatcher, and its stub distinguishes inactive pair-mode from
    active-but-unimplemented flows.
  - Pubkey replacement now reports `PubkeyReplacementRequired` without making
    the new key visible to live TLS verifiers; callers must tear down the host
    and then call `replace_paired_peer_after_teardown`.
  - `ConnectionManager::teardown_host` now removes route channels, host tunnels,
    and direct Link writers for the peer so key replacement has a production
    teardown path.
  - PIN attempts now carry a session-bound handle; only the winning active
    session can consume pair-mode success.
  - Added outbound live-verifier coverage for server cert verification before
    and after trust replacement.
  - Verification: `cargo test -p amux pairing::`, `cargo test -p amux
    identity::`, `cargo test -p amux connection::`, `cargo test -p amux
    services::startup::`, `cargo test -p amux dispatcher::`, `cargo test -p
    amux tunnel::pool::`, `cargo test -p amux routing::link_registry::`, `cargo
    check --workspace --all-targets`, `cargo test -p amux`, and `git diff
    --check` passed.
- Applied PAIR1A round 2 bug-review fixes:
  - Trust-store replacement now marks the host as replacement-pending, causing
    live TLS verifiers to reject both the old key and candidate new key until
    post-teardown replacement is committed.
  - `LinkRegistry::close_host` now requests trust-replacement closure and waits
    for the routing Link writer to be removed by link cleanup before returning.
  - `ConnectionManager::teardown_host` now keeps the peer in a replacement
    barrier while teardown is in progress, ignores late old-key `HostUp` routes,
    and exposes `finish_host_replacement` for the post-trust-update unblock.
  - Verification: `cargo test -p amux identity::`, `cargo test -p amux
    connection::`, `cargo test -p amux routing::link_registry::`, `cargo test
    -p amux pairing::`, `cargo test -p amux services::startup::`, `cargo test
    -p amux routing::connect::`, `cargo test -p amux dispatcher::`, `cargo test
    -p amux tunnel::pool::`, `cargo check --workspace --all-targets`, `cargo
    test -p amux`, and `git diff --check` passed.
- Implemented PAIR1B QR/token responder service:
  - `PairingService::PairByToken` now gates on active pair-mode, validates
    `host_id`/pubkey/name bounds, rejects self-pairing, reserves the one-shot
    token during commit, records the peer with `Reachability::Cloud`, persists
    `trust.json`, consumes the token only after durable success, and returns
    the responder host id/name.
  - Device startup now wires `PairingService` with the local identity, host
    name, live trust store, `ConnectionManager`, and data directory.
  - Existing-pubkey replacement goes through the PAIR1A host teardown barrier
    before the new pubkey is committed.
  - Verification: `cargo test -p amux services::pairing::`, `cargo test -p
    amux services::startup::`, `cargo test -p amux identity::`, `cargo test -p
    amux connection::`, `cargo test -p amux routing::connect::`, `cargo check
    --workspace --all-targets`, `cargo test -p amux`, and `git diff --check`
    passed.
- Applied PAIR1B review fixes:
  - Token attempts now use a scoped reservation guard. Dropping/cancelling the
    RPC aborts the reservation, and save/replacement failures leave pair-mode
    retryable instead of burning the token.
  - Replacement commits now use a scoped guard that restores the old trust state
    and clears the connection replacement barrier if the pairing future is
    cancelled or errors before commit.
  - Pubkey replacement now revokes already-accepted mTLS Trusted Server
    transports and blocks late old-key registrations until replacement finishes;
    revocation is mark-and-return so pairing is not held hostage by HTTP/2 task
    drain timing.
  - `PairingService` stores only public local identity material, validates the
    responder name bound before success, and collapses token-mode mismatch to
    `INVALID_TOKEN`.
  - Verification: `cargo test -p amux pairing::`, `cargo test -p amux
    services::pairing::`, `cargo test -p amux services::startup::`, `cargo
    test -p amux connection::`, `cargo test -p amux transport::io::`, `cargo
    test -p amux dispatcher::`, `cargo check --workspace --all-targets`,
    `cargo test -p amux`, and `git diff --check` passed.
- Implemented PAIR1C PIN/SPAKE2 responder service:
  - `PairingService::PairBySpake2` now gates on active PIN pair-mode, runs the
    responder side of the PAKE exchange, derives key-confirmation and
    per-direction ChaCha20-Poly1305 keys, exchanges AEAD-sealed pairing
    identities, and commits trust only after successful durable persistence.
  - PIN success uses a scoped commit reservation so cancellation or persistence
    failure does not consume the PIN; failed key confirmation and failed
    identity decryptions count against the pair-mode failed-attempt cap.
  - Added focused responder tests for inactive pair-mode, success, invalid PIN
    retry behavior, and PIN commit reservation semantics.
  - Verification before review: `cargo test -p amux services::pairing::`,
    `cargo test -p amux pairing::`, `cargo test -p amux services::startup::`,
    and `cargo check --workspace --all-targets` passed.
- Applied PAIR1C round 1 review fixes:
  - Replaced the Ristretto SPAKE dependency with direct RFC 9382-style
    Ed25519/Curve25519 group operations using the specified M/N constants and
    raw shared point bytes as the amux HKDF input.
  - PIN attempts are now scoped in-flight guards, so the five-attempt cap
    includes concurrent streams and dropped streams release their slots.
  - Pair-mode completion now fails if the session was cancelled before success;
    trust writes are guarded and roll back both memory and `trust.json` if
    pair-mode completion fails or the pairing task is cancelled.
  - `PairingService` now threads a flow reachability through trust commits
    instead of baking that choice into the trust-store mutation path.
  - Trust-store pairing rejects a pubkey already trusted under another
    `host_id`, avoiding ambiguous TLS pubkey-to-host mapping.
  - Verification: `cargo test -p amux services::pairing::`, `cargo test -p
    amux identity::`, `cargo test -p amux pairing::`, `cargo test -p amux
    services::startup::`, and `cargo check --workspace --all-targets` passed.
- Applied PAIR1C round 2 review fixes:
  - Trust commits are now staged and saved before pair-mode completion but are
    not made visible to live TLS verifiers until after the token/PIN is
    consumed; rollback restores persisted trust and replacement barriers.
  - Pairing ingress reachability now travels in `BoxedGrpcConnectInfo`, so
    cloud-routed streams commit `Reachability::Cloud` and direct TCP dispatcher
    streams commit `Reachability::DirectTcp { addr }`.
  - `PairBySpake2` treats peer-sent `PairingError` messages as clean aborts
    instead of protocol violations.
  - `docs/NETWORKING.md` now specifies the edwards25519 scalar derivation,
    M/N constants, point encoding, peer-point validation, and compressed
    shared-point key-schedule input used by PAIR1C.
  - Duplicate-pubkey pairing errors are now opaque protocol violations rather
    than internal errors leaking the existing local host id.
  - Verification: `cargo test -p amux services::pairing::`, `cargo test -p
    amux dispatcher::`, `cargo test -p amux services::startup::`, `cargo test
    -p amux identity::`, `cargo test -p amux pairing::`, `cargo check
    --workspace --all-targets`, `cargo test -p amux`, and `git diff --check`
    passed.
- Started P4 as the first narrow breaking-change checkpoint. Current baseline:
  `amux.proto` still had `PROTOCOL_VERSION = 3`, `TunnelId.target`, and no
  `PairingService`.
- Implemented P4:
  - Bumped `protocol::PROTOCOL_VERSION` from 3 to 4.
  - Replaced proto/domain `TunnelId.target` with `TunnelId.nonce`.
  - Changed tunnel endpoint handling so empty `dst` is the target signal; the
    pool now stores explicit peer metadata for cleanup instead of deriving peer
    identity from `TunnelId`.
  - Added the `PairingService` proto messages and generated-client descriptor
    expectations.
  - Verification: `cargo test -p amux tunnel::`, `cargo test -p amux
    protocol::`, `cargo check -p amux`, and `git diff --check` passed.

## Review Log

### SPECMAP1

- Round 1 simplification review: completed. Kept §12 as a map of the current
  implementation rather than a future refactor plan: no `crypto/`,
  `connection/`, `identity/`, or `trust/` directories; no split
  `routing/connect/*.rs`; no separate CLI pair/relay files.
- Round 1 bug review: completed. Grepped for stale removed paths and old API
  names, fixed the component sketches to use the actual crate-internal
  signatures, and expanded §12.7 from wildcard groups to one row per §10
  invariant.
- Final gpt-5.5 xhigh discrepancy review: completed. Fixed the two actionable
  §12.2 mismatches it found: `PairMode` PIN attempt/commit signatures and
  `PairPinCloudPeer`/`PairQrCloudPeer` response types.

### CLIPPY1

- Simplification review: completed. Fixed warnings at the source, grouped
  related peer-trust commit arguments into small domain structs, and left CI
  unchanged because the required clippy guardrail was already present.
- Bug/security review: completed. Verified the proto oneof rename changes only
  generated Rust names and not field numbers, scoped blocking lock guards before
  awaits in tests, and added no new `#[allow(clippy::...)]` suppressions.

### IDTRUST1

- Round 1 simplification review: completed. Kept the split as two flat sibling
  files instead of adding `identity/` or `trust/` subdirectories; avoided a new
  trust-specific error type because the existing `IdentityError` is already the
  storage/TLS boundary used by callers.
- Round 1 bug/security review: completed. Fixed an accidental readiness
  behavior change by adding read-only `TrustStore::load_in` instead of using
  `load_or_create_in`; verified TLS pin verifiers still read the live trust
  store and that trust tests now run under `trust::`.

### PAIRMOD1

- Round 1 simplification review: completed. Kept the refactor mechanical:
  `services/pairing.rs` remains the service implementation, no behavior moved
  across service boundaries, and no compatibility shims were added for the old
  top-level module names.
- Round 1 bug/security review: completed. Grepped for stale
  `pin_pairing`/`qr_pairing`/`ssh_pairing` module declarations and imports,
  verified the new `pairing::` and `services::pairing::` test filters, and ran
  the full `amux` crate test suite with no findings.

### REVOKE1

- Round 1 simplification review: completed. Kept revocation as a purely local
  admin action, used one `PeerRef` resolver for list/get/unpair, reused the
  existing trust-store atomic save path, and routed cleanup through SWAP1
  `ConnectionManager::teardown_host` instead of creating duplicate revocation
  cleanup machinery.
- Round 1 bug/security review: completed. Added coverage that Unpair sends
  `GO_AWAY_REASON_USER_REVOKED`, evicts routing/core and connection-pool
  state, persists trust removal, and rejects paired remote mTLS callers for the
  new trust-admin RPCs.
- Round 2 simplification review: completed. Kept peer CLI output thin over the
  new RPCs and avoided introducing future propagation/key-rotation concepts
  into the v1 revocation surface.
- Round 2 bug/security review: completed. Verified TLS pin acceptance reads the
  live trust store after removal, `trust.remove` is an active audit category,
  and the focused `trust::` test namespace exists before the later IDTRUST1
  file split.

### OBS1

- Round 1 simplification review: completed. Fixed the main modeling issues by
  centralizing category constants/emitters in `audit.rs`, emitting trust
  insert/update/replace only after durable pairing commits, and moving JWT
  failure audits to request/reauth boundaries instead of lower-level helper
  duplication.
- Round 1 bug/security review: completed. Added missed audit coverage for
  pairing responder failures, cloud credential rejection, disruptive
  client-service call attribution, and committed trust outcomes.
- Round 2 simplification review: completed. Fixed remaining coverage gaps for
  acceptor-side JWT expiry, direct PIN/SSH helper failures, and dispatcher TLS
  accept timeout/protocol/signature failures.
- Round 2 bug/security review: completed. Fixed pair-mode expiry as a terminal
  pairing failure, cancel-only-when-active auditing, acceptor-side reauth error
  coverage, and sanitized peer-controlled pairing error details before audit
  logging.
- Final gpt-5.5 xhigh spec-discrepancy audit: completed. Initial findings on
  filtered client-visible host activity and §5.2.1 SPAKE2 completion-order text
  were fixed; the follow-up audit found no remaining actionable discrepancies
  in the touched STATUS1/OBS1 areas.

### STATUS1

- Round 1 simplification review: completed. Applied the main modeling fixes by
  flattening `HostEntry`, replacing `HostAdded` with `HostUpdated` upserts,
  keeping trusted route loss as an offline update, and clarifying §8.12
  reachability derivation.
- Round 1 bug/security review: completed. Fixed non-agent pairing candidate
  visibility, trust-transition publication after pairing commits, and stale
  call sites from the new wire shape.
- Round 2 simplification review: completed. Applied server-owned host-list
  scope selection, explicit reachability `oneof` encoding, public client row
  invariant checks, cached reachability status-change publication, and progress
  doc reconciliation.
- Round 2 bug/security review: completed. Fixed spoofable cloud-route
  classification by using stable `LinkRole::CloudRelay` metadata, added
  reachability update coverage, and restricted remote callers from enumerating
  untrusted online hosts or pairing candidates.

### R1

- Round 1 simplification review: completed for the DirectTcp runtime Link
  slice. Fixed the high-priority lifecycle finding by retaining pair-time Link
  tasks in the reachability connector and aborting them on connector drop.
  Deferred broader modeling choices for later R1/AUDIT work: whether
  `Reachability::Cloud` should remain persisted peer reachability, whether to
  collapse the direct-route registration mode matrix, and small TLS helper
  deduplication.
- Round 1 bug/security review: completed for the DirectTcp runtime Link slice.
  Fixed the route/channel cleanup race by letting `ConnectionManager` unregister
  direct route channels from the emitted `HostDown`; added an initial `Hello`
  timeout on routing acceptors; added a cloud routing establishment timeout.
- Round 2 simplification review: completed for the DirectTcp runtime Link
  slice. No blockers. Applied the low-priority expected-peer message polish.
- Round 2 bug/security review: completed for the DirectTcp runtime Link slice.
  No DirectTcp blockers. Added direct teardown/channel cleanup regression
  coverage. Deferred periodic DirectTcp retry/backoff as allowed by §8.8, and
  kept SSH trust-model concerns for the SSH integration slice because §3.1/§4.4
  explicitly define SSH reachability as SSH-trust rather than amux pubkey
  pinning.
- Final R1 simplification review: completed. No blocker. Applied low-noise
  cleanup by gating disabled cloud attach task creation and removing stale
  dead-code allowances from live R1 helpers. Deferred cloud-exclusion
  normalization and test-only disabled connector cleanup as non-blocking.
- Final R1 bug/security review: completed after follow-up. Added bidirectional
  DirectTcp coverage for mutual outbound Links. Accepted SSH runtime reachability
  under the documented SSH-trust model from §3.1/§4.4. The final focused
  re-review found no remaining blocker.

### PAIR1D

- Round 1 simplification review: completed. Applied the main collapse by
  routing SSH trust through the daemon's live `ClientService` instead of
  mutating `trust.json` directly; narrowed module exports; collapsed optional
  trust-store reachability helpers; expanded artifacts and docs.
- Round 1 bug review: completed. Blocking findings were fixed: added hidden
  `pair-recv`/`relay` commands, avoided live trust-store bypass, used the
  shared key-replacement teardown path, and hardened SSH target spawning with
  `-T`, `BatchMode=yes`, `--`, and leading-hyphen rejection.
- Round 2 simplification review: completed. Applied required pairing-trust
  wiring for `ClientService` and removed the test-only file-commit SSH path.
  Deferred validation helper deduplication because the remaining duplication is
  small and checkpoint-local; `transport::ssh` remains lightly dead-code-marked
  until R1 wires runtime SSH routes.
- Round 2 bug review: completed. Fixed serialized trust commits with a shared
  commit lock and regression coverage; the one-sided SSH trust window is
  documented as a distributed transaction limitation pending
  revocation/trust-removal design. SPECSIM1 later removed the temporary
  SSH relay sibling socket design.

### P4

- Round 1 simplification review: completed. Findings:
  - Suggested collapsing protocol version negotiation. Not applied because
    `docs/NETWORKING.md` §6.3 explicitly keeps `Hello.supported_protocol_versions`
    and `HelloAccepted.protocol_version` unchanged for this revision.
  - Suggested reusing `PairingIdentity` in `PairByToken` request/response. Not
    applied because §6 defines the token RPC shapes explicitly.
  - Suggested replacing `ActiveTunnel { peer, tunnel }` with peer metadata only
    on outbound channels. Applied in `crates/amux/src/tunnel/pool.rs`.
- Round 1 bug review: completed. No blockers. Low hardening suggestion to assert
  `PairingService` methods in descriptor tests was applied in
  `crates/amux/src/protocol/mod.rs`.
- Round 2 simplification review: completed. No remaining worthwhile
  spec-consistent simplifications found for this checkpoint.
- Round 2 bug review: completed. No blockers. Low hardening suggestion to lock
  exact `TunnelId` descriptor fields and `PairBySpake2` bidi streaming shape
  was applied in `crates/amux/src/protocol/mod.rs`.

### K1

- Round 1 simplification review: completed. Findings:
  - Cloud relay construction was creating device identity files. Applied:
    server construction now receives `as_cloud_relay`; cloud relays keep an
    ephemeral host id and do not touch `device.key`/`host_id`/`trust.json`.
  - CLI init had duplicate identity creation paths. Applied: `run_init` owns
    identity creation through an `EnsureDeviceIdentity` init step; implicit
    setup uses `needs_init`.
  - Trust-store mutation/defaults were premature. Applied: removed K1 `upsert`
    semantics and removed default-filled `TrustEntry` deserialization.
- Round 1 bug review: completed. Blocking findings were fixed:
  - Race-safe first creation now writes a complete private temp file and uses
    `hard_link` into place so concurrent creators reload the winner.
  - `device.key` now uses Ed25519 PKCS#8 v1 DER.
  - Data directories are chmodded `700` on Unix; identity/trust files are
    chmodded `600` on load/create.
  - Existing-file replacement uses platform-specific replacement
    (`rename` on Unix, `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows).
  - Trust-store load/save validates 32-byte pubkeys.
  - Added server tests for persisted device host ids and cloud relay non-creation.
- Round 2 simplification review: completed. Findings:
  - Server construction and `run()` accepted separate cloud/device values.
    Applied: server mode is now stored on `Server`; `run()` is argument-free.
  - Suggested removing `EnsureDeviceIdentity` from the init state machine. Not
    applied because `needs_init` must surface missing/invalid identity before
    prompting, including a too-open existing data dir.
  - Suggested using ring PKCS#8 generation/loading directly. Not applied because
    §10 explicitly requires PKCS#8 v1 DER.
  - Removed the stale pending review line.
- Round 2 bug review: completed. No blockers after fixes. Medium data-dir mode
  gap was fixed by making readiness require Unix mode `700`; added a regression
  test that a pre-existing too-open data dir is detected and repaired.

### G1

- Round 1 simplification review: completed. Findings:
  - Cloud mode was still constructing the device Trusted/Pairing topology.
    Applied: cloud startup now only serves `CloudRoutingService`; device
    startup alone constructs local agent state, Trusted Server, Pairing
    Server, dispatcher ingress, and local sockets.
  - Device startup still had a no-security fallback that forwarded tunnels to
    the Trusted Server. Applied: `start_user_services` now requires
    `DeviceRuntimeSecurity`; cloud routing uses the raw routing-only startup
    path.
  - Suggested dropping dispatcher peer metadata and reclassifying trust only at
    rustls verification time. Not applied because bug review required binding
    `RoutingService.Connect` Hello identity to the TLS-pinned peer.
  - PairingService duplicated dispatcher pair-mode admission. Applied: the
    stub PairingService is unconditional behind Pairing Server ingress; PAIR1
    will add real pairing session state.
  - Server loaded identity and trust separately. Applied: server startup uses a
    single helper returning both identity and trust store.
- Round 1 bug review: completed. Blocking findings were fixed:
  - `RoutingService.Connect` now rejects `Hello.host_id` values that do not
    match the TLS-pinned peer from transport connect info.
  - Outbound multi-hop device tunnels now wrap `TunnelTransport` in client-side
    TLS using the local device identity and peer-pinned trust entry; the legacy
    plaintext direct `server connect` path now fails fast pending CLI1/PAIR1.
  - The running dispatcher still snapshots trust at startup. This is deferred
    to PAIR1 because this checkpoint has no trust-store mutation path yet.
- Round 2 simplification review: completed. Findings:
  - Cloud relay still had a raw-TCP compatibility branch behind
    `enforce_tls_in_cloud_mode`. Applied: cloud relay startup always requires
    TLS cert/key env vars and serves TLS; the config knob was removed.
  - `TunnelPool` still had a plaintext endpoint-channel mode. Applied for
    production: plain pools are forwarding-only and endpoint `channel_to`
    rejects without device TLS; unit tests retain raw transport coverage.
  - Server and startup still modeled impossible device states as `Option`.
    Applied: server mode is now `CloudRelay` or `Device { identity,
    trust_store }`, and `StartedUserServices` always owns a dispatcher.
  - Suggested collapsing `BoxedGrpcAuth` to only a TLS peer. Not applied for
    G1 because the current enum keeps Trusted, Pairing, and local ingress
    distinguishable in tests without changing production topology.
- Round 2 bug review: completed. Blocking and medium findings were fixed:
  - Inbound endpoint tunnel transports now clean up their pool tunnel entry on
    drop, covering TLS/pair-mode rejection and normal stream completion.
  - Outbound tunnel TLS handshakes now have a timeout, and provisional tunnel
    state is removed when handshake setup fails, times out, or is cancelled.
  - Connector-side `HelloAccepted.host_id` can now be bound to an expected TLS
    peer for future paired direct links; a regression test rejects mismatches.
  - Remaining non-blocking test suggestions for cloud-mode service exposure and
    untrusted endpoint negative inventory are deferred because production cloud
    no longer constructs device services and PAIR1/R1 will add explicit
    trust/reachability mutation paths.

### CN1

- Round 1 simplification review: completed. Blocking simplification findings
  were applied:
  - Direct connector routes now share the same `ConnectionPool` used by
    `ConnectionManager`; connector-side direct channels are registered before
    the route-level `HostUp` event.
  - `RoutingCore` now stores insertion-ordered routes per host instead of a
    single first route, allowing `ConnectionManager` to enforce route policy
    against real runtime state rather than test-only side state.
  - The acceptor direct-host admission path now has one activation point:
    `HostUp` is stored only after `LinkRegistry` registration inside the
    established connection path.
  - Legacy one-hop tunnel fallback was removed from connection selection; a
    one-hop route must have a registered direct channel.
- Round 1 bug review: completed. Blocking findings were fixed:
  - `RoutingCore` emits raw `RoutingEvent::HostUp` for each distinct route,
    keeps one client-visible host row until STATUS1's `HostUpdated` projection,
    emits raw `HostDown` for each removed route, and emits client `HostRemoved`
    only when the last route is gone.
  - Connector `spawn_connector_to_channel` registers the direct tonic
    `Channel` by route before storing `HostUp`; cleanup unregisters the route.
  - Acceptor-side `HostUp` was moved after `LinkRegistry` registration so
    routing events cannot race ahead of link availability.
  - `ConnectionManager` re-checks route membership after materialization before
    setting an active route, unregistering stale materialized channels.
  - Regression coverage now includes direct-down fallback to a multi-hop route,
    single-hop-without-channel rejection, connector direct-channel
    registration, multi-route `RoutingCore` events, and a real two-hop
    remote-dispatch tunnel fixture.
- Round 2 simplification review: completed. Blocking findings were fixed:
  - Device acceptors no longer publish an outbound direct route unless a
    `ConnectionPool` channel exists. Cloud relay contexts explicitly opt into
    routing-only direct routes because they forward `Message` envelopes but do
    not expose device service calls over an acceptor-side tonic `Channel`.
  - `ConnectionManager::handle_event` now uses the same activation path for
    direct and multi-hop routes; shorter multi-hop `HostUp` events eagerly
    materialize and swap.
  - Local link reservation now checks only local reserved link names, not
    downstream route hop names from other nodes' link namespaces.
  - Tunnel return-path selection uses the shared shortest/FIFO route selector,
    and active tunnel cleanup is route-keyed. The host-event tunnel cleanup
    task was removed.
- Round 2 bug review: completed. Blocking findings were fixed:
  - Broken one-hop routes now fail instead of falling back to a longer tunnel
    route; regression coverage asserts no tunnel is materialized in that case.
  - `ConnectionManager` now keeps route membership and active-route updates
    under one state lock, so `HostDown` wins the materialization-before-active
    race and unregisters stale channels.
  - `LinkRegistry` snapshot activation deduplicates pending deltas by
    `(host_id, route)`, preserving distinct route-level `HostUp` and
    `HostDown` events.
  - Incoming endpoint tunnel frames without a return route are dropped without
    closing the routing Link, avoiding route-propagation races during eager
    multi-hop activation.
  - Regression coverage was added for all of the above plus acceptor-only
    direct route suppression and cloud-routed remote inventory.

### PAIR1A

- Round 1 simplification review: completed. Findings:
  - Pair-mode state was private to dispatcher startup. Applied: the same
    `Arc<PairMode>` is now passed to `PairingService`, and startup tests lock
    inactive vs active pair-mode behaviour.
  - `TrustStorePairingUpdate::ReplacedPubkey` carried the old pubkey even though
    N-P-5 teardown is host-scoped. Applied: replacement outcomes are unit
    variants.
  - Pair-mode tests used a fake `PairSecret::Test` variant. Applied: dispatcher
    tests activate real token pair-mode state.
- Round 1 bug review: completed. Blocking findings were fixed:
  - Pubkey replacement no longer mutates the live trust store before teardown;
    `upsert_paired_peer` returns `PubkeyReplacementRequired`, and
    `replace_paired_peer_after_teardown` performs the replacement after the
    caller tears down the host.
  - `ConnectionManager::teardown_host` tears down host routes, route-keyed
    channels, active tunnels, and direct Link writers for future N-P-5 key
    replacement.
  - PIN success consumption now requires a session-bound attempt handle, so a
    second concurrent attempt cannot commit after the first successful pairing
    consumes pair-mode.
  - Outbound live trust-store verification now has coverage for trust insertion,
    replacement-required state, and post-teardown replacement.
- Round 2 simplification review: completed. No material simplifications remain.
  Optional cleanups noted: remove a stale temporary binding in connection event
  handling, possibly factor trust-store insert/update helpers later, and avoid a
  minor startup clone if that area is touched again.
- Round 2 bug review: completed. Blocking findings were fixed:
  - Replacement-pending trust now blocks live verifiers from accepting the old
    key during replacement and keeps the candidate new key unavailable until
    `replace_paired_peer_after_teardown`.
  - Host teardown now waits for trust-replaced routing Links to clean up before
    returning.
  - `ConnectionManager` now keeps a host-level replacement barrier so late
    old-key `HostUp` events are ignored until the replacement is finished.

### PAIR1B

- Round 1 simplification review: completed. Blocking findings were fixed:
  - Pair completion is now one guarded commit/abort path instead of a loose
    sequence of token consumption, trust mutation, persistence, teardown, and
    barrier cleanup.
  - `PairingService` stores `LocalPairingIdentity` public material instead of a
    full `DeviceIdentity` with private key bytes.
  - `ConnectionManager::teardown_host` now factors repeated host runtime cleanup
    through one helper.
- Round 1 bug review: completed. Blocking and medium findings were fixed:
  - Existing old-key Trusted Server transports are revoked during pubkey
    replacement, not just routing Links.
  - QR tokens are reserved while trust is being committed and consumed only
    after successful durable commit.
  - Token-mode mismatch and consumed/expired/unknown token cases collapse to
    `INVALID_TOKEN`; responder names are bounded to 256 bytes before success.
- Round 2 simplification review: completed. Blocking findings were fixed:
  - Token reservation and host replacement cleanup are now scoped guards so RPC
    cancellation cannot strand pair-mode, pending trust, or route barriers.
  - Trusted transport revocation marks authority as revoked and returns; it no
    longer waits unbounded for HTTP/2 tasks to observe shutdown.
- Round 2 bug review: completed. Blocking findings were fixed:
  - mTLS Trusted Server streams are registered for revocation immediately after
    dispatcher authentication, before they can sit queued for the Trusted
    Server.
  - Late trusted transport registrations for a host under replacement are
    marked revoked until `finish_host_replacement`.
  - Dropped token attempts abort their reservation and allow retry or expiry.

### PAIR1C

- Round 1 simplification review: completed. Blocking findings were fixed:
  - The responder no longer uses `spake2-conflux::RistrettoGroup`; it uses
    Curve25519/Ed25519 group operations and the RFC M/N constants directly.
  - The amux HKDF schedule now consumes raw SPAKE2 shared point bytes instead
    of a library-derived session key.
  - Pairing trust commits now accept an explicit flow reachability instead of
    hiding `Reachability::Cloud` in the mutation helper.
- Round 1 bug review: completed. Blocking and medium findings were fixed:
  - Concurrent PIN streams now consume in-flight attempt slots, preventing more
    than five simultaneous online guesses in one pair-mode window.
  - Pair-mode success completion fails after explicit cancellation, and the
    trust commit guard rolls back persisted trust if completion fails.
  - Duplicate pubkeys across different host ids are rejected before trust-store
    persistence.
- Round 2 simplification review: completed. Medium findings were fixed:
  - Pairing reachability is now carried by ingress connect metadata instead of
    stored as one service-wide value.
  - Peer-sent `PairingError` messages in the SPAKE2 stream now cleanly abort
    the exchange.
  - Low suggestions to collapse PIN attempt/commit guard state and centralize
    pubkey-replacement orchestration remain candidates for a later cleanup; the
    current guard split is kept because cancellation-safe retry semantics are
    covered by focused regressions.
- Round 2 bug review: completed. High and medium findings were fixed:
  - Staged trust is not visible to TLS verification until pair-mode success is
    consumed, and rollback restores persisted trust/barriers.
  - Direct TCP pairing reachability is propagated from dispatcher ingress into
    the trust commit.
  - SPAKE2 wire/scalar details are now documented in §5.2.1/§6.1.
  - Duplicate-pubkey errors are opaque protocol violations.

## K1 Work Log

### 2026-05-19

- Added `crates/amux/src/identity.rs` for the spec data-dir layout:
  `device.key`, `host_id`, and `trust.json`.
- Device identity now persists a ring-generated Ed25519 private key and derives
  the 32-byte raw public key from it.
- `host_id` is persisted as 16 raw bytes and reused by server startup instead
  of generating a new UUID per process.
- Added JSON trust-store domain types for `Reachability::{Cloud,Ssh,DirectTcp}`
  and `TrustEntry`; pairing-time mutation semantics are deferred to PAIR1.
- Writes create private files with mode `600` on Unix. Existing-file updates
  use temp-file-then-platform-replace; first creation uses temp-file-then-hard-link
  to avoid concurrent creator overwrite races.
- `amux init` and implicit CLI initialization ensure identity/trust files exist
  through a single init step. Device server startup also ensures the same files
  exist for library callers; cloud relay startup deliberately does not.
- Verification: `cargo test -p amux identity::`, `cargo test -p amux-cli
  init::`, `cargo check -p amux-cli`, and `git diff --check` passed.
- Round 1 review fixes added:
  - Race-safe concurrent first-run creation test.
  - Server tests for persisted host ids and cloud relay identity separation.
  - Workspace verification: `cargo test -p amux identity::`, `cargo test -p
    amux server::tests::`, `cargo test -p amux-cli init::`, `cargo check
    --workspace --all-targets`, and `git diff --check` passed.

## G1 Work Log

### 2026-05-19

- Started G1 to replace direct plaintext device network ingress with TLS
  dispatcher admission and to split daemon runtime ingress into Trusted Server
  and Pairing Server service sets.
- Implemented G1:
  - Device identities now derive an Ed25519 self-signed X.509 certificate from
    the persisted PKCS#8 v1 key and build a TLS 1.3 server config that requests
    but does not require client certificates.
  - Added pinned client-cert verification against `trust.json`; unpinned client
    certs fail the handshake, trusted certs dispatch to the Trusted Server, and
    anonymous streams dispatch only to the Pairing Server when pair-mode is
    active.
  - Added a `TunnelDispatcher` for external TCP and terminating tunnel streams.
    Device `tcp_port` no longer starts a plaintext `RoutingService` listener.
  - Split runtime ingress into boxed streams feeding Trusted Server
    (`ClientService`, `AgentService`, `RoutingService`) and Pairing Server
    (`PairingService`) channels.
  - Added an inactive `PairMode` and a stub `PairingService` implementation;
    PAIR1 will implement the token/SPAKE2 flows.
  - Verification: `cargo test -p amux identity::`, `cargo test -p amux
    dispatcher::`, `cargo test -p amux services::startup::`, `cargo test -p
    amux server::tests::`, `cargo check --workspace --all-targets`, and
    `git diff --check` passed.
- Round 1 review fixes added:
  - Cloud relay startup no longer constructs device-local services or identity
    runtime state; device startup requires persisted identity/trust material.
  - TLS transport identity is carried into `RoutingService.Connect`, and Hello
    identity spoofing is rejected before route admission.
  - `TunnelPool` outbound channels can use end-to-end mTLS over tunnel
    transports, while cloud/user routing internals retain raw channels where
    they are not host-to-host payload endpoints.
  - Legacy direct plaintext `server connect` no longer opens a raw
    `RoutingService` channel.
  - Verification: `cargo check -p amux --tests`, `cargo test -p amux
    acceptor_rejects_hello_host_id_that_does_not_match_tls_peer`, `cargo test
    -p amux identity::`, `cargo test -p amux dispatcher::`, `cargo test -p
    amux services::startup::`, `cargo test -p amux tunnel::pool::`, `cargo
    test -p amux server::tests::`, `cargo test -p amux`, `cargo check
    --workspace --all-targets`, and `git diff --check` passed.
- Round 2 review fixes added:
  - Removed the cloud relay plaintext compatibility branch and the
    `enforce_tls_in_cloud_mode` config field.
  - Replaced nullable server device credentials with an explicit server mode
    enum and made user services hold a non-optional dispatcher.
  - Added tunnel transport drop cleanup, outbound tunnel TLS timeout handling,
    and regression tests for rejected inbound tunnel cleanup and outbound
    timeout cleanup.
  - Added connector-side expected-peer binding for future direct TLS links and
    a mismatch regression test.
  - Verification: `cargo check -p amux --tests`, `cargo test -p amux
    dropping_inbound_endpoint_transport_removes_target_tunnel`, `cargo test -p
    amux outbound_tls_timeout_removes_provisional_tunnel`, `cargo test -p amux
    connector_rejects_hello_accepted_host_id_that_does_not_match_expected_peer`,
    `cargo test -p amux acceptor_rejects_hello_host_id_that_does_not_match_tls_peer`,
    `cargo test -p amux identity::`, `cargo test -p amux dispatcher::`, `cargo
    test -p amux services::startup::`, `cargo test -p amux tunnel::pool::`,
    `cargo test -p amux server::tests::`, `cargo test -p amux`, `cargo check
    --workspace --all-targets`, and `git diff --check` passed.

## CN1 Work Log

### 2026-05-19

- Started CN1 to replace the host-keyed tunnel channel cache with a
  route-keyed connection layer.
- Implemented first pass:
  - Added `connection::ConnectionPool` as a `Route -> Channel` registry with
    register/get/unregister and no materialization policy.
  - Added `connection::ConnectionManager` with per-peer route lists,
    `active_route`, shortest-route selection, route-keyed materialization, and
    make-then-break unregister of the previous route.
  - `StartedRoutingServices` now owns one `ConnectionManager`, seeded from
    `RoutingCore` events. `ClientService` remote calls use it instead of
    calling `TunnelPool` directly.
  - `TunnelPool` no longer stores a host-keyed channel cache; it only
    materializes tunnel-backed channels for a supplied route and tracks tunnel
    lifetime by peer metadata.
  - Added `Route::len()` for route policy comparisons.
  - Verification: `cargo check -p amux --tests`, `cargo test -p amux
    connection::`, `cargo test -p amux tunnel::pool::`, `cargo test -p amux
    services::client::`, `cargo test -p amux services::startup::`, `cargo
    test -p amux routing::connect::`, `cargo check --workspace --all-targets`,
    and `git diff --check` passed.
- Round 1 review fixes added:
  - Converted `RoutingCore` from one route per host to ordered route lists,
    preserving first-route host snapshots while emitting raw route-level
    events for all distinct routes.
  - Shared the `ConnectionManager` pool with routing connect contexts so
    connector direct channels are available before direct `HostUp`.
  - Delayed acceptor direct `HostUp` until after link registration and removed
    the host-id collision rejection for distinct routes to the same host.
  - Guarded `ConnectionManager` so direct one-hop routes are never tunneled
    without a registered direct channel, and stale materialization cannot set
    an active route after `HostDown`.
  - Updated the client remote-dispatch harness to use a real two-hop relay
    route for tunnel-backed remote calls.
  - Verification: `cargo check -p amux --tests`, `cargo test -p amux
    routing::core::`, `cargo test -p amux connection::`, `cargo test -p amux
    routing::connect::`, `cargo test -p amux tunnel::pool::`, `cargo test -p
    amux services::startup::`, `cargo test -p amux services::client::`,
    `cargo test -p amux`, `cargo check --workspace --all-targets`, and `git
    diff --check` passed.
- Round 2 review fixes added:
  - Removed fallback from a missing direct one-hop channel to a longer
    multi-hop tunnel route.
  - Made `ConnectionManager` route/active state atomic and made `HostUp`
    eagerly activate better multi-hop routes.
  - Added route suffix deduplication for strictly worse `HostUp` routes and
    limited link-name reservation to local link names.
  - Made inactive link activation route-level in `LinkRegistry`.
  - Changed device acceptor contexts to suppress outbound direct `HostUp`
    without a direct channel; cloud relay contexts opt into routing-only
    direct routes for envelope forwarding.
  - Made tunnel cleanup route-keyed and removed the startup host-event cleanup
    task.
  - Verification: `cargo check -p amux --tests`, `cargo test -p amux
    connection::`, `cargo test -p amux routing::core::`, `cargo test -p amux
    routing::connect::`, `cargo test -p amux routing::link_registry::`,
    `cargo test -p amux tunnel::pool::`, `cargo test -p amux
    services::startup::`, `cargo test -p amux services::client::`, `cargo test
    -p amux`, `cargo check --workspace --all-targets`, and `git diff
    --check` passed.
