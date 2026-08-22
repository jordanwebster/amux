use serde_json::Value;

fn captured_io() -> Vec<Value> {
    include_str!("fixtures/codex_backend/a2a_tools.io.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("capture IO line is JSON"))
        .collect()
}

fn rpc_line(row: &Value) -> Value {
    serde_json::from_str(
        row.get("line")
            .and_then(Value::as_str)
            .expect("capture IO row has JSON-RPC line"),
    )
    .expect("captured JSON-RPC line is JSON")
}

#[test]
fn a2a_fixture_codex_tools() {
    let rows = captured_io();
    let rpc: Vec<_> = rows.iter().map(rpc_line).collect();
    let start = rpc
        .iter()
        .find(|line| line.get("method").and_then(Value::as_str) == Some("thread/start"))
        .expect("captured thread/start");
    let tools = start
        .pointer("/params/dynamicTools")
        .and_then(Value::as_array)
        .expect("thread/start carries dynamicTools");
    assert!(tools.iter().any(|tool| {
        tool.get("name").and_then(Value::as_str) == Some("send")
            && tool.pointer("/inputSchema/type").and_then(Value::as_str) == Some("object")
    }));

    let call_at = rpc
        .iter()
        .position(|line| {
            line.get("method").and_then(Value::as_str) == Some("item/tool/call")
                && line.pointer("/params/tool").and_then(Value::as_str) == Some("send")
        })
        .expect("captured dynamic send tool call");
    assert_eq!(rpc[call_at].pointer("/params/arguments/to").and_then(Value::as_str), Some("probe"));
    assert_eq!(rpc[call_at].pointer("/params/arguments/text").and_then(Value::as_str), Some("C11_SENT"));
    let response = rpc
        .iter()
        .skip(call_at + 1)
        .find(|line| line.get("id") == Some(&Value::from(0)))
        .expect("captured response to dynamic tool call");
    assert_eq!(response.pointer("/result/success").and_then(Value::as_bool), Some(true));
    assert_eq!(
        response.pointer("/result/contentItems/0/type").and_then(Value::as_str),
        Some("inputText")
    );
    assert!(rpc.iter().skip(call_at + 1).any(|line| {
        line.get("method").and_then(Value::as_str) == Some("turn/completed")
            && line.pointer("/params/turn/status").and_then(Value::as_str) == Some("completed")
    }));
}
