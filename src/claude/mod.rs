//! Claude Code integration.
//!
//! This module colocates all Claude Code-specific functionality:
//! - [`types`] — Hook events, permission requests, tool inputs, structured I/O
//! - [`hooks`] — Client-side hook handler (`amux hook` CLI subcommand)
//! - [`transcript`] — JSONL transcript file tailer
//! - [`plugin`] — Plugin installation and update management

pub mod hooks;
pub mod plugin;
pub(crate) mod structured_log_source;
pub(crate) mod transcript;
pub(crate) mod types;
