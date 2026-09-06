use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use amux::InstallationConfig;
pub(crate) use amux::update::MarkerFileReporter;
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::front_door;

#[cfg(test)]
mod tests {
    use amux::{SubscriptionReporter, UpdateInfo, UpdateReporter, UpdateStatus};

    use super::*;

    async fn manifest_fixture(
        route: &'static str,
        version: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, version) = if path == route {
                    ("200 OK", version)
                } else if path == "/current.json" {
                    ("200 OK", env!("CARGO_PKG_VERSION"))
                } else {
                    ("404 Not Found", "0.0.0")
                };
                let body =
                    format!(r#"{{"version":"{version}","release_notes":"","platforms":{{}}}}"#);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (url, server)
    }

    #[tokio::test]
    async fn desktop_installation_markers_reach_cli_and_clear_on_update() {
        use amux::installation::{
            Installation, InstallationRoot, Observed, ProfileId, ProfileLabel, ProfilePaths,
            Registry,
        };
        use amux::test_fixtures::report_profile_status;

        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let temp = amux::test_fixtures::short_installation_root();
            let root = std::fs::canonicalize(temp.path()).unwrap();
            let (cloud_url, cloud_server) = manifest_fixture("/manifest.json", "100.0.0").await;
            let (update_url, manifest_server) = manifest_fixture("/releases/desktop.json", "99.0.0").await;
            let mut config = InstallationConfig {
                root: root.clone(),
                path: Some(root.join("config.yaml")),
                front_door_socket: root.join("amux.sock"),
                keymaps_dir: root.join("keymaps"),
                prevent_idle_sleep: Some(false),
                update_manifest_url: format!("{update_url}/releases/desktop.json"),
                ..InstallationConfig::default()
            };
            std::fs::write(config.path.as_ref().unwrap(), serde_yaml::to_string(&config).unwrap()).unwrap();
            let ids = [ProfileId::new(), ProfileId::new()];
            let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone())).unwrap();
            let mut readers = Vec::new();
            for id in ids {
                registry.create(id, ProfileLabel::default()).unwrap();
                let paths = ProfilePaths::for_id(&root, id).unwrap();
                let profile = amux::ProfileConfig {
                    installation_config: config.path.clone().unwrap(),
                    socket_path: paths.socket_path,
                    data_dir: paths.data_dir,
                    state_path: paths.state_path,
                    cloud_url: cloud_url.clone(),
                    tcp_port: None,
                };
                let path = paths.config_path.unwrap();
                std::fs::write(&path, serde_yaml::to_string(&profile).unwrap()).unwrap();
                // Resolve exactly the config path used by CLI selection and TUI startup.
                let resolved = amux::load_profile_config(&path).unwrap();
                readers.push(MarkerFileReporter::from_state_path(&resolved.profile.state_path));
            }
            drop(registry);
            let owner = Installation::from_config(config.clone()).await.unwrap();
            for id in ids {
                owner.client(id).unwrap().list_agents().await.unwrap();
            }
            while readers.iter().any(|reader| reader.read_update_marker().is_none()) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            for (id, reader) in ids.iter().zip(&readers) {
                let marker = reader.read_update_marker().unwrap();
                assert_eq!(marker.update_version, "99.0.0", "profile {id} used its cloud URL instead of the installation update source");
                assert_eq!(marker.current_version, env!("CARGO_PKG_VERSION"));
                let selected = amux::load_profile_config(
                    &root.join("profiles").join(id.to_string()).join("config.yaml"),
                ).unwrap();
                crate::client_common::print_update_banner(&selected.profile.state_path);
            }
            println!("Both desktop profiles show installation release 99.0.0; the separate cloud server advertises 100.0.0.");
            report_profile_status(&owner, ids[0], Observed::UpdateRequired { minimum_version: Some("99.0.0".into()) }).await;
            report_profile_status(&owner, ids[0], Observed::SubscriptionRequired).await;
            report_profile_status(&owner, ids[1], Observed::Connected).await;
            assert_eq!(readers[0].read_active_update_required(env!("CARGO_PKG_VERSION")).as_deref(), Some("99.0.0"));
            assert!(readers[0].subscription_required());
            assert!(readers[1].read_update_required().is_none());
            assert!(!readers[1].subscription_required());
            let selected = amux::load_profile_config(
                &root.join("profiles").join(ids[0].to_string()).join("config.yaml"),
            ).unwrap();
            crate::client_common::print_update_banner(&selected.profile.state_path);
            println!("Desktop profile A: CLI reads update-required=99.0.0 and TUI reads subscription-required; profile B connecting leaves both intact.");

            report_profile_status(&owner, ids[1], Observed::UpdateRequired { minimum_version: Some("98.0.0".into()) }).await;
            report_profile_status(&owner, ids[1], Observed::SubscriptionRequired).await;
            for (reader, version) in readers.iter().zip(["99.0.0", "98.0.0"]) {
                reader.dismiss_update_required(version);
            }
            // Serve the current version to exercise cleanup without replacing this test executable.
            config.update_manifest_url = format!("{update_url}/current.json");
            run_update(&config).await.unwrap();
            for (reader, version) in readers.iter().zip(["99.0.0", "98.0.0"]) {
                assert!(reader.read_update_marker().is_none());
                assert!(reader.read_update_required().is_none());
                assert!(!reader.is_update_dismissed(version));
                assert!(!reader.subscription_required());
            }
            println!("amux update (already current): update-required, update-dismissed and subscription-required clear in both profile state directories.");
            owner.shutdown(amux::ShutdownReason::UserRequested).await;

            for reader in &readers {
                reader.report(UpdateStatus::Required(Some("99.0.0".into())));
                reader.report_subscription_required(true);
            }
            run_update(&config).await.unwrap();
            for reader in &readers {
                assert!(reader.read_update_required().is_none());
                assert!(!reader.subscription_required());
            }
            println!("amux update also clears both profiles while the installation is stopped.");
            assert!(!root.join("state/state.yaml").exists());
            manifest_server.abort();
            cloud_server.abort();
        }).await.expect("desktop marker regression timed out");
    }

    #[tokio::test]
    async fn replacement_failure_preserves_binary_and_runs_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("amux");
        std::fs::write(&executable, b"previous executable").unwrap();
        let mut recovered = false;
        let error = replace_or_recover(
            &directory.path().join("missing-download"),
            &executable,
            async {
                assert_eq!(std::fs::read(&executable).unwrap(), b"previous executable");
                recovered = true;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(recovered);
        assert!(error.to_string().contains("failed to replace binary"));
        assert_eq!(std::fs::read(&executable).unwrap(), b"previous executable");
    }

    #[tokio::test]
    async fn replacement_success_does_not_run_failure_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("amux");
        let download = directory.path().join("download");
        std::fs::write(&executable, b"previous executable").unwrap();
        std::fs::write(&download, b"new executable").unwrap();
        replace_or_recover(&download, &executable, async {
            panic!("recovery must only run when replacement fails")
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&executable).unwrap(), b"new executable");
    }

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
            let config = amux::Config {
                state_path: temp.path().join("state.yaml"),
                data_dir: temp.path().join("data"),
                socket_path: temp.path().join("amux.sock"),

                prevent_idle_sleep: Some(false),
                ..amux::Config::default()
            };
            let reporter = Arc::new(MarkerFileReporter::from_state_path(&config.state_path));
            reporter.report(UpdateStatus::Required(Some("99.0.0".into())));
            reporter.dismiss_update_required("99.0.0");
            reporter.report_subscription_required(true);

            let root = amux::test_fixtures::short_installation_root();
            let installation = amux::Installation::open(amux::InstallationOptions {
                root: amux::InstallationRoot::OnDisk(root.path().into()),
                settings: amux::InstallationSettings {
                    host_name: config.host_name,
                    prevent_idle_sleep: Some(false),
                    keybinds: config.keybinds,
                    ui: config.ui,
                    keymaps_dir: temp.path().join("keymaps"),
                    minimum_client_versions: Default::default(),
                    update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
                    status_reporters: amux::update::StatusReporters::Host {
                        update: Some(reporter.clone()), subscription: Some(reporter.clone()),
                    },
                },
                listeners: amux::Listeners::InProcessOnly,
                credentials: amux::CredentialSource::ProfileFiles,
                identity_http: Default::default(),
            }).await.unwrap();
            let id = installation.create(amux::OperationId::new(), None).await.unwrap().record.id;
            let client = installation.client(id).unwrap();
            client.list_agents().await.unwrap();
            assert_eq!(reporter.read_update_required().as_deref(), Some("99.0.0"));
            assert!(reporter.is_update_dismissed("99.0.0"));
            assert!(!reporter.subscription_required());
            println!("Runtime started: update-required=99.0.0, update-dismissed=99.0.0, subscription-required absent");

            reporter.report_subscription_required(true);
            drop(client);
            installation.shutdown(amux::ShutdownReason::UserRequested).await;
            while reporter.subscription_required() {
                tokio::task::yield_now().await;
            }

            assert_eq!(reporter.read_update_required().as_deref(), Some("99.0.0"));
            assert!(reporter.is_update_dismissed("99.0.0"));
            assert!(!reporter.subscription_required());
            println!("Embedded owner stopped: update-required=99.0.0, update-dismissed=99.0.0, subscription-required absent");
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

async fn replace_or_recover(
    temp: &Path,
    target: &Path,
    recovery: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    if let Err(error) = replace_binary(temp, target) {
        recovery
            .await
            .with_context(|| format!("{error:#}; the previous server could not resume"))?;
        return Err(error);
    }
    Ok(())
}

fn clear_profile_markers(config: &InstallationConfig) -> Result<()> {
    let profiles = match std::fs::read_dir(config.root.join("profiles")) {
        Ok(profiles) => profiles,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to enumerate profile markers"),
    };
    for entry in profiles {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || entry
                .file_name()
                .to_str()
                .and_then(|name| uuid::Uuid::parse_str(name).ok())
                .is_none()
        {
            continue;
        }
        let path = entry.path().join("config.yaml");
        if !path.exists() {
            continue;
        }
        let resolved = amux::load_profile_config(&path)?;
        MarkerFileReporter::from_state_path(&resolved.profile.state_path).clear_all();
    }
    Ok(())
}

pub async fn run_update(config: &InstallationConfig) -> Result<()> {
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).context("failed to parse current version")?;

    let manifest_url = &config.update_manifest_url;
    println!("Checking for updates...");
    let manifest = fetch_manifest(manifest_url).await?;

    let latest = Version::parse(&manifest.version)
        .with_context(|| format!("invalid version in manifest: {}", manifest.version))?;

    if latest <= current {
        println!("Already up to date (v{current}).");
        clear_profile_markers(config)?;
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

    let was_running = front_door::suspend_for_update_if_running(config).await?;

    replace_or_recover(&tmp_path, &current_exe, async {
        if was_running {
            front_door::resume_with_executable(config, &current_exe).await?;
        }
        Ok(())
    })
    .await?;
    println!("Updated to v{latest}.");

    // A marker cleanup error must not leave the updated daemon stopped.
    let markers_cleared = clear_profile_markers(config);

    if was_running {
        println!("Restarting server...");
        front_door::resume_with_executable(config, &current_exe).await?;
    }

    markers_cleared
}
