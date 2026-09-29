//! The amux binary and the fake providers the net's agents run.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::topology::FakeKind;

/// Where the built `amux` and fake provider binaries are: the target
/// directory this process runs from.
#[derive(Clone, Debug)]
pub struct Binaries {
    dir: PathBuf,
}

impl Binaries {
    /// Builds `amux` and the fake providers once per process with the same
    /// cargo and profile that built this one, and finds them beside it.
    /// Agents run the binary from the tree under test, never an installed
    /// one, and a release run measures release agents.
    pub fn built() -> &'static Binaries {
        static BUILT: OnceLock<Binaries> = OnceLock::new();
        BUILT.get_or_init(|| {
            let dir = target_dir();
            let mut args = vec![
                "build",
                "--locked",
                "-p",
                "amux",
                "-p",
                "provider-fakes",
                "--bins",
            ];
            if dir.file_name().and_then(|name| name.to_str()) == Some("release") {
                args.push("--release");
            }
            let status = provider_fakes::cargo::command()
                .args(args)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .stdout(std::process::Stdio::null())
                .status()
                .expect("cargo runs");
            assert!(
                status.success(),
                "building amux and the fake providers failed"
            );
            Binaries { dir }
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn amux(&self) -> PathBuf {
        self.binary("amux")
    }

    pub fn fake(&self, kind: FakeKind) -> PathBuf {
        self.binary(kind.binary())
    }

    fn binary(&self, name: &str) -> PathBuf {
        self.dir
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    }
}

/// `target/<profile>`, from this executable: a test binary lives one level
/// further down, in `deps`.
fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("this executable's path");
    let dir = exe.parent().expect("a target directory");
    match dir.file_name().and_then(|name| name.to_str()) {
        Some("deps") => dir.parent().expect("a target directory").to_owned(),
        _ => dir.to_owned(),
    }
}
