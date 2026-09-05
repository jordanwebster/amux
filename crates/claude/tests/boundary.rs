use std::fs;
use std::path::Path;

#[test]
fn claude_sdk_stream_start_preserves_absent_null_and_populated_stop_fields() {
    use serde_json::{Value, json};

    let opening = include_str!("../fixtures/sdk/streamed_turn/io.jsonl")
        .lines()
        .find_map(|line| {
            let entry: Value = serde_json::from_str(line).unwrap();
            let row: Value = serde_json::from_str(entry["line"].as_str().unwrap()).unwrap();
            (row["type"] == "stream_event" && row["event"]["type"] == "message_start")
                .then_some(row)
        })
        .expect("recorded opening stream event");

    for reason in [None, Some(Value::Null), Some(json!("end_turn"))] {
        for sequence in [None, Some(Value::Null), Some(json!("DONE"))] {
            let mut row = opening.clone();
            let message = row["event"]["message"].as_object_mut().unwrap();
            for (key, value) in [("stop_reason", reason.clone()), ("stop_sequence", sequence)] {
                message.remove(key);
                if let Some(value) = value {
                    message.insert(key.to_string(), value);
                }
            }
            let parsed = claude::sdk::Message::parse(row.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), row);
        }
    }
}

#[test]
fn sdk_boundary_is_an_event_stream_and_control_handle_without_callback_objects() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sdk");
    for module in [
        "abort.rs",
        "control.rs",
        "dispatch.rs",
        "error.rs",
        "init.rs",
        "mcp.rs",
        "message.rs",
        "options.rs",
        "query.rs",
        "session.rs",
        "types.rs",
    ] {
        assert!(root.join(module).is_file(), "missing SDK module {module}");
    }

    let mut public_dyn = Vec::new();
    let mut source = String::new();
    for entry in fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            let declaration = line.trim_start();
            if (declaration.starts_with("pub ") || declaration.starts_with("pub("))
                && declaration.contains("dyn ")
            {
                public_dyn.push(format!("{}:{}:{declaration}", path.display(), index + 1));
            }
        }
        source.push_str(&text);
    }
    assert!(
        public_dyn.is_empty(),
        "public SDK items contain trait objects:\n{}",
        public_dyn.join("\n")
    );

    for callback_api in [
        "pub trait CanUseTool",
        "pub trait HookCallback",
        "pub trait OnElicitation",
        "pub trait OnUserDialog",
    ] {
        assert!(
            !source.contains(callback_api),
            "callback API leaked through `{callback_api}`"
        );
    }

    let session = fs::read_to_string(root.join("session.rs")).unwrap();
    assert!(session.contains("pub struct Session"));
    assert!(session.contains("pub events: EventStream"));
    assert!(session.contains("pub control: Control"));
    for event in [
        "PermissionRequest",
        "HookCallback",
        "Elicitation",
        "UserDialog",
    ] {
        assert!(session.contains(event), "missing SDK event {event}");
    }
    for answer in [
        "answer_permission",
        "answer_hook",
        "answer_elicitation",
        "answer_user_dialog",
    ] {
        assert!(session.contains(answer), "missing control method {answer}");
    }

    let options = fs::read_to_string(root.join("options.rs")).unwrap();
    assert!(options.contains("pub hook_subscriptions: Vec<HookSubscription>"));
    for callback_field in [
        "pub can_use_tool:",
        "pub on_elicitation:",
        "pub on_user_dialog:",
        "pub hooks:",
    ] {
        assert!(
            !options.contains(callback_field),
            "QueryOptions leaked callback field `{callback_field}`"
        );
    }
}
