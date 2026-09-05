#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn commands() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // These fixtures execute only immediate local scripts. The CLI contracts
    // can be tested on macOS hosts that have not installed GNU coreutils yet.
    executable(
        &dir.path().join("timeout"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    dir
}

#[test]
fn ci_status_cli_emits_typed_failures_at_the_command_boundary() {
    let dir = commands();
    executable(
        &dir.path().join("git"),
        "#!/bin/sh\ncase \"$1\" in\nrev-parse) echo head;;\nls-remote) echo \"$REMOTE refs/heads/nativeapp\";;\nesac\n",
    );
    executable(
        &dir.path().join("gh"),
        "#!/bin/sh\nif [ \"$1\" = repo ]; then echo owner/repo; else echo '{\"workflow_runs\":[]}'; fi\n",
    );
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    for (remote, expected) in [("old", "NotPushed"), ("head", "NoRunForHead")] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["ci-status", "--wait", "0"])
            .env("PATH", &path)
            .env("REMOTE", remote)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::json!({"error": expected})
        );
    }
}

#[test]
fn ios_verify_cli_runs_available_checks_in_order_and_stops_on_failure() {
    let dir = commands();
    std::fs::write(
        dir.path().join(".wt.toml"),
        "[task.test]\nrun='test'\n[task.mobile-check]\nrun='mobile'\n[task.ios-unit]\nrun='unit'\n",
    )
    .unwrap();
    executable(
        &dir.path().join("wt"),
        "#!/bin/sh\necho \"$*\" >> calls\n[ \"$2\" != \"$FAIL_RECIPE\" ]\n",
    );
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    for (fail, success, expected) in [
        ("", true, "run test\nrun mobile-check\nrun ios-unit\n"),
        ("mobile-check", false, "run test\nrun mobile-check\n"),
    ] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("ios-verify")
            .current_dir(dir.path())
            .env("PATH", &path)
            .env("FAIL_RECIPE", fail)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            expected
        );
    }
}
