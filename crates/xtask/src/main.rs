use std::path::Path;

mod ci;
mod door;
mod ios_verify;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("codegen") => codegen(),
        Some("ci-status") => ci::main(),
        Some("ci-observe") => ci::observe_main(),
        Some("door") => door::main(),
        Some("ios-verify") => ios_verify::run(),
        _ => {
            eprintln!(
                "usage: xtask <codegen|ci-status [--wait SECS]|ci-observe [--settle SECS] [--wait SECS] [--record PATH]|door [--simulator NAME] [--bundle-id ID] [--install APP] [--timeout SECS] [--requests FILE] [JSON...]|ios-verify>"
            );
            std::process::exit(2);
        }
    }
}

/// Regenerates the committed protobuf code under
/// `crates/amux/src/protocol/generated/` from `crates/amux/proto/`.
///
/// The output is committed rather than built in `OUT_DIR` so that the
/// generated Rust is an ordinary tracked input — visible to git, review,
/// rust-analyzer, and every build cache — and so building amux needs no
/// protoc. CI regenerates and fails if the committed output is stale.
fn codegen() -> Result<(), Box<dyn std::error::Error>> {
    let amux_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under crates/")
        .join("amux");
    let proto_dir = amux_dir.join("proto");
    let out_dir = amux_dir.join("src/protocol/generated");
    std::fs::create_dir_all(&out_dir)?;

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: set before any codegen work and never mutated again; xtask is a
    // short-lived single-threaded process. The vendored protoc keeps the
    // output independent of whatever protoc the host has installed.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    tonic_prost_build::configure()
        // Keep generated clients, but omit tonic's transport convenience
        // constructors. Otherwise `RoutingService.Connect` collides with the
        // inherent `RoutingServiceClient::connect(endpoint)` constructor.
        .build_transport(false)
        .file_descriptor_set_path(out_dir.join("amux.v1.bin"))
        .out_dir(&out_dir)
        .compile_protos(
            &[
                proto_dir.join("amux/v1/amux.proto"),
                proto_dir.join("amux/v1/claude.proto"),
                proto_dir.join("amux/v1/codex.proto"),
                proto_dir.join("amux/v1/test_agent.proto"),
            ],
            &[proto_dir],
        )?;
    Ok(())
}
