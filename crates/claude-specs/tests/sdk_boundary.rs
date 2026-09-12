#[test]
fn stream_start_preserves_absent_null_and_populated_stop_fields() {
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
