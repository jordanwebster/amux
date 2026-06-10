//! In-process cloud relay for testnet topologies.
//!
//! Mirrors the assembly used by the startup tests: a real
//! [`CloudRoutingService`] served over localhost TCP, with a static
//! bearer-token authenticator standing in for JWT validation. All daemons in
//! a `TestNet` share one cloud user, so the relay bridges them exactly like
//! production cloud routing does for one account.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::{Stream, stream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::HostId;
use crate::config::Config;
use crate::routing::{AuthenticatedRoutingUser, RoutingTokenAuthenticator};
use crate::services::CloudRoutingService;
use crate::transport::TcpServerTransport;
use crate::user_state::{ServerState, ShutdownRequest};

use super::assertions::POLL_INTERVAL;

/// OS-level handles to every TCP connection the relay has accepted, so an
/// outage can sever them for real (tonic's spawned connection tasks outlive
/// an aborted accept loop).
type TrackedConnections = Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>;

pub(crate) struct CloudRelay {
    pub(crate) addr: SocketAddr,
    pub(crate) host_id: HostId,
    pub(crate) token: String,
    user_id: Uuid,
    server: Mutex<Option<RunningCloud>>,
}

struct RunningCloud {
    _service: CloudRoutingService,
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
        let relay = Self {
            addr,
            host_id: Uuid::new_v4(),
            token: format!("spec-token-{}", Uuid::new_v4().simple()),
            user_id: Uuid::new_v4(),
            server: Mutex::new(None),
        };
        relay.serve(listener).await;
        relay
    }

    async fn serve(&self, listener: TcpListener) {
        let state = testnet_server_state("cloud", self.host_id, None, false);
        state.write().await.is_cloud_server = true;
        let service = CloudRoutingService::with_authenticator(
            state,
            Arc::new(StaticTokenAuthenticator {
                token: self.token.clone(),
                user_id: self.user_id,
            }),
        );
        let connections: TrackedConnections = Arc::default();
        let task = service.serve_on_incoming(tracked_tcp_incoming(listener, connections.clone()));
        *self.server.lock().await = Some(RunningCloud {
            _service: service,
            task,
            connections,
        });
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

/// Minimal `ServerState` for an in-process testnet node.
///
/// `tcp_port` / `enable_cloud_mode` mirror what a configured daemon would
/// hold so config-gated surfaces (e.g. `StartPairing`'s QR mode requiring
/// cloud mode) behave like production.
pub(crate) fn testnet_server_state(
    host_name: &str,
    host_id: HostId,
    tcp_port: Option<u16>,
    enable_cloud_mode: bool,
) -> std::sync::Arc<RwLock<ServerState>> {
    let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
    let config = Config {
        host_name: host_name.to_string(),
        tcp_port,
        enable_cloud_mode: enable_cloud_mode.then_some(true),
        ..Config::default()
    };
    std::sync::Arc::new(RwLock::new(ServerState::new(
        config,
        host_id,
        shutdown_tx,
        None,
        None,
    )))
}

#[derive(Clone)]
struct StaticTokenAuthenticator {
    token: String,
    user_id: Uuid,
}

#[tonic::async_trait]
impl RoutingTokenAuthenticator for StaticTokenAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedRoutingUser, tonic::Status> {
        if token != self.token {
            return Err(tonic::Status::unauthenticated("unknown testnet token"));
        }
        Ok(AuthenticatedRoutingUser {
            user_id: self.user_id,
            client_id: "test-client".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
        })
    }
}
