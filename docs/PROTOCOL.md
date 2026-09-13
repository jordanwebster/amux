# The amux protocol

**Status**: current (2026-09-11). This is protocol version 2, exercised by
the prose spec suite in `crates/amux/tests/spec/`. The processes and service
boundaries around it are described in [Architecture](./ARCHITECTURE.md); the
user-facing behavior is described in [How amux works](./HOW_IT_WORKS.md).

## The mental model

> **Carriers carry links. Links carry streams. Streams carry channels. One
> pinned handshake authenticates every channel, whatever carried it.**

The layers have deliberately separate jobs:

- A **carrier** provides an ordered control stream and independent
  bidirectional byte streams.
- A **link** is the live relationship with one adjacent node. Its control
  stream exchanges identity, protocol version, adjacency and link lifetime.
- A **stream** is one native QUIC stream or one yamux stream. Its first message
  says where it is going; after admission, its bytes are opaque to a relay.
- A **channel** is the end-to-end call connection inside that stream. Its
  pinned TLS handshake, not its carrier or route, decides call authority.

The carrier may also authenticate the adjacent link: direct QUIC uses pinned
mutual TLS, the cloud carriers use WebPKI TLS plus a token in `Hello`, and SSH
uses the authenticated SSH channel. That outer admission never replaces the
pinned handshake inside an ordinary peer channel.

## Chapter 0: Discovery

A listening profile advertises `_amux._udp` through DNS-SD. Its SRV record
names the ephemeral or configured QUIC port; its TXT data contains only the
protocol version and host id. Resolving it produces a name and socket-address
dial hints. Browsing is a standing subscription: a goodbye or TTL expiry
removes a result, while a new resolution replaces its addresses.

Discovery is **never trust or presence**. An unpaired result is only a pairing
candidate. A result claiming a paired host's id is only a dial hint; the pinned
handshake must still present that host's key. A paired host is online only
after a link succeeds. Discovery is re-queried at startup, after a network
change or wake, when a direct link drops, when a pairing window opens, and for
`amux peer list`.

## Chapter 1: Profiles, identity and trust

A profile is one device on the wire. An installation may run several profiles,
but each has its own Ed25519 keypair, random 128-bit `host_id`, trust store,
routing state and optional cloud link. A profile and its identity survive
restart. Binding or unbinding an account does not replace them.

Trust is a public-key pin in a local, never-shared store. Pairing adds pins;
local revocation removes one. Revocation closes the peer's links and their
streams immediately. Deleting a profile destroys its whole trust store.
Profiles never inherit another profile's pairing window, pins, routes or
account.

Trusted peer services expose agent operations. Installation lifecycle,
profile lifecycle, trust administration and pairing-window administration
remain on the local installation front door or an in-process owner handle;
they are not peer-call methods.

## Chapter 2: Pairing

PIN and QR pairing use one SPAKE2 exchange. The PIN is six digits. The QR
contains JSON `{host_id, secret, addrs, cloud_url?}` inside an
`amux://pair?payload=...` deep link; `secret` is a one-shot 256-bit value and
`addrs` let pairing work when multicast is unavailable. A found advertisement,
a typed address, the QR addresses, SSH, or a relay can all lead to the same
exchange. Direct candidates are tried with a bounded QUIC handshake so a
silent address does not prevent trying the next one.

SPAKE2 follows RFC 9382 over edwards25519, responder B and initiator A, with
messages B to A then A to B. Both sides hash the big-endian
`PROTOCOL_VERSION` and length-prefixed SPAKE2 messages. HKDF-SHA256 with salt
`amux-pair-spake2-v1` derives confirmation keys (`kc/A`, `kc/B`) and
ChaCha20-Poly1305 keys (`aead/A→B`, `aead/B→A`). The sealed identity contains
the public key and a name of at most 256 bytes, with direction and transcript
in the additional authenticated data. `PairingComplete` commits both stores.
Every secret failure is the same opaque `INVALID_PIN`.

Before trust exists, an open pairing window lets the dispatcher admit one
anonymous channel to `PairingService`. SPAKE2 authenticates that exchange and
creates the pins used by later channel handshakes. Ordinary PIN and QR windows
last about five minutes, are one-shot, and have a five-attempt cap. The init
on-ramp's PIN lasts fifteen minutes. Explicit demo PIN windows are reusable
until their configured expiry. SSH pairing instead exchanges identities over
the already-authenticated SSH stream and records outbound reachability only on
the side that knows how to dial it.

Clients can pause SPAKE2 before granting trust. `begin_pair_pin` and
`begin_pair_qr` return a `PendingPeer` carrying the authenticated host id,
name, SHA-256 public-key fingerprint and expiry, with the expiry sealed inside
the responder identity. Neither trust store changes during this phase. The
local profile administration handle retains the open stream behind an opaque,
single-use token; unresolved streams expire after at most five minutes and at
most 32 are retained. That token is a local capability, not a serializable
trust decision a client could recreate from the identity fields it displays.
`confirm_pair` sends the initiator's sealed identity, waits for the
responder's trust commit and stores the peer. `abandon_pair` sends a rejection
and waits for `PairingAbandoned`, which the responder sends after releasing the
attempt. Cancelling leaves existing trust unchanged and consumes no guess, and
dropping a pending value grants no trust. Begin, confirm, abandon and device
identity inspection are served only by `ProfileService` on the installation
front door, never by a profile socket or a peer stream.

## Chapter 3: Presence

Presence comes from the link control protocol. `Hello` and `HelloAck` each
carry the sender's current adjacent-neighbor snapshot. Later `NeighborUp` and
`NeighborDown` messages are deltas from that snapshot. A cloud relay scopes
these claims to one account; a direct or self-hosted link scopes them to that
adjacent peer.

Presence is a derivation, not an independent assertion: a host is reachable
through a relay when that relay says it has an adjacent link to the host. This
can make an untrusted host a pairing candidate, but does not grant authority or
make a discovery result online. Losing the last route leaves a trusted host in
inventory as offline.

A host also announces what kind of machine it is: `platform` names the
operating system the daemon was built for, in the host's own words. It is
optional because a host built before the field existed says nothing, and a
machine whose kind is unknown is not the same as one claiming to be nothing in
particular. Nothing routes or authorizes on it; it exists so a client can tell
one device from another in a list.

## Chapter 4: Routing and failover

Routing has two rules:

1. **Advertise only adjacency.** A node tells its neighbors only about hosts
   to which it has a direct live link. It never repeats a neighbor learned from
   someone else.
2. **Forward only to adjacency.** A relay forwards a stream only when it has a
   direct live link to the stream preface's destination.

A route is therefore direct or via one adjacent relay. Non-recursive
forwarding makes routes loop-free and limits them to one relay without route
lists, hop counts or split horizon. Presence follows from the first rule; it
is not a separate wire claim. Direct routes win over relayed routes. Route
replacement is make-then-break for new calls; existing channels stay attached
to their original link until that link or channel ends.

## Chapter 5: Carriers, links, streams and channels

### Carriers

All carriers implement the same link interface: one control stream, open and
accept for additional streams, finish and typed reset, and link close.

| Use | Carrier | Link admission | Migration |
| --- | --- | --- | --- |
| Direct device link | QUIC/TLS 1.3, ALPN `amux/2` | pinned mutual TLS | yes |
| Preferred cloud link | QUIC/TLS 1.3, ALPN `amux/2` | WebPKI certificate, then hello token | yes |
| Cloud fallback | TLS over TCP, then symmetric yamux | WebPKI certificate, then hello token | no |
| SSH link | symmetric yamux over SSH stdio | pinned peer reached through SSH | no |

Device QUIC has session tickets, resumption and 0-RTT disabled. It permits only
bidirectional application streams, currently at most 64 concurrently. The
keepalive/idle pair is 20/60 seconds on iOS and 30/120 seconds elsewhere.

### Link control

The connector opens the first bidirectional stream as control. Each control
message is protobuf encoded after a big-endian `u32` byte length and is bounded
by `MESSAGE_SIZE_LIMIT`. The complete control vocabulary is:

- `Hello`: supported protocol versions, this host, the current neighbor
  snapshot, and an authentication token only for a cloud link.
- `HelloAck`: either the accepted version plus the acceptor's host and neighbor
  snapshot, or an error.
- `NeighborUp` and `NeighborDown`: adjacency deltas after the handshake.
- `Reauth`: a fire-and-forget replacement token for a cloud link.
- `LinkClose`: an immediate close with a reason and optional error.

Versioning is equality, not feature negotiation: both peers must select
`PROTOCOL_VERSION = 2`. A failed or expired cloud token closes the link with
`AUTH_EXPIRED`. Reauthentication is not acknowledged; success is silence and
failure is a close.

### Stream preface and refusal

Every non-control stream starts with one length-prefixed `StreamPreface` whose
only field is `dst`, the destination host id. There is no source, stream id,
payload, data message or close message in the control vocabulary. The pinned
channel handshake proves the caller. Graceful stream finish is close. A reset
before acceptance is a refused open and carries one `StreamRefusal` code:

- `NO_ROUTE`: the destination has no adjacent live link, or the route vanished
  while opening it.
- `PAYMENT_REQUIRED`: a cloud-relay link at either end was admitted as free.
- `RATE_LIMITED`: the relay refused the origin's stream-open rate.
- `NOT_ADJACENT`: the destination is invalid, is the relay itself, or the
  stream did not provide a valid preface.
- `SHUTTING_DOWN`: the accepting link is closing.
- `UNSPECIFIED`: reserved for an unknown or unmapped reset code.

### Channel authority and classes

After the preface is accepted, the endpoints run TLS 1.3 inside the stream.
The server reads the live trust store and the client pins the expected peer.
This is the single authority decision for calls on direct QUIC, relay QUIC,
relay TCP and SSH alike. Relays only copy its ciphertext.

The channel pool selects a live link by peer and route, opens a stream, performs
that handshake, and gives the result to tonic as one HTTP/2 channel. Calls and
inventory subscriptions share a cached `Calls` channel per peer and route.
Each agent subscription gets a fresh `Session` channel. Each artifact or diff
fetch gets a fresh `Bulk` channel. Separate streams provide separate flow
control, so a bulk response cannot queue behind a live session at the amux
layer.

## Chapter 6: Relay forwarding and entitlement

A relay receives a non-control stream, validates its preface, looks up a direct
link to `dst`, opens a stream there with the same preface, and copies bytes in
both directions until either side ends. It does not parse channel bytes or keep
protocol-level stream ids, sources, payload buffers or close state. Either end
may open, so a host reached through a relay can call back on the same link, and
one relay can pipe between QUIC and TCP carriers in either direction.

Every link records how it was admitted. A tier belongs to a token-admitted
link on the cloud relay only: `CloudToken { free | pro }`. Direct device and
SSH links are `PinnedKey`. A self-hosted relay between paired peers therefore
never consults an account or tier and pipes their streams normally.

The cloud relay refuses an open with `PAYMENT_REQUIRED` when either its origin
link or its destination link was token-admitted as free. Neighbor control
continues, so a free account receives presence but no call or pairing channel.
Reauthentication may replace a link's tier. The relay also refuses a missing
route, its own address, malformed prefaces and excess opens with the specific
codes above.

The cloud listener serves QUIC on UDP beside the TCP fallback using the same
WebPKI certificate and key. Devices try QUIC immediately and begin TCP after a
300 ms fallback delay; the first completed link wins, with QUIC winning a tie.
A TCP-only win records that relay host as UDP-blocked for one hour, suppressing
QUIC attempts until the memory expires. The memory is process-local.

The fallback's limits are honest. Yamux gives the same symmetric open/accept
interface and per-stream windows, but all streams still share one ordered TCP
byte stream. Packet loss can therefore cause cross-stream head-of-line
blocking; the connection cannot migrate across a network change; and losing
it ends every stream it carries. TCP is only a cloud-relay fallback. A direct
LAN whose UDP is blocked is offline unless another route, such as the relay or
SSH, is available.

## Chapter 7: Agent messaging and remote sessions

Agent calls use the authenticated channels above. A client may name a local
live agent as sender, but the daemon resolves provenance before forwarding;
arbitrary ids never become authenticated senders. Cross-device messages and
completion notifications use the peer agent service. Parent relationships,
work state and create/delete behavior are specified in [Agent-to-agent
messaging](./A2A.md).

Session protocols are closed protobuf variants. Claude PTY exposes
`terminal_v1` and `claude_pty_transcript_v1`; Claude SDK exposes
`claude_sdk_v1`; Codex exposes `terminal_v1` and `codex_sdk_v1`; the test agent
exposes `terminal_v1` and `test_echo_v1`. Inputs and outputs retain that type
end to end, and selecting a protocol that the agent kind does not expose fails
as `ProtocolNotExposed`. Provider ownership is described in [Provider
crates](./PROVIDER_CRATES.md), with PTY input semantics in
[Keymaps](./KEYMAPS.md).

## Chapter 8: Diagnostics

A debug report describes current routes, adjacent links, carrier kinds, cached
call channels, active session streams, retained output and provider state. It
observes the protocol implementation; it creates no additional wire messages
or authority path.

## Chapter 9: Artifact routing, persistence and lifetime

Artifact and diff reads use fresh `Bulk` channels, while their metadata and
storage lifetime remain daemon service concerns. Routing an artifact never
makes a relay understand its bytes. The stream and channel end with the fetch,
independently of cached call channels and live sessions.

## What the wire deliberately does not have

There are no transitive neighbor advertisements, route lists, source routes,
hop caps, split horizon, per-channel forwarding records, stream data messages,
housekeeping acknowledgements, a second pairing protocol, transitive trust, or
transitive presence. Link loss closes its streams; callers reconnect over the
best current route instead of trying to splice byte streams between links.

That small vocabulary is the point: adjacency chooses where bytes may go, the
stream carries them, and the pinned channel handshake decides who may call.
