use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use amux::Config;
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::server_client;

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
        let marker_path = amux::update::update_marker_path(&config.state_path);
        amux::update::clear_update_marker(&marker_path);
        amux::update::clear_update_required(&config.state_path);
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
    let marker_path = amux::update::update_marker_path(&config.state_path);
    amux::update::clear_update_marker(&marker_path);
    amux::update::clear_update_required(&config.state_path);

    if was_running {
        println!("Restarting server...");
        server_client::resume_server_with_executable(config, &current_exe).await?;
    }

    Ok(())
}
