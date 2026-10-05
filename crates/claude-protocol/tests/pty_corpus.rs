//! Every transcript row and hook payload in every terminal Claude recording
//! decodes with nothing left unknown and encodes back to the same JSON. A
//! failure here is provider drift, or a type that does not match what Claude
//! writes.

use std::path::{Path, PathBuf};

use claude_protocol::{hooks, transcript};
use serde_json::Value;

/// A row the recorder's transcript tailer adds when it has read the file
/// to its end; amux's, not Claude's.
const TAILER_MARKER: &str = "amux.transcript_ready";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-specs/fixtures/pty")
}

enum Line {
    Row(String),
    Hook(String),
}

/// Each recorded row and payload with where it came from.
fn lines() -> Vec<(String, Line)> {
    let mut recordings: Vec<_> = std::fs::read_dir(root())
        .expect("fixtures/pty")
        .map(|entry| entry.expect("entry").path().join("io.jsonl"))
        .filter(|path| path.is_file())
        .collect();
    recordings.sort();
    assert!(
        recordings.len() >= 31,
        "found only {} recordings",
        recordings.len()
    );
    let mut lines = Vec::new();
    for path in recordings {
        let name = format!(
            "fixtures/pty/{}/io.jsonl",
            path.parent()
                .and_then(Path::file_name)
                .expect("a recording folder")
                .to_string_lossy()
        );
        let text = std::fs::read_to_string(&path).expect("recording");
        for (number, record) in text.lines().enumerate() {
            let place = format!("{name}:{}", number + 1);
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let transport = record["transport_id"].as_str().expect("transport_id");
            if transport.starts_with("hook") {
                lines.push((place, Line::Hook(line)));
            } else if transport.starts_with("transcript") {
                let wrapped: Value = serde_json::from_str(&line).expect("transcript line is JSON");
                let row = &wrapped["row"];
                if row["type"] == TAILER_MARKER {
                    continue;
                }
                lines.push((place, Line::Row(row.to_string())));
            }
        }
    }
    let rows = lines
        .iter()
        .filter(|(_, line)| matches!(line, Line::Row(_)))
        .count();
    assert!(rows >= 900, "found only {rows} transcript rows");
    assert!(
        lines.len() - rows >= 140,
        "found only {} hook payloads",
        lines.len() - rows
    );
    lines
}

#[test]
fn every_recorded_row_and_payload_is_known() {
    let mut failures = Vec::new();
    let all = lines();
    for (place, line) in &all {
        let drift = match line {
            Line::Row(row) => transcript::strict(row.as_bytes()).err(),
            Line::Hook(payload) => hooks::strict(payload.as_bytes()).err(),
        };
        if let Some(drift) = drift {
            failures.push(format!("{place}: {drift}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} lines are not known:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

#[test]
fn every_recorded_row_and_payload_encodes_back_to_the_same_json() {
    let mut failures = Vec::new();
    for (place, line) in lines() {
        let (written, encoded) = match &line {
            Line::Row(row) => (
                row,
                transcript::encode(&transcript::decode(row.as_bytes()).expect("decodes")),
            ),
            Line::Hook(payload) => (
                payload,
                hooks::encode(&hooks::decode(payload.as_bytes()).expect("decodes")),
            ),
        };
        let written: Value = serde_json::from_str(written).expect("line is JSON");
        let again: Value = serde_json::from_slice(&encoded).expect("encoded is JSON");
        if written != again {
            let mut differences = Vec::new();
            differ("", &written, &again, &mut differences);
            failures.push(format!("{place}: {}", differences.join(", ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} lines changed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn an_invented_row_or_payload_is_unknown_and_fails_strict() {
    let row = br#"{"type":"teleport","destination":"mars","sessionId":"s"}"#;
    match transcript::decode(row).expect("an object decodes") {
        transcript::Row::Unknown(unknown) => {
            assert_eq!(unknown.field("destination").unwrap(), "mars")
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = transcript::strict(row).expect_err("strict refuses it");
    assert!(drift.to_string().contains("teleport"), "{drift}");

    let payload =
        br#"{"hook_event_name":"Teleport","session_id":"s","transcript_path":"/t","cwd":"/"}"#;
    let decoded = hooks::decode(payload).expect("an object decodes");
    assert_eq!(decoded.name(), "Teleport");
    assert!(decoded.common().is_none());
    let drift = hooks::strict(payload).expect_err("strict refuses it");
    assert!(drift.to_string().contains("Teleport"), "{drift}");
}

/// Where two JSON values differ, as paths with what each side holds.
fn differ(path: &str, written: &Value, encoded: &Value, out: &mut Vec<String>) {
    match (written, encoded) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys().filter(|key| !a.contains_key(*key))) {
                let inner = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => differ(&inner, x, y, out),
                    (x, y) => out.push(format!("{inner} written {x:?} encoded {y:?}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differ(&format!("{path}[{index}]"), x, y, out);
            }
        }
        (a, b) if a != b => out.push(format!("{path} written {a} encoded {b}")),
        _ => {}
    }
}
