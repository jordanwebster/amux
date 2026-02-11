use crate::error::{AmuxError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Read a length-prefixed frame from an async reader.
async fn read_frame_impl<R: AsyncRead + Unpin>(reader: &mut R, max_size: usize) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > max_size {
        return Err(AmuxError::InvalidMessage);
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a length-prefixed frame to an async writer.
async fn write_frame_impl<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
    flush: bool,
) -> Result<()> {
    let len = data.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(data).await?;
    if flush {
        writer.flush().await?;
    }
    Ok(())
}

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
        read_frame_impl(&mut self.reader, max_size).await
    }

    pub async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        write_frame_impl(&mut self.writer, data, self.flush).await
    }

    pub fn into_split(self) -> (FrameReader<R>, FrameWriter<W>) {
        (
            FrameReader {
                reader: self.reader,
            },
            FrameWriter {
                writer: self.writer,
                flush: self.flush,
            },
        )
    }
}

/// Read half of length-prefixed framing.
pub struct FrameReader<R> {
    reader: R,
}

impl<R: AsyncRead + Unpin + Send> FrameReader<R> {
    pub async fn read_frame(&mut self, max_size: usize) -> Result<Vec<u8>> {
        read_frame_impl(&mut self.reader, max_size).await
    }
}

/// Write half of length-prefixed framing.
pub struct FrameWriter<W> {
    writer: W,
    flush: bool,
}

impl<W: AsyncWrite + Unpin + Send> FrameWriter<W> {
    pub async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        write_frame_impl(&mut self.writer, data, self.flush).await
    }
}
