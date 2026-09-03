//! Emits `cfg(specs)`.
//!
//! The executable-specification scaffolding (`claude::specs`, the probe
//! binary, the replay tests) is compiled only in a debug profile. Keying that
//! on the profile rather than on a cargo feature means every dev command
//! compiles one `claude` instead of one with the scaffolding and one
//! without, and a release binary cannot contain it however cargo is invoked.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(specs)");
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "debug") {
        println!("cargo::rustc-cfg=specs");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
