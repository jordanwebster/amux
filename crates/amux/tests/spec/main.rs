//! The amux protocol spec suite.
//!
//! Whole-daemon black-box tests over the `amux::testnet` harness. Chapters
//! mirror the protocol story (`docs/PROTOCOL.md`), so reading the suite
//! top-to-bottom works as documentation; the modules below are declared in
//! that reading order.
//!
//! Run with: `timeout 600 cargo test --workspace --test spec`
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

// Test scaffolding exists only in debug profiles (see build.rs); a
// release-profile test build compiles this crate empty rather than failing.
#![cfg(testnet)]
mod discovery;
mod entitlement;
mod smoke; // the harness in one test: the canonical TestNet example // Chapter 0 — Local-network listener discovery

mod agents; // Chapter 7 — Agent messaging & relationships
mod attachments; // Chapter 9 — Artifact routing, persistence & lifetime
mod channels; // Chapter 5 — Native stream classes & lifecycle
mod debug; // Chapter 8 — Live daemon diagnostics
mod identity; // Chapter 1 — Identity & trust
mod pairing; // Chapter 2 — Pairing
mod presence; // Chapter 3 — Presence
mod quic; // Chapter 5 — Direct QUIC behavior and network conditions
mod relay; // Chapter 6 — Relay carriers and opaque stream forwarding
mod routing; // Chapter 4 — Routing & failover
mod sessions; // Chapter 6 — Remote sessions & authority
mod wire; // Chapter 7 — Wire conformance (WirePeer)

mod profiles;
