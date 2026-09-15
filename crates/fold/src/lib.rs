//! Pure observation and transcript folds shared by the daemon and clients.
//!
//! This crate owns deterministic, I/O-free derivation. Provider folds and the
//! persisted store vocabulary are introduced in the next extraction step.

#![forbid(unsafe_code)]
