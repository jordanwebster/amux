#[path = "capture/graduate.rs"]
mod graduate;
#[allow(dead_code)]
#[path = "capture/harness.rs"]
mod harness;
#[path = "capture/redact.rs"]
mod redact;
#[path = "capture/structure.rs"]
mod structure;
#[allow(dead_code)]
#[path = "capture/taxonomy.rs"]
mod taxonomy;

use std::fs;

fn fixture_rows(name: &str) -> Vec<serde_json::Value> {
    let raw = match name {
        "phase7-fixes2-plan-auto-blocked" => {
            include_str!("fixtures/capture-waiter/phase7-fixes2-plan-auto-blocked.rows.jsonl")
        }
        "phase7-fixes2-plan-reject-blocked" => {
            include_str!("fixtures/capture-waiter/phase7-fixes2-plan-reject-blocked.rows.jsonl")
        }
        "phase7-plan-auto-lifecycle" => {
            include_str!("fixtures/capture-waiter/phase7-plan-auto-lifecycle.rows.jsonl")
        }
        "phase7-plan-reject-lifecycle" => {
            include_str!("fixtures/capture-waiter/phase7-plan-reject-lifecycle.rows.jsonl")
        }
        _ => panic!("unknown waiter fixture {name}"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Offline equivalent of the live plan-resolution waiter: the first row at or
/// after `from_index` with the requested plan resolution shape.
fn find_plan_resolution(
    rows: &[serde_json::Value],
    from_index: usize,
    outcome: structure::PlanOutcome,
) -> Option<(usize, &str)> {
    rows.iter()
        .enumerate()
        .skip(from_index)
        .find_map(|(index, row)| {
            let (id, observed) = structure::plan_resolution(row)?;
            (observed == outcome).then_some((index + 1, id))
        })
}

#[test]
fn tool_use_id_is_independent_of_json_key_order() {
    let id_then_name: serde_json::Value = serde_json::from_str(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","input":{"plan":"x"},"name":"ExitPlanMode"}]}}"#,
    )
    .unwrap();
    let name_then_id: serde_json::Value = serde_json::from_str(
        r#"{"message":{"content":[{"name":"ExitPlanMode","id":"toolu_2","type":"tool_use"}]},"type":"assistant"}"#,
    )
    .unwrap();
    assert_eq!(
        structure::tool_use_id(&id_then_name, "ExitPlanMode"),
        Some("toolu_1")
    );
    assert_eq!(
        structure::tool_use_id(&name_then_id, "ExitPlanMode"),
        Some("toolu_2")
    );
}

#[test]
fn exit_plan_hook_is_ready_even_when_the_assistant_row_is_withheld() {
    for fixture in [
        "phase7-fixes2-plan-auto-blocked",
        "phase7-fixes2-plan-reject-blocked",
    ] {
        let rows = fixture_rows(fixture);
        assert!(rows.iter().any(structure::is_exit_plan_request));
        assert!(
            rows.iter()
                .all(|row| structure::tool_use_id(row, "ExitPlanMode").is_none()),
            "{fixture} unexpectedly contains a direct ExitPlanMode assistant row"
        );
    }
}

#[test]
fn offline_waiter_finds_captured_exit_plan_approval() {
    let rows = fixture_rows("phase7-plan-auto-lifecycle");
    let request = rows
        .iter()
        .position(structure::is_exit_plan_request)
        .expect("captured ExitPlanMode hook")
        + 1;
    let (after_result, id) = find_plan_resolution(&rows, request, structure::PlanOutcome::Approved)
        .expect("captured typed approval");
    assert_eq!(id, "toolu_plan_auto");
    assert!(after_result > request);
    assert_eq!(
        rows.iter()
            .find_map(|row| structure::tool_use_id(row, "ExitPlanMode")),
        Some(id)
    );
}

#[test]
fn offline_waiter_finds_captured_exit_plan_rejection_facts() {
    let rows = fixture_rows("phase7-plan-reject-lifecycle");
    let request = rows
        .iter()
        .position(structure::is_exit_plan_request)
        .expect("captured ExitPlanMode hook")
        + 1;
    let (after_result, id) = find_plan_resolution(&rows, request, structure::PlanOutcome::Rejected)
        .expect("captured typed rejection with denial kind and feedback");
    assert_eq!(id, "toolu_plan_reject");
    assert_eq!(
        rows.iter()
            .find_map(|row| structure::tool_use_id(row, "ExitPlanMode")),
        Some(id)
    );
    assert!(rows.iter().skip(after_result).any(|row| {
        row.get("type").and_then(serde_json::Value::as_str) == Some("hook.permission_request")
            && row.get("tool_name").and_then(serde_json::Value::as_str) == Some("AskUserQuestion")
    }));
}

#[test]
fn relative_capture_paths_resolve_from_the_workspace() {
    assert_eq!(
        structure::workspace_path("target/capture/run"),
        structure::workspace_root().join("target/capture/run")
    );
    let absolute = std::env::temp_dir().join("amux-absolute-capture");
    assert_eq!(structure::workspace_path(&absolute), absolute);
}

#[test]
fn markdown_inventory_routes_row_and_nested_taxonomies() {
    let markdown = include_str!("../../../notes/chat-v1/transcript-semantics.md");
    let categories = taxonomy::markdown_categories(markdown);
    for expected in [
        "row/assistant",
        "row/permission-mode",
        "system/turn_duration",
        "attachment/command_permissions",
        "block/tool_use",
        "row/hook.permission_request",
        "row/amux.transcript_ready",
    ] {
        assert!(categories.contains(expected), "missing {expected}");
    }
}

#[test]
fn drift_is_a_structural_diff_not_a_version_gate() {
    let mut baseline = taxonomy::Snapshot::default();
    baseline
        .add_jsonl(
            r#"{"type":"assistant","version":"2.1.228","message":{"content":[{"type":"text","text":"x"}]}}"#,
        )
        .unwrap();
    let mut run = taxonomy::Snapshot::default();
    run.add_jsonl(
        r#"{"type":"assistant","version":"9.0.0","newField":true,"message":{"content":[{"type":"new_block","data":1}]}}"#,
    )
    .unwrap();
    let report = taxonomy::diff("", &baseline, &run);
    assert_eq!(report.baseline_versions, ["2.1.228"]);
    assert_eq!(report.run_versions, ["9.0.0"]);
    assert!(report.additions.contains(&"block/new_block".to_string()));
    assert!(report.removals.contains(&"block/text".to_string()));
    assert!(report.shape_changes.iter().any(|change| {
        change.category == "row/assistant"
            && change
                .added_paths
                .iter()
                .any(|path| path == "newField:bool")
    }));
    assert!(report.render().contains("DATA; never a version gate"));
}

#[test]
fn redaction_and_graduation_fail_loud_then_copy_only_verified_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    let run = temp.path().join("run");
    let fixtures = temp.path().join("fixtures");
    fs::create_dir_all(&scratch).unwrap();
    fs::create_dir_all(&run).unwrap();
    let raw = format!(
        "{{\"type\":\"user\",\"cwd\":{:?},\"message\":{{\"content\":\"safe\"}}}}\n",
        scratch.display().to_string()
    );
    let redacted = redact::redact(&raw, &scratch).unwrap();
    assert!(redacted.contains("[SCRATCH]"));
    fs::write(run.join("sample.redacted.jsonl"), redacted).unwrap();
    fs::write(
        run.join("sample.meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "scenario": "sample",
            "captured_at": "2026-08-12T00:00:00Z",
            "claude_version": "2.1.228",
            "model": "haiku",
            "harness": "test",
            "failure": null
        }))
        .unwrap(),
    )
    .unwrap();
    graduate::graduate(&run, &fixtures, &["sample".to_string()]).unwrap();
    assert!(fixtures.join("sample.rows.jsonl").exists());
    assert!(fixtures.join("sample.meta.json").exists());

    fs::write(run.join("sample.redacted.jsonl"), "Bearer secret\n").unwrap();
    let error = graduate::verify_run(&run, &["sample".to_string()]).unwrap_err();
    assert!(error.to_string().contains("verify"));
}

#[test]
fn every_capture_waiter_fixture_passes_redaction_verification() {
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capture-waiter");
    let mut verified = 0;
    for entry in fs::read_dir(&fixtures).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let candidate = fs::read_to_string(&path).unwrap();
        redact::verify_candidate(&candidate).unwrap_or_else(|error| {
            panic!("{} failed redaction verify: {error:#}", path.display())
        });
        verified += 1;
    }
    assert!(verified > 0, "no capture-waiter fixtures were verified");
}

#[tokio::test]
async fn canceled_scenario_leaves_no_live_recorder_tasks() {
    let state = std::sync::Arc::new(harness::RecorderState::default());
    let raw_live = state.enter();
    let raw = tokio::spawn(async move {
        let _live = raw_live;
        std::future::pending::<()>().await;
    });
    let rows_live = state.enter();
    let rows = tokio::spawn(async move {
        let _live = rows_live;
        std::future::pending::<()>().await;
    });
    let recorders = harness::RecorderTasks::new(raw, rows);
    assert_eq!(state.live_tasks(), 2);

    let scenario = tokio::spawn(async move {
        let _recorders = recorders;
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    scenario.abort();
    let _ = scenario.await;
    state.wait_stopped().await.unwrap();

    assert_eq!(state.live_tasks(), 0);
}
