//! Tonic channels carried by independent native link streams.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use rustls::pki_types::ServerName;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Endpoint};
use wire::pb;

use super::{ByteStream, OpenError};
use crate::dispatcher::TunnelDispatcher;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::routing::{LinkId, LinkRegistry, Route};
use crate::transport::{channel_from_single_io, configure_tonic_endpoint_keepalive};
use crate::trust::SharedTrustStore;
use crate::{AgentId, HostId};

const CHANNEL_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
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
pub enum ChannelError {
    #[error("host {host_id} is not reachable")]
    NoRoute { host_id: HostId },
    #[error(
        "Pairing could not reach this host. Check that both devices are online and signed in to the same cloud account."
    )]
    CloudPairingUnavailable,
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
pub struct ChannelDebug {
    pub peer: HostId,
    pub route: String,
    pub class: ChannelClass,
}

#[derive(Clone)]
struct ChannelSecurity {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
}

pub struct ChannelPool {
    by_key: RwLock<HashMap<ChannelKey, Channel>>,
    lifetimes: RwLock<HashMap<ChannelKey, Vec<Weak<CancellationToken>>>>,
    links: Arc<LinkRegistry>,
    security: Option<ChannelSecurity>,
    handshake_timeout: Duration,
}

impl ChannelPool {
    pub(crate) fn new(links: Arc<LinkRegistry>) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            lifetimes: RwLock::new(HashMap::new()),
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
            lifetimes: RwLock::new(HashMap::new()),
            links,
            security: Some(ChannelSecurity {
                identity,
                trust_store,
            }),
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
        }
    }

    pub fn link_registry(&self) -> Arc<LinkRegistry> {
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
        let channel = self.secure_channel(key, stream).await?;
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
        self.cancel_where(|key| route_link(key.route) == Some(link));
    }

    pub(crate) fn drop_host(&self, host: HostId) {
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| key.peer != host && route_link_peer(key.route) != host);
        self.cancel_where(|key| key.peer == host || route_link_peer(key.route) == host);
    }

    pub(crate) fn drop_route(&self, peer: HostId, route: Route) {
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| key.peer != peer || key.route != route);
        self.cancel_where(|key| key.peer == peer && key.route == route);
    }

    pub fn debug_view(&self) -> Vec<ChannelDebug> {
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

    pub fn active_session_agents(&self, peer: HostId) -> Vec<AgentId> {
        let lifetimes = self
            .lifetimes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut agents = lifetimes
            .iter()
            .filter_map(|(key, streams)| match key.class {
                ChannelClass::Session { agent }
                    if key.peer == peer
                        && streams.iter().any(|stream| stream.strong_count() > 0) =>
                {
                    Some(agent)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        agents.sort_unstable();
        agents
    }

    pub fn active_bulk_streams(&self, peer: HostId) -> usize {
        let lifetimes = self
            .lifetimes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lifetimes
            .iter()
            .filter(|(key, _)| key.peer == peer && key.class == ChannelClass::Bulk)
            .map(|(_, streams)| {
                streams
                    .iter()
                    .filter(|stream| stream.strong_count() > 0)
                    .count()
            })
            .sum()
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
        key: ChannelKey,
        stream: ByteStream,
    ) -> Result<Channel, ChannelError> {
        let security = self
            .security
            .as_ref()
            .ok_or_else(|| ChannelError::Handshake("device identity is unavailable".to_string()))?;
        let config = security
            .identity
            .client_tls_config_for_peer(security.trust_store.clone(), key.peer)?;
        let connector = TlsConnector::from(Arc::new(config));
        let server_name = ServerName::try_from("amux-device".to_string())
            .map_err(|error| ChannelError::Tls(error.to_string()))?;
        let lifetime = Arc::new(CancellationToken::new());
        {
            let mut lifetimes = self
                .lifetimes
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tracked = lifetimes.entry(key).or_default();
            tracked.retain(|lifetime| lifetime.strong_count() > 0);
            tracked.push(Arc::downgrade(&lifetime));
        }
        let stream = crate::transport::ShutdownIo::new_shared(stream, lifetime);
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

    fn cancel_where(&self, predicate: impl Fn(&ChannelKey) -> bool) {
        let mut lifetimes = self
            .lifetimes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys = lifetimes
            .keys()
            .copied()
            .filter(|key| predicate(key))
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(tokens) = lifetimes.remove(&key) {
                for token in tokens {
                    if let Some(token) = token.upgrade() {
                        token.cancel();
                    }
                }
            }
        }
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
