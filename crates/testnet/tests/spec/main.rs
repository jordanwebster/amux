//! The amux protocol spec suite.
//!
//! Whole-daemon black-box tests over the `testnet` harness. Chapters
//! mirror the protocol story (`docs/PROTOCOL.md`), so reading the suite
//! top-to-bottom works as documentation; the modules below are declared in
//! that reading order.
//!
//! Run with: `timeout 1200 wt run spec`
//!
//! Adding a test: build a topology with `TestNet::builder()` —
//! `.daemon(name)`, `.cloud()`, `.paired(a, b, Via::Direct | Via::Cloud)`,
//! `.trusted(a, b)` — then assert with the prose verbs on `Daemon`
//! (`sees`, `cannot_see`, `can_call`, `connects_to(..).via_direct()`,
//! `pair().with_pin()`, `attach`, `restart`, ...). Every verb retries
//! internally with a bounded `eventually()`, so tests state outcomes,
//! never sleeps. Wire-level cases the topology verbs cannot express use
//! `WirePeer`, a scripted protocol actor (chapter 6). Conventions: test
//! names are prose sentences; doc comments say *why* the behavior is the
//! contract; keep daemons-per-test minimal (each is a real in-process
//! daemon with real TLS); anything exercising a shared rate limiter gets
//! its own `TestNet`.

// Chapter 0 — Discovery
mod discovery;

// Chapter 1 — Profiles, identity and trust
mod identity;
mod profiles;

// Chapter 2 — Pairing
mod pairing;

// Chapter 3 — Presence
mod presence;

// Chapter 4 — Routing and failover
mod routing;

// Chapter 5 — Carriers, links, streams and channels
mod channels;
mod quic;
mod wire;

// Chapter 6 — Relay forwarding and entitlement
mod entitlement;
mod relay;

// Chapter 7 — Agent messaging and remote sessions
mod agents;
mod sessions;

// Chapter 8 — Diagnostics
mod debug;

// Chapter 9 — Artifact routing, persistence and lifetime
mod attachments;

mod smoke; // The canonical TestNet harness example
