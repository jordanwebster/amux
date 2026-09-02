//! Local diagnostic report bundles.
//!
//! A report is a directory with a small JSON header that declares every
//! optional part before readers inspect the larger payload files. Reports may
//! contain prompts, code, paths, and daemon state, so every file is created
//! private to the current user.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::RecorderSnapshot;
use crate::recorder::{MSGS_SCHEMA_VERSION, RecorderSnapshotHeader};

/// Bumped whenever the report header or directory layout changes.
pub const REPORT_SCHEMA_VERSION: u32 = 1;
/// Newest automatic reports retained for each automatic kind.
pub const RETAINED_AUTOMATIC_REPORTS: usize = 20;

/// Why a report was captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    Bug,
    Tweak,
    Tripwire,
    ChannelOverflow,
    Panic,
}

impl ReportKind {
    fn slug(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Tweak => "tweak",
            Self::Tripwire => "tripwire",
            Self::ChannelOverflow => "channel-overflow",
            Self::Panic => "panic",
        }
    }
}

/// Whether the issue represented by a report still needs work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Open,
    Done,
}

/// One annotated rectangle in terminal cells.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub note: String,
}

/// Whether a report part was captured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartState {
    Present,
    Absent { reason: String },
}

/// Presence declarations for every optional payload in a report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parts {
    pub frame: PartState,
    pub trace: PartState,
    pub msgs: PartState,
    pub daemon: PartState,
    pub log: PartState,
}

/// The outcome of replaying a captured trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    Unchecked,
    Reproduces,
    Diverges { first_diff: String },
}

/// The small, self-describing entry point for one report bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportHeader {
    pub schema_version: u32,
    pub build: String,
    pub git_sha: String,
    pub created_at: DateTime<Utc>,
    pub stamp: String,
    pub kind: ReportKind,
    pub status: ReportStatus,
    pub detail: Option<String>,
    pub note: String,
    pub marks: Vec<Mark>,
    pub viewport: Option<(u16, u16)>,
    pub parts: Parts,
    pub replay: ReplayVerdict,
}

/// The terminal frame text and per-cell style classes captured together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCapture {
    pub text: String,
    pub styles: String,
}

/// Payloads available to the writer. Missing payloads are declared absent.
pub struct ReportParts {
    pub frame: Option<FrameCapture>,
    pub trace: Option<Vec<u8>>,
    pub msgs: Option<RecorderSnapshot>,
    pub daemon: Option<String>,
    pub log: Option<String>,
    pub absent_reason: String,
}

/// Capture metadata supplied by the report's caller.
pub struct ReportDraft {
    pub kind: ReportKind,
    pub detail: Option<String>,
    pub note: String,
    pub marks: Vec<Mark>,
    pub viewport: Option<(u16, u16)>,
    pub replay: ReplayVerdict,
}

/// Writes versioned report bundles beneath one configured directory.
pub struct ReportWriter {
    dir: PathBuf,
    build: &'static str,
    git_sha: &'static str,
}

impl ReportWriter {
    pub fn new(dir: PathBuf, build: &'static str, git_sha: &'static str) -> Self {
        Self {
            dir,
            build,
            git_sha,
        }
    }

    /// Write a new report. A name collision fails instead of replacing a
    /// prior capture.
    pub fn write(&self, draft: ReportDraft, parts: ReportParts) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.dir)?;

        let created_at = Utc::now();
        let stamp = next_stamp(created_at);
        let report_dir = self.dir.join(format!("{stamp}-{}", draft.kind.slug()));
        create_private_dir(&report_dir)?;

        let states = Parts {
            frame: part_state(&parts.frame, &parts.absent_reason),
            trace: part_state(&parts.trace, &parts.absent_reason),
            msgs: part_state(&parts.msgs, &parts.absent_reason),
            daemon: part_state(&parts.daemon, &parts.absent_reason),
            log: part_state(&parts.log, &parts.absent_reason),
        };

        if let Some(frame) = parts.frame {
            write_private(&report_dir.join("frame.txt"), frame.text.as_bytes())?;
            write_private(&report_dir.join("frame.styles"), frame.styles.as_bytes())?;
        }
        if let Some(trace) = parts.trace {
            write_private(&report_dir.join("trace.jsonl"), &trace)?;
        }
        if let Some(snapshot) = parts.msgs {
            write_recorder_snapshot(&report_dir.join("msgs.jsonl"), &snapshot)?;
        }
        if let Some(daemon) = parts.daemon {
            write_private(&report_dir.join("daemon.json"), daemon.as_bytes())?;
        }
        if let Some(log) = parts.log {
            write_private(&report_dir.join("log.txt"), log.as_bytes())?;
        }

        let header = ReportHeader {
            schema_version: REPORT_SCHEMA_VERSION,
            build: self.build.to_string(),
            git_sha: self.git_sha.to_string(),
            created_at,
            stamp,
            kind: draft.kind,
            status: ReportStatus::Open,
            detail: draft.detail,
            note: draft.note,
            marks: draft.marks,
            viewport: draft.viewport,
            parts: states,
            replay: draft.replay,
        };
        let mut bytes = serde_json::to_vec_pretty(&header).map_err(io::Error::other)?;
        bytes.push(b'\n');
        write_private(&report_dir.join("report.json"), &bytes)?;
        prune(&self.dir)?;

        Ok(report_dir)
    }
}

fn part_state<T>(part: &Option<T>, absent_reason: &str) -> PartState {
    match part {
        Some(_) => PartState::Present,
        None => PartState::Absent {
            reason: absent_reason.to_string(),
        },
    }
}

fn write_recorder_snapshot(path: &Path, snapshot: &RecorderSnapshot) -> io::Result<()> {
    let mut file = create_private_file(path)?;
    let header = RecorderSnapshotHeader {
        format_version: MSGS_SCHEMA_VERSION,
        checkpoint: &snapshot.checkpoint,
    };
    serde_json::to_writer(&mut file, &header).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    for msg in &snapshot.msgs {
        file.write_all(msg.as_bytes())?;
        file.write_all(b"\n")?;
    }
    file.flush()
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create_private_file(path)?;
    file.write_all(bytes)?;
    file.flush()
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

static LAST_STAMP_MILLIS: AtomicU64 = AtomicU64::new(0);

fn next_stamp(now: DateTime<Utc>) -> String {
    let wall_millis = u64::try_from(now.timestamp_millis()).unwrap_or(0);
    let mut previous = LAST_STAMP_MILLIS.load(Ordering::Relaxed);
    let millis = loop {
        let candidate = wall_millis.max(previous.saturating_add(1));
        match LAST_STAMP_MILLIS.compare_exchange_weak(
            previous,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break candidate,
            Err(actual) => previous = actual,
        }
    };
    format!("{millis:013}-{}", std::process::id())
}

/// A report directory and either its parsed header or an explanation of why
/// the header was unreadable.
#[derive(Debug)]
pub struct ReportSummary {
    pub path: PathBuf,
    pub header: Option<ReportHeader>,
    pub error: Option<String>,
}

/// Automatic report directories removed by one retention pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub removed: Vec<PathBuf>,
}

/// Bound each automatic report kind independently. User-authored and
/// unreadable reports are never removed.
pub fn prune(dir: &Path) -> io::Result<PruneSummary> {
    let summaries = match list(dir) {
        Ok(summaries) => summaries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PruneSummary::default());
        }
        Err(error) => return Err(error),
    };
    let mut removed = Vec::new();

    for kind in [
        ReportKind::Tripwire,
        ReportKind::ChannelOverflow,
        ReportKind::Panic,
    ] {
        let mut reports = summaries
            .iter()
            .filter_map(|summary| {
                let header = summary.header.as_ref()?;
                (header.kind == kind).then(|| (header.stamp.as_str(), summary.path.as_path()))
            })
            .collect::<Vec<_>>();
        reports.sort_by(|left, right| left.0.cmp(right.0));

        let remove_count = reports.len().saturating_sub(RETAINED_AUTOMATIC_REPORTS);
        for (_, path) in reports.into_iter().take(remove_count) {
            fs::remove_dir_all(path)?;
            removed.push(path.to_path_buf());
        }
    }

    Ok(PruneSummary { removed })
}

/// List report directories newest first. Unreadable directories remain in
/// the result so corruption is visible to callers.
pub fn list(dir: &Path) -> io::Result<Vec<ReportSummary>> {
    let mut paths = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => Some(entry.path()),
            _ => None,
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.file_name().cmp(&left.file_name()));

    Ok(paths
        .into_iter()
        .map(|path| match read_header(&path) {
            Ok(header) => ReportSummary {
                path,
                header: Some(header),
                error: None,
            },
            Err(error) => ReportSummary {
                path,
                header: None,
                error: Some(error.to_string()),
            },
        })
        .collect())
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("failed to read report header: {0}")]
    Io(#[from] io::Error),
    #[error("failed to parse report header: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported report schema version {found}; expected {expected}")]
    SchemaVersion { found: u32, expected: u32 },
}

#[derive(Deserialize)]
struct SchemaVersion {
    schema_version: u32,
}

/// Read and validate a report's `report.json` header.
pub fn read_header(report: &Path) -> Result<ReportHeader, ReportError> {
    let bytes = fs::read(report.join("report.json"))?;
    let version: SchemaVersion = serde_json::from_slice(&bytes)?;
    if version.schema_version != REPORT_SCHEMA_VERSION {
        return Err(ReportError::SchemaVersion {
            found: version.schema_version,
            expected: REPORT_SCHEMA_VERSION,
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Read at most the final `max_bytes` of a log, discarding a leading partial
/// line. A missing log is not an error.
pub fn log_tail(path: &Path, max_bytes: usize) -> io::Result<Option<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let retained = len.min(u64::try_from(max_bytes).unwrap_or(u64::MAX));
    let truncated = retained < len;
    file.seek(SeekFrom::Start(len - retained))?;

    let mut bytes = Vec::with_capacity(usize::try_from(retained).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    if truncated {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }

    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{Model, Msg};

    fn draft(kind: ReportKind) -> ReportDraft {
        ReportDraft {
            kind,
            detail: Some("capture detail".to_string()),
            note: "main note".to_string(),
            marks: vec![Mark {
                x: 3,
                y: 4,
                width: 5,
                height: 2,
                note: "marked cells".to_string(),
            }],
            viewport: Some((100, 40)),
            replay: ReplayVerdict::Reproduces,
        }
    }

    fn recorder_snapshot() -> RecorderSnapshot {
        RecorderSnapshot {
            checkpoint: Model::default(),
            msgs: vec![
                serde_json::to_string(&Msg::Tick {
                    now: DateTime::from_timestamp(1_754_697_600, 0).expect("fixture time"),
                })
                .unwrap(),
            ],
        }
    }

    #[test]
    fn writes_and_reads_a_full_report_bundle() {
        let root = tempfile::tempdir().unwrap();
        let writer = ReportWriter::new(root.path().to_path_buf(), "0.4.0-test", "abc123");
        let report = writer
            .write(
                draft(ReportKind::Bug),
                ReportParts {
                    frame: Some(FrameCapture {
                        text: "hello frame\n".to_string(),
                        styles: "default default\n".to_string(),
                    }),
                    trace: Some(b"{\"draw\":1}\n".to_vec()),
                    msgs: Some(recorder_snapshot()),
                    daemon: Some("{\"hosts\":[]}".to_string()),
                    log: Some("first\nsecond\n".to_string()),
                    absent_reason: "not captured".to_string(),
                },
            )
            .unwrap();

        assert!(
            report
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-bug")
        );
        let header = read_header(&report).unwrap();
        assert_eq!(header.schema_version, REPORT_SCHEMA_VERSION);
        assert_eq!(header.build, "0.4.0-test");
        assert_eq!(header.git_sha, "abc123");
        assert_eq!(header.kind, ReportKind::Bug);
        assert_eq!(header.status, ReportStatus::Open);
        assert_eq!(header.parts.frame, PartState::Present);
        assert_eq!(header.parts.trace, PartState::Present);
        assert_eq!(header.parts.msgs, PartState::Present);
        assert_eq!(header.parts.daemon, PartState::Present);
        assert_eq!(header.parts.log, PartState::Present);

        let files = fs::read_dir(&report)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            files,
            BTreeSet::from([
                "daemon.json".to_string(),
                "frame.styles".to_string(),
                "frame.txt".to_string(),
                "log.txt".to_string(),
                "msgs.jsonl".to_string(),
                "report.json".to_string(),
                "trace.jsonl".to_string(),
            ])
        );
        assert_eq!(
            fs::read_to_string(report.join("frame.txt")).unwrap(),
            "hello frame\n"
        );
        assert_eq!(
            fs::read(report.join("trace.jsonl")).unwrap(),
            b"{\"draw\":1}\n"
        );
        let msgs = fs::read_to_string(report.join("msgs.jsonl")).unwrap();
        assert_eq!(msgs.lines().count(), 2);

        #[cfg(unix)]
        for file in files {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(report.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn writes_a_degraded_report_with_every_part_declared() {
        let root = tempfile::tempdir().unwrap();
        let writer = ReportWriter::new(root.path().to_path_buf(), "release", "def456");
        let report = writer
            .write(
                draft(ReportKind::Tripwire),
                ReportParts {
                    frame: None,
                    trace: None,
                    msgs: Some(recorder_snapshot()),
                    daemon: None,
                    log: Some("tripwire log\n".to_string()),
                    absent_reason: "unavailable in release build".to_string(),
                },
            )
            .unwrap();

        let header = read_header(&report).unwrap();
        let absent = PartState::Absent {
            reason: "unavailable in release build".to_string(),
        };
        assert_eq!(header.parts.frame, absent);
        assert_eq!(header.parts.trace, absent);
        assert_eq!(header.parts.msgs, PartState::Present);
        assert_eq!(header.parts.daemon, absent);
        assert_eq!(header.parts.log, PartState::Present);
        assert!(!report.join("frame.txt").exists());
        assert!(!report.join("frame.styles").exists());
        assert!(!report.join("trace.jsonl").exists());
        assert!(report.join("msgs.jsonl").exists());
        assert!(!report.join("daemon.json").exists());
        assert!(report.join("log.txt").exists());
    }

    #[test]
    fn list_is_newest_first_and_keeps_unreadable_directories() {
        let root = tempfile::tempdir().unwrap();
        let writer = ReportWriter::new(root.path().to_path_buf(), "test", "abc");
        let first = writer
            .write(
                draft(ReportKind::Bug),
                ReportParts {
                    frame: None,
                    trace: None,
                    msgs: None,
                    daemon: None,
                    log: None,
                    absent_reason: "test".to_string(),
                },
            )
            .unwrap();
        let second = writer
            .write(
                draft(ReportKind::Tweak),
                ReportParts {
                    frame: None,
                    trace: None,
                    msgs: None,
                    daemon: None,
                    log: None,
                    absent_reason: "test".to_string(),
                },
            )
            .unwrap();
        fs::create_dir(root.path().join("broken-report")).unwrap();

        let summaries = list(root.path()).unwrap();
        let readable = summaries
            .iter()
            .filter_map(|summary| summary.header.as_ref().map(|header| (summary, header)))
            .collect::<Vec<_>>();
        assert_eq!(readable.len(), 2);
        assert_eq!(readable[0].0.path, second);
        assert_eq!(readable[1].0.path, first);
        let broken = summaries
            .iter()
            .find(|summary| summary.path.ends_with("broken-report"))
            .unwrap();
        assert!(broken.header.is_none());
        assert!(broken.error.as_deref().unwrap().contains("failed to read"));
    }

    #[test]
    fn read_header_names_found_and_expected_schema_versions() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("report.json"), r#"{"schema_version":99}"#).unwrap();

        let error = read_header(root.path()).unwrap_err().to_string();
        assert!(error.contains("99"));
        assert!(error.contains(&REPORT_SCHEMA_VERSION.to_string()));
    }

    #[test]
    fn log_tail_handles_missing_short_and_truncated_logs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("amux.log");
        assert_eq!(log_tail(&path, 16).unwrap(), None);

        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        assert_eq!(
            log_tail(&path, 100).unwrap().as_deref(),
            Some("one\ntwo\nthree\n")
        );
        assert_eq!(log_tail(&path, 9).unwrap().as_deref(), Some("three\n"));
        assert_eq!(log_tail(&path, 0).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn retention_bounds_each_automatic_kind_and_keeps_user_reports() {
        let root = tempfile::tempdir().unwrap();
        let writer = ReportWriter::new(root.path().to_path_buf(), "test", "abc");
        let empty_parts = || ReportParts {
            frame: None,
            trace: None,
            msgs: None,
            daemon: None,
            log: None,
            absent_reason: "retention fixture".to_string(),
        };

        let tripwire_paths = (0..25)
            .map(|_| {
                writer
                    .write(draft(ReportKind::Tripwire), empty_parts())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for _ in 0..3 {
            writer
                .write(draft(ReportKind::Panic), empty_parts())
                .unwrap();
        }
        for kind in [ReportKind::Bug, ReportKind::Tweak, ReportKind::Bug] {
            writer.write(draft(kind), empty_parts()).unwrap();
        }

        let headers = list(root.path())
            .unwrap()
            .into_iter()
            .filter_map(|summary| summary.header)
            .collect::<Vec<_>>();
        assert_eq!(
            headers
                .iter()
                .filter(|header| header.kind == ReportKind::Tripwire)
                .count(),
            RETAINED_AUTOMATIC_REPORTS
        );
        assert!(tripwire_paths[..5].iter().all(|path| !path.exists()));
        assert!(tripwire_paths[5..].iter().all(|path| path.exists()));
        assert_eq!(
            headers
                .iter()
                .filter(|header| header.kind == ReportKind::Panic)
                .count(),
            3
        );
        assert_eq!(
            headers
                .iter()
                .filter(|header| matches!(header.kind, ReportKind::Bug | ReportKind::Tweak))
                .count(),
            3
        );
        assert!(prune(root.path()).unwrap().removed.is_empty());
    }
}
