#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("process error: {0}")]
    Process(String),

    #[error("transport closed")]
    TransportClosed,

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("daemon error: {0}")]
    Daemon(String),

    #[error("JSON-RPC error ({code}): {message}")]
    Rpc {
        code: i64,
        message: String,
        codex_error_info: Option<String>,
        data: Option<serde_json::Value>,
    },

    #[error("turn already active")]
    TurnActive,

    #[error("thread event queue overflowed for thread {0}")]
    ThreadQueueOverflow(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Safety net for internal anyhow errors not explicitly converted.
    #[error("{0:#}")]
    Internal(#[from] anyhow::Error),
}
