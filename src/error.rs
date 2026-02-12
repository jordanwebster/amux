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

    #[error("Agent already exists: {0}")]
    AgentAlreadyExists(String),

    #[error("Invalid message")]
    InvalidMessage,

    #[error("PTY error: {0}")]
    Pty(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Too many handshake attempts")]
    TooManyHandshakeAttempts,

    #[error("Invalid or missing credentials")]
    InvalidCredentials,

    #[error("amux upgrade required: {0}")]
    VersionMismatch(String),
}
