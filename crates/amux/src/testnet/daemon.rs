//! A whole in-process daemon and its observation/assertion surface.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};

use crate::HostId;
use crate::client::Client;
use crate::connection::ConnectionManager;
use crate::dispatcher::TrackedTcpConnections;
use crate::identity::{device_key_path, load_or_create_device_identity_in};
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
    /// OS-level duplicates of every TCP socket this daemon's runtime holds
    /// open to the outside world: sockets its external TCP listener has
    /// accepted *and* the outbound sockets its cloud connector dialed. A
    /// real restart closes all of them with the process; the in-process
    /// restart severs them explicitly. Merely dropping the runtime is not
    /// enough: per-connection dispatcher tasks are detached, and aborting
    /// the cloud connector task leaks its established link (see the
    /// "Findings from spec-suite construction" entry in
    /// `notes/NETWORKING_REVIEW.md`), so without the sever peers and the
    /// relay keep treating the dead incarnation as online.
    pub(crate) tracked_tcp: TrackedTcpConnections,
}

impl DaemonInner {
    fn sever_tracked_tcp(&self) {
        let connections = std::mem::take(
            &mut *self
                .tracked_tcp
                .lock()
                .expect("tracked TCP connection registry poisoned"),
        );
        for connection in connections {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
    }
}

pub(crate) struct DaemonRuntime {
    pub(crate) services: StartedUserServices,
    pub(crate) trust: SharedTrustStore,
    reachability_tasks: Vec<JoinHandle<()>>,
    cloud_task: Option<JoinHandle<Result<(), tonic::Status>>>,
}

impl DaemonRuntime {
    pub(crate) fn spawn_cloud_connector(&mut self, inner: &DaemonInner) {
        let cloud = inner
            .cloud
            .as_ref()
            .expect("spawn_cloud_connector on a daemon without a cloud attachment");
        if let Some(task) = self.cloud_task.take() {
            task.abort();
        }
        let channel = tracked_cloud_channel(cloud.addr, inner.tracked_tcp.clone());
        // Randomised like production (`randomise_link_name` defaults to
        // true): a restarted daemon must come back under a *new* link name,
        // or its peers and the relay can mistake the new link for the old
        // one and keep stale routes/tunnels alive.
        let ctx = self
            .services
            .routing_connector_ctx(generate_server_link(&format!("cloud-{}", inner.name), true));
        self.cloud_task = Some(spawn_connector_to_channel_with_bearer_token(
            ctx,
            channel,
            cloud.token.clone(),
        ));
    }
}

/// A lazy tonic channel to the testnet cloud relay that registers an
/// OS-level duplicate of every TCP socket it dials in `tracked`, so a
/// daemon "restart" can sever its outbound cloud connection the way a real
/// process exit would.
///
// NOTE: works around the connector-abort link leak documented under
// "Findings from spec-suite construction" in `notes/NETWORKING_REVIEW.md`:
// aborting an established routing-connector task never runs `cleanup_link`,
// and the link registry's clone of the outbound sender keeps the Connect
// request stream open, so neither the relay nor any peer ever observes the
// link going down. Severing the socket gives the relay the same EOF a dead
// process would.
fn tracked_cloud_channel(addr: SocketAddr, tracked: TrackedTcpConnections) -> Channel {
    Endpoint::from_shared(format!("http://{addr}"))
        .expect("testnet cloud endpoint URI")
        .connect_with_connector_lazy(tower::service_fn(move |_uri: Uri| {
            let tracked = tracked.clone();
            async move {
                let stream = tokio::net::TcpStream::connect(addr).await?;
                stream.set_nodelay(true)?;
                let std_stream = stream.into_std()?;
                let duplicate = std_stream.try_clone()?;
                tracked
                    .lock()
                    .expect("tracked TCP connection registry poisoned")
                    .push(duplicate);
                Ok::<_, std::io::Error>(TokioIo::new(tokio::net::TcpStream::from_std(std_stream)?))
            }
        }))
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
/// after direct links are up, which keeps initial topologies deterministic.
/// (Bringing both up concurrently used to trip a route-activation race in
/// `ConnectionManager` that stranded the direct link's pooled channel; that
/// race is now guarded in `ConnectionManager::activate_route` and written
/// up under "Findings from spec-suite construction" in
/// `notes/NETWORKING_REVIEW.md` §6.2.)
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

    let state = testnet_server_state(
        &inner.name,
        inner.host_id,
        inner.tcp_addr.map(|addr| addr.port()),
        inner.cloud.is_some(),
    );
    let agent_state = Arc::new(RwLock::new(AgentServiceState::new()));
    let mut services = start_user_services(state, agent_state, security)
        .await
        .unwrap_or_else(|error| panic!("start daemon '{}': {error}", inner.name));

    if let Some(addr) = inner.tcp_addr {
        let listener = match listener {
            Some(listener) => listener,
            None => bind_addr_with_retries(addr).await,
        };
        services.serve_external_tcp_listener_tracked(listener, inner.tracked_tcp.clone());
    }

    let reachability_tasks = services.spawn_reachability_links(&inner.name, true);
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
    ///
    /// Bounded by the assertion timeout: a call into a dead channel (e.g. a
    /// peer's previous incarnation after a restart) yields `Err`, never a
    /// hung test.
    pub async fn lists_agents_on(&self, other: &Daemon) -> anyhow::Result<Vec<String>> {
        match tokio::time::timeout(
            super::assertions::DEFAULT_TIMEOUT,
            self.lists_agents_on_inner(other),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => anyhow::bail!(
                "routed call from '{}' to '{}' timed out",
                self.name(),
                other.name()
            ),
        }
    }

    async fn lists_agents_on_inner(&self, other: &Daemon) -> anyhow::Result<Vec<String>> {
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

    /// Asserts that a routed call to `other` (eventually) succeeds. Use this
    /// after network churn — restart, failover, re-pairing — where the first
    /// call may legitimately race link re-establishment; use
    /// [`Self::lists_agents_on`] directly when asserting on a settled
    /// network or on call *failure*.
    pub async fn can_call(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' can complete a routed call to '{}'",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || self.lists_agents_on_inner(other).await.is_ok(),
            self.failure_dump(),
        )
        .await;
    }

    /// Graceful stop and restart with the same data dir; identity, trust,
    /// and the direct-TCP listener address all persist. Direct links are
    /// re-established from stored reachabilities before the cloud is
    /// reattached (see [`start_daemon_runtime`]).
    pub async fn restart(&self) {
        // Drop the old runtime first so its tasks abort and the TCP listener
        // port is released before the new runtime rebinds it; sever every
        // tracked socket like the process exit a real restart implies. The
        // runtime lock is released between the steps: holding it across
        // `wait_until_peers_see_us_down` would deadlock the failure dump,
        // which queries this daemon's host table through the same lock.
        *self.inner.runtime.lock().await = None;
        self.inner.sever_tracked_tcp();
        self.wait_until_peers_see_us_down().await;
        let mut runtime = start_daemon_runtime(&self.inner, None).await;
        if self.inner.cloud.is_some() {
            wait_for_stored_direct_peers(&runtime).await;
            runtime.spawn_cloud_connector(&self.inner);
        }
        *self.inner.runtime.lock().await = Some(runtime);
    }

    /// Waits until every other daemon has seen this one go offline, so the
    /// restart comes back into a settled network. Without this beat, a
    /// daemon that reattaches to the cloud before its HostDown propagated
    /// leaves peers holding tunnels/channels to its dead incarnation that
    /// never recover (calls fail with "TLS TunnelTransport already
    /// consumed" indefinitely) — a real fast-restart race, left to the
    /// routing chapter to lock in deliberately.
    async fn wait_until_peers_see_us_down(&self) {
        let Some(net) = self.net.upgrade() else {
            return;
        };
        let my_id = self.host_id();
        for other in &net.daemons {
            if other.host_id() == my_id {
                continue;
            }
            let assertion = format!(
                "'{}' observes '{}' going down for its restart",
                other.name(),
                self.name()
            );
            eventually(
                &assertion,
                async || {
                    !other
                        .host_table()
                        .await
                        .iter()
                        .any(|host| host.id == my_id && host.online)
                },
                other.failure_dump(),
            )
            .await;
        }
    }

    /// Key rotation: stops the daemon, wipes its device key (the `host_id`
    /// file is kept), and restarts. The daemon comes back with the same
    /// `host_id` under a freshly generated keypair, so peers still pin the
    /// old key until they re-pair.
    pub async fn restart_with_new_key(&self) {
        *self.inner.runtime.lock().await = None;
        self.inner.sever_tracked_tcp();
        self.wait_until_peers_see_us_down().await;
        std::fs::remove_file(device_key_path(&self.inner.data_dir)).unwrap_or_else(|error| {
            panic!("remove device key for daemon '{}': {error}", self.name())
        });
        let mut runtime = start_daemon_runtime(&self.inner, None).await;
        if self.inner.cloud.is_some() {
            runtime.spawn_cloud_connector(&self.inner);
        }
        *self.inner.runtime.lock().await = Some(runtime);
    }

    /// The daemon's persisted identity, re-read from its data dir:
    /// `(host_id, pubkey)`.
    pub fn identity_on_disk(&self) -> (HostId, Vec<u8>) {
        let identity = load_or_create_device_identity_in(&self.inner.data_dir)
            .unwrap_or_else(|error| panic!("load identity for daemon '{}': {error}", self.name()));
        (identity.host_id, identity.public_key().to_vec())
    }

    /// Trust-store check: this daemon's entry for `other` pins `other`'s
    /// *current* on-disk public key (catches stale entries after rotation).
    pub async fn trusts_current_key_of(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' trusts the current key of '{}'",
            self.name(),
            other.name()
        );
        let (_, pubkey) = other.identity_on_disk();
        eventually(
            &assertion,
            async || {
                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                parts
                    .trust
                    .read()
                    .map(|store| {
                        store
                            .entry(other.host_id())
                            .is_some_and(|entry| entry.pubkey == pubkey)
                    })
                    .unwrap_or(false)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Trust-store check: this daemon trusts `other` but holds no outbound
    /// reachability hint for it (e.g. the responder side of SSH pairing).
    pub async fn trusts_without_reachability(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' trusts '{}' without any outbound reachability",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || {
                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                parts
                    .trust
                    .read()
                    .map(|store| {
                        store
                            .entry(other.host_id())
                            .is_some_and(|entry| entry.reachabilities.is_empty())
                    })
                    .unwrap_or(false)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Opens a fresh local-admin client to this daemon's `ClientService`,
    /// exactly what a local CLI gets over the Unix socket.
    pub(crate) async fn admin_client(&self) -> Client {
        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let (channel, _accept_task) = runtime.services.open_in_process_client_channel();
        Client::from_client_service_channel(channel, None)
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
        if self.inner.cloud.is_none() {
            return;
        }
        if let Some(runtime) = self.inner.runtime.lock().await.as_mut() {
            runtime.spawn_cloud_connector(&self.inner);
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
