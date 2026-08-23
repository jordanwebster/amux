//! Fail-loud promotion of redacted live recordings into regression fixtures.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::redact;

pub fn verify_run(run_dir: &Path, scenarios: &[String]) -> Result<()> {
    for scenario in scenarios {
        let rows_path = run_dir.join(format!("{scenario}.redacted.jsonl"));
        let meta_path = run_dir.join(format!("{scenario}.meta.json"));
        let rows = std::fs::read_to_string(&rows_path)
            .with_context(|| format!("read {}", rows_path.display()))?;
        redact::verify_candidate(&rows)
            .with_context(|| format!("verify {}", rows_path.display()))?;
        for (line_no, line) in rows.lines().enumerate() {
            if !line.trim().is_empty() {
                serde_json::from_str::<Value>(line).with_context(|| {
                    format!("{}:{} is not valid JSON", rows_path.display(), line_no + 1)
                })?;
            }
        }
        let meta_bytes =
            std::fs::read(&meta_path).with_context(|| format!("read {}", meta_path.display()))?;
        let meta_text = std::str::from_utf8(&meta_bytes)?;
        redact::verify_candidate(meta_text)
            .with_context(|| format!("verify {}", meta_path.display()))?;
        let meta: Value = serde_json::from_slice(&meta_bytes)?;
        if meta.get("scenario").and_then(Value::as_str) != Some(scenario) {
            bail!("{} has the wrong scenario stamp", meta_path.display());
        }
        if !meta.get("failure").is_some_and(Value::is_null) {
            bail!("{} records a failed scenario", meta_path.display());
        }
        for field in ["captured_at", "claude_version", "model", "harness"] {
            if meta.get(field).and_then(Value::as_str).is_none() {
                bail!(
                    "{} is missing provenance field {field}",
                    meta_path.display()
                );
            }
        }
    }
    Ok(())
}

pub fn graduate(run_dir: &Path, fixture_dir: &Path, scenarios: &[String]) -> Result<()> {
    verify_run(run_dir, scenarios)?;
    std::fs::create_dir_all(fixture_dir)?;
    for scenario in scenarios {
        let rows = std::fs::read(run_dir.join(format!("{scenario}.redacted.jsonl")))?;
        let meta_path = run_dir.join(format!("{scenario}.meta.json"));
        let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path)?)?;
        let object = meta
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", meta_path.display()))?;
        object.insert(
            "graduated_at".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        object.insert(
            "graduated_from".to_string(),
            Value::String("redacted capture run".to_string()),
        );
        std::fs::write(fixture_dir.join(format!("{scenario}.rows.jsonl")), rows)?;
        std::fs::write(
            fixture_dir.join(format!("{scenario}.meta.json")),
            serde_json::to_string_pretty(&meta)?,
        )?;
    }
    Ok(())
}

/// Promote A2A carrier observations without pretending they are baseline
/// chat-v1 fixtures. The short names are the durable public corpus names
/// used by the implementation's structural waiters.
pub fn graduate_a2a(run_dir: &Path, fixture_dir: &Path, scenarios: &[String]) -> Result<()> {
    verify_run(run_dir, scenarios)?;
    std::fs::create_dir_all(fixture_dir)?;
    for scenario in scenarios {
        let name = scenario.strip_prefix("a2a_").ok_or_else(|| {
            anyhow::anyhow!("A2A fixture scenario must begin with a2a_: {scenario}")
        })?;
        let rows = std::fs::read(run_dir.join(format!("{scenario}.redacted.jsonl")))?;
        let meta_path = run_dir.join(format!("{scenario}.meta.json"));
        let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path)?)?;
        let object = meta
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", meta_path.display()))?;
        object.insert(
            "graduated_at".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        object.insert(
            "graduated_from".to_string(),
            Value::String("redacted capture run".to_string()),
        );
        std::fs::write(fixture_dir.join(format!("{name}.jsonl")), rows)?;
        std::fs::write(
            fixture_dir.join(format!("{name}.meta.json")),
            serde_json::to_string_pretty(&meta)?,
        )?;
    }
    Ok(())
}
