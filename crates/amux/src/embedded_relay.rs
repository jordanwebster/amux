//! A resolved relay for embedders whose account API lives outside this crate.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tonic::transport::Channel;

use crate::routing::{
    LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRole,
    spawn_connector_to_channel_with_auth_establishment_and_shutdown,
};
use crate::{CredentialProvider, ServerError};

/// Validated endpoint. Cleartext endpoints cannot be constructed in shipping builds.
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

    #[cfg(feature = "debug-tools")]
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

    fn channel(&self) -> Result<Channel, ServerError> {
        #[cfg(feature = "debug-tools")]
        if let Some(address) = self.plain {
            return Ok(
                tonic::transport::Endpoint::from_shared(format!("http://{address}"))
                    .map_err(|e| ServerError::State(e.to_string()))?
                    .connect_lazy(),
            );
        }
        debug_assert!(self.plain.is_none());
        Ok(crate::transport::tls_channel(self.host.clone(), self.port)?)
    }
}

/// Relay connectivity, independent of the in-process client-service connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayConnection {
    Connecting,
    Connected,
    Disconnected { reason: String },
}

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
#[derive(Default)]
pub struct RelayRetry {
    asked: tokio::sync::Notify,
    honoured: std::sync::Mutex<Option<tokio::time::Instant>>,
    attempts: std::sync::atomic::AtomicU64,
    shortened: std::sync::atomic::AtomicU64,
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
        })
    }
}

impl EmbeddedRelay {
    pub(crate) fn spawn(self, context: LinkConnectorCtx) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(250);
            loop {
                self.retry.attempted();
                let result = self.connect(context.clone()).await;
                let reason = match result {
                    Ok(()) => {
                        backoff = Duration::from_millis(250);
                        "relay closed".into()
                    }
                    Err(error) => error,
                };
                self.connection
                    .send_replace(RelayConnection::Disconnected { reason });
                self.retry.wait(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(4));
            }
        })
    }

    async fn connect(&self, context: LinkConnectorCtx) -> Result<(), String> {
        let credentials = Arc::new(RoutingCredentials(self.credentials.clone()));
        let token = credentials
            .refresh_routing_token()
            .await
            .map_err(|e| e.to_string())?;
        let channel = self.endpoint.channel().map_err(|e| e.to_string())?;
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let (task, established) = spawn_connector_to_channel_with_auth_establishment_and_shutdown(
            context.with_link_role(LinkRole::CloudRelay),
            channel,
            LinkConnectorAuth::new(token, credentials),
            shutdown_rx,
        );
        let _guard = AbortOnDrop(task.abort_handle());
        tokio::time::timeout(Duration::from_secs(10), established)
            .await
            .map_err(|_| "relay handshake timed out".to_string())?
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        self.connection.send_replace(RelayConnection::Connected);
        task.await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }
}
