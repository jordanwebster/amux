use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result as AnyResult, bail};
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use serde_yaml::{Mapping, Value};

use crate::config::Config;
use crate::identity::{IdentityError, ensure_device_files_in};
use crate::state::State;

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("state error: {0}")]
    State(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("identity error: {0}")]
    Identity(String),
    #[error("retired Claude plugin cleanup failed: {0}")]
    ClaudePluginCleanup(String),
}

const LEGACY_PLUGIN_REF: &str = "amux@amux";
const LEGACY_MARKETPLACE_NAME: &str = "amux";
const LEGACY_MARKETPLACE_DIR: &str = "claude-marketplace";
const CLAUDE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

impl From<IdentityError> for SetupError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error.to_string())
    }
}

/// Whether cloud mode is enabled (user opted in).
///
/// `true` iff `enable_cloud_mode == Some(true)`. Absent or `Some(false)` both
/// count as disabled.
pub fn cloud_enabled(config: &Config) -> bool {
    config.enable_cloud_mode == Some(true)
}

/// Persist `enable_cloud_mode` to `config.yaml` and update the in-memory
/// `Config`. Writes `config.yaml` as a merge so user-supplied keys are
/// preserved.
pub fn set_enable_cloud_mode(config: &mut Config, value: bool) -> Result<(), SetupError> {
    write_config_bool(config, "enable_cloud_mode", Some(value))?;
    config.enable_cloud_mode = Some(value);
    Ok(())
}

/// Persist `prevent_idle_sleep` to `config.yaml` and update the in-memory
/// `Config`.
pub fn set_prevent_idle_sleep(config: &mut Config, value: bool) -> Result<(), SetupError> {
    write_config_bool(config, "prevent_idle_sleep", Some(value))?;
    config.prevent_idle_sleep = Some(value);
    Ok(())
}

/// Clear `enable_cloud_mode` from `config.yaml` (used by `amux init --reset`).
pub fn clear_enable_cloud_mode(config: &mut Config) -> Result<(), SetupError> {
    write_config_bool(config, "enable_cloud_mode", None)?;
    config.enable_cloud_mode = None;
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

fn legacy_claude_plugin_cleanup_completed(config: &Config) -> bool {
    State::load(&config.state_path)
        .ok()
        .is_some_and(|state| state.claude.legacy_plugin_cleanup_completed)
}

fn set_legacy_claude_plugin_cleanup_completed(config: &Config) -> Result<(), SetupError> {
    State::update(&config.state_path, |s| {
        s.claude.legacy_plugin_cleanup_completed = true;
        s.claude.legacy_marketplace_path = None;
    })
    .map_err(|e| SetupError::State(e.to_string()))?;
    Ok(())
}

#[async_trait]
trait ClaudeCommandRunner: Sync {
    async fn run(&self, args: &[&str]) -> AnyResult<String>;
}

struct SystemClaudeCommandRunner;

#[async_trait]
impl ClaudeCommandRunner for SystemClaudeCommandRunner {
    async fn run(&self, args: &[&str]) -> AnyResult<String> {
        let display = args.join(" ");
        let mut command = tokio::process::Command::new("claude");
        command.args(args).kill_on_drop(true);
        let output = tokio::time::timeout(CLAUDE_COMMAND_TIMEOUT, command.output())
            .await
            .with_context(|| format!("'claude {display}' timed out"))?
            .with_context(|| format!("failed to run 'claude {display}'"))?;
        if !output.status.success() {
            bail!(
                "'claude {display}' failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout)
            .with_context(|| format!("'claude {display}' returned non-UTF-8 output"))
    }
}

/// Remove the retired user-global Claude plugin before starting a local agent
/// host. Clean installations take a state-only fast path and never invoke
/// Claude Code.
pub async fn ensure_legacy_claude_plugin_removed(config: &Config) -> Result<(), SetupError> {
    ensure_legacy_claude_plugin_removed_with(
        config,
        &SystemClaudeCommandRunner,
        &crate::default_data_dir(),
    )
    .await
    .map_err(|error| SetupError::ClaudePluginCleanup(error.to_string()))
}

async fn ensure_legacy_claude_plugin_removed_with(
    config: &Config,
    runner: &impl ClaudeCommandRunner,
    default_data_dir: &Path,
) -> AnyResult<()> {
    if legacy_claude_plugin_cleanup_completed(config) {
        return Ok(());
    }

    let prior_marketplace_path = State::load(&config.state_path)
        .ok()
        .and_then(|state| state.claude.legacy_marketplace_path);
    let mut materialized_paths = vec![config.data_dir.join(LEGACY_MARKETPLACE_DIR)];
    let default_path = default_data_dir.join(LEGACY_MARKETPLACE_DIR);
    if !materialized_paths.contains(&default_path) {
        materialized_paths.push(default_path);
    }
    if let Some(path) = prior_marketplace_path {
        validate_legacy_marketplace_path(&path)?;
        if !materialized_paths.contains(&path) {
            materialized_paths.push(path);
        }
    }
    let materialized = materialized_paths
        .iter()
        .any(|path| path.exists() || path.is_symlink());
    if !materialized {
        set_legacy_claude_plugin_cleanup_completed(config)
            .context("failed to record retired Claude plugin cleanup")?;
        return Ok(());
    }

    let plugins = runner.run(&["plugin", "list", "--json"]).await?;
    let plugin_installed = json_array_has(&plugins, "id", LEGACY_PLUGIN_REF)
        .context("failed to inspect installed Claude Code plugins")?;
    let marketplaces = runner
        .run(&["plugin", "marketplace", "list", "--json"])
        .await?;
    let marketplace_registered = json_array_has(&marketplaces, "name", LEGACY_MARKETPLACE_NAME)
        .context("failed to inspect Claude Code marketplaces")?;
    println!("Removing retired Claude Code amux plugin...");
    if plugin_installed {
        runner
            .run(&["plugin", "uninstall", LEGACY_PLUGIN_REF, "--scope", "user"])
            .await?;
    }
    if marketplace_registered {
        runner
            .run(&[
                "plugin",
                "marketplace",
                "remove",
                LEGACY_MARKETPLACE_NAME,
                "--scope",
                "user",
            ])
            .await?;
    }
    for path in materialized_paths {
        remove_materialized_marketplace(&path)?;
    }
    set_legacy_claude_plugin_cleanup_completed(config)
        .context("failed to record retired Claude plugin cleanup")?;
    println!("Removed.");
    Ok(())
}

fn json_array_has(json: &str, field: &str, expected: &str) -> AnyResult<bool> {
    let entries: JsonValue = serde_json::from_str(json).context("command output was not JSON")?;
    let entries = entries
        .as_array()
        .context("command output was not a JSON array")?;
    Ok(entries
        .iter()
        .any(|entry| entry.get(field).and_then(JsonValue::as_str) == Some(expected)))
}

fn remove_materialized_marketplace(path: &Path) -> AnyResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("failed to remove {}", path.display()))
}

fn validate_legacy_marketplace_path(path: &Path) -> AnyResult<()> {
    if !path.is_absolute() {
        bail!(
            "legacy Claude marketplace path is not absolute: {}; remove the retired marketplace manually and clear applied_marketplace_path from the state file",
            path.display()
        );
    }
    if path.file_name().and_then(|name| name.to_str()) != Some(LEGACY_MARKETPLACE_DIR) {
        bail!(
            "legacy Claude marketplace path has unexpected final component: {}; refusing to remove it",
            path.display()
        );
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        bail!(
            "legacy Claude marketplace path is not a directory or symlink: {}; refusing to remove it",
            path.display()
        );
    }
    let has_retired_plugin_files = path.join("claude-plugin/.mcp.json").is_file()
        || path.join(".claude-plugin/marketplace.json").is_file();
    if !has_retired_plugin_files {
        bail!(
            "legacy Claude marketplace path has no retired amux plugin files: {}; refusing to remove it",
            path.display()
        );
    }
    Ok(())
}

fn write_config_bool(config: &Config, key: &str, value: Option<bool>) -> Result<(), SetupError> {
    let path = config_file_path(config);
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
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::*;

    struct FakeClaudeCommandRunner {
        outputs: Mutex<VecDeque<String>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeClaudeCommandRunner {
        fn new(outputs: &[&str]) -> Self {
            Self {
                outputs: Mutex::new(outputs.iter().map(|output| output.to_string()).collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ClaudeCommandRunner for FakeClaudeCommandRunner {
        async fn run(&self, args: &[&str]) -> AnyResult<String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .context("missing fake Claude command output")
        }
    }

    #[test]
    fn local_host_id_reads_the_stored_identity() {
        let dir = tempdir().unwrap();
        assert_eq!(local_host_id_in(dir.path()), None);

        let identity = crate::identity::ensure_device_files_in(dir.path()).unwrap();
        assert_eq!(local_host_id_in(dir.path()), Some(identity.host_id));
    }

    #[test]
    fn cloud_enabled_requires_some_true() {
        let dir = tempdir().unwrap();
        let mut config = Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };
        assert!(!cloud_enabled(&config));
        config.enable_cloud_mode = Some(false);
        assert!(!cloud_enabled(&config));
        config.enable_cloud_mode = Some(true);
        assert!(cloud_enabled(&config));
    }

    #[test]
    fn set_enable_cloud_mode_persists_and_updates_in_memory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "host_name: test-host\n").unwrap();
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        set_enable_cloud_mode(&mut config, true).unwrap();
        assert_eq!(config.enable_cloud_mode, Some(true));

        let persisted = Config::from_file(&path).unwrap();
        assert_eq!(persisted.enable_cloud_mode, Some(true));
        assert_eq!(persisted.host_name, "test-host");
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
    fn clear_enable_cloud_mode_removes_the_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        set_enable_cloud_mode(&mut config, true).unwrap();
        clear_enable_cloud_mode(&mut config).unwrap();
        assert_eq!(config.enable_cloud_mode, None);

        let yaml = fs::read_to_string(path).unwrap();
        assert!(!yaml.contains("enable_cloud_mode"));
    }

    #[test]
    fn set_enable_cloud_mode_error_mentions_active_config_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config-dir");
        fs::create_dir(&path).unwrap();
        let mut config = Config {
            path: Some(path.clone()),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        let err = set_enable_cloud_mode(&mut config, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to save `enable_cloud_mode`"));
        assert!(msg.contains("active config file"));
        assert!(msg.contains("--config"));
        assert!(msg.contains(&path.display().to_string()));
    }

    #[tokio::test]
    async fn upgrade_cleanup_uninstalls_plugin_marketplace_and_materialized_files_once() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state/state.yaml"),
            data_dir: dir.path().join("data"),
            ..Config::default()
        };
        let materialized = config.data_dir.join(LEGACY_MARKETPLACE_DIR);
        fs::create_dir_all(materialized.join("claude-plugin")).unwrap();
        fs::write(materialized.join("claude-plugin/.mcp.json"), "{}").unwrap();
        let default_data_dir = dir.path().join("default-data");
        let default_materialized = default_data_dir.join(LEGACY_MARKETPLACE_DIR);
        fs::create_dir_all(default_materialized.join("claude-plugin")).unwrap();
        fs::write(default_materialized.join("claude-plugin/.mcp.json"), "{}").unwrap();
        let runner = FakeClaudeCommandRunner::new(&[
            r#"[{"id":"amux@amux","enabled":true}]"#,
            r#"[{"name":"amux","path":"/old/claude-marketplace"}]"#,
            "",
            "",
        ]);

        ensure_legacy_claude_plugin_removed_with(&config, &runner, &default_data_dir)
            .await
            .unwrap();

        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![
                vec!["plugin", "list", "--json"],
                vec!["plugin", "marketplace", "list", "--json"],
                vec!["plugin", "uninstall", "amux@amux", "--scope", "user"],
                vec!["plugin", "marketplace", "remove", "amux", "--scope", "user"],
            ]
        );
        assert!(!materialized.exists());
        assert!(!default_materialized.exists());
        assert!(legacy_claude_plugin_cleanup_completed(&config));

        ensure_legacy_claude_plugin_removed_with(&config, &runner, &default_data_dir)
            .await
            .unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn upgrade_cleanup_uses_marketplace_path_from_legacy_state() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state.yaml"),
            data_dir: dir.path().join("new-data"),
            ..Config::default()
        };
        let old_marketplace = dir.path().join("old-data/claude-marketplace");
        fs::create_dir_all(old_marketplace.join("claude-plugin")).unwrap();
        fs::write(old_marketplace.join("claude-plugin/.mcp.json"), "{}").unwrap();
        fs::write(
            &config.state_path,
            format!(
                "claude:\n  applied_plugin_version: 0.3.0\n  applied_marketplace_path: {}\n",
                old_marketplace.display()
            ),
        )
        .unwrap();
        let runner = FakeClaudeCommandRunner::new(&["[]", "[]"]);

        ensure_legacy_claude_plugin_removed_with(
            &config,
            &runner,
            &dir.path().join("default-data"),
        )
        .await
        .unwrap();

        assert!(!old_marketplace.exists());
        let persisted = fs::read_to_string(&config.state_path).unwrap();
        assert!(persisted.contains("legacy_plugin_cleanup_completed: true"));
        assert!(!persisted.contains("applied_marketplace_path"));
        assert_eq!(runner.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn upgrade_cleanup_refuses_relative_legacy_marketplace_path() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state.yaml"),
            data_dir: dir.path().join("new-data"),
            ..Config::default()
        };
        fs::write(
            &config.state_path,
            "claude:\n  applied_marketplace_path: old-data/claude-marketplace\n",
        )
        .unwrap();
        let runner = FakeClaudeCommandRunner::new(&[]);

        let error = ensure_legacy_claude_plugin_removed_with(
            &config,
            &runner,
            &dir.path().join("default-data"),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("is not absolute"));
        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(!legacy_claude_plugin_cleanup_completed(&config));
    }

    #[tokio::test]
    async fn upgrade_cleanup_refuses_unrecognized_legacy_marketplace_directory() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state.yaml"),
            data_dir: dir.path().join("new-data"),
            ..Config::default()
        };
        let unrelated = dir.path().join("old-data/claude-marketplace");
        fs::create_dir_all(&unrelated).unwrap();
        let preserved = unrelated.join("keep.txt");
        fs::write(&preserved, "not an amux marketplace").unwrap();
        fs::write(
            &config.state_path,
            format!(
                "claude:\n  applied_marketplace_path: {}\n",
                unrelated.display()
            ),
        )
        .unwrap();
        let runner = FakeClaudeCommandRunner::new(&[]);

        let error = ensure_legacy_claude_plugin_removed_with(
            &config,
            &runner,
            &dir.path().join("default-data"),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("no retired amux plugin files"));
        assert_eq!(
            fs::read_to_string(preserved).unwrap(),
            "not an amux marketplace"
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(!legacy_claude_plugin_cleanup_completed(&config));
    }

    #[tokio::test]
    async fn clean_machine_records_cleanup_without_mutating_claude_configuration() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state.yaml"),
            data_dir: dir.path().join("data"),
            ..Config::default()
        };
        let runner = FakeClaudeCommandRunner::new(&[]);

        ensure_legacy_claude_plugin_removed_with(
            &config,
            &runner,
            &dir.path().join("default-data"),
        )
        .await
        .unwrap();

        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(legacy_claude_plugin_cleanup_completed(&config));
    }
}
