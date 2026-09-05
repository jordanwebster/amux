use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::config::Config;
use crate::identity::{IdentityError, ensure_device_files_in};

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("state error: {0}")]
    State(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("identity error: {0}")]
    Identity(String),
}

impl From<IdentityError> for SetupError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error.to_string())
    }
}

/// Persist `prevent_idle_sleep` to `config.yaml` and update the in-memory
/// `Config`.
pub fn set_prevent_idle_sleep(config: &mut Config, value: bool) -> Result<(), SetupError> {
    write_config_bool(config, "prevent_idle_sleep", Some(value))?;
    config.prevent_idle_sleep = Some(value);
    Ok(())
}

/// Clear `prevent_idle_sleep` from `config.yaml` (used by `amux init --reset`).
pub fn clear_prevent_idle_sleep(config: &mut Config) -> Result<(), SetupError> {
    write_config_bool(config, "prevent_idle_sleep", None)?;
    config.prevent_idle_sleep = None;
    Ok(())
}

/// Return whether prevent-idle-sleep support is actually available at runtime.
pub fn prevent_idle_sleep_supported() -> bool {
    crate::sleep_inhibitor::supported()
}

/// True when the identity/trust files in `config.data_dir` already exist and
/// validate.
pub fn device_identity_ready(config: &Config) -> bool {
    crate::identity::device_files_ready_in(&config.data_dir)
}

/// The host id of this device's stored identity, if initialized. Read-only:
/// clients use it to recognize the local host in inventory (the wire does
/// not mark the local host).
pub fn local_host_id(config: &Config) -> Option<crate::HostId> {
    local_host_id_in(&config.data_dir)
}

/// See [`local_host_id`]; explicit data dir for tests and embedding.
pub fn local_host_id_in(data_dir: &Path) -> Option<crate::HostId> {
    crate::identity::stored_host_id_in(data_dir)
}

/// Ensure the device identity and trust-store files from
/// `docs/ARCHITECTURE.md` exist in `config.data_dir`.
pub fn ensure_device_identity(config: &Config) -> Result<(), SetupError> {
    ensure_device_files_in(&config.data_dir)?;
    Ok(())
}

fn write_config_bool(config: &Config, key: &str, value: Option<bool>) -> Result<(), SetupError> {
    let selected = config_file_path(config);
    let path = if selected.exists() {
        let map = read_config_mapping(&selected)
            .map_err(|error| wrap_config_persistence_error(&selected, key, error))?;
        match map.get(Value::String("installation_config".into())) {
            Some(Value::String(path)) => PathBuf::from(path),
            _ => selected,
        }
    } else {
        selected
    };
    let mut map =
        read_config_mapping(&path).map_err(|e| wrap_config_persistence_error(&path, key, e))?;

    match value {
        Some(v) => {
            map.insert(Value::String(key.to_string()), Value::Bool(v));
        }
        None => {
            map.remove(Value::String(key.to_string()));
        }
    }

    let yaml = serde_yaml::to_string(&Value::Mapping(map)).map_err(|e| {
        config_persistence_error(&path, key, format!("failed to serialize config: {e}"))
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            config_persistence_error(
                &path,
                key,
                format!("failed to create {}: {e}", parent.display()),
            )
        })?;
    }
    fs::write(&path, yaml).map_err(|e| {
        config_persistence_error(
            &path,
            key,
            format!("failed to write {}: {e}", path.display()),
        )
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

fn config_persistence_error(path: &Path, key: &str, detail: String) -> SetupError {
    SetupError::Config(format!(
        "failed to save `{key}` to {}: {detail}\n\namux init writes setup choices to the active config file.\nMake that file writable, or rerun with `--config` pointing to a writable config path.",
        path.display()
    ))
}

fn wrap_config_persistence_error(path: &Path, key: &str, error: SetupError) -> SetupError {
    match error {
        SetupError::Config(detail) => config_persistence_error(path, key, detail),
        other => other,
    }
}

#[cfg(test)]
mod tests {

    use tempfile::tempdir;

    use super::*;
    #[test]
    fn local_host_id_reads_the_stored_identity() {
        let dir = tempdir().unwrap();
        assert_eq!(local_host_id_in(dir.path()), None);

        let identity = crate::identity::ensure_device_files_in(dir.path()).unwrap();
        assert_eq!(local_host_id_in(dir.path()), Some(identity.host_id));
    }

    #[test]
    fn set_prevent_idle_sleep_persists_and_updates_in_memory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        set_prevent_idle_sleep(&mut config, true).unwrap();
        assert_eq!(config.prevent_idle_sleep, Some(true));

        let persisted = Config::from_file(&path).unwrap();
        assert_eq!(persisted.prevent_idle_sleep, Some(true));
    }

    #[test]
    fn set_prevent_idle_sleep_persists_false_explicitly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        set_prevent_idle_sleep(&mut config, false).unwrap();
        assert_eq!(config.prevent_idle_sleep, Some(false));

        let yaml = fs::read_to_string(path).unwrap();
        assert!(yaml.contains("prevent_idle_sleep: false"));
    }

    #[test]
    fn set_prevent_idle_sleep_error_mentions_active_config_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config-dir");
        fs::create_dir(&path).unwrap();
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        let err = set_prevent_idle_sleep(&mut config, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to save `prevent_idle_sleep`"));
        assert!(msg.contains("active config file"));
        assert!(msg.contains("--config"));
        assert!(msg.contains(&path.display().to_string()));
    }
}
