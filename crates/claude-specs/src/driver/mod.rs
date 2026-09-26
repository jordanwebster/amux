//! The drivers the specifications record and replay through.
//!
//! They host a Claude session the way a program embedding Claude would: the
//! SDK driver speaks stream-JSON over a child's pipes and answers Claude's
//! control requests; the PTY driver runs Claude in a terminal and reads its
//! hooks and transcript. They exist so a specification can make a claim
//! about provider traffic; the product hosts Claude in its agent process.

pub mod pty;
pub mod sdk;
