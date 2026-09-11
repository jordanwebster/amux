//! Runtime Link establishment from discovery and persisted reachabilities.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::HostId;
use crate::connection::ConnectionManager;
use crate::discovery::{Discovery, DiscoveryEvent, FoundHosts};
use crate::dispatcher::TrackedTcpConnections;
use crate::identity::DeviceIdentity;
use crate::link::{
    CarrierKind, ChannelPool, LinkCarrier as NativeLinkCarrier, MuxCarrier, MuxRole,
};
use crate::routing::{
    Host, LinkCarrier, LinkConnectorCtx, LiveLocalHost, Route, RoutingCore,
    spawn_connector_with_establishment,
};
use crate::transport::{spawn_ssh_relay, trusted_device_stream_tracked};
use crate::trust::{Reachability, SharedTrustStore};

const DIRECT_LINK_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct ReachabilityLinkConnector {
    mode: ReachabilityLinkConnectorMode,
}

#[derive(Clone)]
enum ReachabilityLinkConnectorMode {
    Enabled(Arc<ReachabilityLinkConnectorInner>),
    #[allow(dead_code)]
    Disabled,
}

struct ReachabilityLinkConnectorInner {
    context: ReachabilityLinkContext,
    retained_tasks: Mutex<Vec<JoinHandle<()>>>,
    dialing: Arc<Mutex<HashSet<HostId>>>,
    queued_found: Arc<Mutex<HashSet<HostId>>>,
    direct_enabled: Arc<AtomicBool>,
}

#[derive(Clone)]
struct ReachabilityLinkContext {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    local_host: LiveLocalHost,
    routing: Arc<RoutingCore>,
    channels: Arc<ChannelPool>,
    connections: Arc<ConnectionManager>,
    incoming_streams_tx: tokio::sync::mpsc::Sender<(HostId, crate::link::ByteStream)>,
    runtime: Arc<Mutex<Option<ReachabilityRuntime>>>,
    dialed_tcp_tracker: Arc<Mutex<Option<TrackedTcpConnections>>>,
}

#[derive(Clone)]
struct ReachabilityRuntime {
    data_dir: PathBuf,
    discovery: Arc<dyn Discovery>,
    found_hosts: Arc<FoundHosts>,
}

#[derive(Clone)]
struct ReachabilityLinkAttempt {
    peer: HostId,
    reachability: Reachability,
    ordinal: usize,
}

impl ReachabilityLinkConnector {
    pub(crate) fn new(
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
        local_host: LiveLocalHost,
        routing: Arc<RoutingCore>,
        channels: Arc<ChannelPool>,
        connections: Arc<ConnectionManager>,
        incoming_streams_tx: tokio::sync::mpsc::Sender<(HostId, crate::link::ByteStream)>,
    ) -> Self {
        Self {
            mode: ReachabilityLinkConnectorMode::Enabled(Arc::new(
                ReachabilityLinkConnectorInner {
                    context: ReachabilityLinkContext {
                        identity,
                        trust_store,
                        local_host,
                        routing,
                        channels,
                        connections,
                        incoming_streams_tx,
                        runtime: Arc::new(Mutex::new(None)),
                        dialed_tcp_tracker: Arc::new(Mutex::new(None)),
                    },
                    retained_tasks: Mutex::new(Vec::new()),
                    dialing: Arc::new(Mutex::new(HashSet::new())),
                    queued_found: Arc::new(Mutex::new(HashSet::new())),
                    direct_enabled: Arc::new(AtomicBool::new(true)),
                },
            )),
        }
    }

    pub(crate) fn configure(
        &self,
        data_dir: PathBuf,
        discovery: Arc<dyn Discovery>,
        found_hosts: Arc<FoundHosts>,
    ) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        *inner.context.runtime.lock().unwrap() = Some(ReachabilityRuntime {
            data_dir,
            discovery,
            found_hosts,
        });
    }

    #[cfg(testnet)]
    pub(crate) fn track_dialed_tcp(&self, tracker: TrackedTcpConnections) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        if let Ok(mut slot) = inner.context.dialed_tcp_tracker.lock() {
            *slot = Some(tracker);
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            mode: ReachabilityLinkConnectorMode::Disabled,
        }
    }

    pub(crate) fn spawn_startup_links(&self) -> Vec<JoinHandle<()>> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return Vec::new();
        };
        let attempts = match snapshot_attempts(&inner.context) {
            Ok(attempts) => attempts,
            Err(()) => {
                tracing::warn!("failed to read trust store for reachability Link startup");
                return Vec::new();
            }
        };
        attempts
            .into_iter()
            .filter_map(|attempt| self.spawn_attempt(attempt))
            .collect()
    }

    pub(crate) fn spawn_dial_on_found(
        &self,
        mut events: broadcast::Receiver<DiscoveryEvent>,
    ) -> JoinHandle<()> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return tokio::spawn(async {});
        };
        let connector = self.clone();
        let context = inner.context.clone();
        let enabled = inner.direct_enabled.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let found = match &event {
                            DiscoveryEvent::Found(advert) => Some(advert.clone()),
                            DiscoveryEvent::Lost { .. } => None,
                        };
                        if let Some(runtime) = context.runtime.lock().unwrap().clone() {
                            runtime.found_hosts.apply(event);
                        }
                        let Some(advert) = found else { continue };
                        if !enabled.load(Ordering::SeqCst)
                            || advert.host_id == context.identity.host_id
                            || has_direct_route(&context, advert.host_id).await
                            || !is_trusted(&context.trust_store, advert.host_id)
                        {
                            continue;
                        }
                        connector.dial_found(advert.host_id);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(runtime) = context.runtime.lock().unwrap().clone() {
                            runtime.discovery.requery();
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    pub(crate) fn spawn_pair_time_link(&self, peer: HostId, reachability: Reachability) {
        if matches!(reachability, Reachability::Cloud) {
            return;
        }
        let Some(task) = self.spawn_attempt(ReachabilityLinkAttempt {
            peer,
            reachability,
            ordinal: 0,
        }) else {
            return;
        };
        self.retain_task(task);
    }

    fn dial_found(&self, peer: HostId) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        let addrs = direct_candidates(&inner.context, peer);
        if let Some(task) = self.spawn_attempt(ReachabilityLinkAttempt {
            peer,
            reachability: Reachability::Direct { addrs },
            ordinal: 0,
        }) {
            self.retain_task(task);
            return;
        }
        if !inner.queued_found.lock().unwrap().insert(peer) {
            return;
        }

        // A stored-address attempt may already be in flight when discovery
        // resolves the peer. Wait for that attempt to settle, then give the
        // freshly found addresses their own chance instead of losing Found.
        let connector = self.clone();
        let context = inner.context.clone();
        let dialing = inner.dialing.clone();
        let queued_found = inner.queued_found.clone();
        let enabled = inner.direct_enabled.clone();
        let task = tokio::spawn(async move {
            loop {
                if !enabled.load(Ordering::SeqCst) || has_direct_route(&context, peer).await {
                    queued_found.lock().unwrap().remove(&peer);
                    return;
                }
                if !dialing.lock().unwrap().contains(&peer) {
                    queued_found.lock().unwrap().remove(&peer);
                    connector.dial_found(peer);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        self.retain_task(task);
    }

    pub(crate) fn requery(&self) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        if let Some(runtime) = inner.context.runtime.lock().unwrap().clone() {
            runtime.discovery.requery();
        }
    }

    pub(crate) fn found_candidates(&self) -> Vec<crate::discovery::Advertisement> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return Vec::new();
        };
        inner
            .context
            .runtime
            .lock()
            .unwrap()
            .as_ref()
            .map(|runtime| runtime.found_hosts.candidates())
            .unwrap_or_default()
    }

    pub(crate) fn found_addrs(&self, peer: HostId) -> Vec<std::net::SocketAddr> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return Vec::new();
        };
        inner
            .context
            .runtime
            .lock()
            .unwrap()
            .as_ref()
            .map(|runtime| runtime.found_hosts.addrs_for(peer))
            .unwrap_or_default()
    }

    pub(crate) async fn close_direct_links(&self) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        inner.direct_enabled.store(false, Ordering::SeqCst);
        inner
            .context
            .channels
            .link_registry()
            .close_peer_links()
            .await;
    }

    pub(crate) fn resume_direct_links(&self) -> Vec<JoinHandle<()>> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return Vec::new();
        };
        inner.direct_enabled.store(true, Ordering::SeqCst);
        let tasks = self.spawn_startup_links();
        self.requery();
        tasks
    }

    fn spawn_attempt(&self, attempt: ReachabilityLinkAttempt) -> Option<JoinHandle<()>> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return None;
        };
        let direct = matches!(attempt.reachability, Reachability::Direct { .. });
        if direct {
            if !inner.direct_enabled.load(Ordering::SeqCst) {
                return None;
            }
            let mut dialing = inner.dialing.lock().unwrap();
            if !dialing.insert(attempt.peer) {
                return None;
            }
        }
        let context = inner.context.clone();
        let dialing = inner.dialing.clone();
        let span = tracing::info_span!(
            "reachability_link",
            peer = %attempt.peer,
            reachability = ?attempt.reachability,
            ordinal = attempt.ordinal,
        );
        Some(tokio::spawn(
            async move {
                let peer = attempt.peer;
                let established_direct =
                    establish_reachability_link(context.clone(), attempt).await;
                if direct {
                    dialing.lock().unwrap().remove(&peer);
                    if established_direct
                        && let Some(runtime) = context.runtime.lock().unwrap().clone()
                    {
                        runtime.discovery.requery();
                    }
                }
            }
            .instrument(span),
        ))
    }

    fn retain_task(&self, task: JoinHandle<()>) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            task.abort();
            return;
        };
        let Ok(mut tasks) = inner.retained_tasks.lock() else {
            task.abort();
            tracing::warn!("failed to retain reachability Link task");
            return;
        };
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
}

impl Drop for ReachabilityLinkConnectorInner {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.retained_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

fn snapshot_attempts(
    context: &ReachabilityLinkContext,
) -> Result<Vec<ReachabilityLinkAttempt>, ()> {
    let store = context.trust_store.read().map_err(|_| ())?;
    let mut attempts = Vec::new();
    for (peer, entry) in store.entries() {
        for (ordinal, reachability) in entry
            .reachabilities
            .iter()
            .filter(|reachability| !matches!(reachability, Reachability::Cloud))
            .cloned()
            .enumerate()
        {
            let reachability = match reachability {
                Reachability::Direct { .. } => Reachability::Direct {
                    addrs: direct_candidates(context, peer),
                },
                other => other,
            };
            attempts.push(ReachabilityLinkAttempt {
                peer,
                reachability,
                ordinal,
            });
        }
    }
    Ok(attempts)
}

fn direct_candidates(context: &ReachabilityLinkContext, peer: HostId) -> Vec<std::net::SocketAddr> {
    let mut addrs = context
        .runtime
        .lock()
        .unwrap()
        .as_ref()
        .map(|runtime| runtime.found_hosts.addrs_for(peer))
        .unwrap_or_default();
    if let Ok(store) = context.trust_store.read()
        && let Some(entry) = store
            .entries()
            .find_map(|(id, entry)| (id == peer).then_some(entry))
    {
        for addr in entry
            .reachabilities
            .iter()
            .flat_map(|reachability| match reachability {
                Reachability::Direct { addrs } => addrs.as_slice(),
                _ => &[],
            })
        {
            if !addrs.contains(addr) {
                addrs.push(*addr);
            }
        }
    }
    addrs
}

fn is_trusted(trust_store: &SharedTrustStore, peer: HostId) -> bool {
    trust_store
        .read()
        .map(|store| store.pubkey_for_host(peer).is_some())
        .unwrap_or(false)
}

async fn has_direct_route(context: &ReachabilityLinkContext, peer: HostId) -> bool {
    context
        .routing
        .routes_to(peer)
        .await
        .iter()
        .any(|route| matches!(route, Route::Direct(_)))
}

async fn establish_reachability_link(
    context: ReachabilityLinkContext,
    attempt: ReachabilityLinkAttempt,
) -> bool {
    match attempt.reachability.clone() {
        Reachability::Cloud => false,
        Reachability::Direct { addrs } => {
            let mut last_error = None;
            for addr in addrs {
                match prepare_direct_stream(&context, attempt.peer, addr).await {
                    Ok(stream) => match establish_carrier(
                        &context,
                        attempt.peer,
                        Arc::new(MuxCarrier::new(
                            stream,
                            MuxRole::Connector,
                            CarrierKind::Quic,
                        )),
                        LinkCarrier::Direct,
                    )
                    .await
                    {
                        Ok((host, connector_task, abort_on_drop)) => {
                            persist_working_addr(&context, attempt.peer, addr);
                            context
                                .connections
                                .clear_reachability_error(attempt.peer)
                                .await;
                            tracing::info!(peer_name = %host.name, %addr, "direct Link established");
                            await_connector(connector_task, abort_on_drop).await;
                            return true;
                        }
                        Err(error) => last_error = Some(error),
                    },
                    Err(error) => last_error = Some(error),
                }
            }
            let error = last_error.unwrap_or_else(|| "no direct addresses available".to_string());
            context
                .connections
                .record_reachability_error(attempt.peer, error.clone())
                .await;
            tracing::warn!(error = %error, "direct Link establishment failed");
            false
        }
        Reachability::Ssh { target, profile } => {
            let carrier = match spawn_ssh_relay(&target, profile).map_err(|error| error.to_string())
            {
                Ok(io) => Arc::new(MuxCarrier::new(io, MuxRole::Connector, CarrierKind::Ssh))
                    as Arc<dyn NativeLinkCarrier>,
                Err(error) => {
                    context
                        .connections
                        .record_reachability_error(attempt.peer, &error)
                        .await;
                    tracing::warn!(error = %error, "failed to prepare SSH Link transport");
                    return false;
                }
            };
            match establish_carrier(&context, attempt.peer, carrier, LinkCarrier::Ssh).await {
                Ok((host, connector_task, abort_on_drop)) => {
                    context
                        .connections
                        .clear_reachability_error(attempt.peer)
                        .await;
                    tracing::info!(peer_name = %host.name, "SSH Link established");
                    await_connector(connector_task, abort_on_drop).await;
                }
                Err(error) => {
                    context
                        .connections
                        .record_reachability_error(attempt.peer, &error)
                        .await;
                    tracing::warn!(error = %error, "SSH Link establishment failed");
                }
            }
            false
        }
    }
}

async fn prepare_direct_stream(
    context: &ReachabilityLinkContext,
    peer: HostId,
    addr: std::net::SocketAddr,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    let tracker = context
        .dialed_tcp_tracker
        .lock()
        .ok()
        .and_then(|tracker| tracker.clone());
    trusted_device_stream_tracked(
        addr,
        context.identity.clone(),
        context.trust_store.clone(),
        peer,
        tracker,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn establish_carrier(
    context: &ReachabilityLinkContext,
    peer: HostId,
    native_carrier: Arc<dyn NativeLinkCarrier>,
    carrier: LinkCarrier,
) -> Result<(Host, JoinHandle<Result<(), tonic::Status>>, AbortTaskOnDrop), String> {
    let connector_ctx = LinkConnectorCtx::new_live(
        context.local_host.clone(),
        context.routing.clone(),
        context.channels.link_registry(),
    )
    .with_expected_peer(peer)
    .with_carrier(carrier)
    .with_incoming_streams(context.incoming_streams_tx.clone());
    let (connector_task, established_rx) =
        spawn_connector_with_establishment(connector_ctx, native_carrier);
    let abort_on_failure = AbortTaskOnDrop(connector_task.abort_handle());
    let host = match tokio::time::timeout(DIRECT_LINK_ESTABLISHMENT_TIMEOUT, established_rx).await {
        Ok(Ok(Ok(host))) => host,
        Ok(Ok(Err(status))) => return Err(status.to_string()),
        Ok(Err(_)) => return Err("reachability Link task ended before establishment".to_string()),
        Err(_) => return Err("reachability Link establishment timed out".to_string()),
    };
    Ok((host, connector_task, abort_on_failure))
}

fn persist_working_addr(
    context: &ReachabilityLinkContext,
    peer: HostId,
    addr: std::net::SocketAddr,
) {
    let Some(runtime) = context.runtime.lock().unwrap().clone() else {
        return;
    };
    let Ok(mut store) = context.trust_store.write() else {
        tracing::warn!("failed to lock trust store after direct dial");
        return;
    };
    if store.replace_direct_addrs(peer, vec![addr])
        && let Err(error) = store.save_in(&runtime.data_dir)
    {
        tracing::warn!(error = %error, "failed to persist working direct address");
    }
}

async fn await_connector(
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    _abort_on_drop: AbortTaskOnDrop,
) {
    match connector_task.await {
        Ok(Ok(())) => tracing::info!("reachability Link closed cleanly"),
        Ok(Err(status)) => tracing::warn!(error = %status, "reachability Link closed with error"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(error = %error, "reachability Link task panicked"),
    }
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
