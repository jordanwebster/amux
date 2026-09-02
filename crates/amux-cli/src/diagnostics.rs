//! The diagnostics the shell hands the TUI for report capture.
//!
//! The TUI cannot reach the daemon, the log file or the build's commit on
//! its own, so the CLI resolves all three here and passes them in. Only
//! debug builds get a source: without one the TUI has nothing to capture
//! into a report, which is how the capture key stays out of release builds.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use amux::Config;
use amux_tui::DiagnosticsSource;

/// Build the source the TUI captures reports from, or `None` when this is
/// not a debug build. `daemon_dump` is the shell's fetcher — it runs at the
/// moment of capture, so the dump reflects the daemon's state then rather
/// than at startup.
pub fn source<F, Fut>(
    config: &Config,
    git_sha: &'static str,
    debug_build: bool,
    daemon_dump: F,
) -> Option<DiagnosticsSource>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    if !debug_build {
        return None;
    }
    let fetch = Arc::new(daemon_dump);
    Some(DiagnosticsSource {
        daemon_dump: Arc::new(move || {
            let fetch = Arc::clone(&fetch);
            Box::pin(async move { fetch().await })
        }),
        log_path: Some(resolved_log_path()),
        reports_dir: config.reports_dir(),
        git_sha,
    })
}

/// The file this process logs to: `AMUX_LOG` when set, else the shared
/// default the daemon writes to as well. Resolved in one place so the
/// runtime and a captured report's log tail can never disagree.
pub fn resolved_log_path() -> PathBuf {
    std::env::var_os("AMUX_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(amux::default_log_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        serde_yaml::from_str("data_dir: /srv/amux-dev/data\n").expect("parse test config")
    }

    #[tokio::test]
    async fn source_fetches_the_dump_through_the_shell_closure() {
        let source = source(&config(), "abc1234", true, || async {
            Ok(r#"{"agents":[]}"#.to_string())
        })
        .expect("debug builds get a diagnostics source");

        assert_eq!(
            (source.daemon_dump)().await,
            Ok(r#"{"agents":[]}"#.to_string())
        );
        assert_eq!(source.git_sha, "abc1234");
        assert_eq!(source.reports_dir, config().reports_dir());
        assert_eq!(source.log_path, Some(resolved_log_path()));
    }

    #[tokio::test]
    async fn source_surfaces_a_failed_dump_as_its_reason() {
        let source = source(&config(), "abc1234", true, || async {
            Err("No server running".to_string())
        })
        .expect("debug builds get a diagnostics source");

        assert_eq!(
            (source.daemon_dump)().await,
            Err("No server running".to_string())
        );
    }

    #[test]
    fn release_builds_have_no_diagnostics_source() {
        assert!(
            source(&config(), "abc1234", false, || async { Ok(String::new()) }).is_none(),
            "a release build must not carry a diagnostics source"
        );
    }
}
