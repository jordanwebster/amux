use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use gethostname::gethostname;
use model::ClaudeDriver;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_HOST_NAME_BYTES: usize = 256;

fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    if let Ok(value) = std::env::var(env_var) {
        return PathBuf::from(value);
    }
    #[cfg(windows)]
    {
        let base = if default_suffix.starts_with(".config") {
            std::env::var("APPDATA").ok()
        } else {
            std::env::var("LOCALAPPDATA")
                .ok()
                .or_else(|| std::env::var("APPDATA").ok())
        };
        if let Some(base) = base {
            return PathBuf::from(base);
        }
    }
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    home.map(PathBuf::from)
        .map(|home| home.join(default_suffix))
        .unwrap_or_else(|| PathBuf::from(default_suffix))
}

fn amux_xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    xdg_dir(env_var, default_suffix).join("amux")
}

#[cfg(not(target_os = "ios"))]
fn default_state_path() -> PathBuf {
    amux_xdg_dir("XDG_STATE_HOME", ".local/state").join("state.yaml")
}

#[cfg(target_os = "ios")]
fn default_state_path() -> PathBuf {
    ios_application_support_dir().join("state.yaml")
}

#[cfg(not(target_os = "ios"))]
fn default_data_dir() -> PathBuf {
    amux_xdg_dir("XDG_DATA_HOME", ".local/share")
}

#[cfg(target_os = "ios")]
fn default_data_dir() -> PathBuf {
    ios_application_support_dir()
}

fn keymap_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("keymaps")
}

#[cfg(target_os = "ios")]
fn ios_application_support_dir() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/amux"))
        .unwrap_or_else(|| PathBuf::from("Library/Application Support/amux"))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("configuration disagrees on {field}: expected {}, found {}", expected.display(), actual.display())]
    Disagreement {
        field: &'static str,
        expected: PathBuf,
        actual: PathBuf,
    },
}

impl Clone for ConfigError {
    fn clone(&self) -> Self {
        match self {
            Self::Io(error) => Self::Io(match error.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::new(error.kind(), error.to_string()),
            }),
            Self::Invalid(message) => Self::Invalid(message.clone()),
            Self::Disagreement {
                field,
                expected,
                actual,
            } => Self::Disagreement {
                field,
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}

const DEFAULT_CLOUD_URL: &str = "https://amux.sh";

/// Local-network listener settings for a device profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LanConfig {
    /// Whether this profile accepts direct connections from the LAN.
    pub listen: bool,
    /// Listener port. Zero asks the operating system for an ephemeral port.
    pub port: u16,
}

impl Default for LanConfig {
    fn default() -> Self {
        Self {
            listen: true,
            port: 0,
        }
    }
}

/// The name a new installation presents to nearby devices.
pub fn default_host_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = macos_computer_name() {
        return name;
    }

    fallback_host_name(
        &gethostname()
            .into_string()
            .unwrap_or_else(|_| "unknown".to_string()),
    )
}

#[cfg(target_os = "macos")]
fn macos_computer_name() -> Option<String> {
    let output = Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn fallback_host_name(host_name: &str) -> String {
    host_name
        .strip_suffix(".local")
        .unwrap_or(host_name)
        .to_string()
}

/// Apply the validation shared by persisted settings and setup prompts.
pub fn validate_host_name(host_name: &str) -> Result<(), ConfigError> {
    if host_name.is_empty() {
        return Err(ConfigError::Invalid("host_name must not be empty".into()));
    }
    if host_name.len() > MAX_HOST_NAME_BYTES {
        return Err(ConfigError::Invalid(format!(
            "host_name must be at most {MAX_HOST_NAME_BYTES} bytes"
        )));
    }
    Ok(())
}

fn default_cloud_url() -> String {
    DEFAULT_CLOUD_URL.to_string()
}

/// Per-user runtime directory for the amux socket on Unix.
///
/// - macOS: `$TMPDIR/amux/` (already per-user, e.g. `/var/folders/xx/.../T/`)
/// - Linux: `$XDG_RUNTIME_DIR/amux/` (per-user tmpfs, e.g. `/run/user/1000/`)
/// - Fallback: `/tmp/amux-<uid>/` (UID-embedded for isolation)
#[cfg(unix)]
pub(crate) fn default_socket_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            return PathBuf::from(tmpdir).join("amux");
        }
    } else if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("amux");
    }
    // Fallback: embed UID for per-user isolation
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/amux-{uid}"))
}

#[cfg(unix)]
fn default_socket_path() -> PathBuf {
    default_socket_dir().join("amux.sock")
}

#[cfg(windows)]
fn default_socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
    PathBuf::from(format!(r"\\.\pipe\amux-{user}"))
}

/// A control-key leader parsed from the `ctrl+<char>` format (e.g. `ctrl+a`).
#[derive(Debug, Clone)]
pub struct LeaderKey {
    /// The lowercase character (e.g. 'a' for ctrl+a)
    pub char: u8,
}

impl LeaderKey {
    /// Raw byte value for this key (ctrl+a = 0x01, ctrl+b = 0x02, etc.)
    pub fn raw_byte(&self) -> u8 {
        self.char - b'a' + 1
    }

    /// CSI u escape sequence: ESC[<ascii>;5u
    pub fn csi_u_sequence(&self) -> Vec<u8> {
        let ascii = self.char.to_string();
        let mut seq = vec![27, b'['];
        seq.extend_from_slice(ascii.as_bytes());
        seq.extend_from_slice(b";5u");
        seq
    }

    fn parse(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        let ch = lower
            .strip_prefix("ctrl+")
            .ok_or_else(|| format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"))?;
        if ch.len() != 1 {
            return Err(format!(
                "invalid leader key '{s}': expected single character after 'ctrl+'"
            ));
        }
        let byte = ch.as_bytes()[0];
        if !byte.is_ascii_lowercase() {
            return Err(format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"));
        }
        Ok(Self { char: byte })
    }
}

impl Default for LeaderKey {
    fn default() -> Self {
        Self { char: b'a' }
    }
}

impl fmt::Display for LeaderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ctrl+{}", self.char as char)
    }
}

impl Serialize for LeaderKey {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LeaderKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        LeaderKey::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Keybind configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybinds {
    /// Leader key prefix for keybinds (default: ctrl+a)
    pub leader: LeaderKey,
}

/// Defaults for newly created Claude agents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeSettings {
    /// The driver used when a creation surface has no explicit override.
    pub driver: ClaudeDriver,
}

/// Which mode Enter opens a Claude agent in from the fleet
/// (`docs/CHAT.md` A1). The shipped default is raw attach — the
/// battle-tested path stays the path of least surprise; flipping the
/// default is this settings change, not a migration. Mobile clients are
/// chat-only and carry no such setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenMode {
    /// Raw byte passthrough to the agent's own TUI.
    #[default]
    Raw,
    /// The structured chat view.
    Chat,
}

/// Where the palette comes from: the terminal amux was started in, a
/// shipped theme name, or a YAML theme file path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ThemeSetting {
    /// Derived from what the terminal reports about its own colours, with
    /// the shipped dark palette as the fallback for a terminal that does
    /// not answer.
    #[default]
    Terminal,
    Dark,
    Light,
    File(PathBuf),
}

impl Serialize for ThemeSetting {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Terminal => serializer.serialize_str("terminal"),
            Self::Dark => serializer.serialize_str("dark"),
            Self::Light => serializer.serialize_str("light"),
            Self::File(path) => path.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ThemeSetting {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "terminal" => Self::Terminal,
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::File(PathBuf::from(value)),
        })
    }
}

/// The terminal colour capability preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorSetting {
    #[default]
    Auto,
    TrueColor,
    Ansi,
}

/// Client UI configuration (the TUI; future desktop clients read the same
/// keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// The mode the fleet's Enter opens; the non-default mode opens via
    /// Ctrl+Enter (kitty-detected) or `o`.
    pub default_open_mode: OpenMode,
    /// A shipped theme name or a path resolved beside the config file.
    pub theme: ThemeSetting,
    /// Whether to detect, force, or disable truecolor output.
    pub color: ColorSetting,
    /// Viewing-host attachment cache bound, in mebibytes.
    pub artifact_cache_mib: u64,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            default_open_mode: OpenMode::default(),
            theme: ThemeSetting::default(),
            color: ColorSetting::default(),
            artifact_cache_mib: 256,
        }
    }
}

/// Preferences shared by every profile in an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstallationConfig {
    pub repository_roots: Vec<PathBuf>,
    pub claude: ClaudeSettings,
    pub root: PathBuf,
    pub front_door_socket: PathBuf,
    pub host_name: String,
    pub prevent_idle_sleep: Option<bool>,
    pub keybinds: Keybinds,
    pub ui: UiSettings,
    pub reports_dir: Option<PathBuf>,
    pub keymaps_dir: PathBuf,
    pub update_manifest_url: String,
    pub minimum_client_versions: HashMap<String, String>,
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for InstallationConfig {
    fn default() -> Self {
        Self {
            repository_roots: Vec::new(),
            claude: ClaudeSettings::default(),
            root: default_data_dir(),
            front_door_socket: default_socket_path(),
            host_name: default_host_name(),
            prevent_idle_sleep: None,
            keybinds: Keybinds::default(),
            ui: UiSettings::default(),
            reports_dir: None,
            keymaps_dir: keymap_dir(&default_data_dir()),
            update_manifest_url: format!("{DEFAULT_CLOUD_URL}/manifest.json"),
            minimum_client_versions: HashMap::new(),
            path: None,
        }
    }
}

impl InstallationConfig {
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.yaml")
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let path = absolute_path(path, &std::env::current_dir()?)?;
        let mut config: Self = read_yaml(&path)?;
        let base = path.parent().unwrap();
        config.root = absolute_path(&config.root, base)?;
        config.front_door_socket = absolute_path(&config.front_door_socket, base)?;
        config.keymaps_dir = absolute_path(&config.keymaps_dir, base)?;
        config.reports_dir = config
            .reports_dir
            .as_deref()
            .map(|path| absolute_path(path, base))
            .transpose()?;
        if let ThemeSetting::File(theme) = &mut config.ui.theme {
            *theme = absolute_path(theme, base)?;
        }
        config.path = Some(path);
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        Config {
            host_name: self.host_name.clone(),
            keybinds: self.keybinds.clone(),
            minimum_client_versions: self.minimum_client_versions.clone(),
            ..Config::default()
        }
        .validate()
    }

    pub fn file_path(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| self.root.join("config.yaml"))
    }
}

/// The per-device file named by AMUX_CONFIG. Its installation is explicit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub installation_config: PathBuf,
    pub socket_path: PathBuf,
    pub data_dir: PathBuf,
    pub state_path: PathBuf,
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,
    #[serde(default)]
    pub lan: LanConfig,
    /// Debug/test override for free-tier entitlement refreshes. Release
    /// runtimes parse the key for config compatibility but never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_refresh_secs: Option<u64>,
}

impl ProfileConfig {
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let mut config: Self = read_yaml(path)?;
        if !config.installation_config.is_absolute() {
            return Err(ConfigError::Invalid(
                "installation_config must be an absolute path".into(),
            ));
        }
        let base = path.parent().ok_or_else(|| {
            ConfigError::Invalid("profile config must have a parent directory".into())
        })?;
        config.installation_config = absolute_path(&config.installation_config, base)?;
        config.socket_path = absolute_path(&config.socket_path, base)?;
        config.data_dir = absolute_path(&config.data_dir, base)?;
        config.state_path = absolute_path(&config.state_path, base)?;
        #[cfg(not(any(debug_assertions, test)))]
        if !config.cloud_url.starts_with("https://") {
            return Err(ConfigError::Invalid("cloud_url must use HTTPS".into()));
        }
        Ok(config)
    }

    pub fn artifact_cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache/artifacts")
    }
}

fn read_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    serde_yaml::from_slice(&std::fs::read(path)?)
        .map_err(|error| ConfigError::Invalid(format!("{}: {error}", path.display())))
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

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Human-readable hostname for generating link names.
    #[serde(default = "default_host_name")]
    pub host_name: String,

    /// Cloud API URL for authentication and connection routing
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,

    /// Path to Unix socket for local connections
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,

    /// TCP port for server-to-server connections (None = don't listen)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,

    /// UDP port for the cloud relay's QUIC listener (None = don't listen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_port: Option<u16>,

    /// Local-network listener settings when this config runs a device.
    #[serde(default)]
    pub lan: LanConfig,

    /// Path to state file.
    #[serde(default = "default_state_path")]
    pub state_path: PathBuf,

    /// Data directory for device identity, trust, and runtime artifacts.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Directory where diagnostic report bundles are written. Defaults to the
    /// `reports` directory beneath `data_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_dir: Option<PathBuf>,

    /// Whether to prevent idle system sleep while the server is running. `None`
    /// = not yet asked (init will prompt); `Some(true/false)` = explicit user
    /// choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevent_idle_sleep: Option<bool>,

    /// Per-auth-client minimum version requirements (e.g. {"cli": "0.2.0"}).
    /// Cloud peers whose token client_id matches a key and whose host version
    /// is below the value will be rejected with UpdateRequired.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub minimum_client_versions: HashMap<String, String>,

    /// Directories searched for Git repositories when clients create agents. Empty by default.
    #[serde(default)]
    pub repository_roots: Vec<PathBuf>,

    /// Keybind configuration
    #[serde(default)]
    pub keybinds: Keybinds,

    /// Client UI configuration
    #[serde(default)]
    pub ui: UiSettings,

    /// Defaults for newly created Claude agents.
    #[serde(default)]
    pub claude: ClaudeSettings,

    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host_name: default_host_name(),
            cloud_url: default_cloud_url(),
            socket_path: default_socket_path(),
            tcp_port: None,
            udp_port: None,
            lan: LanConfig::default(),
            state_path: default_state_path(),
            data_dir: default_data_dir(),
            reports_dir: None,
            prevent_idle_sleep: None,
            minimum_client_versions: HashMap::new(),
            repository_roots: Vec::new(),
            keybinds: Keybinds::default(),
            ui: UiSettings::default(),
            claude: ClaudeSettings::default(),
            path: None,
        }
    }
}

/// Resolve the driver for a newly created Claude agent.
///
/// A creation-time override wins over the configured default. With neither,
/// the shipped default remains the PTY driver.
pub fn resolve_claude_driver(explicit: Option<ClaudeDriver>, config: &Config) -> ClaudeDriver {
    explicit.unwrap_or(config.claude.driver)
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// The configured diagnostic report directory, or `data_dir/reports`.
    pub fn reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("reports"))
    }

    /// The viewing-host artifact cache owned by this configured device.
    pub fn artifact_cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache/artifacts")
    }

    /// Default config file path: `$XDG_CONFIG_HOME/amux/config.yaml`,
    /// falling back to `~/.config/amux/config.yaml`.
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.yaml")
    }

    /// Validate config. Call early to surface errors before any work begins.
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        // Leader key must be ctrl+<a-z>
        let ch = self.keybinds.leader.char;
        if !ch.is_ascii_lowercase() {
            return Err(ConfigError::Invalid(format!(
                "invalid leader key: byte 0x{ch:02x} is not a lowercase letter (expected ctrl+<a-z>)"
            )));
        }

        validate_host_name(&self.host_name)?;

        // Release builds must use HTTPS for cloud URLs to protect tokens in transit
        #[cfg(not(any(debug_assertions, test)))]
        if !self.cloud_url.starts_with("https://") {
            return Err(ConfigError::Invalid("cloud_url must use HTTPS".into()));
        }

        // Validate all minimum_client_versions values are valid semver
        for (name, version) in &self.minimum_client_versions {
            if semver::Version::parse(version).is_err() {
                return Err(ConfigError::Invalid(format!(
                    "invalid minimum_client_versions['{name}']: '{version}' is not valid semver (e.g. \"0.2.0\")"
                )));
            }
        }

        Ok(())
    }

    /// Load config from a YAML file
    pub fn from_file(path: &Path) -> std::result::Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let mut config: Config =
            serde_yaml::from_str(&contents).map_err(|e| ConfigError::Invalid(e.to_string()))?;
        config.path = Some(path.to_path_buf());
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_driver_config_defaults_to_pty_and_accepts_sdk() {
        let absent: Config = serde_yaml::from_str("host_name: test\n").unwrap();
        assert_eq!(resolve_claude_driver(None, &absent), ClaudeDriver::Pty);

        let sdk: Config = serde_yaml::from_str("claude:\n  driver: sdk\n").unwrap();
        assert_eq!(resolve_claude_driver(None, &sdk), ClaudeDriver::Sdk);
        assert_eq!(
            resolve_claude_driver(Some(ClaudeDriver::Pty), &sdk),
            ClaudeDriver::Pty
        );
    }

    #[test]
    fn claude_driver_config_rejects_unknown_keys_and_values() {
        assert!(
            serde_yaml::from_str::<Config>("claude:\n  backend: sdk\n")
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        assert!(serde_yaml::from_str::<Config>("claude:\n  driver: other\n").is_err());
    }

    /// Verify serde_yaml round-trips Windows-style backslash paths correctly.
    /// serde_yaml serializes paths unquoted, which YAML parses literally.
    /// (Double-quoted YAML strings would break because `\p`, `\U` etc. are
    /// invalid YAML escape sequences.)
    #[test]
    fn yaml_windows_path_roundtrip() {
        let config = Config {
            socket_path: PathBuf::from(r"\\.\pipe\amux-test"),
            state_path: PathBuf::from(r"C:\Users\me\state.yaml"),
            data_dir: PathBuf::from(r"C:\Users\me\amux-data"),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.socket_path, config.socket_path);
        assert_eq!(parsed.state_path, config.state_path);
        assert_eq!(parsed.data_dir, config.data_dir);
    }

    #[test]
    fn data_dir_defaults_to_default_data_dir() {
        assert_eq!(Config::default().data_dir, default_data_dir());
        let config: Config = serde_yaml::from_str("tcp_port: 9999\n").unwrap();
        assert_eq!(config.data_dir, default_data_dir());
    }

    #[test]
    fn data_dir_yaml_roundtrip() {
        let yaml = "data_dir: /srv/amux-dev/data\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/srv/amux-dev/data"));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.data_dir, PathBuf::from("/srv/amux-dev/data"));
    }

    #[test]
    fn reports_dir_defaults_beneath_data_dir() {
        let config: Config = serde_yaml::from_str("data_dir: /srv/amux-dev/data\n").unwrap();

        assert_eq!(config.reports_dir, None);
        assert_eq!(
            config.reports_dir(),
            PathBuf::from("/srv/amux-dev/data/reports")
        );
    }

    #[test]
    fn reports_dir_yaml_roundtrip() {
        let yaml = "data_dir: /srv/amux-dev/data\nreports_dir: /srv/amux-reports\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.reports_dir(), PathBuf::from("/srv/amux-reports"));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.reports_dir, Some(PathBuf::from("/srv/amux-reports")));
        assert_eq!(parsed.reports_dir(), PathBuf::from("/srv/amux-reports"));
    }

    #[test]
    fn retired_claude_plugin_config_is_rejected() {
        let error =
            serde_yaml::from_str::<Config>("claude:\n  manage_plugin: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn leader_key_default_is_ctrl_a() {
        let leader = LeaderKey::default();
        assert_eq!(leader.char, b'a');
        assert_eq!(leader.raw_byte(), 0x01);
        // ESC[97;5u
        assert_eq!(
            leader.csi_u_sequence(),
            vec![27, b'[', b'9', b'7', b';', b'5', b'u']
        );
    }

    #[test]
    fn leader_key_ctrl_b() {
        let leader = LeaderKey::parse("ctrl+b").unwrap();
        assert_eq!(leader.char, b'b');
        assert_eq!(leader.raw_byte(), 0x02);
        // ESC[98;5u
        assert_eq!(
            leader.csi_u_sequence(),
            vec![27, b'[', b'9', b'8', b';', b'5', b'u']
        );
    }

    #[test]
    fn leader_key_case_insensitive() {
        let leader = LeaderKey::parse("Ctrl+A").unwrap();
        assert_eq!(leader.char, b'a');
    }

    #[test]
    fn leader_key_invalid() {
        assert!(LeaderKey::parse("alt+a").is_err());
        assert!(LeaderKey::parse("ctrl+1").is_err());
        assert!(LeaderKey::parse("ctrl+ab").is_err());
        assert!(LeaderKey::parse("a").is_err());
    }

    #[test]
    fn leader_key_yaml_roundtrip() {
        let yaml = "leader: ctrl+b\n";
        let keybinds: Keybinds = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(keybinds.leader.char, b'b');

        let serialized = serde_yaml::to_string(&keybinds).unwrap();
        let parsed: Keybinds = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.leader.char, b'b');
    }

    #[test]
    fn config_with_keybinds() {
        let yaml = "keybinds:\n  leader: ctrl+b\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.keybinds.leader.char, b'b');
    }

    #[test]
    fn config_without_keybinds_uses_default() {
        let yaml = "tcp_port: 9999\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.tcp_port, Some(9999));
        assert_eq!(config.keybinds.leader.char, b'a');
    }

    /// The shipped default open mode is raw attach (`docs/CHAT.md` A1).
    #[test]
    fn default_open_mode_is_raw() {
        let config = Config::default();
        assert_eq!(config.ui.default_open_mode, OpenMode::Raw);
        let yaml = "tcp_port: 9999\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.default_open_mode, OpenMode::Raw);
    }

    #[test]
    fn default_open_mode_yaml_roundtrip() {
        let yaml = "ui:\n  default_open_mode: chat\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.default_open_mode, OpenMode::Chat);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.default_open_mode, OpenMode::Chat);
    }

    #[test]
    fn ui_theme_and_color_defaults_are_terminal_and_auto() {
        let config = Config::default();
        assert_eq!(config.ui.theme, ThemeSetting::Terminal);
        assert_eq!(config.ui.color, ColorSetting::Auto);

        let parsed: Config = serde_yaml::from_str("ui: {}\n").unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Terminal);
        assert_eq!(parsed.ui.color, ColorSetting::Auto);
    }

    #[test]
    fn ui_theme_and_color_yaml_roundtrip() {
        let yaml = "ui:\n  theme: light\n  color: ansi\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.theme, ThemeSetting::Light);
        assert_eq!(config.ui.color, ColorSetting::Ansi);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Light);
        assert_eq!(parsed.ui.color, ColorSetting::Ansi);
    }

    #[test]
    fn artifact_cache_bound_defaults_and_parses_from_yaml() {
        assert_eq!(Config::default().ui.artifact_cache_mib, 256);
        let parsed: Config = serde_yaml::from_str("ui:\n  artifact_cache_mib: 48\n").unwrap();
        assert_eq!(parsed.ui.artifact_cache_mib, 48);

        let serialized = serde_yaml::to_string(&parsed).unwrap();
        let reparsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.ui.artifact_cache_mib, 48);
    }

    #[test]
    fn ui_theme_file_path_yaml_roundtrip() {
        let yaml = "ui:\n  theme: themes/forest.yaml\n  color: truecolor\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.ui.theme,
            ThemeSetting::File(PathBuf::from("themes/forest.yaml"))
        );
        assert_eq!(config.ui.color, ColorSetting::TrueColor);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.theme, config.ui.theme);
        assert_eq!(parsed.ui.color, config.ui.color);
    }

    #[test]
    fn unknown_ui_color_is_rejected() {
        let error = serde_yaml::from_str::<Config>("ui:\n  color: millions\n").unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn unknown_open_mode_is_rejected() {
        let error =
            serde_yaml::from_str::<Config>("ui:\n  default_open_mode: telepathy\n").unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn ports_default_to_none() {
        let config = Config::default();
        assert_eq!(config.tcp_port, None);
        assert_eq!(config.udp_port, None);
        assert_eq!(config.lan, LanConfig::default());
        assert_eq!(config.prevent_idle_sleep, None);
    }

    #[test]
    fn lan_defaults_to_an_ephemeral_listener_and_parses_overrides() {
        let defaulted: Config = serde_yaml::from_str("host_name: test\n").unwrap();
        assert_eq!(defaulted.lan, LanConfig::default());

        let configured: Config =
            serde_yaml::from_str("lan:\n  listen: false\n  port: 4242\n").unwrap();
        assert_eq!(
            configured.lan,
            LanConfig {
                listen: false,
                port: 4242,
            }
        );
    }

    #[test]
    fn prevent_idle_sleep_yaml_roundtrip() {
        let yaml = "prevent_idle_sleep: true\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.prevent_idle_sleep, Some(true));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.prevent_idle_sleep, Some(true));
    }

    #[test]
    fn prevent_idle_sleep_absent_deserializes_as_none() {
        let yaml = "tcp_port: 9999\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.prevent_idle_sleep, None);
    }

    #[test]
    fn config_split_rejects_retired_cloud_mode() {
        let error = serde_yaml::from_str::<Config>("enable_cloud_mode: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn unknown_config_field_is_rejected() {
        let error = serde_yaml::from_str::<Config>("check_for_updates: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn validate_default_config_ok() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_with_relay_ports() {
        let config = Config {
            tcp_port: Some(9001),
            udp_port: Some(9001),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.tcp_port, Some(9001));
        assert_eq!(parsed.udp_port, Some(9001));
    }

    #[test]
    fn validate_leader_key_bad_char() {
        let mut config = Config::default();
        config.keybinds.leader = LeaderKey { char: b'1' };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("leader key"));
    }

    #[test]
    fn validate_minimum_client_versions_valid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "0.2.0".to_string())]),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_host_name() {
        let config = Config {
            host_name: String::new(),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn validate_accepts_long_host_name() {
        let config = Config {
            host_name: "a".repeat(MAX_HOST_NAME_BYTES),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_oversized_host_name() {
        let config = Config {
            host_name: "a".repeat(MAX_HOST_NAME_BYTES + 1),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn fallback_host_name_strips_the_mdns_suffix() {
        assert_eq!(
            fallback_host_name("Jordans-MacBook.local"),
            "Jordans-MacBook"
        );
        assert_eq!(fallback_host_name("build-host"), "build-host");
    }

    #[test]
    fn validate_minimum_client_versions_invalid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "v0.2.0".to_string())]),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("minimum_client_versions"));
        assert!(err.to_string().contains("cli"));
    }
}
