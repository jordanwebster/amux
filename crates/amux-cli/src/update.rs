use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use amux::{Config, SubscriptionReporter, UpdateInfo, UpdateReporter, UpdateStatus};
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::server_client;

#[derive(Debug, Clone)]
pub(crate) struct MarkerFileReporter {
    state_dir: PathBuf,
}

impl MarkerFileReporter {
    pub(crate) fn from_state_path(state_path: &Path) -> Self {
        Self {
            state_dir: state_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        }
    }

    pub(crate) fn read_update_marker(&self) -> Option<UpdateInfo> {
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

    pub(crate) fn read_update_required(&self) -> Option<String> {
        let contents = std::fs::read_to_string(self.update_required_marker_path()).ok()?;
        let version = contents.trim().to_string();
        if version.is_empty() {
            None
        } else {
            Some(version)
        }
    }

    pub(crate) fn read_active_update_required(&self, current_version: &str) -> Option<String> {
        let minimum_version = self.read_update_required()?;
        if current_satisfies_minimum(current_version, &minimum_version) {
            self.clear_update_required();
            None
        } else {
            Some(minimum_version)
        }
    }

    pub(crate) fn subscription_required(&self) -> bool {
        self.subscription_required_marker_path().is_file()
    }

    pub(crate) fn is_update_dismissed(&self, minimum_version: &str) -> bool {
        match std::fs::read_to_string(self.update_dismissed_marker_path()) {
            Ok(contents) => contents.trim() == minimum_version,
            Err(_) => false,
        }
    }

    pub(crate) fn dismiss_update_required(&self, minimum_version: &str) {
        let _ = std::fs::write(
            self.update_dismissed_marker_path(),
            format!("{minimum_version}\n"),
        );
    }

    pub(crate) fn clear_update_marker(&self) {
        let _ = std::fs::remove_file(self.update_marker_path());
    }

    pub(crate) fn clear_update_required(&self) {
        let _ = std::fs::remove_file(self.update_required_marker_path());
        let _ = std::fs::remove_file(self.update_dismissed_marker_path());
    }

    pub(crate) fn clear_subscription_required(&self) {
        let _ = std::fs::remove_file(self.subscription_required_marker_path());
    }

    pub(crate) fn clear_all(&self) {
        self.clear_update_marker();
        self.clear_update_required();
        self.clear_subscription_required();
    }

    pub(crate) fn update_marker_path(&self) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_marker_round_trips_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        let reporter = MarkerFileReporter::from_state_path(&temp.path().join("state.yaml"));
        let info = UpdateInfo {
            current_version: "0.3.0".to_string(),
            update_version: "0.4.0".to_string(),
        };

        reporter.report(UpdateStatus::Available(Some(info.clone())));

        let marker = reporter.read_update_marker().unwrap();
        assert_eq!(marker.current_version, info.current_version);
        assert_eq!(marker.update_version, info.update_version);

        reporter.report(UpdateStatus::Available(None));

        assert!(reporter.read_update_marker().is_none());
        assert!(!reporter.update_marker_path().exists());
    }

    #[test]
    fn required_marker_dismissal_round_trips_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        let reporter = MarkerFileReporter::from_state_path(&temp.path().join("state.yaml"));

        reporter.report(UpdateStatus::Required(Some("0.4.0".to_string())));

        assert_eq!(reporter.read_update_required().as_deref(), Some("0.4.0"));
        assert!(!reporter.is_update_dismissed("0.4.0"));

        reporter.dismiss_update_required("0.4.0");

        assert!(reporter.is_update_dismissed("0.4.0"));
        assert!(!reporter.is_update_dismissed("0.5.0"));

        reporter.report(UpdateStatus::Required(None));

        assert_eq!(reporter.read_update_required(), None);
        assert!(!reporter.is_update_dismissed("0.4.0"));
    }

    #[tokio::test]
    async fn profile_runtime_local_lifecycle_preserves_update_markers() {
        use std::sync::Arc;
        use std::time::Duration;

        tokio::time::timeout(Duration::from_secs(5), async {
            let temp = tempfile::tempdir().unwrap();
            let config = Config {
                state_path: temp.path().join("state.yaml"),
                data_dir: temp.path().join("data"),
                socket_path: temp.path().join("amux.sock"),

                prevent_idle_sleep: Some(false),
                ..Config::default()
            };
            let reporter = Arc::new(MarkerFileReporter::from_state_path(&config.state_path));
            reporter.report(UpdateStatus::Required(Some("99.0.0".into())));
            reporter.dismiss_update_required("99.0.0");
            reporter.report_subscription_required(true);

            let client = amux::Server::builder()
                .config(config)
                .update_reporter(reporter.clone())
                .subscription_reporter(reporter.clone())
                .embedded()
                .open()
                .await
                .unwrap();
            client.list_agents().await.unwrap();
            assert_eq!(reporter.read_update_required().as_deref(), Some("99.0.0"));
            assert!(reporter.is_update_dismissed("99.0.0"));
            assert!(!reporter.subscription_required());
            println!("Runtime started: update-required=99.0.0, update-dismissed=99.0.0, subscription-required absent");

            reporter.report_subscription_required(true);
            client.shutdown().await.unwrap();

            assert_eq!(reporter.read_update_required().as_deref(), Some("99.0.0"));
            assert!(reporter.is_update_dismissed("99.0.0"));
            assert!(!reporter.subscription_required());
            println!("Shutdown acknowledged: update-required=99.0.0, update-dismissed=99.0.0, subscription-required absent");
        })
        .await
        .expect("runtime marker test timed out");
    }

    #[test]
    fn active_required_marker_clears_when_current_satisfies_minimum() {
        let temp = tempfile::tempdir().unwrap();
        let reporter = MarkerFileReporter::from_state_path(&temp.path().join("state.yaml"));

        reporter.report(UpdateStatus::Required(Some("0.4.0".to_string())));
        reporter.dismiss_update_required("0.4.0");

        assert_eq!(reporter.read_active_update_required("0.4.0"), None);
        assert_eq!(reporter.read_update_required(), None);
        assert!(!reporter.is_update_dismissed("0.4.0"));
    }

    #[test]
    fn active_required_marker_retains_when_current_is_below_minimum() {
        let temp = tempfile::tempdir().unwrap();
        let reporter = MarkerFileReporter::from_state_path(&temp.path().join("state.yaml"));

        reporter.report(UpdateStatus::Required(Some("0.4.0".to_string())));

        assert_eq!(
            reporter.read_active_update_required("0.3.0").as_deref(),
            Some("0.4.0")
        );
        assert_eq!(reporter.read_update_required().as_deref(), Some("0.4.0"));
    }

    #[test]
    fn subscription_marker_round_trips_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        let reporter = MarkerFileReporter::from_state_path(&temp.path().join("state.yaml"));

        reporter.report_subscription_required(true);
        assert!(reporter.subscription_required());

        reporter.report_subscription_required(false);
        assert!(!reporter.subscription_required());
    }
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    release_notes: String,
    platforms: HashMap<String, PlatformBinary>,
}

#[derive(Deserialize)]
struct PlatformBinary {
    url: String,
    sha256: String,
}

fn platform_key() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-arm64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-arm64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-arm64"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
    )))]
    {
        compile_error!("unsupported platform for update")
    }
}

async fn fetch_manifest(url: &str) -> Result<Manifest> {
    let resp = reqwest::get(url)
        .await
        .context("failed to fetch manifest")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("manifest request failed with status {status}");
    }
    resp.json().await.context("failed to parse manifest")
}

async fn download_and_verify(url: &str, expected_sha256: &str, exe_dir: &Path) -> Result<PathBuf> {
    let resp = reqwest::get(url)
        .await
        .with_context(|| format!("failed to download binary from {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("binary download failed with status {status}");
    }
    let bytes = resp.bytes().await.context("failed to read binary body")?;

    let hash = Sha256::digest(&bytes);
    let actual_sha256 = format!("{hash:x}");
    if actual_sha256 != expected_sha256 {
        bail!("SHA256 mismatch: expected {expected_sha256}, got {actual_sha256}");
    }

    let tmp_path = exe_dir.join(".amux-update.tmp");
    std::fs::write(&tmp_path, &bytes).context("failed to write temp binary")?;
    #[cfg(unix)]
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
        .context("failed to set temp binary permissions")?;

    Ok(tmp_path)
}

fn replace_binary(temp: &Path, target: &Path) -> Result<()> {
    std::fs::rename(temp, target).context("failed to replace binary")
}

pub async fn run_update(config: &Config) -> Result<()> {
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).context("failed to parse current version")?;

    let manifest_url = format!("{}/manifest.json", config.cloud_url.trim_end_matches('/'));
    println!("Checking for updates...");
    let manifest = fetch_manifest(&manifest_url).await?;

    let latest = Version::parse(&manifest.version)
        .with_context(|| format!("invalid version in manifest: {}", manifest.version))?;

    if latest <= current {
        println!("Already up to date (v{current}).");
        MarkerFileReporter::from_state_path(&config.state_path).clear_all();
        return Ok(());
    }

    println!("Update available: v{current} -> v{latest}");
    if !manifest.release_notes.is_empty() {
        println!("{}", manifest.release_notes);
    }

    let platform = platform_key();
    let binary = manifest
        .platforms
        .get(platform)
        .with_context(|| format!("no binary available for platform {platform}"))?;

    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;
    let exe_dir = current_exe
        .parent()
        .context("executable has no parent directory")?
        .to_path_buf();

    println!("Downloading...");
    let tmp_path = download_and_verify(&binary.url, &binary.sha256, &exe_dir).await?;

    let was_running = server_client::suspend_server_for_update_if_running(config).await?;

    replace_binary(&tmp_path, &current_exe)?;
    println!("Updated to v{latest}.");

    // Clear update markers so stale banners don't persist.
    MarkerFileReporter::from_state_path(&config.state_path).clear_all();

    if was_running {
        println!("Restarting server...");
        server_client::resume_server_with_executable(config, &current_exe).await?;
    }

    Ok(())
}
