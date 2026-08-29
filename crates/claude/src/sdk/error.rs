#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ProtocolError {
    message: String,
    frame: Option<serde_json::Value>,
}

impl ProtocolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frame: None,
        }
    }

    pub fn with_frame(message: impl Into<String>, frame: serde_json::Value) -> Self {
        Self {
            message: message.into(),
            frame: Some(frame),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn frame(&self) -> Option<&serde_json::Value> {
        self.frame.as_ref()
    }
}

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
    #[error("persisted session error: {0}")]
    Persistence(String),
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
