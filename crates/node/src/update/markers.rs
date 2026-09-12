//! Persistent status shared by desktop profile runtimes and local clients.

use std::path::{Path, PathBuf};

use semver::Version;

use crate::{SubscriptionReporter, UpdateInfo, UpdateReporter, UpdateStatus};

#[derive(Debug, Clone)]
pub struct MarkerFileReporter {
    state_dir: PathBuf,
}

impl MarkerFileReporter {
    pub fn from_state_path(state_path: &Path) -> Self {
        Self {
            state_dir: state_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        }
    }

    pub fn read_update_marker(&self) -> Option<UpdateInfo> {
        let contents = std::fs::read_to_string(self.update_marker_path()).ok()?;
        let mut lines = contents.lines();
        let current_version = lines.next()?.to_string();
        let update_version = lines.next()?.to_string();
        if current_version.is_empty() || update_version.is_empty() {
            return None;
        }
        Some(UpdateInfo {
            current_version,
            update_version,
        })
    }

    pub fn read_update_required(&self) -> Option<String> {
        let contents = std::fs::read_to_string(self.update_required_marker_path()).ok()?;
        let version = contents.trim().to_string();
        if version.is_empty() {
            None
        } else {
            Some(version)
        }
    }

    pub fn read_active_update_required(&self, current_version: &str) -> Option<String> {
        let minimum_version = self.read_update_required()?;
        if current_satisfies_minimum(current_version, &minimum_version) {
            self.clear_update_required();
            None
        } else {
            Some(minimum_version)
        }
    }

    pub fn subscription_required(&self) -> bool {
        self.subscription_required_marker_path().is_file()
    }

    pub fn is_update_dismissed(&self, minimum_version: &str) -> bool {
        match std::fs::read_to_string(self.update_dismissed_marker_path()) {
            Ok(contents) => contents.trim() == minimum_version,
            Err(_) => false,
        }
    }

    pub fn dismiss_update_required(&self, minimum_version: &str) {
        let _ = std::fs::write(
            self.update_dismissed_marker_path(),
            format!("{minimum_version}\n"),
        );
    }

    pub fn clear_update_marker(&self) {
        let _ = std::fs::remove_file(self.update_marker_path());
    }

    pub fn clear_update_required(&self) {
        let _ = std::fs::remove_file(self.update_required_marker_path());
        let _ = std::fs::remove_file(self.update_dismissed_marker_path());
    }

    pub fn clear_subscription_required(&self) {
        let _ = std::fs::remove_file(self.subscription_required_marker_path());
    }

    pub fn clear_all(&self) {
        self.clear_update_marker();
        self.clear_update_required();
        self.clear_subscription_required();
    }

    pub fn update_marker_path(&self) -> PathBuf {
        self.state_dir.join("update-available")
    }

    fn update_required_marker_path(&self) -> PathBuf {
        self.state_dir.join("update-required")
    }

    fn update_dismissed_marker_path(&self) -> PathBuf {
        self.state_dir.join("update-dismissed")
    }

    fn subscription_required_marker_path(&self) -> PathBuf {
        self.state_dir.join("subscription-required")
    }

    fn write_update_marker(&self, info: &UpdateInfo) {
        let contents = format!("{}\n{}\n", info.current_version, info.update_version);
        if let Err(e) = std::fs::write(self.update_marker_path(), contents) {
            tracing::warn!(error = %e, "failed to write update marker");
        }
    }

    fn write_update_required(&self, minimum_version: &str) {
        if let Err(e) = std::fs::write(
            self.update_required_marker_path(),
            format!("{minimum_version}\n"),
        ) {
            tracing::warn!(error = %e, "failed to write update-required marker");
        }
    }

    fn write_subscription_required(&self) {
        if let Err(e) = std::fs::write(self.subscription_required_marker_path(), b"required\n") {
            tracing::warn!(error = %e, "failed to write subscription-required marker");
        }
    }
}

fn current_satisfies_minimum(current_version: &str, minimum_version: &str) -> bool {
    match (
        Version::parse(current_version),
        Version::parse(minimum_version),
    ) {
        (Ok(current), Ok(minimum)) => current >= minimum,
        _ => false,
    }
}

impl UpdateReporter for MarkerFileReporter {
    fn report(&self, status: UpdateStatus) {
        match status {
            UpdateStatus::Available(Some(info)) => self.write_update_marker(&info),
            UpdateStatus::Available(None) => self.clear_update_marker(),
            UpdateStatus::Required(Some(minimum_version)) => {
                self.write_update_required(&minimum_version);
            }
            UpdateStatus::Required(None) => self.clear_update_required(),
        }
    }
}

impl SubscriptionReporter for MarkerFileReporter {
    fn report_subscription_required(&self, required: bool) {
        if required {
            self.write_subscription_required();
        } else {
            self.clear_subscription_required();
        }
    }
}
