//! What the headless stream client reports when the stream fails.

pub use claude_protocol::stream::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid query options: {0}")]
    InvalidOptions(String),
    #[error("send error: {0}")]
    Send(String),
    #[error("stream error: {0}")]
    Stream(String),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("process error: {0}")]
    Process(String),
    #[error("Claude process exited unsuccessfully ({status}): {stderr}")]
    ProcessExit { status: String, stderr: String },
    #[error("query aborted")]
    Aborted,
    #[error("control error: {0}")]
    Control(String),
    #[error("unknown or already answered control request `{0}`")]
    UnknownRequest(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
