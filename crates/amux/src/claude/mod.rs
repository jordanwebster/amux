//! Claude Code integration.
//!
//! This module colocates Claude Code-specific types:
//! - [`types`] — Hook events, permission requests, tool inputs, structured I/O
//! - [`transcript`] — JSONL transcript file tailer

pub(crate) mod structured_log_source;
pub(crate) mod transcript;
pub mod types;
