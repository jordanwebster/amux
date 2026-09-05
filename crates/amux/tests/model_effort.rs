#![cfg(all(unix, testnet))]

use amux::codex_io::{ApprovalPolicy, CodexSdkV1Input, SandboxPolicy};
use amux::derived_rows_test_support::CodexBackendHarness;
use codex::{Codex, CodexConfig, ThreadConfig};
use replay_support::{ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller};
use serde_json::json;

#[tokio::test]
async fn model_effort_backend_delivers_recorded_settings_on_prompt_and_empty_turn() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model-effort");
    let (reader, writer, controller) = replay_transport_with_controller(
        load_script(root.join("io.jsonl")),
        ReplayOptions::default(),
    );
    let driver = controller.clone();
    let replay = tokio::spawn(async move {
        while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
            driver.advance_one().await
        {
            tokio::task::yield_now().await;
        }
    });
    let client = Codex::from_io(
        reader,
        writer,
        CodexConfig {
            client_name: "amux-codex-spec".into(),
            ..CodexConfig::default()
        },
    )
    .await
    .unwrap();
    let session = codex::open(client.start_thread(ThreadConfig::default()).await.unwrap())
        .await
        .unwrap();
    session.control.discover_models().await.unwrap();
    let control = session.control.clone();
    let mut backend = CodexBackendHarness::with_session(session).await.unwrap();
    for (id, input) in [
        (
            "model",
            CodexSdkV1Input::SetModel {
                model: "model-b".into(),
            },
        ),
        (
            "effort",
            CodexSdkV1Input::SetEffort {
                effort: "high".into(),
            },
        ),
        (
            "preset",
            CodexSdkV1Input::SetPreset {
                approval: ApprovalPolicy::OnRequest,
                sandbox: SandboxPolicy::ReadOnly,
            },
        ),
    ] {
        backend.send(id.as_bytes(), input).await.unwrap();
        let settings = backend.wait_for_type("amux.codex_settings").await.unwrap();
        assert_eq!(settings["session"]["model"], "model-b");
        let result = backend.wait_for_type("amux.input_result").await.unwrap();
        assert!(result.get("ok").is_some(), "{result}");
    }
    let before = control.session_facts();
    for input in [
        CodexSdkV1Input::SetModel {
            model: "unreported-model".into(),
        },
        CodexSdkV1Input::SetEffort {
            effort: "low".into(),
        },
    ] {
        backend.send(b"invalid", input).await.unwrap();
        let result = backend.wait_for_type("amux.input_result").await.unwrap();
        assert!(result.get("error").is_some(), "{result}");
        assert_eq!(
            control.session_facts(),
            before,
            "refused changes leave selection intact"
        );
    }
    backend
        .send(
            b"prompt",
            CodexSdkV1Input::UserTurn {
                input: serde_json::to_vec(&json!([{"type":"text","text":"use selected settings"}]))
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    backend.wait_for_type("turn/completed").await.unwrap();
    control.empty_turn().await.unwrap();
    backend.wait_for_type("turn/completed").await.unwrap();
    let retained_settings = control.session_settings();
    let rows = backend.finish().await.unwrap();
    let resumed = client
        .resume_thread("settings-thread", ThreadConfig::default())
        .await
        .unwrap();
    let resumed = codex::session::open_with_settings(resumed, retained_settings)
        .await
        .unwrap();
    resumed.control.discover_models().await.unwrap();
    assert_eq!(
        resumed.control.session_facts(),
        before,
        "reattachment preserves host selections even when thread metadata has older defaults"
    );
    let resumed_control = resumed.control.clone();
    let mut backend = CodexBackendHarness::with_session(resumed).await.unwrap();
    resumed_control.empty_turn().await.unwrap();
    backend.wait_for_type("turn/completed").await.unwrap();
    backend.finish().await.unwrap();
    replay.await.unwrap();
    assert!(controller.finish().unwrap().is_complete());
    // Input results and settings may race with event ingestion, but their per-input
    // order is causal. Store the model facts alone for differential UI replay.
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            matches!(
                row["type"].as_str(),
                Some("amux.codex_ready" | "amux.codex_settings")
            )
        })
        .collect();
    let actual = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    if std::env::var_os("UPDATE_MODEL_EFFORT").is_some() {
        std::fs::write(root.join("rows.jsonl"), &actual).unwrap();
    }
    assert_eq!(
        actual,
        std::fs::read_to_string(root.join("rows.jsonl")).unwrap()
    );
    println!(
        "Strict app-server replay: model-b / high / on-request / readOnly delivered on prompt and empty turns, including after reattachment.\n{actual}"
    );
    client.close().await;
}
