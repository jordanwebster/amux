use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

use crate::{IoEvent, Observed};

/// Additive protocol changes seen by a live probe.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriftReport {
    pub new_frames: BTreeSet<String>,
    pub new_fields: BTreeSet<String>,
    pub new_discriminants: BTreeSet<String>,
    pub raw_payloads: usize,
}

/// Observe the structural vocabulary carried by raw JSON frames.
pub fn observe(frames: &[IoEvent]) -> Observed {
    let mut observed = Observed::default();
    for event in frames {
        let Ok(frame) = serde_json::from_str::<Value>(&event.line) else {
            continue;
        };
        if let Some(frame_type) = frame.get("type").and_then(Value::as_str) {
            observed.frames.insert(frame_type.to_string());
        }
        walk(&frame, None, &mut observed);
    }
    observed
}

/// Return only vocabulary present in `live` and absent from `recorded`.
pub fn drift(recorded: &Observed, live: &Observed, raw_payloads: usize) -> DriftReport {
    DriftReport {
        new_frames: difference(&live.frames, &recorded.frames),
        new_fields: difference(&live.fields, &recorded.fields),
        new_discriminants: difference(&live.discriminants, &recorded.discriminants),
        raw_payloads,
    }
}

fn difference(live: &BTreeSet<String>, recorded: &BTreeSet<String>) -> BTreeSet<String> {
    live.difference(recorded).cloned().collect()
}

fn walk(value: &Value, path: Option<&str>, observed: &mut Observed) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let child = match path {
                    Some(path) => format!("{path}.{key}"),
                    None => key.clone(),
                };
                observed.fields.insert(child.clone());
                if matches!(key.as_str(), "type" | "subtype" | "kind" | "event")
                    && let Some(discriminant) = value.as_str()
                {
                    observed.discriminants.insert(discriminant.to_string());
                }
                walk(value, Some(&child), observed);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                let child = match path {
                    Some(path) => format!("{path}.{index}"),
                    None => index.to_string(),
                };
                observed.fields.insert(child.clone());
                walk(value, Some(&child), observed);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IoDirection, IoEvent};

    fn event(line: Value) -> IoEvent {
        IoEvent {
            us: 0,
            direction: IoDirection::Read,
            line: serde_json::to_string(&line).unwrap(),
            transport_id: None,
            session_id: None,
        }
    }

    #[test]
    fn observe_walks_nested_frames_and_discriminants() {
        let observed = observe(&[event(serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "hello"},
                    {"kind": "tool", "event": "started"}
                ]
            }
        }))]);

        assert!(observed.frames.contains("assistant"));
        assert!(observed.fields.contains("message.content.0.type"));
        assert!(observed.fields.contains("message.content.1.event"));
        assert!(observed.discriminants.contains("assistant"));
        assert!(observed.discriminants.contains("text"));
        assert!(observed.discriminants.contains("tool"));
        assert!(observed.discriminants.contains("started"));
    }

    #[test]
    fn drift_is_empty_for_identical_observations_and_additive_for_new_values() {
        let recorded = observe(&[event(serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text"}]}
        }))]);
        assert_eq!(drift(&recorded, &recorded, 0), DriftReport::default());

        let live = observe(&[
            event(serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "image", "source": "raw"}]}
            })),
            event(serde_json::json!({"type": "rate_limit", "kind": "warning"})),
        ]);
        let report = drift(&recorded, &live, 2);

        assert_eq!(
            report.new_frames,
            BTreeSet::from(["rate_limit".to_string()])
        );
        assert!(report.new_fields.contains("message.content.0.source"));
        assert!(report.new_discriminants.contains("image"));
        assert!(report.new_discriminants.contains("warning"));
        assert_eq!(report.raw_payloads, 2);
    }
}
