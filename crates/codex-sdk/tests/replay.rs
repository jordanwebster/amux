use std::path::PathBuf;
use std::time::Duration;

use codex_sdk::{
    Codex, CodexConfig, DynamicToolCallResponse, FunctionDynamicToolSpec, ListThreadsParams,
    ThreadConfig, TurnEvent,
};
use replay_support::{
    IoDirection, ReplayAdvance, ReplayOptions, load_script, replay_transport_with_controller,
};

fn amux_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amux/tests/fixtures/codex_backend")
        .join(name)
}

fn sdk_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
        .join("io.jsonl")
}

fn sdk_compatible_script(path: PathBuf) -> Vec<replay_support::IoEvent> {
    let mut script = load_script(path);
    let initialize = script
        .iter_mut()
        .find(|event| event.direction == IoDirection::Write)
        .expect("fixture has initialize request");
    let mut value: serde_json::Value =
        serde_json::from_str(&initialize.line).expect("initialize request is JSON");
    value["params"]["clientInfo"]["title"] = serde_json::Value::Null;
    initialize.line = value.to_string();
    script
}

async fn replay_script(
    script: Vec<replay_support::IoEvent>,
    config: CodexConfig,
) -> (Codex, tokio::task::JoinHandle<()>) {
    let (reader, writer, controller) =
        replay_transport_with_controller(script, ReplayOptions::default());
    let driver = tokio::spawn(async move {
        while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
            controller.advance_one().await
        {
            tokio::task::yield_now().await;
        }
    });
    let codex = tokio::time::timeout(
        Duration::from_secs(2),
        Codex::from_io(reader, writer, config),
    )
    .await
    .expect("initialize replay timed out")
    .expect("initialize replay failed");
    (codex, driver)
}

async fn replay(name: &str) -> (Codex, tokio::task::JoinHandle<()>) {
    replay_script(load_script(sdk_fixture(name)), CodexConfig::default()).await
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
    let mut events = thread.events().await.expect("thread events");
    thread.start_turn("hello").await.expect("turn/start");

    assert!(matches!(
        events.next().await.unwrap().map(|event| event.event),
        Some(TurnEvent::FileChangePatchUpdated { ref item_id, ref changes })
            if item_id == "item-1" && changes.len() == 1
    ));
    assert!(matches!(
        events.next().await.unwrap().map(|event| event.event),
        Some(TurnEvent::ModelRerouted { ref to_model, .. }) if to_model == "model-b"
    ));
    assert!(matches!(
        events.next().await.unwrap().map(|event| event.event),
        Some(TurnEvent::ThreadCompacted { ref turn_id }) if turn_id == "turn-1"
    ));
    assert!(matches!(
        events.next().await.unwrap().map(|event| event.event),
        Some(TurnEvent::Warning { ref message }) if message == "careful"
    ));
    assert!(matches!(
        events.next().await.unwrap().map(|event| event.event),
        Some(TurnEvent::TurnCompleted { .. })
    ));
    driver.await.expect("replay driver");
}

#[tokio::test]
async fn a2a_dynamic_tools_replay_matches_graduated_capture() {
    let script = sdk_compatible_script(sdk_fixture("a2a_dynamic_tools"));
    let (codex, driver) = replay_script(script, CodexConfig::default()).await;
    let thread = codex
        .start_thread(ThreadConfig {
            model: Some("gpt-5.6-sol".into()),
            cwd: Some("[SCRATCH]/project".into()),
            dynamic_tools: Some(vec![FunctionDynamicToolSpec {
                name: "send".into(),
                description: "Send a short message to another agent.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "to": {"type": "string"},
                        "text": {"type": "string"}
                    },
                    "required": ["to", "text"]
                }),
                defer_loading: None,
            }]),
            ..Default::default()
        })
        .await
        .expect("thread/start");
    let mut events = thread.events().await.expect("thread events");
    thread
        .start_turn(
            "Call the send tool exactly once with to=probe and text=C11_SENT. Do not use any other tool.",
        )
        .await
        .expect("turn/start");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.next().await.expect("thread event").expect("event");
            match event.event {
                TurnEvent::ToolCallRequired(request) => {
                    assert_eq!(request.tool, "send");
                    assert_eq!(
                        request.arguments,
                        serde_json::json!({"to": "probe", "text": "C11_SENT"})
                    );
                    thread
                        .respond_tool_call(
                            request.request_id,
                            DynamicToolCallResponse {
                                content_items: vec![serde_json::json!({
                                    "type": "inputText",
                                    "text": "sent"
                                })],
                                success: true,
                            },
                        )
                        .await
                        .expect("tool response");
                }
                TurnEvent::TurnCompleted { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("dynamic tool replay timed out");
    driver.await.expect("replay driver");
}

#[tokio::test]
async fn a2a_inject_items_then_start_empty_turn_replays_idle_capture() {
    let script = sdk_compatible_script(amux_fixture("a2a_inject_idle.io.jsonl"));
    let config = CodexConfig {
        client_name: "amux-a2a-capture".into(),
        ..Default::default()
    };
    let (codex, driver) = replay_script(script, config).await;
    let thread = codex
        .start_thread(ThreadConfig {
            model: Some("gpt-5.6-sol".into()),
            cwd: Some("[SCRATCH]/project".into()),
            ..Default::default()
        })
        .await
        .expect("thread/start");
    let mut events = thread.events().await.expect("thread events");
    thread
        .inject_items(vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "C12_INJECT_IDLE"}]
        })])
        .await
        .expect("thread/inject_items");
    thread.start_empty_turn().await.expect("empty turn/start");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.next().await.expect("thread event").expect("event");
            if matches!(event.event, TurnEvent::TurnCompleted { .. }) {
                break;
            }
        }
    })
    .await
    .expect("injected turn replay timed out");
    driver.await.expect("replay driver");
}
