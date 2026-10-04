//! Cargo, for tests that build binaries of their own mid-run.

use std::ffi::OsStr;
use std::process::Command;

/// The cargo that runs these tests, without the variables it set for the
/// package under test.
///
/// Cargo compares a build script's `rerun-if-env-changed` variables
/// against its own environment. `ring` declares `CARGO_MANIFEST_DIR` and
/// `CARGO_PKG_*`, so a build started from one package's tests with that
/// package's values reruns `ring` and recompiles everything above it
/// (rustls, quinn, node, amux): about a minute for every test crate in a
/// workspace run.
pub fn command() -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    for (name, _) in std::env::vars_os() {
        if names_the_package(&name) {
            command.env_remove(name);
        }
    }
    command
}

fn names_the_package(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || matches!(
            name,
            "CARGO_CRATE_NAME" | "CARGO_PRIMARY_PACKAGE" | "CARGO_TARGET_TMPDIR" | "OUT_DIR"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_package_s_own_variables_are_dropped() {
        for name in [
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
            "CARGO_BIN_EXE_amux",
            "CARGO_CRATE_NAME",
            "CARGO_TARGET_TMPDIR",
            "OUT_DIR",
        ] {
            assert!(names_the_package(OsStr::new(name)), "{name}");
        }
        for name in [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_TERM_COLOR",
            "CARGO_BUILD_JOBS",
            "CARGO_INCREMENTAL",
            "RUSTFLAGS",
        ] {
            assert!(!names_the_package(OsStr::new(name)), "{name}");
        }
    }
}
