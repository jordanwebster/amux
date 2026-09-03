//! Debug-only command tree and local report operations.
//!
//! This module is absent from release builds along with the CLI surface that
//! reaches it. The report bundle itself remains in `amux-ui` so release builds
//! can still write degraded tripwire and panic reports.

use std::io;
use std::path::{Path, PathBuf};

use amux::{Config, DebugFormat};
use amux_ui::report::{self, ReplayVerdict, ReportHeader, ReportKind, ReportStatus};
use anyhow::{Context, Result};
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

/// Run a report command against the single directory selected by the config.
///
/// Returning text keeps command behavior testable without redirecting the
/// process-wide stdout stream; the binary owns the final print.
pub fn run_report(command: ReportCommands, config: &Config) -> Result<String> {
    let reports_dir = config.reports_dir();
    match command {
        ReportCommands::List => list_reports(&reports_dir),
        ReportCommands::Show { report } => show_report(&reports_dir, &report),
        ReportCommands::Prune => prune_reports(&reports_dir),
    }
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

    use amux_ui::report::{Mark, PartState, Parts, REPORT_SCHEMA_VERSION};
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
        let lines = listing.lines().collect::<Vec<_>>();
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
        assert_eq!(serde_json::from_str::<ReportHeader>(&shown).unwrap(), newer);
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
        assert!(output.contains("removed\t"));
        assert!(output.contains("0000-tripwire"));
        assert!(output.contains("Removed 1 automatic report(s)"));
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
                .starts_with("No reports in ")
        );
        assert!(
            run_report(ReportCommands::Prune, &config)
                .unwrap()
                .contains("Removed 0 automatic report(s)")
        );
    }
}
