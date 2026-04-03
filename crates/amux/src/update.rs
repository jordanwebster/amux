//! Update checking: fetches the release manifest and compares versions.
//!
//! The server spawns a periodic task that calls [`check_for_update`] and writes
//! the result to a marker file next to `state.yaml`. Clients read this file to
//! decide whether to show an upgrade banner.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Result of an update check.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub update_version: String,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
}

async fn fetch_manifest(url: &str) -> Result<Manifest, reqwest::Error> {
    let resp = reqwest::get(url).await?.error_for_status()?;
    resp.json().await
}

/// Check the remote manifest for a newer version. Returns `Some(UpdateInfo)` if
/// the manifest version is strictly greater than `current_version`.
pub async fn check_for_update(cloud_url: &str, current_version: &str) -> Option<UpdateInfo> {
    let manifest_url = format!("{}/manifest.json", cloud_url.trim_end_matches('/'));

    let manifest = match fetch_manifest(&manifest_url).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(error = %e, "update check failed");
            return None;
        }
    };

    let current = semver::Version::parse(current_version).ok()?;
    let latest = semver::Version::parse(&manifest.version).ok()?;

    if latest > current {
        Some(UpdateInfo {
            current_version: current_version.to_string(),
            update_version: manifest.version,
        })
    } else {
        None
    }
}

/// Derive the marker file path from the state file path.
/// e.g. `~/.local/state/amux/state.yaml` → `~/.local/state/amux/update-available`
pub fn update_marker_path(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("update-available")
}

/// Read the update marker file. Returns `None` if the file doesn't exist or
/// can't be parsed.
pub fn read_update_marker(marker_path: &Path) -> Option<UpdateInfo> {
    let contents = std::fs::read_to_string(marker_path).ok()?;
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

/// Write the update marker file atomically.
fn write_update_marker(marker_path: &Path, info: &UpdateInfo) {
    let contents = format!("{}\n{}\n", info.current_version, info.update_version);
    if let Err(e) = std::fs::write(marker_path, contents) {
        tracing::warn!(error = %e, "failed to write update marker");
    }
}

/// Delete the update marker file (called after successful update or when stale).
pub fn clear_update_marker(marker_path: &Path) {
    let _ = std::fs::remove_file(marker_path);
}

/// Spawn a background task that checks for updates periodically and writes the
/// result to a marker file. If no update is found, the marker is removed.
pub fn spawn_update_checker(
    cloud_url: String,
    current_version: String,
    interval: std::time::Duration,
    state_path: PathBuf,
) {
    let marker_path = update_marker_path(&state_path);
    tokio::spawn(async move {
        loop {
            match check_for_update(&cloud_url, &current_version).await {
                Some(info) => {
                    tracing::info!(
                        current = %info.current_version,
                        latest = %info.update_version,
                        "update available"
                    );
                    write_update_marker(&marker_path, &info);
                }
                None => {
                    clear_update_marker(&marker_path);
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}
