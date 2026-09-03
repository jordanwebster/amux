//! A whole in-process daemon and its observation/assertion surface.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};

use super::NetInner;
use super::assertions::eventually;
use super::net::{RegisteredToken, TokenRegistry, bind_addr_with_retries, testnet_server_state};
use crate::HostId;
use crate::client::Client;
use crate::connection::ConnectionManager;
use crate::dispatcher::TrackedTcpConnections;
use crate::identity::{device_key_path, load_or_create_device_identity_in};
use crate::protocol::wire;
use crate::routing::{
    HostEntry, HostTrustStatus, LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken,
    LinkConnectorTokenRefresher, Route, RoutingCore,
    spawn_connector_to_channel_with_auth_and_establishment,
    spawn_connector_to_channel_with_bearer_token,
};
use crate::services::{
    ClientService, DeviceRuntimeSecurity, PtyAgentHost, StartedUserServices, start_user_services,
};
use crate::trust::{Reachability, SharedTrustStore, TrustStore};
use crate::tunnel::TunnelPool;

/// Parameters a daemon needs to (re)connect to the testnet cloud relay.
pub(crate) struct CloudAttachment {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
    /// The cloud user this daemon attaches as (the relay's authenticator
    /// maps its bearer token to this user).
    pub(crate) user_id: uuid::Uuid,
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
    /// the cloud connector task never runs its link cleanup, leaking the
    /// established link — so without the sever, peers and the relay keep
    /// treating the dead incarnation as online.
    pub(crate) tracked_tcp: TrackedTcpConnections,
    /// OS-level duplicates of the outbound sockets the cloud connector
    /// dialed, kept separately from `tracked_tcp` so credential-rollover
    /// verbs can sever just the cloud link while direct links stay up.
    pub(crate) tracked_cloud_tcp: TrackedTcpConnections,
}

impl DaemonInner {
    fn sever_tracked_tcp(&self) {
        sever_registry(&self.tracked_tcp);
        self.sever_tracked_cloud_tcp();
    }

    fn sever_tracked_cloud_tcp(&self) {
        sever_registry(&self.tracked_cloud_tcp);
    }
}

fn sever_registry(registry: &TrackedTcpConnections) {
    let connections = std::mem::take(
        &mut *registry
            .lock()
            .expect("tracked TCP connection registry poisoned"),
    );
    for connection in connections {
        let _ = connection.shutdown(std::net::Shutdown::Both);
    }
}

pub(crate) struct DaemonRuntime {
    pub(crate) services: StartedUserServices,
    pub(crate) agent_host: Arc<PtyAgentHost>,
    pub(crate) trust: SharedTrustStore,
    reachability_tasks: Vec<JoinHandle<()>>,
    cloud_task: Option<JoinHandle<Result<(), tonic::Status>>>,
    /// Listens for `ClientService.Shutdown`/`Suspend` requests and tears the
    /// daemon down, so a paired peer's routed disruptive op is observable as
    /// the daemon going offline. Aborted when the runtime is dropped.
    shutdown_task: Option<JoinHandle<()>>,
}

impl DaemonRuntime {
    pub(crate) fn spawn_cloud_connector(&mut self, inner: &DaemonInner) {
        let (ctx, channel, token) = self.cloud_connector_parts(inner);
        self.cloud_task = Some(spawn_connector_to_channel_with_bearer_token(
            ctx, channel, token,
        ));
    }

    /// Like [`Self::spawn_cloud_connector`], but with the production-shaped
    /// connector auth: an initial token with an expiry plus a refresher the
    /// Reauth flow calls before that expiry.
    pub(crate) fn spawn_cloud_connector_with_auth(
        &mut self,
        inner: &DaemonInner,
        auth: LinkConnectorAuth,
    ) {
        let (ctx, channel, _token) = self.cloud_connector_parts(inner);
        let (task, _established_rx) =
            spawn_connector_to_channel_with_auth_and_establishment(ctx, channel, auth);
        self.cloud_task = Some(task);
    }

    fn cloud_connector_parts(
        &mut self,
        inner: &DaemonInner,
    ) -> (LinkConnectorCtx, Channel, String) {
        let cloud = inner
            .cloud
            .as_ref()
            .expect("spawn_cloud_connector on a daemon without a cloud attachment");
        if let Some(task) = self.cloud_task.take() {
            task.abort();
        }
        let channel = tracked_cloud_channel(cloud.addr, inner.tracked_cloud_tcp.clone());
        // Every (re)connection is a fresh link instance: a restarted daemon
        // comes back under a new link identity by construction.
        let ctx = self.services.link_connector_ctx();
        (ctx, channel, cloud.token.clone())
    }
}

/// A lazy tonic channel to the testnet cloud relay that registers an
/// OS-level duplicate of every TCP socket it dials in `tracked`, so a
/// daemon "restart" can sever its outbound cloud connection the way a real
/// process exit would.
///
// NOTE: works around the connector-abort link leak:
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
        if let Some(task) = &self.shutdown_task {
            task.abort();
        }
        // `services` aborts its own tasks on drop.
    }
}

/// How long a restarting daemon waits for its stored direct links to come
/// back before attaching to the cloud (peers may legitimately be offline,
/// so this is a grace window, not an assertion).
const RESTART_DIRECT_LINK_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Per-attempt bound for the polled call verbs ([`Daemon::can_call`],
/// [`Daemon::cannot_call`]): one attempt must never eat the whole assertion
/// budget, so a black-holed attempt is retried (or counted as unreachable)
/// instead of hanging the poll.
const CALL_ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Boots the daemon's user services from its data dir. Identity and trust
/// persist on disk, so a restart reuses them; `listener` carries the
/// pre-bound direct-TCP listener on first boot (restarts rebind the
/// recorded address).
///
/// The cloud connector is *not* spawned here: callers attach the cloud
/// after direct links are up, which keeps initial topologies deterministic.
/// (Bringing both up concurrently used to trip a route-activation race in
/// `ConnectionManager` that stranded the direct link's pooled channel; that
/// race is now guarded in `ConnectionManager::activate_route` — a stale
/// activation no longer demotes an active direct route.)
pub(crate) async fn start_daemon_runtime(
    inner: &Arc<DaemonInner>,
    listener: Option<TcpListener>,
) -> DaemonRuntime {
    let identity = load_or_create_device_identity_in(&inner.data_dir)
        .unwrap_or_else(|error| panic!("load identity for daemon '{}': {error}", inner.name));
    let trust_store = TrustStore::load_or_create_in(&inner.data_dir)
        .unwrap_or_else(|error| panic!("load trust store for daemon '{}': {error}", inner.name));
    let security = DeviceRuntimeSecurity::new(identity, trust_store, inner.data_dir.clone());
    let trust = security.shared_trust_store();

    let (state, shutdown_rx) = testnet_server_state(
        &inner.name,
        inner.host_id,
        inner.tcp_addr.map(|addr| addr.port()),
        inner.cloud.is_some(),
    );
    let config = crate::config::Config::default();
    let route = crate::agents::McpLaunchRoute::for_current_process(&config, inner.host_id)
        .expect("testnet managed MCP route should be usable");
    let agent_host =
        PtyAgentHost::new_with_mcp_launch_route(route, crate::keymap_dir(&inner.data_dir))
            .expect("testnet Codex private socket path should be usable");
    let mut services = start_user_services(state, Some(agent_host.clone()), security)
        .await
        .unwrap_or_else(|error| panic!("start daemon '{}': {error}", inner.name));

    if let Some(addr) = inner.tcp_addr {
        let listener = match listener {
            Some(listener) => listener,
            None => bind_addr_with_retries(addr).await,
        };
        services.serve_external_tcp_listener_tracked(listener, inner.tracked_tcp.clone());
    }

    // Track dialed direct-link sockets too: with both ends of a link
    // recording routes, a leaked dialer socket would keep the *acceptor*
    // treating a stopped daemon as online.
    services
        .reachability_link_connector()
        .track_dialed_tcp(inner.tracked_tcp.clone());
    let reachability_tasks = services.spawn_reachability_links();
    let shutdown_task = Some(spawn_shutdown_handler(Arc::downgrade(inner), shutdown_rx));
    DaemonRuntime {
        services,
        agent_host,
        trust,
        reachability_tasks,
        cloud_task: None,
        shutdown_task,
    }
}

/// Drives the daemon's `ClientService.Shutdown`/`Suspend` requests: when a
/// paired peer invokes one over the route, the handler replies success and
/// stops the daemon (severs its external sockets and drops the runtime), so
/// the network observes it going down — the in-process stand-in for a process
/// exit. Suspend is treated like Shutdown for the purposes of the network
/// observable (it parks agents and ends the server in production too).
fn spawn_shutdown_handler(
    inner: Weak<DaemonInner>,
    mut shutdown_rx: tokio::sync::mpsc::Receiver<crate::user_state::ShutdownRequest>,
) -> JoinHandle<()> {
    use crate::user_state::ShutdownRequest;
    tokio::spawn(async move {
        // One disruptive request is enough to take the daemon down; ignore any
        // further requests (the handler is aborted with the runtime anyway).
        let Some(request) = shutdown_rx.recv().await else {
            return;
        };
        match request {
            ShutdownRequest::Shutdown { reply } => {
                let _ = reply.send(Ok(()));
            }
            ShutdownRequest::Suspend { reply, .. } => {
                let _ = reply.send(Ok(0));
            }
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        // Stop off this task: setting the runtime to `None` drops the runtime
        // (which aborts this very handler), so the teardown must run detached
        // or it would cancel itself mid-flight.
        tokio::spawn(async move {
            inner.sever_tracked_tcp();
            *inner.runtime.lock().await = None;
        });
    })
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
    pub(crate) agent_host: Arc<PtyAgentHost>,
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
    /// The daemon's builder-declared name (e.g. `"laptop"`).
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The daemon's persistent host id (stable across restarts).
    pub fn host_id(&self) -> HostId {
        self.inner.host_id
    }

    /// The same verbose JSON diagnostics a local client requests from this
    /// daemon, parsed for structural assertions in the protocol spec.
    pub async fn debug_dump(&self, verbose: bool) -> serde_json::Value {
        let dump = self
            .admin_client()
            .await
            .debug_dump_verbose(verbose, crate::DebugFormat::Json)
            .await
            .unwrap_or_else(|error| {
                panic!("'{}' failed to read its debug dump: {error}", self.name())
            });
        serde_json::from_str(&dump).unwrap_or_else(|error| {
            panic!("'{}' returned invalid debug JSON: {error}", self.name())
        })
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

    /// Presence of a trusted-but-offline peer: `other` is still *listed* on
    /// this daemon's host-listing surface (from the trust store) but with
    /// `online = false`.
    pub async fn sees_offline(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' lists '{}' as a known but offline host",
            self.name(),
            other.name()
        );
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || {
                self.host_table()
                    .await
                    .iter()
                    .any(|host| host.id == other_id && !host.online)
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
        let mut client = wire::client_service_client(channel);
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
    ///
    /// Each attempt is individually time-bounded so the poll can retry: an
    /// attempt whose tunnel TLS lands in a closing peer window blackholes
    /// for the full 10s device handshake timeout (the dispatcher silently
    /// drops the stream and nothing reaches the initiator), while the next
    /// attempt's fresh tunnel goes through.
    pub async fn can_call(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' can complete a routed call to '{}'",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || {
                matches!(
                    tokio::time::timeout(CALL_ATTEMPT_TIMEOUT, self.lists_agents_on_inner(other))
                        .await,
                    Ok(Ok(_))
                )
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Asserts that routed calls to `other` (eventually) fail: revocation or
    /// an outage has made the peer unreachable. An attempt that completes
    /// with an error and an attempt that hangs past the per-attempt bound
    /// both count as unreachable.
    pub async fn cannot_call(&self, other: &Daemon) {
        let assertion = format!(
            "routed calls from '{}' to '{}' fail",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || match tokio::time::timeout(
                CALL_ATTEMPT_TIMEOUT,
                self.lists_agents_on_inner(other),
            )
            .await
            {
                Ok(result) => result.is_err(),
                Err(_) => true,
            },
            self.failure_dump(),
        )
        .await;
    }

    /// The local pairing-candidate inventory: `ClientService.ListHosts` with
    /// `scope = PAIRING_CANDIDATES` over the local-admin surface, as the UI
    /// would request it before prompting the user to pair.
    pub async fn pairing_candidates(&self) -> Vec<HostId> {
        let hosts = self
            .admin_client()
            .await
            .list_pairing_hosts()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to list pairing candidates: {error}",
                    self.name()
                )
            });
        hosts.into_iter().map(|host| host.id).collect()
    }

    /// Pairing-candidate assertion: `other` (eventually) shows up in this
    /// daemon's local pairing-candidate inventory.
    pub async fn sees_pairing_candidate(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' lists '{}' as a pairing candidate",
            self.name(),
            other.name()
        );
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || self.pairing_candidates().await.contains(&other_id),
            self.failure_dump(),
        )
        .await;
    }

    /// An untrusted cloud-visible host should be offered for pairing without
    /// starting the normal trusted-device tunnel path. Before trust exists,
    /// that path can only fail with mTLS errors and consume tunnel budget.
    pub async fn sees_pairing_candidate_without_trusted_dial(&self, other: &Daemon) {
        self.sees_pairing_candidate(other).await;

        let assertion = format!(
            "'{}' keeps '{}' as a pairing candidate without a trusted dial attempt",
            self.name(),
            other.name()
        );
        let other_id = other.host_id();
        super::assertions::consistently_for(
            &assertion,
            std::time::Duration::from_millis(750),
            async || {
                let Some(entry) = self
                    .host_table()
                    .await
                    .into_iter()
                    .find(|host| host.id == other_id)
                else {
                    return false;
                };
                if entry.trust_status != HostTrustStatus::UntrustedButOnline
                    || entry.last_dial_error.is_some()
                {
                    return false;
                }

                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                !parts
                    .tunnels
                    .active_tunnels()
                    .await
                    .into_iter()
                    .any(|(_, peer, _)| peer == other_id)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// A routed `ClientService.ListHosts(PAIRING_CANDIDATES)` against
    /// `other`, i.e. what a *paired remote* caller gets when it asks for
    /// pairing-candidate inventory. The scope is reserved for local
    /// callers (docs/ARCHITECTURE.md "Service surface map").
    pub async fn list_pairing_candidates_on(&self, other: &Daemon) -> anyhow::Result<Vec<HostId>> {
        self.list_hosts_on_scoped(other, wire::list_hosts_request::Scope::PairingCandidates)
            .await
    }

    /// A routed `ClientService.ListHosts(ALL)` against `other`: the host
    /// inventory `other` serves to a paired remote caller.
    pub async fn list_hosts_on(&self, other: &Daemon) -> anyhow::Result<Vec<HostId>> {
        self.list_hosts_on_scoped(other, wire::list_hosts_request::Scope::All)
            .await
    }

    async fn list_hosts_on_scoped(
        &self,
        other: &Daemon,
        scope: wire::list_hosts_request::Scope,
    ) -> anyhow::Result<Vec<HostId>> {
        let request = async {
            let Some(parts) = self.try_parts().await else {
                anyhow::bail!("daemon '{}' is not running", self.name());
            };
            let channel = parts.connections.channel_to(other.host_id()).await?;
            let mut client = wire::client_service_client(channel);
            let hosts = client
                .list_hosts(wire::ListHostsRequest {
                    scope: scope as i32,
                })
                .await?
                .into_inner()
                .hosts;
            hosts
                .into_iter()
                .map(|host| {
                    HostId::from_slice(&host.host_id)
                        .map_err(|error| anyhow::anyhow!("malformed host_id in response: {error}"))
                })
                .collect()
        };
        match tokio::time::timeout(super::assertions::DEFAULT_TIMEOUT, request).await {
            Ok(result) => result,
            Err(_) => anyhow::bail!(
                "routed ListHosts from '{}' to '{}' timed out",
                self.name(),
                other.name()
            ),
        }
    }

    /// Opens a long-lived routed gRPC stream to `other` over this daemon's
    /// *current* route — a `ClientService.SubscribeHosts` subscription
    /// served by `other` — confirmed live by reading its first snapshot
    /// event. Route swaps and revocations are expected to break it; finish
    /// with [`RoutedStream::expect_disconnect`] (or
    /// [`RoutedStream::expect_stalled_open`] where teardown is one-sided).
    pub async fn open_event_stream_to(&self, other: &Daemon) -> RoutedStream {
        let description = format!(
            "routed event stream from '{}' to '{}'",
            self.name(),
            other.name()
        );
        match tokio::time::timeout(
            super::assertions::DEFAULT_TIMEOUT,
            self.open_event_stream_inner(other),
        )
        .await
        {
            Ok(Ok(stream)) => RoutedStream {
                description,
                stream,
            },
            Ok(Err(error)) => panic!("failed to open {description}: {error}"),
            Err(_) => panic!("timed out opening {description}"),
        }
    }

    async fn open_event_stream_inner(
        &self,
        other: &Daemon,
    ) -> anyhow::Result<tonic::Streaming<wire::SubscribeHostsResponse>> {
        let Some(parts) = self.try_parts().await else {
            anyhow::bail!("daemon '{}' is not running", self.name());
        };
        let channel = parts.connections.channel_to(other.host_id()).await?;
        let mut client = wire::client_service_client(channel);
        let mut stream = client
            .subscribe_hosts(wire::SubscribeHostsRequest {})
            .await?
            .into_inner();
        match stream.message().await? {
            Some(_) => Ok(stream),
            None => anyhow::bail!("subscription ended before its first snapshot event"),
        }
    }

    /// Stops the daemon like a process exit: its tasks die and every socket
    /// it held to the outside world is severed. Returns once every other
    /// daemon has observed it going offline, so follow-up assertions start
    /// from a settled network.
    ///
    /// The runtime lock is released before the wait: holding it across
    /// `wait_until_peers_see_us_down` would deadlock the failure dump,
    /// which queries this daemon's host table through the same lock.
    pub async fn stop(&self) {
        *self.inner.runtime.lock().await = None;
        self.inner.sever_tracked_tcp();
        self.wait_until_peers_see_us_down().await;
    }

    /// Stop and restart with the same data dir; identity, trust, and the
    /// direct-TCP listener address all persist. Direct links are
    /// re-established from stored reachabilities before the cloud is
    /// reattached (see [`start_daemon_runtime`]).
    pub async fn restart(&self) {
        // Stop first so the old runtime's tasks abort and the TCP listener
        // port is released before the new runtime rebinds it.
        self.stop().await;
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
        self.stop().await;
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
            agent_host: runtime.agent_host.clone(),
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
        if let Some(route) = parts.routing.route_to(peer).await {
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

    /// Whether this daemon holds a direct (own-link) route to `host_id`.
    pub(crate) async fn has_direct_route_to(&self, host_id: HostId) -> bool {
        match self.try_parts().await {
            Some(parts) => parts
                .routing
                .route_to(host_id)
                .await
                .is_some_and(|route| route.is_direct()),
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

    /// Credential rollover onto a short-lived cloud JWT: severs the current
    /// cloud link (the relay sees the same EOF a re-login would produce) and
    /// reattaches with a bearer token that expires `ttl` from now, plus the
    /// production-shaped refresher the Reauth flow calls. Returns a handle
    /// whose [`ExpiringJwt::expired`] waits out the initial token's expiry.
    ///
    /// `ttl` must be shorter than the assertion timeout and is best kept
    /// well under the production `ROUTING_AUTH_REFRESH_BEFORE_EXPIRY`
    /// (300s), so the connector's proactive refresh fires immediately after
    /// establishment — the only way to drive the flow without real minutes.
    pub async fn reattach_cloud_with_expiring_jwt(&self, ttl: std::time::Duration) -> ExpiringJwt {
        assert!(
            ttl < super::assertions::DEFAULT_TIMEOUT,
            "the JWT ttl must fit within the assertion timeout"
        );
        let net = self.net.upgrade().expect("testnet already dropped");
        let cloud_relay = net
            .cloud
            .as_ref()
            .expect("this testnet was built without .cloud()");
        let attachment = self
            .inner
            .cloud
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not cloud-attached", self.name()));

        // Cut the current link first; reattaching while it is still up would
        // give the daemon two concurrent cloud links.
        self.inner.sever_tracked_cloud_tcp();
        let assertion = format!(
            "'{}' drops its previous cloud link for the credential rollover",
            self.name()
        );
        eventually(
            &assertion,
            async || !self.has_direct_route_to(cloud_relay.host_id).await,
            self.failure_dump(),
        )
        .await;

        let token = format!("jwt-initial-{}", uuid::Uuid::new_v4().simple());
        let expires_at = std::time::SystemTime::now() + ttl;
        cloud_relay.register_token(&token, attachment.user_id, ttl);
        let auth = LinkConnectorAuth::new(
            LinkConnectorToken { token, expires_at },
            Arc::new(RegistryTokenRefresher {
                tokens: cloud_relay.token_registry(),
                user_id: attachment.user_id,
            }),
        );
        {
            let mut guard = self.inner.runtime.lock().await;
            let runtime = guard
                .as_mut()
                .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
            runtime.spawn_cloud_connector_with_auth(&self.inner, auth);
        }
        let assertion = format!(
            "'{}' reattaches to the cloud relay under the short-lived JWT",
            self.name()
        );
        eventually(
            &assertion,
            async || self.knows_host(cloud_relay.host_id).await,
            self.failure_dump(),
        )
        .await;
        ExpiringJwt {
            daemon: self.clone(),
            expires_at,
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
            .spawn_pair_time_link(peer, reachability);
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
                        "  - {} ({}) online={} trust={:?} last_dial_error={}",
                        host.name,
                        host.id,
                        host.online,
                        host.trust_status,
                        host.last_dial_error.as_deref().unwrap_or("none")
                    );
                }
                if let Some(parts) = daemon.try_parts().await {
                    for (id, peer, link) in parts.tunnels.active_tunnels().await {
                        let _ = writeln!(out, "  tunnel {id} peer={peer} link={link}");
                    }
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
                    .map_or_else(|| "none".to_string(), |route| route.to_string()),
                known
                    .iter()
                    .map(|route| route.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        out
    }
}

/// A live routed gRPC stream opened by [`Daemon::open_event_stream_to`].
pub struct RoutedStream {
    description: String,
    stream: tonic::Streaming<wire::SubscribeHostsResponse>,
}

impl RoutedStream {
    /// Asserts the stream breaks with an error status within the assertion
    /// timeout. Messages still arriving before the break are drained; a
    /// clean end-of-stream or survival past the timeout fails the
    /// assertion.
    ///
    /// NOTE: what tonic surfaces when the tunnel transport dies under a
    /// live stream is `Unknown: h2 protocol error: error reading a body
    /// from connection`, not `UNAVAILABLE`. This verb locks the
    /// user-observable contract — the stream breaks promptly with an
    /// error — and leaves the exact status code unasserted.
    pub async fn expect_disconnect(mut self) {
        let deadline = tokio::time::Instant::now() + super::assertions::DEFAULT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.stream.message()).await {
                // Live events may legitimately flow until the route drops.
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) => panic!(
                    "{} ended cleanly; in-flight streams on a dropped route must \
                     break with an error",
                    self.description
                ),
                Ok(Err(_)) => return,
                Err(_) => panic!(
                    "{} is still alive; expected it to break with an error",
                    self.description
                ),
            }
        }
    }

    /// Asserts the stream does *not* terminate within a short observation
    /// window — it has gone silent rather than broken. Stray events that
    /// were already in flight are tolerated; any termination (error or
    /// clean end) fails the assertion.
    ///
    /// For one-sided teardowns where nothing reaches the peer: its
    /// in-flight streams go silent instead of breaking, and only a
    /// transport keepalive would eventually end them.
    pub async fn expect_stalled_open(mut self) {
        const OBSERVATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
        let deadline = tokio::time::Instant::now() + OBSERVATION_WINDOW;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.stream.message()).await {
                // Events already in flight may still be delivered.
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) => panic!("{} ended; expected it to stall open", self.description),
                Ok(Err(status)) => panic!(
                    "{} broke with {status}; expected it to stall open",
                    self.description
                ),
                Err(_) => return, // silent for the whole window
            }
        }
    }
}

/// Handle to a short-lived cloud JWT minted by
/// [`Daemon::reattach_cloud_with_expiring_jwt`].
pub struct ExpiringJwt {
    daemon: Daemon,
    expires_at: std::time::SystemTime,
}

impl ExpiringJwt {
    /// Waits (bounded by the assertion timeout) until the initial token's
    /// expiry moment has passed on the wall clock.
    pub async fn expired(&self) {
        let assertion = format!(
            "the initial cloud JWT of '{}' reaches its expiry",
            self.daemon.name()
        );
        eventually(
            &assertion,
            async || std::time::SystemTime::now() >= self.expires_at,
            self.daemon.failure_dump(),
        )
        .await;
    }
}

/// TTL for the tokens [`RegistryTokenRefresher`] mints: long enough that the
/// connector schedules no further refresh within a test's lifetime.
const REFRESHED_JWT_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// The testnet stand-in for the production cloud-token refresher: mints a
/// fresh long-lived bearer token and registers it at the relay, exactly the
/// observable shape of fetching a new JWT from the cloud API.
struct RegistryTokenRefresher {
    tokens: TokenRegistry,
    user_id: uuid::Uuid,
}

#[tonic::async_trait]
impl LinkConnectorTokenRefresher for RegistryTokenRefresher {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        let token = format!("jwt-refreshed-{}", uuid::Uuid::new_v4().simple());
        let expires_at = std::time::SystemTime::now() + REFRESHED_JWT_TTL;
        self.tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(
                token.clone(),
                RegisteredToken {
                    user_id: self.user_id,
                    ttl: REFRESHED_JWT_TTL,
                },
            );
        Ok(LinkConnectorToken { token, expires_at })
    }
}

/// Pending route-shape assertion created by [`Daemon::connects_to`].
pub struct RouteAssertion<'a> {
    from: &'a Daemon,
    to: &'a Daemon,
}

impl RouteAssertion<'_> {
    /// The route is a direct link to the peer.
    pub async fn via_direct(self) {
        let assertion = format!(
            "'{}' connects to '{}' via a direct link",
            self.from.name(),
            self.to.name()
        );
        let to_id = self.to.host_id();
        eventually(
            &assertion,
            async || matches!(self.from.route_to(to_id).await, Some(route) if route.is_direct()),
            self.from.failure_dump(),
        )
        .await;
    }

    /// The route goes through `relay` (an adjacent node claiming adjacency
    /// to the peer).
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
            async || self.from.route_to(to_id).await == Some(Route::Via(relay_id)),
            self.from.failure_dump(),
        )
        .await;
    }
}
