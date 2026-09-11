use serde::Serialize;

pub(crate) struct DebugView<'a, T: ?Sized> {
    pub inner: &'a T,
    pub verbose: bool,
}

impl<'a, T: ?Sized> DebugView<'a, T> {
    pub fn new(inner: &'a T, verbose: bool) -> Self {
        Self { inner, verbose }
    }
}

pub(crate) struct LossyPath<'a>(pub &'a std::path::Path);

impl Serialize for LossyPath<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}

