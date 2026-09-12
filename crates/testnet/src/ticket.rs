//! Raw QUIC session-ticket probe used by the direct-transport specification.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use node::harness::{
    QUIC_ALPN, ed25519_public_key_from_certificate, load_or_create_device_identity_in,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{
    ClientSessionMemoryCache, ClientSessionStore, Resumption, Tls12ClientSessionValue,
    Tls13ClientSessionValue,
};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, NamedGroup, SignatureScheme};

use super::{Daemon, TestNet};

impl TestNet {
    /// Proves a cached ticket is offered but the production server requires
    /// another certificate-verified TLS 1.3 handshake.
    pub async fn session_ticket_requires_full_handshake(&self, client: &Daemon, server: &Daemon) {
        let client_identity = load_or_create_device_identity_in(&client.inner.data_dir).unwrap();
        let server_identity = load_or_create_device_identity_in(&server.inner.data_dir).unwrap();
        let server_runtime = server.runtime().await;
        let server_runtime = server_runtime.as_ref().expect("server is running");
        let endpoint = server_runtime
            .services
            .reachability_link_connector()
            .quic_endpoint()
            .expect("server has a direct QUIC endpoint");
        let server_trust = server_runtime.trust.clone();

        let mut ticket_tls = server_identity
            .server_tls_config(server_trust.clone())
            .unwrap();
        ticket_tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        ticket_tls.session_storage = rustls::server::ServerSessionMemoryCache::new(32);
        ticket_tls.send_tls13_tickets = 2;
        ticket_tls.max_early_data_size = 0;
        let ticket_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(ticket_tls).unwrap();
        let mut ticket_server_config = quinn::ServerConfig::with_crypto(Arc::new(ticket_crypto));
        ticket_server_config.transport_config(super::udp_proxy::transport_config());
        ticket_server_config.migration(true);
        endpoint.set_server_config(Some(ticket_server_config));

        let full_handshakes = Arc::new(AtomicUsize::new(0));
        let verifier = Arc::new(CountingPinnedVerifier::new(
            server_identity.public_key().to_vec(),
            full_handshakes.clone(),
        ));
        let sessions = Arc::new(TrackingSessionStore::default());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            client_identity.private_key_pkcs8().to_vec(),
        ));
        let mut client_tls =
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_client_auth_cert(
                    vec![CertificateDer::from(
                        client_identity.certificate_der().unwrap(),
                    )],
                    key,
                )
                .unwrap();
        client_tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        client_tls.resumption = Resumption::store(sessions.clone());
        client_tls.enable_early_data = false;
        let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap();
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
        client_config.transport_config(super::udp_proxy::transport_config());
        let mut client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(client_config);

        let first = client_endpoint
            .connect(server.direct_addr(), "amux-device.local")
            .unwrap()
            .await
            .expect("ticket-issuing handshake through UDP proxy");
        tokio::time::timeout(super::assertions::DEFAULT_TIMEOUT, async {
            while sessions.inserted.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ticket-issuing server did not provide a session ticket");
        first.close(0_u32.into(), b"ticket received");

        let mut production_config = server_identity.quic_server_config(server_trust).unwrap();
        production_config.transport_config(super::udp_proxy::transport_config());
        endpoint.set_server_config(Some(production_config));
        let second = client_endpoint
            .connect(server.direct_addr(), "amux-device.local")
            .unwrap()
            .await
            .expect("ticket-refusing handshake through UDP proxy");

        assert!(
            sessions.taken.load(Ordering::SeqCst) > 0,
            "the client must offer its cached ticket"
        );
        assert_eq!(
            full_handshakes.load(Ordering::SeqCst),
            2,
            "the production server must require a second certificate handshake"
        );
        second.close(0_u32.into(), b"probe complete");
    }
}

#[derive(Debug)]
struct CountingPinnedVerifier {
    expected_pubkey: Vec<u8>,
    full_handshakes: Arc<AtomicUsize>,
    supported_algs: WebPkiSupportedAlgorithms,
}

impl CountingPinnedVerifier {
    fn new(expected_pubkey: Vec<u8>, full_handshakes: Arc<AtomicUsize>) -> Self {
        Self {
            expected_pubkey,
            full_handshakes,
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for CountingPinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let pubkey = ed25519_public_key_from_certificate(end_entity)
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if pubkey.as_slice() != self.expected_pubkey.as_slice() {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::BadSignature,
            ));
        }
        self.full_handshakes.fetch_add(1, Ordering::SeqCst);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

#[derive(Debug)]
struct TrackingSessionStore {
    inner: ClientSessionMemoryCache,
    inserted: AtomicUsize,
    taken: AtomicUsize,
}

impl Default for TrackingSessionStore {
    fn default() -> Self {
        Self {
            inner: ClientSessionMemoryCache::new(32),
            inserted: AtomicUsize::new(0),
            taken: AtomicUsize::new(0),
        }
    }
}

impl ClientSessionStore for TrackingSessionStore {
    fn set_kx_hint(&self, server_name: ServerName<'static>, group: NamedGroup) {
        self.inner.set_kx_hint(server_name, group);
    }

    fn kx_hint(&self, server_name: &ServerName<'_>) -> Option<NamedGroup> {
        self.inner.kx_hint(server_name)
    }

    fn set_tls12_session(&self, server_name: ServerName<'static>, value: Tls12ClientSessionValue) {
        self.inner.set_tls12_session(server_name, value);
    }

    fn tls12_session(&self, server_name: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
        self.inner.tls12_session(server_name)
    }

    fn remove_tls12_session(&self, server_name: &ServerName<'static>) {
        self.inner.remove_tls12_session(server_name);
    }

    fn insert_tls13_ticket(
        &self,
        server_name: ServerName<'static>,
        value: Tls13ClientSessionValue,
    ) {
        self.inserted.fetch_add(1, Ordering::SeqCst);
        self.inner.insert_tls13_ticket(server_name, value);
    }

    fn take_tls13_ticket(
        &self,
        server_name: &ServerName<'static>,
    ) -> Option<Tls13ClientSessionValue> {
        let ticket = self.inner.take_tls13_ticket(server_name);
        if ticket.is_some() {
            self.taken.fetch_add(1, Ordering::SeqCst);
        }
        ticket
    }
}
