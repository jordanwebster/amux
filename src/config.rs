use std::path::PathBuf;
use uuid::Uuid;

/// Default Unix socket path
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/amux.sock";

/// Default maximum replay buffer size (10MB)
pub const DEFAULT_MAX_REPLAY_BUFFER: usize = 10 * 1024 * 1024;

/// Default user ID for local mode
pub const DEFAULT_USER_ID: &str = "local";

/// Server configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Unique identifier for this server instance
    pub host_id: String,

    /// User ID (hardcoded for local mode)
    pub user_id: String,

    /// Path to the Unix socket
    pub socket_path: PathBuf,

    /// Maximum size of the replay buffer in bytes
    pub max_replay_buffer: usize,
}

impl Config {
    /// Create a new config with a generated host_id
    pub fn new() -> Self {
        Self {
            host_id: Uuid::new_v4().to_string(),
            user_id: DEFAULT_USER_ID.to_string(),
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
            max_replay_buffer: DEFAULT_MAX_REPLAY_BUFFER,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
