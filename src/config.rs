use crate::error::{AmuxError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Default Unix socket path
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/amux.sock";

/// Default maximum replay buffer size (10MB)
pub const DEFAULT_MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024;

/// Default user ID for local mode
pub const DEFAULT_USER_ID: &str = "local";

/// Default TCP port for server-to-server connections
pub const DEFAULT_TCP_PORT: u16 = 9001;

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Unique identifier for this server instance
    pub host_id: String,

    /// User ID (hardcoded for local mode)
    pub user_id: String,

    /// Path to the Unix socket
    pub socket_path: PathBuf,

    /// Maximum size of the replay buffer in bytes
    pub max_replay_buffer: usize,

    /// TCP port for server-to-server connections (defaults to 9001)
    #[serde(default)]
    pub tcp_port: Option<u16>,
}

impl Config {
    /// Create a new config with defaults (generates random host_id)
    pub fn new() -> Self {
        Self {
            host_id: Uuid::new_v4().to_string(),
            user_id: DEFAULT_USER_ID.to_string(),
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
            max_replay_buffer: DEFAULT_MAX_REPLAY_BUFFER,
            tcp_port: None,
        }
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
