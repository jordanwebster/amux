#![cfg(unix)]

use codex_sdk::{CodexConfig, ListThreadsParams, connect_socket};
use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn websocket_over_uds_uses_one_json_message_per_text_frame() {
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();

        let initialize = websocket.next().await.unwrap().unwrap();
        let Message::Text(initialize) = initialize else {
            panic!("initialize was not a text frame");
        };
        let initialize: serde_json::Value = serde_json::from_str(&initialize).unwrap();
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["clientInfo"]["name"], "amux-test");
        assert_eq!(initialize["params"]["clientInfo"]["version"], "9.8.7");
        assert_eq!(
            initialize["params"]["capabilities"]["optOutNotificationMethods"],
            serde_json::json!(["thread/status/changed"])
        );
        websocket
            .send(Message::Text(
                serde_json::json!({
                    "id": initialize["id"],
                    "result": {
                        "userAgent": "test/0.147.0",
                        "codexHome": "/Users/test/.codex",
                        "platformFamily": "unix",
                        "platformOs": "macos"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let initialized = websocket.next().await.unwrap().unwrap();
        let Message::Text(initialized) = initialized else {
            panic!("initialized was not a text frame");
        };
        let initialized: serde_json::Value = serde_json::from_str(&initialized).unwrap();
        assert_eq!(initialized["method"], "initialized");

        let list = websocket.next().await.unwrap().unwrap();
        let Message::Text(list) = list else {
            panic!("thread/list was not a text frame");
        };
        let list: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert_eq!(list["method"], "thread/list");
        websocket
            .send(Message::Text(
                serde_json::json!({
                    "id": list["id"],
                    "result": {"data": [], "nextCursor": null}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    });

    let config = CodexConfig {
        client_name: "amux-test".into(),
        client_version: "9.8.7".into(),
        opt_out_notification_methods: vec!["thread/status/changed".into()],
        ..CodexConfig::default()
    };
    let codex = connect_socket(&socket_path, config).await.unwrap();
    assert_eq!(
        codex.initialization_result().unwrap().user_agent,
        "test/0.147.0"
    );
    let threads = codex
        .list_threads(ListThreadsParams::default())
        .await
        .unwrap();
    assert!(threads.data.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn large_outbound_frame_survives_interleaved_inbound_frames() {
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let payload = "x".repeat(512 * 1024);
    let expected_payload = payload.clone();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let Message::Text(initialize) = websocket.next().await.unwrap().unwrap() else {
            panic!("initialize was not a text frame");
        };
        let initialize: serde_json::Value = serde_json::from_str(&initialize).unwrap();
        websocket
            .send(Message::Text(
                serde_json::json!({
                    "id": initialize["id"],
                    "result": {
                        "userAgent": "test/0.147.0",
                        "codexHome": "/Users/test/.codex",
                        "platformFamily": "unix",
                        "platformOs": "macos"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let Message::Text(initialized) = websocket.next().await.unwrap().unwrap() else {
            panic!("initialized was not a text frame");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&initialized).unwrap()["method"],
            "initialized"
        );

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        for index in 0..32 {
            websocket
                .send(Message::Text(
                    serde_json::json!({
                        "method": "warning",
                        "params": {"message": format!("interleaved-{index}")}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }

        let Message::Text(rename) = websocket.next().await.unwrap().unwrap() else {
            panic!("thread/name/set was not a text frame");
        };
        let rename: serde_json::Value = serde_json::from_str(&rename).unwrap();
        assert_eq!(rename["method"], "thread/name/set");
        assert_eq!(rename["params"]["name"], expected_payload);
        websocket
            .send(Message::Text(
                serde_json::json!({"id": rename["id"], "result": {}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });

    let codex = connect_socket(&socket_path, CodexConfig::default())
        .await
        .unwrap();
    codex.rename_thread("thread-1", &payload).await.unwrap();
    codex.close().await;
    server.await.unwrap();
}
