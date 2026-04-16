//! TLS helpers for server-to-server and cloud connections.
//!
//! [`tls_connect`] establishes an outbound TLS connection (used by cloud client).
//! [`create_tls_acceptor`] builds a `TlsAcceptor` from PEM-encoded cert/key
//! (used by cloud server mode).

use super::{TcpTransport, configure_tcp_keepalive};
use crate::transport::{Result, TransportError};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Connect to a TLS-enabled server and return a transport
pub(crate) async fn tls_connect(
    host: &str,
    port: u16,
) -> Result<TcpTransport<ClientTlsStream<TcpStream>>> {
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

    Ok(TcpTransport::new(tls_stream))
}

/// Create a TLS acceptor for cloud server mode.
/// Requires TLS certificate and private key files.
pub(crate) fn create_tls_acceptor(cert_pem: &[u8], key_pem: &[u8]) -> Result<TlsAcceptor> {
    use rustls::pki_types::CertificateDer;
    use rustls_pemfile::{certs, private_key};
    use std::io::BufReader;

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
