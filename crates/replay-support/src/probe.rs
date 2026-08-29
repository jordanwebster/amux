use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;

use chrono::Utc;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{DriftReport, Observed, SpecEntry, Verification, drift};

/// One live execution before its ledger or recording is updated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeAttempt {
    pub claim: Result<(), String>,
    pub recorded: Observed,
    pub live: Observed,
    pub raw_payloads: usize,
}

/// One live probe over a provider corpus.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRun {
    pub run_id: String,
    pub provider: String,
    pub version: Version,
    pub dir: PathBuf,
    pub results: Vec<ProbeResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub spec: String,
    pub outcome: ProbeOutcome,
    pub drift: DriftReport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProbeOutcome {
    Passed,
    Failed { claim: String },
    ReRecorded { claim: String },
}

/// Run every registered specification live and update only the evidence its
/// claim permits. Structural drift is retained beside the outcome and never
/// participates in deciding it.
pub async fn probe<F, LiveFuture, P, PassFuture, R, RecordFuture>(
    run: &mut ProbeRun,
    registry: &[SpecEntry],
    mut live: F,
    mut on_pass: P,
    mut re_record: R,
) -> Result<(PathBuf, PathBuf), io::Error>
where
    F: FnMut(SpecEntry) -> LiveFuture,
    LiveFuture: Future<Output = Result<ProbeAttempt, io::Error>>,
    P: FnMut(SpecEntry, Verification) -> PassFuture,
    PassFuture: Future<Output = Result<(), io::Error>>,
    R: FnMut(SpecEntry) -> RecordFuture,
    RecordFuture: Future<Output = Result<(), io::Error>>,
{
    run.results.clear();
    for entry in registry.iter().copied() {
        let attempt = live(entry).await?;
        let report = drift(&attempt.recorded, &attempt.live, attempt.raw_payloads);
        let outcome = match attempt.claim {
            Ok(()) => {
                on_pass(
                    entry,
                    Verification {
                        version: run.version.clone(),
                        at: Utc::now(),
                        run_id: run.run_id.clone(),
                    },
                )
                .await?;
                ProbeOutcome::Passed
            }
            Err(claim) => {
                re_record(entry).await?;
                ProbeOutcome::ReRecorded { claim }
            }
        };
        run.results.push(ProbeResult {
            spec: entry.name.to_string(),
            outcome,
            drift: report,
        });
    }

    std::fs::create_dir_all(&run.dir)?;
    let probe_path = run.dir.join("probe.json");
    let drift_path = run.dir.join("drift.json");
    write_json(&probe_path, run)?;
    let reports = run
        .results
        .iter()
        .map(|result| (result.spec.clone(), result.drift.clone()))
        .collect::<BTreeMap<_, _>>();
    write_json(&drift_path, &reports)?;
    Ok((probe_path, drift_path))
}

fn write_json(path: &std::path::Path, value: &impl Serialize) -> Result<(), io::Error> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::DateTime;

    use super::*;
    use crate::{append_verification, load_recording, migrate_legacy_manifest};

    const MODELS: &[&str] = &["test-model"];
    const REGISTRY: &[SpecEntry] = &[
        SpecEntry {
            name: "passes",
            recording: "passes",
            allowed_models: MODELS,
        },
        SpecEntry {
            name: "fails",
            recording: "fails",
            allowed_models: MODELS,
        },
    ];

    fn write_recording(root: &std::path::Path, entry: SpecEntry) {
        let dir = root.join(entry.recording);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("io.jsonl"),
            b"{\"us\":0,\"dir\":\"stdout\",\"line\":\"{}\"}\n",
        )
        .unwrap();
        std::fs::write(dir.join("spawn.jsonl"), b"{}\n").unwrap();
        let legacy = serde_json::json!({
            "schema_version": 1,
            "spec": entry.name,
            "codex_version": "1.0.0",
            "model": "test-model"
        });
        let manifest = migrate_legacy_manifest(&legacy, "codex", &dir).unwrap();
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(dir.join("manifest.json"), bytes).unwrap();
    }

    fn observed(frame: &str, field: &str, discriminant: &str) -> Observed {
        Observed {
            frames: [frame.to_string()].into_iter().collect(),
            fields: [field.to_string()].into_iter().collect(),
            discriminants: [discriminant.to_string()].into_iter().collect(),
        }
    }

    #[tokio::test]
    async fn probe_appends_passes_rerecords_failures_and_writes_reports() {
        let temp = tempfile::tempdir().unwrap();
        for entry in REGISTRY {
            write_recording(temp.path(), *entry);
        }
        let pass_io_before = std::fs::read(temp.path().join("passes/io.jsonl")).unwrap();
        let pass_spawn_before = std::fs::read(temp.path().join("passes/spawn.jsonl")).unwrap();
        let fail_spawn_before = std::fs::read(temp.path().join("fails/spawn.jsonl")).unwrap();
        let rerecorded = Arc::new(Mutex::new(Vec::new()));
        let pass_root = temp.path().to_path_buf();
        let record_root = temp.path().to_path_buf();
        let rerecorded_for_callback = Arc::clone(&rerecorded);
        let mut run = ProbeRun {
            run_id: "probe-run".to_string(),
            provider: "codex".to_string(),
            version: Version::parse("1.1.0").unwrap(),
            dir: temp.path().join("run"),
            results: vec![ProbeResult {
                spec: "stale".to_string(),
                outcome: ProbeOutcome::Failed {
                    claim: "stale".to_string(),
                },
                drift: DriftReport::default(),
            }],
        };

        let (probe_path, drift_path) = probe(
            &mut run,
            REGISTRY,
            |entry| async move {
                if entry.name == "passes" {
                    Ok(ProbeAttempt {
                        claim: Ok(()),
                        recorded: observed("assistant", "type", "assistant"),
                        live: observed("new_frame", "message.new_field", "new_kind"),
                        raw_payloads: 3,
                    })
                } else {
                    Ok(ProbeAttempt {
                        claim: Err("expected response was absent".to_string()),
                        recorded: Observed::default(),
                        live: Observed::default(),
                        raw_payloads: 0,
                    })
                }
            },
            move |entry, verification| {
                let root = pass_root.clone();
                async move {
                    append_verification(&root.join(entry.recording), verification)
                        .map_err(io::Error::other)
                }
            },
            move |entry| {
                let root = record_root.clone();
                let rerecorded = Arc::clone(&rerecorded_for_callback);
                async move {
                    rerecorded.lock().unwrap().push(entry.name.to_string());
                    std::fs::write(root.join(entry.recording).join("io.jsonl"), b"rerecorded\n")
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(probe_path, run.dir.join("probe.json"));
        assert_eq!(drift_path, run.dir.join("drift.json"));
        assert!(probe_path.is_file());
        assert!(drift_path.is_file());
        assert_eq!(*rerecorded.lock().unwrap(), vec!["fails"]);
        assert_eq!(
            std::fs::read(temp.path().join("passes/io.jsonl")).unwrap(),
            pass_io_before
        );
        assert_eq!(
            std::fs::read(temp.path().join("passes/spawn.jsonl")).unwrap(),
            pass_spawn_before
        );
        assert_eq!(
            std::fs::read(temp.path().join("fails/spawn.jsonl")).unwrap(),
            fail_spawn_before
        );
        assert_eq!(
            std::fs::read(temp.path().join("fails/io.jsonl")).unwrap(),
            b"rerecorded\n"
        );

        let passing = load_recording(&temp.path().join("passes")).unwrap();
        assert_eq!(passing.manifest.verified.len(), 1);
        assert_eq!(passing.manifest.verified[0].version, run.version);
        assert_eq!(passing.manifest.verified[0].run_id, run.run_id);
        assert!(passing.manifest.verified[0].at > DateTime::UNIX_EPOCH);

        assert_eq!(run.results.len(), 2, "stale results must be replaced");
        assert_eq!(run.results[0].outcome, ProbeOutcome::Passed);
        assert_eq!(run.results[0].drift.raw_payloads, 3);
        assert!(run.results[0].drift.new_frames.contains("new_frame"));
        assert_eq!(
            run.results[1].outcome,
            ProbeOutcome::ReRecorded {
                claim: "expected response was absent".to_string()
            }
        );

        let stored_run: ProbeRun =
            serde_json::from_slice(&std::fs::read(probe_path).unwrap()).unwrap();
        assert_eq!(stored_run, run);
        let stored_drift: BTreeMap<String, DriftReport> =
            serde_json::from_slice(&std::fs::read(drift_path).unwrap()).unwrap();
        assert_eq!(stored_drift["passes"], run.results[0].drift);
        assert_eq!(stored_drift["fails"], DriftReport::default());
    }
}
