//! Cross-platform helper for preventing idle system sleep while the amux
//! server is running.
//!
//! Platform-specific behavior:
//! - macOS: Uses native IOKit power assertions.
//! - Linux: Uses the first available backend from an ordered list.
//! - Windows: Uses `PowerCreateRequest` + `PowerSetRequest`.
//! - Other platforms: No-op backend.

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod dummy;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use dummy as imp;
#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(target_os = "windows")]
use windows as imp;

/// Prevent idle sleep for the lifetime of the owning value when enabled.
#[derive(Debug)]
pub(crate) struct SleepInhibitor {
    #[allow(dead_code)] // Held for platform-specific drop cleanup.
    platform: imp::SleepInhibitor,
}

impl SleepInhibitor {
    pub(crate) fn new(enabled: bool) -> Self {
        let mut platform = imp::SleepInhibitor::new();
        if enabled {
            platform.acquire();
        }
        Self { platform }
    }
}

pub(crate) fn supported() -> bool {
    imp::supported()
}

#[cfg(test)]
mod tests {
    use super::SleepInhibitor;

    #[test]
    fn disabled_sleep_inhibitor_is_noop() {
        let _inhibitor = SleepInhibitor::new(false);
    }
}
