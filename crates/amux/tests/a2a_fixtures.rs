use serde_json::Value;

const SOCKET_DELIVERY: &str = include_str!("fixtures/a2a/socket_delivery.jsonl");
const PTY_DELIVERY: &str = include_str!("fixtures/a2a/pty_delivery.jsonl");
const STOP_PAYLOAD: &str = include_str!("fixtures/a2a/stop_payload.jsonl");
const MCP_TOOLS: &str = include_str!("fixtures/a2a/mcp_tools.jsonl");
const SESSION_REGISTRY: &str = include_str!("fixtures/a2a/session_registry.jsonl");
const SESSION_REGISTRY_META: &str = include_str!("fixtures/a2a/session_registry.meta.json");
const CODEX_MCP_SUBSTRATE: &str = include_str!("fixtures/rows/codex/mcp_substrate.jsonl");
const CODEX_MCP_SUBSTRATE_META: &str = include_str!("fixtures/rows/codex/mcp_substrate.meta.json");

fn codex_mcp_substrate() -> Vec<Value> {
    CODEX_MCP_SUBSTRATE
        .lines()
        .map(|line| serde_json::from_str(line).expect("Codex MCP substrate row is JSON"))
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
fn a2a_fixture_codex_mcp_substrate_replays_offline() {
    let meta: Value =
        serde_json::from_str(CODEX_MCP_SUBSTRATE_META).expect("Codex MCP metadata is JSON");
    let recorded_sha = meta
        .pointer("/derived/sha256")
        .and_then(Value::as_str)
        .expect("Codex MCP metadata records the derived fixture digest");
    assert_eq!(
        amux_artifacts::id_of(CODEX_MCP_SUBSTRATE.as_bytes()).as_str(),
        format!("sha256:{recorded_sha}")
    );

    let rows = codex_mcp_substrate();
    let capture = rows
        .iter()
        .find(|row| row.get("type").and_then(Value::as_str) == Some("capture"))
        .expect("capture provenance row");
    assert_eq!(
        capture.get("codex_version").and_then(Value::as_str),
        Some("codex-cli 0.149.0")
    );
    assert_eq!(
        capture.get("codex_source_commit").and_then(Value::as_str),
        Some("aec653daa9873bf44517a623fd033722737817a8")
    );
    assert_eq!(capture.get("isolated_codex_home"), Some(&Value::Bool(true)));
    assert_eq!(capture.get("model_turns"), Some(&Value::from(0)));

    let start_at = rows
        .iter()
        .position(|line| {
            line.get("type").and_then(Value::as_str) == Some("request")
                && line.get("method").and_then(Value::as_str) == Some("thread/start")
        })
        .expect("captured thread/start request");
    let start_config = rows[start_at]
        .pointer("/config/mcp_servers/amux")
        .expect("thread/start carries the amux MCP config");
    assert_eq!(
        start_config.get("command").and_then(Value::as_str),
        Some("<ABSOLUTE_STUB_PYTHON>")
    );
    assert_eq!(
        start_config
            .pointer("/env/AMUX_CONFIG")
            .and_then(Value::as_str),
        Some("/absolute/capture/amux.yaml")
    );
    assert_eq!(start_config.get("enabled"), Some(&Value::Bool(true)));
    assert_eq!(start_config.get("required"), Some(&Value::Bool(true)));
    assert_eq!(
        start_config
            .get("default_tools_approval_mode")
            .and_then(Value::as_str),
        Some("approve")
    );
    assert_eq!(
        start_config.get("startup_timeout_sec"),
        Some(&Value::from(10))
    );
    assert_eq!(start_config.get("tool_timeout_sec"), Some(&Value::from(60)));
    assert_eq!(
        start_config.get("enabled_tools"),
        Some(&serde_json::json!([
            "agents", "send", "spawn", "stop", "status"
        ]))
    );

    let start_exec_at = rows
        .iter()
        .position(|row| {
            row.get("type").and_then(Value::as_str) == Some("stub_exec")
                && row.get("request").and_then(Value::as_str) == Some("thread/start")
        })
        .expect("captured absolute MCP child execution");
    assert_eq!(
        rows[start_exec_at]
            .pointer("/env/AMUX_CONFIG")
            .and_then(Value::as_str),
        Some("/absolute/capture/amux.yaml")
    );
    let start_ready_at = rows
        .iter()
        .position(|row| {
            row.get("type").and_then(Value::as_str) == Some("startup_status")
                && row.get("request").and_then(Value::as_str) == Some("thread/start")
        })
        .expect("captured start status sequence");
    assert_eq!(
        rows[start_ready_at].get("statuses"),
        Some(&serde_json::json!(["starting", "ready"]))
    );
    let inventory = rows
        .iter()
        .find(|row| row.get("type").and_then(Value::as_str) == Some("tool_inventory"))
        .expect("captured filtered tool inventory");
    assert_eq!(
        inventory.get("stub_tools"),
        Some(&serde_json::json!([
            "agents", "send", "spawn", "stop", "status", "extra"
        ]))
    );
    assert_eq!(
        inventory.get("exposed_tools"),
        start_config.get("enabled_tools")
    );
    assert!(start_at < start_exec_at && start_exec_at < start_ready_at);

    let resume_at = rows
        .iter()
        .position(|row| {
            row.get("type").and_then(Value::as_str) == Some("request")
                && row.get("method").and_then(Value::as_str) == Some("thread/resume")
        })
        .expect("captured cold thread/resume request");
    assert_eq!(
        rows[resume_at].pointer("/config/mcp_servers/amux"),
        Some(start_config)
    );
    let resume_ready_at = rows
        .iter()
        .position(|row| {
            row.get("type").and_then(Value::as_str) == Some("startup_status")
                && row.get("request").and_then(Value::as_str) == Some("thread/resume")
        })
        .expect("captured resume status sequence");
    assert_eq!(
        rows[resume_ready_at].get("statuses"),
        Some(&serde_json::json!(["starting", "ready"]))
    );
    assert!(start_ready_at < resume_at && resume_at < resume_ready_at);

    let failure = rows
        .iter()
        .find(|row| row.get("type").and_then(Value::as_str) == Some("required_failure"))
        .expect("captured required-server startup failure");
    assert_eq!(
        failure
            .pointer("/config/mcp_servers/amux/command")
            .and_then(Value::as_str),
        Some("/definitely/missing/amux-capture")
    );
    assert!(
        failure
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("required MCP servers failed to initialize: amux"))
    );
}

fn captured_a2a_io(name: &str) -> Vec<Value> {
    let capture = match name {
        "inject_idle" => include_str!("fixtures/rows/codex/a2a_inject_idle.io.jsonl"),
        "inject_busy" => include_str!("fixtures/rows/codex/a2a_inject_busy.io.jsonl"),
        "last_message" => include_str!("fixtures/rows/codex/a2a_last_message.io.jsonl"),
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

#[test]
fn a2a_fixture_pty_delivery() {
    let rows: Vec<Value> = PTY_DELIVERY
        .lines()
        .map(|line| serde_json::from_str(line).expect("PTY capture row is JSON"))
        .collect();
    let idle = "<amux from=\"probe/host\" id=\"idle\">A2A_PTY_IDLE_21240</amux>";
    assert!(rows.iter().any(|row| {
        row.get("type").and_then(Value::as_str) == Some("user")
            && row.pointer("/message/content").and_then(Value::as_str) == Some(idle)
            && row.get("promptSource").and_then(Value::as_str) == Some("typed")
    }));
    let busy_queue = rows.iter().position(|row| {
        row.get("type").and_then(Value::as_str) == Some("queue-operation")
            && row.get("operation").and_then(Value::as_str) == Some("enqueue")
            && row
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.contains("A2A_PTY_BUSY_21240"))
    });
    let busy_attachment = rows.iter().position(|row| {
        row.pointer("/attachment/type").and_then(Value::as_str) == Some("queued_command")
            && row
                .pointer("/attachment/prompt")
                .and_then(Value::as_str)
                .is_some_and(|prompt| prompt.contains("A2A_PTY_BUSY_21240"))
    });
    assert!(busy_queue.is_some());
    assert!(busy_attachment.is_some());
    assert!(busy_queue < busy_attachment);
}

#[test]
fn a2a_fixture_stop_payload() {
    let rows: Vec<Value> = STOP_PAYLOAD
        .lines()
        .map(|line| serde_json::from_str(line).expect("Stop capture row is JSON"))
        .collect();
    let stop = rows.iter().position(|row| {
        row.get("type").and_then(Value::as_str) == Some("hook.stop")
            && row.get("last_assistant_message").and_then(Value::as_str)
                == Some("STOP_PAYLOAD_21240")
    });
    assert!(stop.is_some());
}

#[test]
fn a2a_fixture_mcp_tools() {
    let rows: Vec<Value> = MCP_TOOLS
        .lines()
        .map(|line| serde_json::from_str(line).expect("MCP capture row is JSON"))
        .collect();
    let tool_use = rows.iter().position(|row| {
        row.get("type").and_then(Value::as_str) == Some("assistant")
            && row
                .pointer("/message/content/0/type")
                .and_then(Value::as_str)
                == Some("tool_use")
            && row
                .pointer("/message/content/0/name")
                .and_then(Value::as_str)
                == Some("mcp__amux__send")
            && row
                .pointer("/message/content/0/input/to")
                .and_then(Value::as_str)
                == Some("probe")
            && row
                .pointer("/message/content/0/input/text")
                .and_then(Value::as_str)
                == Some("A2A_MCP_SENT_21240")
    });
    let tool_result = rows.iter().position(|row| {
        row.get("type").and_then(Value::as_str) == Some("user")
            && row
                .pointer("/message/content/0/type")
                .and_then(Value::as_str)
                == Some("tool_result")
            && row
                .pointer("/message/content/0/tool_use_id")
                .and_then(Value::as_str)
                == Some("toolu_a2a_mcp_send")
    });
    assert!(tool_use.is_some(), "fixture has mcp__amux__send tool_use");
    assert!(tool_result.is_some(), "fixture has paired tool_result");
    assert!(tool_use < tool_result);
    assert!(
        rows.iter()
            .all(|row| row.get("type").and_then(Value::as_str) != Some("hook.permission_request"))
    );
}

fn parse_claude_version(version: &str) -> (u16, u16, u16) {
    let mut parts = version
        .split_whitespace()
        .next()
        .expect("Claude version starts with a version number")
        .split('.');
    let parse = |part: Option<&str>| {
        part.expect("version component")
            .parse::<u16>()
            .expect("numeric version")
    };
    (
        parse(parts.next()),
        parse(parts.next()),
        parse(parts.next()),
    )
}

#[test]
fn a2a_version_gate_parser() {
    let meta: Value = serde_json::from_str(SESSION_REGISTRY_META).expect("registry meta is JSON");
    let version = meta
        .get("claude_version")
        .and_then(Value::as_str)
        .expect("registry meta records claude --version stdout");
    assert!(parse_claude_version(version) >= (2, 1, 224));
    assert_eq!(
        meta.pointer("/notes/assertions/terminal_name")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        meta.pointer("/notes/registry/hook_transcript_path")
            .and_then(Value::as_str)
            .is_some()
    );
    assert!(meta.pointer("/notes/registry/peerProtocol").is_some());

    let rows: Vec<Value> = SESSION_REGISTRY
        .lines()
        .map(|line| serde_json::from_str(line).expect("registry capture row is JSON"))
        .collect();
    assert!(rows.iter().any(|row| {
        row.get("type").and_then(Value::as_str) == Some("hook.stop")
            && row.get("last_assistant_message").and_then(Value::as_str)
                == Some("A2A_REGISTRY_READY_21240")
    }));
}
