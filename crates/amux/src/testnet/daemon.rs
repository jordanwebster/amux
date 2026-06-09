//! A whole in-process daemon and its observation/assertion surface.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tonic::transport::Endpoint;

use crate::HostId;
use crate::connection::ConnectionManager;
use crate::identity::load_or_create_device_identity_in;
use crate::protocol::wire;
use crate::routing::{
    HostEntry, Route, RoutingCore, generate_server_link,
    spawn_connector_to_channel_with_bearer_token,
};
use crate::services::{
    AgentServiceState, ClientService, DeviceRuntimeSecurity, StartedUserServices,
    start_user_services,
};
use crate::trust::{Reachability, SharedTrustStore, TrustStore};
use crate::tunnel::TunnelPool;

use super::NetInner;
use super::assertions::eventually;
use super::net::{bind_addr_with_retries, testnet_server_state};

/// Parameters a daemon needs to (re)connect to the testnet cloud relay.
pub(crate) struct CloudAttachment {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
}

pub(crate) struct DaemonInner {
    pub(crate) name: String,
    pub(crate) host_id: HostId,
    pub(crate) data_dir: PathBuf,
    /// Direct-TCP listener address; stable across restarts so stored
    /// reachabilities keep working. `None` for cloud-only daemons.
    pub(crate) tcp_addr: Option<SocketAddr>,
    pub(crate) cloud: Option<CloudAttachment>,
    pub(crate) runtime: Mutex<Option<DaemonRuntime>>,
}

pub(crate) struct DaemonRuntime {
    pub(crate) services: StartedUserServices,
    pub(crate) trust: SharedTrustStore,
    reachability_tasks: Vec<JoinHandle<()>>,
    cloud_task: Option<JoinHandle<Result<(), tonic::Status>>>,
}

impl DaemonRuntime {
    pub(crate) fn spawn_cloud_connector(&mut self, name: &str, cloud: &CloudAttachment) {
        if let Some(task) = self.cloud_task.take() {
            task.abort();
        }
        let channel = Endpoint::from_shared(format!("http://{}", cloud.addr))
            .expect("testnet cloud endpoint URI")
            .connect_lazy();
        let ctx = self
            .services
            .routing_connector_ctx(generate_server_link(&format!("cloud-{name}"), false));
        self.cloud_task = Some(spawn_connector_to_channel_with_bearer_token(
            ctx,
            channel,
            cloud.token.clone(),
        ));
    }
}

impl Drop for DaemonRuntime {
    fn drop(&mut self) {
        for task in &self.reachability_tasks {
            task.abort();
        }
        if let Some(task) = &self.cloud_task {
            task.abort();
        }
        // `services` aborts its own tasks on drop.
    }
}

/// How long a restarting daemon waits for its stored direct links to come
/// back before attaching to the cloud (peers may legitimately be offline,
/// so this is a grace window, not an assertion).
const RESTART_DIRECT_LINK_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Boots the daemon's user services from its data dir. Identity and trust
/// persist on disk, so a restart reuses them; `listener` carries the
/// pre-bound direct-TCP listener on first boot (restarts rebind the
/// recorded address).
///
/// The cloud connector is *not* spawned here: callers attach the cloud
/// after direct links are up. Bringing both up concurrently trips a
/// route-activation race in `ConnectionManager` (a stale in-flight
/// activation of the cloud route can land after the direct route activates,
/// evicting the direct link's pooled channel for good). The harness
/// sequences bring-up instead of changing production behavior; the race is
/// a finding for the routing chapter.
pub(crate) async fn start_daemon_runtime(
    inner: &DaemonInner,
    listener: Option<TcpListener>,
) -> DaemonRuntime {
    let identity = load_or_create_device_identity_in(&inner.data_dir)
        .unwrap_or_else(|error| panic!("load identity for daemon '{}': {error}", inner.name));
    let trust_store = TrustStore::load_or_create_in(&inner.data_dir)
        .unwrap_or_else(|error| panic!("load trust store for daemon '{}': {error}", inner.name));
    let security = DeviceRuntimeSecurity::new(identity, trust_store, inner.data_dir.clone());
    let trust = security.shared_trust_store();

    let state = testnet_server_state(&inner.name, inner.host_id);
    let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
    let mut services = start_user_services(state, agent_state, security)
        .await
        .unwrap_or_else(|error| panic!("start daemon '{}': {error}", inner.name));

    if let Some(addr) = inner.tcp_addr {
        let listener = match listener {
            Some(listener) => listener,
            None => bind_addr_with_retries(addr).await,
        };
        services.serve_external_tcp_listener(listener);
    }

    let reachability_tasks = services.spawn_reachability_links(&inner.name, false);
    DaemonRuntime {
        services,
        trust,
        reachability_tasks,
        cloud_task: None,
    }
}

/// Waits (bounded by [`RESTART_DIRECT_LINK_GRACE`]) for every peer with a
/// stored `DirectTcp` reachability to become routable again. Best effort:
/// offline peers simply exhaust the grace window.
async fn wait_for_stored_direct_peers(runtime: &DaemonRuntime) {
    let peers: Vec<HostId> = runtime
        .trust
        .read()
        .map(|store| {
            store
                .entries()
                .filter(|(_, entry)| {
                    entry
                        .reachabilities
                        .iter()
                        .any(|reachability| matches!(reachability, Reachability::DirectTcp { .. }))
                })
                .map(|(host_id, _)| host_id)
                .collect()
        })
        .unwrap_or_default();
    let deadline = tokio::time::Instant::now() + RESTART_DIRECT_LINK_GRACE;
    for peer in peers {
        while tokio::time::Instant::now() < deadline {
            if runtime.services.routing.host_entry(peer).await.is_some() {
                break;
            }
            tokio::time::sleep(super::assertions::POLL_INTERVAL).await;
        }
    }
}

/// Cloned-out handles to a running daemon's observable internals, so
/// assertions never hold the runtime lock across awaits.
pub(crate) struct DaemonParts {
    pub(crate) client: ClientService,
    pub(crate) connections: Arc<ConnectionManager>,
    pub(crate) routing: Arc<RoutingCore>,
    pub(crate) tunnels: Arc<TunnelPool>,
    pub(crate) trust: SharedTrustStore,
}

/// Handle to one daemon in a [`super::TestNet`]. Cheap to clone.
#[derive(Clone)]
pub struct Daemon {
    pub(crate) inner: Arc<DaemonInner>,
    pub(crate) net: Weak<NetInner>,
}

impl Daemon {
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn host_id(&self) -> HostId {
        self.inner.host_id
    }

    /// Presence: `other` shows up as online on this daemon's host-listing
    /// surface.
    pub async fn sees(&self, other: &Daemon) {
        let assertion = format!("'{}' sees '{}' online", self.name(), other.name());
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || {
                self.host_table()
                    .await
                    .iter()
                    .any(|host| host.id == other_id && host.online)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Presence negation: `other` is absent or offline on this daemon's
    /// host-listing surface.
    pub async fn cannot_see(&self, other: &Daemon) {
        let assertion = format!("'{}' cannot see '{}' online", self.name(), other.name());
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || {
                !self
                    .host_table()
                    .await
                    .iter()
                    .any(|host| host.id == other_id && host.online)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Route-shape assertions on this daemon's route to `other`; finish with
    /// [`RouteAssertion::via_direct`], [`RouteAssertion::via`], or
    /// [`RouteAssertion::via_cloud`].
    pub fn connects_to<'a>(&'a self, other: &'a Daemon) -> RouteAssertion<'a> {
        RouteAssertion {
            from: self,
            to: other,
        }
    }

    /// Trust-store check: this daemon holds a trust entry for `other`.
    pub async fn trusts(&self, other: &Daemon) {
        let assertion = format!("'{}' trusts '{}'", self.name(), other.name());
        eventually(
            &assertion,
            async || self.trusts_now(other).await,
            self.failure_dump(),
        )
        .await;
    }

    /// Trust-store negation: no trust entry for `other`.
    pub async fn does_not_trust(&self, other: &Daemon) {
        let assertion = format!("'{}' does not trust '{}'", self.name(), other.name());
        eventually(
            &assertion,
            async || !self.trusts_now(other).await,
            self.failure_dump(),
        )
        .await;
    }

    /// A real routed RPC: lists the agents on `other` by calling its
    /// `ClientService.ListAgents` over this daemon's current route. Returns
    /// the agent names.
    pub async fn lists_agents_on(&self, other: &Daemon) -> anyhow::Result<Vec<String>> {
        let Some(parts) = self.try_parts().await else {
            anyhow::bail!("daemon '{}' is not running", self.name());
        };
        let channel = parts.connections.channel_to(other.host_id()).await?;
        let mut client = wire::client_service_client::ClientServiceClient::new(channel);
        let agents = client
            .list_agents(wire::ListAgentsRequest {})
            .await?
            .into_inner()
            .agents;
        Ok(agents
            .into_iter()
            .map(|agent| agent.name.unwrap_or_default())
            .collect())
    }

    /// Graceful stop and restart with the same data dir; identity, trust,
    /// and the direct-TCP listener address all persist. Direct links are
    /// re-established from stored reachabilities before the cloud is
    /// reattached (see [`start_daemon_runtime`]).
    pub async fn restart(&self) {
        let mut guard = self.inner.runtime.lock().await;
        // Drop the old runtime first so its tasks abort and the TCP listener
        // port is released before the new runtime rebinds it.
        *guard = None;
        let mut runtime = start_daemon_runtime(&self.inner, None).await;
        if let Some(cloud) = &self.inner.cloud {
            wait_for_stored_direct_peers(&runtime).await;
            runtime.spawn_cloud_connector(&self.inner.name, cloud);
        }
        *guard = Some(runtime);
    }

    async fn trusts_now(&self, other: &Daemon) -> bool {
        let Some(parts) = self.try_parts().await else {
            return false;
        };
        parts
            .trust
            .read()
            .map(|store| store.entry(other.host_id()).is_some())
            .unwrap_or(false)
    }

    pub(crate) async fn try_parts(&self) -> Option<DaemonParts> {
        let guard = self.inner.runtime.lock().await;
        let runtime = guard.as_ref()?;
        Some(DaemonParts {
            client: runtime.services.client.clone(),
            connections: runtime.services.connections.clone(),
            routing: runtime.services.routing.clone(),
            tunnels: runtime.services.tunnels.clone(),
            trust: runtime.trust.clone(),
        })
    }

    /// The daemon's host-listing surface: trusted peers (online or not) plus
    /// online hosts, exactly what `ClientService` serves to clients.
    pub(crate) async fn host_table(&self) -> Vec<HostEntry> {
        match self.try_parts().await {
            Some(parts) => parts.client.subscribe_hosts_with_snapshot().await.0,
            None => Vec::new(),
        }
    }

    /// The route this daemon would use for a fresh call to `peer`: the best
    /// known route (what `channel_to` activates), falling back to the
    /// currently active route.
    pub(crate) async fn route_to(&self, peer: HostId) -> Option<Route> {
        let parts = self.try_parts().await?;
        if let Some(route) = parts.routing.best_route(peer).await {
            return Some(route);
        }
        parts.connections.active_route(peer).await
    }

    pub(crate) async fn knows_host(&self, host_id: HostId) -> bool {
        match self.try_parts().await {
            Some(parts) => parts.routing.host_entry(host_id).await.is_some(),
            None => false,
        }
    }

    /// Whether this daemon holds a single-hop (own-link) route to `host_id`.
    pub(crate) async fn has_single_hop_route_to(&self, host_id: HostId) -> bool {
        match self.try_parts().await {
            Some(parts) => parts
                .routing
                .best_route(host_id)
                .await
                .is_some_and(|route| route.len() == 1),
            None => false,
        }
    }

    pub(crate) async fn reconnect_cloud(&self) {
        let Some(cloud) = &self.inner.cloud else {
            return;
        };
        if let Some(runtime) = self.inner.runtime.lock().await.as_mut() {
            runtime.spawn_cloud_connector(&self.inner.name, cloud);
        }
    }

    /// Looks up a stored direct-TCP reachability for `peer` in this daemon's
    /// trust store.
    pub(crate) async fn direct_tcp_reachability_to(&self, peer: HostId) -> Option<Reachability> {
        let parts = self.try_parts().await?;
        let store = parts.trust.read().ok()?;
        store
            .entry(peer)?
            .reachabilities
            .iter()
            .find_map(|reachability| {
                matches!(reachability, Reachability::DirectTcp { .. }).then(|| reachability.clone())
            })
    }

    /// Spawns a one-shot direct-link establishment attempt toward `peer`,
    /// the same path daemons use for stored reachabilities at startup.
    pub(crate) async fn spawn_direct_link(&self, peer: HostId, reachability: Reachability) {
        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        runtime
            .services
            .reachability_link_connector()
            .spawn_pair_time_link(&self.inner.name, true, peer, reachability);
    }

    /// Renders the failure dump: declared topology, every daemon's host
    /// table, and this (failing) daemon's known routes.
    pub(crate) async fn failure_dump(&self) -> String {
        let mut out = String::from("=== testnet failure dump ===\n");
        if let Some(net) = self.net.upgrade() {
            let _ = writeln!(out, "declared topology:\n{}", net.topology);
            if let Some(cloud) = &net.cloud {
                let status = if cloud.is_online().await {
                    "online"
                } else {
                    "offline"
                };
                let _ = writeln!(
                    out,
                    "cloud relay: {status} at {} (host_id {})",
                    cloud.addr, cloud.host_id
                );
            }
            for daemon in &net.daemons {
                let _ = writeln!(
                    out,
                    "host table of '{}' ({}):",
                    daemon.name(),
                    daemon.host_id()
                );
                let table = daemon.host_table().await;
                if table.is_empty() {
                    let _ = writeln!(out, "  (empty or daemon not running)");
                }
                for host in table {
                    let _ = writeln!(
                        out,
                        "  - {} ({}) online={} trust={:?}",
                        host.name, host.id, host.online, host.trust_status
                    );
                }
            }
        } else {
            let _ = writeln!(out, "(testnet already dropped; no topology available)");
        }

        let _ = writeln!(out, "routes known to '{}':", self.name());
        let Some(parts) = self.try_parts().await else {
            let _ = writeln!(out, "  (daemon not running)");
            return out;
        };
        let mut peers: BTreeSet<HostId> = self
            .host_table()
            .await
            .into_iter()
            .map(|host| host.id)
            .collect();
        if let Ok(store) = parts.trust.read() {
            peers.extend(store.entries().map(|(host_id, _)| host_id));
        }
        peers.remove(&self.host_id());
        for peer in peers {
            let active = parts.connections.active_route(peer).await;
            let known = parts.connections.known_routes(peer).await;
            let _ = writeln!(
                out,
                "  {peer}: active={} known=[{}]",
                active
                    .as_ref()
                    .map_or_else(|| "none".to_string(), format_route),
                known
                    .iter()
                    .map(format_route)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        out
    }
}

pub(crate) fn format_route(route: &Route) -> String {
    let hops = route
        .iter()
        .map(|link| link.as_str())
        .collect::<Vec<_>>()
        .join(" > ");
    format!("[{hops}]")
}

/// Pending route-shape assertion created by [`Daemon::connects_to`].
pub struct RouteAssertion<'a> {
    from: &'a Daemon,
    to: &'a Daemon,
}

impl RouteAssertion<'_> {
    /// The route is a single hop: a direct link to the peer.
    pub async fn via_direct(self) {
        let assertion = format!(
            "'{}' connects to '{}' via a direct link",
            self.from.name(),
            self.to.name()
        );
        let to_id = self.to.host_id();
        eventually(
            &assertion,
            async || matches!(self.from.route_to(to_id).await, Some(route) if route.len() == 1),
            self.from.failure_dump(),
        )
        .await;
    }

    /// The route's first hop(s) go through `relay`: it extends the route to
    /// the relay node.
    pub async fn via(self, relay: &Daemon) {
        self.via_host(relay.host_id(), relay.name()).await;
    }

    /// The route goes through the testnet cloud relay.
    pub async fn via_cloud(self) {
        let cloud_id = self
            .from
            .net
            .upgrade()
            .and_then(|net| net.cloud.as_ref().map(|cloud| cloud.host_id))
            .expect("topology has no cloud relay");
        self.via_host(cloud_id, "cloud").await;
    }

    async fn via_host(self, relay_id: HostId, relay_name: &str) {
        let assertion = format!(
            "'{}' connects to '{}' via '{relay_name}'",
            self.from.name(),
            self.to.name()
        );
        let to_id = self.to.host_id();
        eventually(
            &assertion,
            async || {
                let Some(route) = self.from.route_to(to_id).await else {
                    return false;
                };
                let Some(relay_route) = self.from.route_to(relay_id).await else {
                    return false;
                };
                route.len() > relay_route.len() && route.starts_with_route(&relay_route)
            },
            self.from.failure_dump(),
        )
        .await;
    }
}
