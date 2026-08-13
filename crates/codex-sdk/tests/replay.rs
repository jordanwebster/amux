use std::path::PathBuf;
use std::time::Duration;

use codex_sdk::{Codex, ListThreadsParams, ThreadConfig, TurnEvent};
use replay_support::{ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller};

async fn replay(name: &str) -> (Codex, tokio::task::JoinHandle<()>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
        .join("io.jsonl");
    let (reader, writer, controller) =
        replay_transport_with_controller(load_script(path), ReplayOptions::default());
    let driver = tokio::spawn(async move {
        while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
            controller.advance_one().await
        {
            tokio::task::yield_now().await;
        }
    });
    let codex = tokio::time::timeout(Duration::from_secs(2), Codex::from_io(reader, writer))
        .await
        .expect("initialize replay timed out")
        .expect("initialize replay failed");
    (codex, driver)
}

#[tokio::test]
async fn initialize_smoke() {
    let (codex, driver) = replay("initialize").await;
    let init = codex.initialization_result().expect("initialize result");
    assert_eq!(init.codex_home, PathBuf::from("/Users/test/.codex"));
    driver.await.expect("replay driver");
}

#[tokio::test]
async fn thread_list_uses_data_envelope() {
    let (codex, driver) = replay("thread_list").await;
    let response = codex
        .list_threads(ListThreadsParams::default())
        .await
        .expect("thread/list");
    assert_eq!(response.data.len(), 1);
    assert_eq!(response.data[0].id, "thread-1");
    driver.await.expect("replay driver");
}

#[tokio::test]
async fn parses_added_thread_notifications_and_emitted_timestamp() {
    let (codex, driver) = replay("turn_notifications").await;
    let thread = codex
        .start_thread(ThreadConfig::default())
        .await
        .expect("thread/start");
    let mut turn = thread.turn("hello").await.expect("turn/start");

    assert!(matches!(
        turn.next().await,
        Ok(Some(TurnEvent::FileChangePatchUpdated { ref item_id, ref changes }))
            if item_id == "item-1" && changes.len() == 1
    ));
    assert!(matches!(
        turn.next().await,
        Ok(Some(TurnEvent::ModelRerouted { ref to_model, .. })) if to_model == "model-b"
    ));
    assert!(matches!(
        turn.next().await,
        Ok(Some(TurnEvent::ThreadCompacted { ref turn_id })) if turn_id == "turn-1"
    ));
    assert!(matches!(
        turn.next().await,
        Ok(Some(TurnEvent::Warning { ref message })) if message == "careful"
    ));
    assert!(matches!(
        turn.next().await,
        Ok(Some(TurnEvent::TurnCompleted { .. }))
    ));
    driver.await.expect("replay driver");
}
