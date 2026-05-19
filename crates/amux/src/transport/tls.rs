//! TLS helpers for server-to-server and cloud connections.
//!
//! [`tls_channel`] establishes an outbound TLS channel (used by cloud client).
//! [`create_tls_acceptor`] builds a `TlsAcceptor` from PEM-encoded cert/key
//! (used by cloud server mode).

use std::fmt;
#[cfg(any(debug_assertions, test))]
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    version,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use super::{channel_from_single_io, configure_tcp_keepalive, configure_tonic_endpoint_keepalive};
use crate::HostId;
use crate::identity::{DeviceIdentity, ed25519_public_key_from_certificate};
use crate::transport::{Result, TransportError};
use crate::trust::SharedTrustStore;

pub(crate) async fn tls_connect_stream(
    host: &str,
    port: u16,
) -> Result<ClientTlsStream<TcpStream>> {
    let root_store = cloud_root_store()?;

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

fn cloud_root_store() -> Result<rustls::RootCertStore> {
    let mut root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    add_debug_cloud_root(&mut root_store)?;
    Ok(root_store)
}

#[cfg(any(debug_assertions, test))]
fn add_debug_cloud_root(root_store: &mut rustls::RootCertStore) -> Result<()> {
    let Ok(path) = std::env::var("AMUX_CLOUD_TLS_CA") else {
        return Ok(());
    };
    let pem = std::fs::read(&path).map_err(|error| {
        TransportError::Config(format!(
            "failed to read AMUX_CLOUD_TLS_CA from {path}: {error}"
        ))
    })?;
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut BufReader::new(pem.as_slice())) {
        let cert = cert.map_err(|error| {
            TransportError::Config(format!("failed to parse AMUX_CLOUD_TLS_CA {path}: {error}"))
        })?;
        root_store.add(cert).map_err(|error| {
            TransportError::Config(format!(
                "invalid AMUX_CLOUD_TLS_CA certificate {path}: {error}"
            ))
        })?;
        added += 1;
    }
    if added == 0 {
        return Err(TransportError::Config(format!(
            "AMUX_CLOUD_TLS_CA {path} contains no certificates"
        )));
    }
    Ok(())
}

#[cfg(not(any(debug_assertions, test)))]
fn add_debug_cloud_root(_root_store: &mut rustls::RootCertStore) -> Result<()> {
    Ok(())
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

pub(crate) fn pin_pairing_channel(addr: SocketAddr) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(format!("https://{addr}"))
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let config = pin_pairing_client_config();
    Ok(
        configure_tonic_endpoint_keepalive(endpoint).connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let connector = TlsConnector::from(Arc::new(config.clone()));
                async move {
                    let stream = TcpStream::connect(addr).await?;
                    stream.set_nodelay(true)?;
                    configure_tcp_keepalive(&stream);
                    let server_name = ServerName::try_from("amux-pairing.local")
                        .expect("static pairing server name is valid");
                    connector
                        .connect(server_name, stream)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                }
            },
        )),
    )
}

pub(crate) fn trusted_device_channel(
    addr: SocketAddr,
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    peer: HostId,
) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(format!("https://{addr}"))
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let config = identity
        .client_tls_config_for_peer(trust_store, peer)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    Ok(
        configure_tonic_endpoint_keepalive(endpoint).connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let connector = TlsConnector::from(Arc::new(config.clone()));
                async move {
                    let stream = TcpStream::connect(addr).await?;
                    stream.set_nodelay(true)?;
                    configure_tcp_keepalive(&stream);
                    let server_name = ServerName::try_from("amux-device.local")
                        .expect("static device server name is valid");
                    connector
                        .connect(server_name, stream)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                }
            },
        )),
    )
}

pub(crate) async fn pin_pairing_channel_from_io<IO>(io: IO) -> Result<Channel>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let connector = TlsConnector::from(Arc::new(pin_pairing_client_config()));
    let server_name =
        ServerName::try_from("amux-pairing.local").expect("static pairing server name is valid");
    let tls = connector.connect(server_name, io).await?;
    Ok(channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("https://pin-pairing")),
        "PIN pairing TLS transport",
        tls,
    ))
}

pub(crate) async fn qr_pairing_channel_from_io<IO>(
    io: IO,
    expected_pubkey: Vec<u8>,
) -> Result<Channel>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let connector = TlsConnector::from(Arc::new(qr_pairing_client_config(expected_pubkey)?));
    let server_name =
        ServerName::try_from("amux-pairing.local").expect("static pairing server name is valid");
    let tls = connector.connect(server_name, io).await?;
    Ok(channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("https://qr-pairing")),
        "QR pairing TLS transport",
        tls,
    ))
}

fn pin_pairing_client_config() -> ClientConfig {
    let verifier = Arc::new(NoServerVerification {
        supported_algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    });
    let mut config = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    config
}

fn qr_pairing_client_config(expected_pubkey: Vec<u8>) -> Result<ClientConfig> {
    let expected_pubkey = expected_qr_pubkey(expected_pubkey)?;
    let verifier = Arc::new(QrServerVerification {
        expected_pubkey,
        supported_algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    });
    let mut config = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

fn expected_qr_pubkey(pubkey: Vec<u8>) -> Result<[u8; 32]> {
    let mut expected = [0_u8; 32];
    if pubkey.len() != expected.len() {
        return Err(TransportError::Config(format!(
            "QR pairing pubkey must be 32 bytes, got {}",
            pubkey.len()
        )));
    }
    expected.copy_from_slice(&pubkey);
    Ok(expected)
}

#[derive(Clone)]
struct NoServerVerification {
    supported_algs: WebPkiSupportedAlgorithms,
}

impl fmt::Debug for NoServerVerification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoServerVerification").finish()
    }
}

impl ServerCertVerifier for NoServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

#[derive(Clone)]
struct QrServerVerification {
    expected_pubkey: [u8; 32],
    supported_algs: WebPkiSupportedAlgorithms,
}

impl fmt::Debug for QrServerVerification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrServerVerification")
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for QrServerVerification {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        let pubkey = ed25519_public_key_from_certificate(end_entity)
            .map_err(|_| TlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        if pubkey == self.expected_pubkey {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::InvalidCertificate(CertificateError::BadSignature))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
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
