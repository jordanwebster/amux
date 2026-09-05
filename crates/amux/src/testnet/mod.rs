//! TestNet: the in-process spec-test harness.
//!
//! [`TestNet`] builds a declared topology of *whole daemons* — real
//! identities, real trust stores, real localhost TCP with device mTLS, and
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
//!     .paired("laptop", "desktop", Via::Tcp)
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
//! - **Eventually, never sleep.** Every observation verb polls through
//!   [`assertions::eventually`] under one default timeout; on expiry it
//!   panics with a dump of the declared topology, every daemon's host table,
//!   and the failing daemon's routes. Tests contain no retry loops.
//! - **Restart = complete teardown.** `Daemon::stop`/`restart` await the cloud
//!   connector's cleanup, then sever direct sockets whose detached dispatcher
//!   tasks model process-owned connections. Aborting an established connector
//!   task skips its asynchronous link cleanup and must not be used as stop.
//!   Identity, trust, and the TCP address persist across restarts.
//! - **Sever = real outage.** `sever_direct`/`cloud_offline` cut sockets (or
//!   close links) hard and return only once the affected daemons have
//!   observed the loss, so follow-up assertions start from a settled net.
//!
//! Compiled only with the `testnet` cargo feature; not part of the public
//! API.

mod assertions;
mod daemon;
mod installation;
mod net;
pub use installation::{InstallationHandle, Profile, RetainedProfileWork, WatchProbe};
mod pairing;
mod session;
mod wire;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use assertions::eventually;
use daemon::{CloudAttachment, DaemonInner, start_daemon_runtime};
pub use daemon::{Daemon, ExpiringJwt, RouteAssertion, RoutedStream};
use net::CloudRelay;
pub use pairing::{PairAttempt, Pin, QrPayload};
pub use session::EchoSession;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
pub use wire::{LinkCloseReason, WirePeer};

use crate::identity::{DeviceIdentity, load_or_create_device_identity_in};
use crate::trust::{Reachability, TrustEntry, TrustStore};

/// How a pre-paired fixture pair reaches each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// Direct TCP: the first daemon stores a `DirectTcp` reachability for
    /// the second (mirroring real PIN pairing over TCP, where only the
    /// initiator learns an address).
    Tcp,
    /// Both daemons store a `Cloud` reachability and meet at the relay.
    Cloud,
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
    installations: Vec<InstallationHandle>,
    pairs: Vec<(String, String, Via)>,
    /// Owns every daemon's data dir; removed when the net is dropped.
    _data_root: tempfile::TempDir,
}

/// How long [`TestNet::cloud_relay_cannot_call`] gives the relay's doomed
/// call attempt before treating "still not completed" as cannot-call.
const RELAY_CALL_ATTEMPT_TIMEOUT: std::time::Duration = assertions::DEFAULT_TIMEOUT;

impl TestNet {
    /// Starts declaring a topology; finish with [`TestNetBuilder::start`].
    pub fn builder() -> TestNetBuilder {
        TestNetBuilder::default()
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

    /// Rejects this account at the production relay authentication boundary.
    pub fn reject_cloud_user(&self, user: &str, error: Option<crate::ProtocolError>) {
        self.cloud()
            .reject_user(user, error.map(crate::protocol::protocol_status));
    }

    /// Takes the cloud relay down hard: accepted sockets are severed, so
    /// daemons observe a genuine outage. Returns once every attached daemon
    /// has dropped its own relay link. (Multi-hop routes to the relay
    /// learned through peers may linger until their HostDowns propagate.)
    pub async fn cloud_offline(&self) {
        let cloud = self.cloud();
        cloud.go_offline().await;
        // Note: the daemons' connector tasks are left running so they can
        // observe the dead socket and tear their links down themselves.
        for daemon in self.cloud_attached_daemons() {
            let assertion = format!(
                "'{}' drops its link to the offline cloud relay",
                daemon.name()
            );
            eventually(
                &assertion,
                async || !daemon.has_direct_route_to(cloud.host_id).await,
                daemon.failure_dump(),
            )
            .await;
        }
    }

    /// Restarts the cloud relay on the same address and reconnects every
    /// cloud-attached daemon.
    pub async fn cloud_online(&self) {
        let cloud = self.cloud();
        cloud.go_online().await;
        for daemon in self.cloud_attached_daemons() {
            daemon.reconnect_cloud().await;
        }
        for daemon in self.cloud_attached_daemons() {
            let assertion = format!("'{}' reattaches to the cloud relay", daemon.name());
            eventually(
                &assertion,
                async || daemon.knows_host(cloud.host_id).await,
                daemon.failure_dump(),
            )
            .await;
        }
    }

    /// Cuts the direct link between `a` and `b` by closing every link either
    /// side holds to the other. Routes through relays are unaffected.
    pub async fn sever_direct(&self, a: &Daemon, b: &Daemon) {
        for (from, to) in [(a, b), (b, a)] {
            if let Some(parts) = from.try_parts().await {
                parts.tunnels.link_registry().close_host(to.host_id()).await;
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
    /// from the `DirectTcp` reachability stored at pairing time.
    pub async fn establish_direct(&self, a: &Daemon, b: &Daemon) {
        let mut attempt = None;
        if let Some(reachability) = a.direct_tcp_reachability_to(b.host_id()).await {
            attempt = Some((a, b, reachability));
        } else if let Some(reachability) = b.direct_tcp_reachability_to(a.host_id()).await {
            attempt = Some((b, a, reachability));
        }
        let Some((from, to, reachability)) = attempt else {
            panic!(
                "establish_direct('{}', '{}'): neither trust store holds a DirectTcp \
                 reachability; pair them Via::Tcp first",
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
            cloud.try_call_into(user_id, target.host_id()),
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
            async || !cloud.has_link_to(user_id, target.host_id()).await,
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
    /// `Via::Tcp` pairs route directly.
    async fn wait_for_steady_state(&self) {
        if let Some(cloud) = &self.inner.cloud {
            for daemon in self.cloud_attached_daemons() {
                let assertion = format!("'{}' attaches to the cloud relay", daemon.name());
                eventually(
                    &assertion,
                    async || daemon.knows_host(cloud.host_id).await,
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
                Via::Tcp => {
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
    cloud_only: bool,
    no_cloud: bool,
    cloud_user: Option<String>,
}

/// Declares a topology for [`TestNetBuilder::start`]: daemons, an optional
/// cloud relay, and pre-seeded trust (pairing-as-fixture). Obtained from
/// [`TestNet::builder`].
#[derive(Default)]
pub struct TestNetBuilder {
    cloud: bool,
    installations: Vec<installation::InstallationSpec>,
    selecting_profile: bool,
    daemons: Vec<DaemonSpec>,
    pairs: Vec<(String, String, Via)>,
    trusted: Vec<(String, String)>,
}

impl TestNetBuilder {
    pub fn installation(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            !self.installations.iter().any(|spec| spec.name == name),
            "duplicate installation '{name}'"
        );
        self.installations.push(installation::InstallationSpec {
            name,
            persistent: false,
            profiles: Vec::new(),
        });
        self.selecting_profile = true;
        self
    }

    pub fn profile(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let installation = self
            .installations
            .last_mut()
            .expect(".profile() must follow .installation()");
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
        });
        self.selecting_profile = true;
        self
    }

    /// Persist the installation in a short temporary root for reopen and lock tests.
    pub fn persistent(mut self) -> Self {
        self.installations
            .last_mut()
            .expect(".persistent() must follow .installation()")
            .persistent = true;
        self
    }

    /// Adds an in-process cloud relay; daemons attach to it by default.
    pub fn cloud(mut self) -> Self {
        self.cloud = true;
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
        self.selecting_profile = false;
        self.daemons.push(DaemonSpec {
            name,
            cloud_only: false,
            no_cloud: false,
            cloud_user: None,
        });
        self
    }

    /// Marks the most recently added daemon as cloud-only: no direct
    /// transports, all traffic through the relay.
    pub fn cloud_only(mut self) -> Self {
        if self.selecting_profile {
            self.installations
                .last_mut()
                .unwrap()
                .profiles
                .last_mut()
                .expect("cloud_only requires a profile")
                .cloud_only = true;
        } else {
            self.last_daemon("cloud_only").cloud_only = true;
        }
        self
    }

    /// Opts the most recently added daemon out of the cloud relay.
    pub fn no_cloud(mut self) -> Self {
        if self.selecting_profile {
            self.installations
                .last_mut()
                .unwrap()
                .profiles
                .last_mut()
                .expect("no_cloud requires a profile")
                .cloud_user = None;
        } else {
            self.last_daemon("no_cloud").no_cloud = true;
        }
        self
    }

    /// Attaches the most recently added daemon to the cloud as a *different*
    /// cloud user (by default every daemon shares one account). Cloud
    /// presence is per-user, so daemons of different users meet nothing of
    /// each other at the relay.
    pub fn cloud_user(mut self, user: impl Into<String>) -> Self {
        if self.selecting_profile {
            self.installations
                .last_mut()
                .unwrap()
                .profiles
                .last_mut()
                .expect("cloud_user requires a profile")
                .cloud_user = Some(user.into());
        } else {
            self.last_daemon("cloud_user").cloud_user = Some(user.into());
        }
        self
    }

    /// Pairing-as-fixture: seeds both trust stores (pubkey, name, and the
    /// reachability implied by `via`) before the daemons start.
    pub fn paired(mut self, a: impl Into<String>, b: impl Into<String>, via: Via) -> Self {
        self.pairs.push((a.into(), b.into(), via));
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

    fn last_daemon(&mut self, verb: &str) -> &mut DaemonSpec {
        self.daemons
            .last_mut()
            .unwrap_or_else(|| panic!(".{verb}() must follow .daemon(..)"))
    }

    /// Starts the declared topology and waits for its steady state.
    pub async fn start(mut self) -> TestNet {
        let (profile_pairs, daemon_pairs): (Vec<_>, Vec<_>) = self
            .pairs
            .into_iter()
            .partition(|(a, b, _)| a.contains('/') || b.contains('/'));
        self.pairs = daemon_pairs;
        let data_root = tempfile::Builder::new()
            .prefix("amux-spec")
            .tempdir()
            .expect("create testnet data root");

        let cloud = if self.cloud {
            Some(CloudRelay::start().await)
        } else {
            None
        };

        // Identities and direct-TCP listeners first, so trust seeding can
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
            let (listener, tcp_addr) = if spec.cloud_only {
                (None, None)
            } else {
                let listener = TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("bind daemon direct-TCP listener");
                let addr = listener.local_addr().expect("daemon listener address");
                (Some(listener), Some(addr))
            };
            preps.push(DaemonPrep {
                identity,
                data_dir,
                listener,
                tcp_addr,
                trust: TrustStore::default(),
                attaches_to_cloud: self.cloud && !spec.no_cloud,
            });
        }

        for (a, b, via) in &self.pairs {
            let ia = index_of(&self.daemons, a);
            let ib = index_of(&self.daemons, b);
            assert_ne!(ia, ib, "cannot pair '{a}' with itself");
            let host_id_a = preps[ia].identity.host_id;
            let host_id_b = preps[ib].identity.host_id;
            let (reach_a_to_b, reach_b_to_a) = match via {
                Via::Tcp => {
                    let addr_b = preps[ib].tcp_addr.unwrap_or_else(|| {
                        panic!(
                            "paired('{a}', '{b}', Via::Tcp) requires '{b}' to expose a \
                             direct-TCP listener, but it is cloud_only"
                        )
                    });
                    // Mirrors real PIN pairing over TCP: only the initiator
                    // stores the peer's address.
                    (vec![Reachability::DirectTcp { addr: addr_b }], Vec::new())
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

        let mut daemon_inners = Vec::with_capacity(preps.len());
        for (spec, prep) in self.daemons.iter().zip(preps) {
            prep.trust
                .save_in(&prep.data_dir)
                .unwrap_or_else(|error| panic!("seed trust store for '{}': {error}", spec.name));
            let inner = Arc::new(DaemonInner {
                name: spec.name.clone(),
                host_id: prep.identity.host_id,
                data_dir: prep.data_dir,
                artifact_clock: Arc::new(daemon::TestArtifactClock::new()),
                tcp_addr: prep.tcp_addr,
                cloud: prep.attaches_to_cloud.then(|| {
                    let cloud = cloud.as_ref().expect("cloud attachment without cloud");
                    let (user_id, token) = match &spec.cloud_user {
                        Some(label) => cloud.credentials_for_user(label),
                        None => (cloud.default_user_id(), cloud.token.clone()),
                    };
                    CloudAttachment {
                        addr: cloud.addr,
                        token,
                        user_id,
                    }
                }),
                runtime: Mutex::new(None),
                installation: None,
                tracked_tcp: Default::default(),
            });
            let runtime = start_daemon_runtime(&inner, prep.listener).await;
            *inner.runtime.lock().await = Some(runtime);
            daemon_inners.push(inner);
        }

        let mut installations = Vec::new();
        if !self.installations.is_empty() {
            let mut users = std::collections::BTreeSet::new();
            for installation in &self.installations {
                for profile in &installation.profiles {
                    if let Some(user) = &profile.cloud_user {
                        users.insert(user.clone());
                        let cloud = cloud.as_ref().expect("cloud_user requires .cloud()");
                        let (user_id, _) = cloud.credentials_for_user(user);
                        cloud.register_token(
                            &crate::test_fixtures::relay_token(user),
                            user_id,
                            std::time::Duration::from_secs(3600),
                        );
                    }
                }
            }
            if users.is_empty() {
                users.insert("default".into());
            }
            let identity = Arc::new(
                crate::test_fixtures::IdentityServer::start(
                    users
                        .into_iter()
                        .map(|sub| crate::test_fixtures::TestAccount {
                            name: Some(format!("{sub} Example")),
                            email: Some(format!("{sub}@example.test")),
                            sub,
                        })
                        .collect(),
                    cloud.as_ref().map(|relay| relay.addr),
                )
                .await,
            );
            for spec in self.installations {
                let installation =
                    installation::start(spec, identity.clone(), cloud.as_ref()).await;
                daemon_inners.extend(installation.daemon_inners());
                installations.push(installation);
            }
        }

        let mut topology = render_topology(
            &self.daemons,
            &daemon_inners,
            cloud.as_ref(),
            &self.pairs,
            &self.trusted,
        );
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
                    "    profile '{}' host={} tcp={:?} cloud_user={:?}",
                    profile.name,
                    profile.host_id,
                    profile.tcp_addr,
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
            installations: installations
                .into_iter()
                .map(|mut installation| {
                    installation.net = weak.clone();
                    installation
                })
                .collect(),
            pairs: self.pairs,
            _data_root: data_root,
        });
        let net = TestNet { inner };
        // Staggered bring-up: direct links first, then the cloud relay (see
        // the note on `start_daemon_runtime`).
        for (a, b, via) in net.inner.pairs.clone() {
            if via == Via::Tcp {
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
                Via::Tcp => a.pair(&b).with_pin(&pin).await,
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
    listener: Option<TcpListener>,
    tcp_addr: Option<SocketAddr>,
    trust: TrustStore,
    attaches_to_cloud: bool,
}

fn index_of(specs: &[DaemonSpec], name: &str) -> usize {
    specs
        .iter()
        .position(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("paired(..) references unknown daemon '{name}'"))
}

fn trust_entry(peer: &DeviceIdentity, name: &str, reachabilities: Vec<Reachability>) -> TrustEntry {
    TrustEntry {
        pubkey: peer.public_key().to_vec(),
        name: name.to_string(),
        paired_at: chrono::Utc::now(),
        reachabilities,
    }
}

fn render_topology(
    specs: &[DaemonSpec],
    inners: &[Arc<DaemonInner>],
    cloud: Option<&CloudRelay>,
    pairs: &[(String, String, Via)],
    trusted: &[(String, String)],
) -> String {
    let mut out = String::new();
    if let Some(cloud) = cloud {
        let _ = writeln!(
            out,
            "  cloud relay at {} (host_id {})",
            cloud.addr, cloud.host_id
        );
    }
    for (spec, inner) in specs.iter().zip(inners) {
        let tcp = inner
            .tcp_addr
            .map_or_else(|| "none".to_string(), |addr| addr.to_string());
        let cloud_attachment = if spec.cloud_only {
            "cloud-only"
        } else if inner.cloud.is_some() {
            "cloud-attached"
        } else {
            "no-cloud"
        };
        let cloud_user = spec
            .cloud_user
            .as_ref()
            .map(|user| format!(" cloud_user='{user}'"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  daemon '{}' (host_id {}) tcp={tcp} {cloud_attachment}{cloud_user}",
            spec.name, inner.host_id
        );
    }
    for (a, b, via) in pairs {
        let _ = writeln!(out, "  paired '{a}' <-> '{b}' via {via:?}");
    }
    for (a, b) in trusted {
        let _ = writeln!(out, "  trusted '{a}' <-> '{b}' (no reachability)");
    }
    out
}
