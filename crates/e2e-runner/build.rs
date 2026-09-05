fn main() {
    println!("cargo::rustc-check-cfg=cfg(testnet)");
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "debug") {
        println!("cargo::rustc-cfg=testnet");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
