use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use amux::{default_data_dir, setup};
use serde::Deserialize;

const MARKETPLACE_NAME: &str = "amux";
const PLUGIN_REF: &str = "amux@amux";
const MATERIALIZED_MARKETPLACE_DIR: &str = "claude-marketplace";

const BUNDLED_MARKETPLACE_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.claude-plugin/marketplace.json"
));
const BUNDLED_PLUGIN_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../claude-plugin/.claude-plugin/plugin.json"
));
const BUNDLED_HOOKS_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../claude-plugin/hooks/hooks.json"
));

const BUNDLED_FILES: &[BundledFile] = &[
    BundledFile {
        relative_path: ".claude-plugin/marketplace.json",
        contents: BUNDLED_MARKETPLACE_MANIFEST,
    },
    BundledFile {
        relative_path: "claude-plugin/.claude-plugin/plugin.json",
        contents: BUNDLED_PLUGIN_MANIFEST,
    },
    BundledFile {
        relative_path: "claude-plugin/hooks/hooks.json",
        contents: BUNDLED_HOOKS_MANIFEST,
    },
];

#[derive(Debug, Clone, Copy)]
struct BundledFile {
    relative_path: &'static str,
    contents: &'static str,
}

#[derive(Debug, Clone)]
struct ClaudePluginPaths {
    root_dir: PathBuf,
}

impl ClaudePluginPaths {
    fn default() -> Self {
        Self {
            root_dir: default_data_dir().join(MATERIALIZED_MARKETPLACE_DIR),
        }
    }

    fn resolve(&self, relative_path: &str) -> PathBuf {
        self.root_dir.join(relative_path)
    }

    fn plugin_manifest_path(&self) -> PathBuf {
        self.resolve("claude-plugin/.claude-plugin/plugin.json")
    }
}

#[derive(Debug)]
struct ClaudePlugin {
    paths: ClaudePluginPaths,
    materialized_version: Option<String>,
    materialized_manifest_valid: bool,
    applied_version: Option<String>,
    applied_marketplace_path: Option<PathBuf>,
}

impl ClaudePlugin {
    fn load() -> Result<Self, String> {
        let state = setup::claude_plugin_setup_state();
        Self::from_paths(
            ClaudePluginPaths::default(),
            state.applied_plugin_version,
            state.applied_marketplace_path,
        )
    }

    fn from_paths(
        paths: ClaudePluginPaths,
        applied_version: Option<String>,
        applied_marketplace_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let (materialized_version, materialized_manifest_valid) =
            read_plugin_version(&paths.plugin_manifest_path())?;
        Ok(Self {
            paths,
            materialized_version,
            materialized_manifest_valid,
            applied_version,
            applied_marketplace_path,
        })
    }

    fn is_current(&self, expected_version: &str) -> bool {
        self.has_materialized_bundle()
            && self.materialized_version.as_deref() == Some(expected_version)
            && self.applied_version.as_deref() == Some(expected_version)
            && self.applied_marketplace_path.as_deref() == Some(self.paths.root_dir.as_path())
    }

    fn needs_materialization(&self, expected_version: &str) -> bool {
        !self.has_materialized_bundle()
            || self.materialized_version.as_deref() != Some(expected_version)
    }

    fn required_action(&self, expected_version: &str) -> Option<PluginAction> {
        if self.is_current(expected_version) {
            None
        } else if self.applied_version.is_none() {
            Some(PluginAction::Install)
        } else if self.applied_marketplace_path.as_deref() != Some(self.paths.root_dir.as_path()) {
            Some(PluginAction::Rebind)
        } else {
            Some(PluginAction::Update)
        }
    }

    fn has_materialized_bundle(&self) -> bool {
        self.materialized_manifest_valid
            && BUNDLED_FILES
                .iter()
                .all(|file| self.paths.resolve(file.relative_path).exists())
    }

    fn materialize_bundle(&mut self) -> Result<(), String> {
        for file in BUNDLED_FILES {
            let path = self.paths.resolve(file.relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
            }
            fs::write(&path, file.contents)
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
        }

        let (materialized_version, materialized_manifest_valid) =
            read_plugin_version(&self.paths.plugin_manifest_path())?;
        self.materialized_version = materialized_version;
        self.materialized_manifest_valid = materialized_manifest_valid;
        Ok(())
    }

    fn set_applied_state(&mut self, version: &str) -> Result<(), String> {
        setup::set_claude_plugin_setup_state(version, &self.paths.root_dir)
            .map_err(|e| format!("failed to save plugin state: {e}"))?;
        self.applied_version = Some(version.to_string());
        self.applied_marketplace_path = Some(self.paths.root_dir.clone());
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginAction {
    Install,
    Update,
    Rebind,
}

#[derive(Debug, Deserialize)]
struct PluginManifest {
    version: String,
}

/// Ensure the Claude Code amux plugin is installed and up to date.
pub async fn ensure_plugin_installed() {
    let expected_version = match bundled_plugin_version() {
        Ok(version) => version,
        Err(e) => {
            eprintln!("error: failed to prepare Claude Code plugin: {e}");
            std::process::exit(1);
        }
    };

    let mut plugin = match ClaudePlugin::load() {
        Ok(plugin) => plugin,
        Err(e) => {
            eprintln!("error: failed to prepare Claude Code plugin: {e}");
            std::process::exit(1);
        }
    };

    if plugin.needs_materialization(expected_version)
        && let Err(e) = plugin.materialize_bundle()
    {
        let action = plugin
            .required_action(expected_version)
            .unwrap_or(PluginAction::Install);
        exit_with_plugin_error(action, e);
    }

    let Some(action) = plugin.required_action(expected_version) else {
        return;
    };

    print_plugin_action(action);

    let result = match action {
        PluginAction::Install => run_install(&plugin.paths).await,
        PluginAction::Update => run_update().await,
        PluginAction::Rebind => run_rebind(&plugin.paths).await,
    };

    match result {
        Ok(()) => {
            if let Err(e) = plugin.set_applied_state(expected_version) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            print_plugin_success(action);
        }
        Err(e) => exit_with_plugin_error(action, e),
    }
}

fn bundled_plugin_version() -> Result<&'static str, String> {
    static VERSION: OnceLock<Result<String, String>> = OnceLock::new();

    match VERSION
        .get_or_init(|| parse_plugin_version(BUNDLED_PLUGIN_MANIFEST, "bundled plugin manifest"))
    {
        Ok(version) => Ok(version.as_str()),
        Err(err) => Err(err.clone()),
    }
}

fn read_plugin_version(path: &Path) -> Result<(Option<String>, bool), String> {
    if !path.exists() {
        return Ok((None, true));
    }

    let contents =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    match parse_plugin_version(&contents, &format!("plugin manifest at {}", path.display())) {
        Ok(version) => Ok((Some(version), true)),
        Err(_) => Ok((None, false)),
    }
}

fn parse_plugin_version(contents: &str, context: &str) -> Result<String, String> {
    serde_json::from_str::<PluginManifest>(contents)
        .map(|manifest| manifest.version)
        .map_err(|e| format!("failed to parse {context}: {e}"))
}

async fn run_install(paths: &ClaudePluginPaths) -> Result<(), String> {
    run_claude_command(vec![
        OsString::from("plugin"),
        OsString::from("marketplace"),
        OsString::from("add"),
        paths.root_dir.clone().into_os_string(),
    ])
    .await?;

    run_claude_command(vec![
        OsString::from("plugin"),
        OsString::from("install"),
        OsString::from(PLUGIN_REF),
        OsString::from("--scope"),
        OsString::from("user"),
    ])
    .await
}

async fn run_update() -> Result<(), String> {
    run_claude_command(vec![
        OsString::from("plugin"),
        OsString::from("marketplace"),
        OsString::from("update"),
        OsString::from(MARKETPLACE_NAME),
    ])
    .await?;

    run_claude_command(vec![
        OsString::from("plugin"),
        OsString::from("update"),
        OsString::from(PLUGIN_REF),
        OsString::from("--scope"),
        OsString::from("user"),
    ])
    .await
}

async fn run_rebind(paths: &ClaudePluginPaths) -> Result<(), String> {
    run_claude_command(vec![
        OsString::from("plugin"),
        OsString::from("marketplace"),
        OsString::from("remove"),
        OsString::from(MARKETPLACE_NAME),
    ])
    .await?;

    run_install(paths).await
}

async fn run_claude_command(args: Vec<OsString>) -> Result<(), String> {
    let display = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    let output = tokio::process::Command::new("claude")
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("failed to run 'claude {display}': {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("'claude {display}' failed: {}", stderr.trim()))
    }
}

fn print_plugin_action(action: PluginAction) {
    match action {
        PluginAction::Install => println!("Installing Claude Code amux plugin..."),
        PluginAction::Update | PluginAction::Rebind => {
            println!("Updating Claude Code amux plugin...")
        }
    }
}

fn print_plugin_success(action: PluginAction) {
    match action {
        PluginAction::Install => println!("Installed."),
        PluginAction::Update | PluginAction::Rebind => println!("Updated."),
    }
}

fn exit_with_plugin_error(action: PluginAction, error: String) -> ! {
    eprintln!(
        "error: failed to {} Claude Code plugin: {}",
        match action {
            PluginAction::Install => "install",
            PluginAction::Update | PluginAction::Rebind => "update",
        },
        error
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn bundled_manifest_has_version() {
        assert_eq!(bundled_plugin_version().unwrap(), "1.0.0");
    }

    #[test]
    fn missing_materialized_manifest_reports_none() {
        let temp = TempDir::new().unwrap();
        let paths = ClaudePluginPaths {
            root_dir: temp.path().join("claude-marketplace"),
        };

        let plugin = ClaudePlugin::from_paths(paths, None, None).unwrap();
        assert!(plugin.materialized_version.is_none());
    }

    #[test]
    fn materialize_bundle_writes_plugin_manifest_and_hooks() {
        let temp = TempDir::new().unwrap();
        let paths = ClaudePluginPaths {
            root_dir: temp.path().join("claude-marketplace"),
        };

        let mut plugin = ClaudePlugin::from_paths(
            paths.clone(),
            Some("0.9.0".to_string()),
            Some(paths.root_dir.clone()),
        )
        .unwrap();
        plugin.materialize_bundle().unwrap();

        assert_eq!(plugin.materialized_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            fs::read_to_string(paths.resolve(".claude-plugin/marketplace.json")).unwrap(),
            BUNDLED_MARKETPLACE_MANIFEST
        );
        assert_eq!(
            fs::read_to_string(paths.resolve("claude-plugin/hooks/hooks.json")).unwrap(),
            BUNDLED_HOOKS_MANIFEST
        );
    }

    #[test]
    fn current_check_requires_complete_materialized_bundle() {
        let temp = TempDir::new().unwrap();
        let paths = ClaudePluginPaths {
            root_dir: temp.path().join("claude-marketplace"),
        };

        fs::create_dir_all(paths.plugin_manifest_path().parent().unwrap()).unwrap();
        fs::write(paths.plugin_manifest_path(), BUNDLED_PLUGIN_MANIFEST).unwrap();

        let plugin = ClaudePlugin::from_paths(
            paths,
            Some("1.0.0".to_string()),
            Some(temp.path().join("elsewhere")),
        )
        .unwrap();
        assert!(!plugin.is_current("1.0.0"));
    }

    #[test]
    fn invalid_materialized_manifest_is_treated_as_stale_bundle() {
        let temp = TempDir::new().unwrap();
        let paths = ClaudePluginPaths {
            root_dir: temp.path().join("claude-marketplace"),
        };

        fs::create_dir_all(paths.plugin_manifest_path().parent().unwrap()).unwrap();
        fs::write(paths.plugin_manifest_path(), "{ not json").unwrap();

        let plugin = ClaudePlugin::from_paths(
            paths,
            Some("1.0.0".to_string()),
            Some(temp.path().join("claude-marketplace")),
        )
        .unwrap();

        assert!(plugin.needs_materialization("1.0.0"));
    }

    #[test]
    fn path_change_requires_rebind_even_when_version_matches() {
        let temp = TempDir::new().unwrap();
        let paths = ClaudePluginPaths {
            root_dir: temp.path().join("claude-marketplace"),
        };

        let mut plugin = ClaudePlugin::from_paths(
            paths.clone(),
            Some("1.0.0".to_string()),
            Some(temp.path().join("old-marketplace")),
        )
        .unwrap();
        plugin.materialize_bundle().unwrap();

        assert_eq!(plugin.required_action("1.0.0"), Some(PluginAction::Rebind));
    }
}
