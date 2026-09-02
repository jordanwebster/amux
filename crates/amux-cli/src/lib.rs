//! The parts of the amux CLI that are worth testing without a terminal.
//!
//! `amux` is a binary; almost everything it does is command dispatch that
//! lives in `main.rs`. This library exists so the pieces the TUI depends on
//! can be unit-tested directly, and the binary uses it like any other
//! dependency.

pub mod diagnostics;
