//! In-process tonic transport helpers.

#[cfg(test)]
use std::io;

#[cfg(test)]
use futures_util::{Stream, stream};
use tokio::io::DuplexStream;
use tonic::transport::{Channel, Endpoint};

use super::{GrpcIo, channel_from_single_io};

pub(crate) const IN_PROCESS_BUF_SIZE: usize = 64 * 1024;

pub(crate) type InProcessTransport = GrpcIo<DuplexStream>;

pub(crate) fn in_process_transport_pair() -> (InProcessTransport, InProcessTransport) {
    let (client_io, server_io) = tokio::io::duplex(IN_PROCESS_BUF_SIZE);
    (
        InProcessTransport::new(client_io),
        InProcessTransport::new(server_io),
    )
}

#[cfg(test)]
pub(crate) fn in_process_incoming(
    transport: InProcessTransport,
) -> impl Stream<Item = io::Result<InProcessTransport>> + Send + 'static {
    stream::once(async move { Ok(transport) })
}

pub(crate) fn in_process_channel(transport: InProcessTransport) -> Channel {
    channel_from_single_io(
        Endpoint::from_static("http://in-process"),
        "in-process transport",
        transport,
    )
}
