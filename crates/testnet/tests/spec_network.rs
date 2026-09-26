//! The network edge between whole daemons, past what the first edge
//! specification shows: pairing windows that expire and lock out, keys
//! that do not match, profiles and accounts that share nothing, revocation
//! closing what a link carries, the relay's QUIC carrier and its TCP
//! fallback, credential refresh under a live link, malformed peer calls,
//! and a slow subscriber beside one that keeps up.
//!
//! Every host is a production daemon in this process. Direct links run over
//! loopback QUIC, discovery is the net's scripted bus, and the relay is a
//! production relay server on loopback beside a stand-in cloud, its
//! credentials timed by the net's policy clock.

#![cfg(unix)]

mod support;

use std::time::Duration;

use node::RelayCarrier;
use node::harness::HostVia;
use support::*;
use testnet::{Net, RELAY_HOST, Topology};

#[tokio::test(flavor = "multi_thread")]
async fn hosts_on_one_account_reach_each_other_through_the_relay_over_quic_or_tcp() {
    let mut net = Net::start(Topology::new().relay(&["ada"]).host("desk").host("phone"))
        .await
        .unwrap();
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    let (desk, phone) = (host_id(&net, "desk"), host_id(&net, "phone"));

    // The phone is on a network that eats UDP: its QUIC dial to the relay
    // never answers, and the TCP carrier started a moment later wins.
    net.block_udp("phone", true).unwrap();
    net.sign_in("desk", "ada").await.unwrap();
    net.sign_in("phone", "ada").await.unwrap();
    assert_eq!(carrier(&net, "desk"), Some(RelayCarrier::Quic));
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Tcp));
    println!("desk on the relay over QUIC; phone, UDP blocked, over TCP");

    // One relay joins the two carriers: each host reaches the other and
    // calls it, through a tunnel the relay pipes but cannot read.
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    let from_phone = peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk through the relay");
    let from_desk = peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect("the desk calls the phone through the relay");
    assert!(from_phone.contains(&desk.as_bytes().to_vec()));
    assert!(from_desk.contains(&phone.as_bytes().to_vec()));
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    let mut expected = vec![(desk, 1), (phone, 1)];
    expected.sort();
    assert_eq!(links, expected, "one relay link per host");
    println!("desk and phone call each other through the relay");

    // The relay carries links but serves nothing: a stream addressed to
    // the relay itself is refused.
    let relay_id = net.relay().unwrap().host_id();
    let refused = peer_inventory_hosts(&edge(&net, "phone"), relay_id)
        .await
        .expect_err("the relay itself is not adjacent");
    println!("a call addressed to the relay itself: {:?}", refused.code());

    // The phone remembers the blocked network, and dials TCP alone until
    // the memory expires on the policy clock, even once UDP is back.
    until("the phone to remember UDP is blocked", || async {
        edge(&net, "phone").remembers_udp_blocked(RELAY_HOST)
    })
    .await;
    net.block_udp("phone", false).unwrap();
    net.advance(node::UDP_BLOCKED_MEMORY - Duration::from_millis(1))
        .unwrap();
    assert!(edge(&net, "phone").remembers_udp_blocked(RELAY_HOST));
    reconnect(&net, "phone").await;
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Tcp));
    net.advance(Duration::from_millis(1)).unwrap();
    assert!(!edge(&net, "phone").remembers_udp_blocked(RELAY_HOST));
    reconnect(&net, "phone").await;
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Quic));
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    println!("memory held TCP-only until it expired; the next link rides QUIC");

    net.shutdown().await.unwrap();
}

/// Pauses and resumes the host's profile, which closes its relay link and
/// dials a new one, and waits for the new one.
async fn reconnect(net: &Net, host: &str) {
    let mut door = door(net, host).await;
    door.pause_profile(operation(profile(net, host)))
        .await
        .expect("the profile pauses");
    until(&format!("{host}'s relay link to close"), || async {
        carrier(net, host).is_none()
    })
    .await;
    door.resume_profile(operation(profile(net, host)))
        .await
        .expect("the profile resumes");
    until(&format!("{host}'s relay link to come back"), || async {
        carrier(net, host).is_some()
    })
    .await;
}
