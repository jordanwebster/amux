//! Transport helpers for gRPC services.

mod embedded_relay;
mod io;
mod memory;
mod single_io;
mod ssh;
mod tcp;
mod tls;
#[cfg(unix)]
mod unix;

use std::time::Duration;

pub(crate) use embedded_relay::RelayTransport;
pub use embedded_relay::{EmbeddedRelay, RelayEndpoint, RelayRetry};
pub(crate) use io::{
    BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcIo, GrpcIo, PreTrustPairingReachability,
    TrustedPeerConnections,
};
pub use memory::InProcessConnection;
#[cfg(test)]
pub(crate) use memory::in_process_transport_pair;
pub(crate) use memory::{ShutdownIo, in_process_channel, managed_in_process_transport_pair};
pub(crate) use single_io::channel_from_single_io;
#[allow(unused_imports)]
pub(crate) use ssh::{SshRelayIo, spawn_ssh_pair_recv, spawn_ssh_relay};
pub(crate) use tcp::configure_relay_tcp_keepalive;
#[allow(unused_imports)]
pub(crate) use tcp::tcp_incoming;
use thiserror::Error;
pub(crate) use tls::{
    create_tls_acceptor, pairing_channel_from_io, pairing_quic_channel, relay_quic_client_config,
    relay_quic_server_config, tls_connect_stream,
};
pub use tls::{
    pairing_quic_client_config, relay_quic_client_config_with_roots,
    relay_quic_server_config_from_der,
};
use tonic::transport::{Endpoint, Server};
#[cfg(unix)]
pub(crate) use unix::{bind_unix_listener, unix_incoming};

pub(crate) type Result<T> = std::result::Result<T, TransportError>;

const GRPC_HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const GRPC_HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Transport config error: {0}")]
    Config(String),
}

pub(crate) fn configure_tonic_endpoint_keepalive(endpoint: Endpoint) -> Endpoint {
    endpoint
        .http2_keep_alive_interval(GRPC_HTTP2_KEEPALIVE_INTERVAL)
        .keep_alive_timeout(GRPC_HTTP2_KEEPALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
}

pub(crate) fn tonic_server_builder() -> Server {
    Server::builder()
        .http2_keepalive_interval(Some(GRPC_HTTP2_KEEPALIVE_INTERVAL))
        .http2_keepalive_timeout(Some(GRPC_HTTP2_KEEPALIVE_TIMEOUT))
}
