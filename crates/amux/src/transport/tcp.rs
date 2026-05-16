//! TCP socket helpers.

use std::io;

use futures_util::{Stream, stream};
use tokio::net::{TcpListener, TcpStream};
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use super::{GrpcIo, Result, TransportError, configure_tonic_endpoint_keepalive};

pub(crate) type TcpServerTransport<T = TcpStream> = GrpcIo<T>;

pub(crate) fn tcp_incoming(
    listener: TcpListener,
) -> impl Stream<Item = io::Result<TcpServerTransport<TcpStream>>> + Send + 'static {
    stream::unfold(listener, |listener| async move {
        let item = match listener.accept().await {
            Ok((stream, _addr)) => {
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(error = %error, "failed to set TCP_NODELAY");
                }
                configure_tcp_keepalive(&stream);
                Ok(TcpServerTransport::new(stream))
            }
            Err(error) => Err(error),
        };
        Some((item, listener))
    })
}

/// Configure TCP keepalive on a stream: 30s idle before first probe, 10s between probes.
pub(crate) fn configure_tcp_keepalive(stream: &tokio::net::TcpStream) {
    use std::time::Duration;

    use socket2::SockRef;

    let sock = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    if let Err(error) = sock.set_tcp_keepalive(&keepalive) {
        tracing::warn!(error = %error, "failed to set TCP keepalive");
    }
}

pub(crate) fn tcp_channel(host: String, port: u16) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(format!("http://{host}:{port}"))
        .map_err(|error| TransportError::Config(error.to_string()))?;
    Ok(
        configure_tonic_endpoint_keepalive(endpoint).connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let host = host.clone();
                async move {
                    let stream = TcpStream::connect((host.as_str(), port)).await?;
                    stream.set_nodelay(true)?;
                    configure_tcp_keepalive(&stream);
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            },
        )),
    )
}
