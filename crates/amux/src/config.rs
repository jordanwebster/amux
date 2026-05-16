use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::{amux_xdg_dir, default_state_path};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

const DEFAULT_CLOUD_URL: &str = "https://amux.sh";

fn default_host_name() -> String {
    gethostname()
        .into_string()
        .unwrap_or_else(|_| "unknown".to_string())
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
fn default_socket_dir() -> PathBuf {
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

fn default_randomise_link_name() -> bool {
    true
}

fn default_enforce_tls_in_cloud_mode() -> bool {
    true
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

    /// Whether to add random suffixes to link names (default: true).
    /// Set to false in tests for deterministic link names.
    #[serde(default = "default_randomise_link_name")]
    pub randomise_link_name: bool,

    /// Path to state file.
    #[serde(default = "default_state_path")]
    pub state_path: PathBuf,

    /// Whether the cloud server should handle TLS itself (default: true).
    /// Set to false when TLS is terminated by a reverse proxy (e.g. nginx).
    #[serde(default = "default_enforce_tls_in_cloud_mode")]
    pub enforce_tls_in_cloud_mode: bool,

    /// Whether the user has opted into cloud mode. `None` = not yet asked (init
    /// will prompt); `Some(true/false)` = explicit user choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_cloud_mode: Option<bool>,

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

    /// Keybind configuration
    #[serde(default)]
    pub keybinds: Keybinds,

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
            randomise_link_name: default_randomise_link_name(),
            state_path: default_state_path(),
            enforce_tls_in_cloud_mode: default_enforce_tls_in_cloud_mode(),
            enable_cloud_mode: None,
            prevent_idle_sleep: None,
            minimum_client_versions: HashMap::new(),
            keybinds: Keybinds::default(),
            path: None,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Default config file path: `$XDG_CONFIG_HOME/amux/config.yaml`,
    /// falling back to `~/.config/amux/config.yaml`.
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.yaml")
    }

    /// Validate config. Call early to surface errors before any work begins.
    /// When `is_cloud` is true, also validates cloud-server requirements.
    pub fn validate(&self, is_cloud: bool) -> std::result::Result<(), ConfigError> {
        // Leader key must be ctrl+<a-z>
        let ch = self.keybinds.leader.char;
        if !ch.is_ascii_lowercase() {
            return Err(ConfigError::Invalid(format!(
                "invalid leader key: byte 0x{ch:02x} is not a lowercase letter (expected ctrl+<a-z>)"
            )));
        }

        if self.host_name.is_empty() {
            return Err(ConfigError::Invalid("host_name must not be empty".into()));
        }

        // Cloud servers must have TCP configured.
        if is_cloud && self.tcp_port.is_none() {
            return Err(ConfigError::Invalid(
                "cloud mode requires tcp_port to be set".into(),
            ));
        }

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

    /// Verify serde_yaml round-trips Windows-style backslash paths correctly.
    /// serde_yaml serializes paths unquoted, which YAML parses literally.
    /// (Double-quoted YAML strings would break because `\p`, `\U` etc. are
    /// invalid YAML escape sequences.)
    #[test]
    fn yaml_windows_path_roundtrip() {
        let config = Config {
            socket_path: PathBuf::from(r"\\.\pipe\amux-test"),
            state_path: PathBuf::from(r"C:\Users\me\state.yaml"),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.socket_path, config.socket_path);
        assert_eq!(parsed.state_path, config.state_path);
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

    #[test]
    fn ports_default_to_none() {
        let config = Config::default();
        assert_eq!(config.tcp_port, None);
        assert_eq!(config.prevent_idle_sleep, None);
        assert_eq!(config.enable_cloud_mode, None);
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
    fn enable_cloud_mode_yaml_roundtrip() {
        let yaml = "enable_cloud_mode: false\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.enable_cloud_mode, Some(false));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.enable_cloud_mode, Some(false));
    }

    #[test]
    fn unknown_config_field_is_rejected() {
        let error = serde_yaml::from_str::<Config>("check_for_updates: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn validate_default_config_ok() {
        let config = Config::default();
        assert!(config.validate(false).is_ok());
    }

    #[test]
    fn validate_cloud_requires_tcp_port() {
        let config = Config::default();
        let err = config.validate(true).unwrap_err();
        assert!(err.to_string().contains("tcp_port"));
    }

    #[test]
    fn validate_cloud_ok_with_tcp_port() {
        let config = Config {
            tcp_port: Some(9001),
            ..Config::default()
        };
        assert!(config.validate(true).is_ok());
    }

    #[test]
    fn validate_leader_key_bad_char() {
        let mut config = Config::default();
        config.keybinds.leader = LeaderKey { char: b'1' };
        let err = config.validate(false).unwrap_err();
        assert!(err.to_string().contains("leader key"));
    }

    #[test]
    fn validate_minimum_client_versions_valid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "0.2.0".to_string())]),
            ..Config::default()
        };
        assert!(config.validate(false).is_ok());
    }

    #[test]
    fn validate_rejects_empty_host_name() {
        let config = Config {
            host_name: String::new(),
            ..Config::default()
        };
        let err = config.validate(false).unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn validate_accepts_long_host_name() {
        let config = Config {
            host_name: "a".repeat(200),
            ..Config::default()
        };
        assert!(config.validate(false).is_ok());
    }

    #[test]
    fn validate_minimum_client_versions_invalid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "v0.2.0".to_string())]),
            ..Config::default()
        };
        let err = config.validate(false).unwrap_err();
        assert!(err.to_string().contains("minimum_client_versions"));
        assert!(err.to_string().contains("cli"));
    }
}
