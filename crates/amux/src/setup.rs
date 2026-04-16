use crate::auth::oauth;
use crate::config::Config;
use crate::state::{CloudState, State, StateError};
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct CloudSetupState {
    pub use_cloud_mode: Option<bool>,
    pub has_refresh_token: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ClaudePluginSetupState {
    pub applied_plugin_version: Option<String>,
    pub applied_marketplace_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("oauth error: {0}")]
    OAuth(#[from] oauth::OAuthError),
    #[error("config error: {0}")]
    Config(String),
}

/// Check whether cloud onboarding is incomplete.
pub fn needs_init(config: &Config) -> bool {
    let state = State::load(&config.state_path).unwrap_or_default();
    let cloud_needs_init = match state.cloud.use_cloud_mode {
        None => true,
        Some(true) => state.cloud.refresh_token.is_none(),
        Some(false) => false,
    };

    cloud_needs_init
        || (prevent_idle_sleep_supported()
            && prevent_idle_sleep_preference(config)
                .map(|preference| preference.is_none())
                .unwrap_or(false))
}

/// Read current cloud onboarding state.
pub fn cloud_setup_state(config: &Config) -> Result<CloudSetupState, SetupError> {
    let state = State::load(&config.state_path)?;
    Ok(CloudSetupState {
        use_cloud_mode: state.cloud.use_cloud_mode,
        has_refresh_token: state.cloud.refresh_token.is_some(),
    })
}

/// Reset cloud onboarding fields in persistent state.
pub fn reset_cloud_state(config: &Config) -> Result<(), SetupError> {
    State::update(&config.state_path, |s| {
        s.cloud = CloudState::default();
    })?;
    Ok(())
}

/// Persist cloud mode preference.
pub fn set_use_cloud_mode(config: &Config, use_cloud_mode: bool) -> Result<(), SetupError> {
    State::update(&config.state_path, |s| {
        s.cloud.use_cloud_mode = Some(use_cloud_mode);
    })?;
    Ok(())
}

/// Run OAuth device flow and persist refresh token.
pub async fn authenticate_cloud(config: &Config) -> Result<(), SetupError> {
    let refresh_token = oauth::device_flow(&config.cloud_url).await?;
    State::update(&config.state_path, |s| {
        s.cloud.refresh_token = Some(refresh_token);
    })?;
    Ok(())
}

/// Read the persisted prevent-idle-sleep preference from config.yaml.
///
/// Returns `Ok(None)` when the config file does not exist or the field has not
/// been set yet.
pub fn prevent_idle_sleep_preference(config: &Config) -> Result<Option<bool>, SetupError> {
    let path = config_file_path(config);
    if !path.exists() {
        return Ok(None);
    }

    let map = read_config_mapping(&path)?;

    Ok(map
        .get(Value::String("prevent_idle_sleep".to_string()))
        .and_then(Value::as_bool))
}

/// Persist the prevent-idle-sleep preference in config.yaml.
pub fn set_prevent_idle_sleep(config: &Config, enabled: bool) -> Result<(), SetupError> {
    let path = config_file_path(config);
    let mut map =
        read_config_mapping(&path).map_err(|e| wrap_config_persistence_error(&path, e))?;

    map.insert(
        Value::String("prevent_idle_sleep".to_string()),
        Value::Bool(enabled),
    );

    let yaml = serde_yaml::to_string(&Value::Mapping(map))
        .map_err(|e| config_persistence_error(&path, format!("failed to serialize config: {e}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            config_persistence_error(&path, format!("failed to create {}: {e}", parent.display()))
        })?;
    }
    fs::write(&path, yaml).map_err(|e| {
        config_persistence_error(&path, format!("failed to write {}: {e}", path.display()))
    })?;
    Ok(())
}

/// Return whether prevent-idle-sleep support is actually available at runtime.
pub fn prevent_idle_sleep_supported() -> bool {
    crate::sleep_inhibitor::supported()
}

/// Read Claude plugin setup state persisted for Claude Code integration.
pub fn claude_plugin_setup_state() -> ClaudePluginSetupState {
    let state_path = State::default_path();
    State::load(&state_path)
        .ok()
        .map(|s| ClaudePluginSetupState {
            applied_plugin_version: s.claude.applied_plugin_version,
            applied_marketplace_path: s.claude.applied_marketplace_path,
        })
        .unwrap_or_default()
}

/// Persist the Claude plugin version and marketplace path last successfully
/// applied to Claude Code.
pub fn set_claude_plugin_setup_state(
    version: &str,
    marketplace_path: &Path,
) -> Result<(), SetupError> {
    let state_path = State::default_path();
    State::update(&state_path, |s| {
        s.claude.applied_plugin_version = Some(version.to_string());
        s.claude.applied_marketplace_path = Some(marketplace_path.to_path_buf());
    })?;
    Ok(())
}

fn config_file_path(config: &Config) -> PathBuf {
    config.path.clone().unwrap_or_else(Config::default_path)
}

fn read_config_mapping(path: &Path) -> Result<Mapping, SetupError> {
    if !path.exists() {
        return Ok(Mapping::new());
    }

    let contents = fs::read_to_string(path)
        .map_err(|e| SetupError::Config(format!("failed to read {}: {e}", path.display())))?;
    match serde_yaml::from_str::<Value>(&contents)
        .map_err(|e| SetupError::Config(format!("failed to parse {}: {e}", path.display())))?
    {
        Value::Null => Ok(Mapping::new()),
        Value::Mapping(map) => Ok(map),
        _ => Err(SetupError::Config(format!(
            "{} must contain a YAML mapping",
            path.display()
        ))),
    }
}

fn config_persistence_error(path: &Path, detail: String) -> SetupError {
    SetupError::Config(format!(
        "failed to save `prevent_idle_sleep` to {}: {detail}\n\namux init writes setup choices to the active config file.\nMake that file writable, or rerun with `--config` pointing to a writable config path.",
        path.display()
    ))
}

fn wrap_config_persistence_error(path: &Path, error: SetupError) -> SetupError {
    match error {
        SetupError::Config(detail) => config_persistence_error(path, detail),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prevent_idle_sleep_preference_is_none_when_config_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let config = Config {
            path: Some(path),
            ..Config::default()
        };

        let preference = prevent_idle_sleep_preference(&config).unwrap();
        assert_eq!(preference, None);
    }

    #[test]
    fn prevent_idle_sleep_preference_is_none_when_config_blank() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "  \n").unwrap();
        let config = Config {
            path: Some(path),
            ..Config::default()
        };

        let preference = prevent_idle_sleep_preference(&config).unwrap();
        assert_eq!(preference, None);
    }

    #[test]
    fn prevent_idle_sleep_preference_is_none_when_config_comment_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "# comment\n").unwrap();
        let config = Config {
            path: Some(path),
            ..Config::default()
        };

        let preference = prevent_idle_sleep_preference(&config).unwrap();
        assert_eq!(preference, None);
    }

    #[test]
    fn set_prevent_idle_sleep_creates_and_updates_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "host_name: test-host\n").unwrap();
        let config = Config {
            path: Some(path.clone()),
            ..Config::default()
        };

        set_prevent_idle_sleep(&config, true).unwrap();
        assert_eq!(prevent_idle_sleep_preference(&config).unwrap(), Some(true));

        let persisted = Config::from_file(&path).unwrap();
        assert_eq!(persisted.host_name, "test-host");
        assert!(persisted.prevent_idle_sleep);
    }

    #[test]
    fn set_prevent_idle_sleep_persists_false_explicitly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let config = Config {
            path: Some(path.clone()),
            ..Config::default()
        };

        set_prevent_idle_sleep(&config, false).unwrap();
        assert_eq!(prevent_idle_sleep_preference(&config).unwrap(), Some(false));

        let yaml = fs::read_to_string(path).unwrap();
        assert!(yaml.contains("prevent_idle_sleep: false"));
    }

    #[test]
    fn set_prevent_idle_sleep_updates_blank_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "\n").unwrap();
        let config = Config {
            path: Some(path.clone()),
            ..Config::default()
        };

        set_prevent_idle_sleep(&config, true).unwrap();

        let persisted = Config::from_file(&path).unwrap();
        assert!(persisted.prevent_idle_sleep);
    }

    #[test]
    fn set_prevent_idle_sleep_error_mentions_active_config_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config-dir");
        fs::create_dir(&path).unwrap();
        let config = Config {
            path: Some(path.clone()),
            ..Config::default()
        };

        let err = set_prevent_idle_sleep(&config, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to save `prevent_idle_sleep`"));
        assert!(msg.contains("active config file"));
        assert!(msg.contains("--config"));
        assert!(msg.contains(&path.display().to_string()));
    }
}
