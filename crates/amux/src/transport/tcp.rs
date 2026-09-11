//! TCP socket helpers.

#[cfg(any(test, test_fixtures))]
use std::io;

#[cfg(any(test, test_fixtures))]
use futures_util::{Stream, stream};
#[cfg(any(test, test_fixtures))]
use tokio::net::TcpListener;
use tokio::net::TcpStream;

#[cfg(any(test, test_fixtures))]
pub(crate) fn tcp_incoming(
    listener: TcpListener,
) -> impl Stream<Item = io::Result<TcpStream>> + Send + 'static {
    stream::unfold(listener, |listener| async move {
        let item = match listener.accept().await {
            Ok((stream, _addr)) => {
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(error = %error, "failed to set TCP_NODELAY");
                }
                configure_relay_tcp_keepalive(&stream);
                Ok(stream)
            }
            Err(error) => Err(error),
        };
        Some((item, listener))
    })
}

/// Keep the relay's ordered-stream fallback alive across idle network gear.
/// Device-to-device LAN connections never use this TCP-only helper.
pub(crate) fn configure_relay_tcp_keepalive(stream: &tokio::net::TcpStream) {
    use std::time::Duration;

    use socket2::SockRef;

    let sock = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    if let Err(error) = sock.set_tcp_keepalive(&keepalive) {
        tracing::warn!(error = %error, "failed to set relay TCP keepalive");
    }
}
