//! SQLite-backed client store.
//!
//! SQLite stays behind this crate boundary. The store implementation is added
//! after the shared I/O-free vocabulary and fold algebra are in place.

#![forbid(unsafe_code)]
