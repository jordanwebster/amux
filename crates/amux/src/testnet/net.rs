//! In-process cloud relay for testnet topologies.
//!
//! Mirrors the assembly used by the startup tests: a real
//! [`CloudLinkService`] served over localhost TCP, with a bearer-token
//! registry standing in for JWT validation. Daemons in a `TestNet` share one
//! cloud user by default, so the relay bridges them exactly like production
//! cloud routing does for one account; the builder's `cloud_user` verb
//! attaches a daemon under a different user, and per-token TTLs let the
//! Reauth flow be driven hermetically.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::{Stream, stream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::assertions::POLL_INTERVAL;
use crate::HostId;
use crate::config::Config;
use crate::connection::ConnectionManager;
use crate::protocol::wire;
use crate::routing::{AuthenticatedLinkUser, LinkTokenAuthenticator};
use crate::services::CloudLinkService;
use crate::transport::TcpServerTransport;
use crate::user_state::{ServerState, ShutdownRequest};

/// OS-level handles to every TCP connection the relay has accepted, so an
/// outage can sever them for real (tonic's spawned connection tasks outlive
/// an aborted accept loop).
type TrackedConnections = Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>;

/// What the relay's authenticator knows about one bearer token. The
/// authenticated session expires `ttl` after each validation, standing in
/// for a JWT `exp` claim.
#[derive(Clone, Copy)]
pub(crate) struct RegisteredToken {
    pub(crate) user_id: Uuid,
    pub(crate) ttl: Duration,
}

/// Shared token → user/TTL registry; the relay's authenticator reads it and
/// test verbs (different cloud users, short-lived JWTs) write it.
pub(crate) type TokenRegistry = Arc<std::sync::RwLock<HashMap<String, RegisteredToken>>>;

/// TTL for ordinary (non-expiring-test) testnet tokens.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(3600);

pub(crate) struct CloudRelay {
    pub(crate) addr: SocketAddr,
    pub(crate) host_id: HostId,
    /// The default cloud user's bearer token.
    pub(crate) token: String,
    user_id: Uuid,
    tokens: TokenRegistry,
    /// builder `cloud_user` label → that user's `(user_id, token)`.
    user_labels: std::sync::Mutex<HashMap<String, (Uuid, String)>>,
    server: Mutex<Option<RunningCloud>>,
}

struct RunningCloud {
    service: CloudLinkService,
    task: JoinHandle<()>,
    connections: TrackedConnections,
}

impl RunningCloud {
    /// Kills the accept loop and severs every accepted socket. Daemons see
    /// their relay links fail like a genuine outage, not a graceful drain.
    fn sever(self) {
        self.task.abort();
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
        self.task.abort();
    }
}

impl CloudRelay {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind testnet cloud relay listener");
        let addr = listener
            .local_addr()
            .expect("testnet cloud relay listener address");
        let token = format!("spec-token-{}", Uuid::new_v4().simple());
        let user_id = Uuid::new_v4();
        let tokens: TokenRegistry = Arc::default();
        tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(
                token.clone(),
                RegisteredToken {
                    user_id,
                    ttl: DEFAULT_TOKEN_TTL,
                },
            );
        let relay = Self {
            addr,
            host_id: Uuid::new_v4(),
            token,
            user_id,
            tokens,
            user_labels: std::sync::Mutex::new(HashMap::new()),
            server: Mutex::new(None),
        };
        relay.serve(listener).await;
        relay
    }

    async fn serve(&self, listener: TcpListener) {
        let (state, _shutdown_rx) = testnet_server_state("cloud", self.host_id, None, false);
        state.write().await.is_cloud_server = true;
        let service = CloudLinkService::with_authenticator(
            state,
            Arc::new(RegistryTokenAuthenticator {
                tokens: self.tokens.clone(),
            }),
        );
        let connections: TrackedConnections = Arc::default();
        let task = service.serve_on_incoming(tracked_tcp_incoming(listener, connections.clone()));
        *self.server.lock().await = Some(RunningCloud {
            service,
            task,
            connections,
        });
    }

    pub(crate) fn default_user_id(&self) -> Uuid {
        self.user_id
    }

    /// The `(user_id, bearer token)` for a builder `cloud_user` label,
    /// minting and registering them on first use.
    pub(crate) fn credentials_for_user(&self, label: &str) -> (Uuid, String) {
        let mut labels = self
            .user_labels
            .lock()
            .expect("testnet user label registry poisoned");
        labels
            .entry(label.to_string())
            .or_insert_with(|| {
                let user_id = Uuid::new_v4();
                let token = format!("spec-token-{label}-{}", Uuid::new_v4().simple());
                self.register_token(&token, user_id, DEFAULT_TOKEN_TTL);
                (user_id, token)
            })
            .clone()
    }

    /// Registers a bearer token the relay will accept for `user_id`, with
    /// authenticated sessions that expire `ttl` after each validation.
    pub(crate) fn register_token(&self, token: &str, user_id: Uuid, ttl: Duration) {
        self.tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(token.to_string(), RegisteredToken { user_id, ttl });
    }

    pub(crate) fn token_registry(&self) -> TokenRegistry {
        self.tokens.clone()
    }

    /// Attempts a routed `ClientService.ListAgents` call from the relay's
    /// own position into `host` (within `user_id`'s routing services). The
    /// relay forwards these peers' bytes, but it has no device identity and
    /// no trust entry, so the call must never complete.
    pub(crate) async fn try_call_into(&self, user_id: Uuid, host: HostId) -> anyhow::Result<()> {
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
        let mut client = wire::client_service_client::ClientServiceClient::new(channel);
        client.list_agents(wire::ListAgentsRequest {}).await?;
        Ok(())
    }

    /// Takes the relay down hard: stops accepting and severs every accepted
    /// socket, so daemons observe a genuine outage (links fail, routes drop).
    pub(crate) async fn go_offline(&self) {
        if let Some(running) = self.server.lock().await.take() {
            running.sever();
        }
    }

    /// Restarts the relay on the same address so daemons can reconnect.
    pub(crate) async fn go_online(&self) {
        if self.server.lock().await.is_some() {
            return;
        }
        let listener = bind_addr_with_retries(self.addr).await;
        self.serve(listener).await;
    }

    pub(crate) async fn is_online(&self) -> bool {
        self.server.lock().await.is_some()
    }
}

/// Accepts TCP connections like the production relay, but keeps an OS-level
/// duplicate handle to each socket so [`RunningCloud::sever`] can cut them.
fn tracked_tcp_incoming(
    listener: TcpListener,
    connections: TrackedConnections,
) -> impl Stream<Item = std::io::Result<TcpServerTransport<TcpStream>>> + Send + 'static {
    stream::unfold((listener, connections), |(listener, connections)| async {
        let item = accept_tracked(&listener, &connections).await;
        Some((item, (listener, connections)))
    })
}

async fn accept_tracked(
    listener: &TcpListener,
    connections: &TrackedConnections,
) -> std::io::Result<TcpServerTransport<TcpStream>> {
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
    Ok(TcpServerTransport::new(TcpStream::from_std(std_stream)?))
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

/// Minimal `ServerState` for an in-process testnet node, plus the
/// shutdown-request receiver the `ClientService.Shutdown`/`Suspend` RPCs feed.
/// Cloud relays drop the receiver (they never shut down via RPC); daemons keep
/// it and wire a handler (see `start_daemon_runtime`).
///
/// `tcp_port` / `enable_cloud_mode` mirror what a configured daemon would
/// hold so config-gated surfaces (e.g. `StartPairing`'s QR mode requiring
/// cloud mode) behave like production.
pub(crate) fn testnet_server_state(
    host_name: &str,
    host_id: HostId,
    tcp_port: Option<u16>,
    enable_cloud_mode: bool,
) -> (
    std::sync::Arc<RwLock<ServerState>>,
    mpsc::Receiver<ShutdownRequest>,
) {
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
    let config = Config {
        host_name: host_name.to_string(),
        tcp_port,
        enable_cloud_mode: enable_cloud_mode.then_some(true),
        ..Config::default()
    };
    let state = std::sync::Arc::new(RwLock::new(ServerState::new(
        config,
        host_id,
        shutdown_tx,
        None,
        None,
    )));
    (state, shutdown_rx)
}

#[derive(Clone)]
struct RegistryTokenAuthenticator {
    tokens: TokenRegistry,
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
        Ok(AuthenticatedLinkUser {
            user_id: registered.user_id,
            client_id: "test-client".to_string(),
            expires_at: SystemTime::now() + registered.ttl,
        })
    }
}
