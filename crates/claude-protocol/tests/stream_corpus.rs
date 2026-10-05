//! Every line of every headless Claude recording, in both directions,
//! decodes with nothing left unknown and encodes back to the same JSON. A
//! failure here is provider drift, or a type that does not match what Claude
//! writes.

use std::path::{Path, PathBuf};

use claude_protocol::stream::{self, Input, Output};
use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-specs/fixtures/sdk")
}

/// Each recorded line with where it came from and which way it went.
fn lines() -> Vec<(String, bool, String)> {
    let mut recordings: Vec<_> = std::fs::read_dir(root())
        .expect("fixtures/sdk")
        .map(|entry| entry.expect("entry").path().join("io.jsonl"))
        .filter(|path| path.is_file())
        .collect();
    recordings.sort();
    assert!(
        recordings.len() >= 41,
        "found only {} recordings",
        recordings.len()
    );
    let mut lines = Vec::new();
    for path in recordings {
        let name = format!(
            "fixtures/sdk/{}/io.jsonl",
            path.parent()
                .and_then(Path::file_name)
                .expect("a recording folder")
                .to_string_lossy()
        );
        let text = std::fs::read_to_string(&path).expect("recording");
        for (number, record) in text.lines().enumerate() {
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let from_claude = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{name}:{}: direction {other:?}", number + 1),
            };
            lines.push((format!("{name}:{}", number + 1), from_claude, line));
        }
    }
    lines
}

#[test]
fn every_recorded_line_is_known() {
    let mut failures = Vec::new();
    let all = lines();
    for (place, from_claude, line) in &all {
        let result = if *from_claude {
            stream::strict(line.as_bytes()).map(drop)
        } else {
            stream::strict_input(line.as_bytes()).map(drop)
        };
        if let Err(drift) = result {
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
fn every_recorded_line_encodes_back_to_the_same_json() {
    let mut failures = Vec::new();
    for (place, from_claude, line) in lines() {
        let encoded = if from_claude {
            stream::encode_output(&stream::decode(line.as_bytes()).expect("decodes"))
        } else {
            stream::encode(&stream::decode_input(line.as_bytes()).expect("decodes"))
        };
        let written: Value = serde_json::from_str(&line).expect("line is JSON");
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
fn an_invented_type_is_unknown_and_fails_strict() {
    let line = br#"{"type":"teleport","destination":"mars","session_id":"s","uuid":"u"}"#;
    match stream::decode(line).expect("an object decodes") {
        Output::Unknown(unknown) => {
            assert_eq!(unknown.kind.as_deref(), Some("teleport"));
            assert_eq!(unknown.raw.get().as_bytes(), line);
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = stream::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("teleport"), "{drift}");

    let input = br#"{"type":"teleport","to":"mars"}"#;
    assert!(matches!(stream::decode_input(input), Ok(Input::Unknown(_))));
    assert!(stream::strict_input(input).is_err());
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
