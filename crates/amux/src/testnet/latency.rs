//! Receive-side delay on real relay bytes, shared by existing and new sockets.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

pub(super) struct Delayed<T> {
    inner: T,
    millis: Arc<AtomicU64>,
    bytes: Vec<u8>,
    offset: usize,
    delay: Option<Pin<Box<Sleep>>>,
}

impl<T> Delayed<T> {
    pub(super) fn new(inner: T, millis: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            millis,
            bytes: Vec::new(),
            offset: 0,
            delay: None,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Delayed<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.bytes.is_empty() {
            let mut storage = [0; 16 * 1024];
            let mut input = ReadBuf::new(&mut storage);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut input))?;
            if input.filled().is_empty() {
                return Poll::Ready(Ok(()));
            }
            this.bytes.extend_from_slice(input.filled());
            let millis = this.millis.load(Ordering::SeqCst);
            this.delay =
                (millis > 0).then(|| Box::pin(tokio::time::sleep(Duration::from_millis(millis))));
        }
        if let Some(delay) = &mut this.delay {
            ready!(delay.as_mut().poll(cx));
            this.delay = None;
        }
        let count = output.remaining().min(this.bytes.len() - this.offset);
        output.put_slice(&this.bytes[this.offset..this.offset + count]);
        this.offset += count;
        if this.offset == this.bytes.len() {
            this.bytes.clear();
            this.offset = 0;
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Delayed<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn testnet_control_latency_preserves_bytes_small_reads_and_eof() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let millis = Arc::new(AtomicU64::new(100));
        let mut reader = Delayed::new(reader, millis.clone());
        writer.write_all(b"hello").await.unwrap();
        let start = tokio::time::Instant::now();
        let mut first = [0; 2];
        reader.read_exact(&mut first).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert_eq!(&first, b"he");
        millis.store(0, Ordering::SeqCst);
        writer.write_all(b" world").await.unwrap();
        writer.shutdown().await.unwrap();
        let start = tokio::time::Instant::now();
        let mut rest = String::new();
        reader.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "llo world");
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}
