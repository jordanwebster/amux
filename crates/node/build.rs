//! Emits debug-only test fixture cfgs.
//!
//! The in-process spec-test harness (`amux::testnet`) and the hooks it needs
//! are compiled only in a debug profile with `local-agents` on. Keying that
//! on the profile rather than on a cargo feature means every dev command —
//! build, test, clippy, run, any `-p` selection — compiles one `amux`
//! instead of one with the harness and one without, and a release binary
//! cannot contain the harness however cargo is invoked.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(testnet)");
    println!("cargo::rustc-check-cfg=cfg(test_fixtures)");
    let debug_profile = std::env::var("PROFILE").is_ok_and(|profile| profile == "debug");
    let local_agents = std::env::var_os("CARGO_FEATURE_LOCAL_AGENTS").is_some();
    if debug_profile {
        println!("cargo::rustc-cfg=test_fixtures");
    }
    if debug_profile && local_agents {
        println!("cargo::rustc-cfg=testnet");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
