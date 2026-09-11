use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::identity::{self, DeviceIdentity, IdentityError};
use crate::pairing::PairMode;
use crate::resource_limits::{
    EXTERNAL_TCP_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_TCP_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_TCP_TLS_HANDSHAKE_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::transport::{
    BoxedGrpcIo, PreTrustPairingReachability, TrustedPeerConnections, configure_tcp_keepalive,
};
use crate::trust::SharedTrustStore;
use crate::{HostId, audit};

#[derive(Debug, Error)]
pub(crate) enum DispatchError {
    #[error("TLS handshake timed out")]
    Timeout,
    #[error("TLS handshake failed: {0}")]
    Tls(#[from] std::io::Error),
    #[error("dispatcher output channel is closed")]
    ChannelClosed,
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

thread_local! {
    static MTLS_AUDIT_EMITTED: AtomicBool = const { AtomicBool::new(false) };
}

pub(crate) fn mark_mtls_audit_emitted() {
    MTLS_AUDIT_EMITTED.with(|emitted| emitted.store(true, Ordering::Relaxed));
}

fn take_mtls_audit_emitted() -> bool {
    MTLS_AUDIT_EMITTED.with(|emitted| emitted.swap(false, Ordering::Relaxed))
}

/// OS-level duplicates of accepted external-TCP sockets, kept so test
/// harnesses can sever every live connection at once (see
/// [`TunnelDispatcher::serve_tcp_listener_tracked`]).
pub(crate) type TrackedTcpConnections = std::sync::Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>;

pub(crate) fn track_tcp_stream(
    stream: tokio::net::TcpStream,
    track: Option<&TrackedTcpConnections>,
) -> std::io::Result<tokio::net::TcpStream> {
    let Some(connections) = track else {
        return Ok(stream);
    };
    let std_stream = stream.into_std()?;
    let duplicate = std_stream.try_clone()?;
    connections
        .lock()
        .expect("tracked TCP connection registry poisoned")
        .push(duplicate);
    tokio::net::TcpStream::from_std(std_stream)
}

#[derive(Clone)]
pub(crate) struct TunnelDispatcher {
    acceptor: TlsAcceptor,
    trust_store: SharedTrustStore,
    pair_mode: std::sync::Arc<PairMode>,
    trusted_connections: TrustedPeerConnections,
    trusted_tx: mpsc::Sender<BoxedGrpcIo>,
    pairing_tx: mpsc::Sender<BoxedGrpcIo>,
    handshake_timeout: Duration,
    external_tcp_handshake_limiter: std::sync::Arc<Mutex<SlidingWindowRateLimiter<IpAddr>>>,
    external_tcp_handshake_slots: std::sync::Arc<Semaphore>,
}

enum DispatchTarget {
    Trusted(HostId),
    Pairing,
    Close,
}

impl TunnelDispatcher {
    pub(crate) fn new(
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        pair_mode: std::sync::Arc<PairMode>,
        trusted_connections: TrustedPeerConnections,
        trusted_tx: mpsc::Sender<BoxedGrpcIo>,
        pairing_tx: mpsc::Sender<BoxedGrpcIo>,
        handshake_timeout: Duration,
    ) -> Result<Self, IdentityError> {
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(
            identity.server_tls_config(trust_store.clone())?,
        ));
        Ok(Self {
            acceptor,
            trust_store,
            pair_mode,
            trusted_connections,
            trusted_tx,
            pairing_tx,
            handshake_timeout,
            external_tcp_handshake_limiter: std::sync::Arc::new(Mutex::new(
                SlidingWindowRateLimiter::new(
                    EXTERNAL_TCP_TLS_HANDSHAKE_RATE_LIMIT,
                    EXTERNAL_TCP_TLS_HANDSHAKE_RATE_WINDOW,
                ),
            )),
            external_tcp_handshake_slots: std::sync::Arc::new(Semaphore::new(
                EXTERNAL_TCP_TLS_HANDSHAKE_CONCURRENCY,
            )),
        })
    }

    pub(crate) fn serve_tcp_listener(&self, listener: TcpListener) -> JoinHandle<()> {
        self.serve_tcp_listener_inner(listener, None)
    }

    /// Test seam: like [`Self::serve_tcp_listener`], but registers an
    /// OS-level duplicate of every accepted socket in `connections` so an
    /// in-process daemon "restart" can sever them the way a real process
    /// exit would (per-connection tasks are detached and would otherwise
    /// keep serving the old runtime).
    #[cfg(any(test, testnet))]
    pub(crate) fn serve_tcp_listener_tracked(
        &self,
        listener: TcpListener,
        connections: TrackedTcpConnections,
    ) -> JoinHandle<()> {
        self.serve_tcp_listener_inner(listener, Some(connections))
    }

    fn serve_tcp_listener_inner(
        &self,
        listener: TcpListener,
        track: Option<TrackedTcpConnections>,
    ) -> JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::warn!(error = %error, "external TCP accept failed");
                        continue;
                    }
                };
                let stream = match track_tcp_stream(stream, track.as_ref()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        tracing::warn!(error = %error, "failed to track accepted TCP stream");
                        continue;
                    }
                };
                if !dispatcher.allow_external_tcp_handshake(addr.ip()).await {
                    tracing::warn!(peer = %addr, "external TCP TLS handshake rate limit exceeded");
                    continue;
                }
                let permit = match dispatcher
                    .external_tcp_handshake_slots
                    .clone()
                    .acquire_owned()
                    .await
                {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(error = %error, "failed to set TCP_NODELAY");
                }
                configure_tcp_keepalive(&stream);
                let dispatcher = dispatcher.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = dispatcher
                        .dispatch(stream, PreTrustPairingReachability::NoReusableReachability)
                        .await
                    {
                        tracing::warn!(peer = %addr, error = %error, "dispatcher rejected TCP stream");
                    }
                });
            }
        })
    }

    pub(crate) fn serve_tunnel_receiver(
        &self,
        mut incoming_rx: mpsc::Receiver<crate::tunnel::TunnelTransport>,
    ) -> JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            while let Some(transport) = incoming_rx.recv().await {
                let dispatcher = dispatcher.clone();
                tokio::spawn(async move {
                    let pairing_reachability = if transport.has_cloud_pairing_reachability() {
                        PreTrustPairingReachability::Cloud
                    } else {
                        PreTrustPairingReachability::NoReusableReachability
                    };
                    if let Err(error) = dispatcher.dispatch(transport, pairing_reachability).await {
                        tracing::warn!(error = %error, "dispatcher rejected tunnel stream");
                    }
                });
            }
        })
    }

    async fn allow_external_tcp_handshake(&self, source: IpAddr) -> bool {
        self.external_tcp_handshake_limiter
            .lock()
            .await
            .allow(source)
    }

    async fn dispatch<IO>(
        &self,
        stream: IO,
        pairing_reachability: PreTrustPairingReachability,
    ) -> Result<(), DispatchError>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let tls_stream = match tokio::time::timeout(
            self.handshake_timeout,
            self.acceptor.accept(stream),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                if !take_mtls_audit_emitted() {
                    audit::auth_mtls_handshake_failure(&error);
                }
                return Err(DispatchError::Tls(error));
            }
            Err(_) => {
                audit::auth_mtls_handshake_failure("TLS handshake timed out");
                return Err(DispatchError::Timeout);
            }
        };

        match self.dispatch_target(&tls_stream)? {
            DispatchTarget::Trusted(peer) => self
                .trusted_tx
                .send(
                    BoxedGrpcIo::tls_trusted(tls_stream, peer)
                        .track_trusted_peer(&self.trusted_connections),
                )
                .await
                .map_err(|_| DispatchError::ChannelClosed),
            DispatchTarget::Pairing => self
                .pairing_tx
                .send(BoxedGrpcIo::pre_trust_pairing(
                    tls_stream,
                    pairing_reachability,
                ))
                .await
                .map_err(|_| DispatchError::ChannelClosed),
            DispatchTarget::Close => Ok(()),
        }
    }

    fn dispatch_target<IO>(
        &self,
        tls_stream: &tokio_rustls::server::TlsStream<IO>,
    ) -> Result<DispatchTarget, IdentityError> {
        let peer_cert = tls_stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|certs| certs.first());

        if let Some(cert) = peer_cert {
            let trust_store = self
                .trust_store
                .read()
                .map_err(|_| IdentityError::TrustStorePoisoned)?;
            return Ok(
                match identity::host_id_for_certificate(&trust_store, cert)? {
                    Some(host_id) => DispatchTarget::Trusted(host_id),
                    None => {
                        audit::auth_mtls_handshake_failure(
                            "trusted client certificate no longer maps to a trusted host",
                        );
                        DispatchTarget::Close
                    }
                },
            );
        }

        if self.pair_mode.is_active() {
            Ok(DispatchTarget::Pairing)
        } else {
            audit::auth_mtls_handshake_failure(
                "missing client certificate and pairing mode inactive",
            );
            Ok(DispatchTarget::Close)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::Arc;

    use chrono::{DateTime, Utc};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{
        WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    };
    use rustls::pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
    };
    use rustls::{
        ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version,
    };
    use tempfile::TempDir;
    use tokio_rustls::TlsConnector;
    use tonic::transport::server::Connected;

    use super::*;
    use crate::identity::load_or_create_device_identity_in;
    use crate::transport::BoxedGrpcAuth;
    use crate::trust::{Reachability, TrustEntry, TrustStore};

    #[derive(Clone)]
    struct TestClientConfig {
        identity: Option<DeviceIdentity>,
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
        ) -> Result<ServerCertVerified, TlsError> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            verify_tls12_signature(message, cert, dss, &self.supported_algs)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            verify_tls13_signature(message, cert, dss, &self.supported_algs)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported_algs.supported_schemes()
        }
    }

    fn temp_identity() -> (TempDir, DeviceIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let identity = load_or_create_device_identity_in(dir.path()).unwrap();
        (dir, identity)
    }

    fn trust_store_for(identity: &DeviceIdentity) -> TrustStore {
        let mut trust_store = TrustStore::default();
        trust_store.insert_for_test(
            identity.host_id,
            TrustEntry {
                pubkey: identity.public_key().to_vec(),
                name: "peer".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        trust_store
    }

    fn client_config(config: TestClientConfig) -> ClientConfig {
        let verifier = Arc::new(NoServerVerification {
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        });
        let builder = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier);

        let mut config = match config.identity {
            Some(identity) => builder
                .with_client_auth_cert(
                    vec![CertificateDer::from(identity.certificate_der().unwrap())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                        identity.private_key_pkcs8().to_vec(),
                    )),
                )
                .unwrap(),
            None => builder.with_no_client_auth(),
        };
        config.alpn_protocols = vec![b"h2".to_vec()];
        config
    }

    async fn run_dispatch(
        dispatcher: TunnelDispatcher,
        client_config: ClientConfig,
    ) -> Result<(), DispatchError> {
        run_dispatch_with_reachability(
            dispatcher,
            client_config,
            PreTrustPairingReachability::Cloud,
        )
        .await
    }

    async fn run_dispatch_with_reachability(
        dispatcher: TunnelDispatcher,
        client_config: ClientConfig,
        reachability: PreTrustPairingReachability,
    ) -> Result<(), DispatchError> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let connector = TlsConnector::from(Arc::new(client_config));
        let client = tokio::spawn(async move {
            connector
                .connect(ServerName::try_from("amux.test").unwrap(), client_io)
                .await
        });
        let server = dispatcher.dispatch(server_io, reachability).await;
        let _ = client.await.unwrap();
        server
    }

    #[tokio::test]
    async fn dispatches_trusted_client_cert_to_trusted_server() {
        let (_server_dir, server_identity) = temp_identity();
        let (_peer_dir, peer_identity) = temp_identity();
        let trust_store = trust_store_for(&peer_identity);
        let pair_mode = Arc::new(PairMode::new());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(trust_store)),
            pair_mode,
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        run_dispatch(
            dispatcher,
            client_config(TestClientConfig {
                identity: Some(peer_identity.clone()),
            }),
        )
        .await
        .unwrap();

        let trusted = trusted_rx.try_recv().unwrap();
        assert_eq!(
            trusted.connect_info().auth,
            BoxedGrpcAuth::TlsTrusted {
                peer: peer_identity.host_id
            }
        );
        assert!(pairing_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn dispatches_anonymous_tls_only_when_pair_mode_is_active() {
        let (_server_dir, server_identity) = temp_identity();
        let pair_mode = Arc::new(PairMode::new());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            pair_mode.clone(),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        run_dispatch(
            dispatcher.clone(),
            client_config(TestClientConfig { identity: None }),
        )
        .await
        .unwrap();
        assert!(trusted_rx.try_recv().is_err());
        assert!(pairing_rx.try_recv().is_err());

        pair_mode
            .start_qr_secret_for_duration([1_u8; 32], Duration::from_secs(60))
            .unwrap();
        run_dispatch(
            dispatcher,
            client_config(TestClientConfig { identity: None }),
        )
        .await
        .unwrap();
        assert!(trusted_rx.try_recv().is_err());
        let pairing = tokio::time::timeout(Duration::from_secs(1), pairing_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pairing.connect_info().auth,
            BoxedGrpcAuth::PreTrustPairing {
                reachability: PreTrustPairingReachability::Cloud,
            }
        );
    }

    #[tokio::test]
    async fn dispatches_direct_pairing_without_reusable_reachability() {
        let (_server_dir, server_identity) = temp_identity();
        let pair_mode = Arc::new(PairMode::new());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            pair_mode.clone(),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        run_dispatch_with_reachability(
            dispatcher,
            client_config(TestClientConfig { identity: None }),
            PreTrustPairingReachability::NoReusableReachability,
        )
        .await
        .unwrap();

        assert!(trusted_rx.try_recv().is_err());
        let pairing = tokio::time::timeout(Duration::from_secs(1), pairing_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pairing.connect_info().auth,
            BoxedGrpcAuth::PreTrustPairing {
                reachability: PreTrustPairingReachability::NoReusableReachability,
            }
        );
    }

    #[tokio::test]
    async fn external_tcp_handshake_limiter_is_per_source_ip() {
        let (_server_dir, server_identity) = temp_identity();
        let (trusted_tx, _trusted_rx) = mpsc::channel(1);
        let (pairing_tx, _pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            Arc::new(PairMode::new()),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();
        let source = "127.0.0.1".parse::<IpAddr>().unwrap();
        let other = "127.0.0.2".parse::<IpAddr>().unwrap();

        for _ in 0..EXTERNAL_TCP_TLS_HANDSHAKE_RATE_LIMIT {
            assert!(dispatcher.allow_external_tcp_handshake(source).await);
        }
        assert!(!dispatcher.allow_external_tcp_handshake(source).await);
        assert!(dispatcher.allow_external_tcp_handshake(other).await);
    }
}
