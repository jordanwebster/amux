//! Shared painted-block vocabulary.

/// Renderer-local identity for an expandable run of related feed entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RunKey(pub(crate) u64);
