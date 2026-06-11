# amux Networking & Security

**Status**: Superseded (v5 spec, historical). The implemented protocol is
**v6**, specified in [`PROTOCOL.md`](./PROTOCOL.md) and locked in by the
spec test suite (`crates/amux/tests/spec/`). v6 replaced this document's
routing (route lists → host-id adjacency), tunneling (implicit opens →
`TunnelOpen`/`TunnelData`/`TunnelClose`), pairing (PairByToken deleted —
SPAKE2 only), drain/ack machinery (deleted), and reachability surface
(three-state → `online` + `last_dial_error`). The identity, trust-store,
two-server, and dispatcher material remains broadly accurate but is no
longer authoritative; trust `PROTOCOL.md` and the spec suite where they
disagree.

This document defines the network and security model for amux. It is
written as a complete specification that a developer or AI agent can
implement against. Subsequent revisions will cover key rotation,
recovery, client authorization, and related concerns
(see §11).

The active overall architecture is described in
[`docs/NEW_ARCHITECTURE.md`](../docs/NEW_ARCHITECTURE.md). This document
layers security and routing on top of that architecture.

## Contents

1. Motivation
2. Glossary
3. Overview
4. Concepts
5. Pairing flows
6. PairingService proto
7. CLI
8. Routing & connections
9. Testing
10. Invariants
11. Deferred to future revisions
12. Reference implementation map (non-normative)

---

## 1. Motivation

The previous model leaves transport security to the user and relies on
the cloud being trusted with routing and identity. This works as a proof
of concept but is unsuitable for production: a compromised cloud relay
would expose every user's devices.

This spec shifts to an end-to-end model with a peer routing graph:

- The cloud becomes a **dumb byte relay**. It can no longer read
  host-to-host traffic content, impersonate hosts, or vouch for trust.
- All host-to-host traffic is encrypted and mutually authenticated with
  **mTLS using pinned device pubkeys** (or SSH for SSH-paired peers).
- Trust is established **out-of-band via explicit pairing** between two
  devices. Pairing is a one-time bootstrap; subsequent connections use
  the pinned pubkeys directly.
- Every direct daemon-to-daemon connection becomes a **Link** in a
  shared routing graph. Hosts learn of each other via propagated
  `HostUp`/`HostDown` events. The cloud is no longer architecturally
  special — it is just one node that happens to be multi-tenant and
  high-availability.

**Scope of this spec**:

- Device identity (keypair, `host_id`)
- The trust store
- Three pairing flows (QR, PIN, SSH)
- `PairingService` proto
- Transport security (mTLS for network paths; SSH wraps its own)
- Routing graph: Links, routes, `HostUp`/`HostDown` propagation
- Connection lifecycle: `ConnectionPool`, `ConnectionManager`, swap policy
- `TunnelId` and tunnel framing for multi-hop traffic
- CLI surface for pairing

**Out of scope** (see §11):

- Key rotation, identity recovery
- Per-peer / per-method authorization beyond the v1 local-admin split
- Cloud ↔ pairing UI interaction
- mDNS / LAN auto-discovery

---

## 2. Glossary

| Term | Meaning |
|---|---|
| **`host_id`** | A random 128-bit (16-byte) identifier for a device, generated on first run, persisted locally. Independent of the device's pubkey. |
| **device keypair** | A long-lived asymmetric keypair generated on first run. Private key never leaves the device. The pubkey is the device's cryptographic identity. |
| **trust store** | A local registry on each device mapping `host_id → (pubkey, name, paired_at, reachabilities: Vec<Reachability>)`. The source of truth for trusted identities; v1 persists local outbound reachability hints beside those identities. |
| **pairing** | A two-step bootstrap: (1) OOB-verified exchange of cryptographic identity, (2) mutual pinning of pubkeys in trust stores. |
| **pairing mode** | A time-bounded local state on a responder daemon: "for the next ~5 minutes, accept incoming pairing attempts authenticated by this PIN or token." |
| **OOB channel** | Out-of-band channel — anything an attacker cannot sit in the middle of. QR scan, SSH stream, user-typed PIN. |
| **PAKE / SPAKE2** | Password-Authenticated Key Exchange. A protocol family where two parties with a low-entropy shared secret (e.g., a 6-digit PIN) derive a high-entropy shared session key, resistant to offline brute-force. |
| **mTLS** | Mutual TLS — both sides present certificates during the TLS handshake; both sides verify the other's. |
| **Link** | A bidi `RoutingService.Connect` stream between two adjacent daemons over a single underlying transport (cloud-relay TCP+TLS, paired-direct TCP+mTLS, or paired-SSH stdio). Every direct daemon-to-daemon connection is a Link. A Link has a name assigned at Hello/HelloAck. |
| **route** | An ordered sequence of link names describing how to reach a host from this daemon. The first link is the next hop. Each forwarding hop pops one link from the route and forwards onward. |
| **direct connection** | A 1-hop runtime connection between two daemons — paired-direct (TCP+mTLS or SSH-stdio) or, from the daemon's perspective, the cloud-relay link. The underlying socket hosts a tonic Channel directly; no tunnel framing. |
| **tunnel** | A logical duplex byte stream between two daemons over a multi-hop route (route length ≥ 2). Carried as `TunnelFrame`s inside the first-hop Link's `RoutingService.Connect` Message envelope; opaque to intermediate hops. Identified by `TunnelId { initiator, nonce }`. End-to-end mTLS sits *inside* a tunnel between the two endpoints, so intermediaries cannot read the gRPC payload. |
| **`TunnelTransport`** | A tunnel's user-facing surface — an `AsyncRead + AsyncWrite` byte stream that tonic wraps into a gRPC Channel. |
| **`ConnectionPool`** | A daemon-internal registry: `Route → tonic Channel`. Dumb storage; materialization is owned by the connection-establishment code. |
| **`ConnectionManager`** | A daemon-internal policy layer: tracks the `active_route` per peer; subscribes to routing events; decides when to swap. |
| **active route** | The route currently in use for outbound calls to a particular peer. Chosen by `ConnectionManager` via shortest-route-wins (FIFO ties). |
| **single-tenant server** | A device daemon serving exactly one user. Holds one `ServerUserState`. |
| **multi-tenant server** | The cloud relay, serving many users. Holds `ServerUserState` keyed by `user_id`. |

---

## 3. Overview

There are **two distinct authentication concerns** in the system:

1. **Cloud authentication.** Devices authenticate to the cloud using JWTs
   (the existing flow). This proves "this connection is from a user logged
   in as `user_id`." The cloud uses this to scope routing, presence, and
   account-related operations.

2. **Device trust.** Devices authenticate to each other using pinned
   pubkeys via mTLS. This proves "this peer is the device whose pubkey I
   OOB-verified during pairing."

The two concerns are **independent**. The cloud knows `user_id` and
`host_id`; the normal v1 routing path does not need device pubkeys or
trust topology. Pairing happens directly between devices and bypasses
the cloud cryptographically (the cloud may route opaque bytes during
pairing, but cannot read or authorize the pairing payload).

This makes the cloud's role narrow:

- Authenticated routing (deliver tunnel bytes between two devices belonging
  to the same user)
- Presence (`HostUp` / `HostRemoved` events scoped per `user_id`)
- JWT-gated admission

mTLS at the host layer means the cloud is no longer the sole gatekeeper. A
compromised cloud cannot impersonate devices, cannot read host-to-host
traffic content, and cannot inject trust into devices' trust stores.

### 3.1 Threat model

**Trust boundary**: the cloud relay is **untrusted**; paired peers
are **trusted** (the user explicitly verified them out-of-band via
pairing). Every security mechanism in this spec exists to bound
blast radius when the cloud is compromised, not to defend against
already-paired devices.

What the design protects against:

- **Compromised cloud relay**: cannot impersonate a paired device
  (mTLS pinning rejects any cert whose pubkey isn't in the trust
  store), cannot read host-to-host gRPC payloads (end-to-end mTLS
  inside multi-hop tunnels), cannot inject trust into a device's
  trust store (pairing is OOB and bypasses the cloud
  cryptographically). The cloud can observe and modify the routing
  graph it forwards (HostUp/HostDown events it emits, tunnel
  framing it routes); the daemon caps memory/handshake cost on
  this surface (§10 Implementation defaults) so cloud-side
  resource attacks degrade gracefully.
- **Off-path network observers** (LAN, ISP, transit): cannot read
  traffic on any Link (hop-by-hop TLS or SSH) or inside tunnels
  (end-to-end mTLS).
- **Active MITM attempting pairing** (e.g., compromised cloud
  relay or off-path PIN guesser): blocked by OOB verification —
  QR pubkey pinning, SPAKE2's resistance to offline PIN brute
  force, one-shot tokens.

What the design does **not** protect against:

- **Compromised paired device**: has full runtime authority over our
  Trusted Server. It can call `ClientService.Shutdown`, create/delete
  agents, subscribe sessions, etc. The only v1 exception is
  daemon-local pairing administration and trust mutation (§10 N-S-2).
  This is the explicit consent semantics of pairing — the user
  verified this device OOB.
- **SSH-paired peer trust transfer**: a peer paired with
  `Reachability::Ssh { target }` is trusted at the level of
  "whoever the SSH config currently routes `target` to."
  Re-pointing the SSH alias is governed by SSH trust, not amux
  pubkey pinning. By design (SSH access to the daemon's host is
  already equivalent to local access).
- **Local OS compromise**: an attacker who can read the daemon's
  files or memory can extract the device private key and trust
  store. Out of scope; same threat surface as any process running
  as the daemon user.

**On pubkey visibility.** Device pubkeys are cryptographic identities,
**not secrets**, and paired devices exchange and pin them explicitly.
Security does not depend on hiding pubkeys from the cloud. The normal
v1 cloud-routing path simply has no reason to send them: cloud-routed
device-to-device TLS runs inside opaque tunnels, and the cloud Link
itself uses standard server-auth TLS + JWT rather than device client
certificates.

---

## 4. Concepts

### 4.1 Identity

Each device has, generated on first run and persisted locally:

- A **device keypair** (e.g., Ed25519). The private key never leaves the
  device.
- A **`host_id`**: a random 128-bit value. Stable for the lifetime of the
  device installation.

`host_id` is independent of the pubkey. This decoupling allows future key
rotation without changing the device's stable identifier.

### 4.2 Trust store

The trust store is a local registry on each device:

```text
trust_store: HostId -> TrustEntry {
    pubkey:         bytes,
    name:           string,
    paired_at:      Timestamp,
    reachabilities: Vec<Reachability>,
}

enum Reachability {
    Cloud,                                  // route via cloud's RoutingService
    Ssh { target: string },                 // ssh <target> amux relay
    DirectTcp { addr: SocketAddr },         // dial directly
}
```

It is the source of truth for "who I trust." The persisted
`reachabilities` are local outbound hints for establishing Links; they
are not themselves trust. The trust store is **local-only** — never
sent to the cloud, never copied to other devices, never persisted on
shared storage.

- Entries are added **only via successful pairing**.
- Entries are removed only via the local revocation flow (§5.4).
- `reachabilities` holds reusable local outbound hints for reaching
  the peer. Each pairing flow may append the `Reachability` it
  bootstrapped (a peer paired by `DirectTcp` and later re-paired by
  `Cloud` ends up with both entries; duplicates are deduplicated). On
  startup the daemon attempts to establish direct Links for every
  `DirectTcp`/`Ssh` reachability in the list (§8.8). A trusted peer may
  have no reachability hints; in that state the daemon trusts the peer
  but has no stored way to dial it.
- The runtime selection of which route to use is *not* made from the
  trust store — `ConnectionManager` picks among currently-live routes
  in the routing graph (§4.9, §8.7). Reachabilities are the
  **bootstrap set** for direct-Link establishment.

The SSH `target` is an opaque string — whatever the user passed to
`amux pair --via-ssh`. It is handed verbatim to `ssh` on each reconnect;
alias resolution (`~/.ssh/config`), identity files, port, and other SSH
options are delegated entirely to the user's SSH configuration.

**On-disk layout.** The daemon's data directory is
`paths::default_data_dir()` (`$XDG_DATA_HOME/amux`, falling back to
`~/.local/share/amux` on Unix; `%APPDATA%\amux` on Windows via the
existing `paths.rs` resolution). Within it:

- `device.key` — private key file, mode `600`
- `host_id` — 16 raw bytes, mode `600`
- `trust.json` — trust-store contents serialized as JSON, mode `600`

Each file is updated by write-temp-then-rename for atomicity. The
daemon's non-secret runtime state (in-flight session info, etc.)
lives separately under `$XDG_STATE_HOME/amux/state.yaml`
(`paths::default_state_path()`).

### 4.3 Pairing

Pairing is the bootstrap of mutual trust. It uses an **out-of-band (OOB)
channel** — something an attacker cannot sit in the middle of — to verify
that each side genuinely controls the keypair it claims to own.

Three OOB channels are supported, each with a different bootstrap
mechanism:

| OOB channel | Bandwidth | Bootstrap mechanism |
|---|---|---|
| QR scan | High (fits full pubkey) | Embed pubkey + one-shot token in QR; initiator verifies responder's pubkey directly via TLS handshake |
| SSH stream | High + already authenticated | Exchange pubkeys through the already-authenticated SSH stream |
| User-typed PIN | Low (~6 digits) | SPAKE2 (PAKE) to amplify the short PIN into a strong shared session key |

After successful pairing, both sides update their trust store entry
for the other's `host_id`: pubkey/name/paired_at recorded (or
replaced — see N-P-5). The side that learns a reusable outbound route
also appends that route to the entry's `reachabilities` list
(deduplicated; see §4.2 and §5). The acceptor does not infer a reusable
reachability from the caller's incoming socket.

### 4.4 Runtime: mTLS on network paths

Runtime connections between paired devices that traverse a **network**
transport use **mTLS** for endpoint authentication and confidentiality:

- Each side presents an X.509 self-signed certificate carrying its
  Ed25519 device pubkey in `SubjectPublicKeyInfo` (§10 Implementation
  defaults).
- Each side verifies the peer's pubkey against its trust-store entry
  for the claimed `host_id`.
- Mismatch → reject the connection.

Where mTLS sits in the stack depends on the route length:

- **1-hop direct paired-TCP connections**: mTLS is at the socket
  level. One TLS handshake per Link, with both sides presenting
  pinned device certs.
- The **cloud-relay Link** is an exception: it uses ordinary
  server-auth TLS (the cloud presents a public-CA-issued certificate
  for its hostname; daemons validate it with standard WebPKI/CA
  hostname validation) + JWT for user authentication. The cloud is not
  pinned by device key (N-X-3a).
- **Multi-hop tunnels** (route length ≥ 2): mTLS is **end-to-end
  between the two endpoint daemons**, sitting *inside* the
  `TunnelTransport`. Intermediate hops see only opaque ciphertext
  at the payload layer. Each intermediate Link has its own
  hop-by-hop encryption (pinned mTLS on paired-direct Links,
  server-auth TLS on the cloud Link, SSH on SSH Links).

**Peers paired via SSH take a different runtime path** for the
underlying transport: bytes flow through `ssh <target> amux relay`
and arrive at the responder's local Unix socket, with **no TLS layer
at the daemon level**. SSH provides the encryption and the remote-user
authentication; the Unix socket's file permissions provide local
access control. This is intentionally local-equivalent: if a user can
SSH into the daemon account and run `amux relay`, they can reach the
same daemon socket as local CLI callers. For multi-hop tunnels that
*traverse* an SSH-paired Link as an intermediate hop, the end-to-end
mTLS inside the tunnel still protects the gRPC payload from the
intermediary. See §5.3.1, §8.1, and §8.3.

**SSH-paired trust is SSH-trust, not amux pubkey trust.** A peer with
`Reachability::Ssh { target }` is trusted at the level of "whoever
the SSH config currently routes `target` to." Re-pointing the SSH
alias (editing `~/.ssh/config`, changing host keys, etc.) transfers
authority along SSH-trust rules — there is no amux pubkey pinning
on this runtime path. This is by design: SSH access to the daemon's
host is already equivalent to local access. See §3.1.

### 4.5 Tenancy model

A clean architectural distinction underpins the security model:

| Server type | Tenancy | Identity |
|---|---|---|
| **Cloud relay** | Multi-tenant. `ServerUserState` keyed by `user_id`. | JWT-authenticated user identities. |
| **Device daemon** | Single-tenant. One `ServerUserState`. | One device keypair + `host_id`. |

Pairing is a **1:1 trust relationship between two single-tenant device
daemons**. The cloud, as a multi-tenant entity, does not participate in
pairing.

This explains why pairing requires no cloud-issued token: the OOB-verified
pairing flow itself is the authorization — it directly links the two
device `ServerUserState`s without needing the cloud to attest.

### 4.6 Transport layering

The runtime has several transport stacks depending on how a daemon
reaches a given peer. They share the same gRPC-service layer at the
top; what changes is how bytes get there.

**Local arrivals** (Unix socket — local CLI/App, `amux pair-recv`):

```text
   gRPC services (ClientService, AgentService, RoutingService)
                              ↑
              Unix socket (no TLS; OS file permissions)
```

**SSH relay arrivals** (`amux relay` from SSH-paired peers) connect to
the same local Unix socket as the CLI. SSH has already authenticated
the remote OS user, and Unix socket permissions still gate access to
the daemon account.

**1-hop direct connection, paired-direct TCP** (or daemon ↔ cloud-relay):

```text
   gRPC services (RoutingService, AgentService, etc.;
                  multiplexed via HTTP/2 streams)
                              ↑
                  mTLS (paired-direct) /
                  server-auth TLS + JWT (cloud-relay)
                              ↑
                         TCP socket
```

**1-hop direct connection, paired-SSH**:

```text
   gRPC services (RoutingService, AgentService, etc.;
                  multiplexed via HTTP/2 streams)
                              ↑
              SSH stdio (SSH provides encryption;
                         no TLS at the daemon level)
                              ↑
        `ssh <target> amux relay` → remote local Unix socket
```

**Multi-hop runtime tunnel** (paired peers, route length ≥ 2 —
through cloud relay or chained through paired peers):

```text
   gRPC services (AgentService, etc. — runtime)
                              ↑
       end-to-end mTLS between the two endpoint daemons
       (pinned device certs; opaque to intermediaries)
                              ↑
                  `TunnelTransport` (A↔B byte stream)
                              ↑
   `TunnelFrame`s in `Message` envelope on the first-hop Link's
   `RoutingService.Connect` bidi stream
                              ↑
   first-hop Link's transport stack (one of the 1-hop stacks
   above)
```

**Multi-hop pairing tunnel** (pre-trust, route length ≥ 2 —
typically through cloud relay during QR/PIN pairing). The cloud may
route raw `TunnelFrame`s to a same-user `host_id` even before
device trust exists; the endpoint's dispatcher decides whether
they reach the Pairing Server.

```text
   PairingService (PairByToken / PairBySpake2)
                              ↑
       per-flow client-side TLS verification inside the tunnel:
       • QR flow: client verifies responder cert pubkey against
         QR-known pubkey
       • PIN flow: client does not verify responder cert; SPAKE2
         + AEAD inside provides actual auth
       (Responder always presents its device cert; differences are
       only in what the client does with it.)
                              ↑
                  `TunnelTransport` (A↔B byte stream)
                              ↑
   `TunnelFrame`s in `Message` envelope on the first-hop Link's
   `RoutingService.Connect` bidi stream
                              ↑
   first-hop Link's transport stack (typically cloud-relay TLS)
```

Each layer is independent of the ones below.

- **Local Unix-socket arrivals** carry no TLS at the daemon layer.
  Unix socket mode `600` ownership is the daemon access gate; arrivals
  forwarded by `amux relay` additionally rely on SSH for remote-user
  authentication. gRPC services trust their caller after admission.
- **1-hop direct connections** have a single TLS (or SSH-equivalent)
  layer at the socket. gRPC runs directly on top — no `TunnelFrame`
  wrapping. The Link's tonic Channel multiplexes all 1-hop service
  calls (RoutingService, AgentService, etc.) via HTTP/2 streams.
- **Multi-hop tunnels** have two encryption layers: hop-by-hop on
  each Link in the route, and end-to-end mTLS between the endpoints
  *inside* the tunnel. The `TunnelFrame` layer carries opaque bytes;
  intermediaries pop the next hop from `dst` and forward. They
  cannot read the gRPC payload because the end-to-end mTLS protects
  it.

The "1-hop = direct gRPC, multi-hop = tunneled" distinction is **not
a runtime branch** anywhere in code. It falls out of how Channels are
registered: direct-Link establishment registers the Link's Channel in
the `ConnectionPool` keyed by a length-1 route; tunnel materialization
registers a `TunnelTransport`-backed Channel keyed by a longer route.
Callers just call `pool.get(route)` and receive a tonic Channel. See
§8.

### 4.7 Service authentication

**Pairing is the trust boundary.** Once paired, a peer has full runtime
authority over the Trusted Server. The only v1 service-layer split is
daemon-local pairing administration and trust mutation: local
Unix-socket and in-process callers may mutate trust; paired remote
mTLS callers may not (§10 N-S-2). SSH `amux relay` uses the local Unix
socket and is local-equivalent.

A device daemon hosts **two gRPC Servers**:

| Server | Hosts | Reached via |
|---|---|---|
| **Trusted Server** | `ClientService` + `AgentService` + `RoutingService` (+ future trusted services) | Local Unix socket **or** mTLS-verified runtime connection (paired peer) |
| **Pairing Server** | `PairingService` | Pre-trust runtime connection (no client cert) in pairing-mode |

The cloud relay also hosts `RoutingService` for its connected daemons,
authenticated via JWT. The cloud is **not** a paired peer; it cannot
reach a daemon's Trusted Server as a caller. Its role is to relay
opaque tunnel bytes between daemons of the same user.

| Service | Hosted by | Reached via |
|---|---|---|
| **`ClientService`** | Daemon's Trusted Server | Local Unix socket or paired-peer runtime connection |
| **`AgentService`** | Daemon's Trusted Server | Local Unix socket or paired-peer runtime connection |
| **`RoutingService`** | Daemon's Trusted Server **and** cloud relay | Daemon: local Unix socket or paired-peer runtime connection. Cloud: TCP+TLS, JWT-authenticated. |
| **`PairingService`** | Daemon's Pairing Server | Pre-trust runtime connection (no client cert) in pairing-mode |

`RoutingService.Connect` is the bidi stream that *defines* a Link
(§4.8). Every direct daemon-to-daemon connection — paired-direct,
paired-SSH, daemon ↔ cloud — establishes a Link by calling
`RoutingService.Connect` on the other side after the underlying
handshake completes.

**Key consequences**:

- **Trusted Server ingress has equivalent runtime authority after admission**:
    - Local Unix socket — local CLI and `amux pair-recv` (writing trust
      entries). OS file permissions are the gate.
    - SSH relay through the local Unix socket — `amux relay` forwarding
      bytes from SSH-paired peers. OS file permissions and SSH
      authentication are the gate, and this has the same authority as
      a local CLI caller running as the daemon user.
    - mTLS-verified runtime connection — paired peer reaching us via
      the external TCP listener (1-hop, dispatched after the pinned-
      cert TLS handshake) or via the terminating end of a multi-hop
      tunnel (dispatched after the end-to-end mTLS handshake inside
      the `TunnelTransport`).
- **Paired peers have full runtime authority.** A peer that has been
  paired with this daemon can invoke runtime methods on the Trusted
  Server, including disruptive operations (e.g., daemon shutdown). The
  local-admin trust mutation RPCs are reserved for local Unix-socket /
  in-process callers. Finer-grained authorization (read-only peers,
  per-method gates) is deferred — see §11.
- **The Pairing Server is bootstrap-only.** Reachable only via
  pre-trust inbound streams (no pinned client cert presented) when
  pairing-mode is active. Once pairing succeeds, future runtime
  traffic goes to the Trusted Server with the appropriate mTLS
  authentication.

#### Tunnel dispatcher

The dispatcher is the central admission point for inbound runtime
byte streams. It distinguishes **pre-trust** streams (paired by an
in-progress pairing flow; routed to the Pairing Server) from
**trusted** streams (mTLS-pinned to an existing trust-store entry;
routed to the Trusted Server). Two source types feed it:

- **External TCP listener** (`tcp_port`): TCP sockets accepted from
  paired peers dialing this daemon directly, or from a pairing
  initiator during pair-mode.
- **Inbound multi-hop tunnels** from the daemon's tunnel pool: each
  `TunnelTransport` whose `dst` arrived empty at this daemon (the
  daemon is the multi-hop tunnel's terminating endpoint). Can carry
  either a paired peer's end-to-end mTLS handshake or a pairing
  initiator's pre-trust handshake.

For each inbound stream:

1. Completes the TLS handshake using a single TLS config: server
   **always** presents its device self-signed cert; client cert is
   **requested but not required**. The server side does not vary
   by flow. Initiator-side TLS verification varies (paired peer
   verifies against trust store; QR initiator verifies against
   QR-known pubkey; PIN initiator does not verify) — but the
   responder is unaware of which until the stream is dispatched.
2. Routes the (now-TLS-wrapped) stream to the appropriate Server:

    | Handshake outcome | Routed to |
    |---|---|
    | Client presented a pinned cert (verified against trust store) | Trusted Server (mTLS-verified runtime) |
    | Client presented no cert + pairing-mode is on | Pairing Server (pre-trust pairing) |
    | Client presented no cert + pairing-mode is off | Closed |
    | Client presented an unpinned cert | Rejected at handshake |

This is the **only** way an unpaired peer reaches anything inside the
daemon: via the Pairing Server during pair-mode, with no
trust-store-side authority. All other inbound streams must
authenticate via mTLS pinning to a trust-store entry.

**Pre-trust admission on the cloud route.** The cloud relay
forwards `TunnelFrame`s to any same-user `host_id` regardless of
device-trust state between initiator and responder — it cannot tell
a paired runtime tunnel from a pre-trust pairing tunnel (both are
opaque ciphertext to the cloud, see N-C-3). Admission is decided
purely at the responder's dispatcher per the table above:
TLS-handshake outcome + local pair-mode flag determines whether the
stream reaches the Pairing Server, the Trusted Server, or is
closed. The cloud's role is byte relay; the responder's dispatcher
is the trust gate.

The local Unix socket bypasses the dispatcher entirely: it feeds the
Trusted Server directly. SSH relay peers reach the Trusted Server via
`amux relay` → local Unix socket, with SSH providing the encryption and
remote-user authentication.

For direct paired-peer inbound connections, the Trusted Server's
`RoutingService.Connect` handler completes the Hello/HelloAck on the
TLS-wrapped stream and registers the Link (§8.5). The same code path
that handles outbound Link establishment handles inbound: just with
the roles reversed at the gRPC layer.

#### Connection-scoped, not per-call

The expensive validation (TLS handshake, client cert verification,
pairing-mode flag read, OS socket connect) runs **once per tunnel /
connection at acceptance time**. After that, the connection is bound
to a Server with a fixed service set. Runtime RPCs do no further auth
checks. The v1 exception is daemon-local pairing administration and
trust mutation on `ClientService`, which checks the accepted ingress
class and rejects paired remote mTLS callers. Per-frame auth cost is
zero.

`Reauth` / `ReauthAck` on `RoutingService.Connect` handles JWT
**refresh** for the long-lived host-to-cloud stream, not per-message
revalidation.

#### Considered alternatives

Two alternative architectures were considered for the daemon and
rejected; both achieve the same security behaviour but with downsides:

1. **Single gRPC Server hosting all services, with per-service
   interceptors.** Every incoming tunnel lands on a shared Server
   hosting all services; per-RPC interceptors gate calls by checking
   TLS state and pairing-mode flag.
   - *Equivalent in security.*
   - *Rejected because*: auth logic is scattered across N service
     interceptors instead of living in one place; larger attack surface
     (services exist on every tunnel and reject rather than not
     existing — an unauthenticated peer can probe what's there);
     adding a new runtime service requires writing a new interceptor.

2. **One gRPC Server spun up per accepted tunnel**, with services
   chosen at tunnel-accept time based on TLS handshake outcome.
   - *Equivalent in security.*
   - *Rejected because*: unnecessary Server-lifecycle churn. The
     two-server model achieves the same per-tunnel service-set
     decision but reuses two long-lived Servers fed by `mpsc` streams
     of accepted tunnels — matching the existing implementation
     pattern in this codebase (`serve_with_incoming` over an `mpsc`
     receiver of `TunnelTransport`).

A third alternative — **three Servers, with `ClientService` on a
local-only Server distinct from a separate runtime-services Server**
— was also considered and rejected for the same reason: it introduced
a "local vs remote" distinction that pairing already supersedes. Once
two devices are paired, treating a remote peer as second-class
relative to a local CLI session adds complexity without changing the
trust model.

The two-Server-plus-dispatcher model has zero per-RPC overhead,
single-location auth logic, minimal attack surface, and reuses the
existing tunnel/server plumbing.

### 4.8 Routing graph

A daemon maintains a **routing graph** of how to reach other hosts.

- A **Link** is a bidi `RoutingService.Connect` stream between two
  adjacent daemons over a single underlying transport (cloud-relay
  TCP+TLS, paired-direct TCP+mTLS, or paired-SSH stdio). Every
  direct daemon-to-daemon connection is a Link. A Link has a name
  assigned at Hello/HelloAck (responder assigns; both endpoints use
  the same name).
- A **route** is an ordered sequence of link names. The first link
  is the next hop. Each forwarding hop pops one link from the route
  and forwards onward.
- The daemon stores **multiple routes per host**. Reachability events
  propagate through the graph; the daemon learns of distant hosts
  through chains of `HostUp` events.

**`HostUp` semantics**: a `HostUp { host: H, route: R }` event means
"the sender (of this event) can reach `H` via route `R` (from the
sender's perspective)." Hosts do not announce themselves via
`HostUp`; endpoint identity is established at link establishment by
Hello/HelloAck.

**Propagation**: when a daemon establishes a new direct Link, it
emits `HostUp(other_endpoint, route=[new_link])` on its existing
Links. Each receiver prepends its incoming link name to the route
(so the receiver's perspective is `[my_inbound_link, new_link]`)
and forwards onward.

**`HostDown` semantics**: a `HostDown { host: H, route: R }` event
invalidates the specific `(host, route)` pair. A "host gone"
reachability signal fires only when the last route for that host is
removed.

**Single source of truth**: originating `HostUp`/`HostDown` events
come *only* from the daemon that owns an immediate link to the host
being announced. Other daemons forward — prepending their own
incoming link name as they relay — but never originate. No
component synthesizes events in response to downstream observations
(e.g., a failed tunnel materialization is not converted to a
`HostDown`). See N-R-4.

**Deduplication**: `HostUp` events are deduplicated on receive. A
strictly worse route — longer than an existing route to the same
host, with the same trailing path — is dropped, not stored or
propagated. This prevents O(N) cascades when new edges form between
nodes already mutually reachable through existing connectivity. See
N-R-2.

**`RoutingService.Connect` is run only on direct connections.**
Learned-via-routing reachability does not trigger new
`RoutingService.Connect` calls. Connection count scales with edges,
not nodes.

**Wire vs logical events.** `HostUp` / `HostDown` are wire-level
routing events, per `(host, route)`. They are what propagates
through Links and feeds the routing core. `ClientService`'s
host-list surface uses `HostUpdated` upserts and `HostRemoved`
deletions for UI inventory state. A subscriber to
`ClientService.SubscribeHosts` sees `HostUpdated(B)` when B first
becomes online by any route and another `HostUpdated(B)` if B is a
trusted host whose last route disappears and it remains as a
trust-store-only entry. `HostRemoved(B)` is reserved for hosts with no
remaining online route and no trust-store entry. Multiple `HostUp`
events for the same host with new routes do not produce duplicate
updates unless the host entry changes.

### 4.9 Connections

The daemon's outbound calling surface is built from three layers:

- **`LinkRegistry`** (lower-level, local-only) — a `HashMap<Link,
  LinkWriter>` of *this daemon's own* outbound writers, one entry
  per direct Link. Each writer is the mpsc-sender that feeds raw
  `Message` envelopes into the Link's Connect stream. Used at frame
  forward time: when a `TunnelFrame` arrives and we pop the next
  hop from `dst`, we look up that link name's writer and forward
  the frame. The codebase implements this in
  `crates/amux/src/routing/link_registry.rs` with a per-link
  `PENDING_ROUTING_EVENT_LIMIT` of 256 (backpressure cap on
  pre-snapshot routing events).

- **`ConnectionPool`** (higher-level) — a `HashMap<Route,
  tonic::Channel>` registry. Dumb storage: callers `register`,
  `get`, and `unregister`. The pool contains no policy and no
  materialization logic. For a 1-hop route `[L]`, the registered
  `Channel` wraps the same byte stream the `LinkRegistry`'s writer
  feeds — i.e., one underlying socket; two views (raw-frame writer
  for forwarding vs gRPC Channel for service calls).

- **`ConnectionManager`** (policy) — subscribes to routing events.
  Tracks `active_route: HashMap<HostId, Route>`. Decides when to
  swap the active route for each peer.

Channel materialization fits two patterns:

- **1-hop Channel**: constructed when a direct Link is established
  (cloud-attach, pairing-direct, or startup re-establish). The
  resulting tonic Channel wraps the underlying socket. Registered
  in the pool keyed by the 1-hop route `[link_name]` *before* the
  corresponding `HostUp` is emitted (N-L-3).

- **Multi-hop Channel**: constructed on demand when the
  `ConnectionManager` decides to use a multi-hop route and no
  Channel exists for it. A `TunnelTransport` is built (a new
  `TunnelId.nonce` is allocated; `TunnelFrame`s flow over the
  first-hop Link's Connect stream). End-to-end mTLS handshakes over
  the `TunnelTransport`. The TLS-wrapped transport is then wrapped
  in a tonic Channel and registered in the pool keyed by the
  multi-hop route.

**Selection policy**: shortest route wins; ties broken
first-known-first. Direct connections (route length 1) win over any
multi-hop route. No transport-flavour preference (`DirectTcp` vs
`Ssh` vs `Cloud`) is encoded; the flavour determines how the Link
is established but not the runtime selection.

**Swaps are make-then-break**: materialize the new Channel and
register it in the pool, flip `active_route[peer]`, then unregister
the old route (which drops the old Channel and breaks any in-flight
gRPC streams on it). Clients reconnect via the new active route on
their next call — the same way any network blip is handled.

**Triggers**: `ConnectionManager` re-evaluates only when a routing
event arrives. It does not probe routes ahead of time. Routes
asserted by `HostUp` are assumed to work; if materialization fails
at use-time, the error propagates and a subsequent `HostDown` (from
the link's owner, when the link is actually broken) evicts the bad
route.

**Startup**: on daemon startup, the daemon iterates its trust store
and attempts to re-establish a direct Link for each entry with
`Reachability::DirectTcp` or `Reachability::Ssh`.
`Reachability::Cloud` entries need no action here; the cloud-attach
flow brings up the cloud Link separately and routing events
propagate from there.

---

## 5. Pairing flows

### 5.1 QR flow (phone setup)

**Setup** on desktop D:

1. User runs `amux pair --qr`.
2. D's daemon generates a one-shot **token** (32 random bytes), stores it
   locally with a ~5 minute TTL.
3. D displays a QR code encoding
   `(D.host_id, D.pubkey, cloud_rendezvous_url, token)`. The QR
   payload is UTF-8 JSON with fields `host_id` (UUID string),
   `pubkey` (32-byte array), `cloud_url` (string), and
   `one_shot_token` (32-byte array). The responder's friendly name is
   not included in the QR; it is returned by `PairByTokenResponse`.

**Pairing** on phone P:

4. P scans the QR. Now has `(D.host_id, D.pubkey, token)`.
5. P opens a tunnel through cloud routing to `D.host_id`. The cloud
   routes opaque bytes; it does not know this is a pairing attempt.
6. P initiates a TLS handshake as client. Configuration: "require the
   server's cert to match `D.pubkey`; I will not present a client cert."
    - D presents its self-signed cert (containing `D.pubkey`).
    - P verifies `cert.pubkey == QR-advertised D.pubkey`. Match → P is now
      sure it is talking to the real D (only D has the matching private
      key). Mismatch → abort.
    - Encrypted, server-authenticated channel established.
7. P calls
   `PairingService.PairByToken(token, P.host_id, P.pubkey, P.name)`
   over this channel.
8. D verifies the token matches the one it issued. If valid:
    - D pins `(P.host_id → P.pubkey, P.name)` with
      `Reachability::Cloud` in its trust store.
    - D responds with `(D.host_id, D.name)` (P already has D.pubkey from
      the QR).
    - D invalidates the token; pair-mode ends (N-P-3).
9. P pins `(D.host_id → D.pubkey, D.name)` with `Reachability::Cloud`.

**Security**:
- P trusts `D.pubkey` because it came OOB via the QR (user's eyes).
- The TLS handshake mathematically requires D to own the corresponding
  private key — only D can complete it.
- D trusts P because P presents the token — and only the QR-scanner could
  have got the token.
- Token is one-shot, ~5 minute TTL.

### 5.2 PIN flow (desktop-to-desktop)

**Setup** on A:

1. User runs `amux pair` (bare).
2. A's daemon enters pairing mode for ~5 minutes, generates a 6-digit PIN,
   displays it (and the LAN listener port if `tcp_port` is configured).

**On B**:

3. User runs `amux pair --connect [target]`.
    - If `[target]` omitted: interactive picker showing cloud-discovered
      devices (from existing `HostUp` data).
    - If a name: cloud lookup.
    - If `ip:port`: direct TCP.
4. B's daemon prompts for the PIN. User types it.

**Handshake** (over either a cloud-routed tunnel or direct TCP):

5. The underlying tunnel completes a TLS handshake under the
   dispatcher's standard config (§4.7): the responder presents its
   **device cert** (the same self-signed cert used for paired-peer
   mTLS — there is no separate "pairing cert"). The initiator
   presents no client cert and **does not verify** the responder's
   cert. The TLS session provides byte-stream encryption only — at
   this layer neither side is authenticated. Because the responder
   always presents its device cert regardless of what kind of
   inbound stream this is, paired-peer clients connecting
   concurrently see the cert they expect.
6. A and B exchange SPAKE2 messages using the PIN as the shared
   secret, sent as gRPC frames inside the TLS session. The
   cryptographic detail is in §5.2.1.
7. After mutual key confirmation, both have a strong session key
   derived from PIN + fresh randomness on both sides. This
   SPAKE2-derived key provides the real mutual authentication —
   the TLS layer in this flow contributes only confidentiality.
8. They exchange `PairingIdentity` messages sealed with AEAD using
   per-direction keys derived from the SPAKE2 secret. Cloud-routed
   PIN pairing stores `Reachability::Cloud`. In direct TCP pairing,
   only the dialer stores `Reachability::DirectTcp { addr }`, using
   the listener address it dialed; the responder MUST NOT store the
   accepted socket's peer address because that is normally the
   dialer's ephemeral source port, not a reusable listener. The
   pubkey/host_id binding comes from the AEAD-sealed identity, **not**
   from the TLS cert (which was the responder's device cert, but the
   initiator never verified it). Pair-mode ends on first success
   (N-P-3).

The AEAD layer over the SPAKE2-derived key is what authenticates the
identity exchange. The TLS layer is uniform with all other inbound
streams (dispatcher always sees a TLS-wrapped stream with the
responder's device cert).

**Security**:
- Eavesdroppers see SPAKE2 messages but cannot recover the PIN — SPAKE2
  is designed to prevent offline brute force.
- Active MITMs (e.g., a compromised cloud relay attempting to
  impersonate) must guess the PIN online; each guess requires a full
  SPAKE2 exchange against a rate-limited responder. With a 6-digit PIN
  and an attempt cap, success chance is negligible.

#### 5.2.1 PIN-flow cryptographic detail

This subsection specifies the SPAKE2 exchange + AEAD-sealed
identity at wire level. The primitive is RFC 9382 SPAKE2 over
Curve25519; this subsection wraps it.

**Roles.** Within the SPAKE2 primitive, the PIN-pair **responder**
(`A`, the side displaying the PIN) plays SPAKE2's "A" role and uses
the static blob `M`. The PIN-pair **initiator** (`B`, the side
typing the PIN) plays SPAKE2's "B" role and uses static blob `N`.

**Password input.** The application PIN input is the UTF-8 bytes of
the 6-digit PIN string. The SPAKE2 scalar is:

- `w = edwards25519_scalar_reduce_wide(SHA-512(PIN_UTF8))`

where `edwards25519_scalar_reduce_wide` is reduction modulo the
edwards25519 basepoint order from a 64-byte little-endian wide
integer. No additional application-level stretching is performed at
this layer (SPAKE2 absorbs the low entropy).

**Group and point encoding.** The ciphersuite is RFC 9382
`edwards25519`:

- `M =
  d048032c6ea0b6d697ddc2e86bda85a33adac920f1bf18e1b0c6d166a5cecdaf`
- `N =
  d3bfb518f44f3430f29d0c92af503865a1ed3281dc69b35dd868ba85f886c4ab`
- `SPAKE2_msg_A` and `SPAKE2_msg_B` are exactly 32-byte compressed
  Edwards-Y encodings.
- Received SPAKE2 points must decode successfully, must not be the
  identity, and must be torsion-free; otherwise abort with
  `PairingError { reason: INVALID_PIN }`.

**Wire message order** (gRPC bidi stream on `PairBySpake2`):

1. `B → A`:  `PairBySpake2Message { spake2_message: SPAKE2_msg_B }`
2. `A → B`:  `PairBySpake2Message { spake2_message: SPAKE2_msg_A }`
3. Both sides locally compute the shared group element `K` per RFC
   9382 from `(M, N, msg_A, msg_B, w)`, including the cofactor
   multiplication `h = 8`, and encode it as a 32-byte compressed
   Edwards-Y value for the amux key schedule below.
4. Both sides derive subkeys via HKDF (see below).
5. `B → A`:  `PairBySpake2Message { key_confirmation: HMAC_KC_B }`
6. `A → B`:  `PairBySpake2Message { key_confirmation: HMAC_KC_A }`
7. Each side verifies the peer's `key_confirmation`; mismatch →
   abort with `PairingError { reason: INVALID_PIN }`.
8. `A → B`:  `PairBySpake2Message { sealed_identity: SEALED_A }`
9. `B → A`:  `PairBySpake2Message { sealed_identity: SEALED_B }`
10. `A` opens `B`'s `sealed_identity`, commits trust for `B`, ends
    pair-mode on first success (N-P-3), and replies with
    `PairBySpake2Message { pairing_complete: {} }`.
11. `B` commits trust for `A` only after receiving `pairing_complete`.
    During a same-`host_id`/different-pubkey replacement, the active
    pairing tunnel may be preserved long enough to deliver this
    completion acknowledgement; all other old-key Links/tunnels for the
    replaced host are torn down before new trusted traffic is accepted.

Either side may instead emit `PairBySpake2Message { error: ... }`
at any point to abort.

**Key schedule.** Let `K` be the SPAKE2 shared secret. Let
`transcript_hash = SHA-256(  PROTOCOL_VERSION_BE_u32
                         || len_u32(SPAKE2_msg_B) || SPAKE2_msg_B
                         || len_u32(SPAKE2_msg_A) || SPAKE2_msg_A )`
covering exactly steps 1–2 of the wire order (the SPAKE2
exchange itself, in `B→A→...` order). All length prefixes are
big-endian `u32`. `PROTOCOL_VERSION_BE_u32` is the constant value
of the current protocol version encoded big-endian: for this
revision, **`0x00 0x00 0x00 0x05`** (PROTOCOL_VERSION = 5). The
PairingService flow does not run Hello/HelloAck, so the version
is not negotiated — both sides hardcode it from the spec.

Derive five 32-byte subkeys using HKDF-SHA256:

- `PRK = HKDF-Extract(salt = "amux-pair-spake2-v1", IKM = K)`
- `KC_A = HKDF-Expand(PRK, info = "kc/A" || transcript_hash, L = 32)`
- `KC_B = HKDF-Expand(PRK, info = "kc/B" || transcript_hash, L = 32)`
- `AEAD_A_to_B = HKDF-Expand(PRK, info = "aead/A→B" || transcript_hash, L = 32)`
- `AEAD_B_to_A = HKDF-Expand(PRK, info = "aead/B→A" || transcript_hash, L = 32)`

**Key confirmation**:

- `HMAC_KC_B = HMAC-SHA256(KC_B, "amux-pair-confirm-B" || transcript_hash)`
- `HMAC_KC_A = HMAC-SHA256(KC_A, "amux-pair-confirm-A" || transcript_hash)`

Each side recomputes the peer's HMAC locally and compares in
constant time; reject on mismatch.

**AEAD sealing** of `PairingIdentity` (ChaCha20-Poly1305):

- Cleartext: the `PairingIdentity` proto message
  serialized with the standard protobuf encoding. (Cleartext schema
  is defined in §6.)
- Key: `AEAD_A_to_B` for the A→B direction's seal,
  `AEAD_B_to_A` for the B→A direction's seal.
- Nonce: 12 bytes (96-bit). Per-direction counter encoded as
  **big-endian unsigned 96-bit integer**, starting at `0` and
  incrementing by `1` on each AEAD operation in that direction.
  Nonce `0` is the twelve-byte sequence
  `00 00 00 00 00 00 00 00 00 00 00 00`. v1 sends exactly one
  sealed message per direction, so nonces in practice are always
  `0`; the counter is specified for forward-compatibility.
- AAD: `"amux-pair-id" || direction_byte || transcript_hash`,
  where `direction_byte = 0x01` for A→B and `0x02` for B→A.

The receiver opens with the same key/nonce/AAD; AEAD-open failure
→ abort with `PairingError { reason: INVALID_PIN }` (same opaque
error as a key-confirmation failure, to avoid leaking the failure
mode — see §6.1).

### 5.3 SSH flow (piggyback)

**Initiation** on A:

1. User runs `amux pair --via-ssh user@host`.
2. A's CLI shells out to `ssh user@host amux pair-recv` (or equivalent).

**Exchange**:

3. SSH establishes its own authenticated, encrypted channel — host-key
   verification + user authentication on the SSH layer.
4. Through SSH's stdin/stdout stream:
    - A sends `(A.host_id, A.pubkey, A.name)`.
    - The `amux pair-recv` process on the remote (B) sends back
      `(B.host_id, B.pubkey, B.name)`.
5. Both pin. A stores
   `Reachability::Ssh { target: "<whatever user typed>" }` in its trust
   entry for B. (No port needed — see §5.3.1.) B pins A's identity but
   does **not** add an outbound reachability from this flow: an incoming
   SSH session proves who authenticated to B, but it does not tell B which
   SSH target string would reach A later. B may learn an outbound
   reachability for A through a later direct or cloud pairing flow.

SSH pairing is not a distributed atomic transaction. Each side commits
to its own local daemon and durable trust store; if one process or host
fails after its local commit but before the peer completes, the operator
may have to remove that one-sided trust entry once revocation/trust
removal is implemented.

**No TLS** at this stage. SSH already provides encryption + authentication
for the one-time identity exchange.

#### 5.3.1 SSH runtime transport

**After pairing**, runtime connections to this peer use SSH as the
transport. A spawns `ssh <target> amux relay` (one child per
SSH-paired peer; kept alive for the Link's lifetime). On the
remote, `amux relay`:

1. Connects to the local daemon's normal Unix socket at
   `Config.socket_path`.
2. Bridges its stdin/stdout to that socket.

The SSH child's stdin/stdout is the byte stream on A's side. On B's
side, the bytes flow through `amux relay` into the Trusted Server via
the Unix socket — **no TLS** on this path (SSH provides encryption;
OS file permissions on the Unix socket plus SSH's user authentication
provide access control; B's pairing with A is what authorised this
peer in the first place).

There is no separate SSH relay daemon socket and no reduced-authority
SSH relay ingress class. SSH access to the daemon's OS account is
local-equivalent for v1.

No `-L` setup, no open external port, no localhost TCP listener
needed. SSH is both the bootstrap OOB channel **and** the ongoing
runtime transport for peers paired this way.

### 5.4 Revocation

Revocation is a local administrative action that removes trust for one
specific peer. It is not a network protocol and cannot be initiated by a
remote peer.

1. A local user invokes revocation against a peer `host_id` or a unique
   display name (`amux unpair <id-or-name>`, or the equivalent local
   `ClientService.Unpair` RPC).
2. The daemon resolves the peer in the local trust store and removes its
   trust entry from `trust.json` atomically via write-temp-then-rename.
3. The daemon sends
   `GoAway { reason: GO_AWAY_REASON_USER_REVOKED, drain_timeout_ms: 0 }`
   on every active Link whose adjacent peer is that `host_id`.
4. The daemon evicts every route for that `host_id` from
   `RoutingCore`, unregisters corresponding `ConnectionPool` Channels,
   drops cached multi-hop `TunnelTransport`s, closes direct Link
   runtime state, and tombstones affected `TunnelId`s for the standard
   window.
5. Subsequent inbound mTLS handshakes from the removed pubkey fail
   naturally because the pinned-cert verifier reads the live trust
   store and no separate stale acceptance cache exists.
6. The daemon emits
   `trust.remove { host_id, name, paired_at, removed_at, reason }`.

---

## 6. PairingService proto

`PairingService` is a new service alongside `AgentService`,
`RoutingService`, and `ClientService`. It is the **only RPC callable
without prior mutual trust**, and only when the responder is in pairing
mode.

```proto
service PairingService {
  // QR flow.
  //
  // Caller has an out-of-band token and knows responder's pubkey
  // (from the QR). The TLS handshake on the underlying tunnel
  // authenticates the responder via the QR-known pubkey; this RPC
  // exchanges identity and verifies the one-shot token.
  rpc PairByToken(PairByTokenRequest) returns (PairByTokenResponse);

  // PIN flow.
  //
  // SPAKE2 + identity exchange over a bidi stream. The underlying
  // tunnel runs TLS with the responder presenting its device cert;
  // the PIN initiator does not verify it (no pubkey known
  // out-of-band). SPAKE2 inside provides the mutual authentication
  // based on the shared PIN; the identity exchange is AEAD-sealed
  // with per-direction keys derived from the SPAKE2 secret.
  // See §5.2 / §5.2.1 / N-P-4.
  rpc PairBySpake2(stream PairBySpake2Message)
    returns (stream PairBySpake2Message);
}

message PairByTokenRequest {
  bytes one_shot_token = 1;

  // Initiator's identity, for the responder to pin.
  bytes host_id = 2;  // 16 bytes
  bytes pubkey = 3;   // 32 bytes; raw Ed25519 public key (RFC 8032 §5.1.5)
  string name = 4;
}

message PairByTokenResponse {
  // Responder's identity, for the initiator to pin.
  // (Initiator already has responder's pubkey from the QR.)
  bytes host_id = 1;  // 16 bytes
  string name = 2;
}

message PairBySpake2Message {
  oneof body {
    // SPAKE2 protocol bytes: 32-byte compressed Edwards25519 point.
    bytes spake2_message = 1;

    // Key confirmation step (HMAC over transcript, both directions).
    bytes key_confirmation = 2;

    // (host_id, pubkey, name) AEAD-sealed with the SPAKE2-derived
    // session key. Cleartext schema: PairingIdentity.
    bytes sealed_identity = 3;

    // Pairing aborted.
    PairingError error = 4;

    // Responder committed trust; initiator may now commit.
    PairingComplete pairing_complete = 5;
  }
}

message PairingComplete {}

message PairingError {
  enum Reason {
    REASON_UNSPECIFIED = 0;
    NOT_IN_PAIRING_MODE = 1;  // responder not in pairing mode
    INVALID_PIN = 2;          // SPAKE2 key confirmation failed
    INVALID_TOKEN = 3;        // token unknown, expired, or already consumed
    PROTOCOL_VIOLATION = 4;   // unexpected message ordering
    TIMEOUT = 5;
    USER_REJECTED = 6;
    SELF_PAIRING = 7;         // peer host_id equals own host_id
  }
  Reason reason = 1;
  string detail = 2;          // human-readable, optional
}

// Identity exchange payload. For QR/PIN, this is the cleartext format
// inside `sealed_identity`.
//
// Serialized, AEAD-encrypted with the SPAKE2-derived session key, then
// placed in PairBySpake2Message.sealed_identity. For SSH pairing, the
// same serialized message is sent directly inside the SSH-protected
// stdin/stdout stream.
message PairingIdentity {
  bytes host_id = 1;  // 16 bytes
  bytes pubkey = 2;   // 32 bytes; raw Ed25519 public key (RFC 8032 §5.1.5)
  string name = 3;
}
```

### 6.1 Proto notes

- `PairByToken` is unary — one request, one response, done.
- `PairBySpake2` uses a single symmetric message type for both
  directions. Direction is implicit from who sent it.
- SPAKE2 bytes are 32-byte compressed Edwards25519 points.
- `sealed_identity` is `bytes` at the wire level (ciphertext). The
  cleartext `PairingIdentity` is defined alongside and is never sent
  unencrypted in QR/PIN pairing. SSH pairing sends the same
  `PairingIdentity` protobuf inside the SSH-protected stdin/stdout
  stream.
- `bytes pubkey` is exactly **32 bytes**: the raw Ed25519 public key
  per RFC 8032 §5.1.5. Certificate wrapping is an implementation
  detail of mTLS, not part of the wire identity.
- `string name` (in any pairing message, and in `Host` carried over
  `RoutingService.Connect`) is **bounded to 256 bytes UTF-8**.
  Receivers reject longer values with `PROTOCOL_VIOLATION` (in
  `PairingError`) or `HelloAck.error` (in routing). This bounds
  memory cost from malicious peers.
- `bytes host_id` is fixed at 16 bytes.
- Some `PairingError::Reason` values deliberately collapse distinct
  internal cases to avoid leaking information. `INVALID_TOKEN` is
  returned for unknown, expired, **and** already-consumed tokens — the
  caller cannot distinguish them, which prevents token-enumeration
  attacks. Similarly `INVALID_PIN` covers both wrong PIN and any
  SPAKE2 key-confirmation failure.

### 6.2 Service location and reachability

- **Lives in `amux.proto`** alongside the other core services.
- **Hosted on the daemon's Pairing Server** (§4.7). Reachable only via
  a pre-trust (no-client-cert) runtime connection during the
  responder's pairing-mode window — over the cloud route or direct
  TCP, with the per-flow TLS configurations of §5 / N-P-4.
- **Cloud servers do not host `PairingService`.** Attempts to route to
  `PairingService` on a cloud server are rejected. See N-C-1.

### 6.3 Proto changes summary

This revision introduces the following breaking changes to
`crates/amux/proto/amux/v1/amux.proto`. **Set `PROTOCOL_VERSION = 5`**
when applying these. Daemons on this revision advertise `5` in
`Hello.supported_protocol_versions`; they may also list `4` if they
support a fallback path during migration, but this spec does not
require it.

**`TunnelId` changed** (was `{ initiator, target }`; both 16-byte
host_ids):

```proto
message TunnelId {
  bytes initiator = 1;  // 16-byte host_id of the tunnel's creator
  bytes nonce     = 2;  // 16 random bytes (UUIDv4); fixed for tunnel lifetime
}
```

`target` is removed — implicit at the empty-`dst` endpoint
(N-TN-2). The forwarding pipeline already routes by `dst`, not by
`TunnelId.target`. `nonce` is opaque 16-byte random; do not encode
it as a counter.

**`PairingService` added.** Full block in §6 above. Hosted on the
daemon's Pairing Server.

**`RoutingService` exposure widened.** No proto change; the service
definition is unchanged. What changes is *who hosts it*: the cloud
relay continues to host it as before, **and** every device daemon
now hosts it on its Trusted Server (N-G-3). Auth methods differ
(JWT for cloud, mTLS-pinned-cert for paired peers via the
dispatcher).

The host-inventory proto was also revised: `ListHostsRequest` owns the
server-side inventory scope, `HostEntry` is the flattened row type, and
`online` / `UNTRUSTED_BUT_ONLINE` name the current presence semantics.
The local-admin pairing/trust RPCs live on `ClientService` but are
rejected from paired remote mTLS callers. `ClientService` now also has
local-admin `ListPeers`, `GetPeer`, and `Unpair` RPCs for trust-store
inspection and revocation.

**`GoAwayReason` added `GO_AWAY_REASON_USER_REVOKED = 8`.** This reason
is sent during revocation with `drain_timeout_ms = 0`.

**Routing proto messages** (`Message`, `Hello`, `HelloAck`,
`RoutingEvent`, `HostUp`, `HostDown`, `TunnelFrame`, `Reauth`,
`ReauthAck`, `GoAway`, `Error`, etc.) otherwise keep their existing
shapes. The on-disk `TrustEntry` and `Reachability` shapes (§4.2) are
not proto messages; they live in `trust.json` and are local-only.

---

## 7. CLI

| Role | CLI | Behaviour |
|---|---|---|
| **Responder**, PIN | `amux pair` | Display PIN. Enable cloud-routed + LAN-direct pair acceptance on whichever transports are already listening. |
| **Responder**, QR | `amux pair --qr` | Display QR (encoding `host_id`, `pubkey`, cloud rendezvous URL, one-shot token). |
| **Initiator**, QR | `amux pair --qr <payload>` | Consume a scanned QR JSON payload, open the QR-pubkey-pinned cloud pairing tunnel, call `PairByToken`, and store `Reachability::Cloud`. |
| **Initiator**, PIN | `amux pair --connect [target]` | Prompt for PIN. If `[target]` is omitted → interactive picker from cloud device list. If a name → cloud lookup. If `ip:port` → direct TCP. Stores `Reachability::Cloud` (cloud lookup) or `Reachability::DirectTcp` (ip:port). |
| **Initiator**, SSH | `amux pair --via-ssh <target>` | Pairs over SSH stdin/stdout AND configures ongoing SSH transport for runtime calls. `<target>` is passed verbatim to `ssh` (alias / config / port delegated to user's SSH config). Stores `Reachability::Ssh { target }`. |
| **Trust admin** | `amux peer list` | Lists locally trusted peers from `trust.json`. |
| **Trust admin** | `amux peer info <id-or-name>` | Shows one trusted peer by `host_id` or unique display name. |
| **Trust admin** | `amux unpair <id-or-name>` | Prompts, then locally revokes trust for the peer; `--force` skips the prompt. |

### 7.1 Responder behaviour (`amux pair` bare)

- Set local pairing-mode flag with the generated PIN.
- Display PIN + (if `tcp_port` is set) the LAN listener port.
- Wait for an incoming pair-protocol attempt on whichever transports are
  already listening (existing cloud routing tunnel, existing external TCP
  listener if `tcp_port` is configured).
- Auto-cancel pairing mode after ~5 minutes.

Pairing-mode state is **purely local**. No cloud registration; the cloud
is not told the daemon is in pairing mode. See N-P-7.

### 7.1.1 `amux pair --listen` requires `tcp_port`

If the user runs `amux pair --listen` (or any LAN-direct responder flow)
without `tcp_port` set in config, the CLI errors with a useful message:
"set `tcp_port` in your config, or use cloud / SSH pairing." Daemons do
not silently fall back. See N-X-8.

### 7.2 First-run setup: `amux init`

Before any pairing flow can run, a daemon needs its identity and config
in place. This is delegated to `amux init`, which performs the
first-run setup:

- Generates the device keypair (N-K-1).
- Generates the `host_id` (N-K-2).
- Picks a friendly name (may prompt the user).
- Prompts for cloud-mode + runs the OAuth device-flow authentication
  if cloud mode is enabled (see §8.5 cloud Link establishment).
- Leaves `tcp_port` **unset by default**. The user explicitly opts
  in by setting a value when they want LAN-direct reachability.
- Creates the data/state directories (§4.2 on-disk layout).

`amux init` is **not** the only entry point. The same init flow
fires implicitly via `ensure_initialized` from any CLI command that
discovers missing state — the CLI calls
`init::run_init(..., InitContext::implicit())` to bring state to
completion before proceeding. The init state machine is pure:
`init::next_step(config, has_refresh_token, ctx)` decides which
step is next (`PromptCloudMode → Authenticate → PromptIdleSleep →
Done`); each step is idempotent. A partial install on next start is
just an earlier point in the same state machine.

The specific behaviour and prompts of `amux init` are an implementation
concern of that command; this document does not specify them. Anything
in this document that "requires the daemon to have an identity" or
"requires `tcp_port` to be configured" assumes `amux init` has already
run successfully.

### 7.3 `--connect` target syntax

`amux pair --connect <target>`:
- `<target>` looks like `ip:port` → direct TCP.
- `<target>` looks like a name or `host_id` → exact cloud lookup against
  the user's known hosts (from `HostUp` data). Ambiguous names fail rather
  than picking arbitrarily.
- `<target>` omitted → interactive picker over the cloud-known device list.

### 7.4 Legacy `amux server connect` is removed

The legacy `amux server connect <host:port>` command was the entry point
for the unencrypted direct-pair-and-connect flow. It is replaced by:

- `amux pair --connect <target>` for the *pairing* step.
- Implicit routing for runtime; once paired, normal commands route
  through cloud or direct automatically.

---

## 8. Routing & connections

### 8.1 Encryption layering

Runtime connections always have at least one encryption layer:

- **Paired-direct TCP Links**: mTLS at the socket, both sides
  presenting pinned device certs (N-X-1, N-X-3).
- **Paired-SSH Links**: SSH stdio (SSH provides encryption);
  no TLS at the daemon level (N-X-5).
- **Cloud-relay Link**: standard server-auth TLS validated with the
  public WebPKI/CA chain and hostname + JWT for cloud-user auth
  (N-X-3a). Not pinned mTLS.
- **Multi-hop tunnels** add a second encryption layer: end-to-end
  mTLS between the two endpoint daemons (inside the
  `TunnelTransport`, opaque to intermediaries), on top of whichever
  hop-by-hop encryption each Link in the path provides.

Endpoint trust verification happens via mTLS (pinned-cert match
against the trust store) at the layer that performs the relevant
handshake:

- 1-hop direct: at Link establishment.
- Multi-hop: end-to-end mTLS verifies the endpoint identities;
  hop-by-hop Link encryption protects against off-path observers
  but is not what establishes trust between the endpoints.

There is no "skip TLS" mode anywhere for network paths. The only
non-TLS daemon-level paths are SSH (SSH substitutes for our
encryption) and the Unix socket (OS file permissions substitute for
network encryption). See N-X-5.

### 8.2 Two connections per host pair

gRPC is client-server asymmetric. For both A and B to call each
other's services, each side needs its own outbound gRPC Channel.

In practice this means **two Links per paired host pair** — one
initiated by each side. Each Link has its own routing-graph entry,
its own `ConnectionPool` slot keyed by its 1-hop route, and its own
`ConnectionManager` `active_route` on that side. Links are
established lazily: the B→A direction only materializes when B first
has reason to call A (or at startup, per §8.8).

(See N-X-4.)

### 8.3 Transport options

Each trust-store entry's `reachabilities` list (appended at pairing
time, §4.2) determines how the daemon establishes its direct Links
to that peer. Multiple reachabilities per peer are allowed; each
direct-flavour entry produces its own Link-establishment attempt.
The routing graph and `ConnectionManager` then pick routes from
whatever Links are up.

Some trust entries can temporarily have an empty `reachabilities`
list. The main v1 case is the responder side of SSH pairing: B trusts
A after `amux pair-recv`, but B has no outbound `ssh <target>` string
for A until another flow supplies one.

The same rule applies to inbound direct TCP pairing: if A dials B's
configured listener, A records `DirectTcp { addr: B }`; B records trust
for A but no reusable reachability for A. Reconnect remains the
dialer's responsibility unless a later pairing flow gives B an
outbound hint.

| Reachability | How the Link is established | Underlying transport |
|---|---|---|
| `Cloud` | The daemon's cloud-attach flow is up; `HostUp` arriving over the cloud Link advertises this peer through cloud routing. No new Link is opened to the peer directly. | n/a — peer reached via cloud Link + forwarding |
| `DirectTcp { addr }` | At pair-time and on every subsequent startup, the daemon dials `addr` and calls `RoutingService.Connect` on the resulting mTLS socket. The new Link is registered. | TCP + mTLS |
| `Ssh { target }` | At pair-time and on every subsequent startup, the daemon spawns `ssh <target> amux relay` and calls `RoutingService.Connect` on the resulting bidi byte stream. The remote `amux relay` bridges to the daemon's local Unix socket. The new Link is registered. | SSH stdio (SSH provides encryption) |
| (Local client) | Unix socket / in-process memory; no Link. | OS file permissions |

The cloud Link itself is established by the daemon's existing
cloud-attach flow (TCP+TLS to the cloud, JWT-authenticated). It is a
Link just like the paired-direct ones, although its underlying
transport differs.

### 8.4 Daemon server topology and listeners

A device daemon runs **two long-lived gRPC Servers** (see §4.7):

| Server | Hosts | Fed by |
|---|---|---|
| **Trusted Server** | `ClientService` + `AgentService` + `RoutingService` (+ future trusted services) | Local Unix socket and mTLS-verified runtime connections (paired peers) |
| **Pairing Server** | `PairingService` | Pre-trust runtime connections (no client cert) in pairing-mode, via dispatcher |

And **two listener types**:

| Listener | Bind | Default | Feeds |
|---|---|---|---|
| **Local Unix socket** | `Config.socket_path` (`default_socket_path()`) | Always on; mode `600`, owned by daemon user | Trusted Server directly (no dispatcher, no TLS); local CLI/App and SSH `amux relay` traffic |
| **External TCP** (`tcp_port`) | `0.0.0.0:<configured>` | **Off by default**; user explicitly sets `tcp_port` in config to enable LAN-direct reachability | Tunnel dispatcher → Trusted Server or Pairing Server depending on TLS outcome |

The daemon also makes **outbound** connections (cloud-attach, paired
DirectTcp/Ssh re-establish — §8.8). Each outbound connection
establishes a Link via `RoutingService.Connect`; once the Link is
up, the underlying byte stream is owned by the routing layer
(`LinkRegistry`) and a tonic Channel wrapping it is registered in
the `ConnectionPool` keyed by the 1-hop route.

The **tunnel dispatcher** sits between **inbound** byte streams that
need a TLS handshake and the two Servers. Two source types feed it:

- The external TCP listener (paired peers dialing this daemon
  directly over TCP).
- The daemon's tunnel pool: inbound multi-hop tunnels whose `dst`
  arrived empty at this daemon (so this daemon is the tunnel's
  terminating endpoint). Each such tunnel produces a
  `TunnelTransport` whose bytes are end-to-end-TLS ciphertext from
  the originating endpoint.

For each incoming stream it completes the TLS handshake (server
presents cert; client cert requested but not required) and routes
the TLS-wrapped stream to the Trusted Server, the Pairing Server,
or closes it, according to N-G-5 / N-G-6.

Local Unix-socket arrivals bypass the dispatcher entirely — there is
no TLS and no pairing-mode gate; OS file permissions are the access
control. SSH-paired peers reach the Trusted Server through the same
local Unix socket via `amux relay` on the remote, with SSH providing
the encryption and remote-user authentication.

### 8.5 Establishing a direct Link

When a direct connection comes up — at pair-completion, daemon
startup, or manual reconnect — the order of operations is:

1. **Establish the underlying transport** and complete its
   handshake: TCP+mTLS for paired-direct TCP, `ssh <target> amux
   relay` for paired-SSH, or TCP+WebPKI-TLS for cloud-relay.
2. **Wrap the resulting byte stream in a tonic `Channel`** (using
   the existing `channel_from_single_io` helper or equivalent).
   gRPC HTTP/2 framing sits on top.
3. **Call `RoutingService.Connect`** on that `Channel`. This opens
   one HTTP/2 stream inside the Channel for the bidi
   `Message`-envelope routing stream; Hello/HelloAck exchanges
   `Host` info; the responder assigns a link name `L`.
4. **Register the Link** in the local `LinkRegistry` (link name →
   outgoing-tx for `Message` envelopes on this stream).
5. **Register the `Channel` in the `ConnectionPool`** keyed by
   route `[L]`. This is the *same* `Channel` from step 2 — it
   already carries the Connect stream and is also what hosts all
   1-hop service calls (`AgentService`, `ClientService`, etc.)
   between this daemon and the peer.
6. **Emit `HostUp(other_endpoint, route=[L])`** on this daemon's
   *other* existing Links — propagating outward — and to internal
   subscribers (including `ConnectionManager`).

**Steps 4–5 must precede step 6** (N-L-3) so that any subscriber
reacting to the `HostUp` finds the Link in `LinkRegistry` and the
`Channel` in `ConnectionPool` already.

For the cloud Link specifically, the Connect call carries the JWT
in metadata; the cloud's `RoutingService` rejects unauthenticated
calls. Otherwise the flow is the same.

**Cloud-Link `Reauth` flow.** JWTs expire while the cloud Link is
intended to live indefinitely. The daemon refreshes proactively
~5 minutes before the JWT's `exp` (matching the existing
`ROUTING_AUTH_REFRESH_BEFORE_EXPIRY = 300s` constant): it sends a
`Reauth { auth_token: <new JWT> }` `Message` on the Connect stream
and waits up to 15s for `ReauthAck` (matching
`ROUTING_AUTH_REAUTH_RESPONSE_TIMEOUT = 15s`). On
`ReauthAck.accepted`, the Link continues without interrupting
in-flight calls. On `ReauthAck.error`, protocol-error, or timeout,
the daemon sends a `GoAway(AUTH_EXPIRED, drain=0)` and tears down
the cloud Link. This is JWT-token refresh only; it does **not**
revalidate device trust (there is no device trust on the cloud
Link) and is not used on paired-peer Links.

### 8.6 ConnectionPool

```rust
struct ConnectionPool {
    by_route: HashMap<Route, Channel>,
}

impl ConnectionPool {
    fn register(&self, route: Route, channel: Channel);
    fn get(&self, route: &Route) -> Option<Channel>;
    fn unregister(&self, route: &Route);
}
```

The pool is a route → Channel registry. It has no materialization
logic and no policy. Anyone who establishes a Channel registers it;
anyone who looks up a Channel calls `get`. When a Channel is
unregistered, the pool drops its `Arc`; if no caller holds a clone,
the underlying transport closes and in-flight gRPC streams on it
fail with `UNAVAILABLE`. See N-CN-1, N-CN-2.

### 8.7 ConnectionManager

```rust
struct ConnectionManager {
    pool: Arc<ConnectionPool>,
    routes: HashMap<HostId, BTreeSet<Route>>,
    active: HashMap<HostId, Route>,
}
```

`ConnectionManager` subscribes to `RoutingCore` events and is the
single component that decides which route to use for each peer.

**On `HostUp(host, route)`**:

1. Insert `route` into `routes[host]` (after the dedup check from
   N-R-2 has already discarded strictly-worse routes upstream in the
   routing core).
2. If `active[host]` is unset, or `route.len() < active[host].len()`,
   or (`route.len() == active[host].len()` and the existing active
   route has gone down — see below), consider a swap:
    - Obtain a Channel for the new route. For 1-hop, the Channel is
      already in the pool from §8.5. For multi-hop, materialize a
      tunnel (allocate a fresh `TunnelId.nonce`, build a
      `TunnelTransport`, complete end-to-end mTLS, wrap in a tonic
      Channel, `pool.register(route, channel)`).
    - Set `active[host] = route`.
    - Unregister the previous active route's Channel from the pool.
      This drops the Channel; in-flight gRPC streams on it fail with
      `UNAVAILABLE`; callers reconnect via `channel_to(host)`, which
      now hits the new active route.

**On `HostDown(host, route)`**:

1. Remove `route` from `routes[host]`.
2. If `active[host] == route`:
    - Unregister the Channel from the pool.
    - Clear `active[host]`. The next caller's `channel_to(host)` will
      pick a new shortest route from `routes[host]` (or fail if the
      set is now empty).

**External call surface**:

```rust
fn channel_to(host: HostId) -> Result<Channel>;
```

If `active[host]` is set, returns the pool's Channel for that route.
Otherwise picks the shortest known route from `routes[host]`,
materializes (1-hop already exists; multi-hop builds a tunnel),
registers, sets `active[host]`, and returns. If no routes are
known, returns an error.

**Policy properties**:

- Selection is **shortest route, FIFO ties**. No transport-flavour
  preference; direct-paired Links (route length 1) win over multi-hop
  naturally.
- **HostUp/HostDown are the only triggers** for re-evaluation. No
  background probing.
- **Make-then-break swaps** (the new Channel exists in the pool
  before the old is unregistered) avoid a transient outage when the
  new path fails to materialize.
- **Failed materialization does not fall back** to longer routes in
  v1. The error propagates; subsequent `HostDown` (originating from
  the link's owner, when the link is actually broken) will evict the
  bad route from `routes[host]`. See N-CN-7.

### 8.8 Startup re-establishment

On daemon startup, after `amux init` state is loaded:

1. The cloud-attach flow brings up the cloud Link (if cloud
   credentials exist). This is the standard outbound
   `RoutingService.Connect` to the cloud relay; HostUp events for
   the user's other online devices arrive as the cloud relay
   forwards them.

2. The daemon iterates its trust store. For each entry, it walks
   the `reachabilities` list and, for every `DirectTcp { addr }` or
   `Ssh { target }` reachability, attempts the corresponding direct
   Link establishment (Flow §8.5 from step 1). Multiple direct
   reachabilities per peer (e.g., DirectTcp and Ssh) each produce
   their own Link attempt; whichever succeed contribute routes to
   the routing graph. `Reachability::Cloud` entries require no
   action here; they become reachable as routing events propagate
   over the cloud Link.

Each successful Link establishment produces a `HostUp` and the
`ConnectionManager` picks up routes through the new Link.

Failed direct re-establishment is non-fatal: the peer remains
unreachable via that flavour until the underlying transport
recovers or the user re-pairs. The daemon may retry periodically;
specific retry/backoff is an implementation detail and not
specified here.

### 8.9 TunnelId and tunnel framing

Multi-hop runtime traffic uses tunnels. A tunnel is identified by:

```proto
message TunnelId {
  bytes initiator = 1;  // 16-byte host_id of the side that created the tunnel
  bytes nonce = 2;      // 16 random bytes (UUIDv4); fixed for the tunnel's lifetime
}
```

Direction is implicit. At any endpoint:
- `initiator == self.host_id` → outbound tunnel this daemon created.
- `initiator != self.host_id` → inbound tunnel hosted for that peer.

**Nonce generation**: the initiator picks `nonce` as **16 random
bytes (UUIDv4)** at tunnel creation, then keeps it fixed for the
tunnel's lifetime — including for all frames in both directions of
that tunnel. Nonces are not monotonic counters; they are random and
opaque. A 128-bit random space makes collisions across all tunnels
on all hosts negligible (birthday bound ~2⁶⁴). Nonces are not
persisted across daemon restarts; tunnels do not survive restarts,
and a fresh nonce is generated for any new tunnel after a restart.

`TunnelFrame`s flow inside the `Message` envelope of the first-hop
Link's `RoutingService.Connect` stream. Each forwarding hop pops the
next link from `dst` and forwards onward. At the endpoint (`dst`
empty), the receiver dispatches to its local tunnel pool by
`TunnelId`.

A tunnel's user-facing surface is a `TunnelTransport` implementing
`AsyncRead + AsyncWrite`. tonic wraps it into a Channel; gRPC runs
on top. The end-to-end mTLS layer (§4.4, §8.1) sits inside this
transport between the two endpoints.

**1-hop direct calls use no `TunnelFrame` wrapping.** They run raw
gRPC over the Link's Channel (which is itself the tonic Channel
wrapping the underlying socket). The "1-hop = direct, multi-hop =
tunneled" distinction is enforced by registration discipline (§8.5
registers the Link's Channel under `[L]`; multi-hop materialization
in §8.7 registers a tunnel-backed Channel under a longer route).
No `route.len() == 1` branch exists in code.

### 8.10 Tunnel lifecycle

A tunnel's lifetime is bound to its underlying byte stream's
lifetime. The protocol piggybacks on HTTP/2 / gRPC semantics over
the `TunnelTransport` rather than defining its own close/error
frames:

- **EOF / orderly close.** Either endpoint dropping the
  `TunnelTransport` closes the byte stream. Tonic propagates this
  as an HTTP/2 stream end, and in-flight gRPC calls receive a
  graceful end-of-stream. No explicit close frame in the
  `Message`/`TunnelFrame` envelopes.
- **Errors.** A broken transport (network failure, link teardown,
  forwarding hop closing) surfaces to gRPC callers as
  `tonic::Status::UNAVAILABLE`. No application-level error frame
  inside the tunnel; the gRPC status conveys it.
- **Idle timeout.** Tonic's keepalive (configured via
  `configure_tonic_endpoint_keepalive`, already used in
  `tunnel/pool.rs`) detects dead peers and tears the Channel down.
  No additional tunnel-level idle timer.
- **`TunnelFrame` payload size cap.** Implementations MUST cap
  `TunnelFrame.payload` at 64 KiB (matching HTTP/2's typical max
  frame size). A frame exceeding the cap is a protocol violation;
  whichever node observes it (forwarding hop or endpoint) closes
  the Link with `GoAway { reason: PROTOCOL_ERROR }`. Silent drop
  is not permitted (N-TN-7).
- **`GoAway` semantics.** Either side may emit a `GoAway` `Message`
  on a Connect stream to signal Link teardown. The receiver
  finishes in-flight calls within `drain_timeout_ms` (`0` for
  immediate, used on auth expiry per the cloud Link Reauth flow,
  §8.5) and stops initiating new ones. After drain, the Link's
  `TunnelTransport`s close, propagating UNAVAILABLE per above.
  `GoAwayReason` values are listed in the proto.
- **Backpressure.** Inherits HTTP/2 flow control end-to-end on each
  tunnel's `TunnelTransport`; intermediate Links' Connect streams
  each have their own HTTP/2 flow control independent of tunnels
  riding inside them. The `LinkRegistry` enforces a per-link
  pending-events cap (`PENDING_ROUTING_EVENT_LIMIT = 256`) on
  pre-snapshot routing events; if exceeded, the Link is closed
  with `LinkCloseReason::OutgoingQueueFull`.

### 8.11 Routing event propagation rules

When a Link comes up, the two endpoints exchange a **routing
snapshot** before streaming deltas:

1. Each side emits `HostUp(host, route)` for every `(host, route)`
   currently in its routing core, with the **route** as that side
   sees it from its own perspective (i.e., starting at one of its
   own outbound links).
2. Each side emits `SnapshotComplete` after its last `HostUp`.
3. After both `SnapshotComplete`s, normal delta propagation begins:
   `HostUp` / `HostDown` events flow as state changes.

The receiver, on every `HostUp(H, route_in)` arriving on link
`L_in`:

1. **Prepend** `L_in` to `route_in`, producing `route_local = [L_in,
   ...route_in]` (this is the receiver-perspective route to H).
2. **Validate** `route_local`:
   - Drop if `route_local.len()` exceeds the **hop cap** (default
     `8`; longer routes are not useful and indicate a loop or
     malicious advertisement).
   - No further structural check is possible: link names beyond
     `L_in` are in downstream namespaces (N-L-2), so name matching
     against the local `LinkRegistry` would be meaningless. Loop
     containment is enforced by the hop cap + split-horizon
     (step 4).
3. **Dedup** against existing routes for `H`: drop if `route_local`
   is strictly worse than an existing route (longer, with same
   trailing path to `H`).
4. If kept, insert into the routing core and **propagate** to
   *other* Links (split-horizon: never forward an event back
   through the link it arrived on).

For `HostDown(H, route_in)`: same prepend, same validation, but
the action is to remove `route_local` from the routing core and
forward to other Links. A "host gone" logical signal (`HostRemoved`
in `ClientService`'s subscription) fires only when the last route
for `H` is removed (N-R-3).

**Forwarding `TunnelFrame`s** (distinct from `HostUp` propagation):
when a `TunnelFrame` arrives, pop the next hop from `dst`. If that
link is not in the local `LinkRegistry`, drop the frame. Otherwise
forward via that link's writer (validated against
`PENDING_ROUTING_EVENT_LIMIT` backpressure on the link).

### 8.12 Host listing & reachability status

`ClientService` exposes a "what hosts can I see and how do they look
right now" surface to the UI. For each host the daemon knows about,
it reports:

- **Trust status**: `trusted` (in trust store) or `untrusted_but_online`
  (seen via cloud `HostUp` but no trust entry — UI prompts to pair).
- **Reachability status** (for trusted hosts): `reachable`,
  `unreachable` (with last error: SSH alias didn't resolve, TCP
  connect refused, etc.), or `unknown` (haven't tried recently).

The daemon populates reachability status lazily — typically on demand
when a dial is attempted — and caches failures. No active polling.
For trusted/local hosts, `reachable` means the host is local or has a
live route, `unreachable` means no live route exists and the
last stored-reachability attempt failed, and `unknown` means no live
route exists and no recent failure is cached.

The concrete `ClientService` surface is a host-list entry, not a raw
routing `Host`:

- `host_id` and display `name` are always present.
- `online` reports whether the host is currently present through
  routing; trusted peers that are only known from the trust store still
  appear with `online = false`.
- `version` and `capabilities` are present only for online hosts.
- `trust_status` is `trusted` for local/trust-store peers and
  `untrusted_but_online` for online cloud-routed hosts without a trust
  entry.
- `reachability_status` is present for trusted/local peers and omitted
  for untrusted-but-online peers.

The server owns untrusted inventory access:

- `ListHostsRequest.scope = ALL` returns normal inventory. Local
  Unix-socket / in-process callers may see untrusted online hosts;
  paired remote mTLS callers and metadata-less callers receive only
  trusted hosts.
- `ListHostsRequest.scope = PAIRING_CANDIDATES` returns local-only
  untrusted online cloud-routed candidates. Paired remote mTLS callers
  and metadata-less callers are rejected.
- `SubscribeHosts` applies the same untrusted filtering for remote and
  metadata-less callers.

The UI uses this surface to:

- Prompt the user to pair an untrusted-but-online host.
- Show a "connection broken — try re-pairing?" prompt when a trusted
  host's stored `Reachability` no longer works (e.g., SSH alias changed,
  paired peer's IP moved).
- Display a sensible status indicator next to each known host.

This is the surface that handles both the "untrusted-but-online"
case from N-P-5's asymmetric-trust scenario and the
"reachability-becomes-stale" case (e.g., user changes their SSH
config, peer moves networks).

### 8.13 Cross-cloud-user pairing

Pairing typically links two devices owned by the **same** cloud user —
this is the common case because `HostUp` discovery is per-`user_id`, so
each user only sees their own devices in the interactive picker.

**Cross-user pairing is allowed but unusual.** A user signed into the
cloud as User A on one machine and User B on another can still pair
those two machines via SSH or LAN-direct flows (which don't involve
the cloud at all). The pairing succeeds normally. Runtime traffic
between the pair must then use the non-cloud `Reachability` they
chose, because the cloud will not route between different users'
devices.

This isn't a bug or a special case — it falls out of the model. The
spec acknowledges it so future readers don't assume "paired implies
same user."

---

## 9. Testing

There is no "skip TLS" mode for network paths in tests. End-to-end
tests pre-pair daemons via a fixture:

```rust
async fn paired_daemons(n: usize) -> Vec<DaemonHandle> {
  // 1. Spawn n daemons; each generates its own keypair on startup.
  // 2. Pre-fill each daemon's trust store with every other daemon's
  //    pubkey + identity + reachability (typically Cloud or DirectTcp
  //    in tests; SSH transport is harder to test hermetically and is
  //    out of scope for the standard fixture).
  // 3. Hand back handles.
}
```

The production mTLS handshake is exercised on the network test paths.
SSH-paired runtime (`amux relay` + local Unix socket) is tested
separately where it's relevant; the standard `paired_daemons` fixture
uses network reachability so the dispatcher + TLS code is exercised.

The routing & connections layer (§8) is tested at two levels:

- **Routing-graph propagation**: `HostUp` / `HostDown` propagation
  across multi-hop topologies. Verify dedup (N-R-2), single-source
  origination (N-R-4), and prepend-on-forward semantics (N-R-5).
- **ConnectionManager swap behavior**: simulate route announcements
  and confirm shortest-route-wins (N-CN-3), make-then-break
  ordering (N-CN-5), and that failed materialization does not fall
  back (N-CN-7). In-flight stream behavior across a swap is
  exercised by holding open a long-running gRPC stream and
  observing that it terminates with `UNAVAILABLE` while a fresh
  call succeeds via the new Channel.

Multi-hop tunnel tests should also verify the `TunnelId` invariants
(N-TN-2 nonce uniqueness, N-TN-4 endpoint dispatch by initiator).

---

## 10. Invariants

The following invariants are binding. They are numbered so that
implementation and review can refer to them precisely.

### Identity and keys

**N-K-1.** Each device generates a long-lived keypair on first run; the
private key never leaves the device.

**N-K-2.** `host_id` is a random 128-bit (16-byte) value, generated on
first run, persisted locally, and independent of the pubkey.

**N-K-3.** Both the keypair and `host_id` persist across daemon restarts.

### Trust store

**N-T-1.** The trust store is local to each device. It is never sent to
the cloud, never copied to other devices, and never persisted on shared
storage.

**N-T-2.** Trust store entries take the form
`host_id → (pubkey, name, paired_at, reachabilities: Vec<Reachability>)`.
Multiple reachabilities per peer are allowed. The list contains local
outbound hints learned by pairing flows and is deduplicated; a trusted
peer may have an empty list.

**N-T-3.** Every host↔host mTLS handshake verifies the peer's pubkey
matches the trust-store entry for the claimed `host_id`. Mismatch →
reject.

**N-T-4.** Trust store entries are added **only** via successful pairing.

**N-T-5.** A `Reachability` describes how to bootstrap a connection
to a peer:
- `Cloud` — peer is reached via cloud-routed multi-hop tunnel; no
  direct Link is opened to the peer.
- `Ssh { target }` — spawn `ssh <target> amux relay` and run
  `RoutingService.Connect` on the resulting stdio to establish a
  Link. The remote `amux relay` bridges to the daemon's Unix
  socket. `target` is opaque (alias / config / port delegated to
  the user's `~/.ssh/config`).
- `DirectTcp { addr }` — TCP+mTLS connect to `addr` (the peer's
  external TCP listener, N-X-7) and run `RoutingService.Connect`
  to establish a Link.

A peer's trust entry may hold multiple reachabilities (e.g., a
peer paired by Cloud and later by DirectTcp). All
direct-flavour reachabilities are attempted at startup (N-CN-8).
Once Links are up, route selection at runtime is governed by
`ConnectionManager` policy (N-CN-3), not by the reachability list
directly.

**N-T-6.** Trust store entries are removed **only** via the local
revocation flow (§5.4). There is no silent trust eviction and no inbound
protocol message that can remove trust.

### Cloud awareness

**N-C-1.** **Cloud servers cannot pair.** Cloud servers do not host
`PairingService`. Attempts to route a `PairingService` call to a cloud
server are rejected.

**N-C-2.** The cloud learns: `host_id`, JWT-derived `user_id`, online
status, friendly name, capabilities, routing metadata, and the routing
graph it is asked to relay.

**N-C-3.** The cloud does **NOT** learn: device **private keys**
(never leave the device), PINs, pairing tokens, trust-store contents,
pairing-mode state, or paired-peer gRPC payloads (end-to-end mTLS
inside multi-hop tunnels makes them opaque to the relay). Device
pubkeys are public/non-secret, but the normal v1 cloud-routing path
does not require or receive them; security is based on private-key
control, OOB pubkey verification, and local pinning, not on hiding
pubkeys from the cloud.

**N-C-4.** Pairing requires **no** cloud-issued token. The OOB-verified
pairing flow itself is the authorization that links two device
`ServerUserState`s.

### Transport security

**N-X-1.** Runtime connections between paired devices use mTLS
with **X.509 self-signed Ed25519 certificates** pinned against the
local trust store (§10 Implementation defaults). The mTLS layer's
position depends on route length: at the socket for 1-hop direct
paired-TCP Links; inside the `TunnelTransport`, end-to-end between
the two endpoints, for multi-hop tunnels. The cloud-relay Link
uses standard server-auth TLS + JWT (N-X-3a), not pinned
mTLS. SSH-paired peers take a non-TLS path for the underlying Link
(see N-X-5 and N-P-8). Pairing-flow connections use the per-flow
configurations in N-P-4.

**N-X-2.** Network transports (TCP, the cloud relay's TCP+TLS, SSH
stdio) are opaque-byte at the application layer. Encryption and
peer authentication for paired peers are at the TLS layer (mTLS
for runtime; per-flow configurations during pairing). For
multi-hop tunnels, hop-by-hop Link encryption protects against
off-path observers and the end-to-end mTLS inside the tunnel
protects the gRPC payload from intermediaries.

**N-X-3.** Trust for **device-to-device** connections is via local
pinning in the trust store: the peer's TLS cert pubkey must match
the pubkey recorded in the local trust-store entry for the claimed
`host_id`. Device certs are **never** validated against a public
certificate authority.

**N-X-3a.** The cloud-relay link is the exception: the cloud relay
presents a standard public-CA-issued certificate for its hostname
(e.g., `s1.amux.sh`), and daemons validate the certificate chain and
hostname using normal WebPKI/CA validation. This is **server-auth TLS +
JWT**, not pinned mTLS. JWT-in-metadata identifies the cloud *user*
(multi-tenancy); the TLS layer identifies the cloud *service*. The
"no CA validation" rule of N-X-3 applies only to device pinned
certs.

**N-X-4.** gRPC is client-server asymmetric, so each direction of
calls between a host pair requires its own outbound Channel. For
direct-paired peers this typically means two Links (one initiated
by each side) with their own underlying sessions: separate
mTLS-over-TCP sockets, or separate `ssh ... amux relay` children.
For peers reachable only via Cloud, each direction is a separate
multi-hop tunnel through the cloud. Links and tunnels are
established lazily — the B→A direction materializes only when B
first calls A (or at startup, §8.8).

**N-X-5.** There is no "skip TLS" mode anywhere in the codebase for
network paths. Every network connection runs through a TLS
handshake. The **responder always presents its device self-signed
cert** regardless of flow; what differs is the initiator's
verification policy: mTLS pinned-against-trust-store for paired-peer
runtime, WebPKI for the cloud-relay Link (cloud side), pinned-
against-QR-pubkey for QR pairing, **no verification** for PIN
pairing (SPAKE2+AEAD inside provides auth). The exceptions are
paths where SSH substitutes for our encryption layer entirely:
**SSH pairing** (no TLS; SSH provides encryption + authentication
for the one-time identity exchange, see N-P-4) and **SSH runtime**
(no TLS; bytes flow SSH → `amux relay` → local Unix socket; SSH
provides encryption + remote-user authentication, Unix socket file
permissions provide local access control).

**N-X-6.** Every device daemon on Unix runs a local Unix socket at
`Config.socket_path` (`default_socket_path()`), mode `600`, owned by
the daemon's user. It is always on and feeds the Trusted Server
directly (no TLS, no dispatcher). OS file permissions are the access
gate. It is used by local CLI / App callers, `amux pair-recv` (writing
trust entries), and `amux relay` (forwarding bytes from SSH-paired
peers). There is no sibling SSH relay socket in v1.

**N-X-7.** Every device daemon may run an **external TCP listener**
controlled by the `tcp_port` config setting. `Some(port)` → bind to
`0.0.0.0:<port>` and feed the tunnel dispatcher (Trusted Server and
Pairing Server reachable subject to N-G-5). `None` → no external
listener. `tcp_port` is **unset by default**; `amux init` does not
write a value. Users explicitly opt in to LAN-direct reachability
by configuring `tcp_port`.

**N-X-8.** When `tcp_port` is `None`, LAN-direct responder flows
(`amux pair --listen`, `amux pair` with the intent of accepting a
direct-TCP pair attempt) fail explicitly with a useful error
("set `tcp_port` in config, or use cloud / SSH pairing"). Daemons do
not silently fall back. Initiator flows (`amux pair --connect <ip:port>`
on the other side) are unaffected by this side's `tcp_port` setting.

**N-X-9.** **`host_id` ↔ pubkey binding at mTLS acceptance** (device-to-device
runtime connections only). After a successful **pinned** mTLS
handshake on an inbound or outbound device-to-device runtime
connection (paired-direct TCP, or end-to-end mTLS inside a
multi-hop tunnel), the daemon binds the connection to a single
authenticated `peer_host_id` — the trust-store entry whose pubkey
matches the presented cert. Any subsequent `Hello.host.host_id`
exchanged over `RoutingService.Connect` on that connection **must**
equal that bound `peer_host_id`; mismatch → reject. This prevents
a paired peer from impersonating a different peer's `host_id` at
the routing layer after authenticating. The invariant does not
apply to the cloud Link (which uses WebPKI server-auth + JWT, not
pinned mTLS — there is no `peer_host_id` to bind from the cert).

### Tenancy

**N-MT-1.** Cloud servers are multi-tenant. `ServerUserState` is keyed
by `user_id`.

**N-MT-2.** Non-cloud (device) servers are single-tenant. They hold one
`ServerUserState`.

**N-MT-3.** Pairing links two single-tenant `ServerUserState` instances.
It is a 1:1 trust relationship between two devices.

### Pairing

**N-P-1.** `PairingService` is the **only** RPC callable without prior
mutual trust.

**N-P-2.** `PairingService` calls are gated by local pairing-mode state
on the responder. Not in pairing mode → reject with
`NOT_IN_PAIRING_MODE`.

**N-P-3.** Pairing mode is a time-bounded local state (~5 minutes)
holding at most one active PIN or token at a time. **The PIN or token
is consumed by the first successful pairing**: pair-mode ends
immediately on success, the secret is invalidated, and any subsequent
attempts to use it are rejected. This prevents a second peer from
racing in on the same PIN behind the user's back. Invoking another
pairing responder (`amux pair`, `amux pair --qr`) while pair-mode is
already active fails with a useful error — the user must wait for
expiry or explicitly cancel the existing pair-mode first.

**N-P-4.** Three pairing flows, each with its own peer-authentication
mechanism during pairing:

- **QR**: server-authenticated TLS (responder presents cert; initiator
  verifies against QR-known pubkey; initiator is anonymous at the TLS
  layer). The initiator is authenticated by the one-shot token inside
  the TLS channel.
- **SSH**: SSH provides encryption + authentication for the one-time
  exchange. Identity is exchanged via SSH stdin/stdout. **No TLS**
  (SSH is wholly outside the gRPC stack).
- **PIN**: The responder presents its **device cert** (the same
  self-signed cert it uses for paired-peer mTLS). The initiator
  presents no client cert and **does not verify** the responder's
  cert (it has no pubkey known out-of-band to verify against).
  Neither side is authenticated at the TLS layer; TLS provides
  only transport encryption. SPAKE2 with the PIN as the shared
  secret runs inside (§5.2.1), deriving per-direction AEAD keys
  via HKDF-SHA256 over the transcript. The identity exchange is
  AEAD-sealed (ChaCha20-Poly1305) with those keys. SPAKE2 is what
  provides mutual authentication; the TLS cert presented is
  incidental. Using the device cert (rather than a throwaway) is
  what keeps the dispatcher's TLS config uniform across
  concurrently-arriving paired-peer streams.

**N-P-5.** After successful pairing, both sides update their trust
store for the other's `host_id`:

- New `host_id`: insert a fresh entry with the exchanged
  `(pubkey, name, paired_at)` and any reusable reachability learned
  from the flow. Cloud pairing stores `Reachability::Cloud`. Direct
  TCP initiators store the listener address they dialed as
  `Reachability::DirectTcp { addr }`; direct TCP responders store no
  reachability unless a future protocol flow explicitly advertises a
  reusable listener address.
- Existing `host_id`, **same pubkey** (re-pairing with a known
  device, possibly via a new flow): update `name`, `paired_at`, and
  append any reusable reachability learned by the flow to
  `reachabilities` (dedup duplicates). Existing Links / tunnels stay
  up.
- Existing `host_id`, **different pubkey** (key rotation case): the
  trust entry's pubkey is **replaced**. The daemon **tears down
  every active Link and tunnel for that `host_id`** before
  accepting new connections under the new pubkey, except that the
  active Pairing Server tunnel carrying the replacement may be
  preserved until its completion acknowledgement is delivered.
  Reason: the old key may have been replaced because it was
  compromised; existing connections authenticated under the old key
  must lose authority.
  The new entry takes the exchanged `(pubkey, name, paired_at)` plus
  any reusable reachability learned by the flow. The user's act of
  re-pairing is the consent for this replacement; no additional
  prompt is required.

Pairing identity exchange is **not atomic across the two peers**.
The sequence is: A sends its identity → B receives and writes its
trust entry for A → B sends its identity → A receives and writes
its trust entry for B. If the second message is lost mid-exchange,
asymmetric trust may result (B trusts A but not vice versa). The
unpaired side will continue to see the peer as untrusted-but-online
and can re-pair; no rollback is attempted.

**N-P-6.** The one-shot token for QR pairing is single-use and
short-lived (~5 minutes). After consumption or expiry it cannot be
reused.

**N-P-7.** The cloud is never told about pairing intent. When pairing
traffic flows through cloud routing, the cloud sees opaque tunnel bytes
that happen to contain pairing protocol traffic. Pairing-mode is a
**local** property of the responder daemon.

**N-P-8.** After pairing, each entry in the peer's
`reachabilities` list determines a way for the daemon to reach this
peer. The daemon establishes a Link for every direct-flavour
reachability:
- `Cloud` — no direct Link to the peer; the peer is reached via the
  cloud-routed multi-hop tunnel (end-to-end mTLS inside; cloud Link
  is hop-by-hop TLS + JWT).
- `DirectTcp { addr }` — a direct TCP+mTLS Link to `addr`.
- `Ssh { target }` — a direct SSH-stdio Link via
  `ssh <target> amux relay` (SSH provides encryption; no TLS at the
  daemon level).

The pairing-specific bootstrap configurations (one-shot token +
QR-pinned-pubkey server-auth verification for QR; SPAKE2 + AEAD
over device-cert TLS with no client-side verification for PIN; SSH
stdio for SSH pairing) are used **only** during the bootstrap, not
at runtime. Once Links are up, the
`ConnectionManager` picks routes according to N-CN-3.

**N-P-9.** Pair attempts where the claimed peer `host_id` equals the
responder's own `host_id` **or** where the claimed peer pubkey
equals the responder's own pubkey are rejected with `SELF_PAIRING`.
A daemon cannot pair with itself, and a daemon cannot accept a peer
masquerading as itself by host_id or by key.

### Service gates

**N-G-1.** `ClientService`, `AgentService`, and `RoutingService` are
co-hosted on the daemon's **Trusted Server**. The Trusted Server has
entry points treated with equivalent runtime authority:
- **Local Unix socket** at `Config.socket_path`, mode `600`, owned by
  the daemon's user. No TLS; OS file permissions are the gate.
- **SSH relay through local Unix socket** — `amux relay` connects to
  the same `Config.socket_path` after SSH authenticates the remote OS
  user. This is local-equivalent in v1.
- **mTLS-verified runtime connections** — paired peers reaching us
  via the external TCP listener (after dispatcher TLS handshake with
  pinned cert) or terminated end-to-end inside a multi-hop tunnel.

**N-G-2.** Pairing is the runtime trust boundary. A paired peer
reaching the Trusted Server via mTLS has full runtime authority, but
daemon-local pairing administration and trust mutation remain local-only
per N-S-2. SSH `amux relay` reaches the local Unix socket and is
local-equivalent.

**N-G-3.** `RoutingService` is hosted by both the cloud relay and
each device daemon's Trusted Server. The cloud's instance
authenticates callers via JWT in metadata. The daemon's instance is
reached via Unix socket ingress (OS-gated local or SSH relay) or via
mTLS-verified runtime connections from paired peers. Every direct
daemon-to-daemon connection — cloud-relay, paired-direct-TCP,
paired-SSH — establishes a Link by calling `RoutingService.Connect` on
the other side after the underlying handshake completes.

**N-G-4.** The cloud's `RoutingService` instance authenticates callers
via JWT, attached as metadata on `Connect`. The daemon's
`RoutingService` instance authenticates callers via the Trusted
Server's existing entry-point auth — Unix socket file permissions
after local access or SSH user authentication, or mTLS-verified
pinned client cert (paired peer). Per N-G-6, auth is decided once at
connection acceptance, not per RPC.

**N-G-5.** `PairingService` is hosted on the daemon's **Pairing
Server**. A tunnel is routed to the Pairing Server only when no client
cert was presented at the TLS handshake AND pairing-mode is active on
the responder (see N-P-2). Otherwise the dispatcher closes the tunnel.

**N-G-6.** **Transport auth is decided once at tunnel / connection
acceptance.** Runtime RPCs assume "if I'm being called, the caller is
authorized." The v1 exception is daemon-local pairing administration
and trust mutation on `ClientService`, which checks the accepted ingress
class and rejects paired remote mTLS callers. Per-frame auth cost is
zero. `Reauth` / `ReauthAck` is for JWT **refresh** on the long-lived
`RoutingService.Connect` stream, not for revalidation.

**N-G-7.** Each daemon hosts exactly two gRPC Server instances: a
**Trusted Server** (`ClientService` + `AgentService` + `RoutingService`
+ future trusted services, fed by the local Unix socket and by
mTLS-verified runtime connections from paired peers) and a **Pairing
Server** (`PairingService`, fed by pre-trust runtime connections gated
on pairing-mode).

### Routing graph (N-R)

**N-R-1.** A `HostUp { host: H, route: R }` event means "the sender
can reach `H` via route `R` (from the sender's perspective)." Hosts
do not announce themselves via `HostUp`; endpoint identity is
established at link establishment by Hello/HelloAck.

**N-R-2.** Each daemon stores multiple routes per host (a
`BTreeSet<Route>` or insertion-ordered equivalent). `HostUp` is
deduplicated on receive: a strictly worse route — longer than an
existing route to the same host, with the same trailing path —
is dropped, not stored, and not propagated onward.

**N-R-3.** `HostDown { host: H, route: R }` invalidates the specific
`(host, route)` pair. A "host gone" reachability signal fires only
when the last route for that host is removed.

**N-R-4.** Originating `HostUp` / `HostDown` events for a host come
*only* from a daemon that owns an immediate link to that host. Other
daemons propagate (prepending their own incoming link name as they
relay) but never originate. No component synthesizes events in
response to downstream observations (e.g., a failed tunnel
materialization is not converted to a `HostDown`).

**N-R-5.** Routes are sequences of link names. The first link is the
next hop. Each forwarding hop pops one link from `TunnelFrame.dst`
and forwards onward.

**N-R-6.** `RoutingService.Connect` is run only on direct
daemon-to-daemon connections (cloud-relay, paired-direct-TCP,
paired-SSH). Learned-via-routing reachability does not trigger new
`RoutingService.Connect` calls. Connection count scales with edges,
not nodes.

**N-R-7.** **Snapshot on Link bring-up.** When a Link reaches the
"active" state, each side streams `HostUp(host, route)` for every
`(host, route)` it currently holds, followed by
`SnapshotComplete`. After both sides emit `SnapshotComplete`,
normal delta propagation begins.

**N-R-8.** **Split-horizon.** A daemon must not forward a `HostUp`
or `HostDown` event back through the Link on which it was received.

**N-R-9.** **Prepend-on-forward.** When a daemon receives a `HostUp`
or `HostDown` on Link `L_in`, it constructs its local
receiver-perspective route by prepending `L_in` to the route in the
event before storing or forwarding.

**N-R-10.** **Hop cap on receive.** A `HostUp` is dropped if its
receiver-perspective route exceeds the hop cap (default 8; see §10
Implementation defaults). This is the only structural check on a
route at receive time: link names are local to each hop's namespace
(N-L-2), so a name-based loop-detection scan over the full route
would produce false positives. Loop *prevention* is via the hop cap
plus split-horizon (N-R-8); loop *amplification* is bounded by the
hop cap; dedup (N-R-2) handles the redundancy that loops generate
before they hit the cap.

**N-R-11.** **Forward-frame next-hop validity.** When forwarding a
`TunnelFrame`, the daemon pops the next link from `dst`; if that
link is not in its `LinkRegistry`, the frame is dropped.

**N-R-12.** **Routing core size caps.** The routing core retains
at most 16 routes per `host_id` and at most 1000 distinct
`host_id`s (see §10 Implementation defaults). On overflow:
- Excess routes for a host: drop the oldest non-active route on
  insert.
- Excess hosts: drop the oldest host with no `active_route` and no
  recent client-visible activity on insert; hosts present in the
  local trust store are exempt and never evicted.

The caps bound memory growth from a compromised cloud injecting
bogus `HostUp` events (§3.1).

### Links (N-L)

**N-L-1.** A **Link** is a bidi `RoutingService.Connect` stream
between two adjacent daemons over a single underlying transport:
cloud-relay TCP+TLS, paired-direct TCP+mTLS, or paired-SSH stdio.

**N-L-2.** Link names are assigned at Hello/HelloAck (responder
assigns; both endpoints of the Link use the same name for it).
**Link-name uniqueness is local**: a name is meaningful only
within the `LinkRegistry` of the node whose perspective a given
route hop refers to. Two different daemons may independently
assign the same link name to unrelated Links of their own; this is
not a collision because each hop in a route is interpreted at that
hop's owning node. Routes never carry "global" link identifiers.

**N-L-3.** Direct Link establishment order is: underlying handshake,
`RoutingService.Connect` (Hello/HelloAck), `LinkRegistry`
registration, `ConnectionPool` registration of the Channel keyed by
the 1-hop route `[link_name]`, **then** `HostUp` emission. The
`HostUp` must be emitted after the Channel is registered in the
pool, so subscribers reacting to the `HostUp` find the Channel.

**N-L-4.** A Link's underlying socket hosts a single tonic Channel
that multiplexes gRPC for all 1-hop service calls between the two
endpoints (`RoutingService`, `AgentService`, `ClientService` over
peer connections, etc.) via HTTP/2 streams. No `TunnelFrame`
wrapping on 1-hop calls.

### Connection pool & manager (N-CN)

**N-CN-1.** `ConnectionPool` is a `Route → Channel` registry. It
holds no materialization logic and no policy.

**N-CN-2.** `materialize(route) -> Channel` and
`pool.register(route, channel)` are separate operations. The
connection-establishment code (Link setup for 1-hop, tunnel
construction for multi-hop) is responsible for both.

**N-CN-3.** `ConnectionManager` holds one `active_route` per peer.
Selection policy: **shortest route wins**; ties broken
first-known-first. No transport-flavour preference; direct
connections (length 1) win over multi-hop naturally.

**N-CN-4.** `HostUp` / `HostDown` events are the *only* triggers for
`ConnectionManager` re-evaluation. The manager does not probe routes
ahead of `HostUp` arrival.

**N-CN-5.** Swaps are make-then-break: materialize the new Channel
and register it in the pool first; flip `active_route[peer]`; then
`pool.unregister(old_route)`. The old Channel drops; in-flight gRPC
streams on it fail with `UNAVAILABLE`; callers reconnect via
`channel_to(peer)`.

**N-CN-6.** At most one *active* route per peer at any moment. The
pool may transiently hold the prior Channel until `unregister` runs;
this is internal bookkeeping, not parallel use.

**N-CN-7.** Failed materialization does not trigger automatic
fallback to longer routes in v1. The error propagates; if the route
is genuinely unreachable, a subsequent `HostDown` (originating from
the link's owner, per N-R-4) will evict the bad route from
`routes[peer]`.

**N-CN-8.** On startup, the daemon iterates its trust store and
attempts a direct-Link establishment for each entry with
`Reachability::DirectTcp` or `Reachability::Ssh`. `Reachability::Cloud`
entries require no special action; the cloud-attach flow brings up
the cloud Link separately and routing events propagate.

**N-CN-9.** Active-connection teardown on revocation. On
`trust.remove` for host `H`, the daemon synchronously unregisters every
Route in `active_route` or `ConnectionPool` whose terminus is `H`,
drops corresponding `TunnelTransport`s, closes active Links for `H`,
and tombstones affected `TunnelId`s for the standard window.

### Tunnels (N-TN)

**N-TN-1.** `TunnelFrame`s exist *only* for multi-hop routes (length
≥ 2). 1-hop direct calls use raw gRPC over the Link's Channel.

**N-TN-2.** `TunnelId { initiator: HostId, nonce: [u8; 16] }`.
`nonce` is **16 random bytes (UUIDv4)** chosen at tunnel creation
and fixed for the tunnel's lifetime; used by both directions of the
tunnel's frames. No target field; the target is implicit at the
empty-`dst` endpoint.

**N-TN-3.** `TunnelFrame`s travel inside `Message` envelopes on the
first-hop Link's `RoutingService.Connect` bidi stream. Each
forwarding hop pops the next link from `dst` and forwards onward.

**N-TN-4.** Endpoint dispatch: when `dst` is empty, the receiving
daemon looks up the tunnel by `TunnelId`. `initiator == self` →
outbound tunnel; `initiator != self` → inbound tunnel hosted for
that peer.

**N-TN-5.** A tunnel's `TunnelTransport` implements
`AsyncRead + AsyncWrite`. tonic wraps it into a Channel; gRPC runs
on top, unchanged. End-to-end mTLS sits *inside* the tunnel between
the two endpoints, so intermediaries cannot read the gRPC payload.

**N-TN-6.** **Tunnel close = byte-stream close.** There is no
explicit "close" `TunnelFrame`. Either endpoint dropping the
`TunnelTransport` ends the underlying byte stream; tonic propagates
this as HTTP/2 stream end, surfacing to gRPC callers as graceful
end-of-stream or `UNAVAILABLE` depending on whether they were
mid-call.

**N-TN-7.** **`TunnelFrame.payload` size limit.** Implementations
MUST enforce a maximum `TunnelFrame.payload` size of 64 KiB (see
§10 Implementation defaults). A `TunnelFrame` whose payload exceeds
the cap is a protocol violation: a forwarding hop that observes it
closes the Link with a `GoAway { reason: PROTOCOL_ERROR }`; an
endpoint receiving one similarly closes the Link. Silent drop is
not permitted.

**N-TN-8.** **`GoAway`.** Either side of any Link may emit a
`GoAway { reason, drain_timeout_ms }` `Message` to signal teardown.
The receiver finishes in-flight calls within `drain_timeout_ms`
(`0` means immediate, used on auth expiry), refuses new calls,
then closes. After drain, all tunnels riding on the Link surface
`UNAVAILABLE` per N-TN-6.

### Platform scope (v1)

**N-S-1.** Phones are cloud-mode only in v1. Direct phone↔desktop
pairing (LAN-direct or SSH) is not supported.

**N-S-2.** Local clients (CLI, App) and paired remote peers reach the
Trusted Server with equivalent runtime authority. The only v1
exception is daemon-local pairing administration and trust mutation:
`StartPairing`, `GetPairingStatus`, `CancelPairing`, `PairPeer`,
`PairPinCloudPeer`, `PairQrCloudPeer`, `ListPeers`, `GetPeer`, and
`Unpair` are accepted from local Unix-socket / in-process admin callers.
SSH `amux relay` connects through the same local Unix socket and is
local-equivalent. These RPCs are rejected from paired remote mTLS
transports. Per-peer or per-method runtime authorization (e.g.,
read-only peers) is deferred to a future revision.

### Implementation defaults

These values are **normative for v1**: any v1 implementation MUST
use them as specified to interoperate. Future revisions may tune
them with a protocol-version bump. Where a value matches an
existing code constant, both are listed for cross-reference.

**Crypto suite (v1)** — these are the only permitted values for
interoperability:
- TLS version: **TLS 1.3 only** for all device-to-device paths.
- Device cert format: **X.509 self-signed only**. The cert carries
  the device pubkey in `SubjectPublicKeyInfo`. RFC 7250 raw public
  keys are not permitted in v1.
- Device key type: **Ed25519** for signing the self-signed cert.
- Public key wire encoding (e.g., `bytes pubkey` in pairing
  messages): **32-byte raw Ed25519 public key** as defined by
  RFC 8032 §5.1.5.
- Private key file format (`device.key`): **PKCS#8 v1 DER**
  encoding of the Ed25519 private key (RFC 8410), file mode `600`.
  Not PEM-wrapped at the file level; raw DER on disk.
- TLS ECDHE: **X25519** (negotiated by TLS 1.3).
- SPAKE2 variant: **RFC 9382 SPAKE2** over **Curve25519**.
- KDF (SPAKE2 → AEAD session key): **HKDF-SHA256** with the salt
  `"amux-pair-spake2-v1"` and info strings as in §5.2.1.
- SPAKE2-AEAD for `sealed_identity`: **ChaCha20-Poly1305** (mobile-
  friendly; phones are in scope).
- Cloud-relay TLS: standard **WebPKI/CA** certificate-chain and
  hostname validation for the configured cloud hostname (e.g.,
  `s1.amux.sh`). See N-X-3a.

**Routing / wire limits**:
- Hop cap: **8** (N-R-10).
- `TunnelFrame.payload` max: **65,536 bytes (64 KiB)** (N-TN-7).
- Per-Link pending-routing-events cap: **256** (matches
  `PENDING_ROUTING_EVENT_LIMIT` in `link_registry.rs`); exceeded →
  `LinkCloseReason::OutgoingQueueFull`.

**Cloud-Link Reauth timing** (matches existing code constants):
- Refresh window before JWT `exp`: **300s**
  (`ROUTING_AUTH_REFRESH_BEFORE_EXPIRY`).
- `ReauthAck` response timeout: **15s**
  (`ROUTING_AUTH_REAUTH_RESPONSE_TIMEOUT`).
- Drain timeout on auth expiry: **0 ms**
  (`ROUTING_AUTH_EXPIRED_DRAIN_TIMEOUT_MS`).

**Pairing limits**:
- PIN format: **6 decimal digits** (matches existing pair flow).
- Pair-mode TTL: **~5 minutes**.
- SPAKE2 attempt cap per pair-mode window: **5** failed attempts,
  then pair-mode auto-cancels and the PIN is invalidated.
- TLS-handshake rate limit per source IP on the external TCP
  listener: **10/minute**, sliding window.
- Concurrent TLS handshakes on the external TCP listener: **128**.

**Cloud-Link resource caps** (bound the blast radius of a
compromised cloud, per §3.1):
- New inbound tunnels arriving via the cloud Link: **30/minute**,
  sliding window. Exceeded → drop the excess `TunnelFrame`s with
  the cloud Link kept up (it's still doing legitimate routing for
  other tunnels). Cloud-Link classification is bound to Link
  establishment metadata, not to mutable routing-table retention.
- Stored routes per `host_id` in the routing core: **16**.
  Eviction on insert: drop the oldest non-active route (the one
  least likely to be the current `active_route` in
  `ConnectionManager`).
- Stored hosts in the routing core: **1000**. Eviction on insert:
  drop the oldest host that has no `active_route` and no recent
  client-visible activity. Hosts in the local trust store are
  exempt from this cap (a paired peer is never evicted).
- Client-visible activity is considered recent for **5 minutes**
  for routing-host eviction.

**On-disk paths**:
- Data dir: `paths::default_data_dir()` →
  `$XDG_DATA_HOME/amux` on Unix (fallback `~/.local/share/amux`) and
  `%APPDATA%\amux` on Windows. Mode `700`. Contains `device.key`,
  `host_id`, `trust.json`.
- State path: `paths::default_state_path()` →
  `$XDG_STATE_HOME/amux/state.yaml`. Mode `600` (non-secret, but
  private by default).
- Log path: `paths::default_log_path()` →
  `$XDG_STATE_HOME/amux/amux.log`.
- All sensitive files (`device.key`, `host_id`, `trust.json`) are
  mode `600`, written atomically via temp-then-rename.

**Audit log categories** (structured tracing entries; default sink
= stderr via the existing `init_tracing` worker; configurable via
`AMUX_LOG`):
- `pairing.start`, `pairing.success`, `pairing.failure`,
  `pairing.cancel`
- `auth.mtls_handshake_failure` (pinned-cert mismatch, etc.)
- `auth.jwt_failure` (Reauth failures, cloud-Link refusals)
- `trust.insert`, `trust.update`, `trust.replace` (pubkey change),
  `trust.remove`
- `link.up`, `link.down` (with `host_id` and link name)
- `client_service.disruptive_call` (`Shutdown`, `Suspend`, etc.)

**Cloud authentication endpoints** (existing OAuth 2.0 device flow,
`auth/oauth.rs`):
- `${cloud_url}/connect/authorize`
- `${cloud_url}/connect/token`
- `${cloud_url}/connect/deviceauthorization`
- Client ID: `"cli"`.
- Scopes: `openid`, `offline_access`, `api`.
- Refresh token stored at `auth_file_path(&config.state_path)`.

**Protocol versioning**: `Hello.supported_protocol_versions` is the
sender's full list. `PROTOCOL_VERSION` for this revision is
**`u32 = 5`** (bumping from `4`, to reflect the breaking changes in
§6.3). The responder intersects with its own
`supported_protocol_versions`; if empty, `HelloAck.error` carries
a `ProtocolVersionMismatch` detail and the connection is closed.
Otherwise `HelloAccepted.protocol_version` is the maximum common
version. `UpdateRequired` (in any `Error.details`) is the soft
signal "your peer wants you to upgrade."

**ClientService surface**: see the `service ClientService` block in
`amux.proto`. In addition to host/agent CRUD, session subscribe/input,
hooks, debug, shutdown/suspend/resume, the local Unix-socket /
in-process admin surface includes pairing administration and trust
mutation RPCs: `StartPairing`, `GetPairingStatus`, `CancelPairing`,
`PairPeer`, `PairPinCloudPeer`, `PairQrCloudPeer`, `ListPeers`,
`GetPeer`, and `Unpair`. These are rejected on paired remote mTLS
transports; SSH `amux relay` uses the local Unix socket and is
local-equivalent.

---

## 11. Deferred to future revisions

The following are intentionally out of scope for this revision and
will be designed in subsequent revisions:

- **Key rotation.** Refreshing a device's keypair while preserving its
  `host_id`. Likely involves a signed-by-old-key transition message
  propagated to paired peers.
- **Identity recovery.** Regaining access after losing all paired
  devices. New device joins via cloud login + a "new chain of trust
  begins here" event. The v1 recovery path is to re-pair from any
  surviving peer; cross-device propagation is deferred (see
  `notes/AUTO_PAIRING.md`).
- **Per-peer / per-method authorization.** v1 grants paired peers full
  authority over the Trusted Server (equivalent to local users).
  Future work may introduce finer-grained policy: read-only peers,
  per-method gates, audit trails. Pairing remains the trust boundary;
  this would refine what authority each paired peer is granted.
- **Cloud ↔ pairing UI interaction.** Should the cloud account UI
  display a user's paired devices? If yes, what does cloud learn? If
  no, where does the user see trust topology?
- **mDNS / auto-discovery on LAN.**
- **Per-interface bind for `tcp_port`.** v1 binds the external TCP
  listener to `0.0.0.0`. A future version may allow restricting to a
  specific interface (e.g., a Tailscale IP) for finer-grained network
  exposure control.
- **SSH process lifecycle.** v1 keeps one `ssh <target> amux relay`
  child alive per SSH-paired peer (one child per Link, multiplexing
  gRPC over its bidi stdio). A future version may introduce SSH
  `ControlMaster` multiplexing or alternative SSH session-management
  strategies if performance or churn use cases warrant.
- **OS-keychain private-key integration.** v1 stores the device
  private key in a single mode-`600` file (§10 Implementation
  defaults). A future revision may integrate per-platform keychains
  (macOS Keychain, Windows DPAPI, Linux Secret Service) for stronger
  at-rest protection.
- **Trust-store migration / corruption recovery.** v1 uses an atomic
  write-temp-rename and refuses to start on detected corruption. A
  future revision may add automatic recovery or backups.

---

## 12. Reference implementation map (non-normative)

This section is a guide for implementers, not part of the normative
spec. It maps the current reference layout for `crates/amux/src/`,
the key component APIs, and how bytes actually flow on direct
(1-hop) vs multi-hop call paths. Implementations may organize
modules differently as long as they satisfy the normative §10
invariants.

### 12.1 Target file structure

Files introduced or relocated by this production-readiness pass are
marked `*NEW*`; existing files are plain text.

```
crates/amux/src/
  lib.rs                          public exports for client/server/config,
                                  setup, user-facing pairing helpers, routing
                                  host/listing types, and update/debug APIs
  config.rs                       + tcp_port (Option<u16>, default None)
  paths.rs state.rs user_state.rs setup.rs suspend.rs update.rs
  server.rs dispatcher.rs audit.rs resource_limits.rs
  identity.rs                     DeviceIdentity, keypair/host_id files,
                                  self-signed certs, pinned mTLS verifiers
  trust.rs                        *NEW* TrustStore, TrustEntry, Reachability,
                                  SharedTrustStore, trust.json persistence
  debug/

  protocol/
    mod.rs                          PROTOCOL_VERSION = 5
    error.rs

  transport/
    tcp.rs tls.rs unix.rs io.rs memory.rs single_io.rs
    ssh.rs                          spawn `ssh <target> amux relay`

  routing/
    mod.rs
    core.rs                         RoutingCore: BTreeSet<Route> per host,
                                    N-R-12 eviction, snapshot generation
    events.rs host.rs link.rs link_registry.rs route.rs types.rs wire.rs
    connect/
      mod.rs                        Connect implementation, Hello/HelloAck,
                                    snapshots, propagation, Reauth

  tunnel/
    mod.rs
    types.rs                        TunnelId { initiator, nonce: [u8; 16] }
    transport.rs                    TunnelTransport (AsyncRead + AsyncWrite)
    pool.rs                         per-TunnelId state, inbound dispatch

  connection.rs                   outbound peer-Channel layer: ConnectionPool,
                                  ConnectionManager, route runtime cleanup,
                                  direct/tunnel Channel materialization

  pairing/                        *NEW* relocated pairing helpers
    mod.rs                          PairMode (token-or-PIN + TTL + attempt counter)
    qr.rs                           QR payload parsing and initiator helpers
    pin.rs                          PIN flow driver (SPAKE2 wire orchestration)
    ssh.rs                          SSH flow (spawn ssh; identity exchange)

  auth/                           cloud OAuth + JWT
  client/                         local client API and connect helpers
  agents/                         agent runtime
  sleep_inhibitor/                platform sleep-inhibition backends

  services/
    mod.rs
    client.rs                       ClientService impl, host/peer listing,
                                    local-admin pairing/trust mutation RPCs
    pairing.rs                      PairingService gRPC impl, SPAKE2/HKDF/AEAD,
                                    trust commit guards
    reachability.rs                 direct/SSH runtime reachability helpers
    agent/                          AgentService impl
    startup/
      mod.rs                        Trusted Server + Pairing Server + listeners
                                    + direct/SSH startup attempts
      cloud.rs                      cloud RoutingService attach
```

CLI side:

```
crates/amux-cli/src/
  main.rs                         command table, including `amux pair`,
                                  `amux pair-recv`, `amux relay`,
                                  `amux peer list`, `amux peer info`,
                                  and `amux unpair`
  init.rs auth.rs server_client.rs session_client.rs
  client_common.rs hooks.rs plugin.rs update.rs
```

### 12.2 Key component APIs

Sketched crate-internal component signatures; the canonical types
live in code. The public crate API stays narrower and re-exports
only user-facing client, server, setup, pairing-helper, routing
listing, update, and debug types.

**`identity::DeviceIdentity`** — loaded once at daemon startup.

```rust
pub(crate) struct DeviceIdentity {
    pub(crate) host_id: HostId,
    // private key + X.509 cert held internally
}

impl DeviceIdentity {
    pub(crate) fn public_key(&self) -> &[u8];
    pub(crate) fn certificate_der(&self) -> Result<Vec<u8>>;
    pub(crate) fn server_tls_config(
        &self,
        trust_store: SharedTrustStore,
    ) -> Result<ServerConfig>;
    pub(crate) fn client_tls_config_for_peer(
        &self,
        trust_store: SharedTrustStore,
        peer: HostId,
    ) -> Result<ClientConfig>;
}

pub(crate) fn ensure_device_files_in(data_dir: &Path) -> Result<DeviceIdentity>;
pub(crate) fn ensure_device_files_with_trust_in(data_dir: &Path) -> Result<DeviceIdentity>;
pub(crate) fn load_or_create_device_identity_in(data_dir: &Path) -> Result<DeviceIdentity>;
pub(crate) fn host_id_for_certificate(cert_der: &[u8]) -> Result<Uuid>;
pub(crate) fn ed25519_public_key_from_certificate(cert_der: &[u8]) -> Result<Vec<u8>>;
```

**`trust::TrustStore`**

```rust
pub(crate) struct TrustStore { /* JSON-backed */ }

impl TrustStore {
    pub(crate) fn load_in(data_dir: &Path) -> Result<Self>;
    pub(crate) fn load_or_create_in(data_dir: &Path) -> Result<Self>;
    pub(crate) fn save_in(&self, data_dir: &Path) -> Result<()>;
    pub(crate) fn entries(&self) -> impl Iterator<Item = (HostId, &TrustEntry)>;
    pub(crate) fn host_id_for_pubkey(&self, pubkey: &[u8]) -> Option<HostId>;
    pub(crate) fn pubkey_for_host(&self, host_id: HostId) -> Option<&[u8]>;
    pub(crate) fn upsert_paired_peer(
        &mut self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<TrustStorePairingUpdate>;
    pub(crate) fn replace_paired_peer_after_teardown(
        &mut self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<TrustStorePairingUpdate>;
    pub(crate) fn remove(&mut self, host_id: HostId) -> Option<TrustEntry>;
}

pub(crate) enum Reachability {
    Cloud,
    Ssh { target: String },
    DirectTcp { addr: SocketAddr },
}
```

**`routing::RoutingCore`**

```rust
impl RoutingCore {
    pub(crate) fn new() -> Self;
    pub(crate) fn with_trust_store(trust_store: SharedTrustStore) -> Self;
    pub(crate) async fn best_route(&self, host_id: HostId) -> Option<Route>;
    pub(crate) async fn reserve_link(&self, proposed: &Link) -> Link;
    pub(crate) async fn apply_host_up(
        &self,
        host: Host,
        route: Route,
        origin_link: Option<Link>,
    ) -> HostUpOutcome;
    pub(crate) async fn apply_host_down(
        &self,
        host_id: HostId,
        route: &Route,
        origin_link: Option<Link>,
    ) -> bool;
    pub(crate) async fn remove_host_routes(
        &self,
        host_id: HostId,
        origin_link: Option<Link>,
    ) -> Vec<RoutingEvent>;
    pub(crate) async fn subscribe_routing_events(&self) -> mpsc::Receiver<RoutingEvent>;
    pub(crate) async fn subscribe_hosts(&self) -> mpsc::Receiver<HostReachabilityEvent>;
    // enforces N-R-2 dedup, N-R-7 snapshots, N-R-12 caps.
}
```

**`tunnel::TunnelPool`**

```rust
impl TunnelPool {
    pub(crate) fn new(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    ) -> Self;
    pub(crate) fn with_device_tls(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
    ) -> Self;
    pub(crate) fn link_registry(&self) -> Arc<LinkRegistry>;
    pub(crate) async fn channel_to_route(&self, peer: HostId, route: Route) -> Result<Channel>;
    pub(crate) async fn pin_pairing_channel_to_route(&self, peer: HostId, route: Route)
        -> Result<Channel>;
    pub(crate) async fn qr_pairing_channel_to_route(
        &self,
        peer: HostId,
        route: Route,
        expected_pubkey: Vec<u8>,
    ) -> Result<Channel>;
    pub(crate) async fn handle_inbound_frame_from_link(
        &self,
        frame: TunnelFrame,
        origin_link: Option<&Link>,
    ) -> Result<()>;
    pub(crate) async fn remove_route(&self, route: &Route);
    pub(crate) async fn remove_host_preserving_tunnel(
        &self,
        host_id: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    );
}
```

**`connection::ConnectionPool` + `ConnectionManager`**

```rust
pub struct ConnectionPool { /* RwLock<HashMap<Route, Channel>> */ }

impl ConnectionPool {
    pub(crate) async fn register(&self, route: Route, channel: Channel);
    pub(crate) async fn get(&self, route: &Route) -> Option<Channel>;
    pub(crate) async fn unregister(&self, route: &Route);
}

pub(crate) struct ConnectionManager {
    routing: Arc<RoutingCore>,
    runtime: RouteRuntimeState,
    state: RwLock<ConnectionState>,
}

impl ConnectionManager {
    pub(crate) fn new(routing: Arc<RoutingCore>, tunnels: Arc<TunnelPool>) -> Self;
    pub(crate) fn route_runtime(&self) -> RouteRuntimeState;
    pub(crate) fn trusted_connections(&self) -> TrustedPeerConnections;
    pub(crate) async fn attach_routing_events(self: Arc<Self>) -> JoinHandle<()>;
    pub(crate) async fn channel_to(&self, peer: HostId) -> Result<Channel>;
    pub(crate) async fn send_goaway_to_host(
        &self,
        peer: HostId,
        reason: GoAwayReason,
        drain_timeout_ms: u32,
    );
    pub(crate) async fn teardown_host(&self, peer: HostId);
    // Subscribes to RoutingCore events after attach_routing_events(); swaps
    // active_route on shorter-route arrival (N-CN-3..5).
}
```

**`pairing::PairMode`**

```rust
pub(crate) struct PairMode { /* singleton */ }

impl PairMode {
    pub(crate) fn new() -> Self;
    pub(crate) fn is_active(&self) -> bool;
    pub(crate) fn start_token(&self) -> Result<[u8; TOKEN_LEN]>;
    pub(crate) fn start_pin(&self) -> Result<String>;
    pub(crate) fn start_token_for_duration(&self, token: [u8; TOKEN_LEN], ttl: Duration)
        -> Result<()>;
    pub(crate) fn start_pin_for_duration(&self, pin: String, ttl: Duration) -> Result<()>;
    pub(crate) fn begin_token_attempt(&self, token: &[u8]) -> Result<PairModeTokenAttempt>;
    pub(crate) fn complete_token_success(&self, attempt: &mut PairModeTokenAttempt) -> Result<()>;
    pub(crate) fn begin_pin_attempt(&self) -> Result<PairModePinAttempt>;
    pub(crate) fn record_pin_failure(&self, attempt: &mut PairModePinAttempt) -> Result<()>;
    pub(crate) fn begin_pin_commit(&self, attempt: &mut PairModePinAttempt)
        -> Result<PairModePinCommit>;
    pub(crate) fn complete_pin_success(&self, commit: &mut PairModePinCommit) -> Result<()>;
    pub(crate) fn cancel(&self) -> bool;
}
```

**`services::client::ClientService` local-admin surface**

```rust
rpc StartPairing(StartPairingRequest) returns (StartPairingResponse);
rpc GetPairingStatus(GetPairingStatusRequest) returns (GetPairingStatusResponse);
rpc CancelPairing(CancelPairingRequest) returns (CancelPairingResponse);
rpc PairPeer(PairPeerRequest) returns (PairPeerResponse);
rpc PairPinCloudPeer(PairPinCloudPeerRequest) returns (PairPinCloudPeerResponse);
rpc PairQrCloudPeer(PairQrCloudPeerRequest) returns (PairQrCloudPeerResponse);
rpc ListPeers(ListPeersRequest) returns (ListPeersResponse);
rpc GetPeer(GetPeerRequest) returns (GetPeerResponse);
rpc Unpair(UnpairRequest) returns (UnpairResponse);
// These RPCs require local Unix/in-process authority. Paired remote mTLS
// callers cannot invoke them.
```

### 12.3 Static structure & ownership

Top-to-bottom layering:

```
 L1  config.rs / paths.rs / state.rs / user_state.rs
     identity.rs / trust.rs / audit.rs / resource_limits.rs

 L2  protocol/ / auth/ / transport/

 L3  routing/ / tunnel/ / connection.rs

 L4  dispatcher.rs / services/{client,pairing,reachability,agent,startup}
     / server.rs

 CLI client/ in the library and crates/amux-cli/
```

### 12.4 Call path — 1-hop direct (paired-direct TCP or SSH peer)

This is the case where the active route for the peer has length 1.
**No `TunnelFrame`s involved.** The Link's tonic Channel is used
directly; gRPC HTTP/2 frames flow over the underlying socket
(TCP+mTLS) or SSH stdio.

```
   ClientService caller / internal daemon code
                       │
                       ▼  channel_to(B)
   ┌──────────────────────────────────┐
   │ connection.rs                    │  active_route[B] = [L_AB]
   │  ConnectionManager reads         │  (length 1)
   │  active_route[B]                 │
   └──────────────────┬───────────────┘
                      │ pool.get([L_AB])  →  Channel (hit)
                      ▼
   ┌──────────────────────────────────┐
   │ connection.rs                    │  Channel wraps the existing
   │  ConnectionPool                  │  direct socket; same Channel
   │  by_route[[L_AB]] → Channel      │
   └──────────────────┬───────────────┘  that hosts RoutingService.Connect
                      │                  on this Link
                      ▼  gRPC call (raw HTTP/2 over socket)
   ┌──────────────────────────────────┐
   │ Link L_AB:                       │  no TunnelFrame wrapping; the
   │   paired-direct TCP + mTLS, or   │  Link's Connect stream and the
   │   SSH stdio                      │  AgentService call multiplex
   │                                  │  as separate HTTP/2 streams
   └──────────────────────────────────┘
```

`TunnelPool` is **not** invoked for this case. The 1-hop Link's
Channel was registered at Link establishment time (§8.5), so
`ConnectionPool.get` is a cache hit.

### 12.5 Call path — multi-hop tunneled (e.g., A→B via cloud relay R)

Active route for B has length ≥ 2. The first hop is one of A's
local Links (most commonly the cloud Link, but could be a
paired-direct Link if chaining through another peer). `TunnelPool`
materializes a tunnel, which becomes the underlying transport for
a tonic Channel.

```
   ClientService caller / internal daemon code
                       │
                       ▼  channel_to(B)
   ┌──────────────────────────────────┐
   │ connection.rs                    │  active_route[B] = [L_AR, L_RB]
   │  ConnectionManager reads         │  (length 2; first hop = cloud Link)
   │  active_route[B]                 │
   └──────────────────┬───────────────┘
                      │ pool.get([L_AR, L_RB])  →  None (cache miss)
                      ▼  materialize in connection.rs
   ┌──────────────────────────────────┐
   │ ConnectionManager                │  asks TunnelPool for a route Channel;
   │                                  │  TunnelPool allocates fresh nonce
   │                                  │  -> TunnelId { A, nonce }
   └──────────────────┬───────────────┘
                      ▼
   ┌──────────────────────────────────┐
   │ TunnelPool.channel_to_route(B,   │  installs tunnel state; gives
   │                          route)  │  back a TunnelTransport (the
   └──────────────────┬───────────────┘  AsyncRead+AsyncWrite shape)
                      │
                      ▼  wrap in tonic Channel + run end-to-end mTLS
   ┌──────────────────────────────────┐
   │ end-to-end mTLS handshake inside │  cert pinning against trust
   │ the TunnelTransport (between A   │  store for B's pubkey
   │ and B; opaque to R)              │
   └──────────────────┬───────────────┘
                      ▼  ConnectionPool.register([L_AR, L_RB], Channel)
                      │
                      ▼  gRPC call goes through the TunnelTransport
   ┌──────────────────────────────────┐
   │ Bytes flow as TunnelFrame.payload│
   │ inside Message envelopes on the  │
   │ A↔R Link (L_AR) — the cloud      │
   │ Link's Connect stream            │
   └──────────────────┬───────────────┘
                      ▼
   ┌──────────────────────────────────┐
   │ At R (cloud relay):              │  routing-layer forwarding only;
   │   pop dst.first() = L_AR         │  R does not see plaintext gRPC
   │   forward TunnelFrame via L_RB   │  (end-to-end mTLS protects it)
   │   (R's link to B)                │
   └──────────────────┬───────────────┘
                      ▼
   ┌──────────────────────────────────┐
   │ At B (endpoint):                 │
   │   dst is now empty               │
   │   TunnelPool.handle_inbound_frame│  routes by TunnelId to local
   │   produces a TunnelTransport     │  per-TunnelId tunnel state;
   │   on B's side                    │  feeds B's dispatcher (§4.7)
   └──────────────────┬───────────────┘
                      ▼
   ┌──────────────────────────────────┐
   │ B's dispatcher completes the     │  end-to-end mTLS terminates
   │ end-to-end TLS handshake on the  │  here on B's side; gRPC server
   │ inbound TunnelTransport          │  sees an authenticated peer
   └──────────────────┬───────────────┘
                      ▼
   ┌──────────────────────────────────┐
   │ B's Trusted Server               │  AgentService / ClientService /
   │ AgentService / etc.              │  RoutingService handlers run
   └──────────────────────────────────┘
```

Note the cloud Link's dual role: from A's perspective it is a
**1-hop Link to R** (carries Hello/HelloAck, routing events, JWT,
and any `TunnelFrame`s for tunnels that traverse it). From the
perspective of an A→B *tunnel* it is **the first hop of a 2-hop
route**. The same Link can be both a "direct connection to R" and
"the first hop of a multi-hop tunnel to B" simultaneously, because
the Connect stream multiplexes both via the `Message` envelope.

### 12.6 Call path — inbound (dispatcher)

Symmetric to §12.4/12.5 but from the receiver's side. The
dispatcher is the choke point for any inbound network byte stream
that needs a TLS handshake:

```
   ┌──────────────────────┐    ┌──────────────────────┐
   │ External TCP         │    │ TunnelPool produces  │
   │ listener accept      │    │ inbound              │
   │  → TCP socket        │    │ TunnelTransports     │
   └──────────┬───────────┘    │ when dst empties     │
              │                └──────────┬───────────┘
              │                           │
              └─────────────┬─────────────┘
                            ▼
   ┌────────────────────────────────────────────┐
   │ dispatcher                                 │
   │   server cert = DeviceIdentity.cert        │  always presented
   │   client cert = requested but not required │
   └──────────┬─────────────────────────────────┘
              │ classify per §4.7 table:
              │
   ┌──────────┴──────────────────────────────────┐
   │ pinned client cert verified                 │
   │ against trust store        → Trusted Server │
   │ no client cert + pair-mode → Pairing Server │
   │ no client cert + no pair   → Close          │
   │ unpinned cert              → reject at TLS  │
   └─────────────────────────────────────────────┘
```

### 12.7 Where each invariant lives

A pointer table to make code review tractable. Each normative §10
invariant has a primary implementation owner; supporting modules are
listed only where the invariant is intentionally cross-cutting.

| Invariant | Primary module |
|---|---|
| `N-K-1` | `identity.rs` |
| `N-K-2` | `identity.rs` |
| `N-K-3` | `identity.rs` |
| `N-T-1` | `trust.rs` |
| `N-T-2` | `trust.rs` |
| `N-T-3` | `identity.rs`, `trust.rs` |
| `N-T-4` | `services/pairing.rs`, `services/client.rs` |
| `N-T-5` | `trust.rs`, `services/reachability.rs` |
| `N-T-6` | `services/client.rs`, `trust.rs` |
| `N-C-1` | `services/startup/mod.rs`, `services/pairing.rs` |
| `N-C-2` | `services/startup/cloud.rs`, `routing/connect/mod.rs` |
| `N-C-3` | `tunnel/pool.rs`, `connection.rs`, `services/startup/cloud.rs` |
| `N-C-4` | `services/pairing.rs` |
| `N-X-1` | `identity.rs`, `transport/tls.rs` |
| `N-X-2` | `transport/`, `tunnel/transport.rs` |
| `N-X-3` | `identity.rs`, `trust.rs` |
| `N-X-3a` | `services/startup/cloud.rs`, `transport/tls.rs` |
| `N-X-4` | `connection.rs`, `tunnel/pool.rs` |
| `N-X-5` | `transport/tls.rs`, `services/startup/cloud.rs` |
| `N-X-6` | `transport/unix.rs`, `services/startup/mod.rs`, `transport/io.rs` |
| `N-X-7` | `dispatcher.rs`, `config.rs` |
| `N-X-8` | `services/client.rs`, `dispatcher.rs`, `config.rs` |
| `N-X-9` | `routing/connect/mod.rs`, `identity.rs` |
| `N-MT-1` | `user_state.rs`, `services/startup/cloud.rs` |
| `N-MT-2` | `server.rs`, `user_state.rs` |
| `N-MT-3` | `services/pairing.rs`, `services/client.rs` |
| `N-P-1` | `services/pairing.rs` |
| `N-P-2` | `pairing/mod.rs`, `services/pairing.rs` |
| `N-P-3` | `pairing/mod.rs` |
| `N-P-4` | `pairing/{pin,qr,ssh}.rs`, `services/pairing.rs` |
| `N-P-5` | `services/pairing.rs`, `connection.rs`, `trust.rs` |
| `N-P-6` | `pairing/mod.rs`, `services/pairing.rs` |
| `N-P-7` | `services/pairing.rs`, `connection.rs` |
| `N-P-8` | `services/reachability.rs`, `pairing/ssh.rs` |
| `N-P-9` | `services/pairing.rs`, `services/client.rs` |
| `N-G-1` | `services/startup/mod.rs` |
| `N-G-2` | `services/client.rs`, `services/agent/` |
| `N-G-3` | `routing/connect/mod.rs`, `services/startup/mod.rs`, `services/startup/cloud.rs` |
| `N-G-4` | `services/startup/cloud.rs`, `auth/jwt.rs` |
| `N-G-5` | `dispatcher.rs`, `services/startup/mod.rs` |
| `N-G-6` | `dispatcher.rs`, `tunnel/pool.rs`, `routing/connect/mod.rs` |
| `N-G-7` | `services/startup/mod.rs` |
| `N-R-1` | `routing/core.rs`, `routing/wire.rs` |
| `N-R-2` | `routing/core.rs` |
| `N-R-3` | `routing/core.rs` |
| `N-R-4` | `routing/core.rs`, `routing/link_registry.rs` |
| `N-R-5` | `routing/route.rs` |
| `N-R-6` | `routing/connect/mod.rs` |
| `N-R-7` | `routing/connect/mod.rs`, `routing/core.rs` |
| `N-R-8` | `routing/wire.rs`, `routing/connect/mod.rs` |
| `N-R-9` | `routing/wire.rs` |
| `N-R-10` | `routing/wire.rs`, `routing/connect/mod.rs` |
| `N-R-11` | `tunnel/pool.rs`, `routing/link_registry.rs` |
| `N-R-12` | `routing/core.rs`, `resource_limits.rs` |
| `N-L-1` | `routing/connect/mod.rs`, `routing/link_registry.rs` |
| `N-L-2` | `routing/core.rs`, `routing/connect/mod.rs` |
| `N-L-3` | `routing/link_registry.rs`, `routing/connect/mod.rs` |
| `N-L-4` | `connection.rs`, `routing/link_registry.rs` |
| `N-CN-1` | `connection.rs` |
| `N-CN-2` | `connection.rs`, `tunnel/pool.rs` |
| `N-CN-3` | `connection.rs` |
| `N-CN-4` | `connection.rs` |
| `N-CN-5` | `connection.rs`, `tunnel/pool.rs` |
| `N-CN-6` | `connection.rs` |
| `N-CN-7` | `connection.rs` |
| `N-CN-8` | `services/reachability.rs`, `services/startup/mod.rs` |
| `N-CN-9` | `services/client.rs`, `connection.rs`, `routing/core.rs`, `tunnel/pool.rs` |
| `N-TN-1` | `tunnel/pool.rs`, `connection.rs` |
| `N-TN-2` | `tunnel/types.rs` |
| `N-TN-3` | `tunnel/pool.rs`, `routing/connect/mod.rs` |
| `N-TN-4` | `tunnel/pool.rs` |
| `N-TN-5` | `tunnel/transport.rs` |
| `N-TN-6` | `tunnel/transport.rs`, `tunnel/pool.rs` |
| `N-TN-7` | `tunnel/pool.rs`, `resource_limits.rs`, `routing/connect/mod.rs` |
| `N-TN-8` | `routing/link.rs`, `routing/connect/mod.rs`, `routing/link_registry.rs`, `connection.rs` |
| `N-S-1` | Deferred; no phone client module exists in v1. |
| `N-S-2` | `services/client.rs`, `services/startup/mod.rs` |

A normative §10 invariant should be referenceable from exactly one
or two implementation modules; if it lives in many, it's likely
overly general and should be split.
