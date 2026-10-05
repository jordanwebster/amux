use std::collections::HashMap;
use std::path::PathBuf;

/// How to start Codex's app-server and who to say the client is.
pub struct CodexConfig {
    /// Path to the `codex` binary. Defaults to `"codex"` (found via PATH).
    pub codex_path: Option<PathBuf>,
    /// Model passed explicitly to the Codex CLI before the app-server command.
    pub model: Option<String>,
    /// Working directory for the subprocess.
    pub cwd: Option<PathBuf>,
    /// Client name sent in the initialize handshake.
    pub client_name: String,
    /// Optional client title sent in the initialize handshake.
    pub client_title: Option<String>,
    /// Client version sent in the initialize handshake.
    pub client_version: String,
    /// Whether to enable the experimental API surface.
    pub experimental_api: bool,
    /// Extra environment variables for the subprocess.
    pub env: Option<HashMap<String, String>>,
    /// `--config key=value` pairs passed to the codex CLI.
    pub config_overrides: Vec<(String, String)>,
    /// Optional JSONL path that receives an exact timestamped tee of JSON-RPC
    /// lines in both directions.
    pub record_io: Option<PathBuf>,
}

// Manual Default because the identity and experimental-API defaults are not
// the field types' own defaults.
impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            codex_path: None,
            model: None,
            cwd: None,
            client_name: "codex-rust-sdk".into(),
            client_title: None,
            client_version: env!("CARGO_PKG_VERSION").into(),
            experimental_api: true,
            env: None,
            config_overrides: Vec::new(),
            record_io: None,
        }
    }
}
