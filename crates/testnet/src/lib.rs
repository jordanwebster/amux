//! TestNet: the in-process spec-test harness.
//!
//! [`TestNet`] builds a declared topology of *whole daemons* — real
//! identities, real trust stores, real localhost QUIC with device mTLS, and
//! an optional in-process cloud relay — then hands out [`Daemon`] handles
//! whose verbs are user-meaningful (`sees`, `trusts`, `can_call`, `pair`,
//! `attach`, …). The net itself carries the network-operator verbs
//! (`sever_direct`, `cloud_offline`, …); [`WirePeer`] is the separate
//! scripted protocol actor for wire-conformance tests. See
//! `notes/SPEC_TESTS_DESIGN.md` for the design contract.
//!
//! ```ignore
//! let net = TestNet::builder()
//!     .cloud()
//!     .daemon("laptop")
//!     .daemon("desktop")
//!     .paired("laptop", "desktop", Via::Direct)
//!     .start()
//!     .await;
//! let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
//!
//! laptop.sees(&desktop).await;
//! laptop.connects_to(&desktop).via_direct().await;
//! laptop.lists_agents_on(&desktop).await.unwrap();
//! ```
//!
//! Three disciplines hold throughout:
//!
//! - **Eventually, never sleep.** Every observation verb waits through
//!   [`assertions::eventually`] under one default timeout, using a causal
//!   notification where the daemon exposes one and polling as the fallback.
//!   On expiry it panics with a dump of the declared topology, every daemon's
//!   host table, and the failing daemon's routes. Tests contain no retry loops.
//! - **Restart = complete teardown.** `Daemon::stop`/`restart` await the cloud
//!   connector's cleanup, then sever direct sockets whose detached dispatcher
//!   tasks model process-owned connections. Aborting an established connector
//!   task skips its asynchronous link cleanup and must not be used as stop.
//!   Identity, trust, and the QUIC address persist across restarts.
//! - **Sever = real outage.** `sever_direct`/`cloud_offline` cut sockets (or
//!   close links) hard and return only once the affected daemons have
//!   observed the loss, so follow-up assertions start from a settled net.
//!
//! Assertions and transports keep wall-clock time. Product-policy time moves
//! only when [`TestNet::advance`] says it does.
//!
//! # The served door
//!
//! The same harness runs as a process: `testnet serve --topology <file>`
//! ([`serve`]) starts a declared topology and answers control requests on
//! loopback, which is how phone journeys and other out-of-process drivers use
//! it. One checked capability table keeps the two entry points in the same
//! language, including the few controls that compose operations or add an
//! effect at the process boundary.
//!
//! <!-- control-capabilities:start -->
//! | Door verb (`serve::Control`) | Harness capability |
//! | --- | --- |
//! | `CloudOffline` | `TestNet::cloud_offline` |
//! | `CloudOnline` | `TestNet::cloud_online` |
//! | `SeverDirect` | `TestNet::sever_direct` |
//! | `EstablishDirect` | `TestNet::try_establish_direct` |
//! | `RestartDaemon` | `TestNet::restart_daemon` + `Provider::close` |
//! | `StopDaemon` | `Daemon::stop` |
//! | `RestartSdkDaemon` | `TestNet::restart_daemon` + `Daemon::create_agent` |
//! | `SuspendRestart` | `Daemon::suspend_restart_agents` + `Provider::close` |
//! | `Unpair` | `Daemon::unpair` |
//! | `StartPinPairing` | `Daemon::start_pin_pairing` |
//! | `StartQrPairing` | `Daemon::try_start_qr_pairing` |
//! | `Latency` | `TestNet::relay_latency` |
//! | `Announce` | `TestNet::announce` + host mDNS publication |
//! | `Withdraw` | `TestNet::withdraw` + host mDNS withdrawal |
//! | `Tier` | `TestNet::cloud_user_tier` |
//! | `UdpBlocked` | `TestNet::udp_blocked` |
//! | `AgentEmit` | `script::Provider::emit` |
//! | `AgentPlay` | `script::Provider::play` |
//! | `AgentRaiseAsk` | `script::Provider::raise_ask` |
//! | `AgentEndTurn` | `script::Provider::end_turn` |
//! | `AgentExit` | `script::Provider::exit` |
//! | `AgentSpawnChild` | `Daemon::spawn_child` |
//! | `AgentVerifyReplay` | `Recorded::verify_replay` |
//! | `AgentObserve` | `script::Provider::observe` or `Daemon::observed_sdk_inputs` |
//! | `DebugDump` | `Daemon::debug_dump` |
//! | `Connections` | `Daemon::connections` or `TestNet::connections` |
//! | `Inventory` | `Daemon::inventory` |
//! | `Shutdown` | `TestNet::shutdown` |
//! <!-- control-capabilities:end -->
//!
//! Adding a verb means naming its harness capability in the checked table.

mod assertions;
mod client;
pub use client::{UserClient, connect_user};
mod clock;
pub use clock::DrivenClock;
mod daemon;
/// The fake identity service (token minting, relay assignment) that stands in
/// for amux.sh. It lives in node so node's unit tests and this harness share
/// one implementation; the relay is the other, separate half of the cloud.
pub mod identity {
    pub use node::test_fixtures::*;
}
mod installation;
mod latency;
pub mod relay;
pub use installation::{
    InstallationHandle, Profile, RetainedProfileWork, UpdatePreparationHold, WatchProbe,
};
mod pairing;
pub mod perf;
pub mod script;
pub mod sdk;
pub mod serve;
mod session;
mod sources;
mod ticket;
mod udp_proxy;
mod wire;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use assertions::eventually;
pub use daemon::{BulkTransferHold, Daemon, ExpiringJwt, RouteAssertion, RoutedStream};
use daemon::{CloudAttachment, DaemonInner, start_daemon_runtime};
use node::discovery::{Advertisement, Discovery, DiscoveryEvent, ScriptedDiscovery};
use node::harness::{
    DeviceIdentity, Reachability, TrustEntry, TrustStore, load_or_create_device_identity_in,
};
pub use pairing::{PairAttempt, Pin, QrPayload};
use relay::CloudRelay;
pub use session::EchoSession;
use tokio::sync::Mutex;
use udp_proxy::UdpProxy;
pub use wire::{LinkCloseReason, WirePeer, native_stream_lifecycle};

/// How a pre-paired fixture pair reaches each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// Direct QUIC: the first daemon stores a direct reachability for
    /// the second (mirroring real PIN pairing over QUIC, where only the
    /// initiator learns an address).
    Direct,
    /// Both daemons store a `Cloud` reachability and meet at the relay.
    Cloud,
}

impl From<Via> for serve::PairVia {
    fn from(via: Via) -> Self {
        match via {
            Via::Direct => Self::Direct,
            Via::Cloud => Self::Cloud,
        }
    }
}

impl From<serve::PairVia> for Via {
    fn from(via: serve::PairVia) -> Self {
        match via {
            serve::PairVia::Direct => Self::Direct,
            serve::PairVia::Cloud => Self::Cloud,
        }
    }
}

/// Carrier a fixture daemon uses for its authenticated cloud-relay link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RelayTransport {
    #[default]
    Auto,
    Quic,
    Tcp,
}

/// A running testnet: the daemons and optional cloud relay declared through
/// [`TestNet::builder`], plus the network-operator verbs that disturb them.
/// Dropping the net tears everything down (runtimes, sockets, data dirs).
pub struct TestNet {
    inner: Arc<NetInner>,
}

pub(crate) struct NetInner {
    pub(crate) topology: String,
    pub(crate) daemons: Vec<Daemon>,
    pub(crate) cloud: Option<CloudRelay>,
    /// The fake identity service, when the topology started one.
    identity: Option<Arc<identity::IdentityServer>>,
    installations: Vec<InstallationHandle>,
    pairs: Vec<(String, String, Via)>,
    pub(crate) discovery: ScriptedDiscovery,
    pub(crate) udp_proxy: UdpProxy,
    pub(crate) clock: Arc<DrivenClock>,
    discovery_events: StdMutex<tokio::sync::broadcast::Receiver<DiscoveryEvent>>,
    /// Owns every daemon's data dir; removed when the net is dropped.
    _data_root: tempfile::TempDir,
    /// Owns every standalone daemon's client socket. Sockets live apart from
    /// data because a Unix socket path is limited to about a hundred bytes and
    /// a data dir under macOS's TMPDIR, nested by whichever harness runs the
    /// net, is not.
    _socket_root: tempfile::TempDir,
}

/// How long [`TestNet::cloud_relay_cannot_call`] gives the relay's doomed
/// call attempt before treating "still not completed" as cannot-call.
const RELAY_CALL_ATTEMPT_TIMEOUT: std::time::Duration = assertions::DEFAULT_TIMEOUT;

/// Default cloud identity for topology configuration, matching an installation.
pub fn default_cloud_url() -> String {
    node::harness::Config::default().cloud_url
}

impl TestNet {
    /// Advances product-policy time without changing transport or assertion
    /// deadlines, then wakes policy sleepers whose deadline was crossed.
    pub fn advance(&self, duration: std::time::Duration) {
        self.inner.clock.advance(duration);
    }

    /// Loopback endpoint for clients outside the harness process.
    pub fn relay_addr(&self) -> SocketAddr {
        self.cloud().relay_addr()
    }

    /// The cloud identity configured by this topology, separate from its relay.
    pub fn cloud_url(&self) -> &str {
        &self.cloud().url
    }

    /// Where the fake identity service answers, when this topology started
    /// one. Separate from the relay: the identity service mints tokens and
    /// names a relay; the relay only carries traffic.
    pub fn identity_url(&self) -> Option<String> {
        self.inner.identity.as_ref().map(|identity| identity.url())
    }

    /// Delay relay traffic by `millis` on both TCP and QUIC carriers,
    /// including existing connections. Direct device links and local admin
    /// calls remain unaffected.
    pub fn relay_latency(&self, millis: u64) {
        self.cloud().relay.set_latency(millis);
    }

    /// Restart a daemon and wait for its previously reachable peers to return.
    pub async fn restart_daemon(&self, daemon: &Daemon) {
        let mut peers = Vec::new();
        for peer in &self.inner.daemons {
            if peer.host_id() != daemon.host_id()
                && peer
                    .host_table()
                    .await
                    .iter()
                    .any(|h| h.id == daemon.host_id() && h.online)
            {
                peers.push(peer.clone());
            }
        }
        daemon.restart().await;
        if self.cloud().relay.is_online().await {
            eventually(
                "restarted daemon attaches to the relay",
                async || daemon.has_direct_route_to(self.cloud().relay.host_id).await,
                daemon.failure_dump(),
            )
            .await;
        }
        for peer in peers {
            peer.sees(daemon).await;
            daemon.sees(&peer).await;
            if peer
                .pairing_admin()
                .await
                .get_peer(daemon.host_id())
                .await
                .is_ok()
                && daemon
                    .pairing_admin()
                    .await
                    .get_peer(peer.host_id())
                    .await
                    .is_ok()
            {
                peer.can_call(daemon).await;
            }
        }
    }

    /// Every host connected to the relay under the account `label`, and how
    /// many links each of them holds.
    ///
    /// A phone is one of them: this is where a client that multiplexes its
    /// whole conversation over one connection is told apart from one that
    /// opens a connection per thing it is watching, and where a client that
    /// has gone away stops appearing at all.
    pub async fn connections(&self, label: &str) -> Vec<(node::HostId, usize)> {
        let (user_id, _) = self.user_credentials(label);
        self.cloud().relay.links_for(user_id).await
    }

    pub fn user_credentials(&self, label: &str) -> (uuid::Uuid, String) {
        self.cloud().credentials_for_user(label)
    }

    /// Stops every daemon and the relay before releasing their data directories.
    pub async fn shutdown(self) {
        for daemon in &self.inner.daemons {
            daemon.stop_runtime().await;
            // The executor can outlive this topology. Release its fixture
            // providers once no backend can call them again.
            daemon.inner.sources.clear();
        }
        if let Some(cloud) = &self.inner.cloud {
            cloud.relay.go_offline().await;
        }
    }

    /// Starts declaring a topology; finish with [`TestNetBuilder::start`].
    pub fn builder() -> TestNetBuilder {
        TestNetBuilder::default()
    }

    /// Loads the same declared network used by `testnet serve` and starts its
    /// daemons and fixture pairings in process. Scripted agents remain a
    /// served-door concern; an in-process spec can create the agents it needs
    /// through the ordinary harness verbs.
    pub async fn from_topology(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let topology = serve::Topology::load(path.as_ref())?;
        Ok(TestNetBuilder::from_topology(&topology).start().await)
    }

    pub fn installation(&self, name: &str) -> InstallationHandle {
        self.inner
            .installations
            .iter()
            .find(|handle| handle.name() == name)
            .unwrap_or_else(|| panic!("no installation named '{name}'"))
            .clone()
    }

    /// Looks up a daemon handle by name.
    pub fn daemon(&self, name: &str) -> Daemon {
        self.inner
            .daemons
            .iter()
            .find(|daemon| daemon.name() == name)
            .unwrap_or_else(|| panic!("no daemon named '{name}' in this testnet"))
            .clone()
    }

    /// Looks up several daemon handles at once, for array destructuring:
    /// `let [a, b] = net.daemons(["a", "b"]);`
    pub fn daemons<const N: usize>(&self, names: [&str; N]) -> [Daemon; N] {
        names.map(|name| self.daemon(name))
    }

    /// Emits a resolved advertisement for a daemon on this test network, and
    /// answers with what a device browsing this network would resolve.
    ///
    /// Handing the advertisement back is what lets a device that cannot browse
    /// this network itself — a simulator, whose browser looks at the machine's
    /// real network rather than at this one — be told the same name, claim and
    /// addresses a browser would have found.
    pub fn announce(&self, daemon: &Daemon) -> Advertisement {
        self.announce_as(daemon, daemon.host_id())
    }

    /// Emits an untrusted advertisement whose claimed host id differs from
    /// the listener's identity. This models a LAN spoof at the TLS boundary.
    pub fn announce_as(&self, daemon: &Daemon, claimed_host_id: node::HostId) -> Advertisement {
        let advertisement = Advertisement {
            host_id: claimed_host_id,
            name: daemon.name().to_string(),
            version: node::PROTOCOL_VERSION,
            addrs: vec![
                daemon
                    .inner
                    .direct_addr
                    .expect("cannot announce a profile whose LAN listener is off"),
            ],
        };
        self.inner
            .discovery
            .announce_unchecked(advertisement.clone());
        advertisement
    }

    /// Emits a goodbye for a daemon on this test network.
    pub fn withdraw(&self, daemon: &Daemon) {
        self.inner.discovery.withdraw_host(daemon.host_id());
    }

    /// Drains discovery events observed since the previous call.
    pub fn discovery_events(&self) -> Vec<DiscoveryEvent> {
        let mut receiver = self.inner.discovery_events.lock().unwrap();
        let mut events = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return events,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return events,
            }
        }
    }

    /// Applies symmetric latency to every QUIC datagram on the test LAN. The
    /// relay's own carrier-specific delay is controlled by `relay_latency`.
    pub fn direct_latency(&self, millis: u64) {
        self.inner.udp_proxy.latency(millis);
    }

    /// Drops this percentage of direct QUIC datagrams deterministically.
    pub fn loss(&self, percent: u8) {
        self.inner.udp_proxy.loss(percent);
    }

    /// Blocks or restores every direct QUIC datagram involving `daemon`.
    pub fn udp_blocked(&self, daemon: &Daemon, blocked: bool) {
        self.inner.udp_proxy.blocked(daemon.inner.proxy_id, blocked);
    }

    /// Moves a daemon's client-facing QUIC socket while preserving its proxy address.
    pub async fn rebind_client(&self, daemon: &Daemon) {
        let socket = self.inner.udp_proxy.rebind(daemon.inner.proxy_id);
        let runtime = daemon.runtime().await;
        runtime
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", daemon.name()))
            .rebind_direct_quic(socket)
            .expect("rebind testnet QUIC endpoint");
    }

    /// Rejects this account at the production relay authentication boundary.
    pub fn reject_cloud_user(&self, user: &str, error: Option<node::ProtocolError>) {
        self.cloud()
            .reject_user(user, error.map(::wire::protocol_status));
    }

    /// Changes the entitlement used by tokens minted for `user` after this
    /// call. Existing links keep their admitted tier until their next reauth.
    pub fn cloud_user_tier(&self, user: &str, tier: node::Tier) {
        self.cloud().set_user_tier(user, tier);
    }

    /// Takes the cloud relay down hard: accepted sockets are severed, so
    /// daemons observe a genuine outage. Returns once every attached daemon
    /// has dropped its own relay link. (Multi-hop routes to the relay
    /// learned through peers may linger until their HostDowns propagate.)
    pub async fn cloud_offline(&self) {
        let cloud = self.cloud();
        cloud.relay.go_offline().await;
        // Note: the daemons' connector tasks are left running so they can
        // observe the dead socket and tear their links down themselves.
        for daemon in self.cloud_attached_daemons() {
            let assertion = format!(
                "'{}' drops its link to the offline cloud relay",
                daemon.name()
            );
            eventually(
                &assertion,
                async || !daemon.has_direct_route_to(cloud.relay.host_id).await,
                daemon.failure_dump(),
            )
            .await;
        }
    }

    /// Restarts the cloud relay on the same address and reconnects every
    /// cloud-attached daemon.
    pub async fn cloud_online(&self) {
        let cloud = self.cloud();
        if cloud.relay.is_online().await {
            return;
        }
        cloud.relay.go_online().await;
        for daemon in self.cloud_attached_daemons() {
            daemon.reconnect_cloud().await;
        }
        for daemon in self.cloud_attached_daemons() {
            let assertion = format!("'{}' reattaches to the cloud relay", daemon.name());
            eventually(
                &assertion,
                async || daemon.knows_host(cloud.relay.host_id).await,
                daemon.failure_dump(),
            )
            .await;
        }
    }

    /// Cuts the direct link between `a` and `b`, holds their pairwise UDP path
    /// down against automatic redial, and leaves relay routes unaffected.
    pub async fn sever_direct(&self, a: &Daemon, b: &Daemon) {
        self.inner
            .udp_proxy
            .direct_pair_blocked(a.host_id(), b.host_id(), true);
        for (from, to) in [(a, b), (b, a)] {
            if let Some(parts) = from.try_parts().await {
                parts
                    .channels
                    .link_registry()
                    .close_host(to.host_id())
                    .await;
            }
        }
        for (from, to) in [(a, b), (b, a)] {
            let assertion = format!(
                "'{}' loses its direct route to '{}'",
                from.name(),
                to.name()
            );
            let to_id = to.host_id();
            eventually(
                &assertion,
                async || !matches!(from.route_to(to_id).await, Some(route) if route.is_direct()),
                from.failure_dump(),
            )
            .await;
        }
    }

    /// Brings the direct link between two already-paired daemons (back) up
    /// from the direct reachability stored at pairing time.
    pub async fn establish_direct(&self, a: &Daemon, b: &Daemon) {
        self.try_establish_direct(a, b)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// Establish a direct link, returning an error for missing reachability or trust.
    pub async fn try_establish_direct(&self, a: &Daemon, b: &Daemon) -> anyhow::Result<()> {
        for (from, to) in [(a, b), (b, a)] {
            let parts = from
                .try_parts()
                .await
                .ok_or_else(|| anyhow::anyhow!("daemon is stopped"))?;
            anyhow::ensure!(
                parts.trust.read().unwrap().entry(to.host_id()).is_some(),
                "daemons are not mutually paired"
            );
        }
        self.inner
            .udp_proxy
            .direct_pair_blocked(a.host_id(), b.host_id(), false);
        let mut attempt = None;
        if let Some(reachability) = a.direct_reachability_to(b.host_id()).await {
            attempt = Some((a, b, reachability));
        } else if let Some(reachability) = b.direct_reachability_to(a.host_id()).await {
            attempt = Some((b, a, reachability));
        }
        let Some((from, to, reachability)) = attempt else {
            anyhow::bail!(
                "establish_direct('{}', '{}'): neither trust store holds a direct \
                 reachability; pair them Via::Direct first",
                a.name(),
                b.name()
            );
        };
        from.spawn_direct_link(to.host_id(), reachability).await;
        // Calls ride tunnels and frames flow both ways, so the link is
        // callable — and routable — from both ends once it is up.
        for (gains, over) in [(from, to), (to, from)] {
            let assertion = format!(
                "'{}' gains a direct route to '{}'",
                gains.name(),
                over.name()
            );
            let over_id = over.host_id();
            eventually(
                &assertion,
                async || matches!(gains.route_to(over_id).await, Some(route) if route.is_direct()),
                gains.failure_dump(),
            )
            .await;
        }
        Ok(())
    }

    /// Asserts the cloud relay cannot complete a routed call into `target`,
    /// even though it forwards every byte between cloud peers: the relay has
    /// no device identity and no trust entry, and `target` only accepts
    /// pinned device-mTLS from trusted peers. A call attempt that errors or
    /// never completes both count.
    pub async fn cloud_relay_cannot_call(&self, target: &Daemon) {
        let cloud = self.cloud();
        let user_id = target
            .inner
            .cloud
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not cloud-attached", target.name()))
            .user_id;
        let attempt = tokio::time::timeout(
            RELAY_CALL_ATTEMPT_TIMEOUT,
            cloud.relay.try_call_into(user_id, target.host_id()),
        )
        .await;
        if let Ok(Ok(())) = attempt {
            panic!(
                "the cloud relay completed a routed call into '{}'; it must only \
                 forward opaque bytes",
                target.name()
            );
        }
    }

    /// Waits until the relay has removed `target` from its authenticated
    /// tenant's live links.
    pub async fn cloud_relay_sees_offline(&self, target: &Daemon) {
        let cloud = self.cloud();
        let user_id = target
            .inner
            .cloud
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not cloud-attached", target.name()))
            .user_id;
        let assertion = format!("cloud relay observes '{}' going offline", target.name());
        eventually(
            &assertion,
            async || !cloud.relay.has_link_to(user_id, target.host_id()).await,
            target.failure_dump(),
        )
        .await;
    }

    fn cloud(&self) -> &CloudRelay {
        self.inner
            .cloud
            .as_ref()
            .expect("this testnet was built without .cloud()")
    }

    fn cloud_attached_daemons(&self) -> impl Iterator<Item = &Daemon> {
        self.inner
            .daemons
            .iter()
            .filter(|daemon| daemon.inner.cloud.is_some())
    }

    /// Waits for the declared steady state: every cloud-attached daemon has
    /// its relay link up, every paired couple sees each other online, and
    /// `Via::Direct` pairs route directly.
    async fn wait_for_steady_state(&self) {
        if let Some(cloud) = &self.inner.cloud {
            for daemon in self.cloud_attached_daemons() {
                let assertion = format!("'{}' attaches to the cloud relay", daemon.name());
                eventually(
                    &assertion,
                    async || daemon.knows_host(cloud.relay.host_id).await,
                    daemon.failure_dump(),
                )
                .await;
            }
        }
        for (a, b, via) in self.inner.pairs.clone() {
            let a = self.daemon(&a);
            let b = self.daemon(&b);
            a.sees(&b).await;
            match via {
                Via::Direct => {
                    a.connects_to(&b).via_direct().await;
                    // Any live link is bidirectional at the call layer: the
                    // acceptor records a route back over the inbound link.
                    b.sees(&a).await;
                }
                Via::Cloud => {
                    b.sees(&a).await;
                }
            }
        }
    }
}

impl std::fmt::Debug for TestNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestNet")
            .field("topology", &self.inner.topology)
            .finish()
    }
}

struct DaemonSpec {
    name: String,
    repository_roots: Vec<std::path::PathBuf>,
    cloud_only: bool,
    no_cloud: bool,
    cloud_user: Option<String>,
    cloud_tier: node::Tier,
    cloud_refresh_interval: Option<std::time::Duration>,
    udp_blocked_memory: Option<std::time::Duration>,
    udp_blocked: bool,
    relay_transport: RelayTransport,
}

#[derive(Clone, Copy)]
enum BuilderSelection {
    None,
    Installation(usize),
    Profile { installation: usize, profile: usize },
    Daemon(usize),
}

/// Declares a topology for [`TestNetBuilder::start`]: daemons, an optional
/// cloud relay, and pre-seeded trust (pairing-as-fixture). Obtained from
/// [`TestNet::builder`].
pub struct TestNetBuilder {
    topology: serve::Topology,
    cloud: bool,
    custom_cloud_url: bool,
    identity: bool,
    installations: Vec<installation::InstallationSpec>,
    selection: BuilderSelection,
    daemons: Vec<DaemonSpec>,
    stale_direct_pairs: std::collections::HashSet<(String, String)>,
    trusted: Vec<(String, String)>,
    undiscoverable: std::collections::HashSet<String>,
    direct_idle_timeout: Option<std::time::Duration>,
    errors: Vec<String>,
}

impl Default for TestNetBuilder {
    fn default() -> Self {
        Self {
            topology: serve::Topology::empty(),
            cloud: false,
            custom_cloud_url: false,
            identity: false,
            installations: Vec::new(),
            selection: BuilderSelection::None,
            daemons: Vec::new(),
            stale_direct_pairs: std::collections::HashSet::new(),
            trusted: Vec::new(),
            undiscoverable: std::collections::HashSet::new(),
            direct_idle_timeout: None,
            errors: Vec::new(),
        }
    }
}

impl TestNetBuilder {
    fn from_topology(topology: &serve::Topology) -> Self {
        let mut builder = Self {
            topology: topology.network_declaration(),
            cloud: true,
            custom_cloud_url: !topology.daemons.iter().any(|daemon| daemon.installation),
            identity: true,
            ..Self::default()
        };
        for daemon in &topology.daemons {
            if daemon.installation {
                builder.installations.push(installation::InstallationSpec {
                    name: daemon.name.clone(),
                    persistent: false,
                    front_door: true,
                    embedded: false,
                    profiles: vec![installation::ProfileSpec {
                        name: daemon.name.clone(),
                        cloud_user: daemon.user.clone(),
                        cloud_only: false,
                        repository_roots: daemon.repository_roots.clone(),
                    }],
                });
            } else {
                builder.daemons.push(DaemonSpec {
                    name: daemon.name.clone(),
                    repository_roots: daemon.repository_roots.clone(),
                    cloud_only: false,
                    no_cloud: daemon.user.is_none(),
                    cloud_user: daemon.user.clone(),
                    cloud_tier: daemon
                        .user
                        .as_ref()
                        .and_then(|user| topology.tiers.get(user))
                        .copied()
                        .unwrap_or(node::Tier::Pro),
                    cloud_refresh_interval: None,
                    udp_blocked_memory: None,
                    udp_blocked: false,
                    relay_transport: RelayTransport::Auto,
                });
            }
        }
        for (a, b, via) in &topology.paired {
            if *via == serve::PairVia::Cloud {
                builder.undiscoverable.insert(a.clone());
                builder.undiscoverable.insert(b.clone());
            }
        }
        builder
    }

    fn validate(&self) -> anyhow::Result<()> {
        if !self.errors.is_empty() {
            anyhow::bail!(self.errors.join("; "));
        }
        let mut names = std::collections::HashSet::new();
        for daemon in &self.topology.daemons {
            anyhow::ensure!(
                names.insert(daemon.name.clone()),
                "duplicate daemon '{}'",
                daemon.name
            );
            if daemon.installation {
                names.insert(format!("{0}/{0}", daemon.name));
            }
        }
        for installation in &self.installations {
            for profile in &installation.profiles {
                names.insert(format!("{}/{}", installation.name, profile.name));
            }
        }
        for (a, b, _) in &self.topology.paired {
            anyhow::ensure!(a != b, "cannot pair '{a}' with itself");
            anyhow::ensure!(
                names.contains(a),
                "paired(..) references unknown daemon '{a}'"
            );
            anyhow::ensure!(
                names.contains(b),
                "paired(..) references unknown daemon '{b}'"
            );
        }
        for (a, b) in &self.trusted {
            anyhow::ensure!(a != b, "cannot seed trust between '{a}' and itself");
            anyhow::ensure!(
                names.contains(a),
                "trusted(..) references unknown daemon '{a}'"
            );
            anyhow::ensure!(
                names.contains(b),
                "trusted(..) references unknown daemon '{b}'"
            );
        }
        for name in &self.undiscoverable {
            anyhow::ensure!(
                names.contains(name),
                "outside_discovery(..) references unknown daemon '{name}'"
            );
        }
        Ok(())
    }

    pub fn installation(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            !self.installations.iter().any(|spec| spec.name == name),
            "duplicate installation '{name}'"
        );
        self.installations.push(installation::InstallationSpec {
            name,
            persistent: false,
            front_door: false,
            embedded: false,
            profiles: Vec::new(),
        });
        self.selection = BuilderSelection::Installation(self.installations.len() - 1);
        self
    }

    pub fn profile(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let installation_index = match self.selection {
            BuilderSelection::Installation(index)
            | BuilderSelection::Profile {
                installation: index,
                ..
            } => index,
            _ => {
                self.errors
                    .push(".profile() must follow .installation()".to_string());
                return self;
            }
        };
        let installation = &mut self.installations[installation_index];
        assert!(
            !installation
                .profiles
                .iter()
                .any(|profile| profile.name == name),
            "duplicate profile '{name}'"
        );
        installation.profiles.push(installation::ProfileSpec {
            name,
            cloud_user: None,
            cloud_only: false,
            repository_roots: Vec::new(),
        });
        self.selection = BuilderSelection::Profile {
            installation: installation_index,
            profile: installation.profiles.len() - 1,
        };
        self
    }

    /// Serve the most recent fixture installation through its production socket.
    pub fn front_door(mut self) -> Self {
        if let Some(index) = self.selected_installation_index("front_door") {
            self.installations[index].front_door = true;
        }
        self
    }

    /// Persist the installation in a short temporary root for reopen and lock tests.
    pub fn persistent(mut self) -> Self {
        if let Some(index) = self.selected_installation_index("persistent") {
            self.installations[index].persistent = true;
        }
        self
    }

    /// Runs the selected installation with in-process-only host integration,
    /// matching the lifecycle policy used by the phone bridge.
    pub fn embedded(mut self) -> Self {
        if let Some(index) = self.selected_installation_index("embedded") {
            self.installations[index].embedded = true;
        }
        self
    }

    /// Adds an in-process cloud relay; daemons attach to it by default.
    pub fn cloud(mut self) -> Self {
        self.cloud = true;
        self
    }

    /// Also starts the fake identity service, even when no installation
    /// needs it, so a client outside the harness can be pointed at it.
    pub fn identity(mut self) -> Self {
        self.identity = true;
        self
    }

    /// Configures the cloud identity written to every daemon config. The
    /// cloud still assigns an independent loopback relay address.
    pub fn cloud_url(mut self, url: impl Into<String>) -> Self {
        self.cloud = true;
        self.custom_cloud_url = true;
        self.topology.cloud_url = url.into();
        self
    }

    /// Adds a daemon. Attaches to the cloud when one is declared, unless
    /// [`Self::no_cloud`] follows.
    pub fn daemon(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            !self.daemons.iter().any(|spec| spec.name == name),
            "duplicate daemon name '{name}'"
        );
        self.selection = BuilderSelection::Daemon(self.daemons.len());
        self.daemons.push(DaemonSpec {
            name: name.clone(),
            repository_roots: Vec::new(),
            cloud_only: false,
            no_cloud: false,
            cloud_user: None,
            cloud_tier: node::Tier::Pro,
            cloud_refresh_interval: None,
            udp_blocked_memory: None,
            udp_blocked: false,
            relay_transport: RelayTransport::Auto,
        });
        self.topology.daemons.push(serve::DaemonDecl {
            name,
            user: None,
            repository_roots: Vec::new(),
            installation: false,
            lan: false,
            sdk_script: None,
        });
        self
    }

    /// Declares the directories searched for repositories by the most recent daemon.
    pub fn repository_roots(mut self, roots: Vec<std::path::PathBuf>) -> Self {
        match self.selection {
            BuilderSelection::Profile {
                installation,
                profile,
            } => self.installations[installation].profiles[profile].repository_roots = roots,
            BuilderSelection::Daemon(index) => {
                self.daemons[index].repository_roots = roots.clone();
                self.topology.daemons[index].repository_roots = roots;
            }
            _ => self
                .errors
                .push(".repository_roots() requires a daemon or profile".to_string()),
        }
        self
    }

    /// Marks the most recently added daemon as cloud-only: no direct
    /// transports, all traffic through the relay.
    pub fn cloud_only(mut self) -> Self {
        match self.selection {
            BuilderSelection::Profile {
                installation,
                profile,
            } => self.installations[installation].profiles[profile].cloud_only = true,
            BuilderSelection::Daemon(index) => self.daemons[index].cloud_only = true,
            _ => self
                .errors
                .push(".cloud_only() requires a daemon or profile".to_string()),
        }
        self
    }

    /// Opts the most recently added daemon out of the cloud relay.
    pub fn no_cloud(mut self) -> Self {
        match self.selection {
            BuilderSelection::Profile {
                installation,
                profile,
            } => self.installations[installation].profiles[profile].cloud_user = None,
            BuilderSelection::Daemon(index) => self.daemons[index].no_cloud = true,
            _ => self
                .errors
                .push(".no_cloud() requires a daemon or profile".to_string()),
        }
        self
    }

    /// Attaches the most recently added daemon to the cloud as a *different*
    /// cloud user (by default every daemon shares one account). Cloud
    /// presence is per-user, so daemons of different users meet nothing of
    /// each other at the relay.
    pub fn cloud_user(mut self, user: impl Into<String>) -> Self {
        let user = user.into();
        match self.selection {
            BuilderSelection::Profile {
                installation,
                profile,
            } => self.installations[installation].profiles[profile].cloud_user = Some(user),
            BuilderSelection::Daemon(index) => {
                self.daemons[index].cloud_user = Some(user.clone());
                self.topology.daemons[index].user = Some(user.clone());
                if !self.topology.users.contains(&user) {
                    self.topology.users.push(user);
                }
            }
            _ => self
                .errors
                .push(".cloud_user() requires a daemon or profile".to_string()),
        }
        self
    }

    /// Gives the most recently added daemon a token with this tier. A
    /// non-default tier receives a distinct token, so one account can
    /// exercise mixed-tier live links without changing account identity.
    pub fn cloud_tier(mut self, tier: node::Tier) -> Self {
        match self.selection {
            BuilderSelection::Daemon(index) => self.daemons[index].cloud_tier = tier,
            _ => self
                .errors
                .push(".cloud_tier() requires a standalone daemon".to_string()),
        }
        self
    }

    /// Overrides the free-tier refresh cadence for the selected daemon.
    pub fn cloud_refresh_interval(mut self, interval: std::time::Duration) -> Self {
        match self.selection {
            BuilderSelection::Daemon(index) => {
                self.daemons[index].cloud_refresh_interval = Some(interval);
            }
            _ => self
                .errors
                .push(".cloud_refresh_interval() requires a standalone daemon".to_string()),
        }
        self
    }

    /// Sets how long a direct QUIC connection on this network goes unanswered
    /// before either end declares it dead. Lengthened past an assertion's
    /// patience, a link whose far end vanished stays registered for the whole
    /// test instead of timing out underneath it.
    pub fn direct_idle_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.direct_idle_timeout = Some(timeout);
        self
    }

    /// Shortens how long the selected daemon remembers a UDP-blocked relay
    /// network. Intended for deterministic fallback expiry specifications.
    pub fn udp_blocked_memory(mut self, duration: std::time::Duration) -> Self {
        match self.selection {
            BuilderSelection::Daemon(index) => {
                self.daemons[index].udp_blocked_memory = Some(duration);
            }
            _ => self
                .errors
                .push(".udp_blocked_memory() requires a standalone daemon".to_string()),
        }
        self
    }

    /// Starts the selected daemon with UDP blocked by the test network.
    pub fn udp_blocked(mut self) -> Self {
        match self.selection {
            BuilderSelection::Daemon(index) => self.daemons[index].udp_blocked = true,
            _ => self
                .errors
                .push(".udp_blocked() requires a standalone daemon".to_string()),
        }
        self
    }

    /// Uses a specific carrier for the selected daemon's cloud-relay link.
    pub fn relay_transport(mut self, transport: RelayTransport) -> Self {
        match self.selection {
            BuilderSelection::Daemon(index) => self.daemons[index].relay_transport = transport,
            _ => self
                .errors
                .push(".relay_transport() requires a standalone daemon".to_string()),
        }
        self
    }

    /// Pairing-as-fixture: seeds both trust stores (pubkey, name, and the
    /// reachability implied by `via`) before the daemons start.
    pub fn paired(mut self, a: impl Into<String>, b: impl Into<String>, via: Via) -> Self {
        let a = a.into();
        let b = b.into();
        if via == Via::Cloud {
            self.undiscoverable.insert(a.clone());
            self.undiscoverable.insert(b.clone());
        }
        self.topology.paired.push((a, b, via.into()));
        self
    }

    /// Seeds a deliberately stale stored address while retaining ordinary
    /// discovery, so a spec can prove that a fresh Found address wins.
    pub fn paired_with_stale_direct(mut self, a: impl Into<String>, b: impl Into<String>) -> Self {
        let pair = (a.into(), b.into());
        self.stale_direct_pairs.insert(pair.clone());
        self.topology
            .paired
            .push((pair.0, pair.1, serve::PairVia::Direct));
        self
    }

    /// Trust-as-fixture *without* reachability: seeds both trust stores
    /// (pubkey and name only), so the two can authenticate end to end but
    /// neither knows how to dial the other — any route between them must be
    /// learned through the routing graph (e.g. a chain of links).
    pub fn trusted(mut self, a: impl Into<String>, b: impl Into<String>) -> Self {
        self.trusted.push((a.into(), b.into()));
        self
    }

    /// Seeds trust while placing both daemons outside this test LAN's
    /// discovery domain. Stored direct fixtures can still connect them.
    pub fn trusted_without_discovery(mut self, a: impl Into<String>, b: impl Into<String>) -> Self {
        let a = a.into();
        let b = b.into();
        self.undiscoverable.insert(a.clone());
        self.undiscoverable.insert(b.clone());
        self.trusted.push((a, b));
        self
    }

    /// Places one daemon outside the scripted LAN discovery domain without
    /// changing its trust or stored reachabilities.
    pub fn outside_discovery(mut self, name: impl Into<String>) -> Self {
        self.undiscoverable.insert(name.into());
        self
    }

    fn selected_installation_index(&mut self, verb: &str) -> Option<usize> {
        match self.selection {
            BuilderSelection::Installation(index)
            | BuilderSelection::Profile {
                installation: index,
                ..
            } => Some(index),
            _ => {
                self.errors
                    .push(format!(".{verb}() must follow .installation(..)"));
                None
            }
        }
    }

    /// Starts the declared topology and waits for its steady state.
    pub async fn start(self) -> TestNet {
        self.validate()
            .unwrap_or_else(|error| panic!("invalid test topology: {error}"));
        let runtime_name = |name: &str| {
            self.topology
                .daemons
                .iter()
                .find(|daemon| daemon.name == name && daemon.installation)
                .map_or_else(|| name.to_string(), |_| format!("{name}/{name}"))
        };
        let pairs = self
            .topology
            .paired
            .iter()
            .map(|(a, b, via)| (runtime_name(a), runtime_name(b), (*via).into()))
            .collect::<Vec<_>>();
        let (profile_pairs, daemon_pairs): (Vec<_>, Vec<_>) = pairs
            .into_iter()
            .partition(|(a, b, _)| a.contains('/') || b.contains('/'));
        let data_root = tempfile::Builder::new()
            .prefix("amux-spec")
            .tempdir()
            .expect("create testnet data root");
        let socket_root = crate::identity::short_installation_root();
        let discovery = ScriptedDiscovery::new();
        let clock = Arc::new(DrivenClock::new());
        let discovery_events = discovery.browse();
        let udp_proxy = self
            .direct_idle_timeout
            .map_or_else(UdpProxy::new, UdpProxy::with_idle_timeout);

        let mut cloud = if self.cloud {
            Some(
                CloudRelay::start_with_url_and_clock(
                    self.topology.cloud_url.clone(),
                    clock.clone(),
                )
                .await,
            )
        } else {
            None
        };
        let cloud_quic_addr = cloud.as_ref().map(|cloud| {
            udp_proxy.route_to_with_latency(cloud.relay_addr(), cloud.latency_control())
        });

        let identity = if self.installations.is_empty() && !self.identity {
            None
        } else {
            let mut users = std::collections::BTreeSet::new();
            for installation in &self.installations {
                for profile in &installation.profiles {
                    if let Some(user) = &profile.cloud_user {
                        users.insert(user.clone());
                        let cloud = cloud.as_ref().expect("cloud_user requires .cloud()");
                        let _ = cloud.register_user(user);
                    }
                }
            }
            if users.is_empty() {
                users.insert("default".into());
            }
            let identity = Arc::new(
                identity::IdentityServer::start_with_clock(
                    users
                        .into_iter()
                        .map(|sub| identity::TestAccount {
                            name: Some(format!("{sub} Example")),
                            email: Some(format!("{sub}@example.test")),
                            sub,
                            tier: node::Tier::Pro,
                        })
                        .collect(),
                    cloud.as_ref().map(|cloud| cloud.relay_addr()),
                    clock.clone(),
                )
                .await,
            );
            // Installation profiles bind through the identity fixture, so
            // the cloud they name must be that fixture. A standalone-daemon
            // topology keeps the cloud it declared; the fixture is then only
            // an address a client outside the harness may sign in to.
            if !self.installations.is_empty()
                && let Some(cloud) = &mut cloud
            {
                assert!(
                    !self.custom_cloud_url,
                    "installation topologies use their identity fixture URL"
                );
                cloud.url = identity.url();
            }
            Some(identity)
        };

        // Identities and direct-QUIC sockets first, so trust seeding can
        // reference peer pubkeys and listener addresses.
        let mut preps = Vec::with_capacity(self.daemons.len());
        for spec in &self.daemons {
            assert!(
                !(spec.cloud_only && spec.no_cloud),
                "daemon '{}' cannot be both cloud_only and no_cloud",
                spec.name
            );
            assert!(
                !spec.cloud_only || self.cloud,
                "daemon '{}' is cloud_only but the topology has no cloud",
                spec.name
            );
            assert!(
                spec.cloud_user.is_none() || (self.cloud && !spec.no_cloud),
                "daemon '{}' has a cloud_user but does not attach to a cloud",
                spec.name
            );
            let data_dir = data_root.path().join(&spec.name);
            std::fs::create_dir_all(&data_dir).expect("create daemon data dir");
            let identity = load_or_create_device_identity_in(&data_dir)
                .unwrap_or_else(|error| panic!("create identity for '{}': {error}", spec.name));
            let binding = udp_proxy.register(identity.host_id);
            let (listener, quic_client_socket, direct_addr) = if spec.cloud_only {
                (None, Some(binding.socket), None)
            } else {
                (Some(binding.socket), None, Some(binding.public_addr))
            };
            if spec.udp_blocked {
                udp_proxy.blocked(identity.host_id, true);
            }
            preps.push(DaemonPrep {
                identity,
                data_dir,
                listener,
                quic_client_socket,
                direct_addr,
                trust: TrustStore::default(),
                attaches_to_cloud: self.cloud && !spec.no_cloud,
            });
        }

        for (a, b, via) in &daemon_pairs {
            let ia = index_of(&self.daemons, a);
            let ib = index_of(&self.daemons, b);
            assert_ne!(ia, ib, "cannot pair '{a}' with itself");
            let host_id_a = preps[ia].identity.host_id;
            let host_id_b = preps[ib].identity.host_id;
            let (reach_a_to_b, reach_b_to_a) = match via {
                Via::Direct => {
                    let actual_addr_b = preps[ib].direct_addr.unwrap_or_else(|| {
                        panic!(
                            "paired('{a}', '{b}', Via::Direct) requires '{b}' to expose a \
                             direct QUIC listener, but it is cloud_only"
                        )
                    });
                    let addr_b = if self.stale_direct_pairs.contains(&(a.clone(), b.clone())) {
                        "127.0.0.1:9".parse().unwrap()
                    } else {
                        actual_addr_b
                    };
                    // Mirrors real PIN pairing over QUIC: only the initiator
                    // stores the peer's address.
                    (
                        vec![Reachability::Direct {
                            addrs: vec![addr_b],
                        }],
                        Vec::new(),
                    )
                }
                Via::Cloud => {
                    assert!(
                        preps[ia].attaches_to_cloud && preps[ib].attaches_to_cloud,
                        "paired('{a}', '{b}', Via::Cloud) requires both daemons to attach \
                         to a declared cloud"
                    );
                    (vec![Reachability::Cloud], vec![Reachability::Cloud])
                }
            };
            let entry_b = trust_entry(&preps[ib].identity, b, reach_a_to_b);
            let entry_a = trust_entry(&preps[ia].identity, a, reach_b_to_a);
            preps[ia].trust.insert_for_test(host_id_b, entry_b);
            preps[ib].trust.insert_for_test(host_id_a, entry_a);
        }

        for (a, b) in &self.trusted {
            let ia = index_of(&self.daemons, a);
            let ib = index_of(&self.daemons, b);
            assert_ne!(ia, ib, "cannot seed trust between '{a}' and itself");
            let host_id_a = preps[ia].identity.host_id;
            let host_id_b = preps[ib].identity.host_id;
            let entry_b = trust_entry(&preps[ib].identity, b, Vec::new());
            let entry_a = trust_entry(&preps[ia].identity, a, Vec::new());
            preps[ia].trust.insert_for_test(host_id_b, entry_b);
            preps[ib].trust.insert_for_test(host_id_a, entry_a);
        }

        for name in &self.undiscoverable {
            if let Some(index) = self.daemons.iter().position(|spec| &spec.name == name) {
                discovery.suppress(preps[index].identity.host_id);
            }
        }

        let mut daemon_inners = Vec::with_capacity(preps.len());
        for (spec, prep) in self.daemons.iter().zip(preps) {
            prep.trust
                .save_in(&prep.data_dir)
                .unwrap_or_else(|error| panic!("seed trust store for '{}': {error}", spec.name));
            let inner = Arc::new(DaemonInner {
                name: spec.name.clone(),
                host_id: prep.identity.host_id,
                data_dir: prep.data_dir,
                socket_path: socket_root.path().join(format!("{}.sock", spec.name)),
                repository_roots: spec.repository_roots.clone(),
                artifact_clock: Arc::new(daemon::TestArtifactClock::new()),
                clock: clock.clone(),
                direct_addr: prep.direct_addr,
                proxy_id: prep.identity.host_id,
                udp_proxy: udp_proxy.clone(),
                cloud: prep.attaches_to_cloud.then(|| {
                    let cloud = cloud.as_ref().expect("cloud attachment without cloud");
                    let (user_id, shared_token) = match &spec.cloud_user {
                        Some(label) => cloud.credentials_for_user(label),
                        None => (cloud.default_user_id(), cloud.token.clone()),
                    };
                    let token = if spec.cloud_tier == node::Tier::Pro {
                        shared_token
                    } else {
                        let token =
                            format!("spec-token-{}-{}", spec.name, uuid::Uuid::new_v4().simple());
                        cloud.register_token_with_tier(
                            &token,
                            user_id,
                            std::time::Duration::from_secs(3600),
                            spec.cloud_tier,
                        );
                        token
                    };
                    cloud
                        .user_tier_registry()
                        .write()
                        .expect("testnet user tier registry poisoned")
                        .insert(user_id, spec.cloud_tier);
                    CloudAttachment {
                        addr: cloud.relay_addr(),
                        quic_addr: cloud_quic_addr.expect("cloud QUIC route missing"),
                        token,
                        user_id,
                        tier: spec.cloud_tier,
                        tokens: cloud.token_registry(),
                        user_tiers: cloud.user_tier_registry(),
                        refresh_interval: spec.cloud_refresh_interval,
                        udp_blocked_memory: spec.udp_blocked_memory,
                        relay_transport: spec.relay_transport,
                        quic_client_config: cloud.quic_client_config(),
                        clock: clock.clone(),
                    }
                }),
                runtime: Mutex::new(None),
                installation: None,
                sources: Default::default(),
            });
            let cloud_url = cloud
                .as_ref()
                .map(|cloud| cloud.url.clone())
                .unwrap_or_else(default_cloud_url);
            daemon::write_daemon_config(&inner, &cloud_url);
            let runtime = start_daemon_runtime(
                &inner,
                prep.listener,
                prep.quic_client_socket,
                discovery.clone(),
            )
            .await;
            *inner.runtime.lock().await = Some(runtime);
            daemon_inners.push(inner);
        }

        let mut installations = Vec::new();
        if !self.installations.is_empty() {
            for spec in self.installations {
                let installation = installation::start(
                    spec,
                    identity.as_ref().unwrap().clone(),
                    cloud.as_ref(),
                    discovery.clone(),
                    udp_proxy.clone(),
                    clock.clone(),
                )
                .await;
                daemon_inners.extend(installation.daemon_inners());
                installations.push(installation);
            }
        }

        for name in &self.undiscoverable {
            if let Some(inner) = daemon_inners.iter().find(|inner| &inner.name == name) {
                discovery.suppress(inner.host_id);
            }
        }

        let mut topology = serde_json::to_string_pretty(&self.topology)
            .expect("test topology declaration serializes");
        topology.push('\n');
        for installation in &installations {
            let _ = writeln!(
                topology,
                "  installation '{}' root={}",
                installation.name(),
                installation.root().display()
            );
            for profile in installation.daemon_inners() {
                let _ = writeln!(
                    topology,
                    "    profile '{}' host={} quic={:?} cloud_user={:?}",
                    profile.name,
                    profile.host_id,
                    profile.direct_addr,
                    profile.cloud.as_ref().map(|cloud| cloud.user_id)
                );
            }
        }
        let inner = Arc::new_cyclic(|weak| NetInner {
            topology,
            daemons: daemon_inners
                .into_iter()
                .map(|inner| Daemon {
                    inner,
                    net: weak.clone(),
                })
                .collect(),
            cloud,
            identity,
            installations: installations
                .into_iter()
                .map(|mut installation| {
                    installation.net = weak.clone();
                    installation
                })
                .collect(),
            pairs: daemon_pairs,
            discovery,
            udp_proxy,
            clock,
            discovery_events: StdMutex::new(discovery_events),
            _data_root: data_root,
            _socket_root: socket_root,
        });
        let net = TestNet { inner };
        // Staggered bring-up: direct links first, then the cloud relay (see
        // the note on `start_daemon_runtime`).
        for (a, b, via) in net.inner.pairs.clone() {
            if via == Via::Direct {
                let a = net.daemon(&a);
                let b = net.daemon(&b);
                a.connects_to(&b).via_direct().await;
            }
        }
        for daemon in net.cloud_attached_daemons() {
            daemon.reconnect_cloud().await;
        }
        net.wait_for_steady_state().await;
        for (a, b, via) in profile_pairs {
            let a = net.daemon(&a);
            let b = net.daemon(&b);
            let pin = b.start_pairing().await;
            match via {
                Via::Direct => a.pair(&b).with_pin(&pin).await,
                Via::Cloud => a.pair(&b).with_cloud_pin(&pin).await,
            }
            .expect("pair installation fixture profiles");
            a.can_call(&b).await;
            b.can_call(&a).await;
        }
        net
    }
}

struct DaemonPrep {
    identity: DeviceIdentity,
    data_dir: std::path::PathBuf,
    listener: Option<std::net::UdpSocket>,
    quic_client_socket: Option<std::net::UdpSocket>,
    direct_addr: Option<SocketAddr>,
    trust: TrustStore,
    attaches_to_cloud: bool,
}

fn index_of(specs: &[DaemonSpec], name: &str) -> usize {
    specs
        .iter()
        .position(|spec| spec.name == name)
        .expect("topology validation missed a daemon reference")
}

fn trust_entry(peer: &DeviceIdentity, name: &str, reachabilities: Vec<Reachability>) -> TrustEntry {
    TrustEntry {
        pubkey: peer.public_key().to_vec(),
        name: name.to_string(),
        paired_at: chrono::Utc::now(),
        reachabilities,
        signed_in: None,
    }
}

#[cfg(test)]
mod topology_tests {
    use super::*;

    #[test]
    fn builder_validation_rejects_a_modifier_without_an_antecedent() {
        let builder = TestNet::builder().cloud_only();
        let error = builder.validate().unwrap_err().to_string();
        assert_eq!(error, ".cloud_only() requires a daemon or profile");
    }

    #[test]
    fn builder_validation_rejects_unknown_pair_references() {
        let builder = TestNet::builder()
            .daemon("known")
            .paired("known", "missing", Via::Direct);
        let error = builder.validate().unwrap_err().to_string();
        assert_eq!(error, "paired(..) references unknown daemon 'missing'");
    }
}
