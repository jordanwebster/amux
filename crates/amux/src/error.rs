use thiserror::Error;

pub type Result<T> = std::result::Result<T, AmuxError>;

#[derive(Debug, Error)]
pub enum AmuxError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationEncode(#[from] rmp_serde::encode::Error),

    #[error("Deserialization error: {0}")]
    SerializationDecode(#[from] rmp_serde::decode::Error),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("PTY error: {0}")]
    Pty(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error(
        "Too many handshake attempts (link name collision) — this is usually transient, retry the command"
    )]
    TooManyHandshakeAttempts,

    #[error("Invalid or missing credentials — run 'amux init' to re-authenticate")]
    InvalidCredentials,

    #[error("amux upgrade required: {0}")]
    VersionMismatch(String),

    #[error("handshake timed out")]
    HandshakeTimeout,

    #[error("heartbeat timed out")]
    HeartbeatTimeout,
}
