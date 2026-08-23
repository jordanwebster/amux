# The amux protocol

**Status**: implemented (2026-06-11). This is the protocol the daemon
speaks; it is locked in by the prose spec suite in
`crates/amux/tests/spec/`. The system around the wire — processes,
servers, the dispatcher, trust storage, service surfaces — is described
in [`ARCHITECTURE.md`](./ARCHITECTURE.md); the user-facing story is
[`HOW_IT_WORKS.md`](./HOW_IT_WORKS.md).

## The mental model

> **Transports carry links. Links carry frames. Tunnels carry calls.
> One pinned mTLS handshake authenticates every tunnel, whatever it rides.**

Everything below is elaboration.

## Identity and trust

Every device has an Ed25519 keypair and a random 128-bit `host_id`, created
on first run, persisted forever. **Trust is a pinned public key** in a
local, never-shared trust store. Keys get pinned exactly one way — pairing —
and unpinned exactly one way — local revocation (`amux unpair`).

**Pairing** is one protocol: SPAKE2, with the shared secret delivered
out-of-band. Two delivery mechanisms, same wire flow: a 6-digit PIN the user
types (no camera), or a 256-bit secret carried in a QR code (point phone at
screen). QR contents are always the production deep link
`amux://pair?payload=<base64url-no-padding-json>`; the decoded JSON payload
is `{host_id, cloud_url, secret}`. The secret itself never crosses the wire
— SPAKE2 proves possession without transmitting it — and is one-shot with a
~5-minute window and a 5-attempt cap. The trust store also records, per
peer, any *reachability hints* this device learned as the dialer (a TCP
address, an SSH target); on startup the daemon re-dials them.
Re-establishment is always the dialer's job.

The wire flow (`PairingService.Pair`, a bidi stream) and its crypto, for
implementers: SPAKE2 per RFC 9382 over edwards25519, responder = B,
initiator = A, messages exchanged B→A then A→B. Both sides hash a
transcript — SHA-256 of the big-endian `PROTOCOL_VERSION` and the
length-prefixed SPAKE2 messages — and derive keys with HKDF-SHA256 (salt
`"amux-pair-spake2-v1"`): two confirmation keys (infos `kc/A`, `kc/B`),
exchanged and checked first, and two ChaCha20-Poly1305 keys (infos
`aead/A→B`, `aead/B→A`) that seal each side's identity — pubkey and a
name capped at 256 bytes — with the direction and transcript bound into
the AAD. A `PairingComplete` commits both trust stores. Every secret
failure surfaces as the same opaque `INVALID_PIN`, whichever delivery
carried the secret. (Pairing over SSH is simpler still: the SSH channel
is the out-of-band trust, and the two ends exchange identities directly
over its stdio.)

## Links: who is my neighbor

A **link** is an authenticated connection to an adjacent node over some
transport: TCP+mTLS to a paired peer, SSH stdio to a paired peer, or
TCP+TLS+JWT to the cloud. The handshake (`Hello`/`HelloAck`) exchanges
identity, protocol version, and the sender's **current neighbor list** —
the snapshot is a field of the handshake, so everything after it is a delta
(`NeighborUp`/`NeighborDown`) by definition. Versioning is equality, not
negotiation: the acceptor requires its own `PROTOCOL_VERSION` in the
Hello's supported set, and the connector requires the ack to confirm that
same version. `LinkClose { reason }` ends a link; on the cloud link, a
fire-and-forget `Reauth { token }` refreshes the JWT before expiry (the
cloud's only answers are silence, or `LinkClose`).

Link authentication answers *who is my neighbor* — it makes adjacency
claims trustworthy and keeps frames off the wire. It grants **frame
forwarding only**, never call authority.

## Routing: two rules

1. **Advertise only adjacency.** A node tells its neighbors "I have a
   direct link to H" — never anything it learned from someone else.
2. **Forward only to adjacency.** A frame addressed to `dst` is forwarded
   iff the relay has a direct link to `dst`; otherwise dropped.

A route is therefore `Direct(link)` or `Via(relay)`, where the relay is
*any* adjacent node. One-proxy-hop-max and loop-freedom are not enforced
rules — they are structural consequences of forwarding being non-recursive.
Presence is a derivation, not a wire claim: H is *online* if some neighbor
claims adjacency to H. Relays keep no routing state: forwarding consults
only their own connection map.

## Tunnels: who am I calling

Every call between peers rides a **tunnel** — an end-to-end byte stream
over at most one relay, with the same lifecycle grammar as a link:
`TunnelOpen { tunnel_id, src, dst }` opens it (a plain UUID id; the reply
address travels exactly once, here), `TunnelData { tunnel_id, dst, payload }`
frames (≤ 64 KiB) carry it, and `TunnelClose` — or link death — ends it.
Only an Open allocates state; Data for an unknown id is a violation and is
dropped without allocation. There is no open-ack: the mTLS handshake inside
is the acknowledgement, and rejection is `TunnelClose`. Replies travel back
out the link they arrived on.

Inside **every** tunnel — even to an adjacent peer — runs an mTLS handshake
pinned against the trust store. This is the system's single authority
decision, made at the receiving daemon's dispatcher when a tunnel
terminates: a pinned client cert reaches the trusted services (full peer
authority); no cert during an active pairing window reaches the pairing
service; anything else is closed. Relays see ciphertext.

Because tunnels are initiated by *sending frames*, and frames flow both
ways on every link, **any live link is fully bidirectional at the call
layer** — a peer that could never dial (an SSH-pairing responder, a device
behind NAT) can still call back over the link its peer established. What
remains asymmetric is only dialing itself.

## The cloud

The cloud is a well-connected, multi-tenant relay — and nothing else. It
forwards frames between one user's devices, advertises their adjacency
(scoped per user), and admits links by JWT. It is **adjacent but
untrusted**: it has no pinned key, so it can never terminate a tunnel into
anyone's trusted services — it cannot create agents, read traffic, or
impersonate a device. A self-hosted relay is just an ordinary always-on
paired peer; relaying is something every node can do.

## The complete wire vocabulary

`Hello` / `HelloAck` · `NeighborUp` / `NeighborDown` · `TunnelOpen` ·
`TunnelData` · `TunnelClose` · `LinkClose` · `Reauth` ·
`PairingService.Pair` (stream). `PROTOCOL_VERSION = 1`.

Agent-to-agent messaging does not add a link frame or change
`PROTOCOL_VERSION`. `ClientService.SendMessage` resolves a human or live local
agent as the sender; remote recipients are forwarded as daemon-authored
envelopes through `AgentService.SendMessage` inside the same authenticated
tunnels as every other peer call. Parent edges, work status, create/delete
lifecycle, and provider carriers are specified in [`A2A.md`](./A2A.md).

## What this protocol deliberately does not have

Route lists, link names, prepend-on-forward, split-horizon, hop caps, route
dedup, snapshot phases, drain timeouts, acknowledgements for housekeeping,
a second pairing protocol, transitive presence, or transitive trust. Each
absence is a class of bugs that cannot be written.

## Why it is shaped this way

- **Host-id routing exists for the relays.** Addressing frames to a host
  and forwarding only to adjacency lets a relay forward with nothing but
  its own connection map — no per-tunnel or per-route state, nothing to
  leak, nothing to desynchronize. Source-routed lists were tried first
  and made every relay a bookkeeper.
- **One proxy hop is the product, not a limitation.** Every real topology
  is "my devices, maybe one always-on box or the cloud between them."
  Arbitrary hop counts bought loop suppression, hop caps, and dedup —
  and no user-visible capability.
- **Only `TunnelOpen` allocates** so that garbage can be dropped for
  free: data for an unknown id is unambiguously a violation, rate
  limiters meter Opens, and a stale frame cannot conjure state. And
  there is no open-ack because the mTLS handshake inside the tunnel
  already *is* the acknowledgement — an ack would confirm delivery to a
  node we don't trust to say so.
- **Housekeeping is never acknowledged.** `Reauth` is fire-and-forget
  because the only honest answers a relay has are "carry on" (silence)
  and "you're done" (`LinkClose`); an ack plus a timeout was a state
  machine that could only invent new failure modes. Refresh exists at
  all because live sessions must never break on a timer.
- **Tunnels die with their link.** Re-pinning a tunnel to a replacement
  link would reorder frames across links and break the TLS stream inside
  it. Cheaper for the caller to reconnect over whatever route is now
  best — which it must be able to do anyway.
- **Adjacent-peer tunnels are doubly encrypted, on purpose.** Skipping
  the inner handshake when the link is already mTLS would make call
  authority depend on transport type — the exact coupling the design
  removes. Uniformity beats the saved handshake.
- **The PIN became SPAKE2's secret, and the QR token became one too.**
  A bearer token sent through TLS authenticates whoever holds the pipe;
  a PAKE authenticates possession without ever transmitting the secret,
  and collapsing both deliveries onto it deleted a second wire protocol.
