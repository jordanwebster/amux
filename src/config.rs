use crate::error::{AmuxError, Result};
use crate::state::State;
use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Resolve an XDG base directory: `$env_var` if set, otherwise `$HOME/{default_suffix}`.
pub(crate) fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    if let Ok(val) = std::env::var(env_var) {
        return PathBuf::from(val);
    }
    home_dir().join(default_suffix)
}

/// Resolve `$HOME`, panics if unset (same as `dirs::home_dir`).
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .expect("$HOME is not set")
}

/// Default Unix socket path
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/amux.sock";

/// Default TCP port for server-to-server connections
pub const DEFAULT_TCP_PORT: u16 = 9001;

/// Default WebSocket port for rich clients
pub const DEFAULT_WEBSOCKET_PORT: u16 = 9002;

/// Default cloud URL
pub const DEFAULT_CLOUD_URL: &str = "https://amux.sh";

fn default_host_name() -> String {
    gethostname()
        .into_string()
        .unwrap_or_else(|_| "unknown".to_string())
}

fn default_cloud_url() -> String {
    DEFAULT_CLOUD_URL.to_string()
}

fn default_socket_path() -> PathBuf {
    PathBuf::from(DEFAULT_SOCKET_PATH)
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
            path: None,
        }
    }
}

impl Config {
    /// Create a new config with defaults
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
