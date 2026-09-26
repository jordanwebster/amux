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
use node::harness::{Advertisement, HostVia};
use support::*;
use testnet::{Net, RELAY_HOST, Topology};
use wire::begin_pair_request;

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

/// The refusal every failed secret reads as, so a guesser learns nothing
/// from which way it failed.
fn is_invalid_secret(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::PermissionDenied && status.message() == "INVALID_PIN"
}

/// A PIN one off from `pin`.
fn wrong_pin(pin: &str) -> String {
    format!("{:06}", (pin.parse::<u32>().unwrap() + 1) % 1_000_000)
}

// Every host in the net dials from 127.0.0.1, and a LAN listener admits ten
// new handshakes a minute from one address, so each part of this story
// pairs with a responder of its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_window_is_one_shot_attempt_capped_and_expires() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("shed", "home"))
            .host_decl(lan_host("attic", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("intruder", "home")),
    )
    .await
    .unwrap();
    let (desk, shed, attic) = (
        host_id(&net, "desk"),
        host_id(&net, "shed"),
        host_id(&net, "attic"),
    );

    // One responder at a time, whatever the secret's form.
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    for second in [pin_mode(), qr_mode()] {
        let refused = start_pairing(&net, "desk", second)
            .await
            .expect_err("a second window while one is open");
        assert_eq!(refused.code(), tonic::Code::FailedPrecondition);
        assert_eq!(refused.message(), "PAIR_MODE_ALREADY_ACTIVE");
    }
    let pin = pin_of(&started);

    // A wrong, a malformed and a wrong-length secret fail alike and commit
    // nothing, and the window stays open.
    for (what, secret) in [
        (
            "a wrong PIN",
            begin_pair_request::Secret::Pin(wrong_pin(&pin)),
        ),
        (
            "a malformed PIN",
            begin_pair_request::Secret::Pin("12ab56".to_owned()),
        ),
        (
            "a QR secret of the wrong length",
            begin_pair_request::Secret::QrSecret(vec![7; 5]),
        ),
    ] {
        let status = begin_pair(&net, "laptop", Some(desk), secret, started.addrs.clone())
            .await
            .expect_err("a bad secret pairs nothing");
        assert!(
            is_invalid_secret(&status),
            "{what} reads as every other refusal, not {status:?}"
        );
    }
    assert!(edge(&net, "desk").trusted().is_empty());
    assert!(edge(&net, "laptop").trusted().is_empty());
    assert!(edge(&net, "desk").pairing_active());

    // The correct PIN, at an address alone: the initiator learns whom it
    // reached from the exchange, and nothing is trusted before the person
    // confirms it. Abandoning releases the attempt, so more abandoned
    // attempts than the guess cap still leave the PIN usable.
    let now_ms = chrono::Utc::now().timestamp_millis();
    for _ in 0..6 {
        let pending = begin_pair(
            &net,
            "laptop",
            None,
            begin_pair_request::Secret::Pin(pin.clone()),
            started.addrs.clone(),
        )
        .await
        .expect("the correct PIN");
        let peer = pending.peer.clone().unwrap();
        assert_eq!(peer.host_id, desk.as_bytes());
        assert_eq!(peer.name, "desk");
        assert_eq!(peer.pubkey, edge(&net, "desk").public_key());
        assert!(peer.expires_at_unix_ms > now_ms);
        assert!(peer.expires_at_unix_ms <= now_ms + 5 * 60 * 1000 + 1000);
        assert!(edge(&net, "desk").trusted().is_empty());
        assert!(edge(&net, "laptop").trusted().is_empty());
        door(&net, "laptop")
            .await
            .abandon_pair(wire::ProfilePendingPairRequest {
                profile_id: profile(&net, "laptop"),
                pairing: Some(wire::PendingPairRequest {
                    token: pending.token,
                }),
                ..wire::ProfilePendingPairRequest::default()
            })
            .await
            .expect("the person abandons the pairing");
    }
    assert!(edge(&net, "desk").trusted().is_empty());
    println!(
        "three bad secrets refused alike; six abandoned attempts left the PIN usable and \
         nothing trusted"
    );
    let pending = begin_pair(
        &net,
        "laptop",
        None,
        begin_pair_request::Secret::Pin(pin.clone()),
        started.addrs.clone(),
    )
    .await
    .unwrap();
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    until("the desk to trust the laptop", || async {
        edge(&net, "desk").is_trusted(host_id(&net, "laptop"))
    })
    .await;

    // The first success consumes the PIN: the window closes and a second
    // machine cannot race in on it.
    assert!(!edge(&net, "desk").pairing_active());
    let late = begin_pair(
        &net,
        "intruder",
        Some(desk),
        begin_pair_request::Secret::Pin(pin),
        started.addrs.clone(),
    )
    .await
    .expect_err("a consumed PIN pairs nothing");
    assert!(!edge(&net, "desk").is_trusted(host_id(&net, "intruder")));
    println!("the PIN paired the laptop once; the intruder's late try: {late:?}");

    // Five wrong guesses close the window; after that the right PIN fails
    // too until someone opens a new one.
    let started = start_pairing(&net, "shed", pin_mode()).await.unwrap();
    let pin = pin_of(&started);
    for _ in 0..5 {
        let status = begin_pair(
            &net,
            "intruder",
            Some(shed),
            begin_pair_request::Secret::Pin(wrong_pin(&pin)),
            started.addrs.clone(),
        )
        .await
        .expect_err("a wrong guess");
        assert!(is_invalid_secret(&status), "{status:?}");
    }
    assert!(!edge(&net, "shed").pairing_active());
    let closed = begin_pair(
        &net,
        "intruder",
        Some(shed),
        begin_pair_request::Secret::Pin(pin),
        started.addrs.clone(),
    )
    .await
    .expect_err("the right PIN after the cap");
    assert!(edge(&net, "shed").trusted().is_empty());
    println!("five wrong guesses closed the window; the right PIN then: {closed:?}");

    // A window lasts its time and no longer, and a new one opens after it.
    let started = start_pairing(
        &net,
        "attic",
        wire::StartPairingRequest {
            ttl_seconds: Some(1),
            ..pin_mode()
        },
    )
    .await
    .unwrap();
    assert_eq!(started.ttl_seconds, 1);
    until("the window to expire", || async {
        !edge(&net, "attic").pairing_active()
    })
    .await;
    begin_pair(
        &net,
        "intruder",
        Some(attic),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs.clone(),
    )
    .await
    .expect_err("an expired PIN");
    let started = start_pairing(&net, "attic", pin_mode())
        .await
        .expect("a new window after expiry");

    // A host cannot pair with itself: by its own id before anything is
    // dialled, and by the key it meets when it dials its own listener.
    for named in [Some(attic), None] {
        let refused = begin_pair(
            &net,
            "attic",
            named,
            begin_pair_request::Secret::Pin(pin_of(&started)),
            started.addrs.clone(),
        )
        .await
        .expect_err("pairing with itself");
        assert_eq!(refused.code(), tonic::Code::InvalidArgument);
        assert_eq!(refused.message(), "SELF_PAIRING");
    }
    assert!(edge(&net, "attic").trusted().is_empty());
    println!("the window expired after its second; self-pairing refused by id and by key");

    net.shutdown().await.unwrap();
}

/// The desk's advertisement as its own daemon makes it, at `addr`.
fn advert(net: &Net, host: &str, addr: std::net::SocketAddr, scope: &str) -> Advertisement {
    Advertisement {
        host_id: host_id(net, host),
        name: host.to_owned(),
        version: wire::PROTOCOL_VERSION,
        addrs: vec![addr],
        scope: scope.to_owned(),
    }
}

/// Holds for a second that `check` never becomes true.
async fn never(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline {
        assert!(!check().await, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_does_not_match_the_pinned_one_is_refused() {
    let mut net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("impostor", "home")),
    )
    .await
    .unwrap();
    let (desk, laptop, impostor) = (
        host_id(&net, "desk"),
        host_id(&net, "laptop"),
        host_id(&net, "impostor"),
    );
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let desk_key = edge(&net, "desk").public_key().to_vec();

    // A machine on the network that nobody paired is a candidate to pair
    // with, and nothing more: it is never dialled for a trusted link.
    assert!(
        edge(&net, "laptop")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == impostor)
    );
    never("the laptop links to an unpaired machine", async || {
        edge(&net, "laptop").via(impostor).await != HostVia::Offline
    })
    .await;

    // With the desk gone, a machine claims the desk's id at its own
    // address. The laptop dials it as the desk and meets a key it never
    // pinned: no link, and its trust in the desk's real key stands.
    net.stop_daemon("desk").await.unwrap();
    let impostor_addr = edge(&net, "impostor").lan_addr().unwrap();
    net.discovery()
        .announce_unchecked(advert(&net, "desk", impostor_addr, "home"));
    until(
        "the laptop to dial the claimed address and fail",
        || async {
            edge(&net, "laptop")
                .last_dial_error(desk)
                .await
                .is_some_and(|error| error.contains(&impostor_addr.to_string()))
        },
    )
    .await;
    // The dialer checks the answering key before it presents its own, so
    // the stranger never sees the laptop's certificate.
    let refused = edge(&net, "laptop").last_dial_error(desk).await.unwrap();
    assert!(
        refused.contains(&format!("{impostor_addr}: QUIC handshake failed"))
            && refused.contains("invalid peer certificate"),
        "{refused}"
    );
    never(
        "the laptop links to a stranger claiming the desk's id",
        async || edge(&net, "laptop").via(desk).await != HostVia::Offline,
    )
    .await;
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .expect("the laptop still trusts the desk");
    assert_eq!(pinned.2, desk_key, "the pinned key is unchanged");
    assert!(!edge(&net, "impostor").is_trusted(laptop));
    println!("a stranger advertising the desk's id at {impostor_addr} was refused: {refused}");
    net.discovery().withdraw_host(desk);

    // The desk comes back with a new key under the same id. The laptop's
    // pinned key no longer matches, so neither side can link, until the two
    // pair again and the new key replaces the old.
    let key = node::harness::device_key_path(&node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    ));
    std::fs::remove_file(&key).unwrap();
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(host_id(&net, "desk"), desk);
    let rotated = edge(&net, "desk").public_key().to_vec();
    assert_ne!(rotated, desk_key, "a new key");
    never("the old pinned key links to the rotated desk", async || {
        edge(&net, "laptop").via(desk).await != HostVia::Offline
            || edge(&net, "desk").via(laptop).await != HostVia::Offline
    })
    .await;
    assert!(
        peer_inventory_hosts(&edge(&net, "laptop"), desk)
            .await
            .is_err()
    );
    pair(&net, "desk", "laptop").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .unwrap();
    assert_eq!(pinned.2, rotated, "pairing again replaced the pinned key");
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop calls the rotated desk");
    peer_inventory_hosts(&edge(&net, "desk"), laptop)
        .await
        .expect("the rotated desk calls the laptop");
    println!("the rotated desk was refused on its old key and linked again after pairing");

    net.shutdown().await.unwrap();
}

/// The direct addresses `host` keeps for `peer`.
fn stored_addrs(net: &Net, host: &str, peer: &str) -> Vec<String> {
    let peer = host_id(net, peer);
    edge(net, host)
        .list_peers()
        .unwrap()
        .into_iter()
        .find(|entry| entry.host_id == peer.as_bytes())
        .expect("a paired host")
        .reachabilities
        .into_iter()
        .filter_map(|reachability| match reachability.kind {
            Some(wire::peer_reachability::Kind::Direct(direct)) => Some(direct.addrs),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_advertises_a_listener_and_finds_a_paired_host_at_its_new_address() {
    // The phone listens for nothing, as a phone does: it dials, and only
    // what discovery finds or it stored tells it where.
    let mut net = Net::start_with(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("phone", "home"))
            .host_decl(testnet::HostDecl {
                lan: false,
                ..lan_host("quiet", "home")
            }),
        testnet::NetOptions {
            edge: Some(std::sync::Arc::new(|host, edge| {
                if host == "phone" {
                    edge.lan = None;
                    edge.dial = true;
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    let (desk, phone, quiet) = (
        host_id(&net, "desk"),
        host_id(&net, "phone"),
        host_id(&net, "quiet"),
    );

    // Only a profile that listens advertises.
    until("the phone to find the desk", || async {
        edge(&net, "phone")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == desk)
    })
    .await;
    let advertised = |host| {
        edge(&net, "desk")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == host)
    };
    assert!(!advertised(quiet), "a profile without a listener");
    assert!(!advertised(phone), "a phone listens for nothing");

    // Trusted after the desk was found, the phone waits for the network to
    // name the desk again, and dials it the moment it does.
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    never(
        "the phone to dial before the desk is found again",
        async || edge(&net, "phone").via(desk).await != HostVia::Offline,
    )
    .await;
    let first = edge(&net, "desk").lan_addr().unwrap();
    net.discovery()
        .announce(advert(&net, "desk", first, "home"));
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    assert_eq!(stored_addrs(&net, "phone", "desk"), vec![first.to_string()]);
    println!("the desk found at {first}: the phone dialled it at once and stored the address");

    // The desk stops and withdraws, and comes back on another port while
    // its old one answers nothing. The address discovery finds is dialled
    // before the stale one the phone stored, and replaces it.
    net.stop_daemon("desk").await.unwrap();
    until("the desk's advertisement to go", || async {
        !edge(&net, "phone")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == desk)
    })
    .await;
    let _held = std::net::UdpSocket::bind(first).expect("the desk's old port");
    net.restart_daemon("desk").await.unwrap();
    let second = edge(&net, "desk").lan_addr().unwrap();
    assert_ne!(second, first);
    let started = tokio::time::Instant::now();
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the found address was dialled before the stale one timed out"
    );
    assert_eq!(
        stored_addrs(&net, "phone", "desk"),
        vec![second.to_string()]
    );
    peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk at its new address");
    println!("the desk back at {second}; the stale {first} was never waited on");

    net.shutdown().await.unwrap();
}
