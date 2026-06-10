# The amux protocol

**Status**: target design (v6), agreed 2026-06-11. The current
implementation is v5, specified in [`NETWORKING.md`](./NETWORKING.md);
this document supersedes its routing, tunneling, and pairing architecture
once implemented. Decision rationale: `notes/PROTOCOL_V6_DECISIONS.md`.

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
screen). The secret itself never crosses the wire — SPAKE2 proves possession
without transmitting it — and is one-shot with a ~5-minute window. The trust
store also records, per peer, any *reachability hints* this device learned
as the dialer (a TCP address, an SSH target); on startup the daemon re-dials
them. Re-establishment is always the dialer's job.

## Links: who is my neighbor

A **link** is an authenticated connection to an adjacent node over some
transport: TCP+mTLS to a paired peer, SSH stdio to a paired peer, or
TCP+TLS+JWT to the cloud. The handshake (`Hello`/`HelloAck`) exchanges
identity, protocol version, and the sender's **current neighbor list** —
the snapshot is a field of the handshake, so everything after it is a delta
(`NeighborUp`/`NeighborDown`) by definition. `LinkClose { reason }` ends a
link; on the cloud link, a fire-and-forget `Reauth { token }` refreshes the
JWT before expiry (the cloud's only answers are silence, or `LinkClose`).

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
identified by `TunnelId { initiator, nonce }` (16 random bytes), carried as
`TunnelData { tunnel_id, dst, payload }` frames (≤ 64 KiB) over at most one
relay. The first frame for an unknown id opens the tunnel; `TunnelClose`
or link death ends it; replies travel back out the link they arrived on.

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

`Hello` / `HelloAck` · `NeighborUp` / `NeighborDown` · `TunnelData` ·
`TunnelClose` · `LinkClose` · `Reauth` · `PairingService.Pair` (stream).
`PROTOCOL_VERSION = 6`.

## What this protocol deliberately does not have

Route lists, link names, prepend-on-forward, split-horizon, hop caps, route
dedup, snapshot phases, drain timeouts, acknowledgements for housekeeping,
a second pairing protocol, transitive presence, or transitive trust. Each
absence is a class of bugs that cannot be written.
