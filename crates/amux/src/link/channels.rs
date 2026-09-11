//! Tonic channels carried by independent native link streams.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tonic::transport::{Channel, Endpoint};

use super::{ByteStream, OpenError};
use crate::dispatcher::TunnelDispatcher;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::protocol::wire::pb;
use crate::routing::{LinkId, LinkRegistry, Route};
use crate::transport::{channel_from_single_io, configure_tonic_endpoint_keepalive};
use crate::trust::SharedTrustStore;
use crate::{AgentId, HostId};

const CHANNEL_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelClass {
    Calls,
    Session { agent: AgentId },
    Bulk,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) struct ChannelKey {
    pub(crate) peer: HostId,
    pub(crate) route: Route,
    pub(crate) class: ChannelClass,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChannelError {
    #[error("host {host_id} is not reachable")]
    NoRoute { host_id: HostId },
    #[error("stream refused: {0:?}")]
    Refused(pb::StreamRefusal),
    #[error("no live link to host {host_id}")]
    LinkUnavailable { host_id: HostId },
    #[error("channel handshake failed: {0}")]
    Handshake(String),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("channel TLS failed: {0}")]
    Tls(String),
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChannelDebug {
    pub(crate) peer: HostId,
    pub(crate) route: String,
    pub(crate) class: ChannelClass,
}

#[derive(Clone)]
struct ChannelSecurity {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
}

pub(crate) struct ChannelPool {
    by_key: RwLock<HashMap<ChannelKey, Channel>>,
    links: Arc<LinkRegistry>,
    security: Option<ChannelSecurity>,
    handshake_timeout: Duration,
}

impl ChannelPool {
    pub(crate) fn new(links: Arc<LinkRegistry>) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            links,
            security: None,
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
        }
    }

    pub(crate) fn with_device_tls(
        links: Arc<LinkRegistry>,
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
    ) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            links,
            security: Some(ChannelSecurity {
                identity,
                trust_store,
            }),
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    pub(crate) fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    pub(crate) async fn channel(&self, key: ChannelKey) -> Result<Channel, ChannelError> {
        let cached = if key.class == ChannelClass::Calls {
            self.by_key
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&key)
                .cloned()
        } else {
            None
        };
        if let Some(channel) = cached {
            if self.route_is_live(key.route).await {
                return Ok(channel);
            }
            return Err(ChannelError::LinkUnavailable {
                host_id: route_link_peer(key.route),
            });
        }

        let stream = self.open_stream(key.peer, key.route).await?;
        let channel = self.secure_channel(key.peer, stream).await?;
        if key.class == ChannelClass::Calls {
            self.by_key
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(key, channel.clone());
        }
        Ok(channel)
    }

    pub(crate) async fn pairing_stream(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<ByteStream, ChannelError> {
        self.open_stream(peer, route).await
    }

    pub(crate) fn drop_link(&self, link: LinkId) {
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| route_link(key.route) != Some(link));
    }

    pub(crate) fn drop_host(&self, host: HostId) {
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| key.peer != host && route_link_peer(key.route) != host);
    }

    pub(crate) fn debug_view(&self) -> Vec<ChannelDebug> {
        let state = self
            .by_key
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut channels = state
            .keys()
            .map(|key| ChannelDebug {
                peer: key.peer,
                route: key.route.to_string(),
                class: key.class,
            })
            .collect::<Vec<_>>();
        channels.sort_unstable_by_key(|channel| {
            (
                channel.peer,
                channel.route.clone(),
                format!("{:?}", channel.class),
            )
        });
        channels
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_key
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    async fn open_stream(&self, peer: HostId, route: Route) -> Result<ByteStream, ChannelError> {
        let carrier = match route {
            Route::Direct(link) => {
                self.links
                    .native_carrier(&link)
                    .await
                    .ok_or(ChannelError::LinkUnavailable {
                        host_id: link.peer(),
                    })?
            }
            Route::Via(relay) => self
                .links
                .native_carrier_to_peer(relay)
                .await
                .map(|(_, carrier)| carrier)
                .ok_or(ChannelError::LinkUnavailable { host_id: relay })?,
        };
        carrier
            .open_stream(pb::StreamPreface {
                dst: peer.as_bytes().to_vec(),
            })
            .await
            .map_err(|error| map_open_error(error, route))
    }

    async fn secure_channel(
        &self,
        peer: HostId,
        stream: ByteStream,
    ) -> Result<Channel, ChannelError> {
        let security = self
            .security
            .as_ref()
            .ok_or_else(|| ChannelError::Handshake("device identity is unavailable".to_string()))?;
        let config = security
            .identity
            .client_tls_config_for_peer(security.trust_store.clone(), peer)?;
        let connector = TlsConnector::from(Arc::new(config));
        let server_name = ServerName::try_from("amux-device".to_string())
            .map_err(|error| ChannelError::Tls(error.to_string()))?;
        let tls = tokio::time::timeout(
            self.handshake_timeout,
            connector.connect(server_name, stream),
        )
        .await
        .map_err(|_| ChannelError::Handshake("TLS handshake timed out".to_string()))?
        .map_err(|error| ChannelError::Tls(error.to_string()))?;
        Ok(channel_from_single_io(
            configure_tonic_endpoint_keepalive(Endpoint::from_static("https://peer")),
            "native link channel",
            tls,
        ))
    }

    async fn route_is_live(&self, route: Route) -> bool {
        match route {
            Route::Direct(link) => self.links.native_carrier(&link).await.is_some(),
            Route::Via(relay) => self.links.native_carrier_to_peer(relay).await.is_some(),
        }
    }
}

pub(crate) fn serve_inbound_streams(
    dispatcher: Arc<TunnelDispatcher>,
    mut streams: mpsc::Receiver<(HostId, ByteStream)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((adjacent_peer, stream)) = streams.recv().await {
            let dispatcher = dispatcher.clone();
            tokio::spawn(async move {
                if let Err(error) = dispatcher.dispatch_link_stream(adjacent_peer, stream).await {
                    tracing::warn!(%error, "dispatcher rejected native link stream");
                }
            });
        }
    })
}

fn map_open_error(error: OpenError, route: Route) -> ChannelError {
    match error {
        OpenError::Refused(reason) => ChannelError::Refused(reason),
        OpenError::LinkClosed => ChannelError::LinkUnavailable {
            host_id: route_link_peer(route),
        },
        OpenError::Io(error) => ChannelError::Handshake(error.to_string()),
    }
}

fn route_link(route: Route) -> Option<LinkId> {
    match route {
        Route::Direct(link) => Some(link),
        Route::Via(_) => None,
    }
}

pub(crate) fn route_link_peer(route: Route) -> HostId {
    match route {
        Route::Direct(link) => link.peer(),
        Route::Via(relay) => relay,
    }
}
