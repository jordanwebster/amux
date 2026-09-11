use std::path::Path;

pub use model::DebugFormat;
use serde::{Serialize, Serializer};

mod server;

pub(crate) use server::dump_server_debug_info;

/// Infallible path serialization wrapper.
pub(crate) struct LossyPath<'a>(pub &'a Path);

impl Serialize for LossyPath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}
