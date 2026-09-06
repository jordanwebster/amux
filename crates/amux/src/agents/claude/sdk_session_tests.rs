use std::time::Duration;

use claude::sdk::PermissionMode;
use tokio::io::{BufReader, duplex};

use super::tests::{read_json_line, record, write_json_line};
use super::*;

fn breakdown() -> Value {
    json!({"categories": [{"name": "messages", "tokens": 42, "color": "blue"}],
        "totalTokens": 42, "maxTokens": 200000, "rawMaxTokens": 200000,
        "percentage": 0.021, "gridRows": [], "model": "launch", "memoryFiles": [],
        "mcpTools": [], "agents": [], "isAutoCompactEnabled": true, "apiUsage": null,
        "future": {"retained": true}})
}

async fn acknowledge(stdout: &mut tokio::io::DuplexStream, request: &Value, response: Value) {
    write_json_line(
        stdout,
        json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request["request_id"], "response": response
        }}),
    )
    .await;
}

#[tokio::test]
async fn claude_sdk_controls_publish_facts_and_only_explicit_context_requests() {
    let id = Uuid::nil();
    let (sdk_stdin, server_stdin) = duplex(32768);
    let (mut stdout, sdk_stdout) = duplex(32768);
    let (quiet_tx, quiet_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut stdin = BufReader::new(server_stdin);
        let init = read_json_line(&mut stdin).await;
        acknowledge(
            &mut stdout,
            &init,
            json!({"commands": [{"name": "compact", "description": "Compact context", "argumentHint": "[instructions]"}], "agents": [], "models": [],
            "account": {}, "output_style": "default", "available_output_styles": []}),
        )
        .await;
        let mode = read_json_line(&mut stdin).await;
        assert_eq!(
            mode["request"],
            json!({"subtype": "set_permission_mode", "mode": "acceptEdits"})
        );
        acknowledge(&mut stdout, &mode, json!({"mode": "plan"})).await;
        let model = read_json_line(&mut stdin).await;
        assert_eq!(
            model["request"],
            json!({"subtype": "set_model", "model": "changed"})
        );
        acknowledge(&mut stdout, &model, json!({})).await;
        let refused = read_json_line(&mut stdin).await;
        assert_eq!(refused["request"]["model"], "refused");
        write_json_line(&mut stdout, json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": refused["request_id"], "error": "model unavailable"
        }})).await;
        let reset = read_json_line(&mut stdin).await;
        assert_eq!(reset["request"], json!({"subtype": "set_model"}));
        acknowledge(&mut stdout, &reset, json!({})).await;
        for effort in [json!("high"), json!("max"), json!(null)] {
            let request = read_json_line(&mut stdin).await;
            assert_eq!(
                request["request"],
                json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": effort}})
            );
            if effort == "max" {
                write_json_line(&mut stdout, json!({"type": "control_response", "response": {
                    "subtype": "error", "request_id": request["request_id"], "error": "effort unavailable"
                }})).await;
            } else {
                acknowledge(&mut stdout, &request, json!({})).await;
            }
        }
        let prompt = read_json_line(&mut stdin).await;
        assert_eq!(prompt["type"], "user");
        assert_eq!(prompt["message"]["content"], "/compact keep the decisions");
        write_json_line(&mut stdout, json!({"type": "assistant", "uuid": id, "session_id": id.to_string(),
            "parent_tool_use_id": null, "message": {"id": "msg", "type": "message", "role": "assistant",
            "model": "launch", "content": [{"type": "text", "text": "done"}],
            "usage": {"input_tokens": 10, "output_tokens": 500, "cache_read_input_tokens": 20,
                "cache_creation_input_tokens": 30}}})).await;
        write_json_line(&mut stdout, json!({"type": "result", "subtype": "success", "uuid": id,
            "session_id": id.to_string(), "duration_ms": 1, "duration_api_ms": 1, "is_error": false,
            "num_turns": 1, "stop_reason": "end_turn", "total_cost_usd": 0.0, "result": "done",
            "usage": {"input_tokens": 9000, "output_tokens": 1000}, "permission_denials": [],
            "modelUsage": {"launch": {"inputTokens": 9000, "outputTokens": 1000, "cacheReadInputTokens": 0,
                "cacheCreationInputTokens": 0, "webSearchRequests": 0, "costUSD": 0.0,
                "contextWindow": 200000, "maxOutputTokens": 10000}}})).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(80), read_json_line(&mut stdin))
                .await
                .is_err(),
            "daemon must send no control after result until the next input"
        );
        quiet_tx.send(()).unwrap();
        let request = read_json_line(&mut stdin).await;
        assert_eq!(request["request"], json!({"subtype": "get_context_usage"}));
        acknowledge(&mut stdout, &request, breakdown()).await;
        let interrupt = read_json_line(&mut stdin).await;
        assert_eq!(
            interrupt["request"]["subtype"], "interrupt",
            "breakdown must request usage exactly once"
        );
        acknowledge(&mut stdout, &interrupt, json!({})).await;
    });
    let session = claude::sdk::from_io(
        BufReader::new(sdk_stdout),
        sdk_stdin,
        QueryOptions {
            session_id: Some(id.to_string()),
            ..QueryOptions::default()
        },
    )
    .await
    .unwrap();
    let mut rec = record(id);
    rec.args = vec!["--model".into(), "launch".into()];
    let mut backend = ClaudeSdkBackend::with_session(rec, session);
    let Plane::Structured { log, input } = backend.plane(Protocol::ClaudeSdkV1).unwrap() else {
        panic!()
    };
    let mut reader = log.subscribe().await.unwrap();
    let (events, _rx) = mpsc::channel(8);
    let ingest = backend.start(&events).unwrap();
    let mut rows = Vec::new();
    for _ in 0..2 {
        rows.push(reader.read().await.unwrap().payload);
    }
    assert_eq!(
        rows[1],
        json!({"type": "amux.claude_sdk.session_facts", "model": "launch",
        "effort": null, "slash_commands": ["compact"], "terminal_slash_commands": [], "permission_mode": "default", "context": null, "mcp_servers": []})
    );
    for (name, command, expected_model, expected_mode, ok) in [
        (
            "bypass",
            ClaudeSdkV1Input::SetPermissionMode {
                mode: PermissionMode::BypassPermissions,
            },
            "launch",
            "default",
            false,
        ),
        (
            "mode",
            ClaudeSdkV1Input::SetPermissionMode {
                mode: PermissionMode::AcceptEdits,
            },
            "launch",
            "plan",
            true,
        ),
        (
            "model",
            ClaudeSdkV1Input::SetModel {
                model: Some("changed".into()),
            },
            "changed",
            "plan",
            true,
        ),
        (
            "refused",
            ClaudeSdkV1Input::SetModel {
                model: Some("refused".into()),
            },
            "changed",
            "plan",
            false,
        ),
        (
            "reset",
            ClaudeSdkV1Input::SetModel { model: None },
            "launch",
            "plan",
            true,
        ),
    ] {
        input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: name.as_bytes().to_vec(),
                input: command,
            })
            .await
            .unwrap();
        if ok {
            let facts = reader.read().await.unwrap().payload;
            assert_eq!(facts["type"], "amux.claude_sdk.session_facts");
            assert_eq!(facts["model"], expected_model);
            assert_eq!(facts["permission_mode"], expected_mode);
            rows.push(facts);
        }
        let result = reader.read().await.unwrap().payload;
        assert_eq!(result["type"], "amux.claude_sdk.input_result");
        assert_eq!(result["outcome"] == "ok", ok);
        assert_eq!(
            backend.runtime.lock().unwrap().facts.model.as_deref(),
            Some(expected_model)
        );
        rows.push(result);
    }
    for (effort, expected, ok) in [
        (Some(claude::sdk::Effort::High), Some("high"), true),
        (Some(claude::sdk::Effort::Max), Some("high"), false),
        (None, None, true),
    ] {
        input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: b"effort".to_vec(),
                input: ClaudeSdkV1Input::SetEffort { effort },
            })
            .await
            .unwrap();
        if ok {
            let facts = reader.read().await.unwrap().payload;
            assert_eq!(facts["type"], "amux.claude_sdk.session_facts");
            assert_eq!(facts["effort"], json!(expected));
            assert_eq!(facts["slash_commands"], json!(["compact"]));
            rows.push(facts);
        }
        let result = reader.read().await.unwrap().payload;
        assert_eq!(result["type"], "amux.claude_sdk.input_result");
        assert_eq!(result["outcome"] == "ok", ok);
        assert_eq!(
            backend.runtime.lock().unwrap().facts.effort.as_deref(),
            expected
        );
        rows.push(result);
    }
    input
        .send(StructuredInputEvent::ClaudeSdk {
            input_id: b"prompt".to_vec(),
            input: ClaudeSdkV1Input::Prompt {
                text: format!(
                    "/{} keep the decisions",
                    rows[1]["slash_commands"][0].as_str().unwrap()
                ),
                image_blocks: vec![],
            },
        })
        .await
        .unwrap();
    quiet_rx.await.unwrap();
    loop {
        let row = reader.read().await.unwrap().payload;
        let final_facts = row["context"]["source"] == "result_usage";
        rows.push(row);
        if final_facts {
            break;
        }
    }
    assert_eq!(
        rows.last().unwrap()["context"],
        json!({"used_tokens": 60, "window_tokens": 200000, "source": "result_usage"})
    );
    for (name, command) in [
        ("context", ClaudeSdkV1Input::RequestContextBreakdown),
        ("interrupt", ClaudeSdkV1Input::Interrupt),
    ] {
        input
            .send(StructuredInputEvent::ClaudeSdk {
                input_id: name.as_bytes().to_vec(),
                input: command,
            })
            .await
            .unwrap();
    }
    server.await.unwrap();
    ingest.await.unwrap();
    while let Some(row) = reader.read().await {
        rows.push(row.payload);
    }
    let context_rows = rows
        .iter()
        .filter(|row| row["type"] == "amux.claude_sdk.context_breakdown")
        .collect::<Vec<_>>();
    assert_eq!(context_rows.len(), 1);
    assert_eq!(context_rows[0]["usage"], breakdown());
    for pair in rows.windows(2) {
        if pair[0]["type"] == "assistant" || pair[0]["type"] == "result" {
            assert_eq!(pair[1]["type"], "amux.claude_sdk.session_facts");
        }
    }
    if let Some(directory) = std::env::var_os("CLAUDE_SDK_FACTS_EVIDENCE") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let capture = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        std::fs::write(
            directory.join("session-controls-and-context.rows.jsonl"),
            capture,
        )
        .unwrap();
    }
}
