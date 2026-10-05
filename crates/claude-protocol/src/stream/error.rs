use std::fmt;

/// A frame this crate could not read, with the frame when it was JSON.
#[derive(Debug, Clone)]
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

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolError {}
