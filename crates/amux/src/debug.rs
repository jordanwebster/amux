use std::path::Path;

use serde::{Serialize, Serializer};

/// Serde-driven debug rendering wrapper used across domains.
pub(crate) struct DebugView<'a, T: ?Sized> {
    pub inner: &'a T,
    pub verbose: bool,
}

impl<'a, T: ?Sized> DebugView<'a, T> {
    pub fn new(inner: &'a T, verbose: bool) -> Self {
        Self { inner, verbose }
    }
}

/// Infallible path serialization wrapper.
pub(crate) struct LossyPath<'a>(pub &'a Path);

impl Serialize for LossyPath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}
