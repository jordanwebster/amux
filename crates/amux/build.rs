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
    prost_build::Config::new()
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(&["proto/amux/v1/amux.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/amux/v1/amux.proto");
    Ok(())
}
