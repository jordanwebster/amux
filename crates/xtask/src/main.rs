use std::path::Path;

mod ci;
mod door;
mod golden;
use xtask::ios_verify;
mod simulator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("codegen") => codegen(),
        Some("proto-check") => {
            xtask::proto_check::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        Some("ci-status") => ci::main(),
        Some("ci-observe") => ci::observe_main(),
        Some("door") => door::main(),
        Some("golden") => golden::main(),
        Some("ios-verify") => ios_verify::run(),
        Some("swift-types") => {
            xtask::swift_types::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        Some("restamp") => restamp(&std::env::args().skip(2).collect::<Vec<_>>()),
        _ => {
            eprintln!(
                "usage: xtask <codegen|swift-types [--check]|proto-check [--update]|ci-status [--wait SECS]|ci-observe [--settle SECS] [--wait SECS] [--record PATH]|golden diff [ARGS]|restamp FROM TO [VERSION]|door [--simulator NAME] [--bundle-id ID] [--install APP] [--timeout SECS] [--requests FILE] [JSON...]|ios-verify>"
            );
            std::process::exit(2);
        }
    }
}

/// Copies the amux binary at FROM to TO stamped with VERSION, by default
/// one just below its own, to play the previous release in overlap tests.
fn restamp(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
        return Err("usage: xtask restamp FROM TO [VERSION]".into());
    };
    let (from, to) = (Path::new(from), Path::new(to));
    let version = match args.get(2) {
        Some(version) => version.clone(),
        None => {
            let own = version_stamp::read_file(from)?;
            version_stamp::previous(&own)
                .ok_or_else(|| format!("{own} has a prerelease; name the version to stamp"))?
        }
    };
    version_stamp::restamp(from, to, &version)?;
    println!("{} reports {version}", to.display());
    Ok(())
}

/// Wire types a view value holds.
const VIEW_VALUES: &[&str] = &[
    "amux.v1.BlobRef",
    "amux.v1.BoundaryKind",
    "amux.v1.Diff",
    "amux.v1.DiffBase",
    "amux.v1.DiffBase.base",
    "amux.v1.Empty",
    "amux.v1.EnvelopeKind",
    "amux.v1.HostVia",
    "amux.v1.Kind",
    "amux.v1.Presence",
    "amux.v1.ReviewComment",
    "amux.v1.SendState",
    "amux.v1.SignInState",
];

/// Regenerates the committed protobuf code under
/// `crates/wire/src/generated/` from `crates/wire/proto/`.
///
/// The output is committed rather than built in `OUT_DIR` so that the
/// generated Rust is an ordinary tracked input — visible to git, review,
/// rust-analyzer, and every build cache — and so building amux needs no
/// protoc. CI regenerates and fails if the committed output is stale.
fn codegen() -> Result<(), Box<dyn std::error::Error>> {
    let wire_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under crates/")
        .join("wire");
    let proto_dir = wire_dir.join("proto");
    let out_dir = wire_dir.join("src/generated");
    std::fs::create_dir_all(&out_dir)?;

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: set before any codegen work and never mutated again; xtask is a
    // short-lived single-threaded process. The vendored protoc keeps the
    // output independent of whatever protoc the host has installed.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    // Type names let code that walks an encoded message by its descriptor,
    // such as the dump redactor, find the descriptor of a generated type.
    let mut config = tonic_prost_build::Config::new();
    config.enable_type_names();
    // The wire values the chat views carry to the phone serialize, with
    // schemas, so the Swift mirrors are generated from these definitions.
    for path in VIEW_VALUES {
        config.type_attribute(
            path,
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        );
    }
    tonic_prost_build::configure()
        // Keep generated clients, but omit tonic's transport convenience
        // constructors. Otherwise `RoutingService.Connect` collides with the
        // inherent `RoutingServiceClient::connect(endpoint)` constructor.
        .build_transport(false)
        .file_descriptor_set_path(out_dir.join("amux.v1.bin"))
        .out_dir(&out_dir)
        .compile_with_config(
            config,
            &xtask::proto_check::PROTO_FILES
                .iter()
                .map(|file| proto_dir.join(file))
                .collect::<Vec<_>>(),
            &[proto_dir],
        )?;
    Ok(())
}
