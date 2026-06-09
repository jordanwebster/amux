//! The amux protocol spec suite.
//!
//! Whole-daemon black-box tests over the `amux::testnet` harness. Chapters
//! mirror the protocol story so reading the suite top-to-bottom works as
//! documentation. See `notes/SPEC_TESTS_DESIGN.md`.
//!
//! Run with: `cargo test -p amux --features testnet --test spec`

mod smoke;

mod identity;
mod pairing;
mod presence;
mod routing;
mod sessions;
mod wire;
