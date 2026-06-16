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

## 2026-06-16: Make local-agents a real seam (LocalAgentHost trait)

### Summary
Replaced the pervasive, implicit `local-agents` boundary with one explicit
abstraction. The core no longer reaches the agent runtime by concrete type;
it depends on a `LocalAgentHost` trait and holds an
`Option<Arc<dyn LocalAgentHost>>` (`None` = the embedded client). The concrete
runtime — sessions, PTY, hooks, lifecycle, suspend/resume, session I/O,
the registry, and its three event sources — moved behind the seam into the
`PtyAgentHost` impl, gated at its module declaration. Every RPC and shutdown
path that used to carry a `#[cfg]` (and usually a hand-written client-only
twin) is now a uniform delegator whose `None` arm is ordinary control flow.

Derived the trait surface from the existing `#[cfg(not)]` stubs (they were the
de-facto "what the core needs without a runtime"); cross-checking surfaced
that the three `EventSource`s and the `SessionEvent` channel were structurally
"core" but only ever fed by runtime code, so they moved into the host too —
that is what makes the boundary clean rather than merely relocated. The
shutdown *orchestration* stays in `server.rs` (so `notify_routing_peers` keeps
interleaving between local-notify and commit); `prepare_suspend` owns the save
and folds prepare/save failures into `Err`, and `server.rs` bails before
notify/commit on `Err`.

Result: `cfg(feature = "local-agents")` sites 87 → 21, and the 21 are all
module declarations, re-exports, construction wiring, or the `routing/host.rs`
capability statement — **no boundary/business-logic gate and no cfg twin
remains**.

### Changes
- `services/agent/host.rs`: new — `PtyAgentHost` + `impl LocalAgentHost`
  (create/rename/delete/send_input/subscribe_session/agent-events/handle_hook/
  resume/stop_all/prepare_suspend/commit_suspend/notify_shutdown/debug_dump).
- `services/agent/state.rs`: new — `AgentServiceState`/`LocalAgentContext`
  registry (moved from `mod.rs`, gated at the `mod` site).
- `services/agent/mod.rs`: `LocalAgentHost` trait + `DebugAgent` (always
  compiled); `AgentServiceCtx` now `{ Option<host>, host_id, is_cloud }` and
  delegates; removed the create/rename/delete twins and moved helpers to host.
- `services/agent/session_rpc.rs`: retargeted `AgentServiceCtx` → `PtyAgentHost`,
  dropped all per-item gates (module gated) and the client-only stub.
- `services/{mod,client}.rs`, `server.rs`, `debug/server.rs`,
  `user_state.rs`, `services/startup/mod.rs`, `testnet/daemon.rs`: route
  through the host; `ServerState` holds `Option<Arc<dyn LocalAgentHost>>`,
  built lazily in `ensure_local_agent_host` (the host spawns a task, so it
  must be constructed inside the runtime, not in `ServerState::new`).
- `debug/server.rs`: consumes owned `Vec<DebugAgent>` from `host.debug_dump`
  (session detail rendered to JSON inside the host) instead of borrowing the
  registry guard.

### Decisions Made
- Host presence = the feature, not the role: `Some` whenever `local-agents`
  is compiled (cloud relays keep an empty registry); cloud-vs-device stays a
  runtime guard. So the host needs only `host_id` to construct.
- Keep suspend's pieces (`prepare`/`commit`/`notify`) as separate trait
  methods: the shutdown orchestration interleaves `notify_routing_peers`, so a
  merged `suspend()` could not preserve ordering.
- `routing/host.rs` capabilities (3 gates) stay: a compile-time capability
  fact consulted where no host handle exists.

### Verification
- `cargo build`/`clippy` (default + `--no-default-features` + `testnet`): clean
  (pre-existing `serve_*_tracked` / embedded-no-default warnings unrelated).
- `cargo test --lib`: 393 (default) / 299 (client-only) passed — including
  `attach_local_agent_events_populates_client_agent_model` (the event-delivery
  bridge through the host).
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check` (embedded, `default-features = false`): clean.

---

## 2026-06-16: Split agent data types from the runtime (seam prep)

### Summary
First chunk of the `local-agents` seam refactor. Separated the agent *data*
types from the *runtime*, so the runtime gates at one module declaration
instead of per item, and removed gates that `lib.rs`'s client-only
`allow(dead_code)` already makes unnecessary. `AgentRecord`, `SessionEvent`,
and `StopPolicy` (plus the `Agent: From<AgentRecord>` impls) moved to a new
ungated `agents/record.rs`; `agents/session.rs` is now pure runtime
(`AgentSession`/`StructuredInputTarget`) gated at `#[cfg] mod session`, with
its 8 internal `local-agents` gates removed. `suspend.rs` references only
ungated data, so its 7 gates were dropped outright — it simply compiles dead
in client-only builds.

### Changes
- `agents/record.rs`: new, holds `AgentRecord`/`SessionEvent`/`StopPolicy`.
- `agents/session.rs`: data types removed; per-item `local-agents` gates
  dropped (module gated at the `mod` site).
- `agents/mod.rs`: `mod record;`, `#[cfg] mod session;`, re-export data from
  `record`.
- `suspend.rs`: removed all 7 `local-agents` gates.

### Decisions Made
- Lean on the existing client-only `allow(dead_code)`: an item needs `#[cfg]`
  only if it would fail to *compile* (names a gated type), not merely if it is
  unused. This is why `suspend.rs` needs no gates.

### Verification
- `cargo build` / `--no-default-features`: clean, no warnings.
- `cargo test --lib`: 393 (default) / 299 (client-only) passed.

### Next Steps
- Introduce the `LocalAgentHost` trait + `PtyAgentHost`; route
  `AgentServiceCtx` and the core consumers through `Option<dyn LocalAgentHost>`.

---

## 2026-06-16: Gate the agent runtime at the AgentSession seam

### Summary
Collapsed the leaf-level `local-agents` gating into a single module seam.
Previously `AgentSession`, `StructuredInputTarget`, and `SuspendedAgent`
each carried a `Disabled` variant in client-only builds, forcing every
match arm into a three-way `#[cfg]` split ending in `unreachable!()`. Now
the agent-runtime types and their impls live entirely behind
`#[cfg(feature = "local-agents")]`: client-only builds don't compile them
at all, so the `Disabled` variants and ~20 `unreachable!()` arms are gone
and `session.rs` / `suspend.rs` read like the original. The data the
client still needs — `AgentRecord`, `SessionEvent`, `AgentType`,
`claude_io` — stays compiled. `AgentServiceCtx` / `AgentServiceState`
remain as the identity/event carrier; only their session-holding
internals (the `local_agents` map, the `lifecycle` module, the
create/rename/delete RPCs) are gated, with `FailedPrecondition` stubs in
the client build.

### Changes
- `agents/session.rs`, `suspend.rs`: gate `AgentSession`,
  `StructuredInputTarget`, `SuspendedAgent` + impls; remove `Disabled`.
- `services/agent/{mod,lifecycle}.rs`: gate the `lifecycle` module and the
  session-holding `AgentServiceState` methods; client-only `create` /
  `rename` / `delete` stubs; added a build-agnostic `local_agent_count()`.
- `services/mod.rs`, `agents/mod.rs`, `server.rs`, `debug/server.rs`:
  gate the suspend/shutdown re-exports and the debug-dump session details.

### Decisions Made
- Gate the type, not the arm: a `#[cfg]` belongs on a module/type/field,
  never inside a match. The `Disabled`/`unreachable!()` pattern was the
  symptom of gating at the wrong altitude.
- Keep `host_id` on `AgentServiceCtx` (Design Y): gating only the
  session-holding internals avoids rippling into the `ClientService` /
  startup / server construction for no functional gain.

### Verification
- `cargo build` / `clippy` (default + `--no-default-features`): clean.
- `cargo test --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check`: clean against the refactored lib.

---

## 2026-06-16: Decouple daemonish behaviors from local-agents; drop client-only marker

### Summary
Reworked the embedded-build gating along the right axes. The "daemonish"
behaviors (peer reachability links, periodic self-update poll, direct
dispatcher TCP listener, local client unix listener, sleep inhibition,
LAN/direct pairing) had been coupled to the `local-agents` compile
feature via a `runtime_profile` shim. Those are *runtime* concerns — a
cloud relay is directly reachable yet hosts no agents — so they now live
on the builder/config axis instead. `spawn_local_background_tasks` takes
a `with_daemon_tasks` flag (desktop daemon `true`, embedded `false`); the
listener/sleep gates revert to the existing `tcp_port` /
`prevent_idle_sleep` config, which already sat in the daemon-only `run()`
path. Deleted the `runtime_profile` module and the redundant
`client-only` marker (an empty feature — `default-features = false` is
the real signal).

### Changes
- `Cargo.toml`: removed `client-only`; documented that `local-agents`
  gates only PTY hosting, not daemonish behavior.
- `server.rs`: `spawn_local_background_tasks(..., with_daemon_tasks)`;
  reverted the sleep / TCP / unix-listener runtime_profile gates.
- `client/mod.rs`, `services/client.rs`, `services/reachability.rs`:
  removed the `runtime_profile` pairing/reachability guards.
- Deleted `runtime_profile.rs`.

### Decisions Made
- Daemonish = runtime, not a feature: keeps cloud-relay/desktop role
  switching in one binary; only `local-agents` (native `portable-pty`)
  earns a compile-time feature.

### Verification
- `cargo build -p amux` and `--no-default-features`: clean.
- `cargo test -p amux --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test -p amux --features testnet --test spec`: 43 passed.

### Next Steps
- Collapse the remaining leaf-level `local-agents` gating: gate the agent
  runtime at the `AgentSession` seam and remove the `Disabled` enum arms
  / `unreachable!()` noise in `session.rs` and `suspend.rs`.

---

## 2026-06-16: Client-only feature gating for embedded iOS builds (first cut)

### Summary
First cut of compile-time gating so the amux lib can build for iOS and
link into the React Native app without spawning local agents. Adds a
`local-agents` default feature (owns PTY-backed Claude/test-agent
spawning via `portable-pty`) plus a `client-only` marker; embedded
builds use `default-features = false`. iOS gets its own state/data/log
paths under the app container, and a `compile_error!` guard rejects iOS
builds that leave `local-agents` enabled. A `runtime_profile` module
centralizes the "is this a local-agent host" decision for listeners,
reachability, sleep inhibition, and periodic update checks.

### Changes
- `Cargo.toml`: `local-agents` (default) + `client-only` features;
  `portable-pty` now optional.
- Feature-gated the agent runtime (`agents::{session,pty,hook,...}`,
  `suspend`, `services::agent` hosting) with `Disabled` enum arms for
  the client build.
- `paths.rs`: iOS app-container state/data/log paths.
- `runtime_profile.rs`: centralized daemonish gates.
- `lib.rs`: `VERSION`/`PROTOCOL_VERSION` exports, iOS compile guard.
- `services/client.rs`: `CreateAgent` now checks the target host's
  supported agent types precisely (`FailedPrecondition` instead of a
  generic `Unreachable`).

### Verification
- `cargo build -p amux --no-default-features`: clean (client-only).
- The app bridge (`amuxapp/modules/amux-core`) builds against this with
  `default-features = false, features = ["client-only"]`.

### Next Steps
- Simplify: collapse the leaf-level gating to a single module seam, move
  the daemonish behaviors onto the builder/config runtime axis, and
  delete `runtime_profile` and the redundant `client-only` marker.

---

## 2026-06-11: Stop the pre-existing Windows test hang from eating 6-hour runners

### Summary
With the clap failure fixed, the Windows test leg ran further and hit a
hang that PREDATES the protocol rework (the 2026-06-07 run on old main
was killed at the same 6-hour limit): `embedded_shutdown_…` and
`embedded_suspend_…` never return on Windows — agent PTY teardown hangs
under ConPTY, the same class of issue that already keeps the Windows e2e
leg disabled. Both tests are now `#[cfg_attr(windows, ignore)]` with the
reason inline. And the "tests always run with a timeout" rule now applies
to CI itself: every job has `timeout-minutes` (10–30), so a future hang
fails in minutes instead of silently burning a 6-hour runner.

### Verification
- `cargo test -p amux --test embedded`: 10 passed (macOS).
- Windows leg verified by the subsequent CI run.

---

## 2026-06-11: Fix Windows CLI arg conflicts referencing the unix-only --via-ssh

### Summary
CI's windows-latest test leg caught five failing `amux pair` arg-parsing
tests: `qr`/`listen`/`connect` listed `via_ssh` in `conflicts_with_all`,
but `via_ssh` is `#[cfg(unix)]`-only, so on Windows clap's debug
assertion fired for a conflict against a nonexistent argument. The
conflict is now declared via `#[cfg_attr(unix, arg(conflicts_with =
"via_ssh"))]` on each of the three args. macOS-only local verification
could not have caught this; the CI matrix did its job.

### Verification
- `cargo test -p amux-cli`: 43 passed (macOS); Windows leg verified by
  the subsequent CI run.

---

## 2026-06-11: Protocol version resets to 1

### Summary
amux is unreleased and the wire owes nothing to history:
`PROTOCOL_VERSION` resets from the internal working number to **1** —
the protocol as designed is the first one that will ever ship. The
internal version labels scrubbed from code comments, docs, and this log
in favor of plain language ("the protocol rework", "the earlier
route-list routing").

### Verification
- Full local CI parity run (fmt --check, CI clippy, workspace build, lib
  tests, spec suite, e2e suite) before rebasing onto main.

---

## 2026-06-11: Docs housekeeping — three docs, repointed instructions, compacted log

### Summary
docs/ now holds exactly three files, one per audience: PROTOCOL.md (the
wire, for implementers), ARCHITECTURE.md (the system, for developers),
HOW_IT_WORKS.md (the mental model + trust story, for the documentation
website). The investigations and ops material moved to gitignored notes/
(deployment, transcript-redaction ×2, session-tracking research).
CLAUDE.md repointed at the three docs + the spec suite, and now carries
the working conventions (timeout-wrapped tests, DEVLOG-per-chunk, no
trailers, docs-vs-notes). The spec suite's main.rs module doc absorbed
the "how to add a test" guide, retiring notes/SPEC_TESTS_DESIGN.md;
notes/REFACTOR_PROGRESS.md (migration complete) deleted. DEVLOG
compacted: June 2026 entries (spec suite + protocol rework + docs) kept verbatim, the
~100 older entries summarized into era paragraphs — full text remains in
this file's git history.

### Changes
- docs/: transcript-redaction-spec/-feasibility, research-claude-session-
  tracking untracked and moved to notes/ (deployment.md was never
  tracked; moved too).
- CLAUDE.md rewritten; tests/spec/main.rs module doc expanded.
- notes/SPEC_TESTS_DESIGN.md, notes/REFACTOR_PROGRESS.md deleted.
- DEVLOG.md: 3,700 → ~840 lines.

### Verification
- Spec suite recompiled and green after the main.rs edit; CI clippy
  clean.

---

## 2026-06-11: Protocol spec grows its survivors; the website one-pager lands

### Summary
docs/PROTOCOL.md absorbed the three protocol-level survivors of the v5
spec — the SPAKE2 wire crypto (RFC 9382/edwards25519, transcript hash,
HKDF infos, sealed identities, opaque INVALID_PIN), the
versioning-is-equality rule, and the QR payload shape — plus a new "Why
it is shaped this way" section folding the load-bearing rationale from
the protocol-rework decision walkthrough (stateless relays, one proxy hop, only-Open-
allocates, no housekeeping acks, tunnels die with their link, deliberate
double encryption, PAKE over bearer tokens). With that folded, the
protocol-decisions working note is deleted; rejected-alternative history
lives in the June DEVLOG entries. New
docs/HOW_IT_WORKS.md: the human-readable one-pager for the documentation
website — mental model (devices not accounts, pairing as a one-time act,
links carry / tunnels authenticate) and the trust story (what a relay
structurally cannot do, local revocation, missing-by-design, executable
spec).

### Changes
- docs/PROTOCOL.md: pairing crypto paragraph, version rule, rationale
  section, header repointed (now ~150 lines).
- docs/HOW_IT_WORKS.md: new.
- the protocol-decisions working note deleted (gitignored; content folded).

### Verification
- Docs-only change; spec suite green on the prior commit re-verified
  (43 passed / 0 ignored).

---

## 2026-06-11: Replace the v5 networking spec with an architecture doc

### Summary
The tombstone chain is gone: `docs/NETWORKING.md` (the superseded 3,202-line
v5 spec), `docs/NETWORKING_PROGRESS.md` (its work ledger), and the three
pointer stubs (`NEW_ARCHITECTURE.md`, `architecture.md`,
`cloud_architecture.md`) are deleted; git history keeps them. In their
place, `docs/ARCHITECTURE.md` — a current-design system doc complementing
PROTOCOL.md: PROTOCOL.md owns the wire, ARCHITECTURE.md owns the system
(process/deployment shapes, identity & trust store, the two-server model,
the dispatcher's classification table, the service surface map, the
multi-tenant cloud deployment, and the LinkRegistry / RoutingCore /
TunnelPool / ConnectionManager layering). Disposition of NETWORKING.md's
material: §3–4 (threat model, identity, trust, two servers, dispatcher,
tenancy), §7 (CLI shape), §8.4/8.12/8.13, and the resource caps were
rewritten for the new design into ARCHITECTURE.md; §4.8, §5–6, §8.5–8.11 were
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
  `testnet/daemon.rs` no longer has a caller — revocation now breaks
  streams instead of stalling them — and could be deleted.

---

## 2026-06-11: e2e runner catches up with the reworked config

### Summary
All 14 e2e tests failed after the protocol-rework chunks with one cause: the runner's
generated `local.yaml` still set `randomise_link_name`, deleted with wire
link names in chunk 2 (the config parser rejects unknown fields). Removed
the field from the template in `e2e-runner/src/executor.rs`.

### Verification
- `cargo run -p e2e-runner -- run`: 14 passed, 0 failed.

---

## 2026-06-11: protocol rework complete — docs graduate

### Summary
Closing docs pass for the protocol-rework implementation (chunks 1–5, commits
6735df6 → ccd7a25). `docs/PROTOCOL.md` graduates from "target design" to
the implemented spec, locked in by the prose suite in
`crates/amux/tests/spec/`. `docs/NETWORKING.md` is marked superseded
(v5, historical) with a banner saying exactly what the rework replaced and that
PROTOCOL.md + the spec suite win where they disagree.

### Changes
- `docs/PROTOCOL.md`: status header → implemented.
- `docs/NETWORKING.md`: supersession banner.

### Verification
- Final state on the rework branch: lib 394 passed; spec suite 43 passed /
  0 ignored; CI clippy clean; workspace build clean. The wire
  `Message.body` oneof matches PROTOCOL.md's vocabulary verbatim:
  Hello · HelloAck · NeighborUp · NeighborDown · TunnelOpen · TunnelData
  · TunnelClose · Reauth · LinkClose, plus `PairingService.Pair`;
  `PROTOCOL_VERSION = 6`.

### Next Steps
- Decide the fate of NETWORKING.md's still-accurate material (identity,
  trust store, two-server model, dispatcher): fold into PROTOCOL.md
  companions or rewrite as a current reference.
- §6 ledger follow-ups: move route activation's TLS handshake off the
  ConnectionManager events task (§6.12); `last_dial_error` changes push
  no event to subscription-only UIs (D15 note).

---

## 2026-06-11: protocol-rework chunk 5 — fire-and-forget Reauth; reachability shrinks to two fields

### Summary
The final protocol-rework code chunk: D12 (P5) and D15 (P7). Credential refresh is now
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
if anyone cares. This completes the reworked wire vocabulary: `Hello`/`HelloAck`
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
- Protocol-rework implementation complete (chunks 1–5: D10/D11 → D2/D13/D14 → D3a/P8 →
  D9 → D12/D15). Graduate the remaining doc work: PROTOCOL.md is current;
  NETWORKING.md still describes v5 and is superseded where they overlap.
- Ledger note: the connector trusts the refresher's `expires_at`
  unconditionally — a refresher that keeps minting tokens already inside
  the 5-minute refresh window drives back-to-back refreshes (pre-existing
  shape, now without even an ack to pace it).

---

## 2026-06-11: protocol-rework chunk 4 — one pairing protocol (SPAKE2), two secret deliveries

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

## 2026-06-11: protocol-rework chunk 3 — every call is a tunnel, with an explicit lifecycle

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
- Additional flips, all structural consequences of the rework, called out here:
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

## 2026-06-11: protocol-rework chunk 2 — route by host id with adjacency-only events

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
- Harness/spec: `WirePeer` speaks the new handshake; chain tests rewritten —
  `endpoints_call_each_other_through_a_chain_regardless_of_dial_direction`
  (the §6.6 pair collapsed; dial direction no longer matters) and
  `presence_reaches_exactly_two_hops_along_a_chain` (catalog 28b inverted:
  three hops out is deliberately invisible).

### Decisions Made
- Pre-planned spec flips only: 28b inversion, §6.6 collapse, handshake-
  snapshot wire tests. Everything else green with mechanical updates.
- Lib tests that asserted old observables were re-pointed at current ones:
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

## 2026-06-11: protocol-rework chunk 1 — delete preserve_tunnel_id and GoAway drain; rename GoAway → LinkClose

### Summary
First implementation chunk of the protocol rework: the two pure deletions (D10/P3,
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
  immediate re-pair over the same relay can black-hole — the new reply rule
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

## 2026-06-11: Protocol rework one-pager

### Summary
Concluded the protocol-simplification walkthrough (all proposals from the
networking review resolved) and wrote `docs/PROTOCOL.md` — the target
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
- Full rationale recorded in the protocol-decisions working note (D1–D15,
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
  the rework lands are pre-recorded in the decisions notes).

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

## Earlier history (compacted 2026-06-11)

Entries below this point were compacted into era summaries; the full
entries remain in this file's git history (any commit before the
compaction).

### 2026-05 → 2026-06-08: Networking & Security v1 (protocol v5)
The full v5 networking architecture landed directly as commits (devlog
gap): gRPC `RoutingService.Connect` bidi links, route-list stack routing,
`TunnelFrame` tunnels carrying end-to-end pinned mTLS, SPAKE2 + token
pairing, the trust store, the two-server model with dispatcher
classification, and the multi-tenant cloud relay (TLS+JWT). Anchor
commits: fd02e8e ("Implement v1 routing and service architecture"),
681ee49 ("Implement amux Networking & Security v1"), 707f620. All of it
was specified in docs/NETWORKING.md (since deleted) and superseded by
the protocol rework — see the June entries above.

### 2026-05-15: Embedded-library refactor
`amux` reshaped into a library for embedded clients plus `amux-ui` (a
reactive client runtime); public API surfaced deliberately; cleanup pass
behind it.

### 2026-04: Hardening and idiomatic-Rust era
Wire enums reshaped for non-Rust clients with forward-compatibility
`Unknown` variants; per-client minimum-version enforcement; CLI
ergonomics (`server` subcommand, positional attach, suspend/resume);
pre-release security hardening; a multi-pass idiomatic-Rust refactor and
domain reshuffle (opaque hook handling, facade cleanup, server-giant
splits); init flow + cloud-state reshape; negotiated idle timeouts for
heartbeats; subscription leases; `amux debug` rework.

### 2026-03: Agents and sessions era
Workspace split into crates with a public API; `AgentSession` enum +
`PtyHandle`; suspend/resume with persisted state; `amux update`;
graceful shutdown; Windows support behind a platform abstraction; the
Claude hook-event family (PreToolUse/PostToolUse/Notification/Stop) and
the AskUserQuestion keystroke work; structured I/O sequence numbers;
external session capture (fork-and-swap); protocol reset to v1 with
handshake extraction and per-user sockets.

### 2026-02: Custom-protocol era
Cloud mode over WebSocket + MessagePack; the protocol-v3 restructure
(command enum, opaque payloads, request_id, route management,
SubscriptionClosed); agent/host discovery (AnnounceAgent/AnnounceHost);
user multi-tenancy (`ServerUserState`); the Claude Code plugin with
auto-install hooks; the tracing migration and the zero-clippy policy;
TCP keepalives; protocol version checking on Connect.

### 2026-01: Prototype era
The initial PTY-multiplexer prototype; milestone 1 with the e2e testing
framework; TCP transport and remote subscriptions; hooks, structured
logs, and the dashboard; link-based stack routing — the design whose
relay statefulness the rework finally resolved by routing on host ids.
