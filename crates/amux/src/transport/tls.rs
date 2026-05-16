//! TLS helpers for server-to-server and cloud connections.
//!
//! [`tls_channel`] establishes an outbound TLS channel (used by cloud client).
//! [`create_tls_acceptor`] builds a `TlsAcceptor` from PEM-encoded cert/key
//! (used by cloud server mode).

use std::sync::Arc;

use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use super::{configure_tcp_keepalive, configure_tonic_endpoint_keepalive};
use crate::transport::{Result, TransportError};

pub(crate) async fn tls_connect_stream(
    host: &str,
    port: u16,
) -> Result<ClientTlsStream<TcpStream>> {
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));

    let addr = format!("{}:{}", host, port);
    let stream = TcpStream::connect(&addr).await?;
    stream.set_nodelay(true)?;
    configure_tcp_keepalive(&stream);

    let domain = ServerName::try_from(host.to_string())
        .map_err(|_| TransportError::Config(format!("Invalid DNS name: {}", host)))?;
    let tls_stream = connector.connect(domain, stream).await?;

    Ok(tls_stream)
}

pub(crate) fn tls_channel(host: String, port: u16) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(format!("https://{host}:{port}"))
        .map_err(|error| TransportError::Config(error.to_string()))?;
    Ok(
        configure_tonic_endpoint_keepalive(endpoint).connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let host = host.clone();
                async move {
                    tls_connect_stream(&host, port)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                }
            },
        )),
    )
}

/// Create a TLS acceptor for cloud server mode.
/// Requires TLS certificate and private key files.
pub(crate) fn create_tls_acceptor(cert_pem: &[u8], key_pem: &[u8]) -> Result<TlsAcceptor> {
    use std::io::BufReader;

    use rustls::pki_types::CertificateDer;
    use rustls_pemfile::{certs, private_key};

    let certs: Vec<CertificateDer<'static>> = certs(&mut BufReader::new(cert_pem))
        .filter_map(|r| r.ok())
        .collect();

    if certs.is_empty() {
        return Err(TransportError::Config(
            "No certificates found in PEM".to_string(),
        ));
    }

    let key = private_key(&mut BufReader::new(key_pem))
        .map_err(|e| TransportError::Config(format!("Failed to parse private key: {}", e)))?
        .ok_or_else(|| TransportError::Config("No private key found in PEM".to_string()))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TransportError::Config(format!("TLS config error: {}", e)))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}
