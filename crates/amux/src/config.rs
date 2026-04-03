use crate::error::{AmuxError, Result};
use crate::state::State;
use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Resolve an XDG base directory: `$env_var` if set, otherwise `$HOME/{default_suffix}`.
///
/// On Windows, maps XDG conventions to standard Windows directories:
/// `.config` → `%APPDATA%`, `.local/state` → `%LOCALAPPDATA%` (fallback `%APPDATA%`).
pub(crate) fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    if let Ok(val) = std::env::var(env_var) {
        return PathBuf::from(val);
    }
    #[cfg(windows)]
    {
        let win_base = if default_suffix.starts_with(".config") {
            std::env::var("APPDATA").ok()
        } else {
            std::env::var("LOCALAPPDATA")
                .ok()
                .or_else(|| std::env::var("APPDATA").ok())
        };
        if let Some(base) = win_base {
            return PathBuf::from(base);
        }
    }
    home_dir().join(default_suffix)
}

/// Resolve `$HOME`, panics if unset (same as `dirs::home_dir`).
#[cfg(unix)]
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .expect("$HOME is not set")
}

/// Resolve the current user's home directory.
#[cfg(windows)]
fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .expect("%USERPROFILE% is not set")
}

const DEFAULT_TCP_PORT: u16 = 9001;
const DEFAULT_WEBSOCKET_PORT: u16 = 9002;
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

fn default_tcp_port() -> u16 {
    DEFAULT_TCP_PORT
}

fn default_websocket_port() -> u16 {
    DEFAULT_WEBSOCKET_PORT
}

fn default_randomise_link_name() -> bool {
    true
}

fn default_enforce_tls_in_cloud_mode() -> bool {
    true
}

fn default_state_path() -> PathBuf {
    State::default_path()
}

/// Default log path: `$XDG_STATE_HOME/amux/amux.log` (co-located with state.yaml).
pub fn default_log_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("amux/amux.log")
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
#[serde(default)]
pub struct Keybinds {
    /// Leader key prefix for keybinds (default: ctrl+a)
    pub leader: LeaderKey,
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Human-readable hostname for generating link names.
    #[serde(default = "default_host_name")]
    pub host_name: String,

    /// Cloud API URL for OAuth and connection routing
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,

    /// Path to Unix socket for local connections
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,

    /// TCP port for server-to-server connections
    #[serde(default = "default_tcp_port")]
    pub tcp_port: u16,

    /// WebSocket port for rich clients
    #[serde(default = "default_websocket_port")]
    pub websocket_port: u16,

    /// Whether to add random suffixes to link names (default: true).
    /// Set to false in tests for deterministic link names.
    /// Only configurable in debug/test builds; release always uses true.
    #[serde(default = "default_randomise_link_name")]
    #[cfg_attr(not(any(debug_assertions, test)), serde(skip_deserializing))]
    pub randomise_link_name: bool,

    /// Path to state file.
    /// Only configurable in debug/test builds; release always uses default.
    #[serde(default = "default_state_path")]
    #[cfg_attr(not(any(debug_assertions, test)), serde(skip_deserializing))]
    pub state_path: PathBuf,

    /// Whether the cloud server should handle TLS itself (default: true).
    /// Set to false when TLS is terminated by a reverse proxy (e.g. nginx).
    #[serde(default = "default_enforce_tls_in_cloud_mode")]
    pub enforce_tls_in_cloud_mode: bool,

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
            tcp_port: default_tcp_port(),
            websocket_port: default_websocket_port(),
            randomise_link_name: default_randomise_link_name(),
            state_path: default_state_path(),
            enforce_tls_in_cloud_mode: default_enforce_tls_in_cloud_mode(),
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
        xdg_dir("XDG_CONFIG_HOME", ".config").join("amux/config.yaml")
    }

    /// Load config from a YAML file
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let mut config: Config =
            serde_yaml::from_str(&contents).map_err(|e| AmuxError::Config(e.to_string()))?;
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
        assert_eq!(config.keybinds.leader.char, b'a');
    }
}
