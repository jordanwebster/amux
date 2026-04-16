//! Length-prefixed framing for byte-stream transports (Unix, TCP).
//!
//! Each frame is a 4-byte big-endian length prefix followed by that many payload
//! bytes. Used by [`UnixTransport`](super::unix::UnixTransport) and
//! [`TcpTransport`](super::tcp::TcpTransport); WebSocket uses its own framing.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::transport::{Result, TransportError};

/// 16MB limit to prevent DoS via huge frames (length-prefixed and WebSocket).
pub(crate) const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Maximum initial allocation for a frame buffer. Frames larger than this are
/// read incrementally in chunks so that a malicious length prefix (e.g. 16MB)
/// cannot force a large allocation before any payload data arrives.
const MAX_PREALLOC: usize = 64 * 1024;

/// Read a length-prefixed frame from an async reader.
async fn read_frame_impl<R: AsyncRead + Unpin>(reader: &mut R, max_size: usize) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > max_size {
        return Err(TransportError::InvalidMessage(format!(
            "frame too large: {} bytes exceeds {} byte limit",
            len, max_size
        )));
    }

    if len <= MAX_PREALLOC {
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        Ok(buf)
    } else {
        // Read in chunks to prevent memory amplification from untrusted length
        // prefixes — the buffer only grows as real data arrives over the wire.
        let mut buf = Vec::with_capacity(MAX_PREALLOC);
        let mut remaining = len;
        while remaining > 0 {
            let chunk = remaining.min(MAX_PREALLOC);
            let start = buf.len();
            buf.resize(start + chunk, 0);
            reader.read_exact(&mut buf[start..]).await?;
            remaining -= chunk;
        }
        Ok(buf)
    }
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
pub(crate) struct LengthPrefixed<R, W>
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
    pub(in crate::transport) fn new(reader: R, writer: W, flush: bool) -> Self {
        Self {
            reader,
            writer,
            flush,
        }
    }

    pub(in crate::transport) async fn read_frame(&mut self, max_size: usize) -> Result<Vec<u8>> {
        read_frame_impl(&mut self.reader, max_size).await
    }

    pub(in crate::transport) async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        write_frame_impl(&mut self.writer, data, self.flush).await
    }

    pub(in crate::transport) fn into_split(self) -> (FrameReader<R>, FrameWriter<W>) {
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
pub(in crate::transport) struct FrameReader<R> {
    reader: R,
}

impl<R: AsyncRead + Unpin + Send> FrameReader<R> {
    pub(in crate::transport) async fn read_frame(&mut self, max_size: usize) -> Result<Vec<u8>> {
        read_frame_impl(&mut self.reader, max_size).await
    }
}

/// Write half of length-prefixed framing.
pub(in crate::transport) struct FrameWriter<W> {
    writer: W,
    flush: bool,
}

impl<W: AsyncWrite + Unpin + Send> FrameWriter<W> {
    pub(in crate::transport) async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        write_frame_impl(&mut self.writer, data, self.flush).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_payload_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_frame_impl(&mut a, &[], true).await.unwrap();
        let data = read_frame_impl(&mut b, 1024).await.unwrap();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn normal_payload_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let payload = b"hello world";
        write_frame_impl(&mut a, payload, true).await.unwrap();
        let data = read_frame_impl(&mut b, 1024).await.unwrap();
        assert_eq!(data, payload);
    }

    #[tokio::test]
    async fn binary_payload_preserves_all_byte_values() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let payload: Vec<u8> = (0..=255).collect();
        write_frame_impl(&mut a, &payload, true).await.unwrap();
        let data = read_frame_impl(&mut b, 1024).await.unwrap();
        assert_eq!(data, payload);
    }

    #[tokio::test]
    async fn multiple_frames_delimited_correctly() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_frame_impl(&mut a, b"first", true).await.unwrap();
        write_frame_impl(&mut a, b"second", true).await.unwrap();
        write_frame_impl(&mut a, b"third", true).await.unwrap();

        assert_eq!(read_frame_impl(&mut b, 1024).await.unwrap(), b"first");
        assert_eq!(read_frame_impl(&mut b, 1024).await.unwrap(), b"second");
        assert_eq!(read_frame_impl(&mut b, 1024).await.unwrap(), b"third");
    }

    #[tokio::test]
    async fn frame_at_max_size_succeeds() {
        let max_size = 256;
        let (mut a, mut b) = tokio::io::duplex(max_size + 64);
        let payload = vec![0xAB; max_size];
        write_frame_impl(&mut a, &payload, true).await.unwrap();
        let data = read_frame_impl(&mut b, max_size).await.unwrap();
        assert_eq!(data.len(), max_size);
        assert_eq!(data, payload);
    }

    #[tokio::test]
    async fn frame_exceeding_max_size_rejected() {
        let max_size = 256;
        let (mut a, mut b) = tokio::io::duplex(max_size + 64);
        let payload = vec![0xAB; max_size + 1];
        write_frame_impl(&mut a, &payload, true).await.unwrap();
        let err = read_frame_impl(&mut b, max_size).await.unwrap_err();
        assert!(matches!(err, TransportError::InvalidMessage(_)));
    }

    #[tokio::test]
    async fn eof_during_length_prefix_is_io_error() {
        let (a, mut b) = tokio::io::duplex(4096);
        drop(a);
        let err = read_frame_impl(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, TransportError::Io(_)));
    }

    #[tokio::test]
    async fn eof_during_payload_is_io_error() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        // Write a length prefix claiming 100 bytes, then close without sending payload
        a.write_all(&100u32.to_be_bytes()).await.unwrap();
        drop(a);
        let err = read_frame_impl(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, TransportError::Io(_)));
    }

    #[tokio::test]
    async fn split_reader_writer_roundtrip() {
        let (a, b) = tokio::io::duplex(4096);
        // LengthPrefixed(reader=b, writer=a): writes go to a, reads come from b
        let framing = LengthPrefixed::new(b, a, true);
        let (mut reader, mut writer) = framing.into_split();

        let payload = b"split test";
        writer.write_frame(payload).await.unwrap();
        let data = reader.read_frame(1024).await.unwrap();
        assert_eq!(data, payload);
    }

    #[tokio::test]
    async fn large_frame_uses_chunked_read() {
        // Frame larger than MAX_PREALLOC exercises the chunked read path
        let size = MAX_PREALLOC + 1000;
        let (mut a, mut b) = tokio::io::duplex(size + 64);
        let payload: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        write_frame_impl(&mut a, &payload, true).await.unwrap();
        let data = read_frame_impl(&mut b, size).await.unwrap();
        assert_eq!(data, payload);
    }

    #[tokio::test]
    async fn large_frame_eof_mid_chunk_is_io_error() {
        // Send length prefix for a large frame but only partial payload
        let claimed_size = MAX_PREALLOC * 3;
        let actual_bytes = MAX_PREALLOC + 100; // less than claimed
        let (mut a, mut b) = tokio::io::duplex(claimed_size + 64);
        a.write_all(&(claimed_size as u32).to_be_bytes())
            .await
            .unwrap();
        let partial: Vec<u8> = vec![0xAA; actual_bytes];
        a.write_all(&partial).await.unwrap();
        drop(a);
        let err = read_frame_impl(&mut b, claimed_size).await.unwrap_err();
        assert!(matches!(err, TransportError::Io(_)));
    }
}
