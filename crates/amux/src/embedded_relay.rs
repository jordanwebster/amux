//! A resolved relay for embedders whose account API lives outside this crate.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tonic::transport::Channel;

use crate::routing::{
    LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRole,
    spawn_connector_to_channel_with_auth_and_establishment,
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
                tokio::time::sleep(backoff).await;
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
        let (task, established) = spawn_connector_to_channel_with_auth_and_establishment(
            context.with_link_role(LinkRole::CloudRelay),
            channel,
            LinkConnectorAuth::new(token, credentials),
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
