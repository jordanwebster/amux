//! Claude Code integration.
//!
//! This module colocates Claude Code-specific types:
//! - [`types`] — Hook event types
//! - [`transcript`] — JSONL transcript file tailer

pub(crate) mod structured_log_source;
pub(crate) mod transcript;
pub mod types;
