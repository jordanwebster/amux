use std::path::Path;
use std::time::Duration;

use amux::{Config, setup};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;

const LEGACY_PLUGIN_REF: &str = "amux@amux";
const LEGACY_MARKETPLACE_NAME: &str = "amux";
const LEGACY_MARKETPLACE_DIR: &str = "claude-marketplace";
const CLAUDE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait]
trait ClaudeCommandRunner: Sync {
    async fn run(&self, args: &[&str]) -> Result<String>;
}

struct SystemClaudeCommandRunner;

#[async_trait]
impl ClaudeCommandRunner for SystemClaudeCommandRunner {
    async fn run(&self, args: &[&str]) -> Result<String> {
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

pub async fn ensure_legacy_plugin_removed(config: &Config) -> Result<()> {
    ensure_legacy_plugin_removed_with(
        config,
        &SystemClaudeCommandRunner,
        &amux::default_data_dir(),
    )
    .await
}

async fn ensure_legacy_plugin_removed_with(
    config: &Config,
    runner: &impl ClaudeCommandRunner,
    default_data_dir: &Path,
) -> Result<()> {
    if setup::legacy_claude_plugin_cleanup_completed(config) {
        return Ok(());
    }

    let mut materialized_paths = vec![config.data_dir.join(LEGACY_MARKETPLACE_DIR)];
    let default_path = default_data_dir.join(LEGACY_MARKETPLACE_DIR);
    if !materialized_paths.contains(&default_path) {
        materialized_paths.push(default_path);
    }
    let materialized = materialized_paths
        .iter()
        .any(|path| path.exists() || path.is_symlink());
    if !materialized {
        setup::set_legacy_claude_plugin_cleanup_completed(config)
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
    if plugin_installed || marketplace_registered || materialized {
        println!("Removing retired Claude Code amux plugin...");
    }
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
    setup::set_legacy_claude_plugin_cleanup_completed(config)
        .context("failed to record retired Claude plugin cleanup")?;
    if plugin_installed || marketplace_registered || materialized {
        println!("Removed.");
    }
    Ok(())
}

fn json_array_has(json: &str, field: &str, expected: &str) -> Result<bool> {
    let entries: Value = serde_json::from_str(json).context("command output was not JSON")?;
    let entries = entries
        .as_array()
        .context("command output was not a JSON array")?;
    Ok(entries
        .iter()
        .any(|entry| entry.get(field).and_then(Value::as_str) == Some(expected)))
}

fn remove_materialized_marketplace(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("failed to remove {}", path.display()))
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
        async fn run(&self, args: &[&str]) -> Result<String> {
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

    #[tokio::test]
    async fn upgrade_cleanup_uninstalls_plugin_marketplace_and_materialized_files_once() {
        let dir = tempdir().unwrap();
        let config = Config {
            state_path: dir.path().join("state/state.yaml"),
            data_dir: dir.path().join("data"),
            ..Config::default()
        };
        let materialized = config.data_dir.join(LEGACY_MARKETPLACE_DIR);
        std::fs::create_dir_all(materialized.join("claude-plugin")).unwrap();
        std::fs::write(materialized.join("claude-plugin/.mcp.json"), "{}").unwrap();
        let default_data_dir = dir.path().join("default-data");
        let default_materialized = default_data_dir.join(LEGACY_MARKETPLACE_DIR);
        std::fs::create_dir_all(default_materialized.join("claude-plugin")).unwrap();
        std::fs::write(default_materialized.join("claude-plugin/.mcp.json"), "{}").unwrap();
        let runner = FakeClaudeCommandRunner::new(&[
            r#"[{"id":"amux@amux","enabled":true}]"#,
            r#"[{"name":"amux","path":"/old/claude-marketplace"}]"#,
            "",
            "",
        ]);

        ensure_legacy_plugin_removed_with(&config, &runner, &default_data_dir)
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
        assert!(setup::legacy_claude_plugin_cleanup_completed(&config));

        ensure_legacy_plugin_removed_with(&config, &runner, &default_data_dir)
            .await
            .unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 4);
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

        ensure_legacy_plugin_removed_with(&config, &runner, &dir.path().join("default-data"))
            .await
            .unwrap();

        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(setup::legacy_claude_plugin_cleanup_completed(&config));
    }
}
