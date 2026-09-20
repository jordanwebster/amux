//! Tonic channels carried by independent native link streams.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, RwLock, Weak};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use rustls::pki_types::ServerName;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
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
const HTTP2_FRAME_HEADER_LEN: usize = 9;
const HTTP2_DATA_FRAME: u8 = 0;

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
    bulk_response_holds: std::sync::Mutex<HashMap<HostId, PendingBulkResponseHold>>,
}

pub struct BulkResponseHold {
    entered: oneshot::Receiver<()>,
    release: Option<oneshot::Sender<()>>,
}

impl BulkResponseHold {
    pub async fn entered(&mut self) -> Result<(), oneshot::error::RecvError> {
        (&mut self.entered).await
    }

    pub fn release(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl Drop for BulkResponseHold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

struct PendingBulkResponseHold {
    entered: Option<oneshot::Sender<()>>,
    release: oneshot::Receiver<()>,
}

impl PendingBulkResponseHold {
    fn has_entered(&self) -> bool {
        self.entered.is_none()
    }

    fn enter(&mut self) {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
    }

    fn poll_release(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.release).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl ChannelPool {
    pub(crate) fn new(links: Arc<LinkRegistry>) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            lifetimes: RwLock::new(HashMap::new()),
            links,
            security: None,
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
            bulk_response_holds: std::sync::Mutex::new(HashMap::new()),
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
            bulk_response_holds: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn hold_next_bulk_response_for_test(&self, peer: HostId) -> BulkResponseHold {
        let (entered_tx, entered) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let previous = self
            .bulk_response_holds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                peer,
                PendingBulkResponseHold {
                    entered: Some(entered_tx),
                    release: release_rx,
                },
            );
        assert!(previous.is_none(), "a bulk stream hold is already armed");
        BulkResponseHold {
            entered,
            release: Some(release),
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
        let endpoint = configure_tonic_endpoint_keepalive(Endpoint::from_static("https://peer"));
        let hold = (key.class == ChannelClass::Bulk).then(|| {
            self.bulk_response_holds
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&key.peer)
        });
        Ok(match hold.flatten() {
            Some(hold) => channel_from_single_io(
                endpoint,
                "held native link channel",
                HoldAfterFirstDataFrame::new(tls, hold),
            ),
            None => channel_from_single_io(endpoint, "native link channel", tls),
        })
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

enum Http2ReadState {
    Header {
        bytes: [u8; HTTP2_FRAME_HEADER_LEN],
        filled: usize,
    },
    Payload {
        frame_type: u8,
        remaining: usize,
    },
}

struct HoldAfterFirstDataFrame<T> {
    inner: T,
    hold: Option<PendingBulkResponseHold>,
    state: Http2ReadState,
}

impl<T> HoldAfterFirstDataFrame<T> {
    fn new(inner: T, hold: PendingBulkResponseHold) -> Self {
        Self {
            inner,
            hold: Some(hold),
            state: Http2ReadState::Header {
                bytes: [0; HTTP2_FRAME_HEADER_LEN],
                filled: 0,
            },
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for HoldAfterFirstDataFrame<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.hold.as_ref().is_some_and(|hold| hold.has_entered()) {
            ready!(this.hold.as_mut().expect("hold exists").poll_release(cx));
            this.hold = None;
        }
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let boundary = match &this.state {
            Http2ReadState::Header { filled, .. } => HTTP2_FRAME_HEADER_LEN - filled,
            Http2ReadState::Payload { remaining, .. } => *remaining,
        };
        let before = output.filled().len();
        let (poll, initialized, read) = {
            let mut limited = output.take(boundary);
            let poll = Pin::new(&mut this.inner).poll_read(cx, &mut limited);
            (poll, limited.initialized().len(), limited.filled().len())
        };
        // SAFETY: `limited` covered exactly this prefix of `output`'s
        // unfilled storage and reported it initialized by the inner reader.
        unsafe { output.assume_init(initialized) };
        output.advance(read);
        match poll {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if read == 0 {
            return Poll::Ready(Ok(()));
        }
        let bytes = &output.filled()[before..before + read];
        match &mut this.state {
            Http2ReadState::Header {
                bytes: header,
                filled,
            } => {
                header[*filled..*filled + read].copy_from_slice(bytes);
                *filled += read;
                if *filled == HTTP2_FRAME_HEADER_LEN {
                    let length = usize::from(header[0]) << 16
                        | usize::from(header[1]) << 8
                        | usize::from(header[2]);
                    if length == 0 {
                        this.state = Http2ReadState::Header {
                            bytes: [0; HTTP2_FRAME_HEADER_LEN],
                            filled: 0,
                        };
                    } else {
                        this.state = Http2ReadState::Payload {
                            frame_type: header[3],
                            remaining: length,
                        };
                    }
                }
            }
            Http2ReadState::Payload {
                frame_type,
                remaining,
            } => {
                *remaining -= read;
                if *remaining == 0 {
                    let is_data = *frame_type == HTTP2_DATA_FRAME;
                    this.state = Http2ReadState::Header {
                        bytes: [0; HTTP2_FRAME_HEADER_LEN],
                        filled: 0,
                    };
                    if is_data && let Some(hold) = &mut this.hold {
                        hold.enter();
                    }
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for HoldAfterFirstDataFrame<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
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
                    if error.is_peer_leaving() {
                        // The peer opened this and left before it said who it
                        // was, which is what a stream on a superseded link
                        // does. Nothing here was refused.
                        tracing::debug!(%error, "a peer left a native link stream unfinished");
                    } else {
                        tracing::warn!(%error, "dispatcher rejected native link stream");
                    }
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
