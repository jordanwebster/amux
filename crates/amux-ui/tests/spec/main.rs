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

// Declared in reading order (blank lines keep rustfmt from re-sorting).

mod connection; // Chapter 1 — Connection: epochs, snapshots, auth expiry

mod inventory; // Chapter 2 — Inventory: upserts, unknown types, authority

mod ops; // Chapter 3 — Operations: the Command write surface

mod sessions; // Chapter 4 — Session streams: lifecycle facts

mod attention; // Chapter 5 — Attention: summarizer folds, subscription policy

mod feed_replay; // Chapter 6 — Chat feed: replay, epochs, retention

mod feed_turns; // Chapter 7 — Chat feed: prompts, messages, markers

mod feed_tools; // Chapter 8 — Chat feed: tool pairing, result facts

mod feed_edges; // Chapter 9 — Chat feed: status, subagents, unknown rows

mod wire_free; // Chapter 10 — Determinism: differential fold, replay, serde

mod asks; // Chapter 11 — Asks: extraction, correlation, lifecycle

mod phase; // Chapter 12 — Phase: the E1 derivation table

mod write; // Chapter 13 — The write path: intents, programs, optimism

mod codex_feed; // Chapter 14 — Codex rows: native items, turns, boundaries

mod codex_asks; // Chapter 15 — Codex asks: raw/synth correlation and blocking

mod codex_write; // Chapter 16 — Codex writes: protobuf-native typed effects

mod codex_agreement; // Chapter 17 — Codex phase/attention/gate agreement

mod claude_agreement; // Chapter 18 — Claude classification/projection agreement

mod a2a_fleet; // Chapter 19 — Families: parent edges folded into fleet rows

mod a2a_claude_inbound; // Chapter 20 — Claude inbound: a message from another agent
