#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use codex_sdk::{CodexConfig, DaemonMode, ListThreadsParams, connect_daemon, ensure_daemon};

#[tokio::test]
async fn real_daemon_initialize_and_thread_list() {
    if std::env::var_os("CODEX_SDK_LIVE").as_deref() != Some(std::ffi::OsStr::new("1")) {
        eprintln!("skipped: set CODEX_SDK_LIVE=1 to run the real daemon probe");
        return;
    }

    tokio::time::timeout(Duration::from_secs(30), async {
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .expect("CODEX_HOME or HOME is required");
        let mode = ensure_daemon(&codex_home).await.expect("ensure daemon");
        let mode_name = match &mode {
            DaemonMode::Existing => "Existing",
            DaemonMode::Spawned(_) => "Spawned",
            DaemonMode::Private(_) => "Private",
            DaemonMode::PrivateExisting(_) => "PrivateExisting",
        };
        eprintln!("ensure_daemon -> {mode_name}");

        let codex = connect_daemon(&codex_home, CodexConfig::default())
            .await
            .expect("connect daemon");
        eprintln!(
            "initialize -> {}",
            serde_json::to_string(codex.initialization_result().expect("initialize result"))
                .unwrap()
        );

        let threads = codex
            .list_threads(ListThreadsParams {
                limit: Some(1),
                ..Default::default()
            })
            .await
            .expect("thread/list");
        let first = threads.data.first().map(|thread| {
            serde_json::json!({
                "id": thread.id,
                "cliVersion": thread.cli_version,
                "source": thread.source,
            })
        });
        eprintln!(
            "thread/list -> {}",
            serde_json::json!({
                "dataCount": threads.data.len(),
                "first": first,
                "nextCursor": threads.next_cursor,
            })
        );
        if let Ok(expected_id) = std::env::var("CODEX_SDK_EXPECT_THREAD_ID") {
            let thread = codex
                .read_thread(&expected_id, false)
                .await
                .unwrap_or_else(|error| panic!("thread/read failed for `{expected_id}`: {error}"));
            if let Ok(expected_name) = std::env::var("CODEX_SDK_EXPECT_THREAD_NAME") {
                assert_eq!(thread.thread.name.as_deref(), Some(expected_name.as_str()));
            }
            eprintln!(
                "thread/read named match -> {}",
                serde_json::json!({"id": thread.thread.id, "name": thread.thread.name})
            );
        }

        codex.close().await;
        match mode {
            DaemonMode::Spawned(process) | DaemonMode::Private(process) => process.shutdown().await,
            DaemonMode::Existing => {}
            DaemonMode::PrivateExisting(_) => {
                unreachable!("ensure_daemon uses the well-known socket")
            }
        }
    })
    .await
    .expect("live daemon probe timed out");
}
