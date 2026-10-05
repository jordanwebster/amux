//! Claude Code's protocols as types, and nothing else.
//!
//! [`stream`] is the headless stream amux drives Claude through. Decoding is
//! tolerant in production and strict in the recording checks, the same rule
//! as Codex's protocol crate. The crate is pure: no process, socket or
//! async code, so the interpreter the phone links can depend on it.

mod strictness;

pub mod stream;
