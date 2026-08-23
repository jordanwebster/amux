use serde_json::Value;

const SOCKET_DELIVERY: &str = include_str!("fixtures/a2a/socket_delivery.jsonl");

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
    assert_eq!(
        rpc[call_at]
            .pointer("/params/arguments/to")
            .and_then(Value::as_str),
        Some("probe")
    );
    assert_eq!(
        rpc[call_at]
            .pointer("/params/arguments/text")
            .and_then(Value::as_str),
        Some("C11_SENT")
    );
    let response = rpc
        .iter()
        .skip(call_at + 1)
        .find(|line| line.get("id") == Some(&Value::from(0)))
        .expect("captured response to dynamic tool call");
    assert_eq!(
        response.pointer("/result/success").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        response
            .pointer("/result/contentItems/0/type")
            .and_then(Value::as_str),
        Some("inputText")
    );
    assert!(rpc.iter().skip(call_at + 1).any(|line| {
        line.get("method").and_then(Value::as_str) == Some("turn/completed")
            && line.pointer("/params/turn/status").and_then(Value::as_str) == Some("completed")
    }));
}

fn captured_a2a_io(name: &str) -> Vec<Value> {
    let capture = match name {
        "inject_idle" => include_str!("fixtures/codex_backend/a2a_inject_idle.io.jsonl"),
        "inject_busy" => include_str!("fixtures/codex_backend/a2a_inject_busy.io.jsonl"),
        "last_message" => include_str!("fixtures/codex_backend/a2a_last_message.io.jsonl"),
        _ => panic!("unknown A2A Codex fixture {name}"),
    };
    capture
        .lines()
        .map(|line| serde_json::from_str(line).expect("capture IO line is JSON"))
        .collect()
}

fn rpc_messages(name: &str) -> Vec<Value> {
    captured_a2a_io(name).iter().map(rpc_line).collect()
}

fn position(messages: &[Value], predicate: impl Fn(&Value) -> bool) -> usize {
    messages
        .iter()
        .position(predicate)
        .expect("captured structural event")
}

#[test]
fn a2a_fixture_codex_inject_idle() {
    let messages = rpc_messages("inject_idle");
    let injected = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("thread/inject_items")
            && message
                .pointer("/params/items/0/type")
                .and_then(Value::as_str)
                == Some("message")
            && message
                .pointer("/params/items/0/role")
                .and_then(Value::as_str)
                == Some("user")
            && message
                .pointer("/params/items/0/content/0/type")
                .and_then(Value::as_str)
                == Some("input_text")
    });
    let empty_turn = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("turn/start")
            && message.pointer("/params/input").and_then(Value::as_array) == Some(&Vec::new())
    });
    let answer = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("item/completed")
            && message.pointer("/params/item/type").and_then(Value::as_str) == Some("agentMessage")
            && message
                .pointer("/params/item/text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("C12_INJECT_IDLE"))
    });
    let completed = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("turn/completed")
            && message
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                == Some("completed")
    });
    assert!(injected < empty_turn && empty_turn < answer && answer < completed);
}

#[test]
fn a2a_fixture_codex_inject_busy() {
    let messages = rpc_messages("inject_busy");
    let started = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("turn/started")
    });
    let injected = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("thread/inject_items")
            && message
                .pointer("/params/items/0/role")
                .and_then(Value::as_str)
                == Some("user")
    });
    let initial = position(&messages, |message| {
        message.pointer("/params/item/text").and_then(Value::as_str) == Some("C13_INITIAL")
    });
    let queued = position(&messages, |message| {
        message.pointer("/params/item/text").and_then(Value::as_str) == Some("C13_INJECT_BUSY")
    });
    let completed = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("turn/completed")
            && message
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                == Some("completed")
    });
    assert!(started < injected && injected < initial && initial < queued && queued < completed);
}

#[test]
fn a2a_fixture_codex_last_message() {
    let messages = rpc_messages("last_message");
    let first = position(&messages, |message| {
        message.pointer("/params/item/text").and_then(Value::as_str) == Some("C14_FIRST")
            && message
                .pointer("/params/item/phase")
                .and_then(Value::as_str)
                == Some("commentary")
    });
    let last = position(&messages, |message| {
        message.pointer("/params/item/text").and_then(Value::as_str) == Some("C14_SECOND")
            && message
                .pointer("/params/item/phase")
                .and_then(Value::as_str)
                == Some("final_answer")
    });
    let completed = position(&messages, |message| {
        message.get("method").and_then(Value::as_str) == Some("turn/completed")
            && message
                .pointer("/params/turn/items/0/type")
                .and_then(Value::as_str)
                == Some("agentMessage")
            && message
                .pointer("/params/turn/items/0/text")
                .and_then(Value::as_str)
                == Some("C14_SECOND")
    });
    assert!(first < last && last < completed);
}

#[test]
fn a2a_fixture_socket_delivery() {
    let rows: Vec<Value> = SOCKET_DELIVERY
        .lines()
        .map(|line| serde_json::from_str(line).expect("socket capture row is JSON"))
        .collect();
    for marker in ["A2A_SOCKET_IDLE_21240", "A2A_SOCKET_BUSY_21240"] {
        let queued = rows.iter().position(|row| {
            row.get("type").and_then(Value::as_str) == Some("queue-operation")
                && row.get("operation").and_then(Value::as_str) == Some("enqueue")
                && row
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| {
                        content.contains(marker) && content.starts_with("<cross-session-message ")
                    })
        });
        let native = rows.iter().position(|row| {
            row.get("type").and_then(Value::as_str) == Some("user")
                && row.pointer("/origin/kind").and_then(Value::as_str) == Some("peer")
                && row.pointer("/origin/name").and_then(Value::as_str) == Some("probe")
                && row.pointer("/origin/fromMode").and_then(Value::as_str) == Some("prompting")
                && row
                    .pointer("/message/content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| content.contains(marker))
        });
        assert!(queued.is_some(), "missing enqueue row for {marker}");
        assert!(native.is_some(), "missing native peer row for {marker}");
        assert!(
            queued < native,
            "socket queue must precede its native row for {marker}"
        );
    }
}
