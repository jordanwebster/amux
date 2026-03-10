#[cfg(unix)]
pub use super::unix::{
    UnixMessageReader as LocalMessageReader, UnixMessageWriter as LocalMessageWriter,
    UnixTransport as LocalTransport,
};

#[cfg(windows)]
pub type LocalTransport = super::named_pipe::NamedPipeClientTransport;

#[cfg(windows)]
pub type LocalMessageReader =
    super::named_pipe::NamedPipeMessageReader<tokio::net::windows::named_pipe::NamedPipeClient>;

#[cfg(windows)]
pub type LocalMessageWriter =
    super::named_pipe::NamedPipeMessageWriter<tokio::net::windows::named_pipe::NamedPipeClient>;
