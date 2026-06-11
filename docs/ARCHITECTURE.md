# The amux system architecture

**Status**: current (2026-06-11). This document describes the system —
processes, servers, trust machinery, service surfaces, and internal
layering. Its companion, [`PROTOCOL.md`](./PROTOCOL.md), owns the wire:
links, frames, tunnels, the routing rules, and the pairing flow. When this
document and the code disagree, the code and the spec suite
(`crates/amux/tests/spec/`) win.

## Processes and deployment shapes

Everything is one binary. `amux server start` runs a **device daemon**;
`amux server start --cloud` runs the same `Server` as a **cloud relay**
(`ServerMode::CloudRelay` in `server.rs`). The split is constructed, not
configured at runtime: a device daemon loads its identity and trust store
from disk and starts the full user-services stack; a cloud relay mints a
throwaway `host_id`, loads no device identity, and starts only the
JWT-gated `LinkService` — it structurally has nothing else to serve.

Around the daemon sit its clients and consumers:

- **CLI** (`crates/amux-cli`): every user-facing command talks
  `ClientService` over the daemon's local Unix socket. Two hidden
  subcommands are protocol plumbing rather than user surface: `amux relay`
  bridges its stdin/stdout to the local socket (the receiving end of an
  SSH link), and `amux pair-recv` runs the responder side of an SSH
  pairing identity exchange.
- **UI runtime** (`crates/amux-ui`): a reactive client library over the
  same `ClientService` surface, for embedding in apps.
- **Test harnesses**: the `testnet` feature compiles an in-process harness
  (`amux::testnet`) that builds whole daemons — real identities, real
  trust stores, real localhost TCP with device mTLS, an optional
  in-process cloud relay — for the spec suite, plus `WirePeer`, a scripted
  protocol actor for wire-conformance tests. `crates/e2e-runner` drives
  real compiled binaries end to end.

A daemon owns two listeners. The **local Unix socket**
(`Config.socket_path`, mode `600`) is always on and carries local
clients. The **external TCP listener** (`tcp_port`) is off by default —
the user opts in for LAN-direct reachability — and feeds the dispatcher
described below. Outbound, the daemon dials the cloud (TCP + WebPKI TLS +
JWT) and re-dials the direct reachabilities recorded in its trust store.

## Identity and the trust store

Each device generates, on first run, an Ed25519 keypair and a random
128-bit `host_id`, persisted in the data directory
(`$XDG_DATA_HOME/amux`, falling back to `~/.local/share/amux`):

- `device.key` — the private key, PKCS#8 v1 DER, mode `600`. It never
  leaves the device.
- `host_id` — 16 raw bytes, mode `600`. Independent of the key, so a
  future key rotation can preserve the device's stable identifier.
- `trust.json` — the trust store, mode `600`.

All three are written atomically (write-temp-then-rename). The daemon's
non-secret runtime state lives separately under
`$XDG_STATE_HOME/amux/state.yaml`.

The trust store (`trust.rs`) maps
`host_id → { pubkey, name, paired_at, reachabilities }`. It is the entire
trust model: a pinned pubkey is what lets a peer's mTLS handshake
terminate into the trusted services. Entries are added only by successful
pairing and removed only by local revocation (`amux unpair`); no inbound
protocol message can mutate trust. The store is local-only — never sent
to the cloud, never synchronized between devices.

`reachabilities` is not trust; it is the list of **dialer-responsibility
markers** this device learned as an initiator: `Cloud`, `DirectTcp { addr }`
(the listener address it dialed), or `Ssh { target }` (the verbatim string
handed to `ssh`). Re-establishment is always the dialer's job: on startup
the `ReachabilityLinkConnector` (`services/reachability.rs`) walks the
store and dials every `DirectTcp`/`Ssh` entry; `Cloud` entries need no
action because the cloud connector brings up that link separately. The
acceptor side of a pairing records no reachability it didn't dial — an
accepted socket's source port is not a reusable address. A trusted peer
with an empty list is a peer we trust but have no stored way to reach;
it shows up offline until it dials us.

## The two-server model

A device daemon runs two long-lived tonic servers
(`services/startup/mod.rs`), each fed an mpsc stream of accepted
connections:

| Server | Hosts | Fed by |
|---|---|---|
| **Trusted Server** | `ClientService`, `AgentService`, `LinkService` | Local Unix socket; pinned-mTLS streams from the dispatcher |
| **Pairing Server** | `PairingService` | Anonymous-TLS streams from the dispatcher, only while a pairing window is open |

The split exists so that authorization is decided **once, at connection
admission**, in one place. A connection lands on a server with a fixed
service set; the services it cannot reach do not exist on its connection,
so an unauthenticated peer cannot even probe for them. Per-RPC
interceptors were considered and rejected (auth scattered across N
services), as was a server-per-connection (needless lifecycle churn).
The one deliberate exception to "admission decides everything" is the
local-admin gate inside `ClientService` (below).

## The dispatcher

`dispatcher.rs` is the single admission point for every inbound stream
that needs a TLS handshake. Two sources feed it: sockets accepted on the
external TCP listener, and inbound tunnels that terminate at this daemon
(the `TunnelPool` hands each one over as a byte stream). Both get the
same treatment — the dispatcher always presents the device's self-signed
certificate and *requests* a client certificate:

| Handshake outcome | Authority granted |
|---|---|
| Client cert pinned in the trust store | Trusted Server, bound to that peer's `host_id` |
| Client cert not pinned | Rejected during the handshake (`PinnedClientCertVerifier`) |
| No client cert, pairing window open | Pairing Server (no trust-side authority) |
| No client cert, no pairing window | Closed |

The `host_id` bound at the handshake is load-bearing: a `Hello` whose
`host_id` contradicts the mTLS-bound identity is rejected, so a paired
peer cannot impersonate another peer at the link layer, and tunnel-borne
calls carry the authenticated peer identity into the services.

The local Unix socket bypasses the dispatcher entirely: arrivals there
are classified `LocalTrusted` and feed the Trusted Server directly, with
OS file permissions as the gate. SSH is deliberately **local-equivalent**:
`amux relay` bridges the SSH stream into that same socket, so anyone who
can SSH into the daemon's account already has what the socket grants —
that is the existing OS trust boundary, not a new one. Peer *calls* still
authenticate uniformly: every call rides a tunnel, and every tunnel runs
a pinned mTLS handshake at its terminating dispatcher, whatever transport
its frames crossed (an SSH link confers no call authority by itself).

The external listener defends itself: TLS handshakes are rate-limited
per source IP (10/minute, sliding window), capped at 128 concurrent, and
timed out after 10 seconds (`resource_limits.rs`, `dispatcher.rs`).

## Service surface map

| Service | Where it lives | Who may call it |
|---|---|---|
| `ClientService` | Trusted Server | Local clients over the Unix socket / in-process; paired peers over tunnels, **minus the local-admin RPCs** |
| `AgentService` | Trusted Server | Local clients and paired peers (this is what remote sessions ride) |
| `LinkService.Connect` | Trusted Server, and the cloud relay | Adjacent nodes establishing a link, over any link transport |
| `PairingService.Pair` | Pairing Server | Anonymous-TLS callers during an open pairing window |

`ClientService` is the client API: host and agent inventory and
subscriptions, agent CRUD, session attach/input, hooks, debug,
shutdown/suspend/resume, and the pairing/trust administration RPCs.
Pairing is the trust boundary — a paired peer has full runtime authority,
including disruptive operations — with exactly one carve-out:
**trust mutation and pairing administration are local-only**.
`StartPairing`, `GetPairingStatus`, `CancelPairing`, `PairPeer`,
`PairPinCloudPeer`, `PairQrCloudPeer`, `ListPeers`, `GetPeer`, and
`Unpair` check the connection's admission class
(`require_local_admin_client` in `services/client.rs`) and refuse anything
that is not `LocalTrusted`. A remote peer can use your agents; it cannot
grow or shrink your trust store.

Host inventory is similarly scoped. `ListHosts(PAIRING_CANDIDATES)` —
untrusted-but-online cloud hosts the user might want to pair — is served
to local callers only; remote callers are refused the scope and never see
untrusted hosts in normal inventory either. Each `HostEntry` carries
`online` (routing-derived presence) and `last_dial_error` (the most
recent failed dial, cleared when a route comes up); nothing probes, so
"unknown" is simply `!online` with no recorded error.

`AgentService` is what tunnels exist for: a peer lists another daemon's
agents, attaches to a session, and round-trips terminal I/O end to end —
through the cloud relay if that is the only shared path, with the relay
seeing ciphertext.

## The cloud deployment

The cloud relay is multi-tenant and minimal. It serves exactly one thing:
`LinkService.Connect` behind a JWT interceptor
(`LinkAuthInterceptor` → `JwtCloudLinkAuthenticator`), on a TCP listener
wrapped in ordinary WebPKI server-auth TLS (certificate and key supplied
via `AMUX_TLS_CERT` / `AMUX_TLS_KEY`). Devices validate the relay's
hostname certificate like any public endpoint and authenticate themselves
with a JWT from the OAuth device flow; the fire-and-forget `Reauth`
refresh keeps a healthy link undisturbed across token expiry.

Tenancy is per-user by construction: `CloudLinkService` holds one
routing-services instance (`RoutingCore` + `TunnelPool` +
`ConnectionManager`) per authenticated `user_id`, created on first link
and shared by that user's devices. Adjacency events fan out only within a
user's instance, so presence is scoped per user, and frames are only ever
forwarded between one user's devices. Two devices logged into different
cloud users cannot see or reach each other through the relay — they can
still pair and connect via LAN or SSH, which never involve the cloud.

What the cloud structurally cannot do follows from what it doesn't have.
It holds no device identity and is pinned in nobody's trust store, so it
can never terminate a tunnel into anyone's trusted services: it cannot
create agents, read session traffic, or impersonate a device. Its own
incoming-tunnel sink discards everything
(`spawn_discard_incoming_tunnels_task`) — there is no service behind the
relay to tunnel into. Pairing traffic crossing the relay is opaque
ciphertext like everything else; the cloud is never told pairing is
happening. What it *does* see is metadata: `host_id`s, JWT-derived user
ids, names and capabilities from handshakes it relays, online status, and
traffic volume/timing.

A device daemon bounds the blast radius of a compromised relay. Inbound
tunnel opens arriving over the cloud link are rate-limited (30/minute,
sliding window — excess frames are dropped while the link stays up), and
the routing table caps untrusted hosts at 1000 with oldest-inactive
eviction; trusted peers are exempt and never evicted
(`resource_limits.rs`, `routing/core.rs`). A self-hosted relay needs none
of this machinery explained separately: relaying is something every node
can do, so an always-on paired peer is a relay with a pinned key.

## Internal layering

The daemon's networking internals are four components under the services,
each with one job:

**`LinkRegistry`** (`routing/link_registry.rs`) — the daemon's live links:
`LinkId → writer` for every established link, each writer feeding frames
into that link's `Connect` stream. It is the single source of truth for
*wire* adjacency, and the adjacency-only advertising rule is structural
here: every `NeighborUp`/`NeighborDown` a peer ever receives from us is
emitted by the registry, under one lock, in registration order — there is
no API for broadcasting anything else. Registering a link also reconciles
the handshake's neighbor snapshot atomically, closing the gap between
composing a snapshot and the link going live.

**`RoutingCore`** (`routing/core.rs`) — the routing table: our own direct
adjacency (`directs`) and our neighbors' adjacency claims (`claims`).
Presence falls out as a derivation — a host is online if we are adjacent
to it or some neighbor claims to be — which is why presence reaches
exactly two hops. `best_route` answers `Direct(link)` or `Via(relay)` and
nothing longer. The untrusted-host cap lives here.

**`TunnelPool`** (`tunnel/pool.rs`) — endpoint state for tunnels this
daemon initiates or hosts, plus the relay forwarding rule. Only a
`TunnelOpen` allocates state; data for an unknown id is dropped without
allocation; closes are sent proactively on teardown. Forwarding consults
only the `LinkRegistry` — a frame for `dst` is forwarded iff a direct
link to `dst` exists — and keeps no per-tunnel state for relayed traffic.
Terminating tunnels are surfaced as byte streams to the dispatcher, which
runs the pinned mTLS handshake inside them.

**`ConnectionPool` / `ConnectionManager`** (`connection.rs`) — outbound
channel selection over the two route shapes. There is exactly one
materialization path: every peer call rides a tunnel, opened over a
direct link (`dst = peer`, zero relays) or over a relay link. The pool
caches one tonic channel per `(peer, route)`; the manager subscribes to
routing events, keeps one active route per peer, prefers the link itself
over any relay path, and swaps make-then-break — a cached channel is only
as alive as the link under it, and a broken stream is the caller's signal
to reconnect over whatever is now best. One learned guard: a route whose
target advertises itself as a cloud relay is recorded but never eagerly
tunneled into, because relays discard inbound tunnels and the handshake
could only time out.

The **dispatcher** ties the inbound half together, and `services/startup/`
wires all of it: routing services first, then the two servers, the
listeners, the reachability connector, and (for cloud-attached daemons)
the cloud link connector.

## What is deliberately deferred

Key rotation and identity recovery (today: re-pair from a surviving
peer), per-peer or per-method authorization beyond the local-admin split,
LAN auto-discovery, and OS-keychain storage for the device key. Pairing
remains the trust boundary for all of them.
