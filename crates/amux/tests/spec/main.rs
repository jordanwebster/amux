//! The amux protocol spec suite.
//!
//! Whole-daemon black-box tests over the `amux::testnet` harness. Chapters
//! mirror the protocol story, so reading the suite top-to-bottom works as
//! documentation; the modules below are declared in that reading order. See
//! `notes/SPEC_TESTS_DESIGN.md` for the design contract and test catalog.
//!
//! Run with: `cargo test -p amux --features testnet --test spec`

mod smoke; // the harness in one test: the canonical TestNet example

mod identity; // Chapter 1 — Identity & trust
mod pairing; // Chapter 2 — Pairing
mod presence; // Chapter 3 — Presence
mod routing; // Chapter 4 — Routing & failover
mod sessions; // Chapter 5 — Remote sessions & authority
mod wire; // Chapter 6 — Wire conformance (WirePeer)
