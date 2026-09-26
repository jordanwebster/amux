//! The network edge between whole daemons: pairing through the front door
//! over the LAN listener on loopback, authenticated links in both
//! directions, a cloud link whose credential the policy clock refreshes, a
//! stranger refused, unpairing, and discovery scoped to one network.
//! Nothing here touches a real LAN, mDNS or relay: the listeners bind
//! 127.0.0.1, discovery is the net's scripted bus, and the relay is the
//! topology's, beside a stand-in cloud.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use node::Edge;
use node::harness::{HostVia, Tier};
use testnet::{ClockMode, HostDecl, Net, NetOptions, PATIENCE, Relay, TierDecl, Topology};
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

/// A host listening for direct links on loopback and on the net's
/// discovery bus.
fn lan_host(name: &str, scope: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        scope: Some(scope.to_owned()),
        ..HostDecl::default()
    }
}

fn edge(net: &Net, host: &str) -> Arc<Edge> {
    net.edge(host).expect("the edge runs")
}

fn id(net: &Net, host: &str) -> String {
    net.host(host).unwrap().profile.to_string()
}

async fn door(net: &Net, host: &str) -> ProfileServiceClient<Channel> {
    ProfileServiceClient::new(channel(&net.host(host).unwrap().front_door).await)
}

async fn info(net: &Net, host: &str) -> ProfileInfo {
    let profile = id(net, host);
    door(net, host)
        .await
        .list_profiles(ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|listed| listed.id == profile)
        .expect("the profile is listed")
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
async fn pair(net: &Net, initiator: &str, responder: &str) -> wire::PeerEntry {
    let started = door(net, responder)
        .await
        .start_pairing(ProfileStartPairingRequest {
            profile_id: id(net, responder),
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
        vec![edge(net, responder).lan_addr().unwrap().to_string()],
        "the invitation names the LAN listener"
    );
    let pending = door(net, initiator)
        .await
        .begin_pair(ProfileBeginPairRequest {
            profile_id: id(net, initiator),
            pairing: Some(BeginPairRequest {
                host_id: edge(net, responder).host_id().as_bytes().to_vec(),
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
        edge(net, responder).public_key(),
        "the pending pairing names the responder's key"
    );
    assert_eq!(pending.via, wire::PeerVia::Direct as i32);
    door(net, initiator)
        .await
        .confirm_pair(ProfilePendingPairRequest {
            profile_id: id(net, initiator),
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
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE))
            .host_decl(lan_host("stranger", "elsewhere")),
    )
    .await
    .unwrap();
    let (desk_id, laptop_id, stranger_id) = (
        edge(&net, "desk").host_id(),
        edge(&net, "laptop").host_id(),
        edge(&net, "stranger").host_id(),
    );

    // Discovery lists only the machines in this network's scope.
    until("the laptop in the desk's candidates", || async {
        edge(&net, "desk")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == laptop_id)
    })
    .await;
    let desk_candidates = edge(&net, "desk")
        .candidates()
        .into_iter()
        .map(|advert| (advert.name, advert.scope))
        .collect::<Vec<_>>();
    assert_eq!(
        desk_candidates,
        vec![("laptop".to_owned(), SCOPE.to_owned())]
    );
    assert!(
        edge(&net, "stranger").candidates().is_empty(),
        "a host in another scope lists none of these"
    );
    println!("desk candidates: {desk_candidates:?}; stranger candidates: []");

    // The device identity the front door reports is the one pairing pins.
    let identity = door(&net, "desk")
        .await
        .get_device_identity(ProfileRequest {
            profile_id: id(&net, "desk"),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(identity.host_id, desk_id.as_bytes());
    assert_eq!(identity.pubkey, edge(&net, "desk").public_key());

    let peer = pair(&net, "laptop", "desk").await;
    assert_eq!(peer.host_id, desk_id.as_bytes());
    assert_eq!(peer.name, "desk");
    println!(
        "laptop paired with {} over {:?}",
        peer.name,
        wire::PeerVia::Direct
    );

    // Both sides now trust each other, and the laptop dialled the desk.
    until("the desk to trust the laptop", || async {
        edge(&net, "desk").is_trusted(laptop_id)
    })
    .await;
    until("a direct link both ways", || async {
        edge(&net, "desk").via(laptop_id).await == HostVia::Direct
            && edge(&net, "laptop").via(desk_id).await == HostVia::Direct
    })
    .await;
    let desk_peers = door(&net, "desk")
        .await
        .list_peers(ProfileRequest {
            profile_id: id(&net, "desk"),
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
    let from_laptop = peer_inventory_hosts(&edge(&net, "laptop"), desk_id).await;
    let from_desk = peer_inventory_hosts(&edge(&net, "desk"), laptop_id).await;
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
    let refused = edge(&net, "laptop")
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
    edge(&net, "stranger")
        .trust(&edge(&net, "desk"))
        .await
        .unwrap();
    edge(&net, "stranger").dial(desk_id, edge(&net, "desk").lan_addr().unwrap());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(edge(&net, "desk").via(stranger_id).await, HostVia::Offline);
    assert_eq!(edge(&net, "stranger").via(desk_id).await, HostVia::Offline);
    let desk_addr = edge(&net, "desk").lan_addr().unwrap();
    let outside_pairing = match edge(&net, "stranger").unpinned_channel(desk_addr).await {
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
    door(&net, "desk")
        .await
        .start_pairing(ProfileStartPairingRequest {
            profile_id: id(&net, "desk"),
            pairing: Some(StartPairingRequest {
                mode: start_pairing_request::Mode::Qr as i32,
                ..StartPairingRequest::default()
            }),
            ..ProfileStartPairingRequest::default()
        })
        .await
        .unwrap();
    let in_pairing = wire::peer_service_client(
        edge(&net, "stranger")
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
    door(&net, "desk")
        .await
        .cancel_pairing(wire::ProfileOperation {
            profile_id: id(&net, "desk"),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap();
    println!(
        "stranger refused: pinned dial never links; unpinned PeerService call {outside_pairing:?}, \
         in pairing mode {in_pairing:?}"
    );
    assert!(!edge(&net, "desk").is_trusted(stranger_id));

    // Unpairing removes the laptop's key, closes its link, and a redial
    // with the now unknown key is refused.
    let removed = door(&net, "desk")
        .await
        .unpair(ProfileUnpairRequest {
            profile_id: id(&net, "desk"),
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
        edge(&net, "desk").via(laptop_id).await == HostVia::Offline
            && edge(&net, "laptop").via(desk_id).await == HostVia::Offline
    })
    .await;
    let gone = door(&net, "desk")
        .await
        .get_peer(ProfileGetPeerRequest {
            profile_id: id(&net, "desk"),
            peer: host_ref(laptop_id),
        })
        .await
        .expect_err("an unpaired host is no peer");
    assert_eq!(gone.code(), tonic::Code::NotFound);
    edge(&net, "laptop").dial(desk_id, edge(&net, "desk").lan_addr().unwrap());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(edge(&net, "desk").via(laptop_id).await, HostVia::Offline);
    println!("desk unpaired the laptop; its redial is refused");

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn trusted_edges_link_in_process_until_the_link_is_severed() {
    // Off the network, so nothing redials once the link is cut: two hosts
    // that find each other would link again directly.
    let net = Net::start(Topology::new().host("one").host("two"))
        .await
        .unwrap();
    let (one_id, two_id) = (edge(&net, "one").host_id(), edge(&net, "two").host_id());

    assert!(
        edge(&net, "one")
            .link_in_process(&edge(&net, "two"))
            .is_err(),
        "hosts that do not trust each other are not linked"
    );
    edge(&net, "one").trust(&edge(&net, "two")).await.unwrap();
    edge(&net, "two").trust(&edge(&net, "one")).await.unwrap();
    let link = edge(&net, "one")
        .link_in_process(&edge(&net, "two"))
        .unwrap();
    until("the in-process link", || async {
        edge(&net, "one").via(two_id).await == HostVia::Direct
            && edge(&net, "two").via(one_id).await == HostVia::Direct
    })
    .await;
    assert!(
        peer_inventory_hosts(&edge(&net, "two"), one_id)
            .await
            .contains(&one_id.as_bytes().to_vec())
    );

    link.sever();
    until("the severed link to leave both hosts", || async {
        edge(&net, "one").via(two_id).await == HostVia::Offline
            && edge(&net, "two").via(one_id).await == HostVia::Offline
    })
    .await;

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_signed_in_profile_refreshes_its_relay_credential_on_the_runtime_clock() {
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(lan_host("desk", SCOPE));
    topology.relay.as_mut().unwrap().accounts[0].tier = TierDecl::Free;
    let net = Net::start_with(
        topology,
        NetOptions {
            clock: ClockMode::Driven,
            edge: Some(Arc::new(|_, edge| {
                edge.cloud.free_refresh_interval = Some(Duration::from_secs(60));
            })),
            ..NetOptions::default()
        },
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let host_id = edge(&net, "desk").host_id();
    let unbound = info(&net, "desk").await;
    assert_eq!(unbound.intent, wire::Intent::Unbound as i32);

    let bound = door(&net, "desk")
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(id(&net, "desk")),
            cloud_url: relay.url().to_owned(),
            staged_refresh_token: Relay::login("ada"),
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account")
        .into_inner();
    assert_eq!(bound.intent, wire::Intent::Bound as i32);
    assert_eq!(bound.email, "ada@example.com");
    assert_eq!(bound.account_name, "ada");

    until("the cloud link to connect", || async {
        let info = info(&net, "desk").await;
        info.observed == wire::Observed::Connected as i32 && info.tier == wire::Tier::Free as i32
    })
    .await;
    assert_eq!(relay.links("ada").await, vec![(host_id, 1)]);
    assert_eq!(relay.connects().len(), 1);
    println!("bound as Ada; connected to the relay on the free tier");

    // The account buys Pro. Nothing happens until the runtime's clock
    // reaches the free tier's refresh interval.
    relay.set_tier("ada", Tier::Pro);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(relay.connects().len(), 1);
    net.advance(Duration::from_secs(60)).unwrap();
    until("the refreshed credential to carry the new tier", || async {
        info(&net, "desk").await.tier == wire::Tier::Pro as i32
    })
    .await;
    assert_eq!(relay.connects().len(), 2);
    assert_eq!(
        relay.presented().as_slice(),
        ["relay-ada-1", "relay-ada-2"],
        "the relay saw the first credential in Hello and the second in Reauth"
    );
    println!("clock +60s: Reauth with relay-ada-2, tier Pro");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        relay.connects().len(),
        2,
        "a fresh Pro credential is not refreshed again at once"
    );

    // On Pro the link refreshes five minutes before its credential expires.
    // Past the first credential's expiry on the relay's clock, the link
    // lives on a refreshed one.
    net.advance(Duration::from_secs(550)).unwrap();
    until("the Pro credential's refresh before expiry", || async {
        relay.presented().contains(&"relay-ada-3".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(relay.links("ada").await, vec![(host_id, 1)]);
    assert_eq!(
        info(&net, "desk").await.observed,
        wire::Observed::Connected as i32
    );
    println!("clock +610s: relay-ada-1 expired, the link lives on relay-ada-3");

    // Pausing drops the link and keeps the credential; resuming brings it
    // back; signing out forgets the credential and keeps the binding.
    let paused = door(&net, "desk")
        .await
        .pause_profile(wire::ProfileOperation {
            profile_id: id(&net, "desk"),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(paused.intent, wire::Intent::Paused as i32);
    until("the relay link to go with the pause", || async {
        relay.links("ada").await.is_empty()
    })
    .await;
    door(&net, "desk")
        .await
        .resume_profile(wire::ProfileOperation {
            profile_id: id(&net, "desk"),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap();
    until("the relay link to come back", || async {
        !relay.links("ada").await.is_empty()
    })
    .await;
    let signed_out = door(&net, "desk")
        .await
        .logout_profile(wire::ProfileOperation {
            profile_id: id(&net, "desk"),
            ..wire::ProfileOperation::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(signed_out.intent, wire::Intent::LoggedOut as i32);
    assert_eq!(signed_out.email, "ada@example.com");
    until("the relay link to go with the sign-out", || async {
        relay.links("ada").await.is_empty()
    })
    .await;
    println!("paused, resumed and signed out");

    net.shutdown().await.unwrap();
}
