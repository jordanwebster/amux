//! A configured cloud identity and its independently addressed test relay.
//!
//! Mirrors the assembly used by the startup tests: a real
//! [`CloudLinkServer`] served over localhost TCP and QUIC, with a token
//! registry standing in for JWT validation. Daemons in a `TestNet` share one
//! cloud user by default, so the relay bridges them exactly like production
//! cloud routing does for one account; the builder's `cloud_user` verb
//! attaches a daemon under a different user, per-token TTLs let the Reauth
//! flow be driven hermetically, and per-user tiers let free-tier refusals be
//! exercised without an identity service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{Stream, stream};
use node::harness::{
    AuthenticatedLinkUser, CloudLinkServer, Config, ConnectionManager, LinkTokenAuthenticator,
    TLS_HANDSHAKE_TIMEOUT, relay_quic_client_config_with_roots, relay_quic_server_config_from_der,
};
use node::user_state::ServerState;
use node::{Clock, HostId, WallClock};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::latency::Delayed;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// OS-level handles to every TCP connection the relay has accepted, so an
/// outage can sever them for real (spawned connection tasks outlive an
/// aborted accept loop).
type TrackedConnections = Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>;

/// What the relay's authenticator knows about one bearer token. The
/// authenticated session expires `ttl` after each validation, standing in
/// for a JWT `exp` claim, and carries the tier the token was minted with.
#[derive(Clone, Copy)]
pub struct RegisteredToken {
    pub user_id: Uuid,
    pub ttl: Duration,
    pub tier: node::Tier,
}

/// Shared token → user/TTL registry; the relay's authenticator reads it and
/// test verbs (different cloud users, short-lived JWTs) write it.
pub type TokenRegistry = Arc<std::sync::RwLock<HashMap<String, RegisteredToken>>>;

/// Shared account → tier registry, so a test can change what an account is
/// entitled to and see it minted into the account's next token.
pub type UserTierRegistry = Arc<std::sync::RwLock<HashMap<Uuid, node::Tier>>>;

/// TTL for ordinary (non-expiring-test) testnet tokens.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(3600);

pub struct CloudRelay {
    pub url: String,
    pub relay: Relay,
    /// The default cloud user's bearer token.
    pub token: String,
    user_id: Uuid,
    tokens: TokenRegistry,
    user_tiers: UserTierRegistry,
    failures: Arc<std::sync::RwLock<HashMap<Uuid, tonic::Status>>>,
    /// builder `cloud_user` label → that user's `(user_id, token)`.
    user_labels: std::sync::Mutex<HashMap<String, (Uuid, String)>>,
}

/// Carries authenticated device traffic; cloud identity and token issuance
/// belong to [`CloudRelay`], which supplies this relay's address to devices.
pub struct Relay {
    pub addr: SocketAddr,
    /// The UDP socket taken alongside the TCP listener, handed to the first
    /// endpoint served on it. Holding it until then is what keeps the port
    /// this relay proved usable from being taken in between.
    quic_socket: Mutex<Option<std::net::UdpSocket>>,
    pub host_id: HostId,
    tokens: TokenRegistry,
    failures: Arc<std::sync::RwLock<HashMap<Uuid, tonic::Status>>>,
    server: Mutex<Option<RunningCloud>>,
    latency_millis: Arc<AtomicU64>,
    quic_server_config: quinn::ServerConfig,
    quic_client_config: quinn::ClientConfig,
    clock: Arc<dyn Clock>,
}

struct RunningCloud {
    service: CloudLinkServer,
    tasks: Vec<JoinHandle<()>>,
    quic_endpoint: quinn::Endpoint,
    connections: TrackedConnections,
}

impl RunningCloud {
    /// Kills the accept loops and severs every accepted socket. Daemons see
    /// their relay links fail like a genuine outage, not a graceful drain.
    fn sever(mut self) {
        self.quic_endpoint
            .close(quinn::VarInt::from_u32(0), b"test relay offline");
        for task in self.tasks.drain(..) {
            task.abort();
        }
        let connections = std::mem::take(
            &mut *self
                .connections
                .lock()
                .expect("testnet cloud connection registry poisoned"),
        );
        for connection in connections {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for RunningCloud {
    fn drop(&mut self) {
        self.quic_endpoint
            .close(quinn::VarInt::from_u32(0), b"test relay dropped");
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl CloudRelay {
    /// A cloud named by the default installation configuration, with a fresh
    /// loopback relay.
    pub async fn start() -> Self {
        Self::start_with_url_and_clock(node::Config::default().cloud_url, Arc::new(WallClock)).await
    }

    pub async fn start_with_url_and_clock(url: String, clock: Arc<dyn Clock>) -> Self {
        let (listener, quic_socket) = bind_relay_sockets();
        let addr = listener
            .local_addr()
            .expect("testnet cloud relay listener address");
        listener
            .set_nonblocking(true)
            .expect("testnet cloud relay listener is nonblocking");
        let listener = TcpListener::from_std(listener).expect("adopt testnet cloud relay listener");
        let (quic_server_config, quic_client_config) = testnet_quic_configs();
        let token = format!("spec-token-{}", Uuid::new_v4().simple());
        let user_id = Uuid::new_v4();
        let tokens: TokenRegistry = Arc::default();
        let user_tiers: UserTierRegistry = Arc::default();
        user_tiers
            .write()
            .expect("testnet user tier registry poisoned")
            .insert(user_id, node::Tier::Pro);
        tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(
                token.clone(),
                RegisteredToken {
                    user_id,
                    ttl: DEFAULT_TOKEN_TTL,
                    tier: node::Tier::Pro,
                },
            );
        let failures = Arc::default();
        let relay = Relay {
            addr,
            quic_socket: Mutex::new(Some(quic_socket)),
            host_id: Uuid::new_v4(),
            tokens: tokens.clone(),
            failures: Arc::clone(&failures),
            server: Mutex::new(None),
            latency_millis: Arc::default(),
            quic_server_config,
            quic_client_config,
            clock,
        };
        relay.serve(listener).await;
        Self {
            url,
            relay,
            token,
            user_id,
            tokens,
            user_tiers,
            failures,
            user_labels: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The relay assigned by this cloud, independent of its identity URL.
    pub fn relay_addr(&self) -> SocketAddr {
        self.relay.addr
    }

    /// The client configuration trusting this relay's QUIC certificate.
    pub fn quic_client_config(&self) -> quinn::ClientConfig {
        self.relay.quic_client_config.clone()
    }

    pub fn latency_control(&self) -> Arc<AtomicU64> {
        self.relay.latency_millis.clone()
    }

    /// Registers or returns the relay account identified by `label`.
    pub fn register_user(&self, label: &str) -> RelayUser {
        let (user_id, token) = self.credentials_for_user(label);
        RelayUser { user_id, token }
    }

    /// Uses this relay's plaintext transport for an unbound profile.
    pub async fn use_for_profile(
        &self,
        installation: &node::Installation,
        id: node::ProfileId,
    ) -> Result<(), node::installation::InstallationError> {
        installation
            .use_test_cloud_transport(id, self.relay.addr)
            .await
    }

    pub fn reject_user(&self, label: &str, error: Option<tonic::Status>) {
        let (id, _) = self.credentials_for_user(label);
        let mut failures = self.failures.write().unwrap();
        if let Some(error) = error {
            failures.insert(id, error);
        } else {
            failures.remove(&id);
        }
    }

    pub fn default_user_id(&self) -> Uuid {
        self.user_id
    }

    /// The `(user_id, bearer token)` for a builder `cloud_user` label,
    /// minting and registering them on first use.
    pub fn credentials_for_user(&self, label: &str) -> (Uuid, String) {
        let mut labels = self
            .user_labels
            .lock()
            .expect("testnet user label registry poisoned");
        labels
            .entry(label.to_string())
            .or_insert_with(|| {
                let user_id = Uuid::new_v4();
                let token = crate::relay_token(label);
                self.user_tiers
                    .write()
                    .expect("testnet user tier registry poisoned")
                    .insert(user_id, node::Tier::Pro);
                self.register_token(&token, user_id, DEFAULT_TOKEN_TTL);
                (user_id, token)
            })
            .clone()
    }

    /// Registers a bearer token the relay will accept for `user_id`, with
    /// authenticated sessions that expire `ttl` after each validation. The
    /// tier is the one the account currently holds.
    pub fn register_token(&self, token: &str, user_id: Uuid, ttl: Duration) {
        let tier = self
            .user_tiers
            .read()
            .expect("testnet user tier registry poisoned")
            .get(&user_id)
            .copied()
            .unwrap_or(node::Tier::Pro);
        self.tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(token.to_string(), RegisteredToken { user_id, ttl, tier });
    }

    pub fn register_token_with_tier(
        &self,
        token: &str,
        user_id: Uuid,
        ttl: Duration,
        tier: node::Tier,
    ) {
        self.tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(token.to_string(), RegisteredToken { user_id, ttl, tier });
    }

    pub fn token_registry(&self) -> TokenRegistry {
        self.tokens.clone()
    }

    pub fn user_tier_registry(&self) -> UserTierRegistry {
        self.user_tiers.clone()
    }

    pub fn set_user_tier(&self, label: &str, tier: node::Tier) {
        let (user_id, token) = if label == "default" {
            (self.user_id, self.token.clone())
        } else {
            self.credentials_for_user(label)
        };
        self.user_tiers
            .write()
            .expect("testnet user tier registry poisoned")
            .insert(user_id, tier);
        // The account's own bearer is re-minted at the new tier, so a device
        // that authenticates with it afterwards is admitted on what the
        // account now buys. Links already up keep the tier they were admitted
        // on: admission is settled once, when a link connects.
        self.register_token_with_tier(&token, user_id, DEFAULT_TOKEN_TTL, tier);
    }
}

impl Relay {
    async fn serve(&self, listener: TcpListener) {
        let state = testnet_server_state("relay", self.host_id, None);
        state.write().await.is_cloud_server = true;
        let service = CloudLinkServer::with_authenticator(
            state,
            Arc::new(RegistryTokenAuthenticator {
                tokens: self.tokens.clone(),
                failures: self.failures.clone(),
                clock: self.clock.clone(),
            }),
        );
        let connections: TrackedConnections = Arc::default();
        let tcp_task = service.serve_on_incoming(tracked_tcp_incoming(
            listener,
            connections.clone(),
            self.latency_millis.clone(),
        ));
        // A relay that went offline released its port along with its endpoint,
        // exactly as a relay that is not running should: a QUIC dial to it is
        // refused rather than left unanswered.
        let quic_endpoint = match self.quic_socket.lock().await.take() {
            Some(socket) => quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(self.quic_server_config.clone()),
                socket,
                Arc::new(quinn::TokioRuntime),
            )
            .expect("serve QUIC on the relay's socket"),
            None => bind_quic_addr_with_retries(self.quic_server_config.clone(), self.addr).await,
        };
        let quic_task =
            service.serve_on_quic_endpoint(quic_endpoint.clone(), TLS_HANDSHAKE_TIMEOUT);
        *self.server.lock().await = Some(RunningCloud {
            service,
            tasks: vec![tcp_task, quic_task],
            quic_endpoint,
            connections,
        });
    }

    /// Attempts a routed `ClientService.ListAgents` call from the relay's
    /// own position into `host` (within `user_id`'s routing services). The
    /// relay forwards these peers' bytes, but it has no device identity and
    /// no trust entry, so the call must never complete.
    pub async fn try_call_into(&self, user_id: Uuid, host: HostId) -> anyhow::Result<()> {
        let connections = {
            let guard = self.server.lock().await;
            let running = guard
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("the cloud relay is offline"))?;
            running.service.user_routing_connections(user_id).await
        };
        let connections: Arc<ConnectionManager> =
            connections.ok_or_else(|| anyhow::anyhow!("relay has no services for this user"))?;
        let channel = connections.channel_to(host).await?;
        let mut client = wire::client_service_client(channel);
        client.list_agents(wire::ListAgentsRequest {}).await?;
        Ok(())
    }

    pub async fn has_link_to(&self, user_id: Uuid, host: HostId) -> bool {
        let service = {
            let guard = self.server.lock().await;
            guard.as_ref().map(|running| running.service.clone())
        };
        match service {
            Some(service) => service.user_has_link_to(user_id, host).await,
            None => false,
        }
    }

    /// Which hosts this account is connected to the relay by, and how many
    /// links each holds.
    pub async fn links_for(&self, user_id: Uuid) -> Vec<(HostId, usize)> {
        let service = {
            let guard = self.server.lock().await;
            guard.as_ref().map(|running| running.service.clone())
        };
        match service {
            Some(service) => service.user_links(user_id).await,
            None => Vec::new(),
        }
    }

    /// Takes the relay down hard: stops accepting and severs every accepted
    /// socket, so daemons observe a genuine outage (links fail, routes drop).
    pub async fn go_offline(&self) {
        if let Some(running) = self.server.lock().await.take() {
            running.sever();
        }
    }

    /// Restarts the relay on the same address so daemons can reconnect.
    pub async fn go_online(&self) {
        if self.server.lock().await.is_some() {
            return;
        }
        let listener = bind_addr_with_retries(self.addr).await;
        self.serve(listener).await;
    }

    pub async fn is_online(&self) -> bool {
        self.server.lock().await.is_some()
    }

    pub fn set_latency(&self, millis: u64) {
        self.latency_millis.store(millis, Ordering::SeqCst);
    }
}

fn testnet_quic_configs() -> (quinn::ServerConfig, quinn::ClientConfig) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate testnet relay certificate");
    let server = relay_quic_server_config_from_der(
        vec![rustls::pki_types::CertificateDer::from(
            cert.der().as_ref().to_vec(),
        )],
        rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            signing_key.serialize_der(),
        )),
    )
    .expect("configure testnet relay QUIC server");
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            cert.der().as_ref().to_vec(),
        ))
        .expect("trust testnet relay certificate");
    let client =
        relay_quic_client_config_with_roots(roots).expect("configure testnet relay QUIC client");
    (server, client)
}

#[derive(Clone, Debug)]
pub struct RelayUser {
    pub user_id: Uuid,
    pub token: String,
}

/// Accepts TCP connections like the production relay, but keeps an OS-level
/// duplicate handle to each socket so [`RunningCloud::sever`] can cut them,
/// and delays received bytes by the relay's configured latency.
fn tracked_tcp_incoming(
    listener: TcpListener,
    connections: TrackedConnections,
    latency: Arc<AtomicU64>,
) -> impl Stream<Item = std::io::Result<Delayed<TcpStream>>> + Send + 'static {
    stream::unfold(
        (listener, connections, latency),
        |(listener, connections, latency)| async {
            let item = accept_tracked(&listener, &connections, latency.clone()).await;
            Some((item, (listener, connections, latency)))
        },
    )
}

async fn accept_tracked(
    listener: &TcpListener,
    connections: &TrackedConnections,
    latency: Arc<AtomicU64>,
) -> std::io::Result<Delayed<TcpStream>> {
    let (stream, _addr) = listener.accept().await?;
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(error = %error, "failed to set TCP_NODELAY");
    }
    let std_stream = stream.into_std()?;
    if let Ok(duplicate) = std_stream.try_clone() {
        connections
            .lock()
            .expect("testnet cloud connection registry poisoned")
            .push(duplicate);
    }
    Ok(Delayed::new(TcpStream::from_std(std_stream)?, latency))
}

/// Binds `addr`, retrying briefly: right after a relay or daemon shutdown the
/// previous listener socket may not have been released by the OS yet.
pub(crate) async fn bind_addr_with_retries(addr: SocketAddr) -> TcpListener {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match TcpListener::bind(addr).await {
            Ok(listener) => return listener,
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("failed to rebind {addr}: {error}");
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

async fn bind_quic_addr_with_retries(
    config: quinn::ServerConfig,
    addr: SocketAddr,
) -> quinn::Endpoint {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match quinn::Endpoint::server(config.clone(), addr) {
            Ok(endpoint) => return endpoint,
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("failed to rebind QUIC {addr}: {error}");
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

/// Takes the relay's one loopback address on both carriers: TCP for its link
/// listener and UDP for its QUIC endpoint, on the same port number.
///
/// A free TCP port says nothing about that number on UDP. Another test's
/// ephemeral socket may hold it, and Windows reserves whole UDP ranges that no
/// process may bind at all, so a port that the kernel handed out for TCP can be
/// unusable for QUIC. Both are taken together here and a number that fails on
/// either is abandoned for another.
fn bind_relay_sockets() -> (std::net::TcpListener, std::net::UdpSocket) {
    const ATTEMPTS: usize = 50;
    let mut abandoned = Vec::new();
    for _ in 0..ATTEMPTS {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("bind testnet cloud relay listener");
        let port = listener
            .local_addr()
            .expect("testnet cloud relay listener address")
            .port();
        match std::net::UdpSocket::bind(("127.0.0.1", port)) {
            Ok(socket) => return (listener, socket),
            // Holding the rejected listener until every attempt is done stops
            // the kernel from offering the same unusable number again.
            Err(_) => abandoned.push(listener),
        }
    }
    panic!("no loopback port was free for both the relay's TCP and QUIC listeners");
}

/// Minimal relay state for an in-process test network.
///
/// `tcp_port` is the relay's configured TCP link port.
pub(crate) fn testnet_server_state(
    host_name: &str,
    host_id: HostId,
    tcp_port: Option<u16>,
) -> std::sync::Arc<RwLock<ServerState>> {
    let config = Config {
        host_name: host_name.to_string(),
        tcp_port,

        ..Config::default()
    };
    std::sync::Arc::new(RwLock::new(ServerState::new(config, host_id, None, None)))
}

#[derive(Clone)]
struct RegistryTokenAuthenticator {
    tokens: TokenRegistry,
    failures: Arc<std::sync::RwLock<HashMap<Uuid, tonic::Status>>>,
    clock: Arc<dyn Clock>,
}

#[tonic::async_trait]
impl LinkTokenAuthenticator for RegistryTokenAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        let registered = self
            .tokens
            .read()
            .expect("testnet token registry poisoned")
            .get(token)
            .copied()
            .ok_or_else(|| tonic::Status::unauthenticated("unknown testnet token"))?;
        if let Some(error) = self.failures.read().unwrap().get(&registered.user_id) {
            return Err(error.clone());
        }
        Ok(AuthenticatedLinkUser {
            user_id: registered.user_id,
            client_id: "test-client".to_string(),
            expires_at: self.clock.system_now() + registered.ttl,
            tier: registered.tier,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn changed_cloud_user_tier_is_minted_into_this_accounts_tokens() {
        let relay = CloudRelay::start().await;
        let (user_id, token) = relay.credentials_for_user("alice");

        relay.set_user_tier("alice", node::Tier::Free);
        relay.register_token("after-tier-change", user_id, DEFAULT_TOKEN_TTL);

        let tier_of = |token: &str| {
            relay
                .tokens
                .read()
                .unwrap()
                .get(token)
                .expect("a registered token")
                .tier
        };
        assert_eq!(tier_of("after-tier-change"), node::Tier::Free);
        assert_eq!(
            tier_of(&token),
            node::Tier::Free,
            "the account's own bearer is what a client that has one keeps using"
        );
    }
}
