#![cfg(all(unix, testnet))]

use amux::codex_io::CodexSdkV1Input;
use amux::derived_rows_test_support::CodexBackendHarness;
use codex::{Codex, CodexConfig, ThreadConfig};
use replay_support::{ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller};
use serde_json::json;

#[tokio::test]
async fn provider_commands_backend_discovers_and_delivers_reported_skill_over_recording() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-commands");
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
    let control = session.control.clone();
    assert_eq!(control.session_facts()["commands"], json!([]));
    control.discover_commands().await.unwrap();
    assert_eq!(
        control.session_facts()["commands"],
        json!([
            {"name":"review", "source":"codex", "terminal_only":false}
        ])
    );
    let mut backend = CodexBackendHarness::with_session(session).await.unwrap();
    for name in ["invented", "disabled", "ambiguous", "elsewhere", "exit"] {
        backend
            .send(
                b"refused",
                CodexSdkV1Input::Command {
                    name: name.into(),
                    args: String::new(),
                },
            )
            .await
            .unwrap();
        let row = backend.wait_for_type("amux.input_result").await.unwrap();
        assert!(row.get("error").is_some(), "{row}");
    }
    backend
        .send(
            b"command",
            CodexSdkV1Input::Command {
                name: "review".into(),
                args: " check the changes\nwith tests".into(),
            },
        )
        .await
        .unwrap();
    backend.wait_for_type("turn/completed").await.unwrap();
    assert!(control.discover_commands().await.is_err());
    assert_eq!(
        control.session_facts()["commands"],
        json!([]),
        "failed discovery clears stale commands"
    );
    assert!(
        control
            .command("review".into(), String::new())
            .await
            .is_err()
    );
    let rows = backend.finish().await.unwrap();
    assert!(
        rows.iter()
            .any(|row| row["type"] == "amux.input_result" && row.get("ok").is_some())
    );
    replay.await.unwrap();
    assert!(controller.finish().unwrap().is_complete());
    let rows = rows
        .into_iter()
        .filter(|row| row["type"] == "amux.codex_ready")
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    if std::env::var_os("UPDATE_PROVIDER_COMMANDS").is_some() {
        std::fs::write(root.join("rows.jsonl"), &rows).unwrap();
    }
    assert_eq!(
        rows,
        std::fs::read_to_string(root.join("rows.jsonl")).unwrap()
    );
    println!(
        "Strict app-server replay consumed typed skill review at its reported host path and exact multiline arguments. Disabled, ambiguous, out-of-directory, unreported and stale commands refused.\n{rows}"
    );
    client.close().await;
}
