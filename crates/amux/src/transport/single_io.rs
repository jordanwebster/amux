//! One-shot tonic channel connector for already-open IO objects.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};
use tower::Service;

#[derive(Clone)]
struct SingleIoConnector<IO> {
    io: Arc<Mutex<Option<IO>>>,
    label: &'static str,
}

impl<IO> SingleIoConnector<IO> {
    fn new(io: IO, label: &'static str) -> Self {
        Self {
            io: Arc::new(Mutex::new(Some(io))),
            label,
        }
    }
}

impl<IO> Service<Uri> for SingleIoConnector<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Response = TokioIo<IO>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TokioIo<IO>>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: Uri) -> Self::Future {
        let io = Arc::clone(&self.io);
        let label = self.label;
        Box::pin(async move {
            io.lock()
                .expect("single-io connector mutex poisoned")
                .take()
                .map(TokioIo::new)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{label} already consumed"),
                    )
                })
        })
    }
}

pub(crate) fn channel_from_single_io<IO>(endpoint: Endpoint, label: &'static str, io: IO) -> Channel
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    endpoint.connect_with_connector_lazy(SingleIoConnector::new(io, label))
}

pub(crate) async fn connect_single_io<IO>(
    endpoint: Endpoint,
    label: &'static str,
    io: IO,
) -> Result<Channel, tonic::transport::Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    endpoint
        .connect_with_connector(SingleIoConnector::new(io, label))
        .await
}
