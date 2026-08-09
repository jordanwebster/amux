//! The amux-ui spec suite (tier 1).
//!
//! Reducer prose specs: ordered Msg sequences in, Model assertions out.
//! Chapters read as documentation, mirroring `crates/amux/tests/spec/`; the
//! normative design is `docs/UI.md`, and where prose and passing spec
//! disagree, the spec wins.
//!
//! Run with: `timeout 600 cargo test -p amux-ui --test spec`
//!
//! No tokio, no network, no clocks anywhere in this suite — everything is a
//! pure fold. Every chapter registers its Msg sequences with the harness so
//! the differential property in `wire_free` wraps them all: after every Msg,
//! fold-from-recording must equal live state.

mod harness;

mod connection; // Chapter 1 — Connection: epochs, snapshots, auth expiry
mod inventory; // Chapter 2 — Inventory: upserts, unknown types, authority
mod ops; // Chapter 3 — Operations: the Command write surface
mod sessions; // Chapter 4 — Session streams: lifecycle facts
mod wire_free; // Chapter 5 — Determinism: differential fold, replay, serde
