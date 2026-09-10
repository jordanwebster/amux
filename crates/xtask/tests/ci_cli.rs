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

fn observation_commands() -> tempfile::TempDir {
    let dir = commands();
    executable(
        &dir.path().join("git"),
        r#"#!/bin/sh
echo "git $*" >> "$CALLS"
case "$*" in
  'rev-parse HEAD') echo head;;
  'branch --show-current') echo "$BRANCH";;
  'status --porcelain --untracked-files=normal') printf '%s' "$DIRTY";;
  'push origin HEAD:nativeapp') exit "${PUSH_EXIT:-0}";;
  'ls-remote origin refs/heads/nativeapp') echo "$REMOTE refs/heads/nativeapp";;
  *) exit 91;;
esac
"#,
    );
    executable(
        &dir.path().join("gh"),
        r#"#!/bin/sh
echo "gh $*" >> "$CALLS"
case "$*" in
  'repo view --json nameWithOwner --jq .nameWithOwner') echo owner/repo;;
  'api repos/owner/repo/actions/workflows/ci.yml/runs?branch=nativeapp&event=push&head_sha=head&per_page=100') echo "$RUNS_JSON";;
  'api repos/owner/repo/actions/workflows/ci.yml/runs?branch=nativeapp&event=push&per_page=100&page=1') echo "$PRIOR_JSON";;
  'api repos/owner/repo/actions/runs/42/jobs?filter=latest&per_page=100&page=1') echo "$JOBS_JSON";;
  *) exit 92;;
esac
"#,
    );
    dir
}

fn run_fixture(id: u64, head: &str, conclusion: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": id, "head_sha": head,
        "html_url": format!("https://github.com/owner/repo/actions/runs/{id}"),
        "status": if conclusion.is_some() { "completed" } else { "queued" },
        "conclusion": conclusion,
    })
}

fn observation_command(dir: &Path, scenario: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    observation_environment(&mut command, dir, scenario);
    command
}

fn observation_environment(command: &mut Command, dir: &Path, scenario: &str) {
    let runs = match scenario {
        "missing" => vec![],
        "failed" => vec![run_fixture(42, "head", Some("failure"))],
        "success" => vec![run_fixture(42, "head", Some("success"))],
        // An older green run must not mask the queued latest run of this head.
        _ => vec![
            run_fixture(40, "head", Some("success")),
            run_fixture(42, "head", None),
        ],
    };
    let prior = match scenario {
        "pending-red" => vec![run_fixture(41, "previous", Some("failure"))],
        "pending-absent" => vec![],
        _ => vec![run_fixture(41, "previous", Some("success"))],
    };
    let jobs = serde_json::json!({"jobs": [{
        "name": "ios", "status": "completed", "conclusion": "success",
        "started_at": "2026-09-05T00:00:00Z", "completed_at": "2026-09-05T00:02:00Z",
        "steps": [{"name": "Run iOS verification", "conclusion": "success"}]
    }]});
    command
        .current_dir(dir)
        .env(
            "PATH",
            format!("{}:{}", dir.display(), std::env::var("PATH").unwrap()),
        )
        .env("CALLS", dir.join("calls"))
        .env("BRANCH", "nativeapp")
        .env("DIRTY", "")
        .env("PUSH_EXIT", "0")
        .env("REMOTE", "head")
        .env(
            "RUNS_JSON",
            serde_json::json!({"workflow_runs": runs}).to_string(),
        )
        .env(
            "PRIOR_JSON",
            serde_json::json!({"workflow_runs": prior}).to_string(),
        )
        .env("JOBS_JSON", jobs.to_string());
}

#[test]
fn ci_observe_cli_records_pending_failed_missing_and_success_without_real_remotes() {
    let dir = observation_commands();
    let record = dir.path().join("nested/observations.jsonl");
    for (scenario, status, exit, error) in [
        ("pending", "pending", 0, None),
        ("pending-absent", "pending", 0, None),
        ("pending-red", "pending", 1, Some("StillRunning")),
        ("failed", "failed", 1, Some("Failed")),
        ("missing", "missing", 1, Some("NoRunForHead")),
        ("success", "success", 0, None),
    ] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let output = observation_command(dir.path(), scenario)
            .args(["ci-observe", "--wait", "0", "--settle", "0", "--record"])
            .arg(&record)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["head"], "head");
        assert_eq!(result["status"], status);
        assert_eq!(result["error"]["error"].as_str(), error);
        assert!(
            result["observed_at"]
                .as_str()
                .unwrap()
                .parse::<chrono::DateTime<chrono::Utc>>()
                .is_ok()
        );
        if scenario == "missing" {
            assert!(result["run_id"].is_null());
            assert!(result["url"].is_null());
        } else {
            assert_eq!(result["run_id"], 42);
            assert_eq!(
                result["url"],
                "https://github.com/owner/repo/actions/runs/42"
            );
        }
        if matches!(scenario, "pending" | "pending-red") {
            assert_eq!(result["prior"]["head"], "previous");
            assert_eq!(result["prior"]["run_id"], 41);
            assert_eq!(
                result["prior"]["url"],
                "https://github.com/owner/repo/actions/runs/41"
            );
            assert_eq!(
                result["prior"]["conclusion"],
                if scenario == "pending-red" {
                    "failure"
                } else {
                    "success"
                }
            );
        } else {
            assert!(result["prior"].is_null());
        }
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1);
        let recorded = std::fs::read_to_string(&record).unwrap();
        assert_eq!(recorded.lines().last().unwrap(), stdout.trim());
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("git push"))
                .collect::<Vec<_>>(),
            ["git push origin HEAD:nativeapp"]
        );
        if let Some(evidence) = std::env::var_os("AMUX_CI_CLI_EVIDENCE_DIR") {
            let evidence = std::path::PathBuf::from(evidence);
            std::fs::create_dir_all(&evidence).unwrap();
            std::fs::write(
                evidence.join(format!("stub-ci-observe-{scenario}.json")),
                stdout,
            )
            .unwrap();
        }
    }
    assert_eq!(std::fs::read_to_string(record).unwrap().lines().count(), 6);
}

#[test]
fn ci_observe_cli_refuses_wrong_branch_dirty_tree_push_failure_and_not_pushed() {
    let dir = observation_commands();
    for (variable, value, error, pushed) in [
        ("BRANCH", "main", "WrongBranch", false),
        ("DIRTY", "?? untracked", "DirtyTree", false),
        ("PUSH_EXIT", "1", "ToolFailure", true),
        ("REMOTE", "old", "NotPushed", true),
    ] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let output = observation_command(dir.path(), "pending")
            .args(["ci-observe", "--settle", "0", "--wait", "0"])
            .env(variable, value)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["error"]["error"], error);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(calls.contains("git push"), pushed);
    }
}

#[test]
fn ci_observe_script_forwards_options_and_reports_record_or_argument_errors() {
    let dir = observation_commands();
    executable(
        &dir.path().join("cargo"),
        r#"#!/bin/sh
[ "$1 $2 $3 $4 $5" = 'run -q -p xtask --' ] || exit 94
shift 5
exec "$XTASK" "$@"
"#,
    );
    let record = dir.path().join("directory with spaces/record.jsonl");
    let mut command = Command::new("sh");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ci-observe.sh"))
        .args(["--wait", "0", "--settle", "0", "--record"])
        .arg(&record)
        .env("XTASK", env!("CARGO_BIN_EXE_xtask"));
    observation_environment(&mut command, dir.path(), "pending");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(record).unwrap(), output.stdout);

    let output = observation_command(dir.path(), "pending")
        .args(["ci-observe", "--settle", "0", "--wait", "0", "--record"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error"]["error"], "ToolFailure");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("record")
    );

    std::fs::write(dir.path().join("calls"), "").unwrap();
    let output = observation_command(dir.path(), "pending")
        .args(["ci-observe", "--wait"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error"]["error"], "ToolFailure");
    assert!(
        std::fs::read_to_string(dir.path().join("calls"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ci_status_and_ci_gate_still_reject_unpushed_missing_pending_and_failed() {
    let dir = observation_commands();
    executable(
        &dir.path().join("wt"),
        r#"#!/bin/sh
echo "wt $*" >> "$CALLS"
[ "$*" = 'run ci-status -- --wait 3000' ] || exit 93
exec "$XTASK" ci-status --wait 0
"#,
    );
    for (scenario, remote, error) in [
        ("pending", "old", "NotPushed"),
        ("missing", "head", "NoRunForHead"),
        ("pending", "head", "StillRunning"),
        ("failed", "head", "Failed"),
    ] {
        for gate in [false, true] {
            let mut command = if gate {
                let mut command = Command::new("sh");
                command.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ci-gate.sh"));
                observation_environment(&mut command, dir.path(), scenario);
                command.env("XTASK", env!("CARGO_BIN_EXE_xtask"));
                command
            } else {
                let mut command = observation_command(dir.path(), scenario);
                command.args(["ci-status", "--wait", "0"]);
                command
            };
            let output = command.env("REMOTE", remote).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{scenario}, gate={gate}");
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["error"], error);
        }
    }
}

fn ios_verify_fixture() -> tempfile::TempDir {
    let dir = commands();
    std::fs::create_dir_all(dir.path().join("ios/Journeys")).unwrap();
    std::fs::write(
        dir.path().join("ios/Journeys/manifest.json"),
        include_str!("../../../ios/Journeys/manifest.json"),
    )
    .unwrap();
    let recipes = [
        "fmt-check",
        "lint",
        "test",
        "spec",
        "mobile-check",
        "ios-lint",
        "ios-rust",
        "ios-simulator",
        "ios-build",
        "ios-loopback-smoke",
        "ios-unit",
        "ios-door-smoke",
        "ios-goldens",
        "ios-journey",
        "ios-accessibility",
        "ios-perf",
        "ios-scope-audit",
    ];
    let config: String = recipes
        .iter()
        .map(|name| format!("[task.{name}]\nrun='true'\n"))
        .collect();
    std::fs::write(dir.path().join(".wt.toml"), config).unwrap();
    executable(
        &dir.path().join("wt"),
        r#"#!/bin/sh
echo "$*" >> calls
[ "$2" != "$FAIL_RECIPE" ] || exit 1
if [ "$2" = ios-journey ]; then
    for id in home-coldstart home conversation asks review writing claude-sessions hosts-lifecycle hosts accounts production-startup reports accessibility; do
        [ "$id" = "$SKIP_JOURNEY" ] || echo "$id: passed"
    done
fi
"#,
    );
    executable(
        &dir.path().join("python3"),
        r#"#!/bin/sh
[ "$*" = '-B scripts/ios-perf.py --machine' ] || exit 92
[ "$MACHINE_ERROR" != 1 ] || { echo 'unknown performance machine' >&2; exit 1; }
printf '%s' "$PERF_MACHINE"
"#,
    );
    dir
}

fn ios_verify_command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .arg("ios-verify")
        .current_dir(dir)
        .env(
            "PATH",
            format!("{}:{}", dir.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "PERF_MACHINE",
            r#"{"name":"pinned-mac","hard":true,"baseline":"unused","baseline_present":false}"#,
        );
    command
}

#[test]
fn ios_verify_cli_runs_full_checks_bare_and_stops_on_failure_or_skipped_journey() {
    let dir = ios_verify_fixture();
    for (fail, skip, success, last) in [
        ("", "", true, "ios-scope-audit"),
        ("fmt-check", "", false, "fmt-check"),
        ("mobile-check", "", false, "mobile-check"),
        ("ios-accessibility", "", false, "ios-accessibility"),
        ("", "claude-sessions", false, "ios-journey"),
        ("", "accounts", false, "ios-journey"),
        ("", "production-startup", false, "ios-journey"),
    ] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let output = ios_verify_command(dir.path())
            .env("FAIL_RECIPE", fail)
            .env("SKIP_JOURNEY", skip)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(calls.ends_with(&format!("run {last}\n")), "{calls}");
        assert!(
            !calls.contains("--"),
            "verification must not filter, update or record: {calls}"
        );
        if success {
            assert_eq!(
                calls,
                "run fmt-check\nrun lint\nrun test\nrun spec\nrun mobile-check\nrun ios-lint\nrun ios-rust\nrun ios-simulator\nrun ios-build\nrun ios-loopback-smoke\nrun ios-unit\nrun ios-door-smoke\nrun ios-goldens\nrun ios-journey\nrun ios-accessibility\nrun ios-perf\nrun ios-scope-audit\n"
            );
        }
        if !skip.is_empty() {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains(&format!("{skip} did not report a full pass"))
            );
        }
    }
}

#[test]
fn ios_verify_cli_reports_missing_runner_baseline_and_fails_machine_errors() {
    let dir = ios_verify_fixture();
    for present in [false, true] {
        std::fs::write(dir.path().join("calls"), "").unwrap();
        let machine = serde_json::json!({"name":"macos-26", "hard":false,
            "baseline":"ios/Perf/baselines/macos-26.json", "baseline_present":present});
        let output = ios_verify_command(dir.path())
            .env("PERF_MACHINE", machine.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(calls.contains("run ios-perf\n"), present);
        assert!(!calls.contains("--baseline"));
        if !present {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("no baseline for this runner")
            );
            let verdict: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dir.path().join("target/ios/perf/verdict.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(verdict["status"], "not_measured");
            assert_eq!(verdict["reason"], "no baseline for this runner");
        }
    }
    let output = ios_verify_command(dir.path())
        .env("MACHINE_ERROR", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown performance machine"));
}
