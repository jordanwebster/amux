//! The network edge between whole daemons in one process: pairing through
//! the front door over the LAN listener on loopback, authenticated links in
//! both directions, a cloud link whose credential the runtime's clock
//! refreshes, a stranger refused, unpairing, and discovery scoped to one
//! network. Nothing here touches a real LAN, mDNS or relay: the listeners
//! bind 127.0.0.1, discovery is a scripted bus, and the cloud and relay are
//! in-process stand-ins.

mod support;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use agent_dir::Clock as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use node::harness::{
    AuthenticatedLinkUser, HostVia, LinkTokenAuthenticator, ScriptedDiscovery, Tier,
    scripted_discovery,
};
use node::{
    CloudLinkServer, CloudOptions, Daemon, Edge, EdgeOptions, LanOptions, RelayIdentity,
    StartOptions,
};
use support::{Install, PATIENCE};
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    BeginPairRequest, BindProfileRequest, Empty, InventoryEvent, ListProfilesRequest, PeerRef,
    PendingPairRequest, ProfileBeginPairRequest, ProfileGetPeerRequest, ProfileInfo,
    ProfilePendingPairRequest, ProfileRequest, ProfileStartPairingRequest, ProfileUnpairRequest,
    StartPairingRequest, begin_pair_request, inventory_event, peer_ref, start_pairing_request,
    start_pairing_response,
};

/// The discovery scope the paired hosts share.
const SCOPE: &str = "edge-test";

/// A daemon serving its front door and a LAN listener on loopback.
struct Host {
    install: Install,
    daemon: Daemon,
    front_door: PathBuf,
}

impl Host {
    async fn start(name: &str, scope: &str, bus: &ScriptedDiscovery, cloud: CloudOptions) -> Self {
        Self::start_with_clock(name, scope, bus, cloud, Arc::new(agent_dir::SystemClock)).await
    }

    async fn start_with_clock(
        name: &str,
        scope: &str,
        bus: &ScriptedDiscovery,
        cloud: CloudOptions,
        clock: Arc<dyn agent_dir::Clock>,
    ) -> Self {
        let install = Install::new();
        let front_door = install.path("door.sock");
        let options = StartOptions {
            front_door: Some(front_door.clone()),
            clock,
            edge: EdgeOptions {
                host_name: name.to_owned(),
                kinds: vec![wire::Kind::ClaudeSdk],
                lan: Some(LanOptions {
                    bind: SocketAddr::from(([127, 0, 0, 1], 0)),
                }),
                discovery: Some(scripted_discovery(bus)),
                discovery_scope: scope.to_owned(),
                dial: true,
                link_socket: false,
                cloud,
            },
            ..install.options("boot-1", node::Launch::default())
        };
        let daemon = node::start(options, None).await.expect("the daemon starts");
        Self {
            install,
            daemon,
            front_door,
        }
    }

    fn edge(&self) -> Arc<Edge> {
        self.daemon
            .profile(self.install.profile)
            .expect("the profile runs")
            .edge()
            .expect("the edge runs")
    }

    fn id(&self) -> String {
        self.install.profile.to_string()
    }

    async fn door(&self) -> ProfileServiceClient<Channel> {
        ProfileServiceClient::new(channel(&self.front_door).await)
    }

    async fn info(&self) -> ProfileInfo {
        self.door()
            .await
            .list_profiles(ListProfilesRequest {})
            .await
            .unwrap()
            .into_inner()
            .profiles
            .into_iter()
            .find(|profile| profile.id == self.id())
            .expect("the profile is listed")
    }

    async fn shutdown(self) {
        self.daemon.shutdown().await.unwrap();
    }
}

async fn channel(path: &Path) -> Channel {
    let path = path.to_owned();
    Endpoint::from_static("http://amux.test")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(TokioIo::new)
            }
        }))
        .await
        .expect("the front door answers")
}

fn host_ref(host: Uuid) -> Option<PeerRef> {
    Some(PeerRef {
        identifier: Some(peer_ref::Identifier::HostId(host.as_bytes().to_vec())),
    })
}

/// Polls `check` until it holds, failing the test after the patience runs
/// out with `what` it was waiting for.
async fn until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Reads a peer's inventory to its CaughtUp and returns the hosts it named.
async fn peer_inventory_hosts(from: &Edge, to: Uuid) -> Vec<Vec<u8>> {
    let mut client = from.peer(to).await.expect("a route to the peer");
    let mut events = client
        .subscribe_inventory(Empty {})
        .await
        .expect("the peer answers a trusted caller")
        .into_inner();
    let mut hosts = Vec::new();
    loop {
        let InventoryEvent { of } = events
            .message()
            .await
            .expect("the inventory streams")
            .expect("the inventory reaches CaughtUp");
        match of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id),
            Some(inventory_event::Of::CaughtUp(_)) => return hosts,
            _ => {}
        }
    }
}

/// Pairs `initiator` with `responder` through both front doors, with a PIN,
/// over the responder's LAN listener.
async fn pair(initiator: &Host, responder: &Host) -> wire::PeerEntry {
    let started = responder
        .door()
        .await
        .start_pairing(ProfileStartPairingRequest {
            profile_id: responder.id(),
            pairing: Some(StartPairingRequest {
                mode: start_pairing_request::Mode::Pin as i32,
                ..StartPairingRequest::default()
            }),
            ..ProfileStartPairingRequest::default()
        })
        .await
        .expect("the responder enters pairing mode")
        .into_inner();
    let Some(start_pairing_response::Secret::Pin(pin)) = started.secret else {
        panic!("a PIN pairing hands out a PIN");
    };
    assert_eq!(
        started.addrs,
        vec![responder.edge().lan_addr().unwrap().to_string()],
        "the invitation names the LAN listener"
    );
    let pending = initiator
        .door()
        .await
        .begin_pair(ProfileBeginPairRequest {
            profile_id: initiator.id(),
            pairing: Some(BeginPairRequest {
                host_id: responder.edge().host_id().as_bytes().to_vec(),
                secret: Some(begin_pair_request::Secret::Pin(pin)),
                addrs: started.addrs,
            }),
            ..ProfileBeginPairRequest::default()
        })
        .await
        .expect("the SPAKE2 exchange completes")
        .into_inner();
    assert_eq!(
        pending.peer.as_ref().unwrap().pubkey,
        responder.edge().public_key(),
        "the pending pairing names the responder's key"
    );
    assert_eq!(pending.via, wire::PeerVia::Direct as i32);
    initiator
        .door()
        .await
        .confirm_pair(ProfilePendingPairRequest {
            profile_id: initiator.id(),
            pairing: Some(PendingPairRequest {
                token: pending.token,
            }),
            ..ProfilePendingPairRequest::default()
        })
        .await
        .expect("the person confirms the peer")
        .into_inner()
        .peer
        .expect("the confirmed peer")
}

#[tokio::test]
async fn paired_hosts_link_both_ways_refuse_strangers_and_unpair() {
    let bus = ScriptedDiscovery::new();
    let desk = Host::start("desk", SCOPE, &bus, CloudOptions::default()).await;
    let laptop = Host::start("laptop", SCOPE, &bus, CloudOptions::default()).await;
    let stranger = Host::start("stranger", "elsewhere", &bus, CloudOptions::default()).await;
    let (desk_id, laptop_id, stranger_id) = (
        desk.edge().host_id(),
        laptop.edge().host_id(),
        stranger.edge().host_id(),
    );

    // Discovery lists only the machines in this network's scope.
    until("the laptop in the desk's candidates", || async {
        desk.edge()
            .candidates()
            .iter()
            .any(|advert| advert.host_id == laptop_id)
    })
    .await;
    let desk_candidates = desk
        .edge()
        .candidates()
        .into_iter()
        .map(|advert| (advert.name, advert.scope))
        .collect::<Vec<_>>();
    assert_eq!(
        desk_candidates,
        vec![("laptop".to_owned(), SCOPE.to_owned())]
    );
    assert!(
        stranger.edge().candidates().is_empty(),
        "a host in another scope lists none of these"
    );
    println!("desk candidates: {desk_candidates:?}; stranger candidates: []");

    // The device identity the front door reports is the one pairing pins.
    let identity = desk
        .door()
        .await
        .get_device_identity(ProfileRequest {
            profile_id: desk.id(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(identity.host_id, desk_id.as_bytes());
    assert_eq!(identity.pubkey, desk.edge().public_key());

    let peer = pair(&laptop, &desk).await;
    assert_eq!(peer.host_id, desk_id.as_bytes());
    assert_eq!(peer.name, "desk");
    println!(
        "laptop paired with {} over {:?}",
        peer.name,
        wire::PeerVia::Direct
    );

    // Both sides now trust each other, and the laptop dialled the desk.
    until("the desk to trust the laptop", || async {
        desk.edge().is_trusted(laptop_id)
    })
    .await;
    until("a direct link both ways", || async {
        desk.edge().via(laptop_id).await == HostVia::Direct
            && laptop.edge().via(desk_id).await == HostVia::Direct
    })
    .await;
    let desk_peers = desk
        .door()
        .await
        .list_peers(ProfileRequest {
            profile_id: desk.id(),
        })
        .await
        .unwrap()
        .into_inner()
        .peers;
    assert_eq!(
        desk_peers
            .iter()
            .map(|peer| peer.name.as_str())
            .collect::<Vec<_>>(),
        vec!["laptop"]
    );

    // Authenticated calls run in both directions, each answered by the
    // other host's own runtime.
    let from_laptop = peer_inventory_hosts(&laptop.edge(), desk_id).await;
    let from_desk = peer_inventory_hosts(&desk.edge(), laptop_id).await;
    assert!(from_laptop.contains(&desk_id.as_bytes().to_vec()));
    assert!(from_desk.contains(&laptop_id.as_bytes().to_vec()));
    println!("PeerService answered laptop -> desk and desk -> laptop");

    // The desk knows who is calling: the laptop may send only as its own
    // agents.
    let impostor = wire::Envelope {
        from: Some(wire::Sender {
            value: Some(wire::sender::Value::Agent(wire::AgentSender {
                agent_id: Uuid::new_v4().as_bytes().to_vec(),
                host_id: stranger_id.as_bytes().to_vec(),
                name: "someone".to_owned(),
                kind: String::new(),
            })),
        }),
        to: Some(wire::AgentParent {
            host_id: desk_id.as_bytes().to_vec(),
            agent_id: Uuid::new_v4().as_bytes().to_vec(),
        }),
        text: "hello".to_owned(),
        ..wire::Envelope::default()
    };
    let refused = laptop
        .edge()
        .peer(desk_id)
        .await
        .unwrap()
        .send_message(impostor)
        .await
        .expect_err("a host cannot speak for another host's agent");
    assert_eq!(refused.code(), tonic::Code::PermissionDenied);

    // A stranger that trusts the desk but that the desk never paired with
    // is refused: its pinned dial never becomes a link, and on the desk's
    // listener, without a trusted key, there is no PeerService to reach.
    stranger.edge().trust(&desk.edge()).await.unwrap();
    stranger
        .edge()
        .dial(desk_id, desk.edge().lan_addr().unwrap());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(desk.edge().via(stranger_id).await, HostVia::Offline);
    assert_eq!(stranger.edge().via(desk_id).await, HostVia::Offline);
    let desk_addr = desk.edge().lan_addr().unwrap();
    let outside_pairing = match stranger.edge().unpinned_channel(desk_addr).await {
        Ok(channel) => wire::peer_service_client(channel)
            .subscribe_inventory(Empty {})
            .await
            .map(|_| ())
            .expect_err("a stranger reaches no PeerService")
            .code(),
        Err(_) => tonic::Code::Unavailable,
    };
    // In pairing mode a stranger's stream reaches the pairing service, and
    // only that.
    desk.door()
        .await
        .start_pairing(ProfileStartPairingRequest {
            profile_id: desk.id(),
            pairing: Some(StartPairingRequest {
                mode: start_pairing_request::Mode::Qr as i32,
                ..StartPairingRequest::default()
            }),
            ..ProfileStartPairingRequest::default()
        })
        .await
        .unwrap();
    let in_pairing = wire::peer_service_client(
        stranger
            .edge()
            .unpinned_channel(desk_addr)
            .await
            .expect("pairing mode admits an unpinned stream"),
    )
    .subscribe_inventory(Empty {})
    .await
    .map(|_| ())
    .expect_err("pairing mode serves only pairing")
    .code();
    assert_eq!(in_pairing, tonic::Code::Unimplemented);
    desk.door()
        .await
        .cancel_pairing(wire::ProfileOperation {
            profile_id: desk.id(),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap();
    println!(
        "stranger refused: pinned dial never links; unpinned PeerService call {outside_pairing:?}, \
         in pairing mode {in_pairing:?}"
    );
    assert!(!desk.edge().is_trusted(stranger_id));

    // Unpairing removes the laptop's key, closes its link, and a redial
    // with the now unknown key is refused.
    let removed = desk
        .door()
        .await
        .unpair(ProfileUnpairRequest {
            profile_id: desk.id(),
            peer: host_ref(laptop_id),
            reason: "retired".to_owned(),
            ..ProfileUnpairRequest::default()
        })
        .await
        .unwrap()
        .into_inner()
        .removed_peer
        .unwrap();
    assert_eq!(removed.name, "laptop");
    until("the link to close both ways", || async {
        desk.edge().via(laptop_id).await == HostVia::Offline
            && laptop.edge().via(desk_id).await == HostVia::Offline
    })
    .await;
    let gone = desk
        .door()
        .await
        .get_peer(ProfileGetPeerRequest {
            profile_id: desk.id(),
            peer: host_ref(laptop_id),
        })
        .await
        .expect_err("an unpaired host is no peer");
    assert_eq!(gone.code(), tonic::Code::NotFound);
    laptop.edge().dial(desk_id, desk.edge().lan_addr().unwrap());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(desk.edge().via(laptop_id).await, HostVia::Offline);
    println!("desk unpaired the laptop; its redial is refused");

    for host in [desk, laptop, stranger] {
        host.shutdown().await;
    }
}

#[tokio::test]
async fn trusted_edges_link_in_process_until_the_link_is_severed() {
    // Apart on the network, so nothing redials once the link is cut: two
    // hosts that find each other would link again directly.
    let one = Host::start(
        "one",
        SCOPE,
        &ScriptedDiscovery::new(),
        CloudOptions::default(),
    )
    .await;
    let two = Host::start(
        "two",
        SCOPE,
        &ScriptedDiscovery::new(),
        CloudOptions::default(),
    )
    .await;
    let (one_id, two_id) = (one.edge().host_id(), two.edge().host_id());

    assert!(
        one.edge().link_in_process(&two.edge()).is_err(),
        "hosts that do not trust each other are not linked"
    );
    one.edge().trust(&two.edge()).await.unwrap();
    two.edge().trust(&one.edge()).await.unwrap();
    let link = one.edge().link_in_process(&two.edge()).unwrap();
    until("the in-process link", || async {
        one.edge().via(two_id).await == HostVia::Direct
            && two.edge().via(one_id).await == HostVia::Direct
    })
    .await;
    assert!(
        peer_inventory_hosts(&two.edge(), one_id)
            .await
            .contains(&one_id.as_bytes().to_vec())
    );

    link.sever();
    until("the severed link to leave both hosts", || async {
        one.edge().via(two_id).await == HostVia::Offline
            && two.edge().via(one_id).await == HostVia::Offline
    })
    .await;

    one.shutdown().await;
    two.shutdown().await;
}

/// Relay credentials the fake cloud minted: the account, when each
/// expires on the driven clock, and the tier it carries.
type Minted = Arc<Mutex<HashMap<String, (Uuid, i64, Tier)>>>;

/// Stands in for the cloud: the OAuth token endpoint, userinfo, and the
/// connect call that hands out relay credentials, whose tier the test
/// changes.
struct FakeCloud {
    url: String,
    relay: SocketAddr,
    tier: Mutex<&'static str>,
    connects: Mutex<u32>,
    clock: Arc<agent_dir::ManualClock>,
    tokens: Minted,
    user: Uuid,
}

impl FakeCloud {
    async fn start(
        relay: SocketAddr,
        clock: Arc<agent_dir::ManualClock>,
        tokens: Minted,
    ) -> Arc<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cloud = Arc::new(Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            relay,
            tier: Mutex::new("free"),
            connects: Mutex::new(0),
            clock,
            tokens,
            user: Uuid::new_v4(),
        });
        let serving = cloud.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let cloud = serving.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let cloud = cloud.clone();
                        async move { Ok::<_, Infallible>(cloud.answer(request).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        cloud
    }

    async fn answer(&self, request: hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
        let path = request.uri().path().to_owned();
        let _ = request.into_body().collect().await;
        let body = match path.as_str() {
            "/connect/token" => serde_json::json!({
                "access_token": "access",
                "token_type": "bearer",
                "expires_in": 3600,
                "refresh_token": "refresh-rotated",
            }),
            "/connect/userinfo" => serde_json::json!({
                "sub": "ada",
                "name": "Ada",
                "email": "ada@example.com",
            }),
            "/api/connect" => {
                let count = {
                    let mut connects = self.connects.lock().unwrap();
                    *connects += 1;
                    *connects
                };
                let tier = *self.tier.lock().unwrap();
                let token = format!("relay-{count}");
                // Each relay credential lives ten minutes on the clock the
                // test drives.
                let expires_ms = self.clock.now_ms() + 600_000;
                self.tokens.lock().unwrap().insert(
                    token.clone(),
                    (
                        self.user,
                        expires_ms,
                        if tier == "pro" { Tier::Pro } else { Tier::Free },
                    ),
                );
                let expires_at =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(expires_ms).unwrap();
                serde_json::json!({
                    "host": "127.0.0.1",
                    "port": self.relay.port(),
                    "token": token,
                    "expires_at": expires_at.to_rfc3339(),
                    "tier": tier,
                })
            }
            _ => {
                return hyper::Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::new()))
                    .unwrap();
            }
        };
        hyper::Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap()
    }
}

/// The relay's check of a connection token: the ones the fake cloud minted,
/// each until its expiry.
struct RegisteredTokens {
    tokens: Minted,
    seen: Mutex<Vec<String>>,
}

#[tonic::async_trait]
impl LinkTokenAuthenticator for RegisteredTokens {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        self.seen.lock().unwrap().push(token.to_owned());
        let (user_id, expires_ms, tier) = *self
            .tokens
            .lock()
            .unwrap()
            .get(token)
            .ok_or_else(|| tonic::Status::unauthenticated("unknown token"))?;
        Ok(AuthenticatedLinkUser {
            user_id,
            client_id: "cli".to_owned(),
            expires_at: SystemTime::UNIX_EPOCH + Duration::from_millis(expires_ms as u64),
            tier,
        })
    }
}

#[tokio::test]
async fn a_signed_in_profile_refreshes_its_relay_credential_on_the_runtime_clock() {
    let start_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let clock = Arc::new(agent_dir::ManualClock::new(start_ms));
    let tokens = Arc::new(Mutex::new(HashMap::new()));
    let authenticator = Arc::new(RegisteredTokens {
        tokens: tokens.clone(),
        seen: Mutex::new(Vec::new()),
    });
    let relay = CloudLinkServer::new(RelayIdentity {
        host_id: Uuid::new_v4(),
        name: "relay".to_owned(),
        authenticator: authenticator.clone(),
        clock: clock.clone(),
    });
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    let _relay = relay.serve_on_tcp_listener(relay_listener);
    let cloud = FakeCloud::start(relay_addr, clock.clone(), tokens).await;

    let bus = ScriptedDiscovery::new();
    let host = Host::start_with_clock(
        "desk",
        SCOPE,
        &bus,
        CloudOptions {
            relay_tcp: Some(relay_addr),
            free_refresh_interval: Some(Duration::from_secs(60)),
            credentials: None,
        },
        clock.clone(),
    )
    .await;
    let host_id = host.edge().host_id();
    let unbound = host.info().await;
    assert_eq!(unbound.intent, wire::Intent::Unbound as i32);

    let bound = host
        .door()
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(host.id()),
            cloud_url: cloud.url.clone(),
            staged_refresh_token: "refresh-staged".to_owned(),
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account")
        .into_inner();
    assert_eq!(bound.intent, wire::Intent::Bound as i32);
    assert_eq!(bound.email, "ada@example.com");
    assert_eq!(bound.account_name, "Ada");

    until("the cloud link to connect", || async {
        let info = host.info().await;
        info.observed == wire::Observed::Connected as i32 && info.tier == wire::Tier::Free as i32
    })
    .await;
    assert!(relay.user_has_link_to(cloud.user, host_id).await);
    assert_eq!(*cloud.connects.lock().unwrap(), 1);
    println!("bound as Ada; connected to the relay on the free tier");

    // The account buys Pro. Nothing happens until the runtime's clock
    // reaches the free tier's refresh interval.
    *cloud.tier.lock().unwrap() = "pro";
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(*cloud.connects.lock().unwrap(), 1);
    clock.advance(60_000);
    until("the refreshed credential to carry the new tier", || async {
        host.info().await.tier == wire::Tier::Pro as i32
    })
    .await;
    assert_eq!(*cloud.connects.lock().unwrap(), 2);
    assert_eq!(
        authenticator.seen.lock().unwrap().as_slice(),
        ["relay-1", "relay-2"],
        "the relay saw the first credential in Hello and the second in Reauth"
    );
    println!("clock +60s: Reauth with relay-2, tier Pro");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        *cloud.connects.lock().unwrap(),
        2,
        "a fresh Pro credential is not refreshed again at once"
    );

    // On Pro the link refreshes five minutes before its credential expires.
    // Past the first credential's expiry on the relay's clock, the link
    // lives on a refreshed one.
    clock.advance(550_000);
    until("the Pro credential's refresh before expiry", || async {
        authenticator
            .seen
            .lock()
            .unwrap()
            .contains(&"relay-3".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(relay.user_has_link_to(cloud.user, host_id).await);
    assert_eq!(host.info().await.observed, wire::Observed::Connected as i32);
    println!("clock +610s: relay-1 expired, the link lives on relay-3");

    // Pausing drops the link and keeps the credential; resuming brings it
    // back; signing out forgets the credential and keeps the binding.
    let paused = host
        .door()
        .await
        .pause_profile(wire::ProfileOperation {
            profile_id: host.id(),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(paused.intent, wire::Intent::Paused as i32);
    until("the relay link to go with the pause", || async {
        !relay.user_has_link_to(cloud.user, host_id).await
    })
    .await;
    host.door()
        .await
        .resume_profile(wire::ProfileOperation {
            profile_id: host.id(),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap();
    until("the relay link to come back", || async {
        relay.user_has_link_to(cloud.user, host_id).await
    })
    .await;
    let signed_out = host
        .door()
        .await
        .logout_profile(wire::ProfileOperation {
            profile_id: host.id(),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(signed_out.intent, wire::Intent::LoggedOut as i32);
    assert_eq!(signed_out.email, "ada@example.com");
    until("the relay link to go with the sign-out", || async {
        !relay.user_has_link_to(cloud.user, host_id).await
    })
    .await;
    println!("paused, resumed and signed out");

    host.shutdown().await;
}
