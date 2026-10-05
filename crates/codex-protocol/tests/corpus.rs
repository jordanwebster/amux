//! Every recorded Codex line, in both directions, decodes with nothing left
//! unknown and encodes back to the same JSON. A failure here is provider
//! drift, or a type that does not match what Codex writes.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every io.jsonl recorded from Codex: the live corpus and the journeys'.
fn recordings() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in ["crates/codex-specs/fixtures/runtime", "journeys/recordings"] {
        let mut entries: Vec<_> = std::fs::read_dir(root().join(dir))
            .unwrap_or_else(|error| panic!("{dir}: {error}"))
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.join("io.jsonl").is_file())
            .collect();
        entries.sort();
        for recording in entries {
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(recording.join("manifest.json")).expect("manifest.json"),
            )
            .expect("manifest is JSON");
            if manifest["recorded"]["provider"] == "codex" {
                found.push(recording.join("io.jsonl"));
            }
        }
    }
    assert!(
        found.len() >= 29,
        "found only {} Codex recordings",
        found.len()
    );
    found
}

/// Each recorded line with where it came from and which way it went.
fn lines() -> Vec<(String, bool, String)> {
    let mut lines = Vec::new();
    for path in recordings() {
        let name = path
            .strip_prefix(root())
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("recording");
        for (number, record) in text.lines().enumerate() {
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let from_server = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{name}:{}: direction {other:?}", number + 1),
            };
            lines.push((format!("{name}:{}", number + 1), from_server, line));
        }
    }
    lines
}

#[test]
fn every_recorded_line_is_known() {
    let mut failures = Vec::new();
    let all = lines();
    for (place, from_server, line) in &all {
        let result = if *from_server {
            codex_protocol::strict(line.as_bytes()).map(drop)
        } else {
            codex_protocol::strict_client(line.as_bytes()).map(drop)
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
    for (place, from_server, line) in lines() {
        let encoded = if from_server {
            codex_protocol::encode_server(
                &codex_protocol::decode(line.as_bytes()).expect("decodes"),
            )
        } else {
            codex_protocol::encode(
                &codex_protocol::decode_client(line.as_bytes()).expect("decodes"),
            )
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
fn an_invented_method_is_unknown_and_fails_strict() {
    let line =
        br#"{"method":"thread/teleported","params":{"threadId":"t","to":"mars"},"emittedAtMs":1}"#;
    match codex_protocol::decode(line).expect("an object decodes") {
        codex_protocol::ServerMessage::Unknown(unknown) => {
            assert_eq!(unknown.method.as_deref(), Some("thread/teleported"));
            assert_eq!(unknown.raw.get().as_bytes(), line);
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = codex_protocol::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("thread/teleported"), "{drift}");
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
