//! A design variant number the renderer may branch on while designs are
//! compared side by side. Zero is the current design, and the shipped client
//! never sets it. The TUI lab cycles it with a key; variant 1 draws the old
//! home.

use std::sync::atomic::{AtomicU8, Ordering};

static VARIANT: AtomicU8 = AtomicU8::new(0);

/// The variant being drawn.
pub fn get() -> u8 {
    VARIANT.load(Ordering::Relaxed)
}

pub fn set(variant: u8) {
    VARIANT.store(variant, Ordering::Relaxed);
}
