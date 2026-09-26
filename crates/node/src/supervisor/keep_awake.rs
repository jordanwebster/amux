//! The machine's sleep assertion, held by the supervisor for its lifetime
//! under `keep_awake: on`, so a phone can spawn on the machine while nobody
//! is at it, whether or not an agent is working. Neither the daemon nor an
//! agent holds one, so it survives daemon restarts and the update gap.
//!
//! On macOS it takes two IOKit assertions: PreventSystemSleep, which the
//! system honours on AC power only, and PreventUserIdleSystemSleep, which it
//! honours on battery too. Together they keep a machine awake when it is
//! plugged in with the lid open; whether a closed lid is honoured on AC is
//! unmeasured. On Windows it is SetThreadExecutionState on a thread kept
//! for the purpose. Linux holds none: a service-managed server does not
//! sleep, and desktops differ in how they would be asked.

/// What `pmset -g assertions` shows as the assertion's name.
pub const REASON: &str = "amux keeps this machine reachable";

/// Held for as long as it lives.
pub struct KeepAwake {
    #[allow(dead_code)] // Released on drop.
    held: imp::Held,
}

impl KeepAwake {
    /// Takes the assertion; a failure is logged and leaves the machine free
    /// to sleep, which is no worse than keep_awake off.
    pub fn hold() -> Self {
        Self { held: imp::hold() }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CString, c_char, c_void};

    use super::REASON;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: *const c_void,
            level: u32,
            name: *const c_void,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    const UTF8: u32 = 0x0800_0100;
    const LEVEL_ON: u32 = 255;
    /// Honoured on AC power only: the strongest a user process can hold.
    const SYSTEM: &str = "PreventSystemSleep";
    /// Honoured on battery too.
    const IDLE: &str = "PreventUserIdleSystemSleep";

    pub struct Held(Vec<u32>);

    struct CfString(*const c_void);

    impl CfString {
        fn new(text: &str) -> Self {
            let text = CString::new(text).expect("no interior NUL");
            // SAFETY: a valid C string and the default allocator.
            Self(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own the one reference Create returned.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    pub fn hold() -> Held {
        let name = CfString::new(REASON);
        let mut held = Vec::new();
        for kind in [SYSTEM, IDLE] {
            let kind_string = CfString::new(kind);
            let mut id = 0;
            // SAFETY: two live CFStrings and an out parameter we own.
            let result =
                unsafe { IOPMAssertionCreateWithName(kind_string.0, LEVEL_ON, name.0, &mut id) };
            if result == 0 {
                held.push(id);
            } else {
                tracing::warn!(
                    assertion = kind,
                    result,
                    "could not take the sleep assertion"
                );
            }
        }
        tracing::info!(assertions = held.len(), "holding the sleep assertion");
        Held(held)
    }

    impl Drop for Held {
        fn drop(&mut self) {
            for id in self.0.drain(..) {
                // SAFETY: an assertion this process created.
                unsafe { IOPMAssertionRelease(id) };
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;

    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    /// The state belongs to the thread that set it, so one thread holds it
    /// and clears it when the sender drops.
    pub struct Held(#[allow(dead_code)] mpsc::Sender<()>);

    pub fn hold() -> Held {
        let (sender, receiver) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            // SAFETY: plain calls on this thread's own execution state.
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } == 0 {
                tracing::warn!("could not take the sleep assertion");
                return;
            }
            tracing::info!("holding the sleep assertion");
            // Returns once the sender is dropped.
            let _ = receiver.recv();
            // SAFETY: as above.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        });
        Held(sender)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub struct Held;

    pub fn hold() -> Held {
        tracing::info!("this platform holds no sleep assertion");
        Held
    }
}
