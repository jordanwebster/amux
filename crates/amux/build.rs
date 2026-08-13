use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build scripts are short-lived single-purpose processes. Setting
    // PROTOC here affects only prost-build invoked below.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let descriptor_path = out_dir.join("amux.v1.bin");
    tonic_build::configure()
        // Keep generated clients, but omit tonic's transport convenience
        // constructors. Otherwise `RoutingService.Connect` collides with the
        // inherent `RoutingServiceClient::connect(endpoint)` constructor.
        .build_transport(false)
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(
            &[
                "proto/amux/v1/amux.proto",
                "proto/amux/v1/claude.proto",
                "proto/amux/v1/codex.proto",
                "proto/amux/v1/test_agent.proto",
            ],
            &["proto"],
        )?;

    println!("cargo:rerun-if-changed=proto/amux/v1/amux.proto");
    println!("cargo:rerun-if-changed=proto/amux/v1/claude.proto");
    println!("cargo:rerun-if-changed=proto/amux/v1/codex.proto");
    println!("cargo:rerun-if-changed=proto/amux/v1/test_agent.proto");
    Ok(())
}
