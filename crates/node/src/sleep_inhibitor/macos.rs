use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use tracing::{info, warn};

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all
)]
mod iokit {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {}

    include!("iokit_bindings.rs");
}

type IOPMAssertionID = iokit::IOPMAssertionID;
type IOPMAssertionLevel = iokit::IOPMAssertionLevel;
type IOReturn = iokit::IOReturn;

const ASSERTION_REASON: &str = "amux server is running";
const ASSERTION_TYPE_PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";

#[derive(Debug, Default)]
pub(crate) struct SleepInhibitor {
    assertion: Option<MacSleepAssertion>,
}

impl SleepInhibitor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn acquire(&mut self) {
        if self.assertion.is_some() {
            return;
        }

        match MacSleepAssertion::create(ASSERTION_REASON) {
            Ok(assertion) => {
                info!("idle sleep prevention active via IOKit");
                self.assertion = Some(assertion);
            }
            Err(error) => {
                warn!(
                    iokit_error = error,
                    "failed to acquire macOS idle sleep inhibitor"
                );
            }
        }
    }
}

pub(crate) fn supported() -> bool {
    true
}

#[derive(Debug)]
struct MacSleepAssertion {
    id: IOPMAssertionID,
}

impl MacSleepAssertion {
    fn create(name: &str) -> Result<Self, IOReturn> {
        let assertion_type = CFString::new(ASSERTION_TYPE_PREVENT_USER_IDLE_SYSTEM_SLEEP);
        let assertion_name = CFString::new(name);
        let mut id: IOPMAssertionID = 0;
        let assertion_type_ref: iokit::CFStringRef = assertion_type.as_concrete_TypeRef().cast();
        let assertion_name_ref: iokit::CFStringRef = assertion_name.as_concrete_TypeRef().cast();
        let result = unsafe {
            iokit::IOPMAssertionCreateWithName(
                assertion_type_ref,
                iokit::kIOPMAssertionLevelOn as IOPMAssertionLevel,
                assertion_name_ref,
                &mut id,
            )
        };
        if result == iokit::kIOReturnSuccess as IOReturn {
            Ok(Self { id })
        } else {
            Err(result)
        }
    }
}

impl Drop for MacSleepAssertion {
    fn drop(&mut self) {
        let result = unsafe { iokit::IOPMAssertionRelease(self.id) };
        if result != iokit::kIOReturnSuccess as IOReturn {
            warn!(
                iokit_error = result,
                "failed to release macOS idle sleep inhibitor"
            );
        }
    }
}
