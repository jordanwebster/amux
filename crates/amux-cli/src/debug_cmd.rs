//! Debug-only command tree and local report operations.
//!
//! This module is absent from release builds along with the CLI surface that
//! reaches it. The report bundle itself remains in `amux-ui` so release builds
//! can still write degraded tripwire and panic reports.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use amux::{Config, DebugFormat};
use amux_tui::replay::{self, Replay};
use amux_ui::report::{
    self, ReplayVerdict, ReportHeader, ReportKind, ReportStatus, read_frame, set_verdict,
};
use anyhow::{Context, Result, anyhow};
use clap::{Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
pub enum DebugCommands {
    /// Print daemon diagnostics
    Daemon {
        /// Include detailed per-agent and per-session state
        #[arg(long)]
        verbose: bool,

        /// Output format
        #[arg(long, value_enum, default_value_t = CliDebugFormat::Yaml)]
        format: CliDebugFormat,
    },

    /// Inspect locally captured diagnostic reports
    Report {
        #[command(subcommand)]
        command: ReportCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum ReportCommands {
    /// List captured reports, newest first
    List,

    /// Print a report's validated header
    Show {
        /// Report directory path or name beneath the configured reports directory
        report: PathBuf,
    },

    /// Replay a report and compare it with the captured frame
    Replay {
        /// Report directory path or name beneath the configured reports directory
        report: PathBuf,

        /// Render the frame after this draw event instead of the final frame
        #[arg(long)]
        at: Option<usize>,

        /// Print the rendered frame text
        #[arg(long)]
        frame: bool,

        /// Print the rendered frame style map
        #[arg(long)]
        styles: bool,
    },

    /// Remove old automatic reports; user-created bug and tweak reports remain
    Prune,
}

/// CLI-side mirror of `DebugFormat` so the core `amux` crate remains
/// independent of clap.
#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CliDebugFormat {
    Yaml,
    Json,
}

impl From<CliDebugFormat> for DebugFormat {
    fn from(value: CliDebugFormat) -> Self {
        match value {
            CliDebugFormat::Yaml => DebugFormat::Yaml,
            CliDebugFormat::Json => DebugFormat::Json,
        }
    }
}

pub struct ReportCommandOutput {
    pub text: String,
    pub exit_code: ExitCode,
}

impl ReportCommandOutput {
    fn success(text: String) -> Self {
        Self {
            text,
            exit_code: ExitCode::SUCCESS,
        }
    }
}

/// Run a report command against the single directory selected by the config.
///
/// Returning text keeps command behavior testable without redirecting the
/// process-wide stdout stream; the binary owns the final print.
pub fn run_report(command: ReportCommands, config: &Config) -> Result<ReportCommandOutput> {
    let reports_dir = config.reports_dir();
    match command {
        ReportCommands::List => list_reports(&reports_dir).map(ReportCommandOutput::success),
        ReportCommands::Show { report } => {
            show_report(&reports_dir, &report).map(ReportCommandOutput::success)
        }
        ReportCommands::Replay {
            report,
            at,
            frame,
            styles,
        } => replay_report(&reports_dir, &report, at, frame, styles),
        ReportCommands::Prune => prune_reports(&reports_dir).map(ReportCommandOutput::success),
    }
}

fn replay_report(
    reports_dir: &Path,
    requested: &Path,
    at: Option<usize>,
    print_frame: bool,
    print_styles: bool,
) -> Result<ReportCommandOutput> {
    let report_dir = resolve_report(reports_dir, requested);
    let expected = read_frame(&report_dir)
        .with_context(|| format!("failed to read captured frame in {}", report_dir.display()))?
        .ok_or_else(|| anyhow!("report {} has no captured frame", report_dir.display()))?;
    let mut replay = Replay::load(&report_dir)
        .with_context(|| format!("failed to load report {}", report_dir.display()))?;

    if let Some(index) = at {
        let draw_indices = replay.draw_indices();
        if !draw_indices.contains(&index) {
            let available = draw_indices
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow!(
                "event {index} is not a draw in report {}; draw events: [{}]",
                report_dir.display(),
                available
            ));
        }
    }

    replay
        .step_to_end()
        .with_context(|| format!("failed to replay report {}", report_dir.display()))?;
    let actual = replay
        .frame()
        .context("the replay produced no final frame")?;
    let diff = replay::frame_diff(&expected, &actual);
    let verdict = replay::verdict(&expected, &actual);
    set_verdict(&report_dir, verdict.clone())
        .with_context(|| format!("failed to update report {}", report_dir.display()))?;

    let mut output = String::new();
    match &verdict {
        ReplayVerdict::Reproduces => output.push_str("Reproduces\n"),
        ReplayVerdict::Diverges { first_diff } => {
            output.push_str(&format!("Diverges: {first_diff}\n"));
        }
        ReplayVerdict::Unchecked => unreachable!("comparing two frames always yields a verdict"),
    }
    if diff.cells.is_empty() {
        output.push_str("Differing cells: none\nBounding rectangle: none\n");
    } else {
        let cells = diff
            .cells
            .iter()
            .map(|(x, y)| format!("({x},{y})"))
            .collect::<Vec<_>>()
            .join(" ");
        output.push_str(&format!("Differing cells: {cells}\n"));
        let bounding = diff.bounding.expect("a non-empty diff has a bounding mark");
        output.push_str(&format!(
            "Bounding rectangle: x={} y={} width={} height={}\n",
            bounding.x, bounding.y, bounding.width, bounding.height
        ));
    }

    if print_frame || print_styles {
        let (capture, position) = match at {
            Some(index) => {
                let mut positioned = Replay::load(&report_dir)
                    .with_context(|| format!("failed to reload report {}", report_dir.display()))?;
                positioned
                    .step_to(index)
                    .with_context(|| format!("failed to replay report to event {index}"))?;
                (
                    positioned
                        .frame()
                        .context("the selected draw produced no frame")?,
                    index,
                )
            }
            None => (actual, replay.position().saturating_sub(1)),
        };
        if print_frame {
            output.push_str(&format!("Frame at event {position}:\n{}", capture.text));
        }
        if print_styles {
            output.push_str(&format!("Styles at event {position}:\n{}", capture.styles));
        }
    }

    Ok(ReportCommandOutput {
        text: output,
        exit_code: if matches!(verdict, ReplayVerdict::Diverges { .. }) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        },
    })
}

fn list_reports(reports_dir: &Path) -> Result<String> {
    let reports = match report::list(reports_dir) {
        Ok(reports) => reports,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(format!("No reports in {}\n", reports_dir.display()));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to list reports in {}", reports_dir.display()));
        }
    };

    if reports.is_empty() {
        return Ok(format!("No reports in {}\n", reports_dir.display()));
    }

    let mut output = String::new();
    for summary in reports {
        match summary.header {
            Some(header) => output.push_str(&format_report_row(&header, &summary.path)),
            None => {
                let name = summary
                    .path
                    .file_name()
                    .unwrap_or(summary.path.as_os_str())
                    .to_string_lossy();
                output.push_str(&format!(
                    "{name}\tunreadable\t-\t-\t{}\t{}\n",
                    summary.path.display(),
                    summary.error.as_deref().unwrap_or("unknown header error")
                ));
            }
        }
    }
    Ok(output)
}

fn show_report(reports_dir: &Path, requested: &Path) -> Result<String> {
    let report_dir = resolve_report(reports_dir, requested);
    let header = report::read_header(&report_dir)
        .with_context(|| format!("failed to read report {}", report_dir.display()))?;
    let mut output =
        serde_json::to_string_pretty(&header).context("failed to render report header")?;
    output.push('\n');
    Ok(output)
}

fn prune_reports(reports_dir: &Path) -> Result<String> {
    let summary = report::prune(reports_dir)
        .with_context(|| format!("failed to prune reports in {}", reports_dir.display()))?;
    let mut output = String::new();
    for path in &summary.removed {
        output.push_str(&format!("removed\t{}\n", path.display()));
    }
    output.push_str(&format!(
        "Removed {} automatic report(s) from {}.\n",
        summary.removed.len(),
        reports_dir.display()
    ));
    Ok(output)
}

fn resolve_report(reports_dir: &Path, requested: &Path) -> PathBuf {
    if requested.is_absolute() || requested.join("report.json").exists() {
        requested.to_path_buf()
    } else {
        reports_dir.join(requested)
    }
}

fn format_report_row(header: &ReportHeader, path: &Path) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\n",
        header.stamp,
        kind_name(header.kind),
        status_name(header.status),
        verdict_name(&header.replay),
        path.display()
    )
}

fn kind_name(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Bug => "bug",
        ReportKind::Tweak => "tweak",
        ReportKind::Tripwire => "tripwire",
        ReportKind::ChannelOverflow => "channel_overflow",
        ReportKind::Panic => "panic",
    }
}

fn status_name(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Open => "open",
        ReportStatus::Done => "done",
    }
}

fn verdict_name(verdict: &ReplayVerdict) -> &'static str {
    match verdict {
        ReplayVerdict::Unchecked => "unchecked",
        ReplayVerdict::Reproduces => "reproduces",
        ReplayVerdict::Diverges { .. } => "diverges",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use amux_tui::chrome::TraceEvent;
    use amux_tui::trace::{Snapshot, TraceWindow};
    use amux_tui::{Notice, Theme, ViewState};
    use amux_ui::report::{
        FrameCapture, Mark, PartState, Parts, REPORT_SCHEMA_VERSION, ReportDraft, ReportParts,
        ReportWriter,
    };
    use amux_ui::{BUILD, Model};
    use chrono::{TimeZone, Utc};
    use clap::Parser;
    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Parser)]
    struct DebugCli {
        #[command(subcommand)]
        command: DebugCommands,
    }

    fn config(reports_dir: &Path) -> Config {
        serde_yaml::from_str(&format!(
            "data_dir: /unused\nreports_dir: {}\n",
            reports_dir.display()
        ))
        .expect("parse test config")
    }

    fn header(stamp: &str, kind: ReportKind, status: ReportStatus) -> ReportHeader {
        ReportHeader {
            schema_version: REPORT_SCHEMA_VERSION,
            build: "debug".to_string(),
            git_sha: "abc1234".to_string(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            stamp: stamp.to_string(),
            kind,
            status,
            detail: None,
            note: "the note".to_string(),
            marks: vec![Mark {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
                note: "the mark".to_string(),
            }],
            viewport: Some((80, 24)),
            parts: Parts {
                frame: PartState::Absent {
                    reason: "test".to_string(),
                },
                trace: PartState::Absent {
                    reason: "test".to_string(),
                },
                msgs: PartState::Absent {
                    reason: "test".to_string(),
                },
                daemon: PartState::Absent {
                    reason: "test".to_string(),
                },
                log: PartState::Absent {
                    reason: "test".to_string(),
                },
            },
            replay: ReplayVerdict::Unchecked,
        }
    }

    fn write_header(root: &Path, name: &str, header: &ReportHeader) -> PathBuf {
        let report_dir = root.join(name);
        fs::create_dir_all(&report_dir).unwrap();
        fs::write(
            report_dir.join("report.json"),
            serde_json::to_vec_pretty(header).unwrap(),
        )
        .unwrap();
        report_dir
    }

    fn write_replayable_report(root: &Path) -> PathBuf {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let event = |event: TraceEvent| serde_json::to_string(&event).unwrap();
        let window = TraceWindow {
            snapshot: Snapshot {
                model: Model::default(),
                view: ViewState::default(),
                theme: Theme::default(),
                at: now,
            },
            events: vec![
                event(TraceEvent::Draw {
                    viewport: (80, 24),
                    now,
                }),
                event(TraceEvent::Notice(Some(Notice::done(
                    "the replay reached the later frame",
                )))),
                event(TraceEvent::Draw {
                    viewport: (80, 24),
                    now,
                }),
            ],
        };
        let report_dir = ReportWriter::new(root.to_path_buf(), BUILD, "test-sha")
            .write(
                ReportDraft {
                    kind: ReportKind::Bug,
                    detail: None,
                    note: "replay command test".to_string(),
                    marks: Vec::new(),
                    viewport: Some((80, 24)),
                    replay: ReplayVerdict::Unchecked,
                },
                ReportParts {
                    frame: Some(FrameCapture {
                        text: "placeholder\n".to_string(),
                        styles: "?\n".to_string(),
                    }),
                    trace: Some(window.to_bytes().unwrap()),
                    msgs: None,
                    daemon: None,
                    log: None,
                    absent_reason: "not captured by this test".to_string(),
                    log_absent_reason: None,
                    daemon_absent_reason: None,
                },
            )
            .unwrap();

        let mut replay = Replay::load(&report_dir).unwrap();
        replay.step_to_end().unwrap();
        let captured = replay.frame().unwrap();
        fs::write(report_dir.join("frame.txt"), captured.text).unwrap();
        fs::write(report_dir.join("frame.styles"), captured.styles).unwrap();
        report_dir
    }

    #[test]
    fn debug_tree_nests_daemon_and_report_commands() {
        let daemon =
            DebugCli::try_parse_from(["debug", "daemon", "--verbose", "--format", "json"]).unwrap();
        assert!(matches!(
            daemon.command,
            DebugCommands::Daemon {
                verbose: true,
                format: CliDebugFormat::Json
            }
        ));

        let report = DebugCli::try_parse_from(["debug", "report", "show", "capture-name"]).unwrap();
        assert!(matches!(
            report.command,
            DebugCommands::Report {
                command: ReportCommands::Show { report }
            } if report == Path::new("capture-name")
        ));

        assert!(DebugCli::try_parse_from(["debug", "--verbose"]).is_err());
    }

    #[test]
    fn report_list_and_show_use_the_configured_directory() {
        let temp = tempdir().unwrap();
        let reports_dir = temp.path().join("elsewhere");
        fs::create_dir_all(&reports_dir).unwrap();
        let older = header("1000", ReportKind::Bug, ReportStatus::Open);
        let newer = header("2000", ReportKind::Tweak, ReportStatus::Done);
        write_header(&reports_dir, "1000-bug", &older);
        write_header(&reports_dir, "2000-tweak", &newer);
        fs::create_dir(reports_dir.join("3000-broken")).unwrap();
        let config = config(&reports_dir);

        let listing = run_report(ReportCommands::List, &config).unwrap();
        assert_eq!(listing.exit_code, ExitCode::SUCCESS);
        let lines = listing.text.lines().collect::<Vec<_>>();
        assert!(lines[0].contains("3000-broken\tunreadable"));
        assert!(lines[1].contains("2000\ttweak\tdone\tunchecked"));
        assert!(lines[2].contains("1000\tbug\topen\tunchecked"));
        assert!(
            lines
                .iter()
                .all(|line| line.contains(&reports_dir.display().to_string()))
        );

        let shown = run_report(
            ReportCommands::Show {
                report: PathBuf::from("2000-tweak"),
            },
            &config,
        )
        .unwrap();
        assert_eq!(shown.exit_code, ExitCode::SUCCESS);
        assert_eq!(
            serde_json::from_str::<ReportHeader>(&shown.text).unwrap(),
            newer
        );
    }

    #[test]
    fn report_prune_removes_only_the_oldest_automatic_report() {
        let temp = tempdir().unwrap();
        let reports_dir = temp.path().join("reports");
        fs::create_dir_all(&reports_dir).unwrap();
        for index in 0..=amux_ui::report::RETAINED_AUTOMATIC_REPORTS {
            let stamp = format!("{index:04}");
            write_header(
                &reports_dir,
                &format!("{stamp}-tripwire"),
                &header(&stamp, ReportKind::Tripwire, ReportStatus::Open),
            );
        }
        write_header(
            &reports_dir,
            "user-bug",
            &header("user", ReportKind::Bug, ReportStatus::Open),
        );

        let output = run_report(ReportCommands::Prune, &config(&reports_dir)).unwrap();
        assert_eq!(output.exit_code, ExitCode::SUCCESS);
        assert!(output.text.contains("removed\t"));
        assert!(output.text.contains("0000-tripwire"));
        assert!(output.text.contains("Removed 1 automatic report(s)"));
        assert!(!reports_dir.join("0000-tripwire").exists());
        assert!(reports_dir.join("user-bug").exists());
    }

    #[test]
    fn missing_report_directory_lists_and_prunes_as_empty() {
        let temp = tempdir().unwrap();
        let reports_dir = temp.path().join("missing");
        let config = config(&reports_dir);

        assert!(
            run_report(ReportCommands::List, &config)
                .unwrap()
                .text
                .starts_with("No reports in ")
        );
        assert!(
            run_report(ReportCommands::Prune, &config)
                .unwrap()
                .text
                .contains("Removed 0 automatic report(s)")
        );
    }

    #[test]
    fn replay_cmd_verifies_renders_an_earlier_frame_and_flags_tampering() {
        let temp = tempdir().unwrap();
        let reports_dir = temp.path().join("reports");
        let report_dir = write_replayable_report(&reports_dir);
        let report_name = PathBuf::from(report_dir.file_name().unwrap());
        let config = config(&reports_dir);

        let verified = run_report(
            ReportCommands::Replay {
                report: report_name.clone(),
                at: None,
                frame: false,
                styles: false,
            },
            &config,
        )
        .unwrap();
        assert_eq!(verified.exit_code, ExitCode::SUCCESS);
        assert!(verified.text.starts_with("Reproduces\n"));
        assert!(verified.text.contains("Differing cells: none"));
        assert!(verified.text.contains("Bounding rectangle: none"));
        assert_eq!(
            report::read_header(&report_dir).unwrap().replay,
            ReplayVerdict::Reproduces
        );

        let earlier = run_report(
            ReportCommands::Replay {
                report: report_name.clone(),
                at: Some(0),
                frame: true,
                styles: false,
            },
            &config,
        )
        .unwrap();
        assert_eq!(earlier.exit_code, ExitCode::SUCCESS);
        assert!(earlier.text.contains("Frame at event 0:"));
        assert!(!earlier.text.contains("the replay reached the later frame"));

        let captured = fs::read_to_string(report_dir.join("frame.txt")).unwrap();
        let mut chars = captured.chars();
        let _ = chars.next().expect("captured frame has a first cell");
        fs::write(
            report_dir.join("frame.txt"),
            format!("\u{2588}{}", chars.collect::<String>()),
        )
        .unwrap();

        let diverged = run_report(
            ReportCommands::Replay {
                report: report_name,
                at: None,
                frame: false,
                styles: false,
            },
            &config,
        )
        .unwrap();
        assert_eq!(diverged.exit_code, ExitCode::FAILURE);
        assert!(diverged.text.starts_with("Diverges:"));
        assert!(diverged.text.contains("Differing cells: (0,0)"));
        assert!(
            diverged
                .text
                .contains("Bounding rectangle: x=0 y=0 width=1 height=1")
        );
        assert!(matches!(
            report::read_header(&report_dir).unwrap().replay,
            ReplayVerdict::Diverges { .. }
        ));
    }
}
