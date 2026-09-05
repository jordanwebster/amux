//! Resolve a command's profile from the installation directory.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use amux::installation::{FrontDoorClient, rpc};
use amux::{Config, InstallationConfig};
use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum ProfileCommands {
    /// Create an unbound device profile
    Create { name: Option<String> },
    /// Destroy a profile's keys, trust, agents and credentials
    Delete {
        #[arg(long)]
        yes: bool,
    },
    /// Override the account label, or restore it with --clear
    Rename {
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Disconnect from the cloud, retaining credentials and local agents
    Pause,
    /// Reconnect a paused profile
    Resume,
}

pub fn last_used(config: &InstallationConfig) -> PathBuf {
    config.root.join("state/last-profile")
}

pub fn remember(path: &Path, id: &str) -> Result<()> {
    let id = Uuid::parse_str(id).context("front door returned an invalid profile UUID")?;
    let parent = path.parent().context("last-profile path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".last-profile.{}", Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&temporary, format!("{id}\n"))?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result.context("cannot remember selected profile")
}

/// Labels remain selectable without their collision suffix so ambiguity is
/// diagnosed with full UUIDs. The displayed labels can also be copied verbatim.
pub fn display_label(info: &rpc::ProfileInfo, directory: &[rpc::ProfileInfo]) -> String {
    if directory.iter().filter(|p| p.label == info.label).count() > 1 {
        // Extend the suffix if profiles happen to share their first eight digits.
        let mut length = 8.min(info.id.len());
        while length < info.id.len()
            && directory
                .iter()
                .any(|other| other.id != info.id && other.id.starts_with(&info.id[..length]))
        {
            length += 1;
        }
        format!("{} ({})", info.label, &info.id[..length])
    } else {
        info.label.clone()
    }
}

fn candidates(directory: &[rpc::ProfileInfo]) -> String {
    directory
        .iter()
        .map(|p| format!("  {}  {}  {}", p.id, display_label(p, directory), p.email))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn select(
    directory: &[rpc::ProfileInfo],
    selector: Option<&str>,
    remembered: Option<&str>,
) -> Result<rpc::ProfileInfo> {
    if let Some(selector) = selector {
        if let Ok(id) = Uuid::parse_str(selector)
            && let Some(profile) = directory.iter().find(|p| p.id == id.to_string())
        {
            return Ok(profile.clone());
        }
        let matches = directory
            .iter()
            .filter(|p| p.label == selector || display_label(p, directory) == selector)
            .cloned()
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [profile] => Ok(profile.clone()),
            [] => Err(anyhow!(
                "Unknown profile {selector:?}. Use `amux profiles` to list profiles."
            )),
            _ => Err(anyhow!(
                "Ambiguous profile {selector:?}; select a UUID:\n{}",
                candidates(&matches)
            )),
        };
    }
    if let Some(id) = remembered.and_then(|id| Uuid::parse_str(id.trim()).ok())
        && let Some(profile) = directory.iter().find(|p| p.id == id.to_string())
    {
        return Ok(profile.clone());
    }
    match directory {
        [profile] => Ok(profile.clone()),
        [] => bail!("No profiles. Run `amux profile create` or `amux login`."),
        _ => bail!(
            "Choose a profile with --profile <name|id>:\n{}",
            candidates(directory)
        ),
    }
}

pub async fn directory(front: &mut FrontDoorClient) -> Result<Vec<rpc::ProfileInfo>> {
    Ok(front
        .profiles
        .list_profiles(rpc::ListProfilesRequest {})
        .await?
        .into_inner()
        .profiles)
}

pub async fn resolve(
    front: &mut FrontDoorClient,
    selector: Option<&str>,
    last_used: &Path,
) -> Result<rpc::ProfileInfo> {
    let directory = directory(front).await?;
    let remembered =
        match selector.map_or_else(|| std::fs::read_to_string(last_used), |_| Ok(String::new())) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("cannot read last-profile"),
        };
    select(&directory, selector, remembered.as_deref())
}

pub fn config_path(installation: &InstallationConfig, info: &rpc::ProfileInfo) -> Result<PathBuf> {
    let id = Uuid::parse_str(&info.id).context("front door returned an invalid profile UUID")?;
    Ok(installation
        .root
        .join("profiles")
        .join(id.to_string())
        .join("config.yaml"))
}

pub fn load(path: &Path) -> Result<Config> {
    let path = std::fs::canonicalize(path)?;
    let resolved = amux::load_profile_config(&path)?;
    Ok(Config {
        host_name: resolved.installation.host_name,
        cloud_url: resolved.profile.cloud_url,
        socket_path: resolved.profile.socket_path,
        tcp_port: resolved.profile.tcp_port,
        state_path: resolved.profile.state_path,
        data_dir: resolved.profile.data_dir,
        reports_dir: resolved.installation.reports_dir,
        prevent_idle_sleep: resolved.installation.prevent_idle_sleep,
        minimum_client_versions: resolved.installation.minimum_client_versions,
        keybinds: resolved.installation.keybinds,
        ui: resolved.installation.ui,
        path: Some(path),
    })
}

pub async fn configuration(path: Option<&Path>, selector: Option<&str>) -> Result<Config> {
    let installation = crate::front_door::configuration(path)?;
    let explicit = path
        .map(std::fs::canonicalize)
        .transpose()?
        .map(|path| amux::load_profile_config(&path))
        .transpose()?
        .map(|config| config.profile_id.to_string());
    let mut front = crate::front_door::connect(&installation, true).await?;
    let info = resolve(
        &mut front,
        selector.or(explicit.as_deref()),
        &last_used(&installation),
    )
    .await?;
    let mut config = load(&config_path(&installation, &info)?)?;
    if !info.available || info.socket_path.is_empty() {
        bail!(
            "Profile {} ({}) is unavailable: {}",
            info.label,
            info.id,
            info.startup_error
        );
    }
    config.socket_path = info.socket_path.into();
    config.validate()?;
    Ok(config)
}

pub fn confirm(message: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("{message}\nConfirmation requires an interactive terminal.");
    }
    print!("{message} [y/N]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub fn operation(info: &rpc::ProfileInfo) -> rpc::ProfileOperation {
    rpc::ProfileOperation {
        operation_id: Uuid::new_v4().to_string(),
        profile_id: info.id.clone(),
    }
}

pub fn print_profile(profile: &rpc::ProfileInfo) {
    println!(
        "{}  {}  {}  {}",
        profile.id,
        profile.label,
        profile.email,
        status(profile)
    );
}

pub fn status(profile: &rpc::ProfileInfo) -> String {
    if !profile.startup_error.is_empty() {
        return format!("unavailable: {}", profile.startup_error);
    }
    if !profile.available {
        return "unavailable".into();
    }
    let intent = rpc::Intent::try_from(profile.intent)
        .map(|v| v.as_str_name())
        .unwrap_or("unknown");
    let observed = rpc::Observed::try_from(profile.observed)
        .map(|v| v.as_str_name())
        .unwrap_or("unknown");
    format!(
        "{} / {}",
        intent.trim_start_matches("INTENT_").to_ascii_lowercase(),
        observed
            .trim_start_matches("OBSERVED_")
            .to_ascii_lowercase()
    )
}

pub async fn administer(
    installation: &InstallationConfig,
    selector: Option<&str>,
    command: ProfileCommands,
) -> Result<()> {
    let mut front = crate::front_door::connect(installation, true).await?;
    if let ProfileCommands::Create { name } = command {
        let info = front
            .profiles
            .create_profile(rpc::CreateProfileRequest {
                operation_id: Uuid::new_v4().to_string(),
                label: name,
            })
            .await?
            .into_inner();
        print_profile(&info);
        return Ok(());
    }
    let info = resolve(&mut front, selector, &last_used(installation)).await?;
    let updated = match command {
        ProfileCommands::Create { .. } => unreachable!(),
        ProfileCommands::Delete { yes } => {
            if !yes
                && !confirm(&format!(
                    "Delete {} ({}) and destroy its keys, trust, agents and credentials?",
                    info.label, info.id
                ))
                .context("Use --yes to confirm a scripted deletion")?
            {
                println!("Deletion cancelled.");
                return Ok(());
            }
            front
                .profiles
                .delete_profile(rpc::DeleteProfileRequest {
                    operation_id: Uuid::new_v4().to_string(),
                    profile_id: info.id.clone(),
                    confirm_revision: info.revision,
                })
                .await?;
            println!("Deleted {} ({}).", info.label, info.id);
            return Ok(());
        }
        ProfileCommands::Rename { name, .. } => front
            .profiles
            .rename_profile(rpc::RenameProfileRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: info.id,
                expected_revision: info.revision,
                override_name: name,
            })
            .await?
            .into_inner(),
        ProfileCommands::Pause => front
            .profiles
            .pause_profile(operation(&info))
            .await?
            .into_inner(),
        ProfileCommands::Resume => front
            .profiles
            .resume_profile(operation(&info))
            .await?
            .into_inner(),
    };
    print_profile(&updated);
    Ok(())
}

pub async fn logout(installation: &InstallationConfig, selector: Option<&str>) -> Result<()> {
    let mut front = crate::front_door::connect(installation, true).await?;
    let info = resolve(&mut front, selector, &last_used(installation)).await?;
    let updated = front
        .profiles
        .logout_profile(operation(&info))
        .await?
        .into_inner();
    print_profile(&updated);
    Ok(())
}

pub async fn login(
    installation: &InstallationConfig,
    cloud_url: &str,
    selector: Option<&str>,
    name: Option<String>,
) -> Result<()> {
    let mut front = crate::front_door::connect(installation, true).await?;
    // Login without a target selects by the authenticated account, never by
    // whichever fleet a previous command happened to open.
    let target = match selector {
        Some(selector) => Some(
            resolve(&mut front, Some(selector), &last_used(installation))
                .await?
                .id,
        ),
        None => None,
    };
    let cloud_url = match target.as_deref() {
        Some(id) => {
            let info = resolve(&mut front, Some(id), &last_used(installation)).await?;
            load(&config_path(installation, &info)?)?.cloud_url
        }
        None => cloud_url.to_owned(),
    };
    let staged_refresh_token = amux::run_device_flow(cloud_url.trim_end_matches('/')).await?;
    let mut request = rpc::BindProfileRequest {
        operation_id: Uuid::new_v4().to_string(),
        profile_id: target,
        cloud_url: cloud_url.to_owned(),
        staged_refresh_token,
        adopt_non_pristine: false,
    };
    let response = front.profiles.bind_profile(request.clone()).await;
    let mut info = match response {
        Ok(response) => response.into_inner(),
        Err(error)
            if error
                .metadata()
                .get("amux-bind-error")
                .and_then(|value| value.to_str().ok())
                == Some("adoption-needs-confirmation") =>
        {
            let id = error
                .metadata()
                .get("amux-profile-id")
                .and_then(|value| value.to_str().ok())
                .context("adoption response omitted its profile UUID")?;
            let profile = resolve(&mut front, Some(id), &last_used(installation)).await?;
            if !confirm(&format!(
                "Profile {} ({}) already has local trust, agents or artifacts. Adopt this device into the account you just authenticated?",
                profile.label, profile.id
            ))? {
                println!("Login cancelled. Credentials were not committed.");
                return Ok(());
            }
            request.operation_id = Uuid::new_v4().to_string();
            request.profile_id = Some(profile.id);
            request.adopt_non_pristine = true;
            front.profiles.bind_profile(request).await?.into_inner()
        }
        Err(error) => {
            if let Some(id) = error
                .metadata()
                .get("amux-profile-id")
                .and_then(|value| value.to_str().ok())
                && let Ok(profile) = resolve(&mut front, Some(id), &last_used(installation)).await
            {
                bail!(
                    "Login refused for {} ({}, {}): {}",
                    profile.label,
                    profile.email,
                    profile.id,
                    error.message()
                );
            }
            return Err(error.into());
        }
    };
    if let Some(name) = name {
        info = front
            .profiles
            .rename_profile(rpc::RenameProfileRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: info.id,
                expected_revision: info.revision,
                override_name: Some(name),
            })
            .await?
            .into_inner();
    }
    // A successful login is an explicit selection, even if cloud connection
    // establishment is still in progress.
    remember(&last_used(installation), &info.id)?;
    println!("Logged in:");
    print_profile(&info);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: u128, name: &str) -> rpc::ProfileInfo {
        rpc::ProfileInfo {
            id: Uuid::from_u128(id).to_string(),
            label: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn profile_selector_resolves_uuid_label_and_last_used() {
        let a = profile(1, "Personal");
        let b = profile(2, "Work");
        let directory = vec![a.clone(), b.clone()];
        for selector in [&a.id, &a.label] {
            assert_eq!(
                select(&directory, Some(selector), Some(&b.id)).unwrap().id,
                a.id
            );
        }
        assert_eq!(select(&directory, None, Some(&b.id)).unwrap().id, b.id);
        assert_eq!(
            select(std::slice::from_ref(&a), None, Some("deleted"))
                .unwrap()
                .id,
            a.id
        );
        assert!(
            select(&directory, None, None)
                .unwrap_err()
                .to_string()
                .contains("Choose a profile")
        );
    }

    #[test]
    fn profile_selector_ambiguity_lists_candidates_and_suffixes_resolve() {
        let a = profile(1, "Alex");
        let b = profile(2, "Alex");
        let directory = vec![a.clone(), b.clone()];
        let error = select(&directory, Some("Alex"), Some(&a.id))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Ambiguous"));
        assert!(error.contains(&a.id) && error.contains(&b.id));
        assert_ne!(display_label(&a, &directory), display_label(&b, &directory));
        assert_eq!(
            select(&directory, Some(&display_label(&b, &directory)), None)
                .unwrap()
                .id,
            b.id
        );
    }

    #[test]
    fn profile_selector_unknown_never_uses_last_used() {
        let a = profile(1, "Personal");
        assert!(
            select(std::slice::from_ref(&a), Some("missing"), Some(&a.id))
                .unwrap_err()
                .to_string()
                .contains("Unknown profile")
        );
        assert!(
            select(&[], Some("missing"), None)
                .unwrap_err()
                .to_string()
                .contains("Unknown profile")
        );
    }

    #[test]
    fn profile_selector_zero_profiles_offers_create_and_login() {
        let error = select(&[], None, None).unwrap_err().to_string();
        assert!(error.contains("amux profile create") && error.contains("amux login"));
    }

    #[test]
    fn profile_selector_remembers_only_uuid_and_survives_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/last-profile");
        let a = profile(1, "Renamed");
        remember(&path, &a.id).unwrap();
        let remembered = std::fs::read_to_string(&path).unwrap();
        assert_eq!(remembered.trim(), a.id);
        assert_eq!(
            select(&[a.clone(), profile(2, "Work")], None, Some(&remembered))
                .unwrap()
                .id,
            a.id
        );
    }
}
