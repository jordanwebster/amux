//! Native client bridge over the shared protocol and UI runtime.

use std::ffi::c_char;

/// Returns the bridge version as a NUL-terminated UTF-8 string.
/// The pointer remains valid for the process lifetime; do not free or modify it.
#[unsafe(no_mangle)]
pub extern "C" fn amux_mobile_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}
