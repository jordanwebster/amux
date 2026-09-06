fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rustc-check-cfg=cfg(testnet)");
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "debug") {
        println!("cargo::rustc-cfg=testnet");
    }
    println!("cargo::rerun-if-changed=build.rs");
    let proto = "../amux/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: this build script is single-threaded and sets PROTOC before codegen.
    unsafe { std::env::set_var("PROTOC", protoc) };
    tonic_prost_build::configure()
        .build_server(false)
        .build_transport(false)
        .compile_protos(
            &[format!("{proto}/amux/v1/amux.proto")],
            &[proto.to_string()],
        )?;
    println!("cargo:rerun-if-changed={proto}");
    Ok(())
}
