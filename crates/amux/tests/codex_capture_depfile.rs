#[path = "codex_capture/depfile.rs"]
mod depfile;

use std::path::Path;
use std::time::Duration;

use depfile::{
    assert_binary_is_current, assert_binary_is_current_from_depfile, parse_depfile_prerequisites,
};

#[test]
fn parses_make_prerequisites_including_continuations_and_non_rust_inputs() {
    let depfile = concat!(
        "/tmp/amux: /work/src/main.rs /work/proto/amux.proto \\",
        "\n /work/plugin\\ files/plugin.json\n"
    );
    let prerequisites = parse_depfile_prerequisites(depfile).unwrap();
    assert_eq!(
        prerequisites,
        [
            Path::new("/work/src/main.rs"),
            Path::new("/work/proto/amux.proto"),
            Path::new("/work/plugin files/plugin.json"),
        ]
    );
}

#[test]
fn parses_windows_drive_paths_without_treating_separators_as_make_escapes() {
    let depfile = concat!(
        "C:\\target\\debug\\amux.exe: C:\\work\\src\\main.rs ",
        "C:\\work\\plugin\\ files\\plugin.json\r\n"
    );
    let prerequisites = parse_depfile_prerequisites(depfile).unwrap();
    assert_eq!(
        prerequisites,
        [
            Path::new(r"C:\work\src\main.rs"),
            Path::new(r"C:\work\plugin files\plugin.json"),
        ]
    );
}

#[test]
fn rejects_when_a_real_depfile_dependency_is_newer_than_the_binary() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("amux");
    let dependency = temp.path().join("amux.proto");
    let depfile = temp.path().join("amux.d");
    std::fs::write(&binary, "binary").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&dependency, "syntax = proto3;").unwrap();
    std::fs::write(
        &depfile,
        format!("{}: {}\n", binary.display(), dependency.display()),
    )
    .unwrap();

    let error = assert_binary_is_current_from_depfile(&binary, &depfile)
        .unwrap_err()
        .to_string();
    assert!(error.contains("amux.proto"), "{error}");
    assert!(error.contains("cargo build -p amux-cli"), "{error}");
}

#[test]
fn ignores_newer_workspace_files_absent_from_the_depfile() {
    let temp = tempfile::tempdir().unwrap();
    let dependency = temp.path().join("included.rs");
    let binary = temp.path().join("amux");
    let unrelated = temp.path().join("e2e-runner.rs");
    let depfile = temp.path().join("amux.d");
    std::fs::write(&dependency, "included").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&binary, "binary").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&unrelated, "unrelated").unwrap();
    std::fs::write(
        &depfile,
        format!("{}: {}\n", binary.display(), dependency.display()),
    )
    .unwrap();

    assert_binary_is_current_from_depfile(&binary, &depfile).unwrap();
}

#[test]
fn missing_depfile_error_names_the_build_command() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("amux");
    std::fs::write(&binary, "binary").unwrap();
    let missing = temp.path().join("amux.d");

    let error = assert_binary_is_current_from_depfile(&binary, &missing)
        .unwrap_err()
        .to_string();
    assert!(error.contains("amux.d"), "{error}");
    assert!(error.contains("cargo build -p amux-cli"), "{error}");
}

#[test]
fn missing_binary_error_names_the_build_command() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("amux");

    let error = assert_binary_is_current(&missing).unwrap_err().to_string();
    assert!(error.contains("amux"), "{error}");
    assert!(error.contains("cargo build -p amux-cli"), "{error}");
}
