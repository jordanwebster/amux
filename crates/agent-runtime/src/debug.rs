pub(crate) struct DebugView<'a, T: ?Sized> {
    pub inner: &'a T,
    pub verbose: bool,
}

impl<'a, T: ?Sized> DebugView<'a, T> {
    pub fn new(inner: &'a T, verbose: bool) -> Self {
        Self { inner, verbose }
    }
}
