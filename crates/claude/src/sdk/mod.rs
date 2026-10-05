//! What a host or client of Claude's headless stream reports when the
//! stream fails. The stream's own types are `claude_protocol::stream`.

pub mod error;

pub use error::Error;
