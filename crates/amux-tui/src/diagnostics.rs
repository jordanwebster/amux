//! What a captured report needs from outside the TUI.
//!
//! The TUI consumes `amux-ui` exclusively and never touches `amux::Client`,
//! so it cannot ask the daemon for its state, resolve the log path, or know
//! which commit it was built from. The embedding shell knows all three and
//! hands them over as this source. Fetching the dump is a closure rather
//! than a captured string because the daemon's state matters at the moment
//! of capture, not at startup.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

/// Asks the daemon for its debug dump. `Err` carries a human-readable
/// reason, which the report records in place of the dump — a report is
/// still worth writing when the daemon is gone.
pub type DaemonDump = Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// The shell-owned facts a report bundle carries beyond the TUI's own
/// state. Absent entirely in a build that captures no reports.
#[derive(Clone)]
pub struct DiagnosticsSource {
    pub daemon_dump: DaemonDump,
    /// The log file a report tails, when this build logs to one.
    pub log_path: Option<PathBuf>,
    /// Where written bundles land.
    pub reports_dir: PathBuf,
    /// The commit the running binary was built from, for the report header.
    pub git_sha: &'static str,
}

impl std::fmt::Debug for DiagnosticsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagnosticsSource")
            .field("log_path", &self.log_path)
            .field("reports_dir", &self.reports_dir)
            .field("git_sha", &self.git_sha)
            .finish_non_exhaustive()
    }
}
