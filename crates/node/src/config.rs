//! Desktop configuration discovery and node runtime adaptation.

use std::path::{Path, PathBuf};

pub use settings::{
    ColorSetting, Config, ConfigError, InstallationConfig, Keybinds, LanConfig, LeaderKey,
    OpenMode, ProfileConfig, ThemeSetting, UiSettings,
};

use crate::installation::{InstallationSettings, ProfileId, ProfilePaths};

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub profile_id: ProfileId,
    pub profile: ProfileConfig,
    pub installation: InstallationConfig,
}

impl ResolvedConfig {
    pub fn artifact_cache_dir(&self) -> PathBuf {
        self.profile.artifact_cache_dir()
    }

    pub fn reports_dir(&self) -> PathBuf {
        self.installation
            .reports_dir
            .clone()
            .unwrap_or_else(|| self.profile.data_dir.join("reports"))
    }
}

/// Load an explicitly named profile and its explicitly referenced installation.
pub fn load_profile_config(path: &Path) -> Result<ResolvedConfig, ConfigError> {
    let path = absolute_path(path, &std::env::current_dir()?)?;
    let profile = ProfileConfig::from_file(&path)?;
    let installation = InstallationConfig::from_file(&profile.installation_config)?;
    let id = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| uuid::Uuid::parse_str(name).ok())
        .map(ProfileId)
        .ok_or_else(|| ConfigError::Invalid("profile config must be in a UUID directory".into()))?;
    let expected = ProfilePaths::allocated(&installation.root, id);
    check_path(
        "profile config",
        expected
            .config_path
            .as_ref()
            .expect("allocated profile path"),
        &path,
    )?;
    check_profile_paths(&profile, &installation.root, id)?;
    Ok(ResolvedConfig {
        profile_id: id,
        profile,
        installation,
    })
}

pub(crate) fn installation_settings(config: &InstallationConfig) -> InstallationSettings {
    InstallationSettings {
        host_name: config.host_name.clone(),
        prevent_idle_sleep: config.prevent_idle_sleep,
        keybinds: config.keybinds.clone(),
        ui: config.ui.clone(),
        keymaps_dir: config.keymaps_dir.clone(),
        minimum_client_versions: config.minimum_client_versions.clone(),
        update_manifest_url: config.update_manifest_url.clone(),
        status_reporters: crate::update::StatusReporters::UpdateMarkerFiles,
    }
}

pub(crate) fn check_path(
    field: &'static str,
    expected: &Path,
    actual: &Path,
) -> Result<(), ConfigError> {
    if expected != actual {
        return Err(ConfigError::Disagreement {
            field,
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn check_profile_paths(
    profile: &ProfileConfig,
    root: &Path,
    id: ProfileId,
) -> Result<(), ConfigError> {
    let expected = ProfilePaths::allocated(root, id);
    for (field, expected, actual) in [
        ("socket_path", &expected.socket_path, &profile.socket_path),
        ("data_dir", &expected.data_dir, &profile.data_dir),
        ("state_path", &expected.state_path, &profile.state_path),
    ] {
        check_path(field, expected, actual)?;
    }
    Ok(())
}

// Resolve aliases in the existing ancestor, even before a socket or state file
// exists. This gives /tmp and /private/tmp the same identity on macOS.
fn absolute_path(path: &Path, base: &Path) -> Result<PathBuf, ConfigError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    match std::fs::canonicalize(&path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ConfigError::Invalid(format!("cannot resolve {}", path.display()))
            })?;
            let parent = absolute_path(parent, base)?;
            match path.file_name() {
                Some(name) => Ok(parent.join(name)),
                None => Err(ConfigError::Invalid(format!(
                    "cannot resolve {}",
                    path.display()
                ))),
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod split_tests;
