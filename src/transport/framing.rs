use crate::error::{AmuxError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Length-prefixed framing for transports.
pub struct LengthPrefixed<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    reader: R,
    writer: W,
    flush: bool,
}

impl<R, W> LengthPrefixed<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(reader: R, writer: W, flush: bool) -> Self {
        Self {
            reader,
            writer,
            flush,
        }
    }

    pub async fn read_frame(&mut self, max_size: usize) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > max_size {
            return Err(AmuxError::InvalidMessage);
        }

        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).await?;
        Ok(buf)
    }

    pub async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let len = data.len() as u32;
        self.writer.write_all(&len.to_be_bytes()).await?;
        self.writer.write_all(data).await?;
        if self.flush {
            self.writer.flush().await?;
        }
        Ok(())
    }
}
