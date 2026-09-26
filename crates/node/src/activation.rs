//! The supervisor pipe: the one thing a daemon knows about being
//! supervised.
//!
//! The daemon writes `prepared` once it has migrated its stores and looked
//! at every agent directory without writing, and waits for `go`. Before
//! `go` nothing it did needs undoing if the supervisor rolls the binary
//! back; after it the daemon is ordinary running. End of file on the pipe
//! means the supervisor is gone, and the daemon shuts down so that whoever
//! restarts the supervisor gets a fresh daemon under it. A daemon with no
//! pipe has no supervisor and goes at once.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub const PREPARED: &str = "prepared";
pub const GO: &str = "go";

type Reader = BufReader<Box<dyn AsyncRead + Send + Unpin>>;

pub struct ActivationPipe {
    reader: Reader,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
}

impl ActivationPipe {
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self {
            reader: BufReader::new(Box::new(reader)),
            writer: Box::new(writer),
        }
    }

    /// Writes `prepared` and waits for `go`.
    pub async fn activate(&mut self) -> Result<(), ActivationError> {
        self.writer
            .write_all(format!("{PREPARED}\n").as_bytes())
            .await
            .map_err(ActivationError::Io)?;
        self.writer.flush().await.map_err(ActivationError::Io)?;
        let mut line = String::new();
        if self
            .reader
            .read_line(&mut line)
            .await
            .map_err(ActivationError::Io)?
            == 0
        {
            return Err(ActivationError::SupervisorGone);
        }
        match line.trim_end() {
            GO => Ok(()),
            other => Err(ActivationError::Unexpected(other.to_owned())),
        }
    }

    /// Resolves when the supervisor is gone: end of file, or a read error.
    /// Anything it writes after `go` is ignored.
    pub async fn closed(mut self) {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActivationError {
    #[error("the supervisor went away before activation")]
    SupervisorGone,
    #[error("the supervisor answered {0:?}, not go")]
    Unexpected(String),
    #[error("the supervisor pipe: {0}")]
    Io(io::Error),
}
