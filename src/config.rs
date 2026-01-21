use crate::error::{AmuxError, Result};
use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default Unix socket path
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/amux.sock";

/// Default maximum replay buffer size (10MB)
pub const DEFAULT_MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024;

/// Default user ID for local mode
pub const DEFAULT_USER_ID: &str = "local";

/// Default TCP port for server-to-server connections
pub const DEFAULT_TCP_PORT: u16 = 9001;

/// Default WebSocket port for rich clients
pub const DEFAULT_WEBSOCKET_PORT: u16 = 9002;

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Human-readable hostname for generating link names.
    /// Defaults to machine hostname if not specified.
    #[serde(default)]
    pub host_name: Option<String>,

    /// Hardcoded for local mode; will be authenticated in cloud mode
    pub user_id: String,

    pub socket_path: PathBuf,

    /// In bytes
    pub max_replay_buffer: usize,

    /// Defaults to 9001
    #[serde(default)]
    pub tcp_port: Option<u16>,

    /// Defaults to 9002
    #[serde(default)]
    pub websocket_port: Option<u16>,
}

impl Config {
    /// Create a new config with defaults
    pub fn new() -> Self {
        Self {
            host_name: None,
            user_id: DEFAULT_USER_ID.to_string(),
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
            max_replay_buffer: DEFAULT_MAX_REPLAY_BUFFER,
            tcp_port: None,
            websocket_port: None,
        }
    }

    /// Get the effective host name (configured or machine hostname)
    pub fn get_host_name(&self) -> String {
        self.host_name.clone().unwrap_or_else(|| {
            gethostname()
                .into_string()
                .unwrap_or_else(|_| "unknown".to_string())
        })
    }

    /// Load config from a YAML file
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&contents).map_err(|e| AmuxError::Config(e.to_string()))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
