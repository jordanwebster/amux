//! TestNet: the in-process spec-test harness.
//!
//! Builds topologies of *whole daemons* — real identities, real trust
//! stores, real localhost TCP with device mTLS, and an optional in-process
//! cloud relay — and exposes user-meaningful observation verbs plus
//! network-operator verbs. See `notes/SPEC_TESTS_DESIGN.md` for the design
//! contract; the assembly recipes come from the `services::startup` tests.
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
//! Compiled only with the `testnet` cargo feature; not part of the public
//! API.

mod assertions;
mod daemon;
mod net;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::Mutex;

pub use daemon::{Daemon, RouteAssertion};

use crate::identity::{DeviceIdentity, load_or_create_device_identity_in};
use crate::trust::{Reachability, TrustEntry, TrustStore};

use assertions::eventually;
use daemon::{CloudAttachment, DaemonInner, start_daemon_runtime};
use net::CloudRelay;

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

pub struct TestNet {
    inner: Arc<NetInner>,
}

pub(crate) struct NetInner {
    pub(crate) topology: String,
    pub(crate) daemons: Vec<Daemon>,
    pub(crate) cloud: Option<CloudRelay>,
    pairs: Vec<(String, String, Via)>,
    /// Owns every daemon's data dir; removed when the net is dropped.
    _data_root: tempfile::TempDir,
}

impl TestNet {
    pub fn builder() -> TestNetBuilder {
        TestNetBuilder::default()
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
                async || !daemon.has_single_hop_route_to(cloud.host_id).await,
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
                async || !matches!(from.route_to(to_id).await, Some(route) if route.len() == 1),
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
        // Only the dialing side gains a single-hop route: the acceptor of a
        // direct link deliberately stores no route back (it has no outbound
        // channel on that link).
        let assertion = format!("'{}' gains a direct route to '{}'", from.name(), to.name());
        let to_id = to.host_id();
        eventually(
            &assertion,
            async || matches!(from.route_to(to_id).await, Some(route) if route.len() == 1),
            from.failure_dump(),
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
                    // The acceptor side of a direct link stores no route
                    // back, so without a cloud it cannot observe the dialer.
                    if a.inner.cloud.is_some() && b.inner.cloud.is_some() {
                        b.sees(&a).await;
                    }
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
}

#[derive(Default)]
pub struct TestNetBuilder {
    cloud: bool,
    daemons: Vec<DaemonSpec>,
    pairs: Vec<(String, String, Via)>,
}

impl TestNetBuilder {
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
        self.daemons.push(DaemonSpec {
            name,
            cloud_only: false,
            no_cloud: false,
        });
        self
    }

    /// Marks the most recently added daemon as cloud-only: no direct
    /// transports, all traffic through the relay.
    pub fn cloud_only(mut self) -> Self {
        self.last_daemon("cloud_only").cloud_only = true;
        self
    }

    /// Opts the most recently added daemon out of the cloud relay.
    pub fn no_cloud(mut self) -> Self {
        self.last_daemon("no_cloud").no_cloud = true;
        self
    }

    /// Pairing-as-fixture: seeds both trust stores (pubkey, name, and the
    /// reachability implied by `via`) before the daemons start.
    pub fn paired(mut self, a: impl Into<String>, b: impl Into<String>, via: Via) -> Self {
        self.pairs.push((a.into(), b.into(), via));
        self
    }

    fn last_daemon(&mut self, verb: &str) -> &mut DaemonSpec {
        self.daemons
            .last_mut()
            .unwrap_or_else(|| panic!(".{verb}() must follow .daemon(..)"))
    }

    /// Starts the declared topology and waits for its steady state.
    pub async fn start(self) -> TestNet {
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

        let mut daemon_inners = Vec::with_capacity(preps.len());
        for (spec, prep) in self.daemons.iter().zip(preps) {
            prep.trust
                .save_in(&prep.data_dir)
                .unwrap_or_else(|error| panic!("seed trust store for '{}': {error}", spec.name));
            let inner = Arc::new(DaemonInner {
                name: spec.name.clone(),
                host_id: prep.identity.host_id,
                data_dir: prep.data_dir,
                tcp_addr: prep.tcp_addr,
                cloud: prep.attaches_to_cloud.then(|| {
                    let cloud = cloud.as_ref().expect("cloud attachment without cloud");
                    CloudAttachment {
                        addr: cloud.addr,
                        token: cloud.token.clone(),
                    }
                }),
                runtime: Mutex::new(None),
            });
            let runtime = start_daemon_runtime(&inner, prep.listener).await;
            *inner.runtime.lock().await = Some(runtime);
            daemon_inners.push(inner);
        }

        let topology = render_topology(&self.daemons, &daemon_inners, cloud.as_ref(), &self.pairs);
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
        let _ = writeln!(
            out,
            "  daemon '{}' (host_id {}) tcp={tcp} {cloud_attachment}",
            spec.name, inner.host_id
        );
    }
    for (a, b, via) in pairs {
        let _ = writeln!(out, "  paired '{a}' <-> '{b}' via {via:?}");
    }
    out
}
