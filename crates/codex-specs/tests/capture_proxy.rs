//! The probe standing in for `codex` on the PATH, as the live lane puts it,
//! reaches the real Codex through a wrapper earlier on the PATH that runs
//! the next `codex` it finds. Were the probe still on the PATH the real
//! provider inherits, the wrapper would find the probe again, and so on
//! without end; the wrapper here gives up after a few rounds instead.

#![cfg(unix)]

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs the next `codex` on the PATH past its own folder, as a usage meter
/// or version manager does.
const WRAPPER: &str = r#"#!/bin/sh
rounds=$((${WRAPPER_ROUNDS:-0} + 1))
if [ "$rounds" -gt 3 ]; then echo looped; exit 3; fi
export WRAPPER_ROUNDS=$rounds
here=$(cd "$(dirname "$0")" && pwd)
IFS=:
for folder in $PATH; do
  [ "$folder" = "$here" ] && continue
  [ -x "$folder/codex" ] && exec "$folder/codex" "$@"
done
echo "no codex past the wrapper"; exit 4
"#;

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_wrapper_ahead_of_the_probe_reaches_the_real_codex() {
    let root = tempfile::tempdir().unwrap();
    let [shim, wrapper, real, capture] =
        ["shim", "wrapper", "real", "capture"].map(|name| root.path().join(name));
    for folder in [&shim, &wrapper, &real, &capture] {
        std::fs::create_dir_all(folder).unwrap();
    }
    symlink(env!("CARGO_BIN_EXE_codex-probe"), shim.join("codex")).unwrap();
    script(&wrapper.join("codex"), WRAPPER);
    script(&real.join("codex"), "#!/bin/sh\necho reached\n");
    let path = std::env::join_paths([
        &shim,
        &wrapper,
        &real,
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();

    let output = Command::new(shim.join("codex"))
        .arg("--version")
        .env("PATH", path)
        .env("CODEX_CAPTURE_PROXY", "1")
        .env("CODEX_CAPTURE_DIR", &capture)
        .env("CODEX_REAL_PATH", wrapper.join("codex"))
        .env_remove("WRAPPER_ROUNDS")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "reached",
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
