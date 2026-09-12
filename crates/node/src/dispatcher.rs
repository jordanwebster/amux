use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustls::pki_types::CertificateDer;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::identity::{self, DeviceIdentity, IdentityError};
use crate::link::{ByteStream, LinkCtx, QuicCarrier, accepted_quic_bidi_stream, run_link};
use crate::pairing::PairMode;
use crate::resource_limits::{
    EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::transport::{BoxedGrpcIo, PreTrustPairingReachability, TrustedPeerConnections};
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
    #[error("native link failed: {0}")]
    Link(String),
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

#[derive(Clone)]
pub(crate) struct TunnelDispatcher {
    acceptor: TlsAcceptor,
    trust_store: SharedTrustStore,
    pair_mode: std::sync::Arc<PairMode>,
    trusted_connections: TrustedPeerConnections,
    trusted_tx: mpsc::Sender<BoxedGrpcIo>,
    pairing_tx: mpsc::Sender<BoxedGrpcIo>,
    handshake_timeout: Duration,
    external_quic_handshake_limiter: std::sync::Arc<Mutex<SlidingWindowRateLimiter<IpAddr>>>,
    external_quic_handshake_slots: std::sync::Arc<Semaphore>,
    link_ctx: Option<LinkCtx>,
}

pub(crate) enum DispatchTarget {
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
            external_quic_handshake_limiter: std::sync::Arc::new(Mutex::new(
                SlidingWindowRateLimiter::new(
                    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
                    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW,
                ),
            )),
            external_quic_handshake_slots: std::sync::Arc::new(Semaphore::new(
                EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY,
            )),
            link_ctx: None,
        })
    }

    pub(crate) fn with_link_ctx(mut self, link_ctx: LinkCtx) -> Self {
        self.link_ctx = Some(link_ctx);
        self
    }

    pub(crate) fn serve_quic_endpoint(
        &self,
        endpoint: quinn::Endpoint,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.accept() => incoming,
                    completed = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                        if let Some(Err(error)) = completed
                            && !error.is_cancelled()
                        {
                            tracing::warn!(error = %error, "external QUIC connection task failed");
                        }
                        continue;
                    }
                    _ = wait_for_shutdown(&mut shutdown_rx) => {
                        connection_tasks.detach_all();
                        break;
                    }
                };
                let Some(incoming) = incoming else { break };
                let addr = incoming.remote_address();

                // Stateless address validation and per-source admission both
                // happen before accepting the Incoming, so quinn has not yet
                // allocated connection or TLS handshake state.
                if !incoming.remote_address_validated() {
                    if !dispatcher.allow_external_quic_handshake(addr.ip()).await {
                        tracing::warn!(peer = %addr, "external QUIC handshake rate limit exceeded");
                        incoming.ignore();
                        continue;
                    }
                    if let Err(error) = incoming.retry() {
                        tracing::debug!(peer = %addr, error = %error, "QUIC address was already validated");
                        error.into_incoming().ignore();
                    }
                    continue;
                };
                let permit = match dispatcher
                    .external_quic_handshake_slots
                    .clone()
                    .try_acquire_owned()
                {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(peer = %addr, "external QUIC handshake concurrency limit exceeded");
                        incoming.refuse();
                        continue;
                    }
                };
                let dispatcher = dispatcher.clone();
                let connection_shutdown = shutdown_rx.clone();
                connection_tasks.spawn(async move {
                    let _permit = permit;
                    let connection = match tokio::time::timeout(
                        dispatcher.handshake_timeout,
                        incoming,
                    )
                    .await
                    {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(error)) => {
                            audit::auth_mtls_handshake_failure(&error);
                            tracing::warn!(peer = %addr, error = %error, "QUIC handshake failed");
                            return;
                        }
                        Err(_) => {
                            audit::auth_mtls_handshake_failure("QUIC handshake timed out");
                            tracing::warn!(peer = %addr, "QUIC handshake timed out");
                            return;
                        }
                    };
                    drop(_permit);
                    if let Err(error) = dispatcher.dispatch_quic(connection, connection_shutdown).await {
                        tracing::warn!(peer = %addr, error = %error, "dispatcher rejected QUIC connection");
                    }
                });
            }
        })
    }

    pub(crate) async fn dispatch_link_stream(
        &self,
        adjacent_peer: HostId,
        stream: ByteStream,
    ) -> Result<(), DispatchError> {
        let directly_paired = self
            .trust_store
            .read()
            .map_err(|_| IdentityError::TrustStorePoisoned)?
            .entry(adjacent_peer)
            .is_some();
        let pairing_reachability = if directly_paired {
            PreTrustPairingReachability::NoReusableReachability
        } else {
            PreTrustPairingReachability::Cloud
        };
        self.dispatch(stream, pairing_reachability).await
    }

    async fn allow_external_quic_handshake(&self, source: IpAddr) -> bool {
        self.external_quic_handshake_limiter
            .lock()
            .await
            .allow(source)
    }

    async fn dispatch_quic(
        &self,
        connection: quinn::Connection,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), DispatchError> {
        match self.classify_quic(&connection)? {
            DispatchTarget::Trusted(peer) => {
                let ctx = self.link_ctx.clone().ok_or_else(|| {
                    DispatchError::Link("native link context is not configured".into())
                })?;
                run_link(
                    ctx.with_authenticated_peer(peer).with_shutdown(shutdown_rx),
                    std::sync::Arc::new(QuicCarrier::from_accepted(connection)),
                    crate::routing::ConnectRole::Acceptor,
                )
                .await
                .map_err(|error| DispatchError::Link(error.to_string()))
            }
            DispatchTarget::Pairing => {
                let (send, recv) =
                    tokio::time::timeout(self.handshake_timeout, connection.accept_bi())
                        .await
                        .map_err(|_| DispatchError::Timeout)?
                        .map_err(|error| DispatchError::Link(error.to_string()))?;
                self.pairing_tx
                    .send(BoxedGrpcIo::pre_trust_pairing(
                        accepted_quic_bidi_stream(send, recv),
                        PreTrustPairingReachability::NoReusableReachability,
                    ))
                    .await
                    .map_err(|_| DispatchError::ChannelClosed)
            }
            DispatchTarget::Close => {
                connection.close(0_u32.into(), b"connection is not admitted");
                Ok(())
            }
        }
    }

    pub(crate) fn classify_quic(
        &self,
        connection: &quinn::Connection,
    ) -> Result<DispatchTarget, IdentityError> {
        let peer_certificates = connection
            .peer_identity()
            .map(|identity| {
                identity
                    .downcast::<Vec<CertificateDer<'static>>>()
                    .map_err(|_| {
                        IdentityError::TlsConfig(
                            "QUIC peer identity was not a certificate chain".into(),
                        )
                    })
            })
            .transpose()?;
        self.dispatch_target_for_certificate(peer_certificates.as_deref().and_then(|c| c.first()))
    }

    async fn dispatch<IO>(
        &self,
        stream: IO,
        pairing_reachability: PreTrustPairingReachability,
    ) -> Result<(), DispatchError>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let tls_stream = self.accept_tls(stream).await?;

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

    async fn accept_tls<IO>(
        &self,
        stream: IO,
    ) -> Result<tokio_rustls::server::TlsStream<IO>, DispatchError>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        match tokio::time::timeout(self.handshake_timeout, self.acceptor.accept(stream)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => {
                if !take_mtls_audit_emitted() {
                    audit::auth_mtls_handshake_failure(&error);
                }
                Err(DispatchError::Tls(error))
            }
            Err(_) => {
                audit::auth_mtls_handshake_failure("TLS handshake timed out");
                Err(DispatchError::Timeout)
            }
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

        self.dispatch_target_for_certificate(peer_cert)
    }

    fn dispatch_target_for_certificate(
        &self,
        peer_cert: Option<&CertificateDer<'_>>,
    ) -> Result<DispatchTarget, IdentityError> {
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

async fn wait_for_shutdown(shutdown_rx: &mut watch::Receiver<bool>) {
    while !*shutdown_rx.borrow_and_update() {
        if shutdown_rx.changed().await.is_err() {
            return;
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
                signed_in: None,
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
    async fn external_quic_handshake_limiter_is_per_source_ip() {
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

        for _ in 0..EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT {
            assert!(dispatcher.allow_external_quic_handshake(source).await);
        }
        assert!(!dispatcher.allow_external_quic_handshake(source).await);
        assert!(dispatcher.allow_external_quic_handshake(other).await);
    }
}
