//! A resolved relay for embedders whose account API lives outside this crate.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use model::RelayCarrier;
pub use model::{DisconnectReason, RelayConnection};
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::link::{CarrierKind, LinkCarrier, MuxCarrier, MuxRole, QuicCarrier};
use crate::profile::status::{Observed, RuntimeStatus};
use crate::routing::{
    LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRole,
    spawn_connector_with_auth_establishment_and_shutdown,
};
use crate::services::{TCP_FALLBACK_DELAY, UdpBlockedMemory, select_cloud_carrier};
use crate::{CredentialProvider, ServerError};

/// Validated endpoint: an HTTPS origin, or a plaintext loopback address that
/// only a caller which has decided to allow one can construct.
#[derive(Clone)]
pub struct RelayEndpoint {
    host: String,
    port: u16,
    plain: Option<SocketAddr>,
    quic: Option<RelayQuicTrust>,
}

/// A relay QUIC identity supplied by whoever resolved the relay, for a relay
/// this process cannot verify from the system trust store.
#[derive(Clone)]
struct RelayQuicTrust {
    server_name: String,
    config: quinn::ClientConfig,
}

/// Where this device's relay dials run, and what they remember about the
/// network they ran on. One profile's QUIC endpoint and its UDP-blocked
/// memory, so a phone that has learned this network eats UDP does not sit
/// through a dial that cannot finish again.
pub(crate) struct RelayTransport {
    pub(crate) quic_endpoint: quinn::Endpoint,
    pub(crate) udp_blocked: Arc<UdpBlockedMemory>,
}

impl RelayEndpoint {
    pub fn system(url: &str) -> Result<Self, ServerError> {
        let url = reqwest::Url::parse(url).map_err(|e| ServerError::State(e.to_string()))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ServerError::State("relay must be an HTTPS origin".into()));
        }
        Ok(Self {
            host: url
                .host_str()
                .ok_or_else(|| ServerError::State("relay host missing".into()))?
                .into(),
            port: url.port_or_known_default().unwrap_or(443),
            plain: None,
            quic: None,
        })
    }

    /// A plaintext relay on this machine. Node offers the capability; whether
    /// a product may use it is the embedding application's decision, and a
    /// shipping build must never expose a way to construct one.
    pub fn plain_loopback(address: SocketAddr) -> Result<Self, ServerError> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(ServerError::State(
                "plaintext relay must be a loopback address with a port".into(),
            ));
        }
        Ok(Self {
            host: address.ip().to_string(),
            port: address.port(),
            plain: Some(address),
            quic: None,
        })
    }

    /// Where the connection is dialled. The cloud hands a device this route;
    /// pairing invitations name the cloud from configuration instead.
    pub fn url(&self) -> String {
        if let Some(address) = self.plain {
            return format!("http://{address}");
        }
        if self.port == 443 {
            format!("https://{}", self.host)
        } else {
            format!("https://{}:{}", self.host, self.port)
        }
    }

    /// Trust this relay's QUIC identity from a supplied configuration instead
    /// of the system store.
    ///
    /// A relay reached over HTTPS is verified the way any other HTTPS origin
    /// is and needs nothing here. A relay that is a machine on this network — a
    /// harness, a private deployment — has no publicly verifiable name, so
    /// without this the only carrier left for it is TCP. Supplying the trust is
    /// how such an embedder gets QUIC.
    pub fn with_quic_trust(mut self, server_name: String, config: quinn::ClientConfig) -> Self {
        self.quic = Some(RelayQuicTrust {
            server_name,
            config,
        });
        self
    }

    /// Opens the byte stream this endpoint names and wraps it as the link's
    /// carrier. Dialling happens here rather than lazily: the caller's retry
    /// loop reports what an unreachable relay did, and it can only do that if
    /// the failure arrives now.
    ///
    /// QUIC first with TCP behind it, the same race every other device runs:
    /// the relay answers both, QUIC is the carrier the product wants, and a
    /// network that silently eats UDP still has to reach the relay rather than
    /// wait out a dial that can never finish. Losing dials are closed, and a
    /// network that has refused UDP is remembered, so the next attempt on it
    /// goes straight to TCP.
    async fn carrier(
        &self,
        transport: &RelayTransport,
    ) -> Result<(Arc<dyn LinkCarrier>, RelayCarrier), ServerError> {
        let tcp = self.tcp_dial();
        let Some(quic) = self.quic_dial(&transport.quic_endpoint) else {
            return tcp
                .await
                .map(|carrier| (carrier, RelayCarrier::Tcp))
                .map_err(ServerError::State);
        };
        select_cloud_carrier(
            &self.host,
            transport.udp_blocked.clone(),
            TCP_FALLBACK_DELAY,
            quic,
            tcp,
        )
        .await
        .map_err(ServerError::State)
    }

    /// The TCP dial: TLS to an HTTPS relay, cleartext to a loopback one.
    fn tcp_dial(&self) -> impl Future<Output = Result<Arc<dyn LinkCarrier>, String>> + Send {
        let plain = self.plain;
        let host = self.host.clone();
        let port = self.port;
        async move {
            fn relay_carrier<IO>(io: IO) -> Arc<dyn LinkCarrier>
            where
                IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
            {
                Arc::new(MuxCarrier::new(
                    io,
                    MuxRole::Connector,
                    CarrierKind::RelayTcp,
                ))
            }

            if let Some(address) = plain {
                let stream = TcpStream::connect(address)
                    .await
                    .map_err(|error| error.to_string())?;
                stream.set_nodelay(true).map_err(|e| e.to_string())?;
                super::configure_relay_tcp_keepalive(&stream);
                return Ok(relay_carrier(stream));
            }
            let stream = super::tls_connect_stream(&host, port)
                .await
                .map_err(|error| error.to_string())?;
            Ok(relay_carrier(stream))
        }
    }

    /// The QUIC dial, or nothing when this relay has no identity QUIC could
    /// verify: a cleartext relay nobody supplied trust for is reachable over
    /// TCP alone, because QUIC has no cleartext mode to offer it.
    fn quic_dial(
        &self,
        endpoint: &quinn::Endpoint,
    ) -> Option<impl Future<Output = Result<Arc<dyn LinkCarrier>, String>> + Send + 'static> {
        let trust = self.quic.clone();
        if self.plain.is_some() && trust.is_none() {
            return None;
        }
        let endpoint = endpoint.clone();
        let plain = self.plain;
        let host = self.host.clone();
        let port = self.port;
        Some(async move {
            let carrier = match trust {
                Some(trust) => {
                    let addrs = match plain {
                        Some(address) => vec![address],
                        None => tokio::net::lookup_host((host.as_str(), port))
                            .await
                            .map_err(|error| error.to_string())?
                            .collect(),
                    };
                    QuicCarrier::connect_relay_candidates_with_config(
                        &endpoint,
                        addrs,
                        &trust.server_name,
                        port,
                        trust.config,
                    )
                    .await
                }
                None => QuicCarrier::connect_relay(&endpoint, &host, port).await,
            };
            carrier
                .map(|carrier| Arc::new(carrier) as Arc<dyn LinkCarrier>)
                .map_err(|error| error.to_string())
        })
    }
}

/// Classify a failure the relay loop saw. Codes rather than messages: the
/// message is a sentence from somewhere below us and changes with the
/// dependency.
fn disconnect_reason(status: &tonic::Status) -> DisconnectReason {
    match status.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => DisconnectReason::Rejected,
        tonic::Code::Unavailable => DisconnectReason::Unreachable,
        tonic::Code::DeadlineExceeded => DisconnectReason::TimedOut,
        _ => DisconnectReason::Ended,
    }
}

/// A relay route and its credentials. Cloud identity belongs to configuration.
pub struct EmbeddedRelay {
    pub endpoint: RelayEndpoint,
    /// Supplies routing tokens, not account access tokens.
    pub credentials: Arc<dyn CredentialProvider>,
    pub connection: watch::Sender<RelayConnection>,
    /// Shared with whoever can ask for an immediate attempt. An embedder that
    /// has no such control leaves it at its default and the loop waits out
    /// every backoff.
    pub retry: Arc<RelayRetry>,
}

/// How long an honoured retry keeps the next one waiting.
///
/// A second, because that is roughly how fast a control can be tapped by
/// somebody who thinks nothing happened.
const RETRY_COOLDOWN: Duration = Duration::from_secs(1);

/// How long a link put away is given to close politely before it is dropped.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Asking the relay connection to stop waiting and try now.
///
/// The connection retries on a backoff after every failure, which is right
/// while nobody is watching and wrong the moment somebody is: a person who has
/// just walked back into signal is looking at the screen, and the honest answer
/// to "try again" is to try again rather than to finish a four-second wait.
///
/// What a request does is shorten one wait. It never resets the schedule and
/// never starts a second connection: a control that can be pressed ten times in
/// a second would otherwise turn an unreachable relay into a tight reconnect
/// loop, which is the thing the backoff exists to prevent. So a request inside
/// `RETRY_COOLDOWN` of an honoured one is dropped, and the wait after the early
/// attempt is the one the backoff had already chosen.
/// Also where being put away is said.
///
/// A phone in somebody's pocket is not a client with a network problem. It
/// holds no connection at all: the socket is released rather than left for the
/// system to freeze, so the machines it was watching see it leave immediately
/// instead of holding a link nobody is reading. Coming back is a dial, not a
/// recovery, and the reconciliation that follows is the ordinary one.
#[derive(Default)]
pub struct RelayRetry {
    asked: tokio::sync::Notify,
    honoured: std::sync::Mutex<Option<tokio::time::Instant>>,
    attempts: std::sync::atomic::AtomicU64,
    shortened: std::sync::atomic::AtomicU64,
    suspended: std::sync::atomic::AtomicBool,
    lifecycle: tokio::sync::Notify,
    carrier: std::sync::Mutex<Option<RelayCarrier>>,
}

impl RelayRetry {
    /// Stop waiting and dial the relay now. Returns immediately; whether the
    /// request shortened anything depends on how recently one already did.
    pub fn now(&self) {
        self.asked.notify_one();
    }

    /// How many times this connection has dialled the relay. It counts every
    /// attempt, whether a wait was shortened for it or it came round on the
    /// backoff, so a reader can tell one attempt from none.
    pub fn attempts(&self) -> u64 {
        self.attempts.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Which carrier the live relay link runs on, and nothing at all while
    /// there is no link. What a device is told about how it is reaching the
    /// relay: the dial decides it, so nobody above can do better than read it.
    pub fn carrier(&self) -> Option<RelayCarrier> {
        *self.carrier.lock().expect("relay carrier lock poisoned")
    }

    fn on_carrier(&self, carrier: Option<RelayCarrier>) {
        *self.carrier.lock().expect("relay carrier lock poisoned") = carrier;
    }

    /// How many of those attempts happened early because somebody asked.
    ///
    /// The only unambiguous evidence that a request reached this connection: a
    /// dial at a relay that is not there arrives nowhere, and the connection
    /// dials on its own schedule anyway, so "an attempt happened" cannot tell
    /// a request apart from the backoff coming round. This can.
    pub fn shortened(&self) -> u64 {
        self.shortened.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Put the connection away, or bring it back.
    ///
    /// Suspending severs the link now rather than letting the system freeze a
    /// socket the far side still believes in; resuming dials at once, because
    /// somebody is looking at the screen.
    pub fn set_active(&self, active: bool) {
        let was = self
            .suspended
            .swap(!active, std::sync::atomic::Ordering::SeqCst);
        if was == !active {
            return;
        }
        self.lifecycle.notify_waiters();
        self.asked.notify_one();
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves when the connection is put away, and never while it is not.
    /// Held against a live connection so that going away severs it.
    async fn until_suspended(&self) {
        self.until(true).await
    }

    /// Parks while the app is away. Nothing dials, nothing backs off, and no
    /// attempt is counted, because none is made.
    async fn while_suspended(&self) {
        self.until(false).await
    }

    /// Waits for suspension to reach `wanted`. The wait is registered before
    /// the state is read, because a change that lands between the two would
    /// otherwise be waited for after it had already happened.
    async fn until(&self, wanted: bool) {
        loop {
            let changed = self.lifecycle.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_suspended() == wanted {
                return;
            }
            changed.await;
        }
    }

    fn attempted(&self) {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Waits out the backoff, or less if somebody asks and is not asking again
    /// too soon after the last time this listened to them.
    async fn wait(&self, backoff: Duration) {
        let sleep = tokio::time::sleep(backoff);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return,
                _ = self.asked.notified() => {
                    let mut honoured = self.honoured.lock().unwrap();
                    let now = tokio::time::Instant::now();
                    if honoured.is_some_and(|last| now.duration_since(last) < RETRY_COOLDOWN) {
                        continue;
                    }
                    *honoured = Some(now);
                    self.shortened
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The account's credentials, plus the last thing they said about the tier.
///
/// The tier is remembered rather than returned because the link's refresher
/// answers a routing question and this answers an entitlement one: the same
/// reply carries both, and only one of them has a caller waiting.
struct RoutingCredentials {
    provider: Arc<dyn CredentialProvider>,
    tier: std::sync::Mutex<Option<crate::Tier>>,
}

impl RoutingCredentials {
    fn new(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            provider,
            tier: std::sync::Mutex::new(None),
        }
    }

    /// What the account service last said this account buys, or free where it
    /// said nothing.
    fn tier(&self) -> crate::Tier {
        self.tier
            .lock()
            .expect("relay tier lock poisoned")
            .unwrap_or(crate::Tier::Free)
    }
}

#[async_trait::async_trait]
impl LinkConnectorTokenRefresher for RoutingCredentials {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        let token = self
            .provider
            .access_token()
            .await
            .map_err(|e| tonic::Status::unauthenticated(e.to_string()))?;
        *self.tier.lock().expect("relay tier lock poisoned") = token.tier;
        Ok(LinkConnectorToken {
            token: token.bearer,
            expires_at: token
                .expires_at
                .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(3600)),
            // An embedder hands us an opaque bearer, so we cannot read the
            // account's tier off it. The local tier decides only how soon to
            // re-authenticate, never what this daemon may do — the relay
            // enforces that from the token's own claim. Assuming free means
            // this client re-checks sooner and can never wrongly grant itself
            // anything.
            tier: crate::Tier::Free,
        })
    }
}

impl EmbeddedRelay {
    pub(crate) fn spawn(
        self,
        context: LinkConnectorCtx,
        transport: RelayTransport,
        status: RuntimeStatus,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(250);
            loop {
                if self.retry.is_suspended() {
                    self.connection.send_replace(RelayConnection::Disconnected {
                        reason: DisconnectReason::Suspended,
                    });
                    // A phone in a pocket has no link and expects one back the
                    // moment somebody looks; that is the same thing a device
                    // waiting out a backoff is doing, and a reader has nothing
                    // to do differently about either.
                    status.report(Observed::Retrying);
                    self.retry.while_suspended().await;
                    // Back in front of somebody: dial now, not on the wait the
                    // last failure had chosen.
                    backoff = Duration::from_millis(250);
                    self.connection.send_replace(RelayConnection::Connecting);
                    continue;
                }
                self.retry.attempted();
                status.report(Observed::Connecting);
                let result = self.connect(context.clone(), &transport, &status).await;
                self.retry.on_carrier(None);
                let reason = match result {
                    Ok(()) => {
                        backoff = Duration::from_millis(250);
                        DisconnectReason::Ended
                    }
                    Err(reason) => reason,
                };
                self.connection
                    .send_replace(RelayConnection::Disconnected { reason });
                status.report(match reason {
                    // The relay turned this device away rather than failing to
                    // answer: nothing a retry does fixes a credential.
                    DisconnectReason::Rejected => Observed::AuthenticationRequired,
                    _ => Observed::Retrying,
                });
                if reason == DisconnectReason::Suspended {
                    continue;
                }
                self.retry.wait(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(4));
            }
        })
    }

    async fn connect(
        &self,
        context: LinkConnectorCtx,
        transport: &RelayTransport,
        status: &RuntimeStatus,
    ) -> Result<(), DisconnectReason> {
        let credentials = Arc::new(RoutingCredentials::new(self.credentials.clone()));
        let token = tokio::select! {
            token = credentials.refresh_routing_token() => token.map_err(|e| {
                tracing::debug!(error = %e, "relay credentials refused");
                disconnect_reason(&e)
            })?,
            () = self.retry.until_suspended() => return Err(DisconnectReason::Suspended),
        };
        let (carrier, relay_carrier) = self.endpoint.carrier(transport).await.map_err(|e| {
            tracing::debug!(error = %e, "relay endpoint unusable");
            DisconnectReason::Unreachable
        })?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (mut task, established) = spawn_connector_with_auth_establishment_and_shutdown(
            context.with_link_role(LinkRole::CloudRelay),
            carrier,
            LinkConnectorAuth::new(token, credentials.clone()),
            shutdown_rx,
            // Nothing outside this loop asks for a fresh token: the refresher
            // below is the only way this link re-authenticates.
            None,
        );
        let _guard = AbortOnDrop(task.abort_handle());
        tokio::select! {
            settled = tokio::time::timeout(Duration::from_secs(10), established) => settled
                .map_err(|_| DisconnectReason::TimedOut)?
                .map_err(|_| DisconnectReason::Ended)?
                .map_err(|e| {
                    tracing::debug!(error = %e, "relay handshake failed");
                    disconnect_reason(&e)
                })?,
            () = self.retry.until_suspended() => return Err(DisconnectReason::Suspended),
        };
        tracing::info!(carrier = ?relay_carrier, "relay link established");
        self.retry.on_carrier(Some(relay_carrier));
        self.connection.send_replace(RelayConnection::Connected);
        // What this device is reaching the relay on, and what the account it
        // is reaching it for buys. Both are facts of this link rather than of
        // any configuration: the dial chose the carrier, and the tier came
        // back with the token the application obtained.
        status.report(Observed::Connected {
            tier: credentials.tier(),
            carrier: relay_carrier,
        });
        tokio::select! {
            joined = &mut task => joined.map_err(|_| DisconnectReason::Ended)?.map_err(|e| {
                tracing::debug!(error = %e, "relay connection ended");
                disconnect_reason(&e)
            }),
            // Going away closes the link rather than abandoning it: a machine
            // this phone was watching should see it leave now, not when the
            // relay eventually gives up on a socket nobody is reading.
            () = self.retry.until_suspended() => {
                let _ = shutdown.send(true);
                let _ = tokio::time::timeout(SHUTDOWN_GRACE, &mut task).await;
                Err(DisconnectReason::Suspended)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::net::TcpListener;

    use super::*;
    use crate::services::UDP_BLOCKED_MEMORY;

    /// A relay at one address, answering on TCP and — when it has been given a
    /// QUIC front — on UDP at the same port, which is how the product's relay
    /// presents itself. Holds what it accepts: a dial has to complete, and a
    /// dropped connection would fail the dial it is standing in for.
    struct RelayEndpoints {
        addr: SocketAddr,
        server_name: String,
        quic_client: quinn::ClientConfig,
        _tasks: Vec<tokio::task::JoinHandle<()>>,
        _quic: Option<quinn::Endpoint>,
    }

    async fn relay_answering(quic: bool) -> RelayEndpoints {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["relay.test".to_string()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(cert.der().as_ref().to_vec()))
            .unwrap();
        let quic_client = super::super::relay_quic_client_config_with_roots(roots).unwrap();

        let server = super::super::relay_quic_server_config_from_der(
            vec![CertificateDer::from(cert.der().as_ref().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der())),
        )
        .unwrap();

        // One port on both namespaces, because the endpoint names one address
        // for both carriers. The system hands out a free TCP port without
        // regard to UDP, so a taken UDP port is somebody else's and the whole
        // pair is tried again rather than failing a test about carriers.
        let (listener, endpoint) = loop {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            if !quic {
                break (listener, None);
            }
            match quinn::Endpoint::server(server.clone(), addr) {
                Ok(endpoint) => break (listener, Some(endpoint)),
                Err(_) => continue,
            }
        };
        let addr = listener.local_addr().unwrap();
        let mut tasks = vec![tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        })];
        if let Some(endpoint) = &endpoint {
            let accepting = endpoint.clone();
            tasks.push(tokio::spawn(async move {
                let mut held = Vec::new();
                while let Some(incoming) = accepting.accept().await {
                    if let Ok(connection) = incoming.await {
                        held.push(connection);
                    }
                }
            }));
        }

        RelayEndpoints {
            addr,
            server_name: "relay.test".into(),
            quic_client,
            _tasks: tasks,
            _quic: endpoint,
        }
    }

    fn dialling_from() -> RelayTransport {
        RelayTransport {
            quic_endpoint: quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap(),
            udp_blocked: Arc::new(UdpBlockedMemory::new(
                UDP_BLOCKED_MEMORY,
                Arc::new(crate::WallClock),
            )),
        }
    }

    #[tokio::test]
    async fn a_relay_answering_both_carriers_is_reached_over_quic() {
        let relay = relay_answering(true).await;
        let endpoint = RelayEndpoint::plain_loopback(relay.addr)
            .unwrap()
            .with_quic_trust(relay.server_name.clone(), relay.quic_client.clone());

        let (carrier, selected) = endpoint.carrier(&dialling_from()).await.unwrap();

        assert_eq!(selected, RelayCarrier::Quic);
        assert_eq!(carrier.kind(), CarrierKind::RelayQuic);
        println!("relay on both carriers: reached over {selected:?}");
    }

    #[tokio::test]
    async fn a_network_that_eats_udp_falls_back_to_tcp_and_is_remembered() {
        let relay = relay_answering(false).await;
        let endpoint = RelayEndpoint::plain_loopback(relay.addr)
            .unwrap()
            .with_quic_trust(relay.server_name.clone(), relay.quic_client.clone());
        let transport = dialling_from();

        let (carrier, selected) = endpoint.carrier(&transport).await.unwrap();
        assert_eq!(selected, RelayCarrier::Tcp);
        assert_eq!(carrier.kind(), CarrierKind::RelayTcp);

        tokio::time::timeout(Duration::from_secs(10), async {
            while !transport.udp_blocked.holds("127.0.0.1") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a QUIC probe that failed everywhere was not remembered");

        let started = tokio::time::Instant::now();
        let (_, again) = endpoint.carrier(&transport).await.unwrap();
        assert_eq!(again, RelayCarrier::Tcp);
        assert!(
            started.elapsed() < TCP_FALLBACK_DELAY,
            "a remembered UDP-blocked network still waited out the QUIC race: {:?}",
            started.elapsed()
        );
        println!("UDP-blocked network: fell back to TCP and dialled TCP first next time");
    }

    #[tokio::test]
    async fn a_cleartext_relay_nobody_vouched_for_is_dialled_over_tcp_alone() {
        let relay = relay_answering(true).await;
        let endpoint = RelayEndpoint::plain_loopback(relay.addr).unwrap();
        let transport = dialling_from();

        let (carrier, selected) = endpoint.carrier(&transport).await.unwrap();

        assert_eq!(selected, RelayCarrier::Tcp);
        assert_eq!(carrier.kind(), CarrierKind::RelayTcp);
        assert!(
            !transport.udp_blocked.holds("127.0.0.1"),
            "a relay with no verifiable identity taught this device nothing about UDP"
        );
        println!("cleartext relay with no supplied trust: TCP only, nothing learned about UDP");
    }

    /// The connection's own loop with the dial taken out of it: it waits, and
    /// what it does when the wait ends is count an attempt. Advancing the
    /// clock and asking for a retry are every input the real loop has, so a
    /// test that has both can say what the real loop would do — without a
    /// relay, a socket, or a second of anybody's wall time.
    fn connection(retry: &Arc<RelayRetry>, backoff: Duration) -> tokio::task::JoinHandle<()> {
        let retry = Arc::clone(retry);
        tokio::spawn(async move {
            loop {
                retry.wait(backoff).await;
                retry.attempted();
            }
        })
    }

    /// Moves the clock and lets the connection act on what that woke. The
    /// advance alone only fires the timer; the task it belongs to still needs
    /// a turn before the test can ask what it did.
    async fn advance(duration: Duration) {
        tokio::time::advance(duration).await;
        tokio::task::yield_now().await;
    }

    /// Nothing has asked, so nothing happens early: the wait runs its course
    /// and the dial that follows it belongs to the backoff.
    #[tokio::test(start_paused = true)]
    async fn a_wait_nobody_shortens_runs_its_course() {
        let retry = Arc::new(RelayRetry::default());
        let _connection = connection(&retry, Duration::from_secs(4));
        tokio::task::yield_now().await;

        advance(Duration::from_millis(3_900)).await;
        assert_eq!(retry.attempts(), 0, "the wait ended early");

        advance(Duration::from_millis(200)).await;
        assert_eq!(retry.attempts(), 1, "the wait never ended");
        assert_eq!(
            retry.shortened(),
            0,
            "a wait was cut short before anything had asked"
        );
    }

    /// Ten presses in half a second are one person pressing again because
    /// nothing looked like it happened. One of them is listened to, and the
    /// wait after it is the one the backoff had already chosen — otherwise a
    /// control that can be held down turns an unreachable relay into a tight
    /// reconnect loop, which is the whole reason the backoff exists.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_requests_cuts_short_exactly_one_wait() {
        let retry = Arc::new(RelayRetry::default());
        let _connection = connection(&retry, Duration::from_secs(4));
        tokio::task::yield_now().await;

        for _ in 0..10 {
            retry.now();
            advance(Duration::from_millis(50)).await;
        }
        assert_eq!(
            retry.shortened(),
            1,
            "ten presses cut short {} waits, not one",
            retry.shortened()
        );
        assert_eq!(
            retry.attempts(),
            1,
            "ten presses dialled {} times, not once",
            retry.attempts()
        );

        // The early dial started the ordinary four-second wait, not another
        // round of dialling.
        advance(Duration::from_millis(3_000)).await;
        assert_eq!(retry.attempts(), 1, "the early dial started a tight loop");
        advance(Duration::from_millis(600)).await;
        assert_eq!(retry.attempts(), 2, "the backoff never came round");
        assert_eq!(
            retry.shortened(),
            1,
            "the backoff coming round was counted as somebody asking"
        );
    }

    /// A press a whole cooldown after the last one is somebody asking again,
    /// not the same press arriving twice, and it is listened to.
    #[tokio::test(start_paused = true)]
    async fn a_request_after_the_cooldown_is_listened_to() {
        let retry = Arc::new(RelayRetry::default());
        let _connection = connection(&retry, Duration::from_secs(4));
        tokio::task::yield_now().await;

        retry.now();
        advance(Duration::from_millis(10)).await;
        assert_eq!(retry.shortened(), 1, "the first press was dropped");

        advance(RETRY_COOLDOWN).await;
        retry.now();
        advance(Duration::from_millis(10)).await;
        assert_eq!(
            retry.shortened(),
            2,
            "a press a whole cooldown later was dropped as too soon"
        );
        assert_eq!(retry.attempts(), 2, "the second press never dialled");
    }
}
