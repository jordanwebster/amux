//! A whole in-process daemon and its observation/assertion surface.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, Weak};

use chrono::{DateTime, TimeDelta, Utc};
use tokio::sync::Mutex;

use super::NetInner;
use super::assertions::eventually;
use super::net::{RegisteredToken, TokenRegistry, UserTierRegistry};
use crate::HostId;
use crate::auth::{AccessToken, AuthError, CredentialProvider};
use crate::client::Client;
use crate::connection::ConnectionManager;
use crate::discovery::{Discovery, ScriptedDiscovery};
use crate::identity::{device_key_path, load_or_create_device_identity_in};
use crate::link::{CarrierKind, ChannelPool, MuxCarrier, MuxRole, QuicCarrier};
use crate::profile::runtime::{
    self, CloudFixtureAuth, Listeners, ProfileRuntime, ProfileRuntimeOptions, RuntimeFixtures,
};
use crate::protocol::wire;
use crate::routing::{
    HostEntry, HostTrustStatus, HostVia, LinkCarrier, LinkConnectorAuth, LinkConnectorToken,
    LinkConnectorTokenRefresher, Route, RoutingCore,
};
use crate::server::ShutdownReason;
use crate::services::{ClientService, PtyAgentHost};
use crate::trust::{Reachability, SharedTrustStore};

/// Parameters a daemon needs to (re)connect to the testnet cloud relay.
pub(crate) struct CloudAttachment {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
    /// The cloud user this daemon attaches as (the relay's authenticator
    /// maps its bearer token to this user).
    pub(crate) user_id: uuid::Uuid,
    pub(crate) tier: crate::Tier,
    pub(crate) tokens: TokenRegistry,
    pub(crate) user_tiers: UserTierRegistry,
    pub(crate) refresh_interval: Option<std::time::Duration>,
}

/// Marks a testnet daemon with a cloud attachment as account-bound. The
/// fixture supplies its relay token directly, so this provider is only the
/// profile binding that production obtains from the installation supervisor.
struct TestnetCredentials;

#[async_trait::async_trait]
impl CredentialProvider for TestnetCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        Ok(AccessToken {
            bearer: "testnet-profile-binding".into(),
            expires_at: None,
        })
    }

    fn invalidate(&self, _token: &AccessToken) {}
}

impl CloudAttachment {
    fn refreshing_auth(&self) -> LinkConnectorAuth {
        LinkConnectorAuth::with_free_refresh_interval(
            LinkConnectorToken {
                token: self.token.clone(),
                expires_at: std::time::SystemTime::now() + REFRESHED_JWT_TTL,
                tier: self.tier,
            },
            Arc::new(RegistryTokenRefresher {
                tokens: self.tokens.clone(),
                user_tiers: self.user_tiers.clone(),
                user_id: self.user_id,
            }),
            Some(
                self.refresh_interval
                    .unwrap_or(crate::services::FREE_TIER_REFRESH_INTERVAL),
            ),
        )
    }
}

pub(crate) struct DaemonInner {
    pub(crate) name: String,
    pub(crate) host_id: HostId,
    pub(crate) data_dir: PathBuf,
    pub(crate) artifact_clock: Arc<TestArtifactClock>,
    /// Direct QUIC listener address; stable across restarts so stored
    /// reachabilities keep working. `None` for cloud-only daemons.
    pub(crate) direct_addr: Option<SocketAddr>,
    pub(crate) cloud: Option<CloudAttachment>,
    pub(crate) runtime: Mutex<Option<DaemonRuntime>>,
    pub(crate) installation: Option<super::installation::ProfileOwner>,
}

pub(crate) struct TestArtifactClock(StdMutex<DateTime<Utc>>);

impl TestArtifactClock {
    pub(crate) fn new() -> Self {
        Self(StdMutex::new(Utc::now()))
    }

    fn advance(&self, duration: std::time::Duration) {
        let delta = TimeDelta::from_std(duration).expect("test artifact time must fit chrono");
        let mut now = self.0.lock().unwrap_or_else(|error| error.into_inner());
        *now += delta;
    }
}

impl amux_artifacts::Clock for TestArtifactClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap_or_else(|error| error.into_inner())
    }
}

pub(crate) struct DaemonRuntime {
    profile: Option<ProfileRuntime>,
}

impl std::ops::Deref for DaemonRuntime {
    type Target = ProfileRuntime;
    fn deref(&self) -> &Self::Target {
        self.profile.as_ref().expect("daemon runtime is running")
    }
}

impl DaemonRuntime {
    pub(crate) async fn spawn_cloud_connector(&mut self, inner: &DaemonInner) {
        let cloud = inner.cloud.as_ref().expect("daemon has cloud attachment");
        self.profile
            .as_mut()
            .unwrap()
            .set_test_cloud_auth(CloudFixtureAuth::Refreshing(cloud.refreshing_auth()))
            .await;
        self.start_cloud().await.expect("start test cloud");
    }

    pub(crate) async fn spawn_cloud_connector_with_auth(&mut self, auth: LinkConnectorAuth) {
        self.profile
            .as_mut()
            .unwrap()
            .set_test_cloud_auth(CloudFixtureAuth::Refreshing(auth))
            .await;
        self.start_cloud().await.expect("start test cloud");
    }

    async fn stop(mut self) {
        self.profile
            .take()
            .unwrap()
            .stop(ShutdownReason::UserRequested)
            .await;
    }
}

impl Drop for DaemonRuntime {
    fn drop(&mut self) {
        if let Some(profile) = self.profile.take() {
            tokio::spawn(profile.stop(ShutdownReason::UserRequested));
        }
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
/// pre-bound direct-QUIC socket on first boot (restarts rebind the
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
    listener: Option<std::net::UdpSocket>,
    discovery: ScriptedDiscovery,
) -> DaemonRuntime {
    let listener = match (listener, inner.direct_addr) {
        (Some(listener), _) => Some(listener),
        (None, Some(addr)) => Some(bind_udp_with_retries(addr).await),
        (None, None) => None,
    };
    let config = crate::config::Config {
        host_name: inner.name.clone(),
        socket_path: inner.data_dir.join("amux.sock"),
        state_path: inner.data_dir.join("state.yaml"),
        data_dir: inner.data_dir.clone(),
        lan: crate::config::LanConfig {
            listen: inner.direct_addr.is_some(),
            port: inner.direct_addr.map_or(0, |addr| addr.port()),
        },

        prevent_idle_sleep: Some(false),
        ..crate::config::Config::default()
    };
    let mut options = ProfileRuntimeOptions::from_legacy_config(
        config,
        inner
            .cloud
            .as_ref()
            .map(|_| Arc::new(TestnetCredentials) as Arc<dyn CredentialProvider>),
        None,
        Listeners::InProcessOnly,
        Arc::new(discovery) as Arc<dyn Discovery>,
    );
    options.fixtures = RuntimeFixtures {
        listener,
        discovery: None,
        artifact_clock: Some(inner.artifact_clock.clone()),
        cloud_transport: None,
        cloud_refresh_interval: None,
        cloud: inner.cloud.as_ref().map(|cloud| {
            (
                cloud.addr,
                CloudFixtureAuth::Refreshing(cloud.refreshing_auth()),
            )
        }),
    };
    let profile = runtime::start(options)
        .await
        .unwrap_or_else(|error| panic!("start daemon '{}': {error}", inner.name));
    DaemonRuntime {
        profile: Some(profile),
    }
}

async fn bind_udp_with_retries(addr: SocketAddr) -> std::net::UdpSocket {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::net::UdpSocket::bind(addr) {
            Ok(socket) => return socket,
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("failed to rebind QUIC socket {addr}: {error}");
                }
                tokio::time::sleep(super::assertions::POLL_INTERVAL).await;
            }
        }
    }
}

/// Waits (bounded by [`RESTART_DIRECT_LINK_GRACE`]) for every peer with a
/// stored direct reachability to become routable again. Best effort:
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
                        .any(|reachability| matches!(reachability, Reachability::Direct { .. }))
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
    pub(crate) channels: Arc<ChannelPool>,
    pub(crate) trust: SharedTrustStore,
}

/// Handle to one daemon in a [`super::TestNet`]. Cheap to clone.
#[derive(Clone)]
pub struct Daemon {
    pub(crate) inner: Arc<DaemonInner>,
    pub(crate) net: Weak<NetInner>,
}

pub(crate) enum RuntimeGuard<'a> {
    Daemon(tokio::sync::MutexGuard<'a, Option<DaemonRuntime>>),
    Profile(Option<tokio::sync::OwnedMutexGuard<Option<ProfileRuntime>>>),
}
impl RuntimeGuard<'_> {
    pub(crate) fn as_ref(&self) -> Option<&ProfileRuntime> {
        match self {
            Self::Daemon(guard) => guard.as_ref().map(|runtime| &**runtime),
            Self::Profile(guard) => guard.as_ref().and_then(|runtime| runtime.as_ref()),
        }
    }
}

impl Daemon {
    pub fn direct_addr(&self) -> SocketAddr {
        self.inner
            .direct_addr
            .expect("daemon does not expose a direct listener")
    }

    pub(crate) async fn runtime(&self) -> RuntimeGuard<'_> {
        if let Some(owner) = &self.inner.installation {
            RuntimeGuard::Profile(owner.runtime().await)
        } else {
            RuntimeGuard::Daemon(self.inner.runtime.lock().await)
        }
    }
    /// A cloud tenant supplies neither a host inventory entry, a pairing
    /// candidate, a claim, nor any active or standby route to this device.
    pub async fn cloud_isolated_from(&self, other: &Daemon) {
        let parts = self.try_parts().await.expect("profile is running");
        super::assertions::consistently_for(
            &format!("{} has no tenant state for {}", self.name(), other.name()),
            std::time::Duration::from_millis(250),
            async || {
                !self.host_table().await.iter().any(|host| host.id == other.host_id())
                    && !self.pairing_candidates().await.contains(&other.host_id())
                    && parts.routing.routes_to(other.host_id()).await.is_empty()
                    && parts.connections.known_routes(other.host_id()).await.is_empty()
                    && !parts.routing.routing_events_snapshot().await.iter().any(|event| {
                        matches!(event, crate::routing::RoutingEvent::ClaimUp { host, .. } if host.id == other.host_id())
                    })
            },
            self.failure_dump(),
        ).await;
    }

    /// Open native streams on this profile's authenticated relay link,
    /// bypassing the local route lookup. A same-tenant control must accept
    /// its stream; the foreign tenant must remain unreachable.
    pub async fn cloud_cannot_forward_to(&self, other: &Daemon, control: &Daemon) {
        let parts = self.try_parts().await.expect("profile is running");
        let carrier = parts
            .channels
            .link_registry()
            .cloud_relay_carrier()
            .await
            .expect("profile has a cloud relay carrier");
        let control_stream = carrier
            .open_stream(wire::pb::StreamPreface {
                dst: control.host_id().as_bytes().to_vec(),
            })
            .await
            .expect("same-tenant relay stream is accepted");
        drop(control_stream);

        let refused = match carrier
            .open_stream(wire::pb::StreamPreface {
                dst: other.host_id().as_bytes().to_vec(),
            })
            .await
        {
            Ok(_) => panic!("cross-tenant relay stream must be refused"),
            Err(refused) => refused,
        };
        assert!(
            matches!(
                refused,
                crate::link::OpenError::Refused(wire::pb::StreamRefusal::NoRoute)
            ),
            "cross-tenant relay stream was not refused as NO_ROUTE: {refused}"
        );
    }

    /// Dial a known address and pin the responder locally so failure must
    /// come from the responder rejecting this device's key, not a missing route.
    pub async fn cannot_authenticate_to(&self, other: &Daemon) {
        let identity = load_or_create_device_identity_in(&self.inner.data_dir).unwrap();
        let (_, pubkey) = other.identity_on_disk();
        let mut trust = crate::trust::TrustStore::default();
        trust.insert_for_test(
            other.host_id(),
            crate::trust::TrustEntry {
                pubkey,
                name: other.name().into(),
                paired_at: Utc::now(),
                reachabilities: vec![],
                signed_in: None,
            },
        );
        let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let result = tokio::time::timeout(
            super::assertions::DEFAULT_TIMEOUT,
            QuicCarrier::connect_direct(
                &endpoint,
                other.direct_addr(),
                &identity,
                Arc::new(std::sync::RwLock::new(trust)),
                other.host_id(),
            ),
        )
        .await
        .expect("device authentication must finish with a refusal");
        assert!(
            result.is_err(),
            "{} authenticated into {} without a pin",
            self.name(),
            other.name()
        );
    }

    /// The daemon's builder-declared name (e.g. `"laptop"`).
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The daemon's persistent host id (stable across restarts).
    pub fn host_id(&self) -> HostId {
        self.inner.host_id
    }

    /// Advances this daemon's injected artifact clock without sleeping.
    pub fn advance_artifact_time(&self, duration: std::time::Duration) {
        self.inner.artifact_clock.advance(duration);
    }

    /// Runs the same loaded-owner sweep used by the daemon background task.
    pub async fn sweep_artifacts(&self) -> Vec<crate::ArtifactId> {
        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        runtime
            .services
            .artifact_owners
            .sweep_loaded(amux_artifacts::EPHEMERAL_TTL)
            .unwrap_or_else(|error| panic!("'{}' failed to sweep artifacts: {error}", self.name()))
    }

    /// Reports whether an agent's authoritative artifact root still exists.
    pub fn artifact_root_exists(&self, agent_id: crate::AgentId) -> bool {
        self.inner
            .data_dir
            .join("agents")
            .join(agent_id.to_string())
            .join("artifacts")
            .exists()
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

    /// Exact live cloud links, including their connection identities. This
    /// distinguishes keeping a link from silently replacing it during a refusal.
    pub async fn cloud_link_ids(&self) -> Vec<String> {
        self.try_parts()
            .await
            .unwrap()
            .channels
            .link_registry()
            .cloud_link_ids()
            .await
    }

    /// Agent identities whose live session channels currently ride the link
    /// from this daemon to `other`.
    pub async fn active_session_streams_to(&self, other: &Daemon) -> Vec<crate::AgentId> {
        self.try_parts()
            .await
            .expect("profile is running")
            .channels
            .active_session_agents(other.host_id())
    }

    /// Waits until a bulk channel is live on the link from this daemon to
    /// `other`, without introducing timing sleeps into a protocol spec.
    pub async fn expects_active_bulk_stream_to(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' has an active bulk stream to '{}'",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || {
                self.try_parts()
                    .await
                    .is_some_and(|parts| parts.channels.active_bulk_streams(other.host_id()) > 0)
            },
            self.failure_dump(),
        )
        .await;
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

    /// Performs one settled routed call and returns its structured protocol
    /// refusal. This is for specs that distinguish policy from reachability.
    pub async fn refused_call_error(&self, other: &Daemon) -> crate::ProtocolError {
        let error = match self.lists_agents_on(other).await {
            Ok(_) => panic!(
                "expected routed call from '{}' to '{}' to be refused",
                self.name(),
                other.name()
            ),
            Err(error) => error,
        };
        if let Some(status) = error.downcast_ref::<tonic::Status>()
            && let Some(error) = crate::protocol::protocol_error_from_status_details(status)
        {
            return error;
        }
        if let Some(crate::link::ChannelError::Refused(refusal)) =
            error.downcast_ref::<crate::link::ChannelError>()
        {
            return match refusal {
                wire::pb::StreamRefusal::PaymentRequired => crate::ProtocolError::PaymentRequired,
                wire::pb::StreamRefusal::RateLimited => crate::ProtocolError::ResourceExhausted {
                    message: "relay stream rate limit reached".into(),
                },
                refusal => panic!(
                    "routed call from '{}' to '{}' got unexpected stream refusal {refusal:?}",
                    self.name(),
                    other.name()
                ),
            };
        }
        panic!(
            "routed call from '{}' to '{}' failed without a structured protocol refusal: {error:#}",
            self.name(),
            other.name()
        );
    }

    /// Asserts that no endpoint tunnel to `other` survives a refusal.
    pub async fn has_no_active_tunnel_to(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' has no active tunnel to '{}'",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || {
                let Some(parts) = self.try_parts().await else {
                    return true;
                };
                !parts
                    .channels
                    .debug_view()
                    .iter()
                    .any(|channel| channel.peer == other.host_id())
            },
            self.failure_dump(),
        )
        .await;
    }

    /// The pairing inventory behind the installation's administrative surface.
    pub async fn pairing_candidates(&self) -> Vec<HostId> {
        self.pairing_candidate_details()
            .await
            .into_iter()
            .map(|candidate| candidate.host.id)
            .collect()
    }

    /// Pairing candidates including their selected route and direct addresses.
    pub async fn pairing_candidate_details(&self) -> Vec<crate::PairingCandidate> {
        match &self.inner.installation {
            Some(owner) => owner
                .admin_client()
                .list_pairing_hosts()
                .await
                .expect("pairing inventory"),
            None => {
                self.try_parts()
                    .await
                    .expect("daemon is running")
                    .client
                    .list_pairing_candidates()
                    .await
            }
        }
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

    /// Waits until this daemon has consumed a discovery address for `other`.
    pub async fn sees_found_address_for(&self, other: &Daemon) {
        let assertion = format!(
            "'{}' records a found address for '{}'",
            self.name(),
            other.name()
        );
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || {
                let runtime = self.runtime().await;
                runtime.as_ref().is_some_and(|runtime| {
                    !runtime
                        .services
                        .reachability_link_connector()
                        .found_addrs(other_id)
                        .is_empty()
                })
            },
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
                let host_entry_is_safe = self
                    .host_table()
                    .await
                    .into_iter()
                    .find(|host| host.id == other_id)
                    .is_none_or(|entry| {
                        entry.trust_status == HostTrustStatus::UntrustedButOnline
                            && entry.last_dial_error.is_none()
                    });

                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                let no_tunnel = !parts
                    .channels
                    .debug_view()
                    .into_iter()
                    .any(|channel| channel.peer == other_id);
                host_entry_is_safe && no_tunnel
            },
            self.failure_dump(),
        )
        .await;
    }

    /// The host inventory served to a paired remote caller.
    pub async fn list_hosts_on(&self, other: &Daemon) -> anyhow::Result<Vec<HostId>> {
        let request = async {
            let Some(parts) = self.try_parts().await else {
                anyhow::bail!("daemon '{}' is not running", self.name());
            };
            let channel = parts.connections.channel_to(other.host_id()).await?;
            let mut client = wire::client_service_client(channel);
            let hosts = client
                .list_hosts(wire::ListHostsRequest {})
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

    /// Stops the production runtime cooperatively and waits until every
    /// other daemon observes it offline. No transport severing is involved.
    ///
    /// The runtime lock is released before the wait: holding it across
    /// `wait_until_peers_see_us_down` would deadlock the failure dump,
    /// which queries this daemon's host table through the same lock.
    pub async fn stop(&self) {
        assert!(
            self.inner.installation.is_none(),
            "stop profiles through their installation"
        );
        let runtime = self.inner.runtime.lock().await.take();
        if let Some(runtime) = runtime {
            runtime.stop().await;
        }
        self.wait_until_peers_see_us_down().await;
    }

    /// Simulates an abrupt direct-transport outage without a graceful link close.
    pub async fn sever_direct_connections(&self) {
        if let Some(parts) = self.try_parts().await {
            parts.channels.link_registry().close_peer_links().await;
        }
    }

    /// Stop and restart with the same data dir; identity, trust, and the
    /// direct QUIC listener address all persist. Direct links are
    /// re-established from stored reachabilities before the cloud is
    /// reattached (see [`start_daemon_runtime`]).
    pub async fn restart(&self) {
        // Stop first so the old runtime's tasks abort and the QUIC socket
        // port is released before the new runtime rebinds it.
        self.stop().await;
        let discovery = self.net.upgrade().unwrap().discovery.clone();
        let mut runtime = start_daemon_runtime(&self.inner, None, discovery).await;
        if self.inner.cloud.is_some() {
            wait_for_stored_direct_peers(&runtime).await;
            runtime.spawn_cloud_connector(&self.inner).await;
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
    pub(crate) async fn wait_until_peers_see_us_down(&self) {
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
        let discovery = self.net.upgrade().unwrap().discovery.clone();
        let mut runtime = start_daemon_runtime(&self.inner, None, discovery).await;
        if self.inner.cloud.is_some() {
            runtime.spawn_cloud_connector(&self.inner).await;
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

    /// Connects to this daemon's ordinary agent and host service.
    pub(crate) async fn admin_client(&self) -> Client {
        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        runtime.client()
    }

    pub(crate) async fn pairing_admin(&self) -> crate::installation::ProfileAdmin {
        if let Some(owner) = &self.inner.installation {
            return owner.installation_admin().await;
        }
        crate::installation::ProfileAdmin::for_test(
            self.try_parts().await.expect("daemon is running").client,
        )
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
        let guard = self.runtime().await;
        let runtime = guard.as_ref()?;
        Some(DaemonParts {
            client: runtime.services.client.clone(),
            agent_host: runtime.test_agent_host.clone(),
            connections: runtime.services.connections.clone(),
            routing: runtime.services.routing.clone(),
            channels: runtime.services.channels.clone(),
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

    /// Waits until the public ListHosts boundary reports the selected route
    /// and last announced account-binding fact for `other`.
    pub async fn sees_host_status(&self, other: &Daemon, via: HostVia, signed_in: Option<bool>) {
        let assertion = format!(
            "'{}' reports '{}' via {via:?} with signed_in={signed_in:?}",
            self.name(),
            other.name()
        );
        let other_id = other.host_id();
        eventually(
            &assertion,
            async || {
                let client = {
                    let runtime = self.runtime().await;
                    runtime.as_ref().map(ProfileRuntime::client)
                };
                let Some(client) = client else {
                    return false;
                };
                client.list_hosts().await.ok().is_some_and(|hosts| {
                    hosts.into_iter().any(|host| {
                        host.id == other_id && host.via == via && host.signed_in == signed_in
                    })
                })
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Opens the production link protocol over an in-memory stream standing
    /// in for SSH stdio.
    pub async fn connect_via_ssh_fixture(&self, other: &Daemon) {
        let connector_ctx = {
            let runtime = self.runtime().await;
            let runtime = runtime
                .as_ref()
                .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
            runtime
                .services
                .link_connector_ctx()
                .with_expected_peer(other.host_id())
                .with_carrier(LinkCarrier::Ssh)
        };
        let acceptor_ctx = {
            let runtime = other.runtime().await;
            runtime
                .as_ref()
                .unwrap_or_else(|| panic!("daemon '{}' is not running", other.name()))
                .services
                .link_ctx()
                .with_authenticated_peer(self.host_id())
                .with_carrier(LinkCarrier::Ssh)
        };
        let (stream, accepted) = tokio::io::duplex(1024 * 1024);
        tokio::spawn(async move {
            let carrier = Arc::new(MuxCarrier::new(
                accepted,
                MuxRole::Acceptor,
                CarrierKind::Ssh,
            ));
            let _ =
                crate::link::run_link(acceptor_ctx, carrier, crate::routing::ConnectRole::Acceptor)
                    .await;
        });
        let carrier = Arc::new(MuxCarrier::new(
            stream,
            MuxRole::Connector,
            CarrierKind::Ssh,
        ));
        let (_task, established) =
            crate::routing::spawn_connector_with_establishment(connector_ctx, carrier);
        tokio::time::timeout(super::assertions::DEFAULT_TIMEOUT, established)
            .await
            .expect("SSH fixture link establishment timed out")
            .expect("SSH fixture link task ended before establishment")
            .expect("SSH fixture link was refused");
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
        if self.inner.installation.is_some() {
            if let Some(runtime) = self.runtime().await.as_ref() {
                runtime
                    .start_cloud()
                    .await
                    .expect("start profile cloud connector");
            }
            return;
        }
        if self.inner.cloud.is_none() {
            return;
        }
        if let Some(runtime) = self.inner.runtime.lock().await.as_mut() {
            runtime.spawn_cloud_connector(&self.inner).await;
        }
    }

    /// Stops only this daemon's cloud connector through its cleanup path.
    /// The runtime and every direct socket stay alive.
    pub async fn stop_cloud(&self) {
        if let Some(runtime) = self.runtime().await.as_ref() {
            runtime.stop_cloud().await;
        }
    }

    /// Refreshes this daemon's relay entitlement on its existing cloud link.
    pub async fn refresh_entitlement(&self) -> crate::Tier {
        self.runtime()
            .await
            .as_ref()
            .expect("daemon is not running")
            .refresh_entitlement()
            .await
            .expect("refresh cloud entitlement")
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

        // Stop the current link first; reattaching while it is still up would
        // give the daemon two concurrent cloud links.
        self.stop_cloud().await;
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
            LinkConnectorToken {
                token,
                expires_at,
                tier: attachment.tier,
            },
            Arc::new(RegistryTokenRefresher {
                tokens: cloud_relay.token_registry(),
                user_tiers: cloud_relay.user_tier_registry(),
                user_id: attachment.user_id,
            }),
        );
        {
            let mut guard = self.inner.runtime.lock().await;
            let runtime = guard
                .as_mut()
                .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
            runtime.spawn_cloud_connector_with_auth(auth).await;
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

    /// Looks up a stored direct QUIC reachability for `peer` in this daemon's
    /// trust store.
    pub(crate) async fn direct_reachability_to(&self, peer: HostId) -> Option<Reachability> {
        let parts = self.try_parts().await?;
        let store = parts.trust.read().ok()?;
        store
            .entry(peer)?
            .reachabilities
            .iter()
            .find_map(|reachability| {
                matches!(reachability, Reachability::Direct { .. }).then(|| reachability.clone())
            })
    }

    /// Returns the direct address set currently persisted for `peer`.
    pub async fn stored_direct_addrs_to(&self, peer: HostId) -> Vec<SocketAddr> {
        match self.direct_reachability_to(peer).await {
            Some(Reachability::Direct { addrs }) => addrs,
            _ => Vec::new(),
        }
    }

    /// Spawns a one-shot direct-link establishment attempt toward `peer`,
    /// the same path daemons use for stored reachabilities at startup.
    pub(crate) async fn spawn_direct_link(&self, peer: HostId, reachability: Reachability) {
        let guard = self.runtime().await;
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
                        "  - {} ({}) online={} trust={:?} via={:?} signed_in={:?} last_dial_error={}",
                        host.name,
                        host.id,
                        host.online,
                        host.trust_status,
                        host.via,
                        host.signed_in,
                        host.last_dial_error.as_deref().unwrap_or("none")
                    );
                }
                if let Some(parts) = daemon.try_parts().await {
                    for channel in parts.channels.debug_view() {
                        let _ = writeln!(
                            out,
                            "  channel peer={} route={} class={:?}",
                            channel.peer, channel.route, channel.class
                        );
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
    user_tiers: UserTierRegistry,
    user_id: uuid::Uuid,
}

#[tonic::async_trait]
impl LinkConnectorTokenRefresher for RegistryTokenRefresher {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        let token = format!("jwt-refreshed-{}", uuid::Uuid::new_v4().simple());
        let expires_at = std::time::SystemTime::now() + REFRESHED_JWT_TTL;
        let tier = self
            .user_tiers
            .read()
            .expect("testnet user tier registry poisoned")
            .get(&self.user_id)
            .copied()
            .unwrap_or(crate::Tier::Pro);
        self.tokens
            .write()
            .expect("testnet token registry poisoned")
            .insert(
                token.clone(),
                RegisteredToken {
                    user_id: self.user_id,
                    ttl: REFRESHED_JWT_TTL,
                    tier,
                },
            );
        Ok(LinkConnectorToken {
            token,
            expires_at,
            tier,
        })
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
