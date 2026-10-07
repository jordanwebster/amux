//! Claude Code's protocols as types, and nothing else.
//!
//! [`stream`] is the headless stream amux drives Claude through;
//! [`transcript`] the rows of the transcript file terminal Claude writes;
//! [`hooks`] the payloads its hook command forwards; [`messaging`] the
//! lines written to its messaging socket. Decoding is tolerant in
//! production and strict in the recording checks, the same rule as Codex's
//! protocol crate. The crate is pure: no process, socket or async code, so
//! the interpreter the phone links can depend on it.

mod absent;
mod decoding;
mod strictness;

pub mod hooks;
pub mod messaging;
pub mod stream;
pub mod transcript;

pub use decoding::{DecodeError, Drift};
