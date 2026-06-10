//! `WirePeer`: a scripted protocol actor for wire-conformance tests.
//!
//! Where [`super::Daemon`] speaks user-meaningful verbs, `WirePeer` connects
//! to a real daemon's *external TCP listener* through real device mTLS and
//! drives the `RoutingService.Connect` bidi stream by hand — sending raw
//! `Message` envelopes (Hello, TunnelFrame) and asserting on the exact
//! replies (HelloAck, GoAway) and on stream closure. It is the conformance
//! lab for the adversarial cases the topology layer cannot express: oversize
//! frames, host_id spoofing after mTLS, version mismatch, malformed frames,
//! and handshake rate limiting.
//!
//! The harness seeds a keypair + trust entry for the wire peer in the
//! victim's live trust store (the same pinned-pubkey mTLS every real peer
//! uses), so the victim's dispatcher routes the connection to its trusted
//! ingress and the real `Connect` handler runs end to end.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, stream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tonic::Request;

use crate::HostId;
use crate::identity::load_or_create_device_identity_in;
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::wire::{self, pb};
use crate::routing::{Capabilities, Host, host_to_wire};
use crate::transport::trusted_device_channel;
use crate::trust::{TrustEntry, TrustStore};

use super::TestNet;
use super::assertions::DEFAULT_TIMEOUT;

/// The GoAway reasons the wire-conformance tests assert on. Mirrors the wire
/// enum so the spec suite need not name the crate-private protobuf type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoAwayReason {
    Unspecified,
    ProtocolError,
    AuthExpired,
    UserShutdown,
    UpdateRequired,
    /// Any wire reason not modelled above, carrying its raw code.
    Other(i32),
}

impl GoAwayReason {
    fn from_wire(reason: i32) -> Self {
        match pb::GoAwayReason::try_from(reason) {
            Ok(pb::GoAwayReason::Unspecified) => Self::Unspecified,
            Ok(pb::GoAwayReason::ProtocolError) => Self::ProtocolError,
            Ok(pb::GoAwayReason::AuthExpired) => Self::AuthExpired,
            Ok(pb::GoAwayReason::UserShutdown) => Self::UserShutdown,
            Ok(pb::GoAwayReason::UpdateRequired) => Self::UpdateRequired,
            _ => Self::Other(reason),
        }
    }
}

/// A scripted protocol actor connected to a victim daemon's `Connect` stream.
pub struct WirePeer {
    victim_name: String,
    /// The wire peer's own mTLS-bound host_id (its seeded device identity).
    bound: HostId,
    /// Pushes raw `Message` envelopes into the Connect request stream. Held
    /// here so the request body stays open until the peer is dropped.
    out_tx: mpsc::Sender<pb::Message>,
    inbound: tonic::Streaming<pb::Message>,
}

impl WirePeer {
    /// Connects to `victim`'s external TCP listener through real device mTLS,
    /// trusted via a freshly-seeded keypair in the victim's trust store, and
    /// opens the `RoutingService.Connect` bidi stream. No Hello is sent yet.
    pub async fn connect_trusted(net: &TestNet, victim: &str) -> Self {
        let victim_daemon = net.daemon(victim);
        let addr = victim_daemon.inner.tcp_addr.unwrap_or_else(|| {
            panic!("WirePeer::connect_trusted: victim '{victim}' has no external TCP listener")
        });
        let victim_host_id = victim_daemon.host_id();
        let (_victim_host_id, victim_pubkey) = victim_daemon.identity_on_disk();

        // The wire peer's own device identity, generated in a throwaway dir.
        let key_dir = tempfile::tempdir().expect("wire peer key dir");
        let identity =
            load_or_create_device_identity_in(key_dir.path()).expect("generate wire peer identity");

        // Seed the victim's *live* trust store so its pinned-cert verifier
        // accepts the wire peer (the acceptor reads the store at handshake
        // time, so a live insert is honored).
        let victim_parts = victim_daemon
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("victim '{victim}' is not running"));
        victim_parts
            .trust
            .write()
            .expect("victim trust store poisoned")
            .insert_for_test(
                identity.host_id,
                trust_entry("wire-peer", identity.public_key().to_vec()),
            );

        // The wire peer's own trust store pins the victim, so the device-TLS
        // client verifier accepts the victim's server cert.
        let mut peer_trust = TrustStore::default();
        peer_trust.insert_for_test(victim_host_id, trust_entry(victim, victim_pubkey));
        let peer_trust = Arc::new(std::sync::RwLock::new(peer_trust));

        let bound = identity.host_id;
        let channel = trusted_device_channel(addr, identity, peer_trust, victim_host_id)
            .expect("build wire peer device channel");
        let mut client = wire::routing_service_client::RoutingServiceClient::new(channel);

        let (out_tx, out_rx) = mpsc::channel::<pb::Message>(64);
        let response = client
            .connect(Request::new(request_stream(out_rx)))
            .await
            .expect("open RoutingService.Connect stream");
        Self {
            victim_name: victim.to_string(),
            bound,
            out_tx,
            inbound: response.into_inner(),
        }
    }

    /// Completes the handshake honestly: sends a Hello carrying the wire
    /// peer's own (mTLS-bound) host_id and the supported protocol version,
    /// then asserts the victim accepts it.
    pub async fn hello(&mut self) {
        self.send_hello(self.bound, vec![PROTOCOL_VERSION]).await;
        self.expect_hello_ack_accepted().await;
    }

    /// Sends a Hello whose `host_id` differs from the wire peer's mTLS-bound
    /// identity, to exercise the N-X-9 host_id↔pubkey binding check.
    pub async fn send_hello_spoofing_host_id(&mut self) {
        let mut spoofed = HostId::new_v4();
        while spoofed == self.bound {
            spoofed = HostId::new_v4();
        }
        self.send_hello(spoofed, vec![PROTOCOL_VERSION]).await;
    }

    /// Sends a Hello advertising only a protocol version the daemon does not
    /// support, to exercise version negotiation.
    pub async fn send_hello_with_unsupported_version(&mut self) {
        self.send_hello(self.bound, vec![PROTOCOL_VERSION + 1])
            .await;
    }

    /// Sends a structurally-empty `Message` (no body) as the stream's first
    /// frame, where a Hello is required — the realistic "malformed frame" the
    /// typed gRPC codec still admits.
    pub async fn send_malformed_first_frame(&self) {
        self.send_message(pb::Message { body: None }).await;
    }

    /// Sends a Hello with an arbitrary `host_id` and supported-version list,
    /// for the adversarial handshake cases. The wire peer's mTLS-bound
    /// identity is whatever its seeded cert pins; pass a different `host_id`
    /// to exercise the N-X-9 binding check.
    pub async fn send_hello(&mut self, host_id: HostId, supported_versions: Vec<u32>) {
        let host = Host {
            id: host_id,
            name: "wire-peer".to_string(),
            version: "0.0.0-wire".to_string(),
            capabilities: Capabilities::default(),
        };
        self.send_message(pb::Message {
            body: Some(pb::message::Body::Hello(pb::Hello {
                supported_protocol_versions: supported_versions,
                proposed_link_name: "wire-peer".to_string(),
                host: Some(host_to_wire(&host)),
            })),
        })
        .await;
    }

    /// The wire peer's own mTLS-bound host_id (its seeded device identity).
    pub fn host_id(&self) -> HostId {
        self.bound
    }

    /// Sends a raw `Message` envelope into the Connect request stream.
    pub async fn send_message(&self, message: pb::Message) {
        self.out_tx
            .send(message)
            .await
            .expect("wire peer Connect request stream closed");
    }

    /// Sends a `TunnelFrame` with the given destination route and payload,
    /// under a fresh tunnel id initiated by this peer.
    pub async fn send_tunnel_frame(&self, dst_links: &[&str], payload: Vec<u8>) {
        let tunnel_id = crate::tunnel::TunnelId::new(self.bound).into();
        self.send_message(pb::Message {
            body: Some(pb::message::Body::TunnelFrame(pb::TunnelFrame {
                dst: Some(pb::Route {
                    links: dst_links.iter().map(|link| (*link).to_string()).collect(),
                }),
                tunnel_id: Some(tunnel_id),
                payload,
            })),
        })
        .await;
    }

    /// Reads the next reply (bounded) and asserts it is an *accepted*
    /// HelloAck.
    pub async fn expect_hello_ack_accepted(&mut self) {
        match self.recv_message().await {
            Some(pb::Message {
                body:
                    Some(pb::message::Body::HelloAck(pb::HelloAck {
                        outcome: Some(outcome),
                    })),
            }) => match outcome {
                pb::hello_ack::Outcome::Accepted(_) => {}
                pb::hello_ack::Outcome::Error(error) => panic!(
                    "expected accepted HelloAck from '{}', got error: {}",
                    self.victim_name, error.message
                ),
            },
            other => panic!(
                "expected accepted HelloAck from '{}', got {other:?}",
                self.victim_name
            ),
        }
    }

    /// Reads the next reply (bounded) and asserts it is an *error* HelloAck,
    /// returning the error so callers can inspect its code/message.
    pub async fn expect_hello_ack_error(&mut self) -> pb::Error {
        match self.recv_message().await {
            Some(pb::Message {
                body:
                    Some(pb::message::Body::HelloAck(pb::HelloAck {
                        outcome: Some(outcome),
                    })),
            }) => match outcome {
                pb::hello_ack::Outcome::Error(error) => error,
                pb::hello_ack::Outcome::Accepted(_) => panic!(
                    "expected error HelloAck from '{}', got an accepted one",
                    self.victim_name
                ),
            },
            other => panic!(
                "expected error HelloAck from '{}', got {other:?}",
                self.victim_name
            ),
        }
    }

    /// Drains replies (bounded) until a `GoAway` with the given reason
    /// arrives, returning its embedded error. Intervening messages (e.g. the
    /// initial routing snapshot) are skipped.
    pub async fn expect_goaway(&mut self, reason: GoAwayReason) -> Option<pb::Error> {
        let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.inbound.message()).await {
                Ok(Ok(Some(pb::Message {
                    body: Some(pb::message::Body::Goaway(goaway)),
                }))) => {
                    let got = GoAwayReason::from_wire(goaway.reason);
                    assert_eq!(
                        got, reason,
                        "expected GoAway({reason:?}) from '{}', got GoAway({got:?})",
                        self.victim_name
                    );
                    return goaway.error;
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) => panic!(
                    "'{}' closed the Connect stream before sending GoAway({reason:?})",
                    self.victim_name
                ),
                Ok(Err(status)) => panic!(
                    "Connect stream from '{}' errored before GoAway({reason:?}): {status}",
                    self.victim_name
                ),
                Err(_) => panic!(
                    "timed out waiting for GoAway({reason:?}) from '{}'",
                    self.victim_name
                ),
            }
        }
    }

    /// Asserts the Connect stream terminates (clean end or error) within the
    /// assertion timeout. Messages still in flight are drained first.
    pub async fn expect_stream_closed(&mut self) {
        let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.inbound.message()).await {
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => return,
                Err(_) => panic!(
                    "Connect stream from '{}' stayed open; expected it to close",
                    self.victim_name
                ),
            }
        }
    }

    /// Asserts the Connect stream stays *open* for a short observation
    /// window — no GoAway, no closure — proving the link survived whatever was
    /// just sent. Stray messages already in flight are tolerated.
    pub async fn expect_stream_stays_open(&mut self) {
        const WINDOW: Duration = Duration::from_secs(1);
        let deadline = tokio::time::Instant::now() + WINDOW;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.inbound.message()).await {
                Ok(Ok(Some(pb::Message {
                    body: Some(pb::message::Body::Goaway(goaway)),
                }))) => panic!(
                    "'{}' sent GoAway({:?}); expected the link to stay up",
                    self.victim_name,
                    GoAwayReason::from_wire(goaway.reason)
                ),
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) => panic!(
                    "'{}' closed the Connect stream; expected the link to stay up",
                    self.victim_name
                ),
                Ok(Err(status)) => panic!(
                    "Connect stream from '{}' errored; expected the link to stay up: {status}",
                    self.victim_name
                ),
                Err(_) => return, // silent for the whole window: link is up
            }
        }
    }

    async fn recv_message(&mut self) -> Option<pb::Message> {
        match tokio::time::timeout(DEFAULT_TIMEOUT, self.inbound.message()).await {
            Ok(Ok(message)) => message,
            Ok(Err(status)) => panic!(
                "Connect stream from '{}' errored while awaiting a reply: {status}",
                self.victim_name
            ),
            Err(_) => panic!("timed out waiting for a reply from '{}'", self.victim_name),
        }
    }

    /// Attempts a single anonymous (no client cert) device-TLS handshake
    /// against `victim`'s external TCP listener, returning whether the TLS
    /// handshake completed. An admitted handshake completes on localhost in a
    /// few milliseconds (the dispatcher then closes the anonymous stream at the
    /// gRPC layer, which this probe does not reach); a rate-limited one is
    /// left unanswered — the dispatcher drops it before TLS — so it never
    /// completes and this returns `false` after a short bound.
    pub async fn anonymous_tls_handshake_succeeds(net: &TestNet, victim: &str) -> bool {
        // A rate-limited connection is dropped server-side before any TLS
        // bytes; on localhost an admitted handshake finishes well within this
        // bound, so a timeout reliably means "refused".
        const HANDSHAKE_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
        let addr = net.daemon(victim).inner.tcp_addr.unwrap_or_else(|| {
            panic!("anonymous_tls_handshake_succeeds: victim '{victim}' has no TCP listener")
        });
        let Ok(stream) = TcpStream::connect(addr).await else {
            return false;
        };
        let _ = stream.set_nodelay(true);
        let connector = TlsConnector::from(Arc::new(anonymous_device_client_config()));
        let server_name =
            ServerName::try_from("amux-device.local").expect("static device server name is valid");
        tokio::time::timeout(
            HANDSHAKE_PROBE_TIMEOUT,
            connector.connect(server_name, stream),
        )
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false)
    }
}

fn trust_entry(name: &str, pubkey: Vec<u8>) -> TrustEntry {
    TrustEntry {
        pubkey,
        name: name.to_string(),
        paired_at: chrono::Utc::now(),
        reachabilities: Vec::new(),
    }
}

fn request_stream(rx: mpsc::Receiver<pb::Message>) -> impl Stream<Item = pb::Message> + Send {
    stream::unfold(
        rx,
        |mut rx| async move { rx.recv().await.map(|msg| (msg, rx)) },
    )
}

fn anonymous_device_client_config() -> ClientConfig {
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

#[derive(Debug)]
struct NoServerVerification {
    supported_algs: WebPkiSupportedAlgorithms,
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
