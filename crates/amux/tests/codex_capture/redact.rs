//! Minimal fail-loud redaction for synthetic Codex capture artifacts.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub fn redact_jsonl(input: &str, scratch: &Path) -> Result<String> {
    let mut output = String::new();
    for (line_no, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(line)
            .with_context(|| format!("parse capture JSONL row {}", line_no + 1))?;
        redact_value(&mut value, scratch);
        output.push_str(&serde_json::to_string(&value)?);
        output.push('\n');
    }
    verify(&output, scratch)?;
    Ok(output)
}

pub fn redact_json(input: &str, scratch: &Path) -> Result<String> {
    let mut value: Value = serde_json::from_str(input)?;
    redact_value(&mut value, scratch);
    let output = serde_json::to_string_pretty(&value)?;
    verify(&output, scratch)?;
    Ok(output)
}

fn redact_value(value: &mut Value, scratch: &Path) {
    match value {
        Value::String(text) => *text = redact_text(text, scratch),
        Value::Array(values) => {
            for value in values {
                redact_value(value, scratch);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, scratch);
            }
        }
        _ => {}
    }
}

fn redact_text(input: &str, scratch: &Path) -> String {
    let mut output = input.replace(&scratch.display().to_string(), "[SCRATCH]");
    if let Ok(home) = std::env::var("HOME") {
        output = output.replace(&home, "[HOME]");
    }
    output
}

fn verify(output: &str, scratch: &Path) -> Result<()> {
    let mut violations = Vec::new();
    let scratch = scratch.display().to_string();
    if output.contains(&scratch) {
        violations.push("scratch path");
    }
    if let Ok(home) = std::env::var("HOME")
        && output.contains(&home)
    {
        violations.push("home path");
    }
    for marker in ["sk-ant-", "oauth_token", "OAUTH_TOKEN", "Bearer "] {
        if output.contains(marker) {
            violations.push(marker);
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!("Codex capture redaction failed: {violations:?}")
    }
}
