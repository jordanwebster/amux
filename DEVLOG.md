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

## 2026-06-11: Replace the v5 networking spec with an architecture doc

### Summary
The tombstone chain is gone: `docs/NETWORKING.md` (the superseded 3,202-line
v5 spec), `docs/NETWORKING_PROGRESS.md` (its work ledger), and the three
pointer stubs (`NEW_ARCHITECTURE.md`, `architecture.md`,
`cloud_architecture.md`) are deleted; git history keeps them. In their
place, `docs/ARCHITECTURE.md` — a v6-true system doc complementing
PROTOCOL.md: PROTOCOL.md owns the wire, ARCHITECTURE.md owns the system
(process/deployment shapes, identity & trust store, the two-server model,
the dispatcher's classification table, the service surface map, the
multi-tenant cloud deployment, and the LinkRegistry / RoutingCore /
TunnelPool / ConnectionManager layering). Disposition of NETWORKING.md's
material: §3–4 (threat model, identity, trust, two servers, dispatcher,
tenancy), §7 (CLI shape), §8.4/8.12/8.13, and the resource caps were
rewritten for v6 into ARCHITECTURE.md; §4.8, §5–6, §8.5–8.11 were
protocol-level and already superseded by PROTOCOL.md (the SPAKE2 crypto
detail of §5.2.1 survives only in `services/pairing.rs` for now); the
glossary, invariant catalog (§10), and reference implementation map (§12)
died with v5's vocabulary.

### Changes
- `docs/ARCHITECTURE.md` created; the five superseded docs `git rm`ed.
- `docs/PROTOCOL.md` status header now points at ARCHITECTURE.md instead
  of the deleted NETWORKING.md.
- Every `docs/NETWORKING.md §x.y` / `NETWORKING_REVIEW.md §6.x` citation in
  `crates/` re-pointed at PROTOCOL.md/ARCHITECTURE.md section names or
  rewritten as self-contained prose (spec chapter headers in
  `tests/spec/{identity,presence,routing,sessions,wire}.rs`; doc comments
  in `connection.rs`, `setup.rs`, `tunnel/pool.rs`, `testnet/daemon.rs`).
  The invariant catalog is not preserved as a numbered list; comments now
  say what they mean.
- `CLAUDE.md`/`AGENTS.md` still reference the deleted docs; they are
  re-pointed in a follow-up chunk.

### Verification
- Spec suite: 43 passed / 0 ignored. Lib tests: 394 passed.
- `cargo fmt --all`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `grep -rn "NETWORKING\|NEW_ARCHITECTURE\|cloud_architecture" --include="*.rs"
  --include="*.md" .` is clean outside `notes/`, DEVLOG history, and
  CLAUDE.md (deferred above).

### Next Steps
- Re-point CLAUDE.md and AGENTS.md at PROTOCOL.md + ARCHITECTURE.md.
- Ledger candidate: `RoutedStream::expect_stalled_open` in
  `testnet/daemon.rs` no longer has a caller — v6 revocation breaks
  streams instead of stalling them — and could be deleted.

---

## 2026-06-11: e2e runner catches up with v6 config

### Summary
All 14 e2e tests failed after the v6 chunks with one cause: the runner's
generated `local.yaml` still set `randomise_link_name`, deleted with wire
link names in chunk 2 (the config parser rejects unknown fields). Removed
the field from the template in `e2e-runner/src/executor.rs`.

### Verification
- `cargo run -p e2e-runner -- run`: 14 passed, 0 failed.

---

## 2026-06-11: protocol v6 complete — docs graduate

### Summary
Closing docs pass for the v6 implementation (chunks 1–5, commits
6735df6 → ccd7a25). `docs/PROTOCOL.md` graduates from "target design" to
the implemented spec, locked in by the prose suite in
`crates/amux/tests/spec/`. `docs/NETWORKING.md` is marked superseded
(v5, historical) with a banner saying exactly what v6 replaced and that
PROTOCOL.md + the spec suite win where they disagree.

### Changes
- `docs/PROTOCOL.md`: status header → implemented.
- `docs/NETWORKING.md`: supersession banner.

### Verification
- Final state on `protocol-v6`: lib 394 passed; spec suite 43 passed /
  0 ignored; CI clippy clean; workspace build clean. The wire
  `Message.body` oneof matches PROTOCOL.md's vocabulary verbatim:
  Hello · HelloAck · NeighborUp · NeighborDown · TunnelOpen · TunnelData
  · TunnelClose · Reauth · LinkClose, plus `PairingService.Pair`;
  `PROTOCOL_VERSION = 6`.

### Next Steps
- Decide the fate of NETWORKING.md's still-accurate material (identity,
  trust store, two-server model, dispatcher): fold into PROTOCOL.md
  companions or rewrite as a v6 reference.
- §6 ledger follow-ups: move route activation's TLS handshake off the
  ConnectionManager events task (§6.12); `last_dial_error` changes push
  no event to subscription-only UIs (D15 note).

---

## 2026-06-11: v6 chunk 5 — fire-and-forget Reauth; reachability shrinks to two fields

### Summary
The final v6 code chunk: D12 (P5) and D15 (P7). Credential refresh is now
fire-and-forget — the daemon sends `Reauth { token }` on the cloud link at
expiry − 5 min and expects nothing back; the cloud's only answers are the
two things it can already say: silence (refresh accepted, the link
continues) or `LinkClose(AUTH_EXPIRED)` (the daemon reconnects with a
fresh token — the recovery path that exists anyway). `ReauthAck`, the 15s
ack-timeout state machine, and `ConnectorReauthState` are deleted; the
protocol never acknowledges housekeeping, it only signals state changes.
A healthy link, and every session riding its tunnels, is never disturbed
by refresh. Separately, the host-listing reachability surface shrinks to
two honest fields: `HostEntry` carries `online: bool` (routing-derived
presence) + `last_dial_error: optional string` (the last failed dial
attempt's outcome, cleared when a route comes up). The
`reachable/unreachable/unknown` enum, its three proto wrapper messages,
and the `StatusChanged` plumbing are deleted — nothing probes, so
"unknown" is `!online && last_dial_error.is_none()`, derived client-side
if anyone cares. This completes the v6 wire vocabulary: `Hello`/`HelloAck`
· `NeighborUp`/`NeighborDown` · `TunnelOpen`/`TunnelData`/`TunnelClose` ·
`LinkClose` · `Reauth` · `PairingService.Pair`.

### Changes
- `proto/amux/v1/amux.proto`: `ReauthAck` deleted from `Message.body`
  (`LinkClose` renumbered to 9); the three `HostReachability*` wrapper
  messages and `HostReachabilityStatus` deleted;
  `HostEntry.reachability_status` → `optional string last_dial_error`.
- `routing/connect/mod.rs`: `ConnectorReauthState` and
  `LINK_AUTH_REAUTH_RESPONSE_TIMEOUT` deleted; `LinkConnectorAuth` itself
  now holds the token in force and gains `refresh_deadline`/`send_refresh`
  (send the Reauth, adopt the refreshed token's expiry as the next
  deadline — no pending/awaiting state). The ack-timeout select arm and
  the `ReauthAck` body arm are gone; the refresh-send arm survives. On the
  acceptor, a good refresh extends auth silently; a bad token (validation
  failure or user mismatch) answers `LinkClose(AUTH_EXPIRED)` and closes.
- `routing/events.rs`: `HostReachabilityStatus` deleted;
  `HostReachabilityEvent::StatusChanged` deleted; `HostEntry` carries
  `last_dial_error`. `routing/core.rs::notify_host_status_changed`
  deleted; `connection.rs` record/clear_reachability_error keep the
  storage (it IS `last_dial_error`) but emit nothing.
- `services/client.rs`: both host-entry builders read the stored dial
  error straight off the connection manager (`stored_last_dial_error`);
  `host_reachability_status_to_wire` deleted; the pairing-candidate filter
  drops its `reachability_status.is_none()` clause (the trust-status check
  already said it); `publish_host_status_update` survives for trust
  transitions but returns nothing; `HostEventOutcome::Updated` deleted.
- `client/mod.rs`: `host_reachability_status_from_wire` and the
  status-presence validation rules deleted; `last_dial_error` decodes as a
  plain optional string. `HostReachabilityStatus` un-exported from
  `lib.rs`/`routing/mod.rs`; `amux-cli` test fixture updated (the CLI/UI
  never rendered the three-state beyond carrying the field).
- Spec: `cloud_peers_keep_communicating_across_a_jwt_expiry` strengthened
  per D12 — an echo session opened before the refresh point echoes again
  after `jwt.expired()` on the same stream/tunnel/link (tunnels die with
  links, so the surviving stream is proof of zero disturbance).

### Decisions Made
- Lib tests:
  `unauthenticated_acceptor_rejects_reauth_ack` deleted (message gone);
  `authenticated_acceptor_accepts_reauth_for_same_user` rewritten as
  `…_silently_extends_auth_on_reauth_for_same_user` (initial token expires
  inside the asserted quiet window, so silence also proves the extension);
  `…_rejects_reauth_for_different_user` rewritten to expect
  `LinkClose(AUTH_EXPIRED)`, plus a new invalid-token twin;
  `connector_sends_reauth_before_token_expiry` rewritten ack-free plus new
  `connector_schedules_the_next_refresh_from_the_refreshed_token` (locks
  the no-ack rescheduling rule); connection.rs
  `reachability_error_changes_emit_host_status_event` rewritten as
  `reachability_errors_are_stored_until_cleared` (storage, no events);
  client.rs status tests re-pointed at `last_dial_error`
  (`host_status_change_publishes_cached_reachability_error` folded into
  the list-hosts dial-error test — there is no status event to publish).
- `last_dial_error` changes do not push `HostUpdated` events (that was the
  deleted `StatusChanged` plumbing); listings re-read the storage on
  demand, which is what the harness verbs and UIs poll anyway.

### Verification
- `cargo build -p amux --features testnet` and `cargo build --workspace`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed.
- Spec suite: 43 passed / 0 ignored, ×2 runs (32.25s / 32.26s), with the
  strengthened JWT-expiry test.

### Next Steps
- v6 implementation complete (chunks 1–5: D10/D11 → D2/D13/D14 → D3a/P8 →
  D9 → D12/D15). Graduate the remaining doc work: PROTOCOL.md is current;
  NETWORKING.md still describes v5 and is superseded where they overlap.
- Ledger note: the connector trusts the refresher's `expires_at`
  unconditionally — a refresher that keeps minting tokens already inside
  the 5-minute refresh window drives back-to-back refreshes (pre-existing
  shape, now without even an ack to pace it).

---

## 2026-06-11: v6 chunk 4 — one pairing protocol (SPAKE2), two secret deliveries

### Summary
P2/D9 plus the D13 rename: pairing is now ONE wire protocol —
`PairingService.Pair(stream PairMessage)`, the SPAKE2 exchange — with two
out-of-band secret-delivery mechanisms: a typed 6-digit PIN (no camera) and
a QR-carried 256-bit secret (scan → paired; the user never sees it). The QR
payload shrinks to `{host_id, cloud_url, secret}` — the pubkey drops out
because SPAKE2 provides mutual authentication from possession of the
secret. This is stronger, not just simpler: the old `PairByToken` flow sent
a bearer token through a pubkey-pinned TLS channel, so the secret crossed
the wire; now it never does. One-shot consumption, ~5-minute TTL, and the
5-attempt cap are uniform across both deliveries (the cap is moot for a
256-bit secret but harmless). The cloud-only token-ingress restriction
(review item D5) is deleted with the token protocol — the Pair stream is
admitted on any pre-trust pairing transport. `PairBySpake2` → `Pair` and
`PairBySpake2Message` → `PairMessage`: the qualifier was a fossil of the
deleted second protocol.

### Changes
- `proto/amux/v1/amux.proto`: `PairByToken` RPC + request/response deleted;
  `PairBySpake2(stream PairBySpake2Message)` → `Pair(stream PairMessage)`;
  `PairingError.Reason::INVALID_TOKEN` deleted (reasons renumbered);
  `PairQrCloudPeerRequest` is `{host_id, secret}`;
  `StartPairingResponse.secret` oneof carries `qr_secret` not
  `one_shot_token`.
- `pairing/mod.rs`: `PairMode` collapses to one secret kind — bytes plus
  attempt accounting (`failed/in_flight/reserved`) and an audit label.
  `PairSecret::Token`, `PairModeTokenAttempt`, `begin/complete/abort`
  token methods, `token_matches`, and `InvalidToken`/`NotTokenMode`/
  `NotPinMode` errors deleted. The attempt machinery drops its PIN
  qualifier (`PairModeAttempt`/`PairModeCommit`, `begin_attempt`,
  `record_failure`, `begin_commit`, `complete_success`,
  `PAIR_ATTEMPT_LIMIT`); `attempt.secret()` returns the SPAKE2 password
  bytes. New `start_qr_secret[_for_duration]` mints the 256-bit secret.
- `services/pairing.rs`: `pair_by_token` server method,
  `pair_by_token_initiator`, `commit_pairing_token`, and the cloud-only
  `token_pairing_request_reachability` deleted; the SPAKE2 initiator
  (`pair_initiator`) takes `secret: &[u8]`; one `pair_mode_status` mapper.
- QR-pinned TLS verifier mode deleted: `transport/tls.rs` loses
  `QrServerVerification`, `qr_pairing_channel_from_io`, and the
  `expected_pubkey` threading (`tunnel/pool.rs::qr_pairing_channel_via`,
  `connection.rs::cloud_qr_pairing_channel_to`). The initiator verification
  matrix is {trust-pinned, none}; the surviving anonymous-TLS channel
  helpers drop their `pin_` prefix (`pairing_channel*`,
  `pairing_channel_via`, `cloud_pairing_channel_to`).
- `services/client.rs`: `PairPinCloudPeer`/`PairQrCloudPeer` now share one
  `pair_cloud_peer_with_secret` path (channel → SPAKE2 → host-id match →
  commit); the QR pubkey/duplicate-pubkey preflight went with the pubkey.
  `pairing/qr.rs` encodes/parses the new payload; `client/mod.rs` has
  `PairingSecret::QrSecret` and the two-argument `pair_qr_cloud_peer`;
  `amux-cli` renders/consumes the new payload.
- Harness: `testnet/pairing.rs::QrPayload` is `{host_id, secret}`;
  `with_qr` drives the same admin RPC as before. Spec chapter prose updated
  to D9 (no behavioral flips; all 43 specs unchanged and green).
- Kept deliberately: `TunnelTransport::has_cloud_pairing_reachability`
  (the responder still learns `Reachability::Cloud` from the pairing
  tunnel's arrival link) and `StartPairing`'s "QR requires cloud mode"
  check (the minted payload's `cloud_url` is what makes a QR consumable
  today; the wire protocol itself no longer cares).

### Decisions Made
- Wrong-secret on the QR path reports the same opaque `INVALID_PIN` as the
  PIN path: one protocol, one failure surface.
- Lib tests that exercised `PairByToken` were deleted where the behavior
  died with the token (cloud-only ingress, token race/reservation, token
  self-pairing preflights) and rewritten on the unified path where the
  behavior survives (trust-save failure releasing the reservation,
  duplicate-pubkey rejection at staging, pubkey replacement incl. the D10
  in-flight-tunnel teardown, QR-secret one-shot consumption).

### Verification
- `cargo build --workspace` and `cargo build -p amux --features testnet`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed (was 402: token tests deleted,
  several consolidated into unified-path equivalents).
- Spec suite: 43 passed / 0 ignored, ×2 runs (~32.3s each), plus a third
  green run after prose-only spec comment updates.

### Next Steps
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger note: `StartPairing` still refuses QR mode without cloud config —
  revisit if a non-cloud QR consumer path (e.g. LAN rendezvous by host_id)
  ever exists.

---

## 2026-06-11: v6 chunk 3 — every call is a tunnel, with an explicit lifecycle

### Summary
P8 + D3a: the tunnel protocol is now the link lifecycle one layer up —
`TunnelOpen { tunnel_id, src, dst }` · `TunnelData { tunnel_id, dst,
payload }` · `TunnelClose { tunnel_id, dst }`, with `tunnel_id` a plain
16-byte UUID. Only an Open allocates endpoint state (the reply address
travels exactly once, in the Open; cloud rate limiting keys on Opens);
Data for an unknown id is a principled drop — zero allocation, link stays
up — which closes B1 structurally; normal teardown sends TunnelClose
proactively from either end (rejection too: there is no open-ack, the
inner mTLS handshake is the acknowledgement). Relays forward all three
frame types statelessly by `dst`. The dual-path channel discipline is
deleted: every peer call rides a tunnel, including to adjacent peers
(`Route::Direct(link)` materializes a tunnel pinned to that link with
`dst = peer`). Because tunnels are opened by sending frames, and frames
flow both ways, every live link is callable from both ends — both sides
now record a Direct route at link establishment (the multi-tenant cloud
acceptor excepted: the relay records no routes), which makes the
SSH-pairing responder's call-back real (D6).

### Changes
- `proto/amux/v1/amux.proto`: `TunnelId`/`TunnelFrame` deleted;
  `TunnelOpen`/`TunnelData`/`TunnelClose` added; `Message.body` renumbered.
- `tunnel/types.rs`: `TunnelId` is a UUID newtype (initiator/nonce gone —
  "initiated by me" is now an explicit `ActiveTunnel.initiated` flag).
- `tunnel/mod.rs`: initiator endpoints send the Open lazily, just ahead of
  their first Data frame (a never-used tunnel never allocates remotely);
  hosted endpoints never open. `TUNNEL_FRAME_PAYLOAD_MAX` →
  `TUNNEL_DATA_PAYLOAD_MAX` (cap unchanged).
- `tunnel/pool.rs`: one inbound handler per frame type; retired-tunnel
  tombstones (`BoundedTunnelIdSet`, `RETIRED_TUNNEL_CAP`) and the cloud
  tunnel-id cache deleted — with explicit Opens there is nothing for a
  stale Data frame to allocate. Endpoint drop/retirement sends a
  best-effort TunnelClose on the pinned link (`LinkRegistry::
  send_best_effort`); `InboundClosed`/`IncomingTunnelsClosed`/
  `MissingTunnelId` error variants gone. New `channel_on_link` for Direct
  routes.
- `connection.rs`: `ChannelKey::Direct`, `RouteRuntimeState`,
  `register_direct`/`remove_direct` deleted; the pool keys on
  `(peer, Route)` and materialization is one path — open a tunnel.
  NeighborUp gained the same never-eagerly-tunnel-into-the-cloud guard
  ClaimUp had (§6.7).
- `routing/connect/mod.rs`: the `direct_channel` threading is gone;
  `run_established_connect` records `apply_direct_up` on both roles
  (`auth_session.is_none()` — i.e. everyone but the cloud acceptor).
- Harness: `WirePeer` scripts Open/Data/Close; testnet tracks *dialed*
  direct-link TCP sockets (`trusted_device_channel_tracked` +
  `ReachabilityLinkConnector::track_dialed_tcp`) so an in-process restart
  severs them — with acceptors recording routes, a leaked dialer socket
  kept the acceptor seeing a stopped daemon online. `over_ssh` now stands
  the post-pairing SSH link in with the test TCP transport, as production
  dials its stored reachability at commit time.

### Decisions Made
- Pre-planned spec flips: the SSH-responder test asserts
  `server.can_call(&laptop)` over the inbound link; the B1 `#[ignore]`
  marker became a real passing test (open → close → late data: dropped,
  link up); wire scripts rewritten to the new grammar.
- Additional flips, all v6-structural consequences, called out here:
  `direct_beats_cloud_when_both_are_available` — the acceptor now routes
  back over the link itself, not via the cloud;
  `revocation_evicts_routes_and_breaks_in_flight_streams` (was
  `…strands_in_flight_streams`) — streams ride tunnels, tunnels die with
  links/TunnelClose, so the §6.5 stall is gone and the spec-intended
  prompt break is real; the SSH-relay lib test gives the responder a
  pinned entry for the initiator (D4: tunnel mTLS is uniform, SSH links no
  longer inherit transport trust); startup lib tests poll for active
  routes (activation now performs a real tunnel TLS handshake).

### Verification
- `cargo test -p amux --lib`: 402 passed.
- Spec suite (`--features testnet --test spec`): 43 passed / 0 ignored,
  ×2 runs (~32.3s each).
- `cargo build --workspace` clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.

### Next Steps
- Chunk 4 (P2/D9): pairing collapse to one SPAKE2 protocol; delete
  `PairByToken`, `INVALID_TOKEN`, the QR-pinned verifier mode.
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger candidates from this chunk: eager route activation now runs a
  device-TLS handshake serially on the events task (the §6.7 "move
  activation off the event loop" smell got heavier); link-up now costs
  two tunnel TLS handshakes (both ends activate eagerly) even if no call
  is ever made.

---

## 2026-06-11: v6 chunk 2 — route by host id with adjacency-only events

### Summary
The core routing rewrite (D2, D14, the routing-relevant D13 renames;
PROTOCOL_VERSION = 6). Routes are now `Direct(link) | Via(relay_host)` and
the wire carries host-id-addressed frames under two structural rules:
advertise only adjacency (`NeighborUp`/`NeighborDown` claim strictly "I
have/lost a direct link to H") and forward only to adjacency (a frame for
`dst` is forwarded iff a direct link to `dst` exists, else dropped). Route
lists, prepend-on-forward, split-horizon, hop caps, route dedup, wire link
names, and the snapshot/delta phase distinction (`SnapshotComplete`,
pre-activation event buffering) are deleted. The neighbor snapshot is a
field of `Hello`/`HelloAccepted`, reconciled atomically with link
registration. Presence is a two-hop derivation; replies leave on the
tunnel's arrival link, which removes the §6.6 direction-dependent
black-hole structurally. `RoutingService` → `LinkService`. The dual-path
channel discipline (direct channels eager, Via tunnels lazy) survives
until chunk 3 (P8).

### Changes
- `proto/amux/v1/amux.proto`: `Route` message deleted; `TunnelFrame.dst`
  is a host id; `HostUp/HostDown` → top-level `NeighborUp { host }` /
  `NeighborDown { host_id, reason }` (no route field); `RoutingEvent`
  envelope and `SnapshotComplete` deleted; `Hello`/`HelloAccepted` lose
  link names and gain `neighbors`; `service RoutingService` →
  `service LinkService`.
- `routing/route.rs` deleted (898 + 443 lines of route-list machinery with
  `routing/core.rs` rewritten around host entries + claims); registry-level
  adjacency discipline moved into `routing/link_registry.rs::register`
  (snapshot diff + `NeighborUp` fanout under one lock).
- `tunnel/pool.rs`: forwarding is the registry lookup
  (`forward_to_peer`), non-awaiting — a congested peer link gets the
  existing try_send-or-request-close policy instead of stalling the origin
  link's inbound processing (head-of-line guard).
- `connection.rs`: `ChannelKey::{Direct(link), Via{target, relay}}`
  replaces route-keyed pooling; §6.2/§6.7 guards reduced to what the new
  model still needs.
- Startup/cloud (`services/startup/*`, `user_state.rs`): per-user
  registries fan out scoped `NeighborUp`s; the cloud is adjacency, not a
  host — neither side records a host entry for the other.
- Harness/spec: `WirePeer` speaks the v6 handshake; chain tests rewritten —
  `endpoints_call_each_other_through_a_chain_regardless_of_dial_direction`
  (the §6.6 pair collapsed; dial direction no longer matters) and
  `presence_reaches_exactly_two_hops_along_a_chain` (catalog 28b inverted:
  three hops out is deliberately invisible).

### Decisions Made
- Pre-planned spec flips only: 28b inversion, §6.6 collapse, handshake-
  snapshot wire tests. Everything else green with mechanical updates.
- Lib tests that asserted v5 observables were re-pointed at v6 ones:
  rate-limit/forwarding tests count `TunnelFrame` bodies (links also carry
  adjacency events now); cloud-startup tests wait on live links in the
  registries instead of host entries (the cloud never appears as a host).

### Verification
- `cargo test -p amux --lib`: 394 passed.
- Spec suite (`--features testnet --test spec`): 42 passed / 1 ignored,
  ×3 runs (~32.3s each).
- `cargo fmt`; CI clippy (`--workspace --all-targets --features
  amux/testnet -- -D warnings`) clean; `cargo build --workspace` clean.

### Next Steps
- Chunk 3 (P8 + D3a): every call a tunnel, `TunnelOpen`/`TunnelData`/
  `TunnelClose` with UUID ids, delete the dual-path materialization and
  `ChannelKey::Direct`; SSH-responder spec test flips cannot_call →
  can_call.

---

## 2026-06-11: v6 chunk 1 — delete preserve_tunnel_id and GoAway drain; rename GoAway → LinkClose

### Summary
First implementation chunk of protocol v6: the two pure deletions (D10/P3,
D11/P6). `preserve_tunnel_id` — the `Option<TunnelId>` threaded from the
dispatcher through the pairing service into `ConnectionManager` so a
same-host_id/different-pubkey replacement could keep the in-flight pairing
tunnel alive — is gone; teardown now tears down everything, and an initiator
that misses `PairingComplete` re-pairs. The GoAway drain machinery
(`drain_timeout_ms`, draining flags, draining-mode frame filtering, the
`LinkDraining`/`Draining` error variants) is gone, and the wire message is
renamed `GoAway` → `LinkClose { reason, error }`: receiving it means "the
link is closed now, here's why" — no grace period.

### Changes
- `proto/amux/v1/amux.proto`: `GoAway` → `LinkClose` (drops
  `drain_timeout_ms`), `GoAwayReason` → `LinkCloseReason` (values unchanged).
  PROTOCOL_VERSION deliberately not bumped — that lands with the full wire
  rewrite next chunk.
- `connection.rs`, `tunnel/pool.rs`, `services/pairing.rs`, `dispatcher.rs`,
  `transport/io.rs`, `tunnel/transport.rs`: every `*_preserving_tunnel`
  doubled method collapsed to its plain form; `BoxedGrpcAuth::PreTrustPairing`
  and `TunnelTransport` lose their `tunnel_id` fields.
- `routing/connect/mod.rs`: drain deadline/flag/filtering deleted; inbound
  `LinkClose` breaks the loop immediately (`PostHandshakeAction::Drain` →
  `LinkClosed`); `goaway_drain_duration` deleted.
- `routing/link_registry.rs`: `draining` writer flag, `mark_draining`,
  `is_draining`, `LinkRegistryError::Draining` deleted;
  `send_goaway_to_*` → `send_link_close_to_*` (no drain arg).
- `server.rs`: shutdown notification renamed; the 200 ms pre-abort sleep is
  kept as a purely local flush grace (`SERVER_LINK_CLOSE_FLUSH_TIMEOUT`).
- Testnet harness mirror `GoAwayReason` → `LinkCloseReason`,
  `expect_goaway` → `expect_link_close`; spec tests renamed accordingly.
- Tests: drain-behavior tests rewritten to assert immediate-close (or
  link-removed) semantics; the cloud pin/QR pairing startup tests no longer
  preload a stale key (they locked in the preserved-tunnel success); new
  `pair_by_token_replacement_retires_the_in_flight_pairing_tunnel` locks the
  D10 behavior directly.

### Decisions Made
- Per D10, a cloud pairing that triggers key replacement now fails at the
  initiator (timeout today; prompt once D3's TunnelClose lands) and the
  initiator re-pairs. Known v5-interim consequence: after the replacement the
  responder holds no route back to the initiator until a link flap, so the
  immediate re-pair over the same relay can black-hole — the v6 reply rule
  (replies ride the arrival link) removes this class structurally.
- Internal `routing::LinkCloseReason` (writer close requests) keeps its name;
  the wire enum converges on the same name per D11.

### Verification
- `cargo build -p amux --features testnet` clean; lib tests 449 passed; spec
  suite green twice (43 passed, 1 ignored, ~32s each); fmt + CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`) clean.

### Next Steps
- P1+P8 core rewrite: host-id routing, every-call-a-tunnel,
  TunnelOpen/TunnelData/TunnelClose, PROTOCOL_VERSION = 6.

---

## 2026-06-11: Protocol v6 one-pager

### Summary
Concluded the protocol-simplification walkthrough (all proposals from the
networking review resolved) and wrote `docs/PROTOCOL.md` — the v6 target
design as a one-pager. Core decisions: host-id routing with one proxy hop
through any relay (advertise-only-adjacency / forward-only-to-adjacency);
every peer call is a tunnel with pinned e2e mTLS inside (links grant
forwarding only, never call authority); QR pairing collapses into the
SPAKE2 flow as a QR-carried 256-bit secret; neighbor snapshot moves into
Hello/HelloAck (SnapshotComplete deleted); GoAway drain deleted and the
message renamed LinkClose; Reauth kept but ReauthAck deleted
(fire-and-forget refresh, so live sessions are never disturbed); TunnelClose
added; HostUp/Down → NeighborUp/Down, TunnelFrame → TunnelData,
RoutingService → LinkService, PairBySpake2 → Pair.

### Decisions Made
- Full rationale recorded in `notes/PROTOCOL_V6_DECISIONS.md` (D1–D15,
  local working notes). Notable rejections: link-scoped tunnel IDs (forces
  relay remap state), tunnels surviving link replacement (cross-link
  reordering), dropping Reauth entirely (hourly breaks of live cloud
  sessions).
- Amended same day (D3a): tunnel lifecycle made explicit —
  TunnelOpen/TunnelData/TunnelClose with plain UUID ids, no open-ack (inner
  TLS is the ack, TunnelClose the rejection); only Opens allocate endpoint
  state. Replaces implicit-open + `TunnelId{initiator, nonce}`; both
  lifecycle grammars (link/tunnel) are now symmetric.

### Verification
- Design-only change; no code. Spec suite unaffected (expected flips when
  v6 lands are pre-recorded in the decisions notes).

### Next Steps
- Implement in sequence: P3+P6 deletions → P1+P8 core rewrite → P2 →
  P5/P7, spec suite green throughout.

---

## 2026-06-10: Spec suite polish, harness docs, CI wiring

### Summary
Final pass over the spec-test suite: hoisted remaining plumbing out of test
bodies into harness verbs (`Pin::wrong_guess`, `WirePeer::flood_handshakes_
until_rate_limited`), documented the testnet harness at module level, ordered
chapter modules as documentation, and wired the suite into CI.

### Changes
- `crates/amux/src/testnet/` rustdoc on all public items; ~25-line module doc
  covering TestNet/Daemon/WirePeer and the eventually/restart/sever
  disciplines.
- `.github/workflows/ci.yml`: test job runs
  `cargo test -p amux --features testnet --test spec`; clippy covers the
  feature.

### Verification
- Spec suite green twice (43 passed, 1 ignored, ~32s); lib 451 passed; fmt +
  clippy (CI invocation) clean.

### Next Steps
- Decide whether `notes/SPEC_TESTS_DESIGN.md` and `notes/NETWORKING_REVIEW.md`
  (gitignored, referenced from committed code comments) should be force-added.

---

## 2026-06-10: Spec chapters 5–6 — remote sessions, authority, wire conformance

### Summary
Catalog items 30–39: remote agent attach with IO round-trips over direct and
cloud routes, session survival across topology churn, full runtime authority
for paired peers, local-admin RPC rejection over remote transports, and a
`WirePeer` scripted protocol actor exercising the real TCP listener +
dispatcher + TLS for the wire-conformance chapter.

### Changes
- `crates/amux/tests/spec/{sessions,wire}.rs`; `src/testnet/{session,wire}.rs`.
- Production: only `#[cfg(test)]` → `#[cfg(any(test, feature = "testnet"))]`
  widenings in `agents/` and `services/agent/session_rpc.rs`.

### Decisions Made
- Item 38's race half (review finding B1: a benign tunnel-close race can
  escalate to a link-level GoAway) is an `#[ignore]`d test documenting the
  bug; the deterministic unknown-tunnel-drop case asserts the spec'd behavior.

### Verification
- Spec suite 43 passed / 1 ignored across 3 consecutive runs; lib 451 passed;
  fmt + clippy clean.

---

## 2026-06-10: Spec chapters 3–4 — presence, routing & failover

### Summary
Catalog items 15–29 (+28b): presence lifecycle, per-user isolation at the
relay, pairing-candidate scoping, direct-beats-cloud selection, failover in
both directions, make-then-break swaps with in-flight stream breakage,
startup re-establishment, revocation teardown, 3- and 4-node relay chains,
and JWT-expiry continuity driven through the real Reauth path with
short-TTL testnet tokens.

### Changes
- `crates/amux/tests/spec/{presence,routing}.rs`; harness verbs `stop`,
  `sees_offline`, `cannot_call`, pairing-candidate observers,
  `open_event_stream_to` (`RoutedStream`), `.cloud_user(...)`,
  `.trusted(...)`, expiring-JWT support in the testnet relay.
- Product fixes found by the suite (NETWORKING_REVIEW.md §6.7/§6.9):
  `connection.rs` no longer eagerly materializes multi-hop routes to relay
  hosts (head-of-line blocked the routing-event loop for the 10s handshake
  timeout); `tunnel/pool.rs` route removal no longer retires healthy hosted
  inbound tunnels on make-then-break swaps.

### Decisions Made
- Tests lock current behavior where it diverges from docs/NETWORKING.md, with
  NOTE comments: dropped-route streams fail with `h2 protocol error` (not
  UNAVAILABLE); revocation strands in-flight streams both ways; chained
  relaying is dial-direction-dependent (acceptor-side HostUp suppression).

### Verification
- Spec suite 32 passed across 5 consecutive runs; lib 451 passed (incl. new
  regression tests for both product fixes); fmt + clippy clean.

---

## 2026-06-10: Spec chapters 1–2 — identity, trust, pairing; harness hardening

### Summary
Catalog items 1–14: identity persistence across restart, trust locality, all
three pairing flows (PIN direct/cloud, QR, SSH over an in-process stream
seam), wrong-PIN opacity, the 5-attempt cap, pair-mode TTL and one-shot
consumption, self-pairing rejection, and key rotation. Pairing verbs drive
the real local-admin RPCs, not trust-store pokes.

### Changes
- `crates/amux/tests/spec/{identity,pairing}.rs`; `src/testnet/pairing.rs`.
- Harness hardening after a hang hunt: `eventually()` now bounds both the
  condition poll and the failure dump; routed-call verbs are
  timeout-bounded (`can_call` for post-churn assertions); `restart()` severs
  tracked inbound and outbound sockets so peers observe a real process death
  (aborting the connector task alone leaks the established link on both
  ends — documented finding).
- Product fix (NETWORKING_REVIEW.md §6.2): a stale
  `ConnectionManager::activate_route` could demote a fresh direct route and
  permanently strand its pooled channel; activation now refuses
  longer-or-equal demotion.

### Verification
- Spec suite 16 passed, 3 consecutive runs, zero hangs; lib 450 passed.

---

## 2026-06-09: Testnet spec-test harness and smoke tests

### Summary
Built the `amux::testnet` harness (behind a `testnet` feature) and the
`tests/spec` integration target per `notes/SPEC_TESTS_DESIGN.md`: whole-daemon
in-process networks with real localhost TCP + mTLS and a real cloud relay,
declarative `TestNet` builder with trust pre-pairing, eventually-assertions
with topology failure dumps, and operator verbs (cloud outage, link sever,
restart). Two smoke tests cover the canonical example and the operator verbs.

### Changes
- `crates/amux/src/testnet/{mod,net,daemon,assertions}.rs`;
  `crates/amux/tests/spec/`; `testnet` feature + `[[test]] spec` target.
- Production: cfg-gate widenings plus feature-gated accessors and a tracked
  TCP-accept seam in `dispatcher.rs` (no behavior changes).

### Decisions Made
- Assembly generalizes the existing `services/startup/mod.rs` integration
  tests rather than inventing a new path; no skip-TLS anywhere.
- Daemon bring-up is sequenced (direct links, then cloud) to avoid a
  route-activation race discovered during construction — later root-caused
  and fixed as NETWORKING_REVIEW.md §6.2.

### Verification
- `cargo test -p amux --features testnet --test spec` — 2 passed, stable
  across 10 runs; lib 450 passed; clippy + fmt clean.

---

## 2026-05-15: Library cleanup pass after embedded refactor

### Summary
Implemented the four follow-ups from `LIB_CLEANUP.md`: moved CLI refresh-token state out of amux-owned `state.yaml`, replaced library-owned update marker writes with an injected reporter, restored `AgentEntry` as the route-carrying addressable agent type, and removed public `SendInputRequest::input_id`.

### Changes
- `amux-cli` now stores device-flow refresh tokens in CLI-owned `auth.yaml` next to `state.yaml`, using temp-file-plus-rename writes.
- `amux` now exposes `UpdateStatus` and `UpdateReporter`; the server only performs periodic update checks when a reporter is configured.
- CLI update banners and `amux update` marker cleanup now use `MarkerFileReporter` in `amux-cli`.
- Public `Agent` no longer carries `route`; `AgentEntry { agent, route }` is used for list/resolve/create/rename results and `amux-ui` inventory.
- `SendInputRequest` no longer requires callers to fabricate an input id; the protocol encoder synthesizes the protobuf `SessionInput.input_id`.
- Embedded servers configured with an update reporter now run the same periodic available-update check as daemon servers.
- `amux-ui` now emits `AgentUpdated` for route/metadata changes and includes the full `AgentEntry` on rename notifications.

### Decisions Made
- `auth.yaml` is intentionally not migrated from old `state.yaml` refresh-token data because the project is pre-release and the cleanup pass explicitly accepts re-running `amux init`.
- Update marker filenames and banner behavior stayed in the CLI; embedded clients can omit `.update_reporter(...)` and get no periodic update task.
- `amux-ui` now treats agent inventory as `AgentEntry` because UI clients usually need both identity metadata and the route to address sessions/input.

### Verification
- `cargo check --workspace --all-targets` — clean.
- `cargo +nightly fmt --all` and `cargo +nightly fmt --all -- --check` — clean.
- `cargo +nightly clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p amux-ui --test runtime` — 1 passed.
- `cargo doc -p amux-ui --no-deps` — clean.
- `cargo test --workspace` — all workspace tests passed.
- `cargo run -p e2e-runner -- run` — 13 passed, 0 failed.
- `git diff --check` — clean.
- GPT-5.5 xhigh review rounds completed; follow-up fixes added direct coverage for CLI update markers, auth-file replacement, embedded update reporter scheduling, cloud update reporter status delivery, `AgentEntry` wire round-tripping, generated send-input wire IDs, and `amux-ui` route-change reconciliation.
- Cleanup audits: no `refresh_token`, `_extra`, or `CloudState` matches remain in `crates/amux/src`; no legacy update marker helpers remain in `crates/amux/src`; no `agent.route` access remains in `crates/amux/src`, `crates/amux-cli/src`, or `crates/amux-ui/src`; no public `input_id` remains in `amux::SendInputRequest` or `amux-ui`.

### Next Steps
- None for this cleanup pass.

---

## 2026-05-15: Library refactor for embedded clients and amux-ui

### Summary
Refactored the workspace around the new library boundary in `LIB_REFACTOR.md`: `amux` now owns the embeddable server/client core, `amux-cli` owns device flow and daemon spawn-or-attach glue, and the new `amux-ui` crate exposes a command/notification runtime for app-style clients.

### Changes
- `amux` exposes `AccessToken`, `AuthError`, `CredentialProvider`, `Server::builder()`, embedded/daemon builders, cheap-clone `Client`, request-based client operations, `SessionStream`, and routing/agent event streams.
- OAuth device flow and refresh-token persistence moved out of `amux` into `crates/amux-cli/src/auth.rs` as `DeviceFlowProvider`.
- Public `Connection`, `RpcClient`, `ConnectPolicy`, `DaemonOptions`, `connect`, `spawn_daemon`, `run_server`, and `run_server_with_credentials` were removed from the `amux` exports.
- `MemoryTransport` is available for embedded server/client wiring, with `Server::builder().embedded().open()` returning a client over in-process transport.
- CLI subcommands now use `amux::Client`; daemon spawn/wait/startup diagnostics live in `amux-cli/src/client_common.rs`.
- Added `crates/amux-ui` with `Runtime`, `Cmd`, `Notification`, `CmdId`, session phase/failure reasons, and re-exported domain types.
- Added embedded integration coverage in `crates/amux/tests/embedded.rs` and `crates/amux-ui/tests/runtime.rs`.
- Added `PROGRESS.md` to track the refactor work.

### Decisions Made
- Auth refresh stays consumer-owned. The server asks a `CredentialProvider` for access tokens and invalidates rejected tokens; it does not know about device flow or refresh-token storage.
- The CLI spawn-or-attach path remains CLI-specific. `amux::DaemonBuilder::open()` only connects to an existing daemon.
- Embedded mode uses memory transport and an owned guard so the in-process server lifetime is tied to cloned clients.
- `amux-ui` is intentionally v0: it provides bounded notifications, command correlation, basic inventory snapshots/deltas, and refcounted session subscriptions, but not reconnect/backoff or app projection state.

### Verification
- `cargo check --workspace --all-targets` — clean.
- `cargo +nightly fmt --all` — applied.
- `cargo +nightly clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p amux-ui --test runtime` — 1 passed.
- `cargo test --workspace` — all workspace tests passed.
- `cargo doc -p amux-ui --no-deps` — clean.
- `cargo run -p e2e-runner -- run` — 13 passed, 0 failed.
- `cargo +nightly fmt --all -- --check` — clean.
- After making `amux_ui::CmdId` opaque: `cargo check -p amux-ui --all-targets`, `cargo test -p amux-ui --test runtime`, and `cargo +nightly clippy -p amux-ui --all-targets -- -D warnings` — clean.
- Three GPT-5.5 xhigh code review rounds were completed. Follow-up fixes landed for state preservation of CLI-owned refresh tokens, daemon-safe `amux-ui` shutdown, session detach cancellation/refcounting, embedded cloud startup and credential validation, retriable provider errors, terminating stream adapters, and embedded shutdown/suspend lifecycle behavior.
- Final post-review validation: `cargo check --workspace --all-targets` — clean; `cargo +nightly clippy --workspace --all-targets -- -D warnings` — clean; `cargo test --workspace` — 329 amux-lib tests plus embedded/CLI/UI/e2e-runner/test-agent/doc tests passed; `cargo doc -p amux-ui --no-deps` — clean; escalated `cargo run -p e2e-runner -- run` — 13 passed, 0 failed; `cargo +nightly fmt --all -- --check` and `git diff --check` — clean.
- Cleanup audit found no `oauth`/`OAuth`/`refresh_token` matches in `crates/amux/src` and no legacy public API names (`RpcClient`, `ConnectPolicy`, `DaemonOptions`, public `connect`/`run_server`) in the workspace source.

### Next Steps
- None for this refactor. Future `amux-ui` work can add reconnect/backoff and richer host-agent event projection as app requirements harden.

---

## 2026-04-17: Negotiated idle timeout for heartbeats

### Summary
Replaced the hardcoded `60s idle + 10s ack_timeout` heartbeat model with a single negotiated `idle_timeout`. The acceptor publishes the timeout in `ConnectResult` (from config, default 180s), both peers drop the connection on inbound silence past the timeout, and the dialer keeps it alive by sending `Heartbeat` at its own cadence (currently `idle_timeout / 3`, not on the wire). `HeartbeatRole::Disabled` is gone — absence of a negotiated timeout (Unix sockets) means heartbeats are off entirely.

### Changes

**Protocol**
- `protocol/handshake.rs` — `ConnectResult` gains `idle_timeout_secs: Option<u32>`. `None` means heartbeats disabled. Added a serde-default test so older payloads decode with `None`.
- `transport/handshake.rs` — `connect_handshake` now returns `HandshakeOutcome { link, idle_timeout_secs }` instead of just `Link`. Dialer-side callers extract the negotiated value.

**Config**
- `config.rs` — new `idle_timeout_secs: u32` field, default 180 via `default_idle_timeout_secs()`.

**Heartbeat model**
- `server/connection/context.rs` — dropped `HeartbeatRole::Disabled`. `HeartbeatRole` is now just `{ Dialer, Acceptor }`. Added `HeartbeatSetup { role, idle_timeout }`. `ConnectionContext::heartbeat_role` became `heartbeat: Option<HeartbeatSetup>` — `None` = disabled.
- `server/connection/heartbeat.rs` — removed `HEARTBEAT_IDLE_INTERVAL`, `HEARTBEAT_ACK_TIMEOUT`, `HeartbeatConfig`, `heartbeat_config_for_role`, `DialerHeartbeatState::probe_deadline`, `HeartbeatState::pause_for_refresh`, `HeartbeatState::ack_pending`, `HeartbeatState::note_inbound_activity`. Inbound tracking lives solely in `ConnectionActivity::last_inbound_at` — `HeartbeatState::deadlines` now takes `&ConnectionActivity` and computes the kill deadline from it. The dialer keeps its own `last_tx_at` for the preemptive send-deadline update, since that can't be derived from the write-callback-driven `last_outbound_at` without racing. Switched `HeartbeatState` to struct-style variants.
- `server/connection/driver.rs` — merged `connection_loop_with_heartbeat` back into `connection_loop` (setup is on the ctx). Removed `pause_for_refresh` calls and the redundant `heartbeat.note_inbound_activity()` call. Log helpers no longer reference `ack_pending` or the dead `Disabled` arm.

**Plumbing**
- `server/accept.rs` — acceptor reads `config.idle_timeout_secs` (`None` for Unix) and passes it into `accept_handshake`, which echoes it in the success `ConnectResult`. Constructs `HeartbeatSetup` for the context.
- `auth/cloud.rs` — `CloudConnection` stores `idle_timeout_secs` from the cloud server's `ConnectResult` and exposes it via `idle_timeout_secs()`.
- `server/cloud.rs` — consumes `CloudConnection::idle_timeout_secs()` to build the dialer's `HeartbeatSetup`.

**Tests**
- `server/connection.rs` — rewrote heartbeat tests for the new symmetric model: `dialer_times_out_on_inbound_idle`, `acceptor_times_out_when_peer_is_silent`. Removed tests that exercised the dead `probe_deadline` / `pause_for_refresh` paths.
- `protocol/handshake.rs` — new test confirms older `ConnectResult` payloads missing `idle_timeout_secs` decode to `None`.
- Test helpers updated throughout (`runtime.rs`, `handlers/tests.rs`, `handlers/command.rs`).

**Docs**
- `ARCHITECTURE.md` — rewrote the heartbeat paragraph to describe the negotiated timeout and symmetric kill rule.

### Decisions Made
- **Acceptor publishes, dialer adopts.** The acceptor is authoritative because it's the party that will actually drop the connection. A two-way proposal was discussed (Connect proposes a client-side timeout, take min) but there's no current caller that benefits, so it's one-sided.
- **Cadence is not wire-level.** Dialer picks its own heartbeat interval (T/3 today). Letting it drift or tune per-transport doesn't require a protocol change.
- **No `ack_timeout`.** The old 10s probe window caught silent peers faster than the idle interval alone, but the acceptor already replies to every `Heartbeat` with `HeartbeatAck`, so the dialer's inbound-idle timer naturally fires within `idle_timeout` of any dead peer. One timer, one knob.
- **Default 180s, not 60s.** The old default was aggressive for networks with transient blips. Three minutes gives more tolerance; users can tune via `idle_timeout_secs` in `config.yaml`.
- **`Option<HeartbeatSetup>` over keeping `Disabled`.** Dropping the third enum variant eliminated the `unreachable!("disabled connections...")` arm and an entire log-state branch. Absence-as-disabled is the tidier model.

### Verification
- `cargo check --workspace --all-targets`: clean
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test -p amux --lib`: 305 passed, 0 failed
- `cargo run -p e2e-runner -- run`: 12 passed, 0 failed

### Next Steps
None.

---

## 2026-04-17: Init flow + cloud-state reshape

### Summary
Walked back yesterday's `CloudState` enum in `state.yaml`. The enum mixed a user preference ("do I want cloud mode?") with a runtime credential (refresh token), and its `NotConfigured` / `Unauthenticated` variants encoded init-flow state into what should just be cloud state — which is why past AI contributors assumed init was cloud-specific. Split the two concerns, rewrote `amux-cli/src/init.rs` as a pure-function state machine, and extended implicit init coverage to `amux server start`.

### Changes

**Data split**
- `state.rs` — `CloudState` enum deleted. Replaced with a `CloudState` *struct* namespaced under `cloud:` in `state.yaml` (mirrors the existing `claude:` block), currently just `{ refresh_token: Option<String> }`. The struct is a pure runtime-credential container — no preference or phase state. Future cloud-runtime fields have a natural home.
- `config.rs` — added `enable_cloud_mode: Option<bool>`. Changed `prevent_idle_sleep: bool` → `Option<bool>`. `None` means "user has not been prompted"; `Some(true/false)` means "user gave an explicit answer." Runtime consumers (`server/runtime.rs:102`) `.unwrap_or(false)` at the call site; by design init always runs before these are read.
- `setup.rs` — public `CloudState` mirror, `cloud_setup_state`, `reset_cloud_state`, and the smelly `prevent_idle_sleep_preference` raw-YAML reader are gone. New helpers: `cloud_enabled(config)`, `cloud_refresh_token(config)`, `set_enable_cloud_mode`, `set_prevent_idle_sleep`, `clear_enable_cloud_mode`, `clear_prevent_idle_sleep`, `set_cloud_refresh_token`. One generic `write_config_bool(key, Some/None)` backs the set/clear pair for each config field.

**Init as a state machine**
- `amux-cli/src/init.rs` — rewrote `run_init` around a pure `next_step(config, has_refresh_token, ctx) -> InitStep` function. The whole state-machine graph lives in one place; step functions (`prompt_cloud_mode`, `authenticate`, `prompt_idle_sleep`) only know how to execute their step, not whether they should run. The loop calls `next_step`, dispatches, and re-evaluates until `InitStep::Done`. No more per-step `needs_*` predicates, no re-reading config after each step to "refresh" status.
- New `InitContext { explicit: bool }` struct threaded through `run_init`. `amux init` passes `InitContext::explicit()`; `ensure_initialized` passes `InitContext::implicit()`. Today's three steps ignore the flag — it's there so future "show prompt"-style steps (e.g. a "will you use Claude?" prompt) can self-gate inside `next_step` without touching call sites.

**Entry points**
- `amux-cli/src/main.rs` — `ensure_initialized` now runs on `amux server start` as well (was previously a gap; server started regardless of init state). Init runs in the interactive CLI process before daemon spawn.
- `ensure_initialized` fast path: a pure `next_step` check against the in-memory config plus one `state.yaml` read. Zero extra IO when init is already done.
- When init is needed but a server is already running, skip prompting. The running server's config is frozen at startup — persisting new answers now would confuse the user. Added `server_client::server_is_running(config) -> bool` as a thin wrapper over the existing `existing_server` probe. The probe only runs in the rare init-needed case, not on every command.
- Explicit `amux init` with a running server: run init normally, then print a "restart the server to apply" notice at the end.

**Call-site follow-ups**
- `auth/cloud.rs` — reads `state.cloud_refresh_token` directly; uses `setup::cloud_enabled(config)` in `CloudConnection::connect`.
- `server/cloud.rs`, `server/debug.rs` — `state.cloud.is_enabled()` → `setup::cloud_enabled(config)`.
- `amux-cli/src/main.rs:check_upgrade_required` — same substitution.
- `e2e-runner/src/executor.rs` — fixture now sets `enable_cloud_mode: false` + `prevent_idle_sleep: false` in the generated `config.yaml` instead of seeding `cloud: { status: disabled }` in `state.yaml`.

### Decisions made

- **`Option<bool>` on `Config`, not raw-YAML sniffing for absence.** Previously `prevent_idle_sleep: bool` paired with a `prevent_idle_sleep_preference()` helper that read the deserialized-config-bypassing raw YAML mapping to distinguish "unset" from "false." Two code paths reading the same field differently; init and runtime drifted. Making both `enable_cloud_mode` and `prevent_idle_sleep` `Option<bool>` on `Config` itself is honest about the tristate at the type level and collapses to one read path.
- **Pure `next_step` over mutation-based control flow.** Yesterday's init flow was `let mut status = cloud_setup_state(...); if matches!(status, NotConfigured) { ...; status = cloud_setup_state(...); }` — mutating state and re-reading to advance. The new design is a loop around a pure function that maps current state to "what runs next"; steps only own their own prompt/persist work. Adding a step is three additions (enum variant, `next_step` arm, `run_init` arm); future steps that only apply in explicit init gate on `ctx.explicit` inside `next_step`.
- **Plugin install stays a separate call-site step.** The three-line cadence at `amux new claude` (`ensure_initialized` → `check_upgrade_required` → `ensure_plugin_installed`) reads cleanly and keeps the three concerns independent. Folding plugin install into the init loop would have required threading `agent_type` through `InitContext` for a concern that's already command-specific and idempotent.
- **Probe-and-skip only when init is needed.** The socket connect is cheap (~1ms) but not free. `next_step` is pure and ~instant; running it first means the probe only happens in the rare "init incomplete but something else is going on" case.

### Verification
- `cargo check --workspace` — clean.
- `cargo +nightly fmt --all` — applied.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 307 amux-lib tests (up from 304, added six `next_step` + setup-helper tests, retired the old raw-YAML preference reader tests). 18 amux-cli tests pass (added five `next_step` pure-function tests). All other targets clean.
- `cargo run -p e2e-runner -- run` — 12 passed, 0 failed.

### Next steps
- None blocking. Future "will you use Claude?" preference prompt is now an additive change: new `InitStep` variant, new arm in `next_step` gated on `ctx.explicit && config.claude_preference.is_none()`, new step function. No call-site migration needed.

---

## 2026-04-17: Review follow-ups — LinkName→Link rename, three panic fixes

### Summary
Addressed review comments on the Idiomatic-Rust refactor. Renamed `LinkName` → `Link` / `link_name` field → `link` across the code (kept `InvalidLinkName` and `LinkNameTaken` error variants). Fixed three reachable panics / regressions the test suite hadn't caught: `Route::deserialize` panicking from `AgentRegistry::resolve`, `Connect::decode` rejecting malformed link names before the server could reply with `InvalidLinkName`, and `generate_server_link` panicking on empty `host_name`. Removed the silent-ID-minting `Default` impl on `SubscriptionId` and renamed its constructor `new()` → `random()` to avoid the `new_without_default` lint without reintroducing the footgun.

### Changes

- **Rename** (`\bLinkName\b → Link`, `\blink_name\b → link`, word-boundary preserves `InvalidLinkName`, `LinkNameTaken`, `randomise_link_name`, `validate_link_name`, `link_name_rejects_period`): applied across 28 `.rs` files. One shadowing collision fixed in `server/connection/driver.rs` (`Some(link) if *link == link` → `Some(hop) if *hop == link`).
- **Fix C1** `server/registry.rs`: `Route::deserialize(...).expect("Route deserialization cannot fail")` → `Route::deserialize(...).ok()?`. Malformed route-qualified identifiers like `"bad..hop:agent"` are lookup misses, not panics.
- **Fix C2** `protocol/handshake.rs` + `server/accept.rs`: reverted `Connect.link` to `String` so `Connect::decode` accepts empty/dotted names and `validate_link_name` can reply with `ProtocolError::InvalidLinkName`. Construction into `Link` happens after validation (`Link::new(proposed_link).expect(...)` — validator enforces the invariants). Added `connect_decodes_invalid_link_names` regression test. Updated `transport/handshake.rs` and `auth/cloud.rs` to encode `link.as_str().to_string()`.
- **Fix C3** `config.rs:Config::validate`: added non-empty check for `host_name`. `generate_server_link` was calling `Link::new("").expect(...)` → panic during outbound cloud/peer connect when `host_name: ""` + `randomise_link_name: false`. Added `validate_rejects_empty_host_name` regression test.
- **S1** `protocol/message/common.rs`: removed `impl Default for SubscriptionId` (silent random-UUID minting was a footgun via `#[derive(Default)]` / `unwrap_or_default()`). Renamed `SubscriptionId::new()` → `SubscriptionId::random()` to avoid the `new_without_default` clippy lint without reintroducing `Default`. ~15 call sites updated.

### Decisions made

- **Keep `InvalidLinkName` / `LinkNameTaken` error variants unchanged.** The error describes an issue with the *name string* (non-empty, no `.`, collision), not the link itself.
- **`Connect.link` is `String` on the wire, `Link` in memory.** The newtype invariant is enforced at the server boundary after `validate_link_name` succeeds. Moving the invariant into serde (which the refactor attempted) turned a structured `ProtocolError::InvalidLinkName` reply into a generic invalid-handshake disconnect — a behavioral regression on malformed input.
- **Replaced `SubscriptionId::new()` with `random()` rather than re-adding `Default`.** The CLAUDE.md "no `#[allow(clippy::...)]`" policy ruled out suppressing `new_without_default`; the rename is more explicit anyway.

### Verification
- `cargo check --workspace --all-targets` — clean.
- `cargo +nightly fmt --all` — applied.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 304 lib tests passed (up from 302; added the two regression tests above), all other targets clean.
- `cargo run -p e2e-runner -- run` — 12 passed, 0 failed.

### Next steps
- None blocking. Reviewer-1 polish items (dead `try_routable` decision, `remove_subscription_if_reply_failed` inline, `Message::type_label` delegating to `Command::type_label`, doc comments on validation layering) are left for an opportunistic pass.

---

## 2026-04-16: Idiomatic-Rust refactor — follow-up pass

### Summary
Continued the refactor plan from `notes/amux-refactor-plan.md`, landing the remaining items that pass the "encodes a real invariant" bar. Also reverted four plan items that turned out to be ceremony over substance: `#[non_exhaustive]`, `AgentType` / `StructuredProtocol` enums on propagated protocol fields, `SubscriptionCancel` newtype around `oneshot::Sender<()>`, and `Arc<Config>` in `ServerState`. Updated the plan to capture why.

### Changes

**Phase 3 — type modeling**
- **`CloudState` phase enum** (`state.rs`, `setup.rs`, `auth/cloud.rs`, `server/{debug,cloud}.rs`, `amux-cli/src/{init,main}.rs`): replaced the `Option<bool>` / `Option<String>` pair with a 4-variant tagged enum (`NotConfigured`, `Disabled`, `Unauthenticated`, `Authenticated { refresh_token }`). Added `is_enabled`, `needs_init`, `refresh_token` methods. Collapsed four states (one illegal) to exhaustive variants. `state.yaml` wire format changed (`use_cloud_mode: bool` → `status: enum`) — acceptable pre-release. `e2e-runner/src/executor.rs` fixture updated to the new format.
- **`LinkName` newtype** (new `protocol/link.rs`): non-empty, no `.`, validated at construction. Custom `Serialize` (bare string) to preserve `Route`'s dotted wire format; `Deserialize` validates. `Borrow<str>` for `HashMap`/`HashSet` lookups by `&str`. Threaded through `Route` internals (`VecDeque<LinkName>`), `Connect.link_name`, `ShutdownRequest::{Shutdown,Suspend}.link_name`, `TokenRefreshState.link_name`, `ConnectionContext.link_name`, `ServerUserState.routes`/`peer_links` keys, `generate_server_link`/`generate_terminal_link` return types. **`Route::from_link(link: LinkName) -> Self` is now infallible by construction** — the `debug_assert!` chain is gone. ~40 call sites updated across the amux + amux-cli crates + e2e-runner.

**Phase 4 — visibility**
- `notify_other_clients` narrowed from `pub(in crate::server)` to `pub(super)` — it's only called from inside `runtime.rs`. Remaining `pub(in crate::server)` items verified to have cross-submodule consumers.

**Phase 5 — RAII extractions**
- **`ServerUserState::try_reserve_link(link_name)`** method on the type that owns the routes map. Atomically test-and-inserts a new connection handle; returns the reserved `ConnectionHandle` + `Receiver`, or the original `LinkName` on conflict. `server/accept.rs` replaces its read-then-write double-check pattern (with explicit `drop(us)`) with a single write-lock call to the new method.
- **`notify_local_clients(user_state, reason)`** helper in `server/runtime/notify.rs`, paired with the existing `notify_other_clients`. `server/cloud.rs` replaces its inline `let us = user_state.read().await; for ...; drop(us); sleep; exit` block with a call to the helper.
- **Test-scope block-scope rewrite**: swept 33 `drop(msgs)` / `drop(us)` test patterns into `{ let x = ...; ... }` blocks across `server/routing/tests.rs`, `server/handlers/{routable,direct,command}.rs`, `server/runtime.rs`. The lock guard now falls out of scope at the block boundary — no explicit drop, no chance of accidentally holding across an `.await` added later.
- **`server/connection/driver.rs:119`**: `drop(cancel_subscriptions_matching(...))` rewritten as `let _cancelled = ...` with a comment explaining that dropping the returned `Vec<SubscriptionEntry>` fires each entry's cancel sender. Intent-as-name instead of intent-as-drop.

### Reverted items (with plan updated to explain why)

- **`#[non_exhaustive]` on 13 public enums** (Phase 1 from the prior commit): removed. Would have forced downstream `_ =>` arms that silently absorb future variants; for the amux protocol we *want* downstream to fail to compile when a new variant is added so every call site handles it explicitly.
- **`AgentType` / `StructuredProtocol` as typed wire enums**: reverted mid-session. `agent_type` and `structured_protocol` on `DirectMessage::AnnounceAgent` and `RoutableMessage::SubscribeStructuredResult` stay as opaque `String`s because intermediate routing servers propagate them without parsing — only the source and sink of a given flow need to agree on the value. Typing them would force every relay to understand every variant, which is the opposite of the design. Plan updated with this architectural invariant.
- **`SubscriptionCancel` newtype around `oneshot::Sender<()>`**: attempted, then reverted. `oneshot::Sender::drop` is already the RAII cancel signal. Wrapping it in a `fn cancel(self)` consume-method is pure ceremony — same runtime behavior, added API surface, no new type-level guarantee. The existing `drop(cancel)` call sites are fine as-is.
- **`Arc<Config>` in `ServerState`**: attempted, then reverted. The "10 clones saved" claim was wrong: only two whole-struct `Config::clone()` calls exist in the codebase (in the cloud-connect path, which runs once at startup and on rare reconnects). The remaining "config clones" are field-level clones (`state.config.host_name.clone()`, etc.) that Arc doesn't help with — those still produce owned `String`/`PathBuf`. Threading `Arc<Config>` through four type signatures for a one-off startup cost fails the guide §14 test: `Arc<T>` is for "shared ownership genuinely in the model," not reflexive wrapping.

### Decisions made

- **Don't wrap a type to make its intent "more explicit" if the wrap adds no compile-time guarantee.** Rust's RAII, `Option`, and channel-drop semantics are already expressive. Wrapping them in a facade is ceremony that fragments the vocabulary without buying safety. This killed the `SubscriptionCancel` and `Arc<Config>` items, and guided the decision to skip `AgentName`, `HostName`, and `Arc<LinkName>`.
- **Opacity is an architectural invariant, not a backwards-compat concern.** Even with all peers on the same version, intermediate routing servers should not need to parse `agent_type` or `structured_protocol` to propagate an `AnnounceAgent`. Pluggability of agent kinds lives at the endpoints, not at every hop. This principle is now documented in the refactor plan to prevent the same mistake next time.
- **Test drops become block scopes, not helpers.** A closure-based `with_locked(|msgs| { ... })` would structurally prevent hold-across-await bugs, but the test bodies vary too much for a single helper, and block scopes are the minimal intervention that achieves the same release-at-end semantics. 33 sites rewritten mechanically.
- **Skipped `AgentName`, `HostName`, `Arc<LinkName>`**: their validation invariants are weak (non-empty names nobody constructs, derived host-name limits already enforced at the link layer) and they primarily trade one kind of ceremony for another. If profiling later identifies a specific clone or swap-bug hotspot, address it then.

### Verification
- `cargo check --workspace` — clean.
- `cargo fmt` — applied.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p amux --lib` — 302 passed, 0 failed.
- `cargo run -p e2e-runner -- run` — 12 passed, 0 failed.

### Next steps
- Nothing blocking. The plan in `notes/amux-refactor-plan.md` reflects what was done and what was deliberately skipped. If clone traffic becomes a measured problem later, profile first and Arc-wrap the specific value the profile points at.

---

## 2026-04-16: Idiomatic-Rust refactor pass (phases 1, 2, 4, partial 3 & 5)

### Summary
Executed the multi-phase refactor plan from `notes/amux-refactor-plan.md` in a single commit. Completed the quick wins, all four structural splits, the visibility sweep, and the two most self-contained items from the type-modeling and RAII phases. Deferred the remaining Phase 3 newtypes (LinkName, AgentName, HostName, CloudState, AgentType enum-everywhere, StructuredProtocol) and the bulk of the Phase 5 redesigns (test helpers, SubscriptionCancel, try_reserve_link, Arc<Config>, by-value bindings) since each would touch 15–50+ sites and warrants its own pass.

### Changes

**Phase 1 — quick wins**
- Added `Route::len()` alongside `is_empty()`.
- Removed the `lib.rs` `pub use protocol::Route` hybrid and the `agent.rs` `pub(crate) use claude::ClaudeSession` hybrid; updated `amux-cli/src/session_client.rs` and `agent/session.rs` to use the canonical module paths.
- Converted 5 invariant-assertion `.unwrap()` calls to `.expect("…")` with messages in `server/connection/driver.rs` and `server/routing/naming.rs`.
- Removed 5 no-op `drop(permit)` calls in `server/runtime.rs` (the async block already drops the permit on exit).
- Added intent comments to the remaining deliberate `drop(…)` sites (`agent/pty.rs::slave`, `server/runtime.rs::listeners`, `client/connection.rs::send`).

**Phase 2 — structural splits**
- Split `crates/amux/src/protocol/message.rs` (836 lines) into a facade plus `message/{common,routable,direct,command,envelope}.rs`.
- Split `crates/amux/src/server/runtime.rs` free helpers into `runtime/{notify,forward,sweep,events}.rs`; `Server::{with_config, run}` stays intact.
- Split `crates/amux/src/server/handlers/routable.rs` into a facade plus eight per-arm files under `routable/{subscribe_raw,subscribe_structured,extend,unsubscribe,create_agent,rename_agent,delete_agent,io}.rs`; the big dispatch match and tests stay in `routable.rs`. `io::handle_structured_input` takes a `StructuredInputReply` struct to stay under the clippy arg limit.
- Split `crates/amux/src/server/handlers/direct.rs` into a facade plus `direct/{reauth,agent,host}.rs`; trivial arms (Heartbeat, HeartbeatAck, InitialSyncComplete, ReauthResult, Unknown) stay inline.

**Phase 3 — type modeling (partial)**
- Promoted `SubscriptionId` from `type SubscriptionId = Uuid` to a `#[serde(transparent)]` newtype in `protocol/message/common.rs` with `new`, `nil`, `as_uuid`, and `Default`/`Display` impls. Updated ~30 call sites across handlers, connection, tests, and transport to construct via `SubscriptionId::new()` / `SubscriptionId::nil()` and index into it via `.as_uuid()` where needed. Wire format bytes unchanged.
- Added `Message::try_routable(…) -> Result<…>` as the fallible constructor for routable messages; `Message::routable(…)` now delegates to it and keeps the panic on encode failure.
- Deferred: `LinkName`, `AgentName`, `HostName`, `CloudState` phase enum, `AgentType`-everywhere, `StructuredProtocol`. Each is a multi-file change (15–50 sites) that merits its own commit.

**Phase 4 — visibility sweep**
- Demoted `pub(crate)` items in `server/state.rs`, `server/registry.rs`, `server/debug.rs`, and the new `server/runtime/{forward,sweep,events}.rs` to `pub(in crate::server)` so they are only visible within `crate::server`.
- Updated `server.rs` facade re-exports to `pub(in crate::server) use …` for internal items and kept `pub(crate) use runtime::Server` / `pub use runtime::ServerError` for the items that must cross the server boundary.
- Left `TcpMessageReader/Writer` and `WsMessageReader/Writer` at `pub(crate)` because they're named in `TransportSplit` associated types (E0446 would fire on any narrower visibility).

**Phase 5 — RAII + clone redesigns (partial)**
- `PtyHandle::resize` now takes a `TerminalSize` instead of two `u16` args. Updated the single in-tree caller.
- Split the JWKS cache-check into `JwtValidator::is_cache_fresh` + `JwtValidator::refresh_jwks`, with `ensure_jwks_fresh` as the two-line composition. Removes the awkward `drop(last); … write().await` pattern.
- Deferred: test-helper extraction (42 `drop(g)` sites), `SubscriptionCancel` newtype, `ServerUserState::try_reserve_link`, `Arc<Config>` in `ServerState`, by-value `AnnounceAgent`/`AnnounceHost` bindings, `Arc<LinkName>` in `ConnectionContext` (depends on deferred `LinkName`).

### Decisions Made
- **Single commit as instructed**, contrary to the plan's 15–19 PR breakdown.
- **Prod LOC, not total LOC**, is what drives split decisions: the plan's own "oversized" list already excluded `handlers/command.rs`, `buffer.rs`, `server/connection.rs`, and `server/registry.rs` because their prod surfaces are small, and `server/runtime.rs::run()` because it's sequential ceremony around a `tokio::select!`.
- **`pub(in crate::server)` over `pub(super)`** for the server facade re-exports. At the crate root, `pub(super) use` equals `pub(crate) use`, which is strictly wider than the `pub(in crate::server)` source items; the compiler rejects the widening. Using `pub(in crate::server)` on both sides keeps the contract tight.
- **Kept transport Reader/Writer types at `pub(crate)`**: they're exposed via `TransportSplit`'s associated types, which enforces visibility ≥ the trait's.
- **Deferred the heavy Phase 3 newtypes** rather than rushing them: each one is a cross-cutting rename across 15–50 call sites, and getting them wrong in a single monster commit is harder to review and bisect than skipping them now.

### Verification
- `cargo check` — clean.
- `cargo fmt` — applied (6 files reformatted by the import-grouping config).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --lib -p amux` — 299 passed, 0 failed.
- `cargo build --workspace` — clean.
- `cargo run -p e2e-runner -- run` — 12 passed, 0 failed.

### Next Steps
- Revisit the deferred Phase 3 newtypes as separate passes: `LinkName` first (unlocks infallible `Route::from_link` per plan §3 Option B), then `AgentName`, `HostName`, `AgentType`-everywhere, `StructuredProtocol`, `CloudState` enum.
- Revisit deferred Phase 5 items as separate passes: test-helper extraction collapses 42 `drop(g)` sites; `Arc<Config>` / by-value bindings / `Arc<LinkName>` eliminate the clone hotspots the plan identified.

---

## 2026-04-16: Finish post-facade housekeeping and tighten the remaining internal seams

### Summary
Completed the follow-up cleanup after the facade/module split. The server and Claude session giants were split where the seams were real, handler tests were moved next to their implementations, `super::super::` imports were replaced with absolute crate paths, `get_`-prefixed action names were corrected, the remaining internal-only error plumbing and visibility leaks were reduced, and the HOME-less XDG fallback no longer points at a shared temp-root `amux` directory.

### Changes
- **Import and naming cleanup** — Replaced nested `super::super::...` imports under `server/handlers/` and `server/routing/` with `crate::server::...` paths, and renamed `get_connection`/`refresh_and_get_connection`/`get_or_create_user_state` to `fetch_connection`/`refresh_and_fetch_connection`/`ensure_user_state`.
- **Handler test colocation** — Split the old monolithic `crates/amux/src/server/handlers/tests.rs` into per-handler `#[cfg(test)]` blocks in `command.rs`, `direct.rs`, `routable.rs`, and `subscription.rs`, keeping only shared fixtures and the genuinely cross-cutting test in the shared test module.
- **`server/connection` split** — Broke `crates/amux/src/server/connection.rs` into a facade plus `context.rs`, `driver.rs`, `heartbeat.rs`, `reauth.rs`, and `subscription.rs`, keeping the message loop, heartbeat policy, and token-refresh flow as separate units with narrower visibility.
- **`agent/claude/session` split** — Broke `crates/amux/src/agent/claude/session.rs` into a facade plus `core.rs`, `hooks.rs`, `input.rs`, and `name_sniffer.rs`, isolating lifecycle, hook handling, structured input, and name sniffing.
- **Error and visibility audit** — Removed the crate-wide internal `AgentError`, collapsed `CreateAgentError` and `LocalAgentRenameError` into boundary strings backed by contextual internal errors, dropped the dead `HookError::Handling` variant, narrowed `OAuthError` and `StateError`, made route-link generators crate-internal, and reduced the generated macOS IOKit bindings from `pub` to `pub(super)`.
- **HOME-less path fallback** — Changed `crates/amux/src/paths.rs` so missing home-directory variables now fall back to the relative XDG suffix (for example `.local/state`) instead of the shared temp root, avoiding cross-user state/config collisions in scrubbed environments.

### Decisions Made
- **Split only where the seams were real**: `server/connection` and `agent/claude/session` were split because the resulting files have coherent responsibilities; this was not done as a line-count exercise.
- **Keep concrete errors when callers branch on shape**: `ConnectError`, `CloudError`, `HandshakeError`, `ConnectionError`, `SubscribeError`, `AgentRegistryError`, and the public-facing config/setup/transport errors stayed concrete because callers match on specific variants.
- **Collapse plumbing that only became strings**: agent startup/rename internals now use contextual internal errors and convert to user-facing messages only at the module boundary that actually needs them.
- **Facade ownership stays explicit**: deep modules no longer expose `pub` names unless that path is intentionally public; helper paths and generated bindings were reduced to the narrowest visibility that still compiles.

### Verification
- `cargo check`
- `cargo fmt`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test`
- `cargo build --workspace`
- `cargo run -p e2e-runner -- run`
- Result: all checks passed. The E2E run needed an unrestricted rerun because the sandbox could not allocate local TCP ports.

### Next Steps
- The refactor follow-through requested in the housekeeping pass is complete.
- Remaining work should be feature-driven or come from new concrete pain points rather than more structural cleanup.

## 2026-04-16: Complete refactor-plan boundary cleanup and split the remaining server giants

### Summary
Finished the refactor-plan pass that turns the earlier domain reshuffle into real compiler-enforced boundaries. All `crates/amux/src/**/mod.rs` files are gone, the layering and suspend/resume cycles are broken, `server/handlers.rs` and `server/routing.rs` are now thin facades with responsibility-based submodules, the Claude hook API exposed to the CLI was reduced to a narrow helper instead of the full enum, large inline server test blocks were moved into sibling test files, and internal leaf modules were tightened to `pub(crate)` or narrower unless they are part of a true public API.

### Changes
- **Module facades** — Replaced `mod.rs` with named roots across `agent`, `auth`, `client`, `protocol`, `server`, `sleep_inhibitor`, and `transport`, and kept the new root files thin.
- **`crates/amux/src/paths.rs`** — New shared XDG/path helper module used by config/state/log path code.
- **`crates/amux/src/protocol/agent.rs` / `crates/amux/src/agent/session.rs`** — Moved `From<agent::Agent> for protocol::Agent` out of `protocol` and removed serde derives from the internal agent metadata type.
- **`crates/amux/src/transport/handshake.rs` / `client/connect.rs` / `server/accept.rs`** — Extracted neutral handshake logic out of the client/server domains.
- **`crates/amux/src/suspend.rs` / `crates/amux/src/agent/session.rs` / `crates/amux/src/agent/claude/session.rs` / `crates/amux/src/agent/test_agent.rs`** — Broke the suspend/agent cycle by keeping suspend DTOs/filesystem logic in `suspend.rs` and recreating sessions inside the agent layer.
- **`crates/amux/src/server/registry.rs`** — Removed the extra stored wrapper and now track `Agent` directly.
- **`crates/amux/src/server/handlers/`** — Split message handling into `command.rs`, `direct.rs`, `routable.rs`, and `subscription.rs`; `handlers.rs` is now a 49-line facade and the large test block moved to `server/handlers/tests.rs`.
- **`crates/amux/src/server/routing/`** — Split agent lifecycle, peer propagation, and naming into `agents.rs`, `peers.rs`, and `naming.rs`; `routing.rs` is now a 30-line facade and its tests live in `server/routing/tests.rs`.
- **`crates/amux/src/agent/log_source.rs`** — Moved `StructuredLogSource` out of `agent/claude/` so the test agent no longer depends on Claude internals.
- **`crates/amux/src/agent/claude/hooks.rs` / `crates/amux-cli/src/hooks.rs` / `crates/amux/src/lib.rs`** — Removed the root `ClaudeHook` leak and replaced it with `extract_external_agent_id(payload)` for the CLI path.
- **`crates/amux/src/server.rs` / `agent.rs` / `transport.rs` / `client.rs` / `protocol.rs` / `auth.rs` plus internal leaf modules** — Facades now determine what is visible; broad internal `pub` items were downgraded to `pub(crate)` or narrower, and dead root re-exports like suspended-state DTOs were removed.

### Decisions Made
- **Breaking changes are acceptable**: No effort was spent preserving old internal paths or overexposed APIs; the code now favors cleaner boundaries.
- **Facade files own exposure**: Root module files determine visibility, and leaf modules are only as visible as their actual callers require.
- **Wire types stay in `protocol`, runtime types stay outside it**: Internal metadata and runtime behavior no longer leak back down into the protocol layer.
- **Resume sanitization belongs with session construction**: Suspended state stays dumb/serializable; turning it back into a live session is agent-layer logic.
- **The CLI only gets the one Claude capability it actually needs**: extracting an external session ID is a stable boundary; the full Claude hook enum remains an internal agent detail.
- **Do not split large single-purpose files just for line count**: `server/runtime.rs` and `server/connection.rs` were reviewed after the server splits and left intact for now because they still represent cohesive orchestration units, unlike the old mixed handler/routing files.

### Verification
- `cargo fmt --all`
- `cargo check --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo build --workspace`
- `cargo run -p e2e-runner -- run`
- Result: all checks passed; workspace tests passed (`amux` 296, `amux-cli` 9, `e2e-runner` 9), and the E2E runner passed all 12 scenarios. The E2E command required an unrestricted rerun because the sandbox could not open local TCP ports.

### Next Steps
- The documented refactor-plan cleanup is complete.
- Further changes should be normal follow-up work driven by features or concrete pain points rather than more tree-shuffling.

## 2026-04-16: Reshuffle amux domains and make hook handling opaque

### Summary
Reshuffled `crates/amux` into clearer top-level domains (`agent`, `auth`, `client`, `protocol`, `server`) and finished the follow-up cleanup needed to make that split real instead of cosmetic. The wire-level `HandleHook` command now carries `agent_id`, `provider`, opaque `payload`, and an explicit `external` flag; provider-specific hook parsing and readonly bootstrap live under the agent layer instead of `server`. The refactor also split generic debug helpers from server debug rendering, extracted suspended snapshot persistence into `suspend.rs`, removed the crate-wide `AmuxError` export, and migrated `amux-cli` off the old root error API so the workspace builds and tests cleanly again.

### Changes
- **`crates/amux/src/lib.rs`** — Replaced the old flat module layout with domain modules and removed the root `AmuxError` export.
- **`crates/amux/src/protocol/`** — Split wire DTOs into dedicated modules (`agent`, `handshake`, `message`, `route`) and kept protocol hook payloads opaque.
- **`crates/amux/src/agent/`** — Moved agent/session implementations under a dedicated domain, including Claude hook parsing/bootstrap and the test agent.
- **`crates/amux/src/server/`** — Added `debug.rs`, `registry.rs`, and `state.rs`; updated handlers/routing to use the new agent and protocol boundaries.
- **`crates/amux/src/debug.rs`** — Reduced to shared debug helpers such as `DebugView` and `LossyPath`.
- **`crates/amux/src/suspend.rs`** — New shared module for suspended snapshot types and persistence helpers.
- **`crates/amux/src/server/accept.rs` / `client/connect.rs` / `server/connection.rs` / related modules** — Converted away from the old crate-wide error pattern toward local concrete error types.
- **`crates/amux-cli/src/*.rs`** — Migrated CLI code off `amux::AmuxError` / `amux::Result` and onto the new library error surface.

### Decisions Made
- **Directional boundaries over perfect APIs**: The main goal was to make ownership clear enough that the next cleanup pass can target specific ugly APIs, rather than to over-design abstractions up front.
- **Opaque hook payloads at the protocol layer**: `protocol` now carries only `agent_id`, `provider`, `payload`, and `external`; Claude-specific parsing and readonly bootstrap live in the agent layer.
- **Keep `provider` and `external`**: `provider` is needed to route unknown-agent external hooks to the correct implementation; `external` remains a CLI-sourced hint and must not be inferred from lookup failure.
- **Shared helpers stay shared**: Generic debug helpers stay top-level, while server-specific dump rendering moved under `server`.
- **No new global error enum**: Removing `AmuxError` was intentional; module-local concrete errors are a better fit for the new domain split, and the CLI now owns its own adaptation layer.

### Verification
- `cargo test -p amux`
- `cargo test -p amux-cli`
- `cargo test`
- `rg -n "amux::AmuxError|amux::Result" crates/amux-cli/src`
- Result: all test commands passed; the ripgrep check returned no remaining CLI references to the removed root error API.

### Next Steps
- The reshuffle is in place; future work should focus on cleaning up any awkward internal APIs exposed by the new boundaries rather than moving modules around again.

## 2026-04-15: Add server-level idle sleep prevention and persist onboarding preference

### Summary
Added a cross-platform server-owned idle sleep inhibitor controlled by a new `prevent_idle_sleep` config flag, and taught `amux init` to persist that preference into the active config file. The server now holds the inhibitor for the full `Server::run()` lifetime, macOS uses native IOKit `PreventUserIdleSystemSleep`, Linux selects the first available backend from an ordered list, and Windows uses a native power request. Follow-up fixes made onboarding ask for the preference independently of cloud mode, treat blank/comment-only config files as “unset”, conservatively parse prompt input, and surface clearer errors when writing the active config file fails.

### Changes
- **`crates/amux/src/config.rs`** — Added `prevent_idle_sleep: bool` with serde/default coverage and tests.
- **`crates/amux/src/sleep_inhibitor/`** — New module directory with shared `mod.rs`, macOS IOKit backend, Linux ordered backend selection (`systemd-inhibit`, `gnome-session-inhibit`), Windows power-request backend, dummy backend, and colocated `iokit_bindings.rs`.
- **`crates/amux/src/server/mod.rs`** — Server now creates a sleep inhibitor for the duration of `Server::run()` based on config.
- **`crates/amux/src/setup.rs`** — Added helpers to read/write `prevent_idle_sleep` in `config.yaml`, runtime support detection, blank/comment-only config handling, and clearer active-config-file persistence errors.
- **`crates/amux-cli/src/init.rs` / `crates/amux-cli/src/main.rs`** — `init` now prompts for `prevent_idle_sleep` when unset, even for local-only users, updates in-memory config immediately, and re-prompts on invalid input.
- **`crates/amux/Cargo.toml` / `Cargo.lock`** — Added platform dependencies for macOS `core-foundation` and Windows power APIs.

### Decisions Made
- **Server lifetime, not agent lifetime**: The inhibitor belongs to the server so remote create/attach still works even when no agent is currently active.
- **`--config` is authoritative and writable**: Setup choices are persisted back into the active config file; write failures are surfaced to the user instead of silently falling back elsewhere.
- **Native macOS semantics**: Use `IOPMAssertionCreateWithName(..., "PreventUserIdleSystemSleep", ...)` rather than shelling out to `caffeinate`.
- **Shared Linux backend ordering**: Prompt gating and runtime acquisition both use the same ordered backend selection logic to avoid drift.
- **Blank config means “unset”**: Empty or comment-only YAML should not suppress onboarding for the new preference.

### Verification
- `cargo fmt`
- `cargo test -p amux prevent_idle_sleep`
- `cargo test -p amux-cli`
- Result: all tests passed, including new coverage for blank/comment-only config handling, conservative prompt parsing, and active-config-file persistence errors.

### Next Steps
- Consider replacing the Linux wrapper backends with a native logind D-Bus inhibitor later.
- Consider documenting explicitly that `--config` selects the authoritative writable config file used by `amux init`.

## 2026-04-15: Fix MessagePack deserialization of byte fields in internally tagged enums

### Summary
Messages were failing to deserialize because internally tagged serde enums (`#[serde(tag = "...")]`) buffer all fields into a generic intermediate representation before decoding. MessagePack `bin` data in `Vec<u8>` fields gets buffered as a sequence of integers rather than a byte blob, causing type mismatches on replay. Added `#[serde(with = "serde_bytes")]` to all `Vec<u8>` fields inside tagged enums so binary data survives the buffer-and-replay cycle.

### Changes
- **`Cargo.toml`** — Added `serde_bytes = "0.11"` to workspace dependencies
- **`crates/amux/Cargo.toml`** — Added `serde_bytes` dependency
- **`crates/amux/src/message.rs`** — Added `#[serde(with = "serde_bytes")]` to `Message::Routable::payload`, `RoutableMessage::RawInput::data`, and `RoutableMessage::RawOutput::data`

### Decisions Made
- Kept internal tagging + `serde_bytes` rather than reverting `Message` to external tagging, for consistency across all enums. All enums use internal tagging; byte fields get `serde_bytes`.
- `RoutableMessage` must stay internally tagged because `#[serde(other)]` on `Unknown` (forward compatibility) only works with internal/adjacent tagging.
- Performance overhead of internal tagging's buffer-and-replay is negligible for this workload (terminal multiplexer, not millions of messages per second).

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 286 unit tests pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 12 E2E tests pass

---

## 2026-04-15: Pre-release security hardening

### Summary
Security audit identified several hardening gaps. Fixed four issues: WebSocket frame size was unbounded (DoS vector on cloud servers), `suspended.yaml` was created with default permissions instead of 0o600, link names had no length/charset validation, and `cloud_url` could be set to HTTP in release builds.

### Changes
- **`crates/amux/src/server/accept.rs`** — Replaced `accept_async` with `accept_async_with_config`, setting `max_message_size` and `max_frame_size` to `MAX_FRAME_SIZE` (16MB) matching TCP/Unix transports. Added `validate_link_name` function enforcing non-empty, max 128 bytes, `[a-zA-Z0-9_-]` only; replaces the old `.contains('.')` check.
- **`crates/amux/src/transport/mod.rs`** — Made `MAX_FRAME_SIZE` `pub(crate)` so `accept.rs` can reference it for WebSocket config.
- **`crates/amux/src/state.rs`** — `save_suspended` now uses `OpenOptions` with `mode(0o600)` instead of `fs::write`, matching how `state.yaml` is written.
- **`crates/amux/src/config.rs`** — `Config::validate` now rejects non-HTTPS `cloud_url` in release builds (`#[cfg(not(any(debug_assertions, test)))]`).
- **`crates/amux-cli/src/plugin.rs`** — Fixed pre-existing clippy `collapsible_if` warning.

### Decisions Made
- Link name charset `[a-zA-Z0-9_-]` matches what generators produce; strict enough to prevent log injection and routing edge cases.
- HTTPS enforcement is unconditional (not gated on `is_cloud`) since local servers also send tokens to `cloud_url`. Only gated on release builds so `http://localhost` works in dev.
- WebSocket size limits match the existing TCP/Unix 16MB `MAX_FRAME_SIZE` for consistency.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 286 unit tests pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 12 E2E tests pass

### Next Steps
- Consider OS keychain integration for refresh token storage (currently plaintext in 0o600 file)
- Consider per-IP rate limiting for cloud server connections
- Consider audit logging for security-relevant events

---

## 2026-04-15: Bundle Claude marketplace locally and track applied plugin state

### Summary
Reworked the Claude plugin install/update flow so amux no longer adds the marketplace from GitHub. The CLI now embeds the marketplace and plugin assets into the binary, materializes them into an amux-managed local marketplace directory, derives the expected plugin version from the bundled `plugin.json`, and tracks the last Claude-applied plugin version separately from the generated files on disk. This keeps `amux new claude` on a cheap local fast path while avoiding false-success cases after failed installs.

### Changes
- **`crates/amux-cli/src/plugin.rs`** — Replaced the old `PLUGIN_VERSION: u32` sentinel with a structured local plugin manager. Added bundled marketplace/plugin/hook assets via `include_str!`, local materialization under the amux data dir, manifest-derived version parsing, explicit `Install` / `Update` / `Rebind` actions, and strict success-only persistence of applied state.
- **`crates/amux/src/state.rs`** — Replaced `claude.plugin_version: Option<u32>` with `claude.applied_plugin_version: Option<String>` and added `claude.applied_marketplace_path: Option<PathBuf>`.
- **`crates/amux/src/setup.rs`** — Replaced the old version-only getters/setters with `ClaudePluginSetupState` so version and marketplace path are loaded and persisted together.
- **`crates/amux/src/config.rs` / `crates/amux/src/state.rs` / `crates/amux/src/lib.rs`** — Added a shared `amux_xdg_dir()` helper and routed config/state/log/data paths through it so amux-owned directories are resolved consistently.
- **`crates/amux-cli/Cargo.toml` / `Cargo.lock`** — Added `tempfile` as a CLI test dependency for the new plugin materialization tests.

### Decisions Made
- **Bundle the marketplace/plugin into the binary**: Avoids cloning the whole amux repo just to install Claude hooks and keeps the plugin tightly coupled to the amux release artifact.
- **Use the bundled manifest version as the expected version**: Removes the split between a private integer sentinel and Claude’s plugin metadata version.
- **Track applied state separately from materialized files**: Writing generated files to disk is not the same as Claude successfully installing/updating them, so the fast path now requires both the materialized bundle and the last successfully applied state to match.
- **Rebind when the marketplace source path changes**: A plain `claude plugin marketplace update amux` refreshes the existing registered source, so if the local marketplace path changes amux now removes and re-adds the marketplace before reinstalling the plugin.
- **Self-heal corrupt generated manifests**: Invalid JSON in the materialized `plugin.json` is treated as stale generated state and rewritten from the embedded bundle instead of hard-failing before repair.
- **Keep strict failure behavior**: Claude command failures still stop the flow immediately; the new logic only hardens amux-owned generated state.

### Verification
- `cargo fmt`
- `cargo test -p amux -p amux-cli`
- Result: all tests passed, including new coverage for incomplete bundles, corrupt materialized manifests, and marketplace-path rebinds.

### Next Steps
- Consider an `amux doctor` or explicit repair command later for Claude-side drift caused by manual user actions outside amux.
- Decide whether the materialized marketplace should eventually get a more explicit release/version layout if amux needs to support multiple concurrent plugin channels.

## 2026-04-15: Refactor CLI client split and centralize daemon spawning

### Summary
Refactored the CLI ergonomics implementation to simplify control flow and reduce duplication without changing behavior. Split the old mixed CLI client into separate session and server modules, made `main()` do a single-pass command dispatch, and moved daemon spawning into a shared library helper so explicit `amux server start` and implicit auto-spawn paths use the same detached process setup.

### Changes
- **`crates/amux-cli/src/main.rs`** — Simplified command dispatch: early bare-help handling, dedicated `server start --config-from-stdin` path, centralized config loading/validation, single `match` over `Commands`.
- **`crates/amux-cli/src/session_client.rs`** — New module containing `new`, `attach`, `list`, and the attached-session PTY/lease handling that previously lived in the monolithic `client.rs`.
- **`crates/amux-cli/src/server_client.rs`** — New module containing `start`, `stop`, `connect`, `suspend`, `resume`, and `debug` helpers. Added typed `StartOptions`/`StartStyle` instead of boolean-heavy APIs.
- **`crates/amux-cli/src/client_common.rs`** — New shared helper module for daemon options/policy and update-banner rendering.
- **`crates/amux/src/connect.rs`** — Added shared daemon-spawn helpers and `ServerMode`, so both explicit background start and `ConnectPolicy::SpawnDaemon` use the same detached spawn path.
- **`crates/amux/src/lib.rs`** — Re-exported `ServerMode` and `spawn_daemon`.
- **`crates/amux-cli/src/update.rs`** — Switched to `server_client` helpers after the module split.
- **`crates/amux-cli/Cargo.toml` / `Cargo.lock`** — Removed the CLI-only `libc` dependency after moving Unix detachment fully into the library.
- **`crates/amux-cli/src/client.rs`** — Removed after splitting its responsibilities across the new modules.

### Decisions Made
- **Keep daemon spawning in the library**: The explicit CLI lifecycle path and the implicit auto-spawn path must share one implementation, otherwise they drift and regress independently.
- **Split session and server concerns**: The PTY attach loop and the server lifecycle/control-plane logic are unrelated enough that keeping them in one module was making both harder to reason about.
- **Use typed start options**: `StartOptions { mode, style }` is clearer at call sites than passing `cloud` / `foreground` booleans around.

### Verification
- `cargo fmt --all`
- `cargo test`
- `cargo test -p amux --lib`
- `cargo run -p e2e-runner -- run`
- Result: all checks passed; library tests (286) and E2E tests (12/12) remained green after the refactor.

### Next Steps
- If the hidden `server suspend` / `server resume` flow remains part of the CLI surface, consider whether the cloud-relay behavior should be encoded more explicitly in persisted state rather than left as an implicit local-server-only path.

## 2026-04-14: CLI ergonomics — server subcommand, positional attach, suspend/resume

### Summary
Restructured the CLI for better ergonomics. Grouped server lifecycle commands under `amux server` (start/stop/connect), made the attach name a positional argument, added hidden `suspend`/`resume` commands as standalone alternatives to the update-only flow, and changed bare `amux` to print help instead of auto-attaching.

### Changes
- **`crates/amux-cli/src/main.rs`** — Replaced top-level `Serve`, `Shutdown`, `Connect` with nested `Server { Start, Stop, Connect, Suspend, Resume }`. `Attach` name changed from `--name` flag to positional. Bare `amux` now prints help.
- **`crates/amux-cli/src/client.rs`** — Added `start_server` (idempotent, with daemon spawn and stale socket cleanup), `stop_server` (renamed from `kill_server`), `suspend_server`, `suspend_server_if_running`, `resume_server`, `resume_server_with_executable`. Extracted `server_is_running`, `spawn_server_daemon`, `wait_for_server` helpers.
- **`crates/amux-cli/src/update.rs`** — Replaced inline suspend/resume logic with calls to `client::suspend_server_if_running` and `client::resume_server_with_executable`.
- **`crates/amux/src/connect.rs`** — Updated `SpawnDaemon` to use `server start --foreground --config-from-stdin`.
- **`crates/amux/src/server/mod.rs`** — Minor update for config validation.
- **`crates/amux-cli/Cargo.toml`** — Added `serde_yaml` dependency for config serialization in daemon spawn.
- **`e2e-tests/*.test`** — Updated all tests using `shutdown` to use `server stop`. Added `bare_help.test` and `server_lifecycle.test`.
- **`crates/e2e-runner/src/executor.rs`** — Updated E2E runner for new command structure.

### Decisions Made
- **Nested `server` group over flat commands**: Prevents verb collision if agent-level kill/stop is added later. `amux server stop` vs future `amux kill <agent>` is unambiguous.
- **`--foreground` flag instead of coupling cloud=foreground**: Cloud mode and process mode are orthogonal. `--cloud --foreground` for systemd, `--cloud` alone for a cloud daemon.
- **Bare `amux` prints help**: Placeholder — the bare command will be repurposed in the future.
- **`suspend`/`resume` hidden**: Useful for manual server management but not part of the public API yet.

### Verification
- `cargo check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — all passing
- E2E: 12/12 passing (including new `bare_help` and `server_lifecycle` tests)

### Next Steps
- Consider `amux kill <name>` for stopping individual agents (protocol support exists via `DeleteAgent`)
- Bare `amux` behavior to be repurposed

---

## 2026-04-14: Strip claude/types.rs to production essentials

### Summary
Gutted `claude/types.rs` down to the three types the server actually needs: `Hook`, `ClaudeHook`, and `HookCommon`. Removed all tool input types (`PreToolUse`, `BashToolInput`, `EditToolInput`, etc.), all tool output types (`PostToolUse`, `*ToolOutput`, `PatchHunk`, etc.), and all specialized hook structs (`ClaudePermissionRequest`, `ClaudeStop`, `ClaudeNotification`, etc.). The server forwards all structured output as opaque `serde_json::Value` blobs and only needs to discriminate hook variants and extract `session_id`/`transcript_path`/`cwd` — all variants now carry a single `HookCommon` struct. Also removed the vestigial unknown-tool warning from CLI hooks.

### Changes
- **`crates/amux/src/claude/types.rs`** — Collapsed from ~1580 lines to ~190. Replaced per-variant structs with shared `HookCommon`. Removed `PreToolUse`, `ClaudePermissionTool`, all `*ToolInput` structs (26), all `*ToolOutput` structs (25), `PostToolUse`, `PatchHunk`, `GitDiff`, and associated Display impls and tests.
- **`crates/amux/src/claude/mod.rs`** — Updated module doc comment.
- **`crates/amux/src/agents/claude.rs`** — Updated hook construction in tests to use `HookCommon`. Removed `notification_type` field access (was debug-log only).
- **`crates/amux/src/server/handlers.rs`** — Updated hook construction in tests to use `HookCommon`.
- **`crates/amux-cli/src/hooks.rs`** — Removed `PreToolUse` import and vestigial unknown-tool warning.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — 286 tests pass, zero warnings.

---

## 2026-04-14: Make client_name/client_version optional, per-client minimum versions

### Summary
Made `client_version` optional in the Connect handshake and added an optional `client_name` field, so that different client implementations (CLI, mobile app) can identify themselves independently. Replaced the single `minimum_client_version` config with a `minimum_client_versions` map keyed by client name, allowing per-client version enforcement. Clients that send no `client_name` or `client_version` are allowed through (the server can't check what it doesn't know).

### Changes
- **`crates/amux/src/handshake.rs`** — `client_version` changed from `String` to `Option<String>` with `#[serde(default)]`. Added `client_name: Option<String>`. Updated tests: removed `connect_requires_client_version_field`, added `connect_without_client_name_or_version_decodes`.
- **`crates/amux/src/config.rs`** — Replaced `minimum_client_version: Option<String>` with `minimum_client_versions: HashMap<String, String>`. Updated `validate()` to check each map value is valid semver. Updated tests.
- **`crates/amux/src/server/accept.rs`** — Version check now looks up `client_name` in the `minimum_client_versions` map. Skipped if client sends no `client_name`. Return type extended to include `client_name`. Outbound Connect messages send `client_name: "amux-cli"`.
- **`crates/amux/src/server/handlers.rs`** — Reauth handler uses same per-client-name lookup.
- **`crates/amux/src/server/connection.rs`** — `ConnectionContext` now has `client_name: Option<String>` and `client_version: Option<String>`.
- **`crates/amux/src/cloud.rs`** — Outbound cloud Connect sends `client_name: "amux-cli"`.
- **`crates/amux/src/server/cloud.rs`**, **`crates/amux/src/server/mod.rs`** — Updated ConnectionContext construction in test helpers.
- **`CLOUD_ARCHITECTURE.md`** — Updated handshake docs.

### Decisions Made
- **Optional fields, no committed semantics**: `client_name` and `client_version` are purely informational. The server may or may not enforce minimums based on them. This keeps the door open to moving version checks elsewhere (e.g. REST API, manifest) without a breaking protocol change.
- **Per-client map over single minimum**: The CLI and mobile app have different version numbers and release cadences. A flat minimum would force lockstep versioning.
- **Permissive default**: Clients that don't send `client_name` bypass version checks entirely. This is intentional for forward compatibility with new or unknown clients.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — 304 tests pass, zero warnings.

---

## 2026-04-14: Add forward-compatibility Unknown variants and flatten AgentProtocol

### Summary
Added `#[serde(other)] Unknown` catch-all variants to all protocol enums (`Message`, `DirectMessage`, `RoutableMessage`, `Command`, `SubscribeQuery`, `ProtocolError`, `AgentType`) so that peers at different versions can deserialize unrecognized variants without frame-level decode failures. Split the old `UnknownMessage` response into `UnsupportedMessage` (parsed but unrecognized routable tag) vs `InvalidMessage` (corrupt/undecodable payload bytes). Replaced the `AgentProtocol` enum with an opaque `structured_protocol: Option<String>` on both `AnnounceAgent` and `SubscribeStructuredResult`, and changed `agent_type` in the registry from `AgentType` enum to a plain `String`.

### Changes
- **`crates/amux/src/message.rs`** — Added `Unknown` variants with `#[serde(other)]` to `Message`, `DirectMessage`, `RoutableMessage`, `Command`, `SubscribeQuery`, `ProtocolError`, `AgentType`. Replaced `UnknownMessage` with `UnsupportedMessage` + `InvalidMessage` + `Unknown`. Removed `ClaudeMode`, `AgentProtocol` enums. Changed `SubscribeStructuredResult.protocol` to `structured_protocol: Option<String>`. Updated forward-compat tests to assert `Unknown` deserialization instead of decode errors. Added new test for unknown routable variant.
- **`crates/amux/src/protocol.rs`** — Removed `AgentProtocol` and `ClaudeMode` re-exports.
- **`crates/amux/src/agents/mod.rs`** — Replaced `agent_protocol()` with `structured_protocol()` returning `Option<String>`. Changed `to_agent()` to emit `agent_type` as a string and include `structured_protocol`.
- **`crates/amux/src/agent_registry.rs`** — Changed `Agent` and `StoredAgent` `agent_type` from `AgentType` enum to `String`, added `structured_protocol: Option<String>`.
- **`crates/amux/src/buffer.rs`** — Added `SubscribeQuery::Unknown` arm returning empty slice (defensive; handler rejects before reaching buffer).
- **`crates/amux/src/debug.rs`** — Emit `structured_protocol` in agent debug dump when present.
- **`crates/amux/src/server/handlers.rs`** — Handle `Unknown` variants in `handle_message`, `handle_routable`, `handle_command`, `handle_direct` (log and drop). Distinguish `RoutableMessage::Unknown` (→ `UnsupportedMessage` reply) from decode failure (→ `InvalidMessage` reply). Reject `SubscribeQuery::Unknown` with `UnsupportedSubscribeQuery` error. Added `unknown_routable_variant_returns_unsupported_message` test.
- **`crates/amux/src/server/connection.rs`** — Updated doc comments: frame-level decode errors are now only for truly undecodable frames since known-but-unsupported variants deserialize to `Unknown`.
- **`crates/amux/src/server/routing.rs`** — Pass `structured_protocol` in `announce_agent_message`. Reject `AgentType::Unknown` in `create_agent`. Updated test agent types to strings.
- **`crates/amux-cli/src/main.rs`** — Handle `AgentType::Unknown` as unreachable in CLI parser.
- **`ARCHITECTURE.md`** — Updated `RoutableMessage`, `DirectMessage`, and error handling sections to match.

### Decisions Made
- **`#[serde(other)]` on every protocol enum.** This shifts version-skew handling from frame-level skip (reader_loop) to handler-level drop, preserving envelope context (src/dst/request_id) for logging and reply routing.
- **`UnsupportedMessage` vs `InvalidMessage` split.** "I parsed your payload but don't know this variant" is a different situation from "your bytes are corrupt." The sender can act differently: retry with a fallback vs investigate a serialization bug.
- **`AgentProtocol` → `structured_protocol: Option<String>`.** Non-Rust clients don't benefit from a typed enum they'd have to mirror. A simple string like `"claude_pty_v1"` is easier to match on from JS/Go/Python. Trade-off: no compile-time exhaustiveness on the protocol value.
- **`agent_type` as `String` in registry.** Decouples the registry (which also stores remote agents announced by peers) from the local `AgentType` enum. A remote peer can announce an agent type this node doesn't know how to create, and that's fine — it just stores and re-announces the string.

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all 313 tests pass (304 library + 9 CLI).
- E2E tests: 10/10 passed (attach, list_agents, local_agent_ended, multiple_agents, new_agent, remote_agent_ended, remote_attach_by_alias, remote_connection, remote_list_agents, replay_buffer).

---

## 2026-04-14: Reshape protocol wire enums for non-Rust clients

### Summary
Changed the public protocol message shapes away from serde's default external tagging and onto explicit tagged objects that are easier for JS/TS, Go, and Python clients to consume. The top-level envelope now uses `kind`, direct/routable/command payload enums use `type`, protocol errors use `code`, and `AgentProtocol` is now structurally tied to the owning agent type via `agent_type`, `mode`, and `version`. UUIDs were deliberately left as MessagePack bytes on the wire.

### Changes
- **`crates/amux/src/message.rs`** — Switched `Message` to `kind`-tagged envelopes, with `Direct { message: ... }` and `Command { command: ... }`. Switched `RoutableMessage`, `DirectMessage`, `Command`, and `SubscribeQuery` to `type`-tagged forms. Switched `ProtocolError` to `code`-tagged form and changed `ServerError(String)` to `ServerError { message }`. Changed `Command::ShutdownNotification` to a struct variant. Changed `AgentType::TestAgent(String)` to `TestAgent { command }`. Replaced `ClaudeProtocol` with `ClaudeMode`, and changed `AgentProtocol` to `Claude { mode, version }`.
- **`crates/amux/src/protocol.rs`** — Re-exported `ClaudeMode` instead of the old `ClaudeProtocol`.
- **`crates/amux/src/agents/*.rs`** — Updated agent protocol reporting and structured-input error construction to match the new enum shapes.
- **`crates/amux/src/server/*.rs` / `crates/amux/src/cloud.rs`** — Updated all routing, command handling, cloud reauth, shutdown/suspend notifications, and heartbeat paths to use the new envelope and error shapes.
- **`crates/amux-cli/src/*.rs`** — Updated CLI command send/receive handling to match `Message::Command { command: ... }` and the new protocol enum forms.
- **Tests across `message.rs`, `server/*`, and `claude/types.rs`** — Updated serialization roundtrips and pattern matches to the new wire shapes. Also updated forward-compat serialization tests to emit internally-tagged/`kind`-tagged future variants instead of old external-tagged forms.

### Decisions Made
- Use **`kind`** only for the top-level envelope and **`type`** for actual message variants. This keeps the outer transport class (`routable` / `direct` / `command`) distinct from inner operation names (`subscribe_structured`, `heartbeat`, etc.).
- Use **`code`** for `ProtocolError` instead of overloading `type` again. Error payloads read more naturally as `{ "code": "unknown_subscription" }`.
- Keep **UUIDs as bytes** on the wire for now. Cross-language ergonomics would be better with strings, but the app already speaks byte UUIDs and this cut was intentionally limited to message shape.
- Keep **`AgentProtocol` coupled to the agent family**. Flattening to namespaced strings like `claude_pty_v1` would have hidden the relationship between payload schema and agent type behind naming convention. The new shape is explicit: `{ "agent_type": "claude", "mode": "pty", "version": 1 }`.
- Name the top-level command field **`command`**, not `message`. `kind = "command"` plus `command = {...}` is clearer than another generic `message` field.
- Do not preserve backwards compatibility or bump the protocol version. amux is still pre-release and simplifying the wire contract now is cheaper than carrying transitional shapes.

### Verification
- `cargo fmt`
- `cargo check`
- `cargo test -p amux`
- `cargo test`
- Result: all checks passed; `cargo test -p amux` ran 301 passing library tests after the protocol shape change.

### Next Steps
- Update any external clients (notably `amuxapp`) to the new tagged wire shapes before relying on these protocol messages again.
- Revisit string UUIDs later as a separate protocol simplification if cross-language client ergonomics become the dominant concern.

## 2026-04-13: Add Unreachable message for forwarding failures

### Summary
Added a new `RoutableMessage::Unreachable` variant, analogous to ICMP Destination Unreachable. When an intermediate hop cannot forward a message because the next hop doesn't exist in the routing table, it sends `Unreachable` back to the original sender via the reverse path. This gives clients immediate error feedback instead of hanging until timeout.

### Changes
- **`crates/amux/src/message.rs`** — Added `Unreachable { request_id: u64 }` variant to `RoutableMessage`, updated `type_label()`, added roundtrip test.
- **`crates/amux/src/server/handlers.rs`** — In the `None` (no route) arm of `handle_routable()` forwarding, send `Unreachable` back via `Route::reply(src)`. Added `Unreachable` to the destination noop match arm. Updated existing test, added three new tests.

### Decisions Made
- Named `Unreachable` (not `UnreachableRoute`) following ICMP/BGP/SCTP convention — the routing layer reports it can't deliver, the name is self-evident in that context.
- `Unreachable` carries `request_id` so it's self-describing in the payload, even though the envelope also carries it. Mirrors ICMP including the original datagram header.
- No reason enum (NoRoute vs LinkDown) — the client treats both identically. Can add `Option<UnreachableReason>` later if needed.
- No hop identification — can add `Option<String>` later if diagnostics warrant it.
- Only the no-route case sends Unreachable. The channel-closed case (route exists but peer disconnected between lookup and send) does not — `src` is consumed by the forwarded message so we can't reply. This is acceptable because `send()` succeeding only means the message landed in the channel buffer, not that the peer processed it. Reliable delivery requires application-level acks (`*Result` messages), so senders must use timeouts for in-flight losses regardless.

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all 301 tests pass (3 new: forwarding to nonexistent route, forwarding over closed channel, empty-src forwarding; plus roundtrip serialization test).

---

## 2026-04-13: Always return StructuredInputResult

### Summary
Changed StructuredInput handling to always send a StructuredInputResult back to the caller, including on success. Previously, a successful StructuredInput was silent (fire-and-forget) and only errors produced a result. This was inconsistent with the other request/result pairs (CreateAgent, DeleteAgent, RenameAgent, etc.) which all unconditionally return a result.

### Changes
- **`crates/amux/src/server/handlers.rs`** — Restructured the StructuredInput handler to extract reply routes up front and always send StructuredInputResult with `error: None` on success.

### Decisions Made
- Always-respond is the right pattern for StructuredInput because clients (e.g. mobile fork flow) need positive confirmation that input was accepted before advancing UI state. The seq handshake implies the client cares about correctness, and a positive ack completes that contract.
- RawInput remains fire-and-forget — it's a byte stream with no sequencing contract, and the PTY echo is the feedback loop.

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all 298 tests pass.

---

## 2026-04-12: Move Claude PTY encoding to the app client

### Summary
Moved Claude PTY input encoding out of the `amux` server and into `amuxapp`. The app now converts semantic Claude intents into opaque `PtyInput[]` payloads (`Bytes` / `Delay`) before sending them over the wire, and the server now treats Claude structured input as transport-only PTY actions. This breaks the old semantic wire format on purpose so Claude UI coupling lives in the app instead of the server.

### Changes
- **`crates/amux/src/agents/claude.rs`** — Added `PtyInput` enum, removed all server-side Claude keystroke generation helpers/tests, and changed `send_structured_input()` to deserialize `Vec<PtyInput>` and execute it directly against the PTY.
- **`crates/amux/src/claude/types.rs`** — Removed input-only Claude semantic types (`PermissionResponse`, `AskUserQuestionResponse`, `PlanReviewResponse`, `ClaudeStructuredInput`) and the tests that only exercised those input payloads.
- **`../amuxapp/agents/claude/pty-encoding.ts`** — Added the client-side pure PTY encoder and exported the Claude intent → `PtyInput[]` mapping.
- **`../amuxapp/agents/claude/input.ts` / `agents/types.ts` / `agents/claude/module.ts`** — Plumbed optional runtime-version context through `buildInput()` and returned `PtyInput[]` directly for Claude.
- **`../amuxapp/session/store.ts` / `session/types.ts` / `services/ws-manager.ts`** — Passed runtime version into input building, added explicit `interrupt` / `passive` effect flags, and stopped sniffing semantic payload strings in the transport.
- **`../amuxapp/logging/structured-input.ts`** — Added an opaque array fallback summary for `PtyInput[]`.
- **`../amuxapp/agents/claude/pty-encoding.test.ts`** — Ported the Claude PTY golden tests to TypeScript so byte sequences are now validated on the client side.

### Decisions Made
- **Make the server transport-only**: the server now only validates seq/read-only state and executes already-encoded PTY actions. Claude TUI knowledge no longer lives in infrastructure.
- **Keep `StructuredInput { payload: Value }` opaque**: no protocol-wide input schema was introduced; agent-specific payloads still deserialize per agent type.
- **Use effect flags instead of payload sniffing**: `interrupt` and `passive` semantics now come from dispatch-time intent knowledge, not from transport-layer inspection of Claude-specific payload strings.
- **Keep logging simple at the transport layer**: `PtyInput[]` logging only records `PtyV1` + action count. Rich semantic detail stays closer to the intent layer.

### Verification
- **App focused tests**: `npx vitest run agents/claude/pty-encoding.test.ts logging/structured-input.test.ts session/store.test.ts services/ws-manager.test.ts`
- **App full suite**: `npx vitest run`
- **App typecheck**: `npx tsc --noEmit`
- **Server format/checks**: `cargo fmt`, `cargo check`, `cargo clippy --workspace --all-targets -- -D warnings`
- **Server tests**: `cargo test` and `cargo test --workspace`
- **Results**: all commands passed; `cargo test --workspace` ran the `amux` crate tests that cover the new `PtyInput` deserialization path.

### Next Steps
- Add version-dependent PTY encoding branches in `amuxapp` when Claude UI behavior diverges by runtime version.
- Keep future non-PTY agent protocols (`SdkV1`, etc.) on their own opaque structured-input shapes instead of reintroducing server-side semantic translation.

---

## 2026-04-12: Minimum client version enforcement

### Summary
Added server-side minimum client version enforcement. Cloud servers (or any server) can set `minimum_client_version` in their config to reject clients running old binaries. This is separate from the existing protocol version mismatch check — it enforces semver-level policy ("you must be at least v0.2.0") rather than wire-format compatibility. Also renamed `VersionMismatch` to `ProtocolMismatch` across `ProtocolError`, `CloudError`, `AmuxError`, and `CloudConnectionError` to disambiguate from the new `UpgradeRequired` variant.

### Changes
- **`crates/amux/src/handshake.rs`** — Added `client_version: String` field to `Connect` struct (required, breaking change). Added test for missing field.
- **`crates/amux/src/message.rs`** — Added `ProtocolError::UpgradeRequired { minimum_version, client_version }` variant.
- **`crates/amux/src/error.rs`** — Added `AmuxError::UpgradeRequired { minimum_version, client_version }` variant.
- **`crates/amux/src/config.rs`** — Added `minimum_client_version: Option<String>` to `Config`.
- **`crates/amux/src/cloud.rs`** — Added `CloudError::UpgradeRequired` variant. Updated `check_handshake_connect_result` and `check_reauth_result` to handle the new protocol error.
- **`crates/amux/src/server/accept.rs`** — Added semver check in `accept_handshake()` after protocol version check. Updated `connect_handshake()` to handle `UpgradeRequired` response. All `Connect` construction sites now include `client_version`.
- **`crates/amux/src/server/connection.rs`** — Added `client_version: String` field to `ConnectionContext`.
- **`crates/amux/src/server/handlers.rs`** — Added minimum version re-check in `Reauth` handler (catches config changes on already-connected clients at next token refresh).
- **`crates/amux/src/server/cloud.rs`** — Added `CloudConnectionError::UpgradeRequired` variant. Writes `upgrade-required` marker file on rejection, clears it on successful connection.
- **`crates/amux/src/update.rs`** — Added marker file functions: `write_upgrade_required`, `read_upgrade_required`, `clear_upgrade_required`, `is_upgrade_dismissed`, `dismiss_upgrade`.
- **`crates/amux-cli/src/main.rs`** — Added `check_upgrade_required()` pre-command warning before `amux new` (only when cloud mode enabled and not dismissed). Interactive prompt: Enter to continue, 'd' to dismiss permanently (until next update), Ctrl-C to exit.
- **`crates/amux-cli/src/client.rs`** — Enhanced `print_update_banner()` to show "update REQUIRED" when upgrade-required marker exists (takes priority over "update available").
- **`crates/amux-cli/src/update.rs`** — `amux update` now clears upgrade-required and upgrade-dismissed markers.

### Decisions Made
- **`client_version` is required (not Optional)**: Breaking change is fine since amux is pre-release. Old clients that don't send it will fail deserialization.
- **Marker files instead of State**: Follows the same pattern as the existing `update-available` marker. Avoids file locks and YAML parsing in the CLI for a simple flag check. Three files: `upgrade-required` (minimum version), `upgrade-dismissed` (dismissed version), cleared by `amux update`.
- **Reauth checks minimum version**: If the cloud bumps `minimum_client_version` while a client is connected, the next token refresh will reject them. Enforcement latency bounded by token refresh interval.
- **Warning only on `amux new`**: Not on `list`, `attach`, etc. `new` is when you're starting a session that benefits from cloud.
- **Dismiss is per-minimum-version**: If the minimum changes, the dismissed file won't match and the warning reappears.

### Verification
- `cargo check` — clean
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 325 tests pass
- E2E tests — 10/10 pass

### Next Steps
- Option 3 (manifest.json `minimum_version`) can be added later for client-side proactive warnings without cloud rejection

## 2026-04-10: Add `Notification` hook support

### Summary
Added support for Claude Code's `Notification` hook event (https://code.claude.com/docs/en/hooks#notification). Notifications fire for permission prompts, idle prompts, auth success, and elicitation dialogs. Like `PermissionRequest` and `Stop`, the Notification hook is propagated to subscribers as structured output by passing through the original raw JSON with a `"type": "hook.notification"` field injected — lossless, no field loss from typed round-tripping.

### Changes
- **`crates/amux/src/claude/types.rs`** — Added `ClaudeNotification` struct (`session_id`, `transcript_path`, `cwd`, `message`, `title: Option<String>`, `notification_type: String`) and `ClaudeHook::Notification(ClaudeNotification)` variant. Extended `ClaudeHook::session_id()`, `cwd()`, `transcript_path()` accessors and the `Display` impl. Added two unit tests covering deserialization with and without the optional `title` field.
- **`crates/amux/src/agents/claude.rs`** — Added a `Notification` arm in `ClaudeSession::handle_hook()` mirroring the `Stop` and `PermissionRequest` passthrough pattern: take the raw `Value`, inject `"type": "hook.notification"`, write to `log_source`. Updated the doc comment on `handle_hook` to mention notification.
- **`crates/amux/src/server/handlers.rs`** — Added `Notification` to the `hook_type` debug-tracing match.
- **`crates/amux-cli/src/main.rs`** — Added `Notification` variant to the `ClaudeHookEvent` clap subcommand enum so `amux hooks claude notification` is a valid invocation.
- **`claude-plugin/hooks/hooks.json`** — Registered a `Notification` entry that runs `amux hooks claude notification` async, matching the existing `SessionStart`/`SessionEnd`/`PermissionRequest`/`Stop` registrations.

### Decisions Made
- **`notification_type` is a `String`, not an enum.** Claude Code documents four matcher values (`permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog`) but new types may appear over time. Using `String` keeps the parser forward-compatible — we don't reject hooks just because Claude added a new notification kind. Consumers that care can match on the string.
- **`title` is `Option<String>` with `#[serde(default)]`.** The Claude Code docs explicitly mark `title` as "Optional notification title", so it can be absent on the wire. The added `test_notification_deserializes_without_title` test pins this behavior.
- **Notification propagates as structured output (not internal-only).** Unlike `SessionStart`/`SessionEnd` which are bookkeeping events, notifications are user-facing — clients want to render them. Slotted into the same passthrough lane as `PermissionRequest` and `Stop`.
- **`hooks.json` uses the same async fire-and-forget pattern.** The hook handler in `crates/amux-cli/src/hooks.rs` is event-name-agnostic — it reads the JSON from stdin and uses the `hook_event_name` field for dispatch — so adding the new variant only required registering it in the manifest and adding a clap subcommand. No new code path in `handle_claude_hook`.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 309 amux unit tests pass (was 307; +2 from the new notification tests).
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 e2e tests pass.

---

## 2026-04-10: Add `ClaudeStructuredInput::Interrupt` and `CyclePermissions`

### Summary
Added two new unit variants to `ClaudeStructuredInput`: `Interrupt` sends a single Esc byte (`\x1b`) to interrupt Claude mid-response, and `CyclePermissions` sends Shift+Tab (`\x1b[Z`) to cycle Claude Code's permission mode. Both mirror keystrokes a user would press in an interactive Claude Code session.

### Changes
- **`crates/amux/src/claude/types.rs`** — Added `ClaudeStructuredInput::Interrupt` and `ClaudeStructuredInput::CyclePermissions` (both unit variants, no payload).
- **`crates/amux/src/agents/claude.rs`** — Added `ESC: &[u8] = b"\x1b"` and `SHIFT_TAB: &[u8] = b"\x1b[Z"` constants alongside the existing arrow-key constants. Added `interrupt_keystrokes()` and `cycle_permissions_keystrokes()` helpers (each returns a single `PtyAction::Send`). Added match arms in `ClaudeSession::send_input()` that log and dispatch the new variants. Added `test_interrupt_keystrokes` and `test_cycle_permissions_keystrokes` unit tests.

### Decisions Made
- **Unit variants, not structs/enums.** Both inputs are parameterless — they're just "send key X". Modeled as unit variants rather than tuple variants with placeholder types so the wire format and Rust ergonomics stay minimal.
- **Single keystroke, no delay or follow-up.** Other helpers like `submit_prompt_keystrokes` and `plan_review_response_keystrokes` insert `DELAY` between keystrokes because the Claude TUI needs settling time between multi-step inputs. These are single keystrokes, so no delay is required.
- **Shift+Tab as `\x1b[Z` (CSI Z / "back tab").** This is the standard xterm sequence for Shift+Tab and is what the Claude TUI listens for to cycle permission mode.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 307 amux unit tests pass (was 305; +2 from the new keystroke tests).

---

## 2026-04-10: Add `amux.replay_finished` marker; remove vestigial `LinkState`

### Summary
Added a synthetic in-band marker, `{ "type": "amux.replay_finished" }`, that the transcript tailer writes into the structured output buffer at the moment the catchup drain completes and live tailing begins. Subscribers waiting to "catch up" (the fork-coordination use case) can wait for this marker instead of trying to infer caught-up state from a UUID watermark or other positional heuristic. The marker lives in the broadcast buffer like any other entry, so new subscribers see it in their replay in position, and a relink-driven (compaction) replay just emits another one. Same change ripped out the vestigial `LinkState` (`Unlinked`/`Linking`/`Linked`/`Failed`/`Closed`) state machine that DEVLOG-2026-04-10-debug had already flagged as having zero subscribers in the codebase — its only functional uses were a same-path-relink optimization and the `amux debug` dump, both of which no longer need it.

### Changes
- **`crates/amux/src/claude/transcript.rs`** — In `tail_transcript()`, write the `amux.replay_finished` marker via `buffer.write(...)` immediately after the catchup `read_line` loop returns 0 bytes (i.e. at the catchup→live transition). Removed the `log_source: StructuredLogSource` parameter from `TranscriptTailer::new()` and the inner `tail_transcript()` function — the tailer no longer needs to call `mark_linked()` / `mark_failed()` and can write the marker directly into the buffer it already owns. Removed the `super::structured_log_source::StructuredLogSource` import. Updated the existing `tailer_writes_lines_to_buffer` test to expect 3 entries (two transcript lines + marker) and added a new `tailer_emits_replay_finished_for_empty_transcript` test that verifies the marker fires even when the catchup drain processes zero entries.
- **`crates/amux/src/claude/structured_log_source.rs`** — **Deleted** the `LinkState` enum, `link_state_tx: watch::Sender<LinkState>` field, `mark_linked()`, `mark_failed()`, `LinkState::as_str()`, and the `tokio::sync::watch` import. Simplified the same-path-relink check in `link_transcript()` from `current_path == path && state in {Linking, Linked}` to plain `current_path == path` — the gating clause was a workaround for retry-after-failure that was never an intended use case (path changes are what trigger relinks). Updated `link_transcript()` to no longer pass `self.clone()` into `TranscriptTailer::new()`. Simplified `impl Serialize for DebugView<'_, StructuredLogSource>` to emit only `current_path` (no more `link_state` / `link_error` fields). Updated `relink_discards_entries_from_previous_generation` and `same_path_relink_is_ignored` test wait/assert conditions to account for the marker's seq increment, and added a marker-read assertion to `subscriber_receives_replay_after_immediate_subscribe` and `relink_discards_entries_from_previous_generation`.

### Decisions Made
- **Single end marker, no `replay_started` bookend.** The fork-coordination use case only needs a "you're caught up now" signal — clients waiting for catch-up watch for the marker, everyone else ignores it. A `replay_started` bookend would be useful if future consumers wanted to display "loading…" during a compaction-driven replay or buffer entries between markers for atomic apply, but no such consumer exists today and adding the start marker later is a non-breaking change. Single marker is the smallest surface area that solves the actual problem.
- **Marker is written by the tailer directly to its `buffer`, not via `log_source.write()`.** Originally the marker write went through `log_source.write()`, but that broke the unit tests in `transcript.rs` because the tests construct a tailer with an explicit buffer that's separate from the log_source's internal buffer. In production these are the same `Arc<MultiplexStructuredBuffer>`, but in the test the tailer writes catchup entries to the explicit buffer while the marker would have gone to a different buffer. The fix was to drop the `log_source` parameter from the tailer entirely — with `mark_linked()`/`mark_failed()` gone, the tailer doesn't need it anymore.
- **Same-path-relink check reduced to plain path equality.** The previous version gated on `LinkState::Linking | LinkState::Linked`, which allowed retry-after-failure for the same path. In practice the only thing that calls `link_transcript()` is `sync_hook_metadata()` extracting `transcript_path` from a hook event, and path changes are what trigger relinks — failed-link retry on the same path was never an intended use case. Plain path equality is what the optimization logically wants.
- **No metadata on the marker payload yet.** Just `{"type": "amux.replay_finished"}`. If consumers later need to disambiguate "which replay" (generation counter, transcript path, reason), add fields then — adding fields to a tagged JSON object is non-breaking.
- **Tailer-failure-during-catchup leaves clients hanging.** If `read_line()` errors before the catchup drains, the marker is never emitted and any client waiting for it hangs. This matches today's behavior (nothing previously emitted a "tailer broken" signal either) and is out of scope. If it becomes a real problem, the catchup loop can be wrapped in a function whose return value is checked, with the marker emitted unconditionally before the early return.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 305 amux unit tests pass (was 304; net +1 from the new `tailer_emits_replay_finished_for_empty_transcript` test). All other crates pass too.
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 e2e tests pass.

### Next Steps
- Wire up the client-side fork coordination that *waits for* the marker. Lives in the consumer (TBD — likely whichever crate ends up owning the fork command). Out of scope for this change.

---

## 2026-04-10: Rework `amux debug` around `DebugView` newtype + manual `Serialize`

### Summary
Rewrote the `amux debug` command to remove the protocol-level `ServerDebug*` struct hierarchy and the centralized `build_debug_info` builder. Each type that participates in the dump now owns its own `impl Serialize for DebugView<'_, T>`, colocated with the type's source file. The wire format collapses to `DebugResult { dump: String }`, format selection rides on the request as `DebugFormat { Yaml, Json }`, and the renderer is just whichever serde `Serializer` the server picks. The CLI gained a `--format` flag so `amux debug --verbose --format json | jq …` works.

### Changes
- **`crates/amux/src/debug.rs`** (new) — `DebugView<'a, T>` newtype, async `dump_server_debug_info` entry point that acquires read guards and dispatches to either `serde_yaml::to_string` or `serde_json::to_string_pretty`, plus manual `Serialize` impls for `ServerDebugView` (top-level), `UserView`, `RoutesView`, `HostsView`, `AgentsView`, `SubscriptionsView` and their entry types. The structured intermediate stays inside this file — nothing leaks onto the wire or onto the types being dumped.
- **`crates/amux/src/message.rs`** — Added `DebugFormat` enum (`Yaml`/`Json`, default `Yaml`). Slimmed `Command::Debug` to `{ verbose, format }` and `Command::DebugResult` to `{ dump: String }`. **Deleted** `ServerDebugInfo`, `ServerDebugVerboseInfo`, `ServerDebugLocalHostInfo`, `ServerDebugUserInfo`, `ServerDebugRouteInfo`, `ServerDebugHostInfo`, `ServerDebugAgentInfo`, `ServerDebugSubscriptionInfo`, `ServerDebugAgentLocation`, `ServerDebugRouteKind`, `ServerDebugSubscriptionMode` (~120 lines).
- **`crates/amux/src/protocol.rs`** — Removed the `ServerDebug*` re-exports, added `DebugFormat`.
- **`crates/amux/src/server/handlers.rs`** — Deleted `build_debug_info` and helpers (`AgentSubscriptionCounts`, `debug_route_kind`, `debug_subscription_mode`, ~280 lines). The `Command::Debug` arm is now a one-liner that calls `crate::debug::dump_server_debug_info`. Replaced the typed-contract debug test with two minimal smoke tests (one for YAML, one for JSON) that assert the dump is non-empty and parses.
- **`crates/amux/src/server/mod.rs`** — Widened `ServerState`, `ServerUserState`, `SubscriptionEntry`, `SubscriptionMode` from `pub(super)` to `pub(crate)` so the new `crate::debug` module can read their fields directly without accessor methods.
- **`crates/amux/src/claude/structured_log_source.rs`** — Flipped `current_path` from `tokio::sync::Mutex<…>` to `std::sync::Mutex<…>` (it's never held across an `await`, so it was always safe). **Deleted** the debug-only public accessors `current_path()`, `link_status()`, `link_error()`. Added `impl Serialize for DebugView<'_, StructuredLogSource>` which reads `inner.current_path` and `inner.link_state_tx.borrow()` directly.
- **`crates/amux/src/agents/claude.rs`** — **Deleted** four debug-only public accessors (`session_id`, `transcript_path`, `transcript_status`, `transcript_error`). Added `impl Serialize for DebugView<'_, ClaudeSession>` which reads `agent_id`, `session_id`, `readonly`, `pty.is_some()`, and delegates the transcript sub-map to `DebugView<'_, StructuredLogSource>`.
- **`crates/amux/src/agents/mod.rs`** — **Deleted** four debug-only `AgentSession` accessors (`session_id`, `transcript_path`, `transcript_status`, `transcript_error`). Added `impl Serialize for DebugView<'_, AgentSession>` that delegates to the variant impls.
- **`crates/amux/src/agents/testagent.rs`** — Added `impl Serialize for DebugView<'_, TestAgentSession>` (`TestAgentSession` itself is `cfg(any(debug_assertions, test))`).
- **`crates/amux/src/lib.rs`** — Added `mod debug;`.
- **`crates/amux/src/cloud.rs`, `crates/amux/src/server/connection.rs`** — Updated four test sites that constructed `Command::Debug { verbose: false }` to include `format: DebugFormat::Yaml`.
- **`crates/amux-cli/src/main.rs`** — Added a CLI-side `CliDebugFormat` `clap::ValueEnum` (parallel to `protocol::DebugFormat`) and a `--format` flag on the `Debug` subcommand. Avoids pulling clap into the `amux` library crate.
- **`crates/amux-cli/src/client.rs`** — `client::debug` now takes a `DebugFormat`, sends it in the request, and returns the pre-rendered `String` instead of a typed `ServerDebugInfo`.

### Decisions Made
- **Per-type `Serialize` impls instead of a centralized builder.** The original approach forced six new public accessors onto `ClaudeSession`/`AgentSession`/`StructuredLogSource` and a 200-line `build_debug_info` that knew the shape of every subsystem. Inverting it so each type owns its own debug rendering eliminated the public-API growth, deleted the central builder entirely, and made future "add a debug field" changes a one-liner next to the field itself.
- **`DebugView` newtype + manual `Serialize` over `serde_json::Value` tree or `DebugWriter` DSL.** `DebugView` is the most idiomatic Rust path: it's pure serde, no intermediate allocation, no new dependencies, no global feature flags (`preserve_order` would have unified across the workspace and changed observable JSON ordering in unrelated subsystems). Format selection becomes "which `Serializer` did we pass" — the impls don't know YAML from JSON.
- **Wire format is `String`, not `Value`.** Sending a `Value` over MessagePack would have been a more flexible protocol but added complexity for no real benefit: the CLI doesn't post-process the dump (`jq` operates on the rendered text), debug is one-shot not streaming, and the renderer code lives one module away from the request handler regardless. `String` keeps the protocol minimal.
- **`current_path` flipped to `std::sync::Mutex`.** It was always safe — never held across `await` — so this is the right shape. Doing it lets the `Serialize` impl for `StructuredLogSource` read the path from sync context without an async pre-fetch step or a snapshot side-channel.
- **Dropped `structured_output_seq` from the dump.** The remaining async-only field was the buffer's `current_seq` (behind a `tokio::sync::RwLock`). Exposing a `try_read`-based sync wrapper would have widened the public surface again for marginal value, so it's out.
- **Visibility widening was `pub(super)` → `pub(crate)`, not `pub`.** Stays internal to the `amux` crate, no public API impact.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — clean across the workspace, 304 unit tests pass.
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 e2e tests pass.
- New smoke tests in `server::handlers::tests`: `debug_yaml_dump_is_non_empty_and_parses` and `debug_json_dump_is_non_empty_and_parses` populate a representative state (local agent + remote agent + peer link + named route + subscription) and assert both formats produce parseable output with the expected top-level keys.

### Net diff shape
~250 lines deleted overall — protocol module ~−120, handlers ~−280, agent files ~−80 in deleted accessors — in exchange for one new ~530-line `debug.rs` file that's cleanly scoped and small `Serialize` impls in each type's home file.

### Next Steps
- Optional follow-up: `link_state_tx` is a `watch::Sender` with zero subscribers anywhere in the codebase. It could be simplified to a plain `std::sync::Mutex<LinkState>` in a separate small commit. Out of scope for this work to keep the change focused on debug.

---

## 2026-04-09: Jitter cloud reconnects and reset backoff after stable sessions

### Summary
Adjusted the cloud reconnect loop so retry sleeps now include both relative jitter and an absolute early-retry smear, which reduces reconnect storms after shared cloud disconnects. Fixed the stale-backoff behavior by resetting the exponential base after clean disconnects and after cloud sessions that stayed up long enough to count as stable, while preserving exponential growth for repeated post-handshake failures like heartbeat or transport flaps.

### Changes
- `crates/amux/src/server/cloud.rs` — Added jittered retry delay calculation using `base_backoff + uniform(-25%, +25%) + uniform(0s, 5s)`, split base-backoff progression into helpers, logged both `base_backoff` and actual `retry_delay`, and introduced a `30s` stability threshold before post-handshake failures reset the retry base. Added focused unit tests for jitter bounds, capped doubling, and the stability-threshold reset behavior.
- `CLOUD_ARCHITECTURE.md` — Updated the cloud reconnect description to note jittered exponential backoff and that the base resets after a stable session or clean disconnect.

### Decisions Made
- Keep the `300s` cap as the capped exponential base, not the final sleep. Jitter is applied on top of the capped base so the retry curve stays easy to reason about while still spreading reconnect load.
- Add an absolute `0..5s` jitter term in addition to `+/-25%` relative jitter so mass disconnects do not cause all clients to retry within the same first second.
- Do not reset backoff immediately on handshake success. That would collapse exponential backoff under repeated post-handshake failures, so the reset now requires either a clean EOF or at least `30s` of successful uptime.

### Verification
- `cargo fmt --all`
- `cargo test -p amux server::cloud::tests --lib`
- `cargo clippy --workspace --all-targets`

### Next Steps
- Observe production reconnect logs to see whether the `30s` stability threshold and `0..5s` absolute jitter need tuning.

## 2026-04-09: Migrate subscriptions to leased subscription IDs

### Summary
Completed the subscription protocol migration from agent-scoped output streams to owner-issued leased subscription IDs. Raw and structured subscriptions now return `subscription_id` plus `lease_ms`, outputs and close events are keyed by `subscription_id`, the server owns subscription lifecycle end-to-end, and the CLI renews leases and unsubscribes explicitly on detach. Follow-up fixes tightened cancellation ordering, made lease renewal server-authoritative, raised the lease to five minutes, moved the lease sweeper to a non-blocking best-effort close path, and routed unsolicited close messages through the destination connection's own request-id counter.

### Changes
- `crates/amux/src/message.rs` — Added `SubscriptionId`, `SubscriptionCloseReason`, `ExtendSubscription`, `Unsubscribe`, lease-bearing subscribe/extend results, and `ProtocolError::UnknownSubscription`. Removed `agent_id` from subscription-scoped output/close messages and updated codec coverage.
- `crates/amux/src/server/mod.rs` — Replaced `active_streams` with `active_subscriptions`, added lease deadlines, a 5-minute lease constant, a 10-second sweep cadence, `ConnectionHandle` for per-route sender plus request-id allocation, and a non-blocking lease-expiry close path.
- `crates/amux/src/server/connection.rs` — Switched subscription bookkeeping to subscription-keyed helpers for register/cleanup/extend/unsubscribe and route-based cancellation.
- `crates/amux/src/server/handlers.rs` — Raw and structured subscribe now allocate owner-side subscription IDs, return leases before spawning streams, emit output by `subscription_id`, handle extend/unsubscribe, and ensure cancel happens before best-effort close. Added subscription lifecycle, lease-expiry, remote-unsubscribe, and no-synthetic-EOF regressions.
- `crates/amux/src/server/accept.rs`, `crates/amux/src/server/cloud.rs`, `crates/amux/src/server/routing.rs` — Threaded `ConnectionHandle` through route registration so forwarded and server-originated routable messages share the destination connection’s request-id counter.
- `crates/amux-cli/src/client.rs` — Added `AttachedSession`, switched attach to server-assigned subscription IDs, folded lease renewal into the main attach loop, updated renewal timing from `ExtendSubscriptionResult.lease_ms`, treated `UnknownSubscription` as terminal, and sent `Unsubscribe` on graceful detach.
- `Cargo.toml`, `crates/amux/src/protocol.rs`, `crates/amux/src/transport/unix.rs` — Updated supporting exports/tests and enabled paused-time Tokio test utilities used by the new lease-expiry coverage.

### Decisions Made
- Subscription identity is now `subscription_id` only after subscribe. `agent_id` remains on subscribe and input messages so output routing stays decoupled from input routing.
- Lease expiry is cleanup, not topology. Unexpected disconnects are handled by lease timeout rather than intermediate-hop subscription bookkeeping.
- Renewal timing is server-authoritative. The client replaces its current lease with each successful `ExtendSubscriptionResult.lease_ms` and reschedules from “now”.
- Unsolicited routable messages should use the destination connection’s request-id counter, not a global counter or `0`, so route entries now store both sender and counter.
- Lease-expiry close sends are best-effort and non-blocking so a saturated subscriber queue cannot stall the server main loop.

### Verification
- `cargo check`
- `cargo fmt`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test`
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 10/10 tests passed (`attach`, `list_agents`, `local_agent_ended`, `multiple_agents`, `new_agent`, `remote_agent_ended`, `remote_attach_by_alias`, `remote_connection`, `remote_list_agents`, `replay_buffer`)

### Next Steps
- Update any out-of-tree clients or tools to the new leased subscription wire format.
- Revisit lease duration and sweep cadence with real-world attach/disconnect behavior once the new protocol has more usage data.

## 2026-04-08: Store full stream routes for subscription cleanup

### Summary
Refactored active subscription bookkeeping so `StreamEntry` stores the full destination route instead of splitting it across `link` plus a suffix. This fixes a stale structured/raw subscription cleanup bug introduced during the host-routing overhaul: when a downstream host link disappeared behind an otherwise live peer, `WithdrawHost` cleanup compared a full withdrawn route against only the stored suffix and failed to cancel the upstream stream task. With full routes in `active_streams`, host-withdrawal and peer-disconnect cleanup now reason about the same route shape.

### Changes
- `crates/amux/src/server/mod.rs` — Simplified `StreamEntry` to store only `stream_id`, `cancel`, and full `dst`.
- `crates/amux/src/server/handlers.rs` — `spawn_subscription_stream()` now reconstructs the full subscriber route before registering the stream. Added a regression test covering `WithdrawHost` cancellation for a matching multi-hop route.
- `crates/amux/src/server/connection.rs` — Local connection teardown now identifies attached streams by `entry.dst.peek()` instead of a separate stored link.
- `crates/amux/src/server/routing.rs` — Peer-disconnect cleanup now cancels streams purely by route containment. Updated routing tests for the full-route stream model.

### Decisions Made
- `active_streams` should store one canonical full route, not a split representation, because route-based cleanup is now the primary model and the split form was too easy to misuse.
- The runtime send path still uses `reply_src`/`reply_dst`; `StreamEntry` is cleanup metadata only, so it should optimize for correctness and clarity rather than mirroring the post-`Route::send()` transport state.

### Verification
- `cargo fmt`
- `cargo test -p amux withdraw_host_cancels_streams_with_matching_full_route`
- `cargo test -p amux withdraw_host_route_mismatch_preserves_root_but_cleans_stale_descendants`
- `cargo test -p amux peer_disconnect_cancels_streams_on_link`
- `cargo test -p amux peer_disconnect_cancels_streams_routed_through_link`
- `cargo test -p amux peer_disconnect_full_cascade`
- `cargo test -p amux stream_cancelled_stops_without_subscription_closed`

## 2026-04-07: Separate network topology from agent announcements

### Summary
Completed the separation of network topology (AnnounceHost/WithdrawHost) from application-level resource announcements (AnnounceAgent/WithdrawAgent). Removed `route` from `AnnounceAgent` — agents are now associated with hosts via `host_id`, and routes are derived from the host table at read time. Added `route` to `WithdrawHost` so receivers can match the withdrawal to the correct path and cascade cleanup. Added `Route::replace_prefix` for local host route normalization when a host is re-announced via a different path. Cloud servers now reject `CreateAgent` and `Resume` requests since they are stateless relays. Peer disconnect now computes root hosts and sends minimal `WithdrawHost` messages (one per root, not per descendant).

### Changes
- `crates/amux/src/message.rs` — Removed `route` from `AnnounceAgent`. Added `route` to `WithdrawHost`.
- `crates/amux/src/route.rs` — Added `Route::replace_prefix` for in-place prefix substitution.
- `crates/amux/src/agent_registry.rs` — `remove_for_link` and `remove_for_route_prefix` replaced with generic `remove_where(host_route, predicate)`. Added `StoredAgent::announce_message()`. Removed route validation from registration methods.
- `crates/amux/src/server/handlers.rs` — `AnnounceAgent` validates host exists and is reachable via sender's link, but no longer threads route through the `Agent` struct. `AnnounceHost` rewrites descendant host routes locally when a parent's route changes. `WithdrawHost` cascades: removes agents, cancels streams, removes descendant hosts, and propagates. Cloud server rejects `CreateAgent` and `Resume`. Extensive new tests for announce/withdraw/rename/delete flows.
- `crates/amux/src/server/routing.rs` — `handle_peer_disconnect` computes `disconnected_hosts` and `disconnected_host_roots` to send minimal withdrawal messages. Helper functions `descendant_host_ids`, `rewrite_descendant_host_routes`, `remove_descendant_hosts` for host subtree operations. New tests for root-only withdrawal and full cascade.

### Decisions Made
- `WithdrawHost` carries `route` so receivers can match the withdrawal to the correct stored path and avoid removing hosts learned via a different route
- Descendant host route rewrites on `AnnounceHost` are local normalization only — not rebroadcast. Each hop applies the same normalization independently.
- Peer disconnect sends `WithdrawHost` only for root hosts (shortest routes). Descendants are removed locally and cleaned up by each receiver when processing the root withdrawal.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 278 tests pass

---

## 2026-04-05: Structured I/O transport refactor — opaque JSON passthrough

### Summary
Replaced the typed semantic structured output model (`ClaudeStructuredOutput` with 18 variants, `AgentStructuredOutput` wrapper, `TranscriptParser` state machine) with an opaque `serde_json::Value` transport. amux no longer interprets transcript semantics — it passes through raw JSONL entries as `Value` and lets the client own all transcript/tool interpretation. Hook-originated events use `hook.*` type namespace. Added `structured_protocol: Option<String>` to agent announcements so clients know how to interpret payloads.

### Changes
- `crates/amux/src/buffer.rs`: `StructuredOutput.data: AgentStructuredOutput` → `StructuredOutput.payload: Value`
- `crates/amux/src/message.rs`: `RoutableMessage::StructuredOutput/StructuredInput` now carry `payload: Value`; removed `ProtocolError::StructuredInputTypeMismatch`; added `structured_protocol` to `AnnounceAgent`
- `crates/amux/src/agent_registry.rs`: Added `structured_protocol: Option<String>` to `Agent`
- `crates/amux/src/claude/types.rs`: Removed `AgentStructuredOutput`, `ClaudeStructuredOutput` (18 variants), `AgentStructuredInput`, `AssistantContentBlock`, `MessageUsage`, `CompactMetadata`; kept `ClaudeStructuredInput` and all hook/tool types
- `crates/amux/src/claude/transcript.rs`: Gutted `TranscriptParser` — now just parses each JSONL line to `Value` and writes directly to buffer
- `crates/amux/src/claude/structured_log_source.rs`: `write()` accepts `Value`
- `crates/amux/src/claude/types.rs`: `Hook` now carries `(ClaudeHook, Value)` — typed parse for internal side effects, raw JSON for lossless structured output passthrough; added `Hook::from_claude()` convenience constructor for tests
- `crates/amux/src/agents/claude.rs`: `hook.permission_request` and `hook.stop` pass through raw hook JSON with `type` field injected (lossless — no field loss from typed round-tripping); `SessionEnd` is internal-only cleanup, not emitted as structured output; structured input parsed from `Value` to `ClaudeStructuredInput` locally; name sniffer reads `Value` fields directly; `to_agent()` sets `structured_protocol: Some("claude_pty_v1")`
- `crates/amux-cli/src/hooks.rs`: Captures raw JSON `Value` alongside typed `ClaudeHook` and sends both in `Hook::Claude`
- `crates/amux/src/agents/mod.rs`: `send_structured_input` accepts `Value`; removed agent-type dispatch
- `crates/amux/src/server/handlers.rs`, `routing.rs`: Updated for new types and `structured_protocol`

### Decisions Made
- `serde_json::Value` over `String` or `Box<RawValue>`: Value gets efficient MessagePack packing (maps/arrays/scalars) instead of double-encoding "JSON text inside a MessagePack string"
- Field named `payload` (not `json`) to avoid confusion with the serialization format
- Hook events are lossless: raw JSON from stdin is carried alongside the typed parse and emitted with only a `type` field injected (`hook.permission_request`, `hook.stop`). `SessionEnd` is not emitted — it's for amux-internal agent cleanup only
- `ClaudeStructuredInput` kept locally inside Claude PTY implementation — transport carries opaque `Value`, Claude session parses it

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 245 tests pass
- E2E tests — 10/10 pass

---

## 2026-04-03: Optional TCP/WebSocket listeners and centralized config validation

### Summary
Made TCP and WebSocket server listeners optional — they only start when the corresponding port is configured. Added a centralized `Config::validate()` method called early in startup so invalid configs fail fast with clear error messages, regardless of which subcommand is being run.

### Changes
- `crates/amux/src/config.rs`: Changed `tcp_port` and `websocket_port` from `u16` to `Option<u16>` (default: `None`). Added `Config::validate(is_cloud: bool)` that checks leader key format and cloud port requirements. Added tests.
- `crates/amux/src/server/mod.rs`: TCP/WS listeners are now conditional. Uses `std::future::pending()` in the select loop when a listener is disabled. Added server-side validation call.
- `crates/amux/src/server/accept.rs`, `crates/amux/src/server/handlers.rs`: Updated `tcp_port` reads to unwrap `Option` (safe: cloud mode guarantees `Some` via validation).
- `crates/amux-cli/src/main.rs`: Calls `config.validate()` immediately after loading config, before any command runs. Malformed implicit config now errors instead of silently falling back to defaults.
- `crates/amux/src/connect.rs`: `connect_embedded` and `connect_daemon` validate config before spawning, so callers get the real error instead of an opaque 5s timeout.

### Decisions Made
- Ports default to `None` (no listener) rather than defaulting to 9001/9002: local-only users shouldn't bind network ports they don't need.
- Cloud mode requires both ports via validation, so the `expect()` calls in JWT token handling are safe.
- Leader key validation in `validate()` is a belt-and-suspenders check alongside serde deserialization validation — covers programmatic construction too.
- Malformed implicit config (default path exists but fails to parse) is now a hard error, not a warning-and-fallback. With ports defaulting to None, the old fallback would silently disable network listeners.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — 263 unit tests pass
- E2E tests: 10/10 pass (E2E runner auto-assigns ports in generated configs)

---

## 2026-04-03: Update availability notification and graceful shutdown broadcast

### Summary
Added an update notification system so users see a banner when a newer amux version is available. The server spawns an hourly background task that fetches the release manifest and writes a marker file when an update exists. Clients read this file and display a banner on exit (attached sessions) or after command output (one-shot commands like `list`, `shutdown`). Also added graceful `ShutdownNotification` broadcast to all connected clients on server shutdown/suspend, so attached sessions see `[server shutting down]` or `[server updating]` instead of a raw connection error.

### Changes
- `crates/amux/src/update.rs` (new) — public module with manifest fetch, semver comparison, marker file read/write/clear, and `spawn_update_checker` background task
- `crates/amux/src/lib.rs` — added `pub mod update`
- `crates/amux/Cargo.toml` — added `semver` dependency
- `crates/amux/src/message.rs` — added `ShutdownReason::Updating` variant
- `crates/amux/src/server/mod.rs` — spawns hourly update checker (local server only), added `notify_other_clients()` to broadcast `ShutdownNotification` to all routes except the requester, notifications sent before `shutdown_server()`/`suspend_server()` so clients see them before streams close
- `crates/amux/src/server/handlers.rs` — `ShutdownRequest` now carries `link_name` for exclude-from-broadcast
- `crates/amux-cli/src/client.rs` — `print_update_banner()` reads marker file with stale version self-healing, called after output in `list_agents`/`kill_server` and on all attached session exit paths (skipped when `ShutdownReason::Updating`)
- `crates/amux-cli/src/update.rs` — `amux update` clears marker file after successful update and on "already up to date"

### Decisions Made
- File-based approach over protocol messages: avoids breaking request/response ordering in one-shot CLI flows, allows showing banners before or after any command without protocol changes, instant (no network round-trip)
- Marker file (`~/.local/state/amux/update-available`) contains two lines: current_version and update_version. Self-heals on version mismatch (stale marker from pre-upgrade deleted by client)
- Server owns the marker (writes/refreshes/clears on check), `amux update` also clears it. Normal clients never delete.
- `ShutdownNotification` broadcast sent before `suspend_server()`/`shutdown_server()` to avoid race where `SubscriptionClosed` from stream teardown arrives first
- Update banner shown after command output (not before) for one-shot commands — command output is what the user asked for, banner is supplementary

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` — all 248 tests pass, zero warnings
- Manual testing: temporarily set version to 0.1.21, built, ran `amux-dev list` — banner displayed correctly after agent list. Attached to agent, detached — banner shown after `[detached from session]`. Reverted version.

### Next Steps
- Consider interactive upgrade prompt before commands (e.g. "Update now? [y/N]")
- Future: use `AnnounceHost` to notify remote peers about available updates for remote update support

---

## 2026-04-03: Remove readonly session reaper and PID tracking

### Summary
Removed the periodic reaper task that checked whether external Claude processes behind readonly sessions were still alive. The reaper was added based on the incorrect belief that `SessionEnd` hooks didn't fire on Ctrl+C — this turned out to be a local testing misconfiguration. The existing `SessionEnd` hook handler already withdraws readonly sessions reliably, making the reaper redundant.

### Changes
- Deleted `crates/amux/src/process.rs` — `current_parent_pid()` and `process_exists()` helpers
- `crates/amux/src/lib.rs` — removed `mod process` and `pub use process::current_parent_pid`
- `crates/amux/src/agents/claude.rs` — removed `external_pid` field, `sync_hook_source_ppid()`, `external_pid()` accessor, and `source_ppid` parameter from `handle_hook()`
- `crates/amux/src/agents/mod.rs` — removed `source_ppid` parameter from `AgentSession::handle_hook()` and `external_pid()` method
- `crates/amux/src/message.rs` — removed `source_ppid` field from `Command::HandleHook`
- `crates/amux/src/server/mod.rs` — removed `READONLY_REAP_INTERVAL`, reaper task spawn, `withdraw_dead_readonly_sessions_with()`, `reap_dead_readonly_sessions()`, and related tests
- `crates/amux/src/server/handlers.rs` — removed `source_ppid` threading through hook handling
- `crates/amux-cli/src/hooks.rs` — removed `current_parent_pid` import and usage
- `crates/amux/src/claude/types.rs` — removed `source_ppid` from test
- `crates/amux/Cargo.toml` — removed `windows-sys` dependency (only used by `process.rs`)

### Decisions Made
- Rely on `SessionEnd` hook for readonly session cleanup (already implemented in handlers.rs)
- Remove `windows-sys` dependency entirely since it was only used for PID checking in `process.rs`
- Keep `libc` dependency — still used in `config.rs` for `getuid()`

### Verification
- `cargo check` — clean
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 241 tests pass

---

## 2026-04-03: Add created_at timestamp to agents

### Summary
Added `created_at: DateTime<Utc>` field to agents. The timestamp is set when an agent is first created (including readonly agents), propagated via `AnnounceAgent` messages to remote servers, and preserved through suspend/resume cycles.

### Changes
- `crates/amux/src/agent_registry.rs` — added `created_at: DateTime<Utc>` to `Agent` struct
- `crates/amux/src/message.rs` — added `created_at` field to `DirectMessage::AnnounceAgent` variant
- `crates/amux/src/agents/claude.rs` — added `created_at` field to `ClaudeSession`, set to `Utc::now()` in `new()` and `new_readonly()`
- `crates/amux/src/agents/testagent.rs` — added `created_at` field to `TestAgentSession`, set to `Utc::now()` in `new()`
- `crates/amux/src/agents/mod.rs` — added `created_at()` accessor on `AgentSession`, threaded through `to_agent()`, `SuspendedAgent` enum variants, `suspend()`, and `into_session()`
- `crates/amux/src/server/routing.rs` — propagated `created_at` in all 4 `AnnounceAgent` construction sites (create, resume, rename, initial announcements)
- `crates/amux/src/server/handlers.rs` — propagated `created_at` in readonly agent creation and AnnounceAgent receive/re-broadcast handler

### Decisions Made
- Uses `chrono::DateTime<Utc>` — consistent with existing usage in cloud.rs/oauth.rs, serde support already enabled
- Readonly agents get `Utc::now()` at the time amux first sees them (external process start time is unknown)
- Resumed agents preserve their original `created_at` from before suspend

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 243 unit tests pass
- E2E tests — 10/10 pass

---

## 2026-04-03: Add local agent name sniffing with provider-derived rename support

### Summary
Agents can now be automatically renamed from structured log output. A name sniffer task watches the structured log source for slug and agent_name events, emitting `SessionEvent::NameCandidateChanged` when a new best name is discovered. The server applies candidates respecting a precedence hierarchy (Unset < ProviderSlug < ProviderName < Amux), updates the registry, session, and re-announces to peers. User-supplied (`Amux`) names are never overridden.

### Changes
- `crates/amux/src/agent_registry.rs` — added `NotFound` error variant, `update_local()` method for in-place metadata updates with alias collision protection
- `crates/amux/src/agents/mod.rs` — added `LocalAgentNameSource` enum with `is_automatic()`/`rank()` methods, `SessionEvent::NameCandidateChanged` variant, name sniffer lifecycle methods on `AgentSession`, `name_source` field in `SuspendedAgent`
- `crates/amux/src/agents/claude.rs` — added `NameSnifferState` (split into `ingest`/`effective_candidate`/`observe`), `spawn_name_sniffer()`, name sniffer lifecycle on `ClaudeSession`
- `crates/amux/src/server/routing.rs` — added `apply_local_name_candidate()` with read-then-write phase separation, name sniffer startup on agent creation and resume
- `crates/amux/src/server/mod.rs` — extracted `handle_session_event()`, added `NameCandidateChanged` handling
- `crates/amux/src/server/handlers.rs` — name sniffer startup for readonly external sessions
- `crates/amux/src/message.rs` — doc comment on `AnnounceAgent`
- `crates/amux/src/state.rs` — updated suspended agent roundtrip tests for `name_source`

### Decisions Made
- Precedence as methods on the enum (`is_automatic()`, `rank()`) rather than a free function returning `Option<u8>` — keeps domain logic co-located and simplifies call sites
- `NameSnifferState` split into three methods (ingest, effective_candidate, observe) — separates mutation from computation from dedup orchestration
- `apply_local_name_candidate` uses immutable borrow for validation, then mutable for mutations — avoids tuple-extraction pattern to work around borrow checker
- Same-name with higher-precedence source upgrades provenance without peer re-announcement — avoids unnecessary network chatter

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 243 tests pass

---

## 2026-04-02: Restore immediate structured subscribe semantics

### Summary
Reverted Claude structured subscribe to the pre-March-29 “attach immediately to the current buffer” behavior so opening a brand-new or not-yet-linked chat no longer blocks the websocket connection. Kept the later transcript-link protections that are still required for the newer hook model: same-path transcript links are ignored, path changes still clear and relink safely, and PTY exit still stops the full structured log source so transcript tailers do not leak.

### Changes
- `crates/amux/src/server/handlers.rs` — changed `SubscribeStructured` back to immediate subscribe against the current structured buffer snapshot instead of waiting for transcript linkage
- `crates/amux/src/agents/claude.rs` / `crates/amux/src/agents/mod.rs` / `crates/amux/src/agents/testagent.rs` — restored immediate structured subscribe APIs and removed the Claude-side “structured session not ready” input gate
- `crates/amux/src/claude/structured_log_source.rs` / `crates/amux/src/claude/transcript.rs` — removed subscribe-time readiness gating and pending-write buffering, but kept same-path relink suppression, awaited old tailer shutdown before clear-on-relink, and full source close semantics
- Added regression coverage for immediate empty subscribe, delayed replay after immediate subscribe, same-path relink no-op, generation clearing on actual relink, and unlinked Claude subscribe returning immediately

### Decisions Made
- Immediate subscribe is the server contract again: clients own fork/input readiness and must tolerate history arriving after subscribe
- Keep transcript-link dedupe by path: `sync_hook_metadata()` links on every hook carrying `transcript_path`, so re-linking the same transcript on later hooks must not clear/replay the session
- Keep awaited tailer shutdown on real relinks and full `StructuredLogSource::close()` on PTY exit: those later fixes prevent stale-generation writes and transcript tailer leaks and are independent of subscribe semantics

### Verification
- `cargo fmt --all`
- `cargo test -p amux --lib` — 238 tests pass

### Next Steps
- Client: use fork-anchor/watermark logic to decide when optimistic input is ready to send on a newly forked runtime

## 2026-03-31: Add PlanReviewResponse structured input

### Summary
Added `PlanReviewResponse` as a new variant of `ClaudeStructuredInput`, enabling the app to send plan review responses (YesAuto, YesManual, No with optional feedback) to Claude Code via PTY keystrokes. Also renamed `PlanModeReview` to `PlanReviewResponse` across amuxapp for consistency with the existing `AskUserQuestionResponse` and `PermissionRequestResponse` naming pattern.

### Changes
- `crates/amux/src/claude/types.rs`: Added `PlanReviewResponse` enum (YesAuto, YesManual, No(Option<String>)) and new `ClaudeStructuredInput::PlanReviewResponse` variant
- `crates/amux/src/agents/claude.rs`: Added `plan_review_response_keystrokes()` function and wired it into `send_input()`
- `amuxapp`: Renamed `PlanModeReview` → `PlanReviewResponse` across types/chat.ts, agents/claude/input.ts, session/types.ts, components/chat/permission-panel.tsx, logging/structured-input.ts, logging/structured-input.test.ts, session/store.test.ts

### Decisions Made
- PTY keystroke mapping: 1=YesAuto, 2=YesManual, 3=No (matches Claude Code's plan review prompt order)
- For No responses: send "3", delay, optional feedback message, delay, Enter (because Claude expects an optional description after rejection)
- Wire format key is `PlanReviewResponse` (not `PlanModeReview`) for consistency with other response types

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 235 tests pass, zero warnings

---

## 2026-03-31: Transcript parser: slug, new entry types, stop_hook_summary skip

### Summary
Compared the transcript parser against a live Claude Code JSONL transcript and filled in missing entry types, added `slug` propagation, and suppressed internal bookkeeping entries from structured output.

### Changes
- `crates/amux/src/claude/transcript.rs` — added `slug` field to `User`, `Assistant`, `System` transcript entry variants; added explicit `FileHistorySnapshot`, `QueueOperation` variants (parsed but not emitted); added `local_command` system subtype → `LocalCommand` output; suppressed `stop_hook_summary` from structured output; refactored `parse_assistant`/`parse_tool_result` to use `EntryContext` struct (clippy too-many-arguments); removed debug logging from `parse_entry`; added tests for all new behavior
- `crates/amux/src/claude/types.rs` — added `slug: Option<String>` to `UserMessage`, `AssistantMessage`, `PostToolUseEvent`, `ToolUseRejected`, `TurnDuration`, `ApiError`, `CompactBoundary`, `LocalCommand`, `SystemEvent` variants; added `LocalCommand` variant to `ClaudeStructuredOutput`
- `crates/amux/src/buffer.rs`, `crates/amux/src/claude/structured_log_source.rs` — updated test helpers for new `slug` field

### Decisions Made
- `stop_hook_summary` is internal hook machinery — skip silently rather than emitting as structured output
- `file-history-snapshot` and `queue-operation` are internal bookkeeping — parse explicitly but don't emit (queue-operation may be useful later)
- `slug` goes on all output types sourced from entries that carry it, but not on `PreToolUseEvent` (synthesized from assistant content blocks, slug lives on the parent entry)
- `local_command` is worth surfacing — it captures slash commands like `/rename` that change session state

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — all 235 tests pass, zero warnings

### Next Steps
- Consider propagating `queue-operation` as structured output if needed for UI message queueing indicators

## 2026-03-30: Reap readonly Claude sessions by external PID

### Summary
Readonly Claude sessions were sticking around because Claude Code does not reliably emit `SessionEnd` for interactive exits, and `Stop` is not a terminal signal. Changed amux to capture the hook process parent PID for external Claude hooks, track that PID on readonly sessions, and periodically reap readonly sessions whose external Claude process no longer exists. The reaper only runs on local/client servers, not the cloud relay.

### Changes
- `crates/amux-cli/src/hooks.rs` / `crates/amux/src/message.rs` / `crates/amux/src/claude/types.rs` — added `source_ppid` to `HandleHook` and propagated it through hook encoding/decoding
- `crates/amux/src/process.rs` / `crates/amux/src/lib.rs` / `crates/amux/Cargo.toml` / `Cargo.lock` — added cross-platform helpers for parent-PID capture and process-existence checks
- `crates/amux/src/agents/claude.rs` / `crates/amux/src/agents/mod.rs` — added readonly `external_pid` tracking and surfaced it through `AgentSession`
- `crates/amux/src/server/mod.rs` — added a readonly-session reaper that runs every 5 minutes on non-cloud servers and withdraws readonly sessions whose tracked external PID has exited
- `crates/amux/src/server/handlers.rs` — threaded `source_ppid` through existing hook handling and readonly session creation

### Decisions Made
- Do not treat `Stop` as terminal — Claude can emit `Stop` for a live session and continue sending later hooks, so cleanup must not be hook-driven
- Key cleanup off external process liveness, not hook semantics — if Claude dies without a final lifecycle hook, the reaper still converges state
- Run the reaper only on client/local servers — the cloud relay should never own readonly Claude sessions, so sweeping there is redundant work
- Use a 5 minute sweep interval — readonly cleanup does not need to be phone-responsive, and the longer interval reduces pointless periodic work and resume flicker
- Accept best-effort PID identity — this depends on the hook parent PID being the Claude process as observed today; if Claude changes hook launching to insert a wrapper shell, this heuristic may need to be revisited

### Verification
- Manual hook probe: verified `SessionEnd` fires for a vanilla interactive Claude session when the Claude process is terminated directly, and verified interactive TUI exit paths can skip `SessionEnd`
- Manual persistence check: verified readonly sessions persist under the PID-based approach instead of being immediately reaped
- `cargo test -p amux`
- `cargo test --workspace --no-run`

### Next Steps
- If PID false-positives ever appear, harden the external-process identity check with process start-time validation instead of PID alone

## 2026-03-29: Fix structured subscribe gating and stream EOF teardown

### Summary
Changed structured subscriptions to wait for Claude transcript linkage instead of relying on delayed agent announcement timing, and fixed two lifecycle regressions uncovered during review. Withdrawing an agent now preserves active stream entries until their underlying buffers close so clients still receive terminal EOF notifications, and PTY exit now closes the full `StructuredLogSource` so `wait_until_linked()` resolves with a closed error if Claude exits before any transcript is linked.

### Changes
- `crates/amux/src/agents/mod.rs` — added `StructuredSubscription`, routed Claude subscriptions through `wait_until_linked()`, and changed PTY exit cleanup to close the full `StructuredLogSource`
- `crates/amux/src/claude/structured_log_source.rs` — added explicit link lifecycle state (`Unlinked`/`Linking`/`Linked`/`Failed`/`Closed`), pending-write buffering, and close/failure wakeups for blocked subscribers
- `crates/amux/src/claude/transcript.rs` — wired transcript tailers to mark link success/failure on the owning `StructuredLogSource`
- `crates/amux/src/server/handlers.rs` — changed `SubscribeStructured` to surface link-wait errors and readonly `SessionEnd` cleanup to stop the withdrawn session after releasing the write lock
- `crates/amux/src/server/routing.rs` — changed `withdraw_agent()` to preserve `active_streams` until stream tasks observe EOF and clean themselves up
- `crates/amux/src/server/mod.rs` — changed fork cleanup to stop the withdrawn readonly session after the registry/state removal
- `crates/amux/src/agents/claude.rs` / `crates/amux/src/agents/testagent.rs` — aligned session subscribe APIs with the new fallible structured-subscribe path and added coverage for non-`SessionStart` transcript linking / immediate test-agent subscribe

### Decisions Made
- Keep `SubscriptionClosed` tied to reader exhaustion, not synthetic cancellation — route teardown still uses explicit cancellation, but normal session withdrawal now lets streams terminate naturally
- Close the entire `StructuredLogSource` on process exit — closing only the underlying buffer was not enough because `wait_until_linked()` needed a terminal watch-state transition
- Preserve the fork-and-swap model — readonly source sessions are still withdrawn immediately on fork, but subscribers now drain via EOF instead of being cancelled out from under the stream task

### Verification
- `cargo fmt --all`
- `cargo test -p amux --lib`

### Next Steps
- Keep the mobile client aligned with the blocking `SubscribeStructured` semantics and explicit subscribe-error handling during readonly fork handoff

## 2026-03-23: External session capture and fork-and-swap

### Summary
Added the ability for amux to capture Claude sessions started outside of amux as readonly sessions. When a Claude Code hook fires without `AMUX_AGENT_ID`, the hook handler uses the session's `session_id` as the `agent_id` and sends the hook to the server. The server creates a readonly `ClaudeSession` (no PTY, just transcript tailing) and announces it to peers. Clients can fork a readonly session into a full PTY-backed session by creating a new agent with `--resume <id> --fork-session` args; an event handler detects this and auto-withdraws the readonly source. A `SessionEnd` hook cleans up readonly sessions when the external Claude process exits.

### Changes
- `crates/amux/src/message.rs` — added `args: Vec<String>` to `CreateAgentRequest`, `agent_type: AgentType` and `readonly: bool` to `AnnounceAgent`
- `crates/amux/src/agent_registry.rs` — added `agent_type: AgentType` and `readonly: bool` to `Agent` struct
- `crates/amux/src/claude/types.rs` — added `SessionEnd` variant to `ClaudeHook`, added `cwd` and `transcript_path` fields to all hook variant structs, added `session_id()`, `cwd()`, `transcript_path()` accessors
- `crates/amux/src/agents/claude.rs` — added `readonly` and `args` fields, `new_readonly()` constructor, `link_transcript()` method, `SessionEnd` hook handling, readonly input gating
- `crates/amux/src/agents/mod.rs` — added `readonly()` method, `SessionEvent::Created` variant, updated `to_agent()` with `agent_type`
- `crates/amux-cli/src/hooks.rs` — restructured to handle external sessions (no `AMUX_AGENT_ID`)
- `crates/amux/src/server/handlers.rs` — readonly session creation on external hooks, `SessionEnd` cleanup, `agent_type` propagation in `AnnounceAgent`
- `crates/amux/src/server/routing.rs` — `withdraw_agent()` helper, `SessionEvent::Created` emission
- `crates/amux/src/server/mod.rs` — fork detection event handler, `withdraw_agent()` usage

### Decisions Made
- `readonly` as a flag on `Agent`/`AnnounceAgent` (not a separate `AgentType` variant) — readonly is orthogonal to agent type and will apply to Codex and future agents too
- `agent_type` propagated through `AnnounceAgent` and `Agent` — sets up for adding Codex and other agent types
- Use `session_id` as `agent_id` for readonly sessions — avoids needing a mapping table
- Any hook (not just SessionStart) can create a readonly session — all hooks carry `cwd` and `transcript_path`, so sessions survive server restarts
- Skip readonly sessions during suspend — they're ephemeral, the next hook re-creates them
- `withdraw_agent()` extracted as a method — the remove-from-agents + remove-from-registry + broadcast-withdraw pattern appeared in 4 places
- `CreateAgentRequest` gets `args: Vec<String>` for general CLI passthrough — enables `--fork-session --resume <id>` and future args like `--allow-dangerously-skip-permissions`

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — all tests pass
- E2E tests — all 10 pass

### Next Steps
- CLI support for `amux new claude --name x -- --extra-args`
- Mobile app integration: detect readonly sessions, fork-on-input UX
- E2E test for external session capture flow

---

## 2026-03-22: Add sequence numbers to structured I/O

### Summary
Added monotonic sequence numbers to the structured output buffer so the server can reject stale structured input from concurrent clients. Every structured output entry gets a `seq` number assigned by a new `SequencedStructuredBuffer` wrapper. Clients must include the latest `seq` when sending `StructuredInput`; the server rejects mismatches with a `StructuredInputResult` error.

### Changes
- `crates/amux/src/claude/types.rs` — renamed `StructuredOutput` → `AgentStructuredOutput`, `StructuredInput` → `AgentStructuredInput`
- `crates/amux/src/buffer.rs` — added `StructuredOutput` envelope struct (seq + data), `SequencedStructuredBuffer` wrapper with atomic seq counter
- `crates/amux/src/claude/transcript.rs` — updated to use `SequencedStructuredBuffer`
- `crates/amux/src/claude/structured_log_source.rs` — switched to `SequencedStructuredBuffer`, added `current_seq()` accessor
- `crates/amux/src/message.rs` — added `seq` field to `RoutableMessage::StructuredOutput`, `StructuredInput`, and `SubscribeStructuredResult`; added `StructuredInputResult` variant; added `ProtocolError::SequenceNumberMismatch`
- `crates/amux/src/agents/mod.rs` — added `current_seq()` to `AgentSession` dispatch
- `crates/amux/src/agents/claude.rs` — added `current_seq()` to `ClaudeSession`
- `crates/amux/src/agents/testagent.rs` — added `current_seq()` to `TestAgentSession`
- `crates/amux/src/server/handlers.rs` — structured output stream extracts seq from envelope, structured input handler validates seq and returns `StructuredInputResult` on mismatch

### Decisions Made
- `clear()` does NOT reset the seq counter — avoids confusion when clients hold a seq from before the clear
- Seq starts at 0 (no writes yet), first write gets seq 1
- `SequencedStructuredBuffer` wraps `BroadcastBuffer<StructuredPolicy>` rather than modifying the generic — keeps the generic buffer simple
- Test agent returns 0 for `current_seq()` since it has no structured log source until started

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 213 unit tests pass (including new seq tests)
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass

### Next Steps
- Client-side: include `seq` from latest `StructuredOutput` when sending `StructuredInput`
- Client-side: handle `StructuredInputResult` error responses

---

## 2026-03-20: Add PostToolUseFailure hook event

### Summary
Added `PostToolUseFailure` hook variant to handle Claude Code's failure event when a tool execution fails. Unlike `PostToolUse` (which carries `tool_response`), this event carries `error` and `is_interrupt` fields. Reuses `PreToolUse` via `#[serde(flatten)]` for typed tool dispatch, matching the existing pattern.

### Changes
- `crates/amux/src/claude/types.rs`: Added `ClaudePostToolUseFailure` struct, `PostToolUseFailure` variant to `ClaudeHook`, `PostToolUseFailureEvent` variant to `ClaudeStructuredOutput`, Display impl arm
- `crates/amux/src/agents/claude.rs`: Added `PostToolUseFailure` handler in `handle_hook` to emit `PostToolUseFailureEvent` structured output
- `crates/amux/src/server/handlers.rs`: Added `PostToolUseFailure` to hook_type label match
- `crates/amux-cli/src/hooks.rs`: Added unknown-tool warning for `PostToolUseFailure` hooks

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 219 tests pass, zero warnings

---

## 2026-03-19: PreToolUse / PostToolUse structured output via hooks

### Summary
Added PreToolUse and PostToolUse hook support to emit typed structured output events for tool activity cards. Renamed `ClaudePermissionTool` → `PreToolUse` (with type alias for backward compatibility). Added `PostToolUse` enum with typed result data per tool, `ToolUseResult` wrapper, `PatchHunk` struct, and custom deserialization for the PostToolUse hook JSON format.

### Changes
- `crates/amux/src/claude/types.rs`: Renamed `ClaudePermissionTool` → `PreToolUse`, added `PostToolUse`, `PatchHunk`, `ToolUseResult`, `ClaudePreToolUse`, `ClaudePostToolUse` structs, custom `deserialize_post_tool_use`, new `ClaudeHook` variants, new `ClaudeStructuredOutput::PreToolUseEvent`/`PostToolUseEvent` variants, 12 new tests
- `crates/amux/src/agents/claude.rs`: Handle PreToolUse/PostToolUse hooks → emit structured output events
- `crates/amux-cli/src/hooks.rs`: Updated imports (`PreToolUse`), added unknown-tool filtering for PreToolUse hooks
- `crates/amux-cli/src/main.rs`: Added `PreToolUse`/`PostToolUse` to `ClaudeHookEvent` enum
- `crates/amux-cli/src/plugin.rs`: Bumped `PLUGIN_VERSION` to 2 for new hook registration
- `crates/amux/src/server/handlers.rs`: Added PreToolUse/PostToolUse to hook_type match
- `crates/amux/src/server/connection.rs`: Boxed `Message` in `Incoming::Msg` to fix clippy large_enum_variant

### Decisions Made
- Kept `ClaudePermissionTool` as a type alias for backward compatibility
- PostToolUse uses custom deserializer because hook JSON has `tool_name`, `tool_input`, `tool_response` as separate top-level fields — both PreToolUse and PostToolUse need `tool_name` as discriminator
- All PostToolUse fields use `#[serde(default)]` for resilience to missing/partial tool_response data
- `success: false` → `ToolUseResult::Rejected`; `Failed` variant exists for future use

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — all 206 tests pass, zero clippy warnings

### Next Steps
- Wire up client-side UI to render PreToolUseEvent/PostToolUseEvent as tool activity cards
- Plugin manifest (upstream) needs PreToolUse/PostToolUse hook registration

---

## 2026-03-19: Rename AskUserQuestion `markdown` field to `preview`

### Summary
Renamed the `markdown` field on `AskUserQuestionOption` to `preview` to match the upstream Claude Code rename.

### Changes
- `crates/amux/src/claude/types.rs`: Renamed struct field and updated all tests
- `crates/amux/src/agents/claude.rs`: Updated field references and doc comments

### Verification
- `cargo check`, `cargo fmt`, `cargo clippy`, `cargo test` — all 194 tests pass

---

## 2026-03-15: Fix multi-select Other and refine AskUserQuestion keystroke timing

### Summary
Fixed multi-select custom "Other" text handling and refined keystroke timing throughout AskUserQuestion PTY generation. Key changes: removed the Space before typing custom Other text (typing auto-selects the checkbox in Claude Code's TUI), added delays between arrow navigation presses, unified ChatAboutThis to always use arrow-nav + Enter (instead of digit press for single-select), and implemented auto-advance awareness so digit presses and preview Enter in multi-question forms don't emit unnecessary Right arrow navigation.

### Changes
- `crates/amux/src/agents/claude.rs`:
  - `multi_select_keystrokes`: Removed Space before custom Other text (typing auto-selects); added delay between text and Up arrow; added delays between arrow nav presses
  - `preview_keystrokes`: Moved delay between each Down press instead of one delay before Enter
  - `chat_about_this_keystrokes`: Unified to always use arrow-nav + Enter (removed digit-press path for single-select)
  - `ask_question_keystrokes`: Added auto-advance tracking — digit press and preview Enter auto-advance `current_page`, eliminating redundant Right arrow presses; added delays around all Right/Left arrow navigation; single multi-select question now navigates to submit page
  - Updated 8 unit tests to match new keystroke sequences

### Decisions Made
- Typing custom text auto-selects the Other checkbox in Claude Code's TUI, so sending Space first was double-toggling (deselecting)
- ChatAboutThis always uses arrow-nav + Enter because it's not digit-selectable in the TUI — the old digit-press approach for single-select was incorrect
- Delays between every arrow press give the TUI time to process navigation, critical for mobile-initiated keystrokes

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 203 tests pass
- Mobile stress testing (18 tests):
  1. Two single-select questions — digits auto-advance, no double-advance
  2. Mixed single-select + multi-select — digit auto-advances, multi-select Right arrow submit
  3. Three single-select questions — chained auto-advances
  4. Other custom text Q1 + predefined Q2 — Other Enter auto-advances
  5. Preview Q1 + single-select Q2 — preview Enter auto-advances
  6. ChatAboutThis on Q2 with Q1 answered
  7. ChatAboutThis on Q1 (form exits immediately)
  8. Multi-select Q1 + ChatAboutThis on Q2
  9. Three questions, ChatAboutThis on middle one
  10. Preview Q1 + ChatAboutThis on Q2
  11. Other custom text Q1 + ChatAboutThis on Q2
  12. ChatAboutThis on Q1 with Q2 answered (navigate-back)
  13. Multi-select Q1 (predefined + custom Other) + single-select Q2
  14. Multi-select with custom Other Q1 + single-select Q2 (variant)
  15. Multi-select with custom Other Q1 + ChatAboutThis on Q2
  16. Multi-select with custom Other Q1 + ChatAboutThis on Q2 (repeat)
  17. Preview Q1 + multi-select with custom Other Q2
  18. Three questions with multi-select Other in the middle

---

## 2026-03-14: Redesign AskUserQuestionResponse to match Claude Code format

### Summary
Replaced the internal structured answer types (`SelectedOption`, `SingleSelectAnswer`, `MultiSelectAnswer`, `AskUserQuestionAnswer`) with a self-describing format that matches Claude Code's actual tool_result: the response echoes back the questions and provides answers as label strings in a `HashMap<String, String>`. Keystroke generation now derives question type (select/preview/multi-select) and option indices from the echoed questions rather than requiring pre-computed indices from the client.

### Changes
- `crates/amux/src/claude/types.rs`: Removed `SelectedOption`, `SingleSelectAnswer`, `MultiSelectAnswer`, `AskUserQuestionAnswer` enums; redesigned `AskUserQuestionResponse` with `questions: Vec<AskUserQuestionItem>`, `answers: HashMap<String, String>`, and `chat_about_this: Option<String>`; added `HashMap` import; rewrote tests
- `crates/amux/src/agents/claude.rs`: Replaced old keystroke functions with `select_keystrokes`, `preview_keystrokes`, `multi_select_keystrokes` (new impl), `chat_about_this_keystrokes`, plus `find_option_index` helper; new `ask_question_keystrokes` with two-phase processing (answers first, then ChatAboutThis with backward navigation support); rewrote all tests plus 5 new ChatAboutThis tests

### Decisions Made
- Questions echoed in the response eliminates the need for clients to compute 1-based indices — they just send labels
- Preview questions (options with `markdown`) use arrow-nav + Enter instead of digit press
- Multi-select answers use comma-separated labels ("Auth, Cache") matching Claude Code's natural format
- ChatAboutThis modeled as `chat_about_this: Option<String>` on the response — out-of-band from answers, carries the question text to navigate to; keystroke generation processes all answers first then navigates (possibly backwards) to the ChatAboutThis page

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 203 tests pass

---

## 2026-03-14: AskUserQuestionResponse → PTY keystrokes

### Summary
Implemented PTY keystroke generation for `AskUserQuestionResponse`. Response types are now self-describing (each variant carries its UI index), and a pure `ask_question_keystrokes` function translates semantic responses into `PtyAction` sequences. All three `ClaudeStructuredInput` variants now go through a unified `PtyAction`-based pipeline.

### Changes
- `crates/amux/src/claude/types.rs`: Changed `SelectedOption` from tuple variants to struct variants with `index` field; `Custom` now also carries `index`; `ChatAboutThis` now carries `index` and `multi_select` fields; updated all existing tests
- `crates/amux/src/agents/claude.rs`: Added `PtyAction` enum and arrow-key constants; implemented `single_select_keystrokes`, `multi_select_keystrokes`, `chat_about_this_keystrokes`, `ask_question_keystrokes` (page navigation with ChatAboutThis support), `permission_response_keystrokes`, `submit_message_keystrokes`, `execute_pty_actions`; refactored `send_input` to produce `Vec<PtyAction>` for all input variants; added 10 unit tests

### Decisions Made
- Added `multi_select: bool` to `ChatAboutThis` (not in original plan) so keystroke generation can distinguish digit-press (single-select) vs navigate+space (multi-select) without needing the question metadata
- Wire format for `SelectedOption` and `ChatAboutThis` changed (struct variants vs tuple/unit) — acceptable since these types are new and not yet used in production

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 190 tests pass, zero warnings

### Next Steps
- Client-side construction of `AskUserQuestionResponse` from TUI interaction
- E2E testing with live Claude Code AskUserQuestion prompts

---

## 2026-03-14: AskUserQuestion type completeness

### Summary
Added the missing `markdown` field to `AskUserQuestionOption` and introduced response types (`SelectedOption`, `SingleSelectAnswer`, `MultiSelectAnswer`, `AskUserQuestionAnswer`, `AskUserQuestionResponse`) for conveying user selections back from clients. Wired `AskUserQuestionResponse` into `ClaudeStructuredInput` with a no-op handler in `ClaudeSession::send_input()`.

### Changes
- `crates/amux/src/claude/types.rs`: Added `markdown: Option<String>` to `AskUserQuestionOption` (with `#[serde(default)]`); added `SelectedOption`, `SingleSelectAnswer`, `MultiSelectAnswer`, `AskUserQuestionAnswer`, `AskUserQuestionResponse` types; added `AskUserQuestionResponse` variant to `ClaudeStructuredInput`; added 7 new tests
- `crates/amux/src/agents/claude.rs`: Added match arm for `AskUserQuestionResponse` (logs debug, no PTY action yet)

### Decisions Made
- `markdown` uses `#[serde(default)]` for backward compatibility with payloads that omit it
- `ChatAboutThis` lives at the `AskUserQuestionAnswer` level (not `SelectedOption`) so the type system prevents mixing it with actual selections
- Response is positionally matched to the `questions` array; truncated after a `ChatAboutThis` entry
- PTY keystroke translation for `AskUserQuestionResponse` deferred — type definitions only for now

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 179 tests pass
- New tests cover: option with/without markdown, single-select predefined/custom, multi-select, ChatAboutThis truncation, round-trip through ClaudeStructuredInput

### Next Steps
- Wire `AskUserQuestionResponse` to actual PTY keystroke sending in `ClaudeSession`
- Implement client-side UI for rendering AskUserQuestion and collecting answers

---

## 2026-03-10: Windows support + platform abstraction cleanup

### Summary
Added Windows support across the codebase, then refactored to consolidate scattered `#[cfg]` blocks behind platform abstractions. The initial implementation replaced all Unix-specific APIs with cross-platform equivalents (crossterm for terminal control, named pipes for local IPC, `LocalTransport` type alias for transport abstraction) and added Windows to CI and release workflows. A follow-up pass consolidated ~33 `#[cfg]` blocks down to ~20 by introducing a `LocalListener` abstraction, fixing a type mismatch bug, cleaning up dependencies, and simplifying several platform-conditional helpers.

### Changes

**Transport layer — platform-abstracted local IPC:**
- `crates/amux/src/transport/local.rs` (new): Type aliases (`LocalTransport`, `LocalMessageReader`, `LocalMessageWriter`) that resolve to `UnixTransport` on Unix and `NamedPipeClientTransport` on Windows
- `crates/amux/src/transport/named_pipe.rs` (new): `NamedPipeTransport<S>` generic over `NamedPipeClient`/`NamedPipeServer`, with `Transport` and `TransportSplit` implementations using length-prefixed framing
- `crates/amux/src/transport/mod.rs`: Added `local` and `named_pipe` modules; gated `unix` module on `#[cfg(unix)]`; replaced `pub use unix::UnixTransport` with `pub use local::{LocalTransport, ...}`
- `crates/amux/src/connection.rs`: Changed `Connection` from hardcoded `UnixMessageReader`/`UnixMessageWriter` to `LocalMessageReader`/`LocalMessageWriter`

**Server — `LocalListener` abstraction:**
- `crates/amux/src/server/mod.rs`: Created `LocalListener` struct encapsulating Unix socket (wraps `UnixListener`) and named pipe (stores pipe name string) behind `bind()`/`accept()` methods. Replaced 5 cfg-gated functions (`bind_local_listener` x2, `accept_local_transport` x2, `accept_named_pipe`) and the platform-varying `local_listener` variable type. Fixed a type mismatch bug where the Windows `accept_local_transport` constructed `NamedPipeTransport<NamedPipeClient>` from a `NamedPipeServer`. Added `#[cfg(unix)]` gate on socket file removal at server exit.
- `crates/amux/src/server/accept.rs`: Renamed `unix_accept` → `local_accept`, changed to accept `impl TransportSplit` instead of `UnixStream`, removed `UnixTransport`/`UnixStream` imports

**Client — cross-platform terminal and connection:**
- `crates/amux-cli/src/client.rs`: Replaced `libc`-based `get_terminal_size()` (ioctl/TIOCGWINSZ) with `crossterm::terminal::size()`; replaced `libc`-based `RawModeGuard` (tcgetattr/cfmakeraw/tcsetattr) with `crossterm::terminal::enable_raw_mode()`/`disable_raw_mode()`; removed `std::os::unix::io::AsRawFd` import
- `crates/amux-cli/src/hooks.rs`: Removed `socket_path.exists()` guard (not reliable for named pipes); always attempt delivery, log at debug level on failure
- `crates/amux/src/connect.rs`: Replaced `UnixStream::connect` with `connect_local_transport()` (two cfg-gated versions: Unix uses `UnixStream`, Windows uses `ClientOptions` with retry loop for ERROR_PIPE_BUSY). Rewrote `connect_daemon` to use connect-then-retry pattern instead of socket file existence checks. Added `DETACHED_PROCESS` creation flag (0x00000008) for Windows daemon spawn.

**Config — Windows path conventions:**
- `crates/amux/src/config.rs`: Added `#[cfg(windows)]` `home_dir()` using `%USERPROFILE%`; added Windows `default_socket_dir()` (`temp_dir()/amux`) and `default_socket_path()` (`\\.\pipe\amux-{USERNAME}`); restructured `xdg_dir()` Windows fallback to use `default_suffix` hint (`.config` → `%APPDATA%`, others → `%LOCALAPPDATA%`) instead of comparing `env_var` strings

**Update — Windows platform support:**
- `crates/amux-cli/src/update.rs`: Added `windows-x86_64` and `windows-arm64` platform keys; gated `PermissionsExt` import and binary permission setting on `#[cfg(unix)]`

**E2E runner — cross-platform test infrastructure:**
- `crates/e2e-runner/src/terminal.rs`: Split `spawn()` into cfg-gated Unix (sh -c with stty/exec) and Windows (direct ConPTY) paths; changed `args` parameter from `&[&str]` to `&[String]`; gated `shell_quote` on `#[cfg(unix)]`
- `crates/e2e-runner/src/executor.rs`: Replaced string-based command transformation with `shell_words::split` producing `ResolvedCommand { program, args }`; consolidated 4 cfg-gated socket path functions into 1; removed `socket_dir` field from `ExecutorConfig`; gated socket file cleanup on `#[cfg(unix)]`
- `crates/e2e-runner/src/main.rs`: Replaced 2 cfg-gated `debug_binary_path` functions with single function using `std::env::consts::EXE_SUFFIX`

**CI and release:**
- `.github/workflows/ci.yml`: Added `windows-latest` to check, clippy, and test matrices
- `.github/workflows/release.yml`: Added `windows-latest`/`x86_64-pc-windows-msvc` build target; added `--target` flag to build; updated artifact paths for `.exe`; added Windows binary to release assets and checksums

**Dependencies:**
- `Cargo.toml` (workspace): Added `crossterm = "0.28"` and `shell-words = "1"`
- `crates/amux/Cargo.toml`: Moved `libc` to `[target.'cfg(unix)'.dependencies]`
- `crates/amux-cli/Cargo.toml`: Replaced `libc` with `crossterm`
- `crates/e2e-runner/Cargo.toml`: Added `shell-words`

### Decisions Made
- **`LocalTransport` type alias pattern** (in `transport/local.rs`): Rather than a trait object or enum, the local transport is a compile-time type alias. This is zero-cost and lets all existing generic code (`impl TransportSplit`) work unchanged.
- **`LocalListener::accept()` returns `impl TransportSplit + use<>`**: The `use<>` precise capture syntax (Rust 2024) tells the compiler the returned transport doesn't borrow `&self`, allowing it to be moved into `tokio::spawn`. A boxed trait object would add unnecessary overhead.
- **crossterm over raw libc**: The terminal size query and raw mode handling previously used `libc::ioctl`/`tcgetattr`/`cfmakeraw` directly. `crossterm` provides the same functionality cross-platform without unsafe code.
- **`shell_words::split` for command parsing**: The E2E executor previously did naive string splitting, which broke on paths with spaces. `shell_words` handles proper shell quoting.
- **Named pipe retry loop on Windows**: `ClientOptions::new().open()` can fail with ERROR_PIPE_BUSY (231) when the server pipe hasn't recycled. The client retries up to 20 times with 50ms delay.
- **Connect-then-retry pattern**: `connect_daemon` previously checked `socket_path.exists()` before connecting, which doesn't work for named pipes (they're kernel objects, not filesystem entries). Now it just attempts connection and retries on IO errors.
- **Remaining cfg blocks (~20) kept**: These represent genuine platform differences (Unix socket paths vs named pipe names, stty vs ConPTY, UID-based socket dirs vs temp dir) rather than abstraction leaks.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass
- Windows-specific verification requires CI (the Windows runner in the updated ci.yml)

---

## 2026-03-09: Graceful server shutdown and suspend

### Summary
Moved shutdown/suspend orchestration from connection handler tasks into `Server::run()`. Previously, both `Shutdown` and `Suspend` command handlers called `std::process::exit(0)` from within spawned connection tasks, which skipped destructors, didn't clean up resources, and caused a race condition during suspend where the accept loop kept running and clients could reconnect to the old server before it exited. Now handlers send a typed `ShutdownRequest` to the main loop via an mpsc channel, and the main loop handles everything: stop/suspend agents, send the client response, drop listeners, remove socket, grace period, then return normally.

### Changes
- `crates/amux/src/server/mod.rs`: Added `ShutdownRequest` enum (Shutdown/Suspend variants with reply channel), `shutdown_tx` field to `ServerState`, `shutdown_rx` field to `Server`, new select arm in `Server::run()` loop, and post-loop cleanup (drop listeners, remove socket, grace sleep)
- `crates/amux/src/server/handlers.rs`: Replaced Shutdown and Suspend handler bodies with thin forwarding stubs that send `ShutdownRequest` to the main loop; removed unused imports (`shutdown_server`, `suspend_server`, `ShutdownReason`); moved `Duration` import into test module

### Decisions Made
- Handlers are thin relays: they only send the request and return `Ok(())`. All orchestration logic lives in `Server::run()` so the main loop controls listener teardown ordering.
- The `reply` field in `ShutdownRequest` is the connection's `mpsc::Sender<Message>`, allowing the main loop to send the response directly to the requesting client after completing the work.
- Reply is deferred until after listener drop + socket removal: the main loop builds the response message inside the select arm, breaks, tears down listeners, removes the socket file, and only then sends the reply. This eliminates the race where `amux update` could receive `SuspendResult`, see the old socket still exists, and reconnect to the dying server instead of spawning a new one.
- On suspend save failure, the main loop sends an error response and `continue`s (doesn't break), keeping the server alive.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all pass (171 unit tests)
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass

---

## 2026-03-08: `amux update` command

### Summary
Added `amux update` command for self-updating the binary from GitHub releases. Fetches a version manifest from `{cloud_url}/manifest.json`, compares versions using semver, downloads the platform-specific binary with SHA256 verification, and performs an atomic rename to replace the running binary. If a server is running, it suspends agents before replacing the binary and resumes them on the new server.

### Changes
- `Cargo.toml`: Added `semver` and `sha2` to workspace dependencies
- `crates/amux-cli/Cargo.toml`: Added `reqwest`, `serde`, `semver`, `sha2` dependencies
- `crates/amux-cli/src/update.rs`: New module with manifest fetching, download/verify, suspend/resume, and binary replacement
- `crates/amux-cli/src/main.rs`: Added `Update` command variant and dispatch

### Decisions Made
- Atomic rename: temp file in same directory as binary ensures same-filesystem rename
- Connection drop after suspend is treated as success (server exits after sending SuspendResult)
- `connect(config, Daemon)` for resume: `current_exe()` returns same path, now pointing to new binary
- Platform detection via compile-time `#[cfg]` with `compile_error!` for unsupported platforms

### Verification
- `cargo check` passes
- `cargo fmt` clean
- `cargo clippy --workspace --all-targets -- -D warnings` passes
- `cargo test` passes

### Next Steps
- Serve `manifest.json` from the cloud server
- Build release binaries in CI and publish to GitHub releases

---

## 2026-03-08: SuspendedServerState struct + YAML persistence

### Summary
Replaced the raw `Vec<SuspendedAgent>` tuple return from `suspend_server()` with a proper `SuspendedServerState` struct, and switched persistence from MessagePack (`suspended.msgpack`) to YAML (`suspended.yaml`) for human-readability and consistency with `state.yaml`.

### Changes
- `crates/amux/src/agents/mod.rs`: Added `SuspendedServerState` struct wrapping `Vec<SuspendedAgent>`
- `crates/amux/src/server/routing.rs`: `suspend_server` now returns `(SuspendedServerState, Vec<String>)`
- `crates/amux/src/state.rs`: `save_suspended` accepts `&SuspendedServerState`, `load_and_remove_suspended` returns `SuspendedServerState`, file changed to `suspended.yaml`, replaced `rmp_serde` with `serde_yaml`, renamed `StateError::MsgPack` to `StateError::Suspended`
- `crates/amux/src/server/handlers.rs`: Updated Suspend/Resume handlers to use `.agents` field
- `crates/amux/src/lib.rs`: Exported `SuspendedServerState`

### Decisions Made
- Renamed `StateError::MsgPack` to `StateError::Suspended` since the variant is now format-agnostic
- Kept `rmp_serde` dependency since it's still used for transport serialization

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 171 tests pass, zero warnings

---

## 2026-03-08: Suspend/resume for agent sessions

### Summary
Added suspend/resume capability to support future `amux update` flow. Two prerequisite refactors removed `Arc` from agent storage (enabling ownership transfer for suspend) and moved lifecycle monitoring from sessions to server (decoupling sessions from server bookkeeping). The feature includes `SuspendedAgent` serialization, `--resume` support for Claude agents, `Suspend`/`Resume` command variants, server orchestration, and MessagePack state persistence.

### Changes
- `agents/mod.rs` — Added `SuspendedAgent` enum, `suspend(self)` method, `terminal_size()` accessor, `into_session()` for resume. Added `args` parameter to `spawn_pty_agent`. Removed `event_tx`/`user_id` from `spawn_pty_agent` (returns `JoinHandle<()>` instead).
- `agents/claude.rs` — Added `session_id` field (set via SessionStart hook; when pre-set before `start()`, passes `--resume` to claude). Removed `event_tx`/`user_id` from struct and `new()`. `start()` returns `Result<JoinHandle<()>>`.
- `agents/testagent.rs` — Same struct/new() cleanup. `start()` returns `Result<JoinHandle<()>>`.
- `server/mod.rs` — Changed `agents: HashMap<Uuid, Arc<AgentSession>>` to `HashMap<Uuid, AgentSession>`.
- `server/routing.rs` — Removed Arc wrapping in `create_agent`, added server-side exit monitoring. Added `suspend_server()` and `resume_agents()`. Changed `shutdown_server` to use `mem::take`.
- `server/handlers.rs` — Updated patterns to work without Arc. Added `Suspend` and `Resume` command handlers.
- `message.rs` — Added `Suspend`, `SuspendResult`, `Resume`, `ResumeResult` command variants with type labels.
- `state.rs` — Added `save_suspended()`, `load_and_remove_suspended()` for MessagePack persistence.
- `lib.rs` — Exported `SuspendedAgent`.

### Decisions Made
- `SuspendedAgent` uses MessagePack (not YAML) for consistency with all other transports
- Suspend file lives alongside state.yaml as `suspended.msgpack`
- Claude resume uses `--resume <session_id>` flag
- Test agents restart fresh (no resume semantics)
- `session_id` is captured from SessionStart hook — suspend errors if hook never arrived

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all pass
- 171 unit tests (168 existing + 3 new: serde roundtrip, nonexistent file, into_session)
- 10 E2E tests pass unchanged

### Next Steps
- Add CLI `amux update` command that sends Suspend, waits, updates binary, starts new server, sends Resume
- Add CLI `amux suspend` / `amux resume` commands for manual use

---

## 2026-03-08: Refactor LocalAgentSession into AgentSession enum + PtyHandle

### Summary
Decoupled agent lifecycle management from PTY management by replacing the monolithic `LocalAgentSession` with an `AgentSession` enum dispatching to concrete session types (`ClaudeSession`, `TestAgentSession`). PTY operations are now encapsulated in `PtyHandle`. Hook handling and structured input translation moved from server handlers into session implementations, preparing for future non-PTY backends.

### Changes
- Created `crates/amux/src/agents/mod.rs` — `AgentSession` enum, `PtyHandle`, `StopPolicy`, `SessionEvent`, `spawn_pty_agent()` helper
- Created `crates/amux/src/agents/claude.rs` — `ClaudeSession` with two-phase init, hook handling, structured input translation, `permission_response_keystroke()`
- Created `crates/amux/src/agents/testagent.rs` — `TestAgentSession` (#[cfg(any(debug_assertions, test))])
- Updated `lib.rs` — replaced `mod session` with `mod agents`
- Updated `server/mod.rs` — `agents: HashMap<Uuid, Arc<AgentSession>>`
- Updated `server/routing.rs` — `create_agent()` dispatches on `AgentType`, `handle_subscribe()` uses `get_pty_handle()`, `shutdown_server()` uses `StopPolicy::Interrupt`, removed `permission_response_keystroke()`
- Updated `server/handlers.rs` — simplified `HandleHook` to `session.handle_hook()`, `StructuredInput` to `session.send_input()`, `RawInput` through `get_pty_handle()`
- Updated `server/connection.rs`, `server/accept.rs` — import path changes
- Deleted `crates/amux/src/session.rs`

### Decisions Made
- Two-phase init (new + start): allows metadata storage before process spawn, cleaner error handling
- TestAgent hook/input as no-ops: test agents don't need Claude-specific behavior; handler tests simplified to verify response only
- `spawn_pty_agent()` shared helper: both session types reuse the same PTY creation + task spawning logic

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 168 tests pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 10/10 E2E tests pass

### Next Steps
- Add Codex backend as a new `AgentSession` variant (non-PTY, stdin/stdout)

---

## 2026-03-07: Split amux into workspace crates with public API

### Summary
Split the single amux crate into a proper Cargo workspace with separate library (`amux`) and binary (`amux-cli`) crates. The `amux` crate now exposes a clean public API (`connect()`, `Connection`, `ConnectPolicy`) that third parties can use to build applications (mobile apps, custom UIs, etc.) on top of amux.

### Changes
- Root `Cargo.toml` → workspace-only with `[workspace.dependencies]`
- `crates/amux/` — library crate with all protocol, server, and transport code
- `crates/amux-cli/` — binary crate producing the `amux` binary
- `crates/amux/src/connect.rs` — **New**: `connect()` function with `ConnectPolicy` enum (Daemon, Embedded, ExistingOnly)
- `crates/amux/src/connection.rs` — **New**: `Connection` struct wrapping split transport with `send(&self)` / `recv(&self)` via `tokio::Mutex`
- `crates/amux/src/lib.rs` — **New**: public module layout and API re-exports
- `crates/amux-cli/src/main.rs` — Updated imports, removed `ensure_server_running()`, added `--config-from-stdin` flag
- `crates/amux-cli/src/client.rs` — Uses `amux::connect()` + `Connection` instead of raw transport
- `crates/amux-cli/src/hooks.rs` — Moved from `claude/hooks.rs`, updated imports
- `crates/amux-cli/src/init.rs` — Moved, replaced `thiserror` with manual `Error` impl (CLI-only)
- `crates/amux-cli/src/plugin.rs` — Moved from `claude/plugin.rs`, updated imports
- Moved `test-agent/` and `e2e-runner/` to `crates/`
- Made key types `pub` in message.rs, handshake.rs, route.rs for cross-crate access
- Updated `CLAUDE.md` structure diagram

### Decisions Made
- **`Connection` uses `tokio::Mutex` internally**: both `send` and `recv` take `&self`, so consumers can use them in `select!` or across tasks without splitting
- **`Daemon` policy passes config via stdin**: spawns `amux serve --config-from-stdin` and writes serialized Config to stdin, avoiding temp files
- **All core modules are `pub` for now**: CLI needs broad access; public/private boundary will be refined later
- **`anyhow` stays CLI-only**: core uses `thiserror` via `AmuxError`
- **`tracing-subscriber`/`tracing-appender` stay CLI-only**: core uses `tracing` for instrumentation only
- **`link_name` exposed on `Connection`**: needed by client.rs for routing (Route::from_link)

### Verification
- `cargo check` — passes
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings
- `cargo test --workspace` — 175 tests pass (169 lib + 6 e2e-runner)
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass

### Next Steps
- Refine public API surface (mark internal modules `#[doc(hidden)]` or restructure exports)
- Add examples showing `ConnectPolicy::Embedded` usage for third-party apps
- Consider publishing `amux` crate to crates.io

---

## 2026-03-03: Handshake extraction + Reauth split (protocol reset to v1)

### Summary
Performed a big-bang protocol cutover that removes `Connect/ConnectResult` from the session `Message` enum. Handshake is now a standalone bootstrap protocol (`src/handshake.rs`) exchanged as raw MessagePack frames before entering the normal message loop. In-band cloud token refresh now uses explicit `DirectMessage::Reauth` / `ReauthResult`. Protocol version was reset to `1`, and handshake `version` is now required.

### Changes
- `src/handshake.rs` — **New** standalone handshake types: `Connect`, `ConnectResult`, `PROTOCOL_VERSION = 1`, plus encode/decode helpers and tests
- `src/message.rs` — Removed handshake variants from `DirectMessage`; added `Reauth { token: String }` and `ReauthResult { error: Option<ProtocolError> }`; updated labels/tests
- `src/transport/mod.rs`, `src/transport/tcp.rs`, `src/transport/unix.rs`, `src/transport/websocket.rs` — Added raw frame methods to `Transport` (`read_frame`/`write_frame`) so handshake can run outside `Message`
- `src/server/accept.rs` — Reworked accept/connect handshake paths to use standalone `Connect`/`ConnectResult` frame decode/encode
- `src/cloud.rs`, `src/server/connection.rs`, `src/server/handlers.rs` — Migrated refresh path from in-band `Connect` to `Reauth` and updated interception/validation logic
- `src/claude/hooks.rs` — Updated local hook client handshake to standalone `Connect`/`ConnectResult`
- `src/server/cloud.rs` — Version mismatch reporting now references handshake protocol version
- `ARCHITECTURE.md`, `CLOUD_ARCHITECTURE.md` — Updated docs to reflect handshake/session split and `Reauth` flow

### Decisions Made
- Big-bang cutover only: no backward-compat shims for old in-band `Connect`
- Handshake remains MessagePack map encoding (`to_vec_named`), separate from session `Message` decoding
- `Reauth.token` is required (`String`) to keep refresh semantics strict and avoid optional-credential ambiguity

### Verification
- `cargo check` — pass
- `cargo clippy --workspace --all-targets -- -D warnings` — pass
- `cargo test` — pass (169 tests)
- E2E runner was not used for final verification in sandboxed CI context due networking constraints

### Next Steps
- Run full E2E on a non-sandboxed environment to validate end-to-end attach/connect flows against the new handshake boundary

---

## 2026-03-03: Handle Stop hook and add AgentStopped structured output

### Summary
Added handling for Claude Code's `Stop` hook event, which fires when the agent finishes responding. Attached clients now receive an `AgentStopped` structured output entry in the log buffer, signaling the agent is idle and waiting for input. Also added forward-compatibility for unknown hook events via `#[serde(other)]` on `ClaudeHook`.

### Changes
- `src/claude/types.rs` — Added `ClaudeStop` struct, `Stop(ClaudeStop)` variant to `ClaudeHook`, `AgentStopped` variant to `ClaudeStructuredOutput`, `Unknown` variant with `#[serde(other)]` for forward-compat, updated `Display` impl
- `src/main.rs` — Added `ClaudeHookEvent::Stop` to `is_handled_hook_event` so it's no longer fast-path dropped
- `src/claude/hooks.rs` — Filter unknown hook variants client-side before sending to server (same pattern as unknown permission tools)
- `src/server/handlers.rs` — Added `ClaudeHook::Stop` arm in `HandleHook` handler that writes `AgentStopped` to the session log buffer; unknown hooks ack immediately with a warning; added two tests

### Decisions Made
- `AgentStopped` is a unit variant (no fields): the `last_assistant_message` from the Stop hook JSON is not stored since transcript tailer already provides assistant messages
- Unknown hooks filtered client-side in `hooks.rs` (fail fast), with server-side safety net that warns and acks to prevent hook client hangs
- `#[serde(other)]` on `ClaudeHook` mirrors the existing `ClaudePermissionTool` pattern — works because both use internally-tagged serde format

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 167 tests pass

### Next Steps
- Client UI can now react to `AgentStopped` entries (e.g., show idle indicator)

---

## 2026-03-03: Per-user Unix socket and log paths

### Summary
Changed default socket and log paths from global `/tmp/amux.sock` and `/tmp/amux.log` to per-user locations, preventing conflicts on multi-user machines.

### Changes
- `src/config.rs`: Removed `DEFAULT_SOCKET_PATH`, added `default_socket_dir()` with platform-aware logic (macOS `$TMPDIR`, Linux `$XDG_RUNTIME_DIR`, fallback `/tmp/amux-<uid>/`). Added `default_log_path()` using XDG state dir.
- `src/server/mod.rs`: Create socket parent directory with `0o700` permissions before bind.
- `src/main.rs`: Default log path moved to `~/.local/state/amux/amux.log` (co-located with `state.yaml`). Parent directory created on startup.
- `ARCHITECTURE.md`, `CLOUD_ARCHITECTURE.md`: Updated socket path references.

### Decisions Made
- macOS uses `$TMPDIR` (already per-user), Linux uses `$XDG_RUNTIME_DIR`, both fall back to `/tmp/amux-<uid>/`
- Log moved to XDG state dir rather than runtime dir — logs should persist across reboots
- `cfg!(target_os = "macos")` runtime check (not `#[cfg]`) keeps both paths as valid Rust for testability

### Verification
- `cargo check && cargo fmt && cargo clippy -- -D warnings && cargo test` — all 165 tests pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass

---

## 2026-03-01: Env-var agent ID + StructuredLogSource composite type

### Summary
Fixed hook system breaking when Claude Code's `session_id` changes (via `/clear`, `/compact`, `/fork`). Previously the server looked up agents by `session_id` extracted from the hook JSON payload, but after compaction Claude has a new `session_id` that doesn't match any agent. Now agents are identified by `AMUX_AGENT_ID` env var (set when spawning Claude), and transcript tailing supports clean re-linking with buffer clearing.

### Changes
- `src/buffer.rs`: Added `clear()` method to `BroadcastBuffer` — resets storage but keeps subscribers connected. Added test.
- `src/claude/structured_log_source.rs`: **New** — composite type owning buffer + transcript tailer with `link_transcript()` that supports re-linking (stops old tailer, clears buffer, starts new tailer).
- `src/claude/mod.rs`: Registered new module.
- `src/session.rs`: Replaced `log_buffer` + `transcript_tailer` fields with `StructuredLogSource`. Removed `MAX_LOG_ENTRIES` (moved to `StructuredLogSource`). Set `AMUX_AGENT_ID` env var when spawning Claude. Dropped `--session-id` CLI flag.
- `src/message.rs`: Added `agent_id: Uuid` field to `Command::HandleHook`. Updated `AgentType::Claude` doc comment.
- `src/claude/hooks.rs`: Read `AMUX_AGENT_ID` from environment, pass to `Command::HandleHook`.
- `src/server/handlers.rs`: `HandleHook` handler uses `agent_id` from command instead of `session_id` from hook payload. Updated 4 hook tests.

### Decisions Made
- `AMUX_AGENT_ID` env var is the stable agent identifier, decoupled from Claude's session_id which changes on `/clear`/`/compact`/`/fork`.
- `StructuredLogSource::link_transcript()` clears the buffer on re-link so subscribers don't see stale entries from the old session.
- `BroadcastBuffer::clear()` is separate from `close()` — clear resets data but keeps the buffer open and subscribers connected.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test`: 165 tests pass
- E2E tests: 10/10 pass

### Next Steps
- Manual test with Claude Code: verify `/compact` triggers SessionStart hook with new transcript path and the tailer re-links correctly.

---

## 2026-02-28: Improve OAuth error diagnostics for expired refresh tokens

### Summary
Fixed opaque OAuth error messages and incorrect retry behavior when a refresh token expires. Previously, an expired token produced `"Server returned error response"` and retried forever with exponential backoff. Now it detects `InvalidGrant` specifically, logs a clear message, and stops retrying immediately.

### Changes
- `src/oauth.rs`: Added `RefreshTokenExpired` variant to `OAuthError`. Pattern-match on `RequestTokenError::ServerResponse` to detect `InvalidGrant` and preserve error descriptions for other server errors.
- `src/server/cloud.rs`: Treat `RefreshTokenExpired` as non-retriable (same as `NotAuthenticated`), stopping the reconnect loop immediately.

### Decisions Made
- Map `InvalidGrant` to a dedicated error variant rather than a generic string, so upstream code can match on it for control flow (non-retriable vs retriable)
- Keep other `ServerResponse` errors as `OAuthError::Request` but include the actual error code and description instead of the opaque oauth2 crate `.to_string()`

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 164 tests pass
- Manual test confirmed clear log output: `ERROR cloud non-retriable error, stopping error=Authentication failed — run 'amux init' to re-authenticate`

---

## 2026-02-22: Consolidated branch cleanup and hardening

### Summary
Collapsed a long cleanup branch into a cohesive reliability and maintainability pass across transport, connection lifecycle, cloud reconnect/auth flows, and Claude integration structure. The work removes duplicated logic, tightens safety boundaries, improves logging context, and adds focused test coverage for critical connection and refresh paths.

### Changes
- Consolidated Claude integration under `src/claude/` and removed wildcard type re-exports in favor of explicit imports.
- Hardened connection paths with handshake/connect timeouts, tighter Unix socket permissions, and guardrails for connection/agent fan-out limits.
- Refactored shared lifecycle logic (`run_connection`, shared handshake/subscription helpers, token refresh state machine) to reduce repetition and drift.
- Improved operational visibility by removing duplicate error logs, adding structured span fields (`user_id`, cloud URL, hook type), and improving decode/mismatch handling.
- Expanded/modernized tests around connection loops, reader behavior, token refresh transitions, and removed low-signal roundtrip-only tests.

### Decisions Made
- Prefer explicit module boundaries/imports over compatibility re-export layers to reduce hidden coupling.
- Centralize repeated connection/auth/subscription flows in shared helpers so behavior changes stay consistent.
- Keep forward-compatible runtime behavior (for example, handling unknown/undecodable payloads safely) while improving diagnostics.

### Verification
- `cargo test` — 164 passed; 0 failed

### Next Steps
- None; this entry intentionally summarizes the full branch cleanup as a single change.

---

## 2026-02-22: Switch WebSocket transport to MessagePack binary frames

### Summary
Replaced JSON text frames with MessagePack binary frames in the WebSocket transport, unifying serialization across all transports (Unix, TCP, WebSocket). Binary frames handle byte blobs cleanly without base64 encoding needed for opaque payloads.

### Changes
- `src/transport/websocket.rs` — Replaced all 4 `serde_json` call sites with `Message::encode()`/`decode()` + `WsMessage::Binary` (matching the TCP/Unix pattern). Updated doc comment.
- `src/transport/mod.rs` — Updated module doc comment.
- `ARCHITECTURE.md` — Updated 5 references from JSON/WebSocket to MessagePack everywhere.
- `CLOUD_ARCHITECTURE.md` — Updated 2 references (connection flow description, transport table).
- `CLAUDE.md` — Updated serialization reference in Rust Idioms section.

### Decisions Made
- Keep `serde_json` in `Cargo.toml`: still needed for Claude Code hook JSON (`hooks.rs`) and transcript JSONL (`transcript.rs`) — those are external formats we don't control.
- Use `AmuxError::SerializationEncode`/`SerializationDecode` error variants (same as TCP/Unix) instead of the previous `AmuxError::Config` wrapper.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 125/125 tests pass
- E2E tests — 9/10 pass (1 known flaky `attach` test, pre-existing)

---

## 2026-02-22: Documentation update for protocol v3

### Summary
Updated ARCHITECTURE.md, CLOUD_ARCHITECTURE.md, and CLAUDE.md to match the current protocol v3 codebase. All three docs had significant drift from the v2→v3 migration.

### Changes
- `ARCHITECTURE.md` — Updated Message enum to three variants (Routable/Direct/Command) with opaque payload + request_id. Updated RoutableMessage (removed success/Error/AgentEnded, added SubscriptionClosed/UnknownMessage). Replaced DirectMessage (stripped to Connect/Announce/Withdraw only). Added Command enum docs. Updated ProtocolError (removed NoRouteFound, added InvalidLinkName/VersionMismatch). Updated ConnectionContext (user_state, user_id, is_local, next_request_id). Updated ServerState to multi-user model. Rewrote routing section (silent drops, WithdrawHost as routing truth). Updated Transport traits (MessageReader/MessageWriter/TransportSplit). Updated ClaudePermissionTool (full list of tools, removed PermissionTool). Updated CreateAgentRequest (terminal_size). Updated agent lifecycle (SubscriptionClosed). Replaced dashboard references with "rich clients".
- `CLOUD_ARCHITECTURE.md` — Updated Connect messages (added version field, DirectMessage prefix). Updated ConnectResult patterns (error: None). Updated session propagation (agents now propagated to peers). Removed "agent propagation" from deferred complexity. Replaced dashboard reference.
- `CLAUDE.md` — Updated current state section (milestones complete, v3 protocol). Updated core types (Route, AgentRegistry, Host). Fixed serialization reference (bincode→MessagePack). Replaced flat file structure with module structure. Updated common tasks (handle_routable, handle_command, TransportSplit). Removed stale suggested implementation order.

### Verification
- Grep-checked all three docs for 12 categories of outdated v2 references (success:bool, AgentEnded, NoRouteFound, PermissionTool, DirectMessage::Error, etc.) — all clean

---

## 2026-02-22: Opaque payload + request_id (protocol v3, phase 4)

### Summary
Phase 4 of protocol v3 upgrade. Changed `Message::Routable` from carrying a typed `message: RoutableMessage` field to an opaque `payload: Vec<u8>` + `request_id: u64`. Intermediate servers (cloud relays) can now forward routable messages without deserializing the payload — they just copy bytes. Only the final destination decodes the payload via a two-step process: decode `Message` (outer envelope), then decode `RoutableMessage` from the payload bytes. Added `RoutableMessage::UnknownMessage` variant for forward compatibility (returned when payload decode fails). Added `RoutableMessage::encode()`/`decode()` methods mirroring `Message::encode()`/`decode()`. Added `Message::routable()` convenience constructor to reduce boilerplate at ~20 construction sites. Added `next_request_id: Arc<AtomicU64>` to `ConnectionContext` for unique request IDs on stream messages. Client-side uses a local `AtomicU64` counter threaded through `new_agent`/`attach`/`subscribe_and_stream`/`run_attached`.

### Changes
- `src/message.rs` — Changed `Message::Routable` fields from `{ src, dst, message: RoutableMessage }` to `{ src, dst, request_id: u64, payload: Vec<u8> }`. Added `RoutableMessage::UnknownMessage` variant. Added `encode()`/`decode()` on `RoutableMessage`. Added `Message::routable()` convenience constructor. Updated 2 existing tests, added 3 new tests (encode/decode roundtrip, opaque two-step roundtrip, UnknownMessage roundtrip).
- `src/server/connection.rs` — Added `next_request_id: Arc<AtomicU64>` to `ConnectionContext`. Changed `msg_type_label` Routable arm to return `"Routable"` (can't inspect opaque payload). Changed logging filter to trace-level for all Routable messages. Updated `handle_routable` signature to take `request_id: u64, payload: Vec<u8>`. Forwarding path passes payload verbatim (the key optimization). Local delivery uses two-step decode with `RoutableMessage::decode()`, returning `UnknownMessage` on decode failure. All ~15 response construction sites use `Message::routable()`. Stream tasks clone `Arc<AtomicU64>` and generate per-message request_ids. Updated test helper `test_ctx` with `next_request_id`.
- `src/server/accept.rs` — Added `next_request_id: Arc::new(AtomicU64::new(1))` to both `ConnectionContext` construction sites (`accept_connection`, `tcp_connect`).
- `src/server/cloud.rs` — Added `next_request_id: Arc::new(AtomicU64::new(1))` to `ConnectionContext` in `run_cloud_connection`.
- `src/client.rs` — Added `AtomicU64` counter threaded through `new_agent`→`subscribe_and_stream`→`run_attached` and `attach`→`subscribe_and_stream`→`run_attached`. All construction sites use `Message::routable()`. All match sites use two-step decode (`Message::Routable { payload, .. }` then `RoutableMessage::decode(&payload)`).
- `src/transport/unix.rs` — Updated 3 test construction sites to use `Message::routable()` and two-step decode.

### Decisions Made
- Opaque payload enables zero-copy forwarding at relay servers — intermediate hops never need to understand the routable message content
- `request_id` is per-message (not per-request/response pair) — stream messages each get a unique ID, request/response messages echo the incoming ID
- `UnknownMessage` returned on decode failure rather than dropping the message — gives the sender a signal that the destination couldn't understand the payload
- Dashboard intentionally left untouched (deadweight per user)

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — 125 tests pass, zero warnings
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass

### Next Steps
- Protocol v3 phase 4 complete; all four phases done

---

## 2026-02-22: Route management overhaul + SubscriptionClosed (protocol v3, phase 3)

### Summary
Phase 3 of protocol v3 upgrade. Made `AnnounceHost`/`WithdrawHost` the single source of routing truth. Renamed `AgentEnded` to `SubscriptionClosed` (semantic clarity: it signals subscription EOF, not agent death). Added `Route::starts_with_route` for prefix matching and `AgentRegistry::remove_for_route_prefix` for bulk agent removal by route. Enhanced `WithdrawHost` handler to cascade-remove agents and cancel streams for withdrawn hosts. Removed `dead_routes` HashSet and simplified forwarding failures to silent debug-level drops. Peer disconnect now only broadcasts `WithdrawHost` (no more per-agent `WithdrawAgent` broadcasts).

### Changes
- `src/message.rs` — Renamed `RoutableMessage::AgentEnded` to `SubscriptionClosed`
- `src/route.rs` — Added `starts_with_route(&self, prefix: &Route) -> bool` with 5 unit tests
- `src/agent_registry.rs` — Added `remove_for_route_prefix(&mut self, prefix: &Route) -> Vec<Uuid>` with 3 unit tests
- `src/server/connection.rs` — Renamed all `AgentEnded` references. Enhanced `WithdrawHost` handler to call `remove_for_route_prefix` and `cancel_streams_matching`. Removed `dead_routes: HashSet<String>` and its parameter from `handle_message`/`handle_routable`. Simplified forwarding path to try-send-or-drop with debug logging. Removed `use std::collections::HashSet`. Added `withdraw_host_removes_agents_with_matching_route` test.
- `src/server/routing.rs` — Removed per-agent `WithdrawAgent` broadcast loop from `handle_peer_disconnect`; kept `remove_for_link` for local cleanup
- `src/client.rs` — Updated `AgentEnded` match arm to `SubscriptionClosed`

### Decisions Made
- `WithdrawHost` cascades to agents: when a host is withdrawn, all agents reachable through that host's route are bulk-removed, matching the spec that host discovery is the single source of routing truth
- Silent drops on forwarding failure: instead of tracking dead routes, forwarding failures are logged at debug level and silently dropped — `WithdrawHost` propagation handles the cleanup
- `SubscriptionClosed` name: clarifies that this message signals the end of a subscription stream (buffer EOF), not the death of the agent itself

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — 122 tests pass, zero warnings
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass
- Grep confirms no `AgentEnded` or `dead_routes` in `src/`

### Next Steps
- Protocol v3 is complete; continue with remaining milestone 2 work

---

## 2026-02-22: Result cleanup, error removal, PermissionTool dedup (protocol v3 continued)

### Summary
Phase 2 of protocol cleanup. Removed the duplicate `PermissionTool` enum (promoting `ClaudePermissionTool` as the canonical type). Removed `ProtocolError::NoRouteFound`, `RoutableMessage::Error`, `DirectMessage::Error`, and `impl From<&AmuxError> for Message`. Dropped the redundant `success: bool` field from all 6 result variants — `error: None` now means success, `error: Some(e)` means failure. Deleted 6 NoRouteFound tests that referenced removed types. Kept `dead_routes` insert-only tracking as planned.

### Changes
- `src/message.rs` — Deleted `PermissionTool` enum and 3 variants (`RoutableMessage::Error`, `DirectMessage::Error`, `ProtocolError::NoRouteFound`). Removed `success: bool` from `SubscribeRawResult`, `SubscribeStructuredResult`, `CreateAgentResult`, `ConnectResult`, `ConnectToServerResult`, `HandleHookResult`. Deleted `impl From<&AmuxError> for Message`. Changed `ClaudeStructuredOutput::PermissionRequest` to use `ClaudePermissionTool` directly. Added `PartialEq` derive to `ClaudePermissionTool` and 8 tool input structs. Updated 2 tests.
- `src/hooks.rs` — Deleted `impl From<ClaudePermissionTool> for PermissionTool`. Updated ConnectResult and HandleHookResult match arms.
- `src/server/connection.rs` — Simplified routing failure block to dead_routes-only tracking. Removed Error match arms from `msg_type_label`, local delivery, and `handle_direct`. Changed `perm_req.tool.clone().into()` to `.clone()`. Deleted 6 NoRouteFound tests. Updated 2 surviving connect_reauth tests.
- `src/server/accept.rs` — Removed 3 `Message::from(&e)` error-send sites. Removed `DirectMessage::Error` match arm. Updated ConnectResult constructions and matches.
- `src/client.rs` — Updated all result match arms. Removed `DirectMessage::Error` and `RoutableMessage::Error` arms.
- `src/cloud.rs` — Updated ConnectResult match arms in `connect()` and `handle_response()`.

### Decisions Made
- `ClaudePermissionTool` promoted as the single permission tool enum (was duplicated as `PermissionTool` with a lossy From impl). Required adding `PartialEq` derive cascading through 8 tool input structs.
- `success: bool` removed since it was always redundant with `error: Option<ProtocolError>`.
- `dead_routes` tracking kept as insert-only per plan spec (no reads, no error messages sent back). Will be removed in Phase 3.
- 6 NoRouteFound tests deleted (they tested error construction for a removed variant). Net test count: 119 → 113.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 113 tests pass, zero warnings.
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 9/10 pass (attach test is a pre-existing flaky failure).
- Grep verified: no remaining `success: bool`, `NoRouteFound`, `RoutableMessage::Error`, `DirectMessage::Error`, `enum PermissionTool`, or `impl From<&.*AmuxError` in src/.

### Next Steps
- Phase 3: Routing restructure (remove dead_routes, simplify routing logic)

---

## 2026-02-21: Command enum + ShutdownNotification (protocol v3)

### Summary
Separated CLI-only messages from peer-to-peer protocol messages. Added `Command` enum for CLI commands (ListAgents, Shutdown, Debug, etc.) that must not be accepted from remote peers. Added structured `ShutdownNotification(ShutdownReason)` replacing the string-based `ServerShutdown`. Bumped `PROTOCOL_VERSION` from 2 to 3.

### Changes
- `src/message.rs` — Added `ShutdownReason` enum (ProtocolMismatch, UserRequested) with Display impl. Added `Command` enum with all CLI-only variants. Added `Message::Command(Command)` variant. Trimmed `DirectMessage` to only peer-to-peer messages (Connect, ConnectResult, Announce*, Withdraw*, Error). Bumped PROTOCOL_VERSION to 3. Updated 3 tests.
- `src/server/connection.rs` — Added `is_local: bool` to `ConnectionContext`. Added `Message::Command` arm to `msg_type_label` and `handle_message` (rejects remote commands). Created `handle_command` function (extracted from `handle_local`). Simplified `handle_local` catch-all. Updated 3 tests.
- `src/server/accept.rs` — Passed `is_local` to `ConnectionContext` in `accept_connection` and `tcp_connect`.
- `src/server/cloud.rs` — Passed `is_local: false` to `ConnectionContext`. Replaced `DirectMessage::ServerShutdown` with `Command::ShutdownNotification(ShutdownReason::ProtocolMismatch)`.
- `src/client.rs` — All CLI sends/receives changed from `DirectMessage` to `Command` variants. Changed `shutdown_reason` from `Option<String>` to `Option<ShutdownReason>`.
- `src/hooks.rs` — Changed `HookEvent`/`HookEventResult` to `Command::HandleHook`/`Command::HandleHookResult`.

### Decisions Made
- CLI commands in `Command` enum are rejected at the `handle_message` level for remote connections (is_local=false), providing defense-in-depth against protocol abuse.
- `ShutdownReason` uses an enum rather than strings for type safety and Display impl.
- Dashboard TypeScript files left as-is (will be updated separately).

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 119 tests pass, zero warnings.
- `cargo build --workspace && cargo run -p e2e-runner -- run` — all 10 E2E tests pass (attach test has pre-existing flakiness under parallel load).

### Next Steps
- Phase 2: Error reporting cleanup (DirectMessage::Error migration)
- Dashboard TypeScript alignment with new protocol

---

## 2026-02-21: Protocol restructure — clean naming and agent-type-keyed messages

### Summary
Pre-launch protocol restructure establishing clean naming conventions and agent-type-keyed message structure before the wire format is locked in. Bumps `PROTOCOL_VERSION` from 1 to 2.

### Changes

**Renames (Phase 1):**
- `LocalMessage` → `DirectMessage`, `Message::Local(...)` → `Message::Direct(...)`
- `RoutableMessage::InputBytes` → `RawInput`, `RoutableMessage::Output` → `RawOutput`
- `DirectMessage::ConnectResponse` → `ConnectResult` (consistency with other `*Result` variants)
- `AgentInfo` → `Agent`, `HostInfo` → `Host`
- `Agent.agent_id` → `Agent.id`, `Agent.alias` → `Agent.name`
- `alias` → `name` everywhere (struct fields, variables, method names, registry internals)
- `to_agent_info()` → `to_agent()`, `alias_taken()` → `name_taken()`
- Updated all match arms, type guards, and `msg_type_label` strings across all Rust files

**Restructured StructuredOutput (Phase 2):**
- Moved `StructuredLog` and `PermissionTool` from `src/structured_log.rs` into `src/message.rs`
- Renamed `StructuredLog` → `ClaudeStructuredOutput`, added `#[serde(other)] Unknown` for forward compatibility
- Added `StructuredOutput` wrapper enum: `enum StructuredOutput { Claude(ClaudeStructuredOutput) }`
- Changed `RoutableMessage::StructuredOutput` field from `entry: StructuredLog` to `data: StructuredOutput`
- Deleted `src/structured_log.rs`

**Added StructuredInput (Phase 3):**
- Added `ClaudeStructuredInput` enum with `PermissionResponse(PermissionResponse)` and `SubmitMessage { data: Vec<u8> }`
- Added `StructuredInput` wrapper enum: `enum StructuredInput { Claude(ClaudeStructuredInput) }`
- Replaced `RoutableMessage::SubmitInput` and `RoutableMessage::PermissionRequestResponse` with single `RoutableMessage::StructuredInput { agent_id, data: StructuredInput }`
- Single handler in `connection.rs` replaces two separate handlers

**Split Subscribe (Phase 4):**
- `Subscribe` → `SubscribeRaw` / `SubscribeStructured` (two separate variants)
- `SubscribeResult` → `SubscribeRawResult` / `SubscribeStructuredResult`
- Removed `SubscribeMode` enum
- Split subscribe handler into two match arms in `connection.rs`

**Finalize (Phase 5):**
- Bumped `PROTOCOL_VERSION` from 1 to 2
- Updated all tests in `message.rs`, `connection.rs`, `multiplex_log_buffer.rs`, `transcript.rs`
- Updated dashboard TypeScript (`protocol.ts`, `appStore.ts`, `useWebSocket.ts`, `Message.tsx`)
- Updated `ARCHITECTURE.md` with new type names and descriptions

**Files modified:** `src/message.rs`, `src/server/connection.rs`, `src/server/accept.rs`, `src/server/routing.rs`, `src/server/cloud.rs`, `src/server/mod.rs`, `src/client.rs`, `src/hooks.rs`, `src/session.rs`, `src/transcript.rs`, `src/multiplex_log_buffer.rs`, `src/cloud.rs`, `src/transport/unix.rs`, `src/lib.rs`, `dashboard/src/types/protocol.ts`, `dashboard/src/store/appStore.ts`, `dashboard/src/hooks/useWebSocket.ts`, `dashboard/src/components/Message.tsx`, `ARCHITECTURE.md`

**Files deleted:** `src/structured_log.rs`

### Decisions Made
- Wrapper enums (`StructuredOutput`, `StructuredInput`) use serde's default externally-tagged format for the outer key (e.g. `{"Claude": {...}}`), while inner Claude-specific enums use internally-tagged format (`#[serde(tag = "type")]`)
- `#[serde(other)] Unknown` on `ClaudeStructuredOutput` for forward compatibility with new output types
- Split Subscribe into two variants (instead of keeping `SubscribeMode`) — cleaner handler code, each variant carries only the fields it needs (`SubscribeRaw` has `terminal_size`, `SubscribeStructured` does not)
- Wire format for dashboard: `StructuredOutput.data.Claude` to unwrap, `{Claude: {PermissionResponse: "Yes"}}` for structured input

### Verification
- `cargo check` — passes
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — passes
- `cargo test` — 119 tests pass
- `npx tsc --noEmit` (dashboard) — passes

### Next Steps
- Run E2E tests (`cargo build --workspace && cargo run -p e2e-runner -- run`)
- Update `dashboard/CLAUDE.md` protocol notes

---

## 2026-02-21: Replace rows/cols with Option\<TerminalSize\>

### Summary
Replaced bare `rows: u16, cols: u16` fields with `Option<TerminalSize>` in `CreateAgentRequest` and `RoutableMessage::Subscribe`. `TerminalSize` is a small struct with `Default` impl (24x80). `None` means "use defaults" (and in the future, headless/no-PTY mode). The server only resizes the PTY when `Some(size)` is provided.

### Changes
- `src/message.rs` — Added `TerminalSize` struct with `Default` (24x80), `PartialEq`, `Eq`, `Copy`. Changed `CreateAgentRequest` to `terminal_size: Option<TerminalSize>` with `#[serde(default)]`. Changed `Subscribe` to `terminal_size: Option<TerminalSize>` with `#[serde(default)]`. Updated tests.
- `src/client.rs` — `get_terminal_size()` now returns `TerminalSize`. `new_agent()` and `attach()` pass `Some(terminal_size)`. `subscribe_and_stream()` takes `Option<TerminalSize>`.
- `src/session.rs` — `LocalAgentSession::new()` uses `req.terminal_size.unwrap_or_default()` for PTY creation.
- `src/server/routing.rs` — `handle_subscribe()` takes `Option<TerminalSize>`, only resizes when `Some`.
- `src/server/connection.rs` — Subscribe handler passes `terminal_size` through. Structured subscribe only resizes when `Some`. Updated tests.
- `src/transport/unix.rs` — Updated test.

### Decisions Made
- `Option<TerminalSize>` rather than sentinel values — `None` is unambiguous "no preference"
- Applied to both `CreateAgentRequest` and `Subscribe` for consistency
- `#[serde(default)]` on both fields for wire compatibility with older clients (deserializes as `None`)
- `Default` impl on `TerminalSize` (24x80) keeps unwrap sites clean

### Verification
- `cargo check` — passes
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — passes
- `cargo test` — 119 tests pass
- E2E tests — 10/10 pass (attach test flakiness was caused by stale amux processes from prior runs; `pkill -f amux` before running fixes it — not flaky in CI)

---

## 2026-02-21: Move CreateAgent/CreateAgentResult to RoutableMessage

### Summary
Moved `CreateAgent` and `CreateAgentResult` from `LocalMessage` to `RoutableMessage`, enabling remote agent creation. A mobile app or remote client can now send `CreateAgent` with a dst route pointing to a host, the cloud relay forwards it, and the local server creates the agent and routes the result back. For local CLI usage, the behavior is identical (dst is empty, message is handled locally).

### Changes
- `src/message.rs` — Moved `CreateAgent(CreateAgentRequest)` and `CreateAgentResult` from `LocalMessage` to `RoutableMessage`. Added `agent_id: Uuid` field to `CreateAgentResult` for consistency with `SubscribeResult`. Updated `test_message_roundtrip_create_agent` to use `Message::Routable`.
- `src/server/connection.rs` — Moved `CreateAgent`/`CreateAgentResult` labels from `LocalMessage` to `RoutableMessage` branch in `msg_type_label`. Removed `CreateAgent` handler from `handle_local`. Added `CreateAgent` handler to `handle_routable` local delivery section (follows same pattern as `Subscribe`: compute reply route, call `create_agent`, send result back). Added `CreateAgentResult` to the silent-ignore arm for response messages at destination.
- `src/client.rs` — Updated `new_agent()` to send `Message::Routable` with `RoutableMessage::CreateAgent` instead of `Message::Local`. Moved `full_route` construction before the send. Added `RoutableMessage::Error` arm for routing failures. Response matching now uses `Message::Routable { message: RoutableMessage::CreateAgentResult { .. }, .. }`.
- `src/transport/unix.rs` — Updated `test_message_roundtrip` to use `Message::Routable` with `RoutableMessage::CreateAgent`. Removed unused `LocalMessage` import.

### Decisions Made
- Added `agent_id` to `CreateAgentResult` for consistency with other routable responses (`SubscribeResult`)
- Client builds route before sending CreateAgent (moved `Route::from_link` up) so the same route is used for both CreateAgent and Subscribe
- Added `RoutableMessage::Error` match in client for future remote creation failures (routing errors)

### Verification
- `cargo check` — passes
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — passes
- `cargo test` — 119 tests pass
- E2E tests — 10/10 pass (kill stale amux processes first)

### Next Steps
- Remote agent creation from mobile/web clients via cloud relay
- ListAgents could also move to RoutableMessage for remote listing

---

## 2026-02-21: Implement AnnounceHost / WithdrawHost host discovery

### Summary
Added host discovery messages so local amux servers announce themselves as hosts when connecting to peers (cloud or direct TCP). This follows the existing AnnounceAgent/WithdrawAgent pattern exactly — hop-by-hop propagation with link-match guards to prevent message explosion.

### Changes
- `src/message.rs` — Added `HostInfo` struct with `id`, `name`, `route`, `version` fields. Added `AnnounceHost` and `WithdrawHost` variants to `LocalMessage`. Added `host_count` to `ServerDebugInfo`. Added 3 serialization roundtrip tests.
- `src/server/mod.rs` — Added `host_id: Uuid` to `ServerState` (generated at startup). Added `hosts: HashMap<Uuid, HostInfo>` to `ServerUserState`.
- `src/server/connection.rs` — Added `AnnounceHost`/`WithdrawHost` to `msg_type_label`. Added handlers mirroring AnnounceAgent/WithdrawAgent (skip own host_id, prepend link to route, link-match guard on withdraw). Updated Debug handler to include `host_count`. Added 5 unit tests.
- `src/server/routing.rs` — Renamed `send_initial_announcements` to `send_initial_agent_announcements` (now private). Added `send_initial_host_announcements`. Added unified `send_initial_announcements` that calls both plus sends own AnnounceHost for non-cloud servers. Updated `handle_peer_disconnect` to withdraw hosts learned from the dead link.
- `src/server/accept.rs` — Updated `accept_connection` and `tcp_connect` to read global state (host_id, host_name, cloud_mode) and pass to unified `send_initial_announcements`.
- `src/server/cloud.rs` — Same pattern for cloud connections.

### Decisions Made
- Cloud servers don't announce themselves as hosts — they're stateless relays
- `host_id` is ephemeral (Uuid::new_v4() at startup), not persisted — reconnection generates a new one
- No host registry/alias resolution — a simple `HashMap<Uuid, HostInfo>` suffices
- Lock ordering: callers read global state first, drop the lock, then pass extracted values alongside the user state write lock to avoid holding both locks simultaneously
- `version` uses `env!("CARGO_PKG_VERSION")` at compile time

### Verification
- `cargo check` — passes
- `cargo fmt` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — passes (fixed collapsible_if)
- `cargo test` — 119 tests pass (8 new: 3 roundtrip + 5 handler tests)
- E2E tests — all 10 pass

### Next Steps
- Wire host info into list/display commands (e.g., `amux list-hosts`)
- Mobile client host selection UI

---

## 2026-02-20: Add all Claude Code permission tool variants

### Summary
Added typed parsing for all 8 Claude Code tools that require permissions: Bash, Edit, Write, WebFetch, WebSearch, NotebookEdit, Skill, and ExitPlanMode. (Edit and AskUserQuestion were already handled.) Previously unrecognized tools fell through to the `Unknown` catch-all, got warned, and dropped. Now they are properly deserialized with full tool input data, converted to `PermissionTool` for structured logs, and described in hook debug logging.

### Changes
- `src/message.rs` — Added 8 tool input structs (`BashToolInput`, `WriteToolInput`, `WebFetchToolInput`, `WebSearchToolInput`, `NotebookEditToolInput`, `SkillToolInput`, `ExitPlanModeToolInput`, `ExitPlanModePrompt`) and 8 corresponding `ClaudePermissionTool` variants. Added 7 new deserialization tests.
- `src/structured_log.rs` — Added 8 `PermissionTool` variants carrying all fields from the tool input structs.
- `src/hooks.rs` — Added 8 conversion arms in `From<ClaudePermissionTool> for PermissionTool` and 8 `describe_hook` arms.

### Decisions Made
- Tool set based on Claude Code docs listing tools that require permissions (not read-only tools like Read/Grep/Glob)
- `PermissionTool` variants carry all fields from the corresponding tool input (matching Edit/AskUserQuestion pattern) so dashboard clients have the full data
- `ExitPlanMode` has structured `allowed_prompts` vec rather than flattening

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 111 tests pass, zero warnings

---

## 2026-02-20: Add AskUserQuestion parsing to hooks

### Summary
Added `AskUserQuestion` as a recognized variant of `ClaudePermissionTool`, parsing the full question/option/multiSelect structure from Claude Code's JSON. Previously this tool was caught by the `Unknown` fallback and logged but dropped.

### Changes
- `src/message.rs` — Added `AskUserQuestionToolInput`, `AskUserQuestionItem`, `AskUserQuestionOption` structs and `AskUserQuestion` variant to `ClaudePermissionTool`. Updated existing unknown-tool test to use a truly unknown tool name. Added two new tests for single-select and multi-select deserialization.
- `src/structured_log.rs` — Added `AskUserQuestion { questions }` variant to `PermissionTool`, reusing `AskUserQuestionItem` from message.rs.
- `src/hooks.rs` — Added conversion arm in `From<ClaudePermissionTool> for PermissionTool` and `describe_hook` arm showing question count.

### Decisions Made
- Reuse `AskUserQuestionItem` type in structured_log rather than duplicating: it already derives all needed traits
- `multiSelect` uses `#[serde(default, rename = "multiSelect")]` to match Claude Code's camelCase JSON

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 105 tests pass, zero warnings

### Next Steps
- Forward AskUserQuestion to dashboard clients for interactive answering

---

## 2026-02-20: Handle unknown ClaudePermissionTool variants gracefully

### Summary
When Claude Code sends a PermissionRequest hook with a `tool_name` that isn't `Edit` (e.g., `AskUserQuestion`), deserialization would fail because `ClaudePermissionTool` only had the `Edit` variant. Added an `Unknown` fallback variant using `#[serde(other)]` so parsing succeeds, then return early in the hook handler (log the raw input at info level, don't forward to the server).

### Changes
- `src/message.rs` — Added `#[serde(other)] Unknown` variant to `ClaudePermissionTool`
- `src/hooks.rs` — Early return for Unknown tools with info log, added match arm in `describe_hook`, added arm in `From<ClaudePermissionTool> for PermissionTool`
- `src/structured_log.rs` — Added `Unknown` variant to `PermissionTool`

### Decisions Made
- Early return before sending to server: unknown tools have no useful data to forward
- Log at warn level: signals that amux needs updating to handle new tools
- Added `Unknown` to `structured_log::PermissionTool` defensively even though the conversion is unreachable for Unknown tools

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 101 tests pass, zero warnings

### Next Steps
- Add specific tool variants as needed when Claude Code adds new permission tools

---

## 2026-02-18: Add TCP keepalives to all TCP connections

### Summary
When the cloud server restarts, connected local servers couldn't detect the dead TCP connection — the reader blocks indefinitely on a read that never returns, and OS defaults are too slow (macOS: ~2 hours). Added TCP keepalive configuration (30s idle, 10s probe interval) to all 4 TCP socket creation points so dead connections are detected within ~60s.

### Changes
- `Cargo.toml` — Added `socket2 = "0.5"` dependency (already a transitive dep via tokio)
- `src/transport/mod.rs` — Added `configure_tcp_keepalive` helper function
- `src/server/mod.rs` — Called helper after TCP accept (line 282) and WebSocket accept (line 320)
- `src/server/accept.rs` — Called helper in `tcp_connect` after `set_nodelay` (line 416)
- `src/transport/tls.rs` — Called helper in `tls_connect` after `set_nodelay` (line 26)

### Decisions Made
- 30s idle / 10s interval chosen as reasonable defaults — fast enough to detect cloud restarts, not so aggressive as to waste bandwidth
- Warn-and-continue on failure (matching existing `set_nodelay` pattern) rather than hard error, since keepalive is an optimization
- Used `socket2::SockRef` to configure keepalive on tokio's `TcpStream` without taking ownership

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 101 tests pass
- 9/10 E2E tests pass (`attach` failure is pre-existing)

---

## 2026-02-18: Fix WebSocket cloud mode authentication and peer registration

### Summary
WebSocket connections in cloud mode had two bugs:
1. `websocket_accept` hardcoded `verify_token=false` instead of passing `is_cloud_server` (fixed in v0.1.10)
2. The arguments to `accept_connection` were swapped: `(false, verify_token)` instead of `(verify_token, false)`, meaning `verify_token` was passed as `is_local` and vice versa. This caused WebSocket clients to skip authentication AND be treated as local/terminal connections (skipping `peer_links` insertion and `send_initial_announcements`).

### Changes
- `src/server/mod.rs` — Pass `is_cloud_server` to `websocket_accept` (v0.1.10)
- `src/server/accept.rs` — Add `verify_token: bool` parameter to `websocket_accept`, fix argument order in `accept_connection` call from `(false, verify_token)` to `(verify_token, false)`

### Decisions Made
- The swapped arguments were a positional boolean trap — both params are `bool` so the compiler couldn't catch it. Verified all other callers (`tcp_accept`, `unix_accept`) have the correct order.

### Verification
- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test` — all 101 tests pass
- Confirmed argument order matches for all three callers: tcp_accept `(verify_token, false)`, unix_accept `(false, true)`, websocket_accept `(verify_token, false)`

---

## 2026-02-15: Add targeted tracing spans (connection, session, stream)

### Summary

Added three tracing spans to eliminate repeated log fields and correlate all log lines within a lifecycle. The `connection` span carries `link` and `transport`, the `session` span carries `agent_id` and `command`, and the `stream` span carries `stream_id`, `agent_id`, and `mode`. Removed ~18 manually repeated field arguments from tracing calls that are now inherited from parent spans.

### Changes

- `src/server/accept.rs` — added `connection` span in `accept_connection` and `tcp_connect`; instrumented reader/writer/connection_loop tasks
- `src/server/cloud.rs` — added `connection` span in `run_cloud_connection`; instrumented reader/writer/connection_loop tasks
- `src/server/connection.rs` — removed `link = %ctx.link_name` from ~12 tracing calls; added `stream` span for both structured and raw subscribe handlers
- `src/session.rs` — added `session` span in `LocalAgentSession::new()`; instrumented 3 spawned tasks (PTY reader, input writer, child wait)

### Decisions Made

- Used manual `info_span!` + `.instrument()` instead of `#[tracing::instrument]` because key fields are derived after function entry, functions take complex generic/Arc args, and spans don't align with function boundaries
- Used `span.enter()` guard for `spawn_blocking` closures since spans don't auto-propagate into blocking contexts
- No per-message spans (Output/InputBytes fire thousands of times per second)
- No client-side or cloud-reconnect spans (short-lived, low value)

### Verification

- `cargo check && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test` — 101 tests pass
- `cargo build --workspace && cargo run -p e2e-runner -- run` — 10 E2E tests pass

---

## 2026-02-15: Enforce zero clippy warnings in CLAUDE.md and CI

### Summary

Updated CLAUDE.md to use `cargo clippy --workspace --all-targets -- -D warnings` (warnings are errors) and added an explicit "no `#[allow(clippy::...)]`" policy. Updated CI to add `--workspace` to the clippy job so all workspace members are checked, not just the root crate.

### Changes

- `CLAUDE.md` — updated clippy commands to use `-D warnings` and `--workspace --all-targets`, added clippy policy note
- `.github/workflows/ci.yml` — added `--workspace` to clippy step

### Verification

- `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings across all crates

---

## 2026-02-15: Fix all clippy warnings and remove clippy exceptions

### Summary

Fixed all 10 clippy warnings (9 `collapsible_if`, 1 `clone_on_copy`) and removed the one `#[allow(clippy::type_complexity)]` exception by introducing a `PreparedEnvironment` type alias.

### Changes

- `src/agent_registry.rs` — collapsed 2 nested ifs, replaced `.clone()` with `*` dereference on `Copy` type `Uuid`
- `src/jwt.rs` — collapsed 2 nested ifs (cache check, JWK parsing)
- `src/server/connection.rs` — collapsed 1 nested if (stale route cleanup)
- `src/server/routing.rs` — collapsed 2 nested ifs (alias check, peer broadcast)
- `src/main.rs` — collapsed 1 nested if (hook event fast-path)
- `e2e-runner/src/executor.rs` — replaced `#[allow(clippy::type_complexity)]` with `PreparedEnvironment` type alias

### Verification

- `cargo check` — clean
- `cargo fmt` — no changes
- `cargo clippy --workspace` — zero warnings
- `cargo test` — 101 tests pass

---

## 2026-02-15: Replace custom log! macro with tracing

### Summary

Replaced the custom `log!` macro (hardcoded to `/tmp/amux.log`, no levels, no structured fields) with the `tracing` crate. Overhauled all ~150 log sites: removed noise, added missing instrumentation, used proper levels (`info`, `warn`, `error`, `debug`) and structured fields. Output goes to file only (configurable via `AMUX_LOG` env var, default `/tmp/amux.log`). Default level is `info`, overridable via `RUST_LOG`.

### Changes

- `Cargo.toml` — added `tracing`, `tracing-subscriber` (env-filter, fmt), `tracing-appender`
- `src/log.rs` — deleted entirely
- `src/lib.rs` — removed `#[macro_use] pub mod log`
- `src/main.rs` — replaced `log::init()` with `init_tracing()` using non-blocking file writer and `EnvFilter`; `WorkerGuard` held in `main()`
- `src/session.rs` — 8 log sites replaced; removed 2 redundant cleanup logs
- `src/client.rs` — 14 log sites replaced; removed 3 redundant logs
- `src/hooks.rs` — 6 log sites replaced; removed raw input from parse error log
- `src/cloud.rs` — 3 log sites replaced
- `src/jwt.rs` — 1 log site replaced; added `debug!("fetching JWKS")` instrumentation
- `src/transcript.rs` — 1 log site replaced
- `src/server/mod.rs` — 19 log sites replaced; removed 2 redundant "client connected" logs
- `src/server/accept.rs` — 16 log sites replaced
- `src/server/cloud.rs` — 10 log sites replaced
- `src/server/connection.rs` — 30 log sites replaced; added `msg_type_label()` helper; removed empty-dst drop log
- `src/server/routing.rs` — 6 log sites replaced

### Decisions Made

- **File-only output**: No stdout/stderr logging — amux controls the terminal, so all tracing goes to file
- **`amux=info` default filter**: Matches the most useful level for scanning logs; `debug` available via `RUST_LOG=amux=debug`
- **`msg_type_label` helper**: Avoids logging full `Debug` representations of messages (which can be large); logs just the variant name
- **Removed redundant logs**: "PTY master dropped", "multiplex buffers closed" (implied by "agent exited"); "agent created successfully" (immediately followed by subscribe); "client connected" (immediately followed by "connection established")
- **Connection errors at `debug`**: Client disconnects are normal operation, not worth `warn`

### Verification

- `cargo check && cargo fmt && cargo clippy` — clean (pre-existing warnings only)
- `cargo test` — 101 tests pass
- E2E tests — 10/10 pass

---

## 2026-02-12: Fix debug command to show global state after multi-tenancy

### Summary

The `amux debug` command was broken on cloud servers after multi-user tenancy was added. It read from `ctx.user_state` (per-user state), so it only showed agents/routes/peers for the requesting user instead of global server state. Fixed by iterating all users in `state.users` and aggregating counts. Also removed per-route/per-link name lists (not useful for debugging) and added `user_count` and `peer_link_count` fields.

### Changes

- `src/message.rs` — `ServerDebugInfo`: removed `routes: Vec<String>` and `peer_links: Vec<String>`, added `user_count: usize` and `peer_link_count: usize`
- `src/server/connection.rs` — `Debug` handler now iterates all users in `state.users` to produce global aggregates instead of reading a single user's state

### Verification

- `cargo check && cargo fmt && cargo clippy` — clean
- `cargo test` — 100 tests pass
- E2E tests — 10/10 pass

---

## 2026-02-12: Fix e2e test server cleanup

### Summary

E2E tests were leaking background `amux serve` processes. Each test spawns servers via `ensure_server_running` (a detached background process), but the executor never killed them after the test completed. Over time this accumulated ~90 zombie server processes holding open TCP/WebSocket ports, causing test failures due to resource exhaustion (the 500ms init window became insufficient).

### Changes

- `e2e-runner/src/executor.rs` — Extracted step execution into `execute_steps()` method; `run_test_inner` now runs `kill-server` for each test config after steps complete (pass or fail), then removes socket files

### Verification

- `cargo check && cargo fmt && cargo clippy` — clean
- `cargo test` — 100 tests pass
- E2E tests — 10/10 pass, zero leaked `amux serve` processes after run

---

## 2026-02-12: Protocol version checking on Connect handshake

### Summary

Added `PROTOCOL_VERSION` (starting at 1) to the Connect handshake. The server rejects version mismatches with `ProtocolError::VersionMismatch`. Old clients without the version field default to version 0 via `#[serde(default)]` and are rejected. When a cloud server rejects a local server due to version mismatch, all attached terminals receive a `ServerShutdown` message for a clean exit.

### Changes

- `src/message.rs` — Added `PROTOCOL_VERSION: u32 = 1` const; added `version: u32` field with `#[serde(default)]` to `LocalMessage::Connect`; added `VersionMismatch` variant to `ProtocolError`; added `LocalMessage::ServerShutdown`; added roundtrip tests for new fields/messages
- `src/error.rs` — Added `AmuxError::VersionMismatch(String)`
- `src/client.rs` — Added `version: PROTOCOL_VERSION` to Connect; handle `VersionMismatch` in handshake; handle `ServerShutdown` in `run_attached` select loop (clean exit with reason message)
- `src/server/accept.rs` — Version check in `accept_handshake` (before link name/token validation); added `version: PROTOCOL_VERSION` to `connect_handshake`; handle `VersionMismatch` response
- `src/cloud.rs` — Added `CloudError::VersionMismatch`; added version to both Connect sends; handle `VersionMismatch` in `connect()` and `handle_response()`
- `src/server/cloud.rs` — Added `CloudConnectionError::VersionMismatch`; map from `CloudError` and `AmuxError`; on version mismatch, send `ServerShutdown` to all terminal routes then `process::exit(1)`
- `src/hooks.rs` — Added `version: PROTOCOL_VERSION` to Connect
- `src/server/connection.rs` — Use `..` in re-auth Connect pattern (no version check on established connections); updated tests with version field

### Decisions Made

- **Version 0 = old client**: `#[serde(default)]` on `version: u32` means old clients without the field get version=0, which is rejected
- **Check only on accept side**: `accept_handshake` is the single enforcement point for new connections
- **No check on re-auth**: Token refresh Connect doesn't re-check version (already established)
- **ServerShutdown for clean terminal exit**: New message type gives terminals a clean exit path similar to `AgentEnded`
- **Hard exit on version mismatch**: `process::exit(1)` after notifying terminals — appropriate for alpha

### Verification

- `cargo check` — clean
- `cargo fmt && cargo clippy` — clean
- `cargo test` — 100 tests pass (including 5 new roundtrip/display tests)
- E2E tests — 10/10 pass (prior `attach` failure was caused by leaked server processes from previous runs)

### Next Steps

- Increment `PROTOCOL_VERSION` when making breaking protocol changes
- Consider graceful degradation or negotiation for future versions

---

## 2026-02-12: User multi-tenancy via ServerUserState

### Summary

Split `ServerState` into global state and per-user state (`ServerUserState`). Each authenticated user gets fully isolated state: agents, routes, registry, peer links, and active streams. Non-authenticated (local) connections share a default user (`LOCAL_USER_ID = Uuid::nil()`). The wire protocol is unchanged, and local mode is behaviorally identical to before.

### Changes

- `src/session.rs` — Changed `SessionEvent::Ended(Uuid)` to `SessionEvent::Ended { agent_id: Uuid, user_id: Uuid }`; added `user_id: Uuid` parameter to `LocalAgentSession::new()`
- `src/server/mod.rs` — Added `LOCAL_USER_ID` constant; extracted per-user fields into `ServerUserState` struct; `ServerState` retains only global fields (config, cloud_mode, jwt_validator) plus `users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>`; added `user_state()` (get-or-create) and `get_user_state()` (read-only) helpers; updated event handler to look up user state by user_id
- `src/server/routing.rs` — All functions changed from `ServerState` to `ServerUserState` parameters; `create_agent` takes additional `user_id` for session creation
- `src/server/connection.rs` — `ConnectionContext` now holds both `state` (global) and `user_state` (per-user) plus `user_id`; `handle_routable` and `handle_local` use `ctx.user_state` for per-user operations (routes, agents, registry, streams) and `ctx.state` for global operations (config, cloud_mode, jwt_validator); stream helpers (`register_stream`, `cleanup_stream`, `cancel_streams_matching`) operate on `ServerUserState`; all 16 unit tests updated
- `src/server/accept.rs` — `accept_handshake` returns user_id and user state; parses `claims.sub` as Uuid after JWT validation; gets/creates user state from global state with proper lock ordering (drop global lock before acquiring user lock); `tcp_connect` takes additional `user_state` parameter
- `src/server/cloud.rs` — `establish_cloud_connection` gets default user state before reconnection loop; `run_cloud_connection` takes `user_state` parameter; routes/peer_links/announcements operate on user state

### Decisions Made

- **Per-user routes for security:** Global routes would expose per-user connection context globally. If User B discovered User A's link name and agent_id, B could craft a routable message that forwards through A's link to A's agent. Per-user routes make this structurally impossible — B's handler can only access B's route table.
- **Announcement isolation:** Per-user `peer_links` ensures agent announcements only reach the owning user's connections. Without this, `broadcast_to_peers` would leak agent_ids, aliases, and routes to all connected users.
- **No runtime auth checks needed:** Authentication happens once at connection time via signed JWT. The `user_id` maps to a `ServerUserState`. All operations are scoped to that user's state, providing complete isolation without per-message authorization checks.
- **Agent sharing is a future concern:** Per-user state provides complete isolation. If agent sharing is needed later, the design is TBD (could be cross-user forwarding, injecting shared routes into recipient state, etc.). Any sharing mechanism must include explicit authorization checks.
- **Easily reversible:** This is a pure implementation detail, contained to `src/server/` files and `src/session.rs`. No protocol or client changes. Local mode behaves identically since everything runs under `LOCAL_USER_ID`.
- **Lock ordering:** Always acquire global state lock to get/create `Arc<RwLock<ServerUserState>>`, then drop global lock before acquiring user lock. This prevents deadlocks.

### Verification

- `cargo check && cargo fmt && cargo clippy` — clean
- `cargo test` — 95 tests pass
- `cargo run -p e2e-runner -- run` — 10 E2E tests pass

---

## 2026-02-11: Fix remote attach hang when agent process exits

### Summary

Fixed a bug where clients attached to a remote agent (through a TCP peer connection) would hang indefinitely when the agent process died. The root cause was that `AgentEnded` was a `LocalMessage` with no routing information, so it could not be forwarded across server hops. Additionally, a race condition in the `SessionEvent::Ended` handler could cancel streaming tasks before they had a chance to send `AgentEnded` at all.

### Changes

- `src/message.rs` — Moved `AgentEnded { agent_id }` from `LocalMessage` to `RoutableMessage`
- `src/server/connection.rs` — Streaming tasks now send `Routable AgentEnded` with src/dst routing; added `AgentEnded` to route-failure suppression and empty-dst drop arms; removed dead `LocalMessage::AgentEnded` handler
- `src/server/mod.rs` — Removed `active_streams.remove(&agent_id)` from `SessionEvent::Ended` handler to eliminate race with buffer close
- `src/session.rs` — Close `log_buffer` alongside `buffer` in child waiter task so structured stream tasks see EOF
- `src/client.rs` — Match on `RoutableMessage::AgentEnded` instead of `LocalMessage::AgentEnded`
- `test-agent/src/main.rs` — Added `exit` command (process exits cleanly after echoing)
- `e2e-runner/src/parser.rs` — Preserve blank lines within output groups; strip trailing blank lines
- `e2e-tests/local_agent_ended.test` — New E2E test: local attach receives `[session ended]` when agent exits
- `e2e-tests/remote_agent_ended.test` — New E2E test: remote attach receives `[session ended]` when agent exits

### Decisions Made

- AgentEnded is semantically an in-band stream EOF, so it belongs in `RoutableMessage` alongside `Output` and `StructuredOutput`
- Only streaming tasks emit AgentEnded (after draining all output); subscriber disconnect via `cancel_rx` does not emit it
- Stream cleanup is task-local via `cleanup_stream` rather than centralized in the event handler
- E2E test uses `exit` command in test-agent rather than signal-based killing, keeping the test framework simple

### Verification

- `cargo check && cargo fmt && cargo clippy` — clean
- `cargo test` — 95 tests pass
- `cargo run -p e2e-runner -- run` — 10 E2E tests pass (8 existing + 2 new)

---

## 2026-02-11: Claude Code plugin with auto-install and fast-path hooks

### Summary

Added an in-repo Claude Code plugin that registers all 14 hook events, with automatic install/update during `amux new-agent claude` and fast-path exit for unhandled events. Unhandled hooks exit after clap parse only (~5-8ms), avoiding stdin reads, config loading, and socket connections.

### Changes

- `.claude-plugin/marketplace.json` — Marketplace manifest pointing to `./claude-plugin`
- `claude-plugin/.claude-plugin/plugin.json` — Plugin manifest with version 1.0.0
- `claude-plugin/hooks/hooks.json` — Registers all 14 hook events, each calling `amux hooks claude <event-name>`
- `src/state.rs` — Renamed `ClaudeState::is_plugin_installed: Option<String>` to `plugin_version: Option<u32>`
- `src/plugins/mod.rs` + `src/plugins/claude.rs` — New module for Claude plugin install/update logic (`ensure_plugin_installed()`, `PLUGIN_VERSION` const, `run_install()`, `run_update()`, `run_claude_command()`)
- `src/main.rs` — Expanded `ClaudeHookEvent` from 2 to 14 variants; added fast-path exit before config loading for unhandled events; added `is_handled_hook_event()`; wired `plugins::claude::ensure_plugin_installed()` into `NewAgent` path for Claude agent type only

### Decisions Made

- **Fast-path before config**: Unhandled hook events exit immediately after clap parse, no stdin/config/socket overhead
- **Version as u32**: Simpler than semver string; bump the const to trigger re-install across all users
- **Error propagation**: Plugin install failures exit 1; "command not found: claude" naturally tells users what's needed
- **Only on `new-agent claude`**: Plugin install never blocks non-Claude workflows

### Verification

- `cargo check && cargo fmt && cargo clippy && cargo test` — all 95 tests pass

### Next Steps

- Handle additional hook events beyond SessionStart and PermissionRequest
- Test plugin installation end-to-end with `claude` CLI

---

## 2026-02-11: Cancellation-safe connection loop and stale stream cleanup

### Summary

Fixed two bugs causing cloud relay disconnections: (1) `tokio::select!` cancellation unsafety — `read_exact()` inside `read_frame()` is not cancellation-safe; when `select!` cancels a partially-completed read, bytes are lost from the TCP stream, desynchronizing length-prefixed framing and triggering `InvalidMessage`; (2) stale stream flooding — when a subscriber disconnects at a downstream hop, streaming tasks never learn about it, causing thousands of "no route" log messages per second.

### Changes

**Phase 1: Transport Split Infrastructure**
- `src/transport/mod.rs` — Added `MessageReader`, `MessageWriter` (with `background()` for idle pong handling), `TransportSplit` traits
- `src/transport/framing.rs` — Added `FrameReader<R>`, `FrameWriter<W>`, `into_split()` method; deduplicated read/write logic into shared `read_frame_impl`/`write_frame_impl` free functions
- `src/transport/tcp.rs` — Added `TcpMessageReader<S>`, `TcpMessageWriter<S>`, `TransportSplit` impl
- `src/transport/unix.rs` — Added `UnixMessageReader`, `UnixMessageWriter`, `TransportSplit` impl
- `src/transport/websocket.rs` — Added `WsMessageReader`, `WsMessageWriter` with pong forwarding via `mpsc::channel(4)`, `TransportSplit` impl; `background()` sends pongs during idle periods

**Phase 2: Cancellation-Safe Connection Loop**
- `src/server/connection.rs` — Restructured: added `Incoming` enum, `reader_loop`, `writer_loop` (selects on `background()` for pong handling); `connection_loop` now uses pure channel I/O (no transport generics); all handlers take `&mpsc::Sender<Message>` instead of `&mut T: Transport`; token refresh split into `send_connect`/`handle_response` with ConnectResponse interception and 30s timeout
- `src/server/accept.rs` — Orchestrates transport split + reader/writer task spawning; cancels streams on local connection teardown
- `src/server/cloud.rs` — Same pattern for cloud connections
- `src/cloud.rs` — Split `refresh_and_reconnect` into `send_connect` and `handle_response`; fixed token refresh to use actual `expires_at` from OAuth instead of hardcoded 55 minutes

**Phase 3: Stale Stream Cleanup**
- `src/server/mod.rs` — Added `StreamEntry` struct with `oneshot::Sender<()>` cancellation, `link` field for teardown, `active_streams` and `next_stream_id` fields in `ServerState`
- `src/server/connection.rs` — Extracted `register_stream`, `cleanup_stream`, `cancel_streams_matching` helpers; subscribe handlers register `StreamEntry` with cancellation tokens and link name; spawned tasks `select!` between buffer read and `cancel_rx`; dead route tracking with `HashSet<String>` (first Output failure sends `NoRouteFound`, subsequent suppressed); `NoRouteFound` handler cancels streams via `contains_link` matching (not exact route equality)
- `src/server/routing.rs` — `handle_peer_disconnect` cancels streams by link name and `contains_link`
- `src/route.rs` — Added `first_hop()` and `contains_link()` methods

### Decisions Made

- **Typed `Incoming` enum over encoding errors as Messages**: Avoids conflating transport errors with protocol errors; `Eof` preserves clean-disconnect semantics
- **No generics in connection handlers**: All handlers take `&mpsc::Sender<Message>`, making the code simpler and testable with `mock_tx()`
- **dead_routes HashSet for rate-limiting**: First Output/StructuredOutput failure per route sends NoRouteFound back; subsequent failures suppressed silently. Naturally bounded since dead link names never revive (connections reconnect with new randomised names)
- **Stream cancellation via oneshot drop**: Dropping the `oneshot::Sender` in `StreamEntry` triggers `cancel_rx` in the streaming task — clean, zero-cost signal
- **NoRouteFound matching by `contains_link`**: The dead_route's first_hop is the failed hop name; `entry.dst.contains_link(dead_hop)` catches streams at any depth. Exact route equality was wrong because NoRouteFound carries the traversed src path, not the stream's reply dst
- **Stream link tracking for teardown**: `StreamEntry.link` records which connection spawned the stream. On teardown, all streams for that link are cancelled, ensuring their sender clones are dropped so the writer task can exit
- **WebSocket `background()` for idle pong handling**: `writer_loop` selects on `writer.background()` alongside messages. Default impl pends forever (no-op for TCP/Unix). `WsMessageWriter` awaits pong_rx, sending pongs even when no messages are flowing
- **Token refresh uses actual `expires_at`**: `send_connect` stores `pending_expires_at` from the OAuth connection info; `handle_response` applies it on success

### Verification

- `cargo check`, `cargo fmt`, `cargo clippy` — clean
- 95 unit tests pass (including 2 new route tests for `contains_link`)
- 8 E2E tests pass

---

## 2026-02-10: Fix config default path and sanitize hostnames in link names

### Summary

Fixed two bugs and hardened period validation: (1) config file at `~/.config/amux/config.yaml` was not being read on macOS because the `dirs` crate returns `~/Library/Application Support` as the config dir, bypassing the `~/.config` fallback; (2) periods in hostnames (e.g., `my.laptop.local`) broke route serialization since `.` is the route separator; (3) server now validates proposed link names during handshake, rejecting any containing `.` with an `InvalidLinkName` protocol error.

### Changes

- `src/config.rs` — Replaced `dirs::config_dir()` with explicit XDG logic: check `$XDG_CONFIG_HOME` first, fall back to `~/.config`. Added `xdg_dir()` helper used by both config and state paths.
- `src/state.rs` — Replaced `dirs::state_dir()` with `xdg_dir("XDG_STATE_HOME", ".local/state")` for consistency.
- `src/route.rs` — Added `sanitize_host_name()` that replaces `.` with `-`. `generate_server_link()` now sanitizes the hostname. Added `debug_assert` in `from_link()`/`push()` to catch periods in link names. Added 5 new tests.
- `src/message.rs` — Added `InvalidLinkName` variant to `ProtocolError`.
- `src/server/accept.rs` — `accept_handshake` validates proposed link name for `.` before any other checks, returns `InvalidLinkName` error (fatal, not retried). `connect_handshake` handles `InvalidLinkName` as a fatal error.
- `src/client.rs` — `connect_and_handshake` handles `InvalidLinkName` as a fatal error (no retry).
- `Cargo.toml` — Removed `dirs` dependency (no longer needed).

### Decisions Made

- Use XDG env vars with `~/.config` / `~/.local/state` defaults: matches what we document and what users expect, works cross-platform without platform-specific crate behavior.
- Replace `.` with `-` in hostnames: simple, preserves readability (e.g., `my-laptop-local`), avoids route separator collision.
- `debug_assert` (not hard error) for period validation in `from_link`/`push`: defense in depth without changing public API signatures.
- Server-side `InvalidLinkName` validation is fatal (returns error immediately, no retry loop): a period in a link name is a bug in the client, not a transient condition.

### Verification

- `cargo check`, `cargo fmt`, `cargo clippy` — clean
- 92 unit tests pass (including 5 new route/sanitization tests)
- 8 E2E tests pass

---

## 2026-02-10: AgentRegistry + ResolveAgent + typed agent_id

### Summary

Introduced centralized agent tracking via `AgentRegistry`, added server-side agent resolution via `ResolveAgent` message, and changed `agent_id` from `String` to `Uuid` in all `RoutableMessage` variants. This consolidates scattered agent tracking (local `agents` + `remote_agents` maps) into a single registry with bidirectional alias-UUID mapping, moves identifier resolution from client to server, and enforces type-safe agent IDs throughout the protocol.

### Changes

- `src/agent_registry.rs` — **New.** `AgentRegistry` with `AgentEntry`/`AgentKind` types. Methods: `register_local`, `register_remote`, `remove`, `remove_for_link`, `resolve` (handles `route:id`, UUID, and alias formats), `list_all`, `count_remote`, `iter_entries`. 15 unit tests.
- `src/main.rs` — Added `mod agent_registry;`
- `src/message.rs` — Changed `agent_id: String` → `Uuid` in all 7 `RoutableMessage` variants. Added `ResolveAgent`/`ResolveAgentResult` to `LocalMessage`.
- `src/server/mod.rs` — Added `registry: AgentRegistry` to `ServerState`. Removed `RemoteAgent` struct and `remote_agents` HashMap. `SessionEvent::Ended` removes from registry.
- `src/server/routing.rs` — Removed `resolve_agent` and `remove_agents_for_link`. `create_agent` uses registry for uniqueness checks and registration. `handle_subscribe` takes `&Uuid` instead of `&str`. `handle_peer_disconnect` uses `registry.remove_for_link`. `send_initial_announcements` iterates `registry.iter_entries()`.
- `src/server/connection.rs` — All handlers use `state.agents.get(&agent_id)` directly (Uuid). Added `ResolveAgent` handler. `AnnounceAgent`/`WithdrawAgent` use registry. Updated all tests to use `Uuid` and registry assertions.
- `src/client.rs` — `attach()` sends `ResolveAgent` for server-side resolution. Removed `parse_target()`. `subscribe_and_stream`/`run_attached` take `Uuid` (Copy, no cloning needed).
- `src/transport/unix.rs` — Updated test `agent_id` fields from `String` to `Uuid`.
- `e2e-tests/remote_attach_by_alias.test` — **New.** Tests attaching to a remote agent by alias only (no route prefix), verifying server-side resolution works.

### Decisions Made

- **Server-side resolution**: Moved identifier parsing from client `parse_target()` to server `AgentRegistry::resolve()`. The server has complete knowledge of all agents (local + remote), making it the right place for resolution.
- **First-one-wins for remote aliases**: Remote `register_remote` silently skips if alias is taken by another agent. Local `register_local` errors on conflict. This matches the existing last-write-wins semantics while giving local agents priority.
- **`Uuid` instead of `String`**: Enforces resolve-before-subscribe at the type level. `Uuid` is `Copy`, eliminating many `.clone()` calls in spawned tasks.
- **Re-announce clears old alias**: When the same UUID re-announces with a different alias, the old alias is freed. Prevents stale alias mappings.

### Verification

- `cargo check` — clean
- `cargo fmt` — clean
- `cargo clippy` — clean
- `cargo test` — 87 tests passed (15 new registry tests, 2 new connection tests)
- `cargo run -p e2e-runner -- run` — 8/8 E2E tests passed (including new `remote_attach_by_alias`)

### Next Steps

- Consider increasing the `@@sleep` in `remote_attach_by_alias.test` or adding a synchronization mechanism if the test proves flaky

---

## 2026-02-09: AnnounceAgent / WithdrawAgent — remote agent discovery

### Summary

Added agent propagation between peer connections. When a non-local (peer) connection is established, both sides announce all known agents via `AnnounceAgent` messages. When a peer disconnects, its agents are withdrawn and propagated to remaining peers via `WithdrawAgent`. This enables `list-agents` on one server to show agents running on connected peers.

### Changes

- `src/route.rs` — Added `Route::empty()` for local agent announcements
- `src/message.rs` — Added `AnnounceAgent` and `WithdrawAgent` variants to `LocalMessage`; added optional `route` field to `AgentInfo`; added `remote_agent_count` and `peer_links` to `ServerDebugInfo`
- `src/server/mod.rs` — Added `RemoteAgent` struct, `remote_agents` HashMap and `peer_links` HashSet to `ServerState`; broadcast `WithdrawAgent` on `SessionEvent::Ended`
- `src/server/routing.rs` — Added `broadcast_to_peers`, `send_initial_announcements`, `remove_agents_for_link`, `handle_peer_disconnect` helpers; `create_agent` now broadcasts `AnnounceAgent` to peers
- `src/server/connection.rs` — Added `AnnounceAgent` handler (prepends link, stores in remote_agents, propagates); added `WithdrawAgent` handler (link-match guard, propagates); `ListAgents` now includes remote agents with route; `Debug` includes new fields
- `src/server/accept.rs` — `accept_connection` takes `is_local` parameter; peer connections register in `peer_links` and receive initial announcements; cleanup uses `handle_peer_disconnect`; `tcp_connect` registers as peer
- `src/server/cloud.rs` — `run_cloud_connection` registers as peer and uses `handle_peer_disconnect`
- `src/client.rs` — `list_agents` shows `(via {route})` for remote agents
- `e2e-runner/src/parser.rs` — Added `@@sleep <ms>` directive
- `e2e-runner/src/executor.rs` — Handle `TestStep::Sleep`
- `e2e-tests/remote_list_agents.test` — New E2E test

### Decisions Made

- **`is_local` parameter on `accept_connection`**: Determined by caller context — Unix = local (ephemeral client, no announcements), TCP/WebSocket = not local (peer, gets announcements). Clean separation without transport-type coupling.
- **Last-write-wins for duplicate announces**: `remote_agents` HashMap overwrites on same agent_id. Simple and correct for the current topology.
- **Link-match guard on WithdrawAgent**: Only remove if `stored.link == sender_link`. Prevents stale withdrawals from wrong paths.
- **Local agent takes precedence**: AnnounceAgent for an agent_id that exists locally is silently ignored.
- **`try_send` for peer broadcasts**: Non-blocking send avoids holding the write lock across async operations. Acceptable since peer channels have 256-slot buffers.
- **`@@sleep` E2E directive**: Simple `@@sleep <ms>` syntax for timing-sensitive tests. Needed because async announcement propagation has no synchronization point visible to the test.

### Verification

- 70 unit tests pass (including 6 new: Route::empty, AnnounceAgent/WithdrawAgent serialization, handler tests for store/prepend/skip-local/withdraw-match/withdraw-mismatch/overwrite)
- 7 E2E tests pass (including new `remote_list_agents`)
- `cargo check && cargo fmt && cargo clippy && cargo test` clean
- Manual test: two servers connected via TCP, agent on A visible via `list-agents` on B with `(via host-b)` suffix

---

## 2026-02-08: Architecture docs rewrite + config improvements

### Summary

Rewrote both architecture documents from scratch — the previous versions documented the original Milestone 1 design (flat `Message` enum, `ConnectionId(u64)`, bincode, raw mode) which no longer exists. Also added default config file loading from `~/.config/amux/config.yaml` and an `enforce_tls_in_cloud_mode` config parameter for reverse-proxy deployments.

### Decisions Made

- **Full rewrite over incremental patches**: Documents were so far from current codebase that patching would have been harder to review than starting fresh.
- **Default config failure is a warning (not fatal)**: The file may be partially written; falling back to defaults is safer. Explicit `--config` failure remains fatal.
- **`verify_token` decoupled from TLS**: Previously `verify_token` was `true` only when `tls_acceptor` was `Some`. Now derived from `is_cloud_server`, so cloud mode behind a reverse proxy still validates JWT tokens.

---

## 2026-02-06: Routable/Local message split + generic error forwarding

### Summary

Two major protocol changes. First, restructured the flat `Message` enum into `Message::Routable { src, dst, message }` and `Message::Local(LocalMessage)`. This collapses six separate forwarding arms into one generic forwarding path and encodes routing capability in the type system. Second, added `RoutableMessage::Error(ProtocolError)` so any routable message that can't be forwarded gets a typed error sent back via normal routing, with stale route cleanup.

### Decisions Made

- **Two-variant top-level enum**: `Routable`/`Local` cleanly captures the routing distinction while keeping the wire format simple. Breaking wire format is intentional — the old format mixed routing fields into individual variants.
- **AgentEnded stays Local**: Each server decides how to propagate end-of-session semantics to its own subscribers.
- **Amplification prevention**: If a `RoutableMessage::Error` itself fails to forward, it's logged and dropped rather than generating another error.
- **Stream message error suppression**: Output/StructuredOutput forwarding failures don't send routable errors back — high-frequency stream messages would cause churn without triggering teardown.
- **Conditional stale route cleanup**: When a channel send fails, check `is_closed()` before removing — a new connection may have already replaced the route.
- **Handshake link-name uniqueness**: Moved route insertion into `accept_handshake` so uniqueness check and insert happen atomically under one write lock.
- **Route leak prevention**: If ConnectResponse write fails after route insertion, the stale route is cleaned up before returning the error.
- **Lock hygiene**: Restructured to avoid holding write locks across `.await` points — use scoped read locks for checks, drop before I/O, re-acquire write lock only for mutations.

### Cleanup (same session)

Flattened `LocalControl` wrapper back into top-level `Message` variants. Removed `ConnectionKind` gating (all directly connected clients are equally trusted). Added missing forwarding arms for multi-hop response routing. Extracted then later inlined `forward_to_next_hop` helper as generic forwarding collapsed to one site.

- **Kept `block_in_place` for `ConnectToServer`**: The async type recursion cycle (handle_message → tcp_connect → connection_loop → handle_message) requires breaking the cycle at the type level. `block_in_place` + `block_on` is simplest without boxing.

### Future

- **Agent propagation + route-based cleanup**: `list-agents` only returns local agents. An `AdvertiseAgent` message for peers would need agent→route tracking and purge on route death.
- **WebSocket token validation in cloud mode**: WebSocket connections currently bypass authentication.

---

## 2026-02-02 → 2026-02-04: Cloud mode infrastructure

### Summary

Implemented the full cloud mode stack: OAuth 2.0 device flow authentication, JWT validation with JWKS caching, TLS transport, persistent state management (`~/.local/state/amux/state.yaml`), cloud connection manager with exponential backoff, and server-side cloud mode support (`amux serve --cloud`). Integrated outbound cloud connections into the server using the unified `tcp_peer_loop` pattern with optional token refresh.

### Decisions Made

- **Unix socket always available**: Even cloud servers need Unix socket for local management commands (`list-agents`, `kill-server`). Created unconditionally.
- **No polling for cloud mode**: Instead of polling every 60 seconds waiting for cloud mode to be enabled, `establish_cloud_connection` checks once at startup. Users must restart after `amux init`.
- **Retriable vs non-retriable errors**: Auth failures (`NotAuthenticated`, `Auth`, `CloudDisabled`, `InvalidCredentials`) stop reconnection immediately. Connection errors trigger exponential backoff (1s → 5min max).
- **Generic TcpTransport**: `TcpTransport<S>` generic over stream type — TLS is an implementation detail of connection setup, not the transport layer. Eliminated `TlsTcpClientTransport`/`TlsTcpServerTransport`.
- **verify_token parameter**: Token validation decoupled from TLS. `accept_handshake()` takes `verify_token: bool` — cloud servers pass `true`, local servers pass `false`.
- **Unified peer loop**: Single generic `tcp_peer_loop<T: Transport>` with `Option<TokenRefreshState>`. Uses `std::future::pending()` when None so token refresh branch never fires for non-cloud connections.
- **HostChanged triggers reconnection**: When token refresh indicates a different cloud server, the peer loop returns an error which triggers full reconnection via the auto-connect task.
- **State file with file locking**: `fs2::FileExt` for concurrent access from multiple processes (hook handlers and server).
- **Two separate cloud fields**: `is_cloud_server` (running as cloud relay with TLS+auth) vs `use_cloud_mode` (cloud enabled in state.yaml) — different concepts that were confusing when combined.
- **Serde defaults for Config**: `#[serde(default)]` at struct level allows partial YAML configs while ensuring all fields have values at runtime.
- **Test-only field semantics**: Fields like `randomise_link_name` use `#[cfg_attr(not(any(debug_assertions, test)), serde(skip_deserializing))]` — readable in all builds but only settable via config in debug/test.

---

## 2026-01-21: Link-based stack routing

### Summary

Converted from hierarchical host_id routing (using "/" separator) to link-based stack routing. Routes are `VecDeque<String>` stacks that get popped/pushed at each hop. Before sending through link X: pop X from dst, push X to src. On receive: if dst is empty, process locally; otherwise route to next hop. Replies reverse automatically by swapping src↔dst.

This replaced the earlier hierarchical routing which had prefix-based resolution and a NAT-like scheme where each server prefixed `src_host` when forwarding upstream and stripped its prefix when routing downstream.

### Decisions Made

- **VecDeque for stack**: `push_front`/`pop_front` for efficient stack operations. Top of stack is the front (first element).
- **Dot-separated serialization**: Routes serialize as "AB.BC.CD" with top on left. Compact and readable in logs.
- **Link name generation**: nanoid with lowercase alphanumeric (36 chars). Terminal links `term-{4}`, hook links `hook-{4}`, server links `{hostname}-{4}`.
- **Collision detection with retry**: Clients retry up to 5 times with new random names. With 36^4 = 1.6M possible suffixes, collisions are rare.
- **ProtocolError enum**: Typed errors (`ServerError(String)`, `LinkNameTaken`, `NoRouteFound`) instead of `Option<String>`.

### Protocol rules

1. Before sending through link X: pop X from dst, push X to src
2. src must never be empty — it's the return path for replies
3. For responses: use `Route::reply(incoming_src)` to prepare reply routes
4. For forwarding: manipulate the incoming src/dst, push the outgoing link

### Stack routing example

```
A creates:      dst=[AB,BC,CD]  src=[]
A sends via AB: dst=[BC,CD]    src=[AB]      → B
B sends via BC: dst=[CD]       src=[BC,AB]   → C
C sends via CD: dst=[]         src=[CD,BC,AB]→ D
D receives:     dst=[]         → process locally (src has full return path)
Reply: swap src↔dst, route automatically reversed.
```

---

## 2026-01-15 → 2026-01-18: Hooks, structured logs, and dashboard

### Summary

Built the Claude Code integration layer: hooks system for session start and permission requests, structured log parsing from Claude's transcript files, WebSocket transport with JSON serialization for the React dashboard, and dashboard input via `SubmitInput`. Migrated from bincode to MessagePack (rmp-serde) for binary serialization.

### Key decisions and lessons

- **Bincode → msgpack migration**: Bincode fails with `DeserializeAnyNotSupported` on `#[serde(tag = "...")]` tagged enums. MessagePack with `to_vec_named` (named map format) handles tagged enums and provides forward/backward compatibility across protocol versions.
- **Serde's full power for Claude JSON parsing**: Claude sends `tool_name` + `tool_input` as separate fields. Instead of manual `serde_json::Value` parsing, use `#[serde(tag = "tool_name", content = "tool_input")]` (adjacently-tagged) with `#[serde(flatten)]` to deserialize directly into typed structs.
- **Two input message types**: Raw terminal clients use `InputBytes` for direct byte passthrough. Dashboard uses `SubmitInput` which adds a 20ms delay between text and Enter to ensure Claude Code interprets them as separate events (PTY read boundary semantics).
- **Connection type determines subscription mode**: WebSocket subscribes to structured logs, Unix/TCP subscribes to raw bytes. No new Subscribe variants needed.
- **Separate `MultiplexLogBuffer`**: Logs need entry-count limits, not byte limits, so a separate buffer type was created.
- **Runtime nesting fix**: Hook commands run through `#[tokio::main]`, so creating a nested runtime panicked. Fixed with `tokio::task::block_in_place` + `Handle::current().block_on()`.
- **Hooks fail silently**: Errors logged to `/tmp/amux.log` but exit code 0. Hooks should not block Claude Code workflow.
- **CSI u keyboard protocol**: Modern terminals (iTerm2, kitty, WezTerm) send `ESC[98;5u` for Ctrl-b instead of raw `0x02`. Code detects both for detach.
- **StdinEvent enum over AtomicBool**: Using an enum through the channel lets the main loop react immediately to detach, rather than polling a flag that was never checked because the loop was blocked in `select!`.
- **Keystroke-based permission response**: Claude Code's TUI accepts 1/2/3 for Yes/Yes(all)/No — single character responses.
- **Subscriber leak fix**: Dead subscribers accumulated in `MultiplexBuffer`. Fixed with `subs.retain(|tx| tx.send(...).is_ok())` — combines broadcast and cleanup in a single pass.

---

## 2026-01-13 → 2026-01-15: TCP transport and remote subscriptions

### Summary

Added TCP transport for server-to-server connections, implemented remote agent subscriptions, and evolved the connection handler architecture to its current symmetric form. A client on Server B can attach to an agent on Server A. Fixed a critical mutex deadlock by switching from shared transport access to channel-based message passing.

### Key decisions and lessons

- **Channel-based routing (deadlock fix)**: The original design used `Arc<Mutex<Box<dyn Transport>>>` in the routes table. This caused deadlock: TCP handler holds mutex while blocked on `read_message().await`, Unix client handler tries to acquire mutex to write → blocked forever. Solution: store `mpsc::Sender<Message>` in routes. Each handler owns its transport and uses `select!` to read from transport OR receive from channel.
- **Raw mode removed (premature optimization)**: The raw byte mode optimization for local Unix sockets was removed. Message framing overhead is negligible for local sockets, and consistent message-based protocol simplifies debugging. Can be added back if profiling shows it matters.
- **SubscriptionHandle removed**: Introduced as an abstraction, then removed — it added complexity without clear benefit. Session now exposes `MultiplexReader` and input sender directly.
- **Connect goes through local server**: `amux connect` sends `ConnectToServer` to the local server via Unix socket — the server makes the outbound TCP connection. Keeps connection state managed by the server.
- **Symmetric handler naming**: `unix_accept`/`tcp_accept`, `unix_client_loop`/`tcp_peer_loop`, `unix_handle_message`/`tcp_handle_message`. Handlers kept separate because Unix (local client) and TCP (peer server) serve different roles.
- **Subscribe spawns output task**: When Subscribe succeeds, spawn a task that reads from buffer_reader and sends Output messages via the client's route channel. Main loop continues handling all messages — allows commands while attached.

---

## 2026-01-10: Milestone 1 complete + E2E testing framework

### Summary

Converted the early prototype to the production architecture: message-based protocol with serde/bincode serialization, length-prefixed framing, raw byte streaming after subscribe, multi-client support with replay buffers. Built a declarative E2E regression testing framework with explicit output matching and variable substitution.

### Key decisions

- **CLI design (tmux-style)**: `new-agent -t <name> <command>` and `attach [-t <name>]`. Command is positional to new-agent, not attach.
- **Separate CreateAgent and Subscribe**: Creating an agent and subscribing are separate messages — allows creating without attaching.
- **MultiplexBuffer atomic subscribe (race condition fix)**: Replaced separate `replay_buffer` + `broadcast_tx` with unified `MultiplexBuffer`. The old architecture had a race: data could be lost between getting replay and subscribing to broadcast. Fix: `write()` holds lock during append AND broadcast; `subscribe()` holds lock during snapshot AND registration. Either new data is in the snapshot, OR the subscriber is registered before it's broadcast.
- **AgentType enum**: Type safety ensures only known agent types. `TestAgent(String)` variant excluded from release builds via `#[cfg(any(debug_assertions, test))]`.
- **session_id = agent_id for hook linking**: Pass agent's target name as Claude's `--session-id`, then look up `agents.get(session_id)` when the hook arrives. Replaces fragile `agents.iter().last()` hack.
- **UUID-based agent IDs with alias support**: `agent_id` is auto-generated UUID; `-t` flag sets optional human-readable alias. `resolve_agent()` tries UUID first, falls back to alias scan.
- **E2E explicit output**: Tests show exactly what the terminal shows — PTY echo followed by agent response. More verbose but completely transparent.
- **E2E auto-injection**: Test files use simple `amux` and `test-agent` names; executor injects absolute paths and `--config` flag automatically.

---

## 2026-01-XX: Initial Prototype (Pre-architecture)

### Summary
Initial prototype demonstrating basic PTY multiplexing. Used raw command bytes (0x01=ATTACH, 0x02=LIST, 0x03=KILL) instead of structured messages. Proved out the core concepts but needed restructuring.

### Key Learnings Carried Forward
- `portable-pty` works well for PTY management
- `spawn_blocking` needed for PTY reads (blocking I/O)
- `broadcast::channel` works well for multi-client fan-out
- Child waiter task pattern for clean process lifecycle
- `RawModeGuard` RAII pattern for terminal state restoration

---
