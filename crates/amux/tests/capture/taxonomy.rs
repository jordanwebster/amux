//! Claude transcript taxonomy snapshots and version-drift reporting.
//!
//! Drift is evidence, not a test failure. The expected category inventory is
//! read from `notes/chat-v1/transcript-semantics.md`; the committed redacted
//! fixtures provide the last-observed structural key sets. A live run can add,
//! remove, or reshape categories without making the H suite red. The report is
//! the input to the spec-first drift protocol.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    categories: BTreeMap<String, BTreeSet<String>>,
    versions: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShapeChange {
    pub category: String,
    pub added_paths: Vec<String>,
    pub removed_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DriftReport {
    pub baseline_versions: Vec<String>,
    pub run_versions: Vec<String>,
    pub additions: Vec<String>,
    pub removals: Vec<String>,
    pub shape_changes: Vec<ShapeChange>,
}

impl DriftReport {
    pub fn changed(&self) -> bool {
        !self.additions.is_empty() || !self.removals.is_empty() || !self.shape_changes.is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("Claude transcript taxonomy drift (DATA; never a version gate)\n");
        out.push_str(&format!(
            "baseline versions: {:?}\nrun versions: {:?}\n",
            self.baseline_versions, self.run_versions
        ));
        render_list(&mut out, "added categories", &self.additions);
        render_list(&mut out, "missing categories", &self.removals);
        if self.shape_changes.is_empty() {
            out.push_str("shape changes: none\n");
        } else {
            out.push_str("shape changes:\n");
            for change in &self.shape_changes {
                out.push_str(&format!("  {}\n", change.category));
                for path in &change.added_paths {
                    out.push_str(&format!("    + {path}\n"));
                }
                for path in &change.removed_paths {
                    out.push_str(&format!("    - {path}\n"));
                }
            }
        }
        out
    }
}

fn render_list(out: &mut String, label: &str, items: &[String]) {
    if items.is_empty() {
        out.push_str(&format!("{label}: none\n"));
    } else {
        out.push_str(&format!("{label}:\n"));
        for item in items {
            out.push_str(&format!("  {item}\n"));
        }
    }
}

impl Snapshot {
    pub fn from_capture_dir(dir: &Path) -> Result<Self> {
        Self::from_dir(dir, |name| {
            name.ends_with(".redacted.jsonl") || name.ends_with(".rows.jsonl")
        })
    }

    pub fn from_fixture_dir(dir: &Path) -> Result<Self> {
        Self::from_dir(dir, |name| name.ends_with(".rows.jsonl"))
    }

    fn from_dir(dir: &Path, include: impl Fn(&str) -> bool) -> Result<Self> {
        let mut paths = std::fs::read_dir(dir)
            .with_context(|| format!("read taxonomy directory {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort_unstable_by_key(std::fs::DirEntry::file_name);
        let mut snapshot = Self::default();
        let mut files = 0usize;
        for entry in paths {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !include(&name) {
                continue;
            }
            files += 1;
            let input = std::fs::read_to_string(entry.path())?;
            snapshot
                .add_jsonl(&input)
                .with_context(|| format!("parse taxonomy from {}", entry.path().display()))?;
        }
        if files == 0 {
            bail!("no transcript JSONL files found in {}", dir.display());
        }
        Ok(snapshot)
    }

    pub fn add_jsonl(&mut self, input: &str) -> Result<()> {
        for (line_no, line) in input.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = serde_json::from_str(line)
                .with_context(|| format!("invalid JSONL row {}", line_no + 1))?;
            self.add_row(&row);
        }
        Ok(())
    }

    fn add_row(&mut self, row: &Value) {
        let row_type = row
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        self.add_shape(format!("row/{row_type}"), row);
        if let Some(version) = row.get("version").and_then(Value::as_str) {
            self.versions.insert(version.to_string());
        }
        if row_type == "system" {
            let subtype = row
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            self.add_shape(format!("system/{subtype}"), row);
        }
        if row_type == "attachment"
            && let Some(attachment) = row.get("attachment")
        {
            let kind = attachment
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            self.add_shape(format!("attachment/{kind}"), attachment);
        }
        if let Some(blocks) = row.pointer("/message/content").and_then(Value::as_array) {
            for block in blocks {
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>");
                self.add_shape(format!("block/{kind}"), block);
            }
        }
    }

    fn add_shape(&mut self, category: String, value: &Value) {
        let fields = self.categories.entry(category).or_default();
        collect_shape_paths(value, "", fields);
    }
}

fn collect_shape_paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.is_empty() && !prefix.is_empty() {
                out.insert(format!("{prefix}:object"));
            }
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_shape_paths(child, &path, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{prefix}[]");
            if items.is_empty() {
                out.insert(format!("{path}:empty"));
            } else {
                for item in items {
                    collect_shape_paths(item, &path, out);
                }
            }
        }
        Value::Null => {
            out.insert(format!("{prefix}:null"));
        }
        Value::Bool(_) => {
            out.insert(format!("{prefix}:bool"));
        }
        Value::Number(_) => {
            out.insert(format!("{prefix}:number"));
        }
        Value::String(_) => {
            out.insert(format!("{prefix}:string"));
        }
    }
}

/// Categories stated by the markdown inventory. Parsing the normative note
/// keeps additions/removals tied to the document instead of a second hand-
/// maintained Rust list. Phase-correction fixtures are unioned by [`diff`].
pub fn markdown_categories(markdown: &str) -> BTreeSet<String> {
    let mut categories = BTreeSet::new();
    parse_first_column(
        markdown,
        "## 3. Row taxonomy",
        "## 4.",
        "row/",
        &mut categories,
    );
    parse_first_column(
        markdown,
        "## 7. Content blocks",
        "## 8.",
        "block/",
        &mut categories,
    );
    parse_first_column(
        markdown,
        "## 8. `system` rows",
        "## 9.",
        "system/",
        &mut categories,
    );
    parse_first_column(
        markdown,
        "## 9. `attachment` rows",
        "## 10.",
        "attachment/",
        &mut categories,
    );
    parse_first_column(
        markdown,
        "## 10. Session-state",
        "## 11.",
        "row/",
        &mut categories,
    );
    parse_first_column(
        markdown,
        "## 11. File-history",
        "## 12.",
        "row/",
        &mut categories,
    );
    categories.insert("row/amux.transcript_ready".to_string());
    for hook in ["hook.permission_request", "hook.stop", "hook.notification"] {
        categories.insert(format!("row/{hook}"));
    }
    categories
}

fn parse_first_column(
    markdown: &str,
    start: &str,
    end: &str,
    prefix: &str,
    out: &mut BTreeSet<String>,
) {
    let Some(section) = markdown.split_once(start).map(|(_, tail)| tail) else {
        return;
    };
    let section = section.split_once(end).map_or(section, |(body, _)| body);
    for line in section.lines() {
        let Some(first) = line
            .strip_prefix('|')
            .and_then(|line| line.split('|').next())
        else {
            continue;
        };
        let first = first.trim();
        let Some(value) = first.strip_prefix('`').and_then(|v| v.strip_suffix('`')) else {
            continue;
        };
        if matches!(value, "type" | "block" | "subtype" | "attachment.type") {
            continue;
        }
        out.insert(format!("{prefix}{value}"));
    }
}

pub fn diff(markdown: &str, baseline: &Snapshot, run: &Snapshot) -> DriftReport {
    let mut expected = markdown_categories(markdown);
    expected.extend(baseline.categories.keys().cloned());
    let actual: BTreeSet<_> = run.categories.keys().cloned().collect();

    let additions = actual.difference(&expected).cloned().collect();
    let removals = expected.difference(&actual).cloned().collect();
    let mut shape_changes = Vec::new();
    for category in expected.intersection(&actual) {
        let Some(before) = baseline.categories.get(category) else {
            continue;
        };
        let after = &run.categories[category];
        let added_paths: Vec<_> = after.difference(before).cloned().collect();
        let removed_paths: Vec<_> = before.difference(after).cloned().collect();
        if !added_paths.is_empty() || !removed_paths.is_empty() {
            shape_changes.push(ShapeChange {
                category: category.clone(),
                added_paths,
                removed_paths,
            });
        }
    }
    DriftReport {
        baseline_versions: baseline.versions.iter().cloned().collect(),
        run_versions: run.versions.iter().cloned().collect(),
        additions,
        removals,
        shape_changes,
    }
}

pub fn write_report(
    run_dir: &Path,
    fixture_dir: &Path,
    semantics_markdown: &str,
) -> Result<DriftReport> {
    let baseline = Snapshot::from_fixture_dir(fixture_dir)?;
    let run = Snapshot::from_capture_dir(run_dir)?;
    let report = diff(semantics_markdown, &baseline, &run);
    std::fs::write(
        run_dir.join("taxonomy-drift.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    std::fs::write(run_dir.join("taxonomy-drift.txt"), report.render())?;
    Ok(report)
}
