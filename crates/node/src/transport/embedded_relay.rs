//! A resolved relay for embedders whose account API lives outside this crate.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub use model::{DisconnectReason, RelayConnection};
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::link::{CarrierKind, LinkCarrier, MuxCarrier, MuxRole};
use crate::routing::{
    LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRole,
    spawn_connector_with_auth_establishment_and_shutdown,
};
use crate::{CredentialProvider, ServerError};

/// Validated endpoint: an HTTPS origin, or a plaintext loopback address that
/// only a caller which has decided to allow one can construct.
#[derive(Clone)]
pub struct RelayEndpoint {
    host: String,
    port: u16,
    plain: Option<SocketAddr>,
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

    /// Opens the byte stream this endpoint names and wraps it as the link's
    /// carrier. Dialling happens here rather than lazily: the caller's retry
    /// loop reports what an unreachable relay did, and it can only do that if
    /// the failure arrives now.
    async fn carrier(&self) -> Result<Arc<dyn LinkCarrier>, ServerError> {
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

        if let Some(address) = self.plain {
            let stream = TcpStream::connect(address)
                .await
                .map_err(|e| ServerError::State(e.to_string()))?;
            stream
                .set_nodelay(true)
                .map_err(|e| ServerError::State(e.to_string()))?;
            super::configure_relay_tcp_keepalive(&stream);
            return Ok(relay_carrier(stream));
        }
        let stream = super::tls_connect_stream(&self.host, self.port)
            .await
            .map_err(|e| ServerError::State(e.to_string()))?;
        Ok(relay_carrier(stream))
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

struct RoutingCredentials(Arc<dyn CredentialProvider>);
#[async_trait::async_trait]
impl LinkConnectorTokenRefresher for RoutingCredentials {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        let token = self
            .0
            .access_token()
            .await
            .map_err(|e| tonic::Status::unauthenticated(e.to_string()))?;
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
    pub(crate) fn spawn(self, context: LinkConnectorCtx) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(250);
            loop {
                if self.retry.is_suspended() {
                    self.connection.send_replace(RelayConnection::Disconnected {
                        reason: DisconnectReason::Suspended,
                    });
                    self.retry.while_suspended().await;
                    // Back in front of somebody: dial now, not on the wait the
                    // last failure had chosen.
                    backoff = Duration::from_millis(250);
                    self.connection.send_replace(RelayConnection::Connecting);
                    continue;
                }
                self.retry.attempted();
                let result = self.connect(context.clone()).await;
                let reason = match result {
                    Ok(()) => {
                        backoff = Duration::from_millis(250);
                        DisconnectReason::Ended
                    }
                    Err(reason) => reason,
                };
                self.connection
                    .send_replace(RelayConnection::Disconnected { reason });
                if reason == DisconnectReason::Suspended {
                    continue;
                }
                self.retry.wait(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(4));
            }
        })
    }

    async fn connect(&self, context: LinkConnectorCtx) -> Result<(), DisconnectReason> {
        let credentials = Arc::new(RoutingCredentials(self.credentials.clone()));
        let token = tokio::select! {
            token = credentials.refresh_routing_token() => token.map_err(|e| {
                tracing::debug!(error = %e, "relay credentials refused");
                disconnect_reason(&e)
            })?,
            () = self.retry.until_suspended() => return Err(DisconnectReason::Suspended),
        };
        let carrier = self.endpoint.carrier().await.map_err(|e| {
            tracing::debug!(error = %e, "relay endpoint unusable");
            DisconnectReason::Unreachable
        })?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (mut task, established) = spawn_connector_with_auth_establishment_and_shutdown(
            context.with_link_role(LinkRole::CloudRelay),
            carrier,
            LinkConnectorAuth::new(token, credentials),
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
        self.connection.send_replace(RelayConnection::Connected);
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
