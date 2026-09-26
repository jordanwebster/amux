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

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_host_closes_what_it_holds_open_over_a_direct_link_and_over_the_relay() {
    // The phone reaches the desk directly, with the relay beside it as a
    // fallback; the tablet reaches it through the relay alone.
    let mut net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("desk", "home")
            })
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("phone", "home")
            })
            .host_decl(testnet::HostDecl {
                name: "tablet".to_owned(),
                account: Some("ada".to_owned()),
                ..testnet::HostDecl::default()
            })
            .agent(
                testnet::AgentDecl::new("worker", "desk")
                    .prompt("Watch the build.")
                    .steps(vec![
                        provider_fakes::script::Step::Text {
                            chunks: vec!["Watching.".to_owned()],
                        },
                        provider_fakes::script::Step::TurnEnd,
                    ]),
            ),
        testnet::NetOptions::default(),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let worker = net.agent("worker").unwrap().id;
    pair(&net, "phone", "desk").await;
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    until_via(&net, "tablet", "desk", HostVia::Relay).await;

    // Each holds the worker's session and the desk's inventory open.
    let mut held = Vec::new();
    for host in ["phone", "tablet"] {
        let session = open_session(&edge(&net, host), desk, worker)
            .await
            .unwrap_or_else(|status| panic!("{host} opens the worker's session: {status:?}"));
        let inventory = edge(&net, host)
            .peer(desk)
            .await
            .unwrap()
            .subscribe_inventory(wire::Empty {})
            .await
            .unwrap()
            .into_inner();
        held.push((host, session, inventory));
    }

    // Revoking closes every stream the revoked host holds, at once, and
    // neither route lets it back in: the desk no longer holds its key. The
    // relay may still say the host is online, as it says of any host on the
    // account; saying so grants nothing.
    for (host, mut session, mut inventory) in held {
        let id = host_id(&net, host);
        net.untrust("desk", host).await.unwrap();
        let session_end = ended(&mut session, &format!("{host}'s session")).await;
        let inventory_end = ended(&mut inventory, &format!("{host}'s inventory")).await;
        let refused = open_session(&edge(&net, host), desk, worker)
            .await
            .expect_err("a revoked host opens nothing");
        let unasked = peer_inventory_hosts(&edge(&net, "desk"), id)
            .await
            .expect_err("the desk calls no host it forgot");
        println!(
            "{host} revoked: session {session_end}, inventory {inventory_end}; reopening: {:?}; \
             the desk calling it: {:?}; the desk sees it {:?}",
            refused.code(),
            unasked.code(),
            edge(&net, "desk").via(id).await,
        );
    }
    never("a revoked host gets a call through", async || {
        peer_inventory_hosts(&edge(&net, "phone"), desk)
            .await
            .is_ok()
            || peer_inventory_hosts(&edge(&net, "tablet"), desk)
                .await
                .is_ok()
    })
    .await;

    // The relay's word that the tablet is online is what lets it pair
    // again, by the code the desk shows, with neither side reconnecting:
    // the exchange runs inside a tunnel the relay pipes and cannot read,
    // with a QR secret as well as a PIN.
    let links_before = net.relay().unwrap().links("ada").await;
    let started = start_pairing(&net, "desk", qr_mode()).await.unwrap();
    let Some(wire::start_pairing_response::Secret::QrSecret(secret)) = started.secret else {
        panic!("a QR window hands out a secret");
    };
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::QrSecret(secret),
        Vec::new(),
    )
    .await
    .expect("pairing through the relay");
    assert_eq!(pending.via, wire::PeerVia::Relay as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    until("the tablet's calls to go through again", || async {
        peer_inventory_hosts(&edge(&net, "tablet"), desk)
            .await
            .is_ok()
    })
    .await;
    assert_eq!(net.relay().unwrap().links("ada").await, links_before);
    println!("the tablet paired again through the relay with a QR secret, on the same links");

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_and_accounts_share_no_keys_windows_presence_or_administration() {
    let net = Net::start(
        Topology::new()
            .relay(&["ada", "bob"])
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("desk", "home")
            })
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("laptop", "home")
            })
            // Down the street: on no local network with the others, so the
            // relay is the only way it could find them.
            .host_decl(testnet::HostDecl {
                account: Some("bob".to_owned()),
                ..lan_host("shop", "street")
            }),
    )
    .await
    .unwrap();
    let (desk, laptop, shop) = (
        host_id(&net, "desk"),
        host_id(&net, "laptop"),
        host_id(&net, "shop"),
    );
    let relay = net.relay().unwrap();

    // Two accounts on one relay: each sees its own hosts, and the relay
    // offers no route, and no pairing route, from one to the other.
    let mut ada = relay.links("ada").await;
    ada.sort();
    let mut expected = vec![(desk, 1), (laptop, 1)];
    expected.sort();
    assert_eq!(ada, expected);
    assert_eq!(relay.links("bob").await, vec![(shop, 1)]);
    // Hosts on one account see each other through the relay before any
    // trust, which is what lets them pair there.
    until_via(&net, "desk", "laptop", HostVia::Relay).await;
    never("the relay routes between accounts", async || {
        edge(&net, "shop").via(desk).await != HostVia::Offline
            || edge(&net, "desk").via(shop).await != HostVia::Offline
    })
    .await;
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let across = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing route through another account's relay");
    assert_eq!(across.code(), tonic::Code::Unavailable);
    println!("ada's relay links {ada:?}; bob's shop sees none of them: {across:?}");

    // Pairing at an address the desk hands out is authority of its own,
    // whatever account either host is on.
    let pending = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs.clone(),
    )
    .await
    .expect("pairing across accounts at the desk's address");
    confirm_pair(&net, "shop", pending.token).await.unwrap();
    until_via(&net, "shop", "desk", HostVia::Direct).await;
    peer_inventory_hosts(&edge(&net, "shop"), desk)
        .await
        .expect("the shop calls the desk it paired with");

    // A second profile on the desk's installation is a host of its own:
    // its own id and key, its own pairing window and attempt budget.
    let work = door(&net, "desk")
        .await
        .create_profile(wire::CreateProfileRequest {
            label: Some("work".to_owned()),
            ..wire::CreateProfileRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let work_profile: node::ProfileId = work.id.parse().unwrap();
    let work_edge = net.profile_edge("desk", work_profile).unwrap();
    assert_ne!(work_edge.host_id(), desk);
    assert_ne!(work_edge.public_key(), edge(&net, "desk").public_key());
    let work_start = door(&net, "desk")
        .await
        .start_pairing(wire::ProfileStartPairingRequest {
            profile_id: work.id.clone(),
            pairing: Some(pin_mode()),
            ..wire::ProfileStartPairingRequest::default()
        })
        .await
        .expect("the work profile opens its own window beside the desk's")
        .into_inner();
    let desk_start = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let wrong_for_work = begin_pair(
        &net,
        "laptop",
        Some(work_edge.host_id()),
        begin_pair_request::Secret::Pin(pin_of(&desk_start)),
        work_start.addrs.clone(),
    )
    .await
    .expect_err("the desk's PIN at the work profile");
    assert!(is_invalid_secret(&wrong_for_work));
    for _ in 0..4 {
        begin_pair(
            &net,
            "laptop",
            Some(work_edge.host_id()),
            begin_pair_request::Secret::Pin(wrong_pin(&pin_of(&work_start))),
            work_start.addrs.clone(),
        )
        .await
        .expect_err("a wrong guess at the work profile");
    }
    assert!(
        !work_edge.pairing_active(),
        "five guesses closed work's window"
    );
    assert!(
        edge(&net, "desk").pairing_active(),
        "the desk's window keeps its own budget"
    );
    let pending = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&desk_start)),
        desk_start.addrs.clone(),
    )
    .await
    .unwrap();
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    assert!(
        work_edge.trusted().is_empty(),
        "the desk's pairing commits nothing to work"
    );

    // The laptop's key is pinned by the desk's profile only: dialling the
    // work profile's listener with it gets it nowhere.
    edge(&net, "laptop").trust(&work_edge).await.unwrap();
    edge(&net, "laptop").dial(work_edge.host_id(), work_edge.lan_addr().unwrap());
    never(
        "a key one profile pinned authenticates into another",
        async || {
            edge(&net, "laptop").via(work_edge.host_id()).await != HostVia::Offline
                || work_edge.via(laptop).await != HostVia::Offline
        },
    )
    .await;
    println!(
        "work profile {}: own key, own window, the desk's peers refused",
        work.id
    );

    // Administration is the installation's: a trusted peer's tunnel and a
    // profile's client socket serve neither profiles nor pairing.
    let tunnel = edge(&net, "laptop").channel(desk).await.unwrap();
    let over_tunnel = wire::profile_service_client::ProfileServiceClient::new(tunnel)
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .expect_err("no profile administration over a peer tunnel");
    assert_eq!(over_tunnel.code(), tonic::Code::Unimplemented);
    let socket = node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    )
    .join(node::PROFILE_SOCKET);
    let on_socket = wire::profile_service_client::ProfileServiceClient::new(
        testnet::local_channel(&socket).await.unwrap(),
    )
    .start_pairing(wire::ProfileStartPairingRequest {
        profile_id: profile(&net, "desk"),
        pairing: Some(pin_mode()),
        ..wire::ProfileStartPairingRequest::default()
    })
    .await
    .expect_err("no pairing administration on a profile's client socket");
    assert_eq!(on_socket.code(), tonic::Code::Unimplemented);

    net.shutdown().await.unwrap();
}

/// A host on the relay alone: no listener, no discovery.
fn relay_host(name: &str, account: &str) -> testnet::HostDecl {
    testnet::HostDecl {
        name: name.to_owned(),
        account: Some(account.to_owned()),
        ..testnet::HostDecl::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_free_link_lists_its_hosts_and_opens_nothing_until_a_refresh_brings_pro() {
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(relay_host("desk", "ada"))
        .host_decl(relay_host("phone", "ada"));
    topology.relay.as_mut().unwrap().accounts[0].tier = testnet::TierDecl::Free;
    let mut net = Net::start(topology).await.unwrap();
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    let (desk, phone) = (host_id(&net, "desk"), host_id(&net, "phone"));
    assert_eq!(tier(&net, "desk"), Some(node::Tier::Free));

    // On a free account the relay lists the account's hosts and opens no
    // tunnel between them, for calls or for pairing; the refused pairing
    // leaves the responder's window open, as the secret never reached it.
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    let call = peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect_err("no tunnel on a free account");
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let pairing = begin_pair(
        &net,
        "phone",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing tunnel on a free account");
    assert!(edge(&net, "desk").pairing_active());
    println!(
        "free: listed via the relay; call {:?}; pairing {:?}",
        call.code(),
        pairing.code()
    );

    // The account buys Pro. The desk, where the purchase happened, asks at
    // once and reauthenticates on its live link; the phone has not asked
    // yet, and a Pro host still opens nothing toward a free one.
    net.relay().unwrap().set_tier("ada", node::Tier::Pro);
    let connects = net.relay().unwrap().connects().len();
    assert_eq!(
        edge(&net, "desk").refresh_entitlement().await.unwrap(),
        node::Tier::Pro
    );
    until("the desk's link to carry Pro", || async {
        tier(&net, "desk") == Some(node::Tier::Pro)
    })
    .await;
    assert_eq!(tier(&net, "phone"), Some(node::Tier::Free));
    peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect_err("a Pro link opens nothing toward a free one");

    // The phone asks on its free refresh interval, on the policy clock,
    // and from then on the two call each other, on the same links.
    net.advance(node::FREE_TIER_REFRESH_INTERVAL).unwrap();
    until("the phone's link to carry Pro", || async {
        tier(&net, "phone") == Some(node::Tier::Pro)
    })
    .await;
    assert_eq!(net.relay().unwrap().connects().len(), connects + 2);
    peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk on Pro");
    peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect("the desk calls the phone on Pro");
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    let mut expected = vec![(desk, 1), (phone, 1)];
    expected.sort();
    assert_eq!(links, expected, "Pro arrived on the links already up");
    println!("Pro: the desk at once, the phone after its interval, on the same links");

    // A credential refresh under a live link keeps what the link carries:
    // the phone's inventory of the desk, held open, sees a new agent after
    // the desk's link has reauthenticated on a fresh credential.
    let mut held = edge(&net, "phone")
        .peer(desk)
        .await
        .unwrap()
        .subscribe_inventory(wire::Empty {})
        .await
        .unwrap()
        .into_inner();
    let presented = net.relay().unwrap().presented().len();
    net.advance(testnet::CREDENTIAL_TTL - Duration::from_secs(4 * 60))
        .unwrap();
    until("both links to present fresh credentials", || async {
        net.relay().unwrap().presented().len() >= presented + 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(net.relay().unwrap().links("ada").await.len(), 2);
    let spawned = net
        .spawn(testnet::AgentDecl::new("scout", "desk").prompt("Look around."))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    loop {
        let event = tokio::time::timeout_at(deadline, held.message())
            .await
            .expect("the held inventory to show the new agent")
            .expect("the held inventory is still open")
            .expect("the held inventory is still open");
        if let Some(wire::inventory_event::Of::Agent(agent)) = event.of
            && agent.agent_id == spawned.agent_id
        {
            break;
        }
    }
    println!(
        "credentials refreshed on the live links ({} presented); the held inventory kept streaming",
        net.relay().unwrap().presented().len()
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_peer_requests_and_handshake_floods_are_refused_and_the_link_carries_on() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("stranger", "elsewhere"))
            .agent(testnet::AgentDecl::new("worker", "desk").prompt("Hold on.")),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let worker = net.agent("worker").unwrap().id.as_bytes().to_vec();
    let mut peer = edge(&net, "laptop").peer(desk).await.unwrap();

    // Each call names something that cannot be, or leaves out what it
    // needs; each is answered as the caller's mistake, never as a fault of
    // the desk's, and none of them costs the link.
    let short = vec![1, 2, 3];
    let unknown = uuid::Uuid::new_v4().as_bytes().to_vec();
    let mut answers = Vec::new();
    answers.push((
        "Subscribe with a three-byte agent id",
        peer.subscribe(wire::SubscribeRequest {
            agent_id: short.clone(),
            from: Some(wire::subscribe_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Subscribe to an agent the desk never had",
        peer.subscribe(wire::SubscribeRequest {
            agent_id: unknown.clone(),
            from: Some(wire::subscribe_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Fetch with a three-byte agent id",
        peer.fetch(wire::FetchRequest {
            agent_id: short.clone(),
            before_order: None,
            limit: 10,
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Get with an empty key",
        peer.get(wire::GetRequest {
            agent_id: worker.clone(),
            key: String::new(),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "SendInput with no input",
        peer.send_input(wire::SendInputRequest {
            agent_id: worker.clone(),
            input: None,
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "CreateAgent with no configuration",
        peer.create_agent(wire::CreateAgentRequest::default())
            .await
            .map(|_| ()),
    ));
    answers.push((
        "SendMessage with no sender or recipient",
        peer.send_message(wire::Envelope::default())
            .await
            .map(|_| ()),
    ));
    answers.push((
        "GetBlob with a malformed hash",
        peer.get_blob(wire::GetBlobRequest {
            agent_id: worker.clone(),
            hash: vec![0; 3],
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "StopAgent with a three-byte agent id",
        peer.stop_agent(wire::StopAgentRequest {
            agent_id: short.clone(),
            mode: wire::StopMode::Kill as i32,
        })
        .await
        .map(|_| ()),
    ));
    for (what, answer) in &answers {
        let status = answer.as_ref().expect_err(&format!("{what} is refused"));
        assert!(
            matches!(
                status.code(),
                tonic::Code::InvalidArgument
                    | tonic::Code::NotFound
                    | tonic::Code::PermissionDenied
                    | tonic::Code::FailedPrecondition
            ),
            "{what}: {status:?}"
        );
        println!("{what}: {:?}", status.code());
    }
    assert!(
        peer_inventory_hosts(&edge(&net, "laptop"), desk)
            .await
            .unwrap()
            .contains(&desk.as_bytes().to_vec()),
        "the link carries on"
    );

    // A stranger opening handshake after handshake at the desk's listener
    // is cut off after ten a minute, before the desk spends anything on
    // them, while the laptop's link stays up.
    let desk_addr = edge(&net, "desk").lan_addr().unwrap();
    let mut answered = 0;
    let mut ignored = 0;
    for _ in 0..12 {
        match tokio::time::timeout(
            Duration::from_secs(1),
            edge(&net, "stranger").unpinned_channel(desk_addr),
        )
        .await
        {
            Ok(Ok(_)) | Ok(Err(_)) if ignored == 0 => answered += 1,
            _ => ignored += 1,
        }
    }
    assert!(ignored >= 2, "{answered} answered, {ignored} ignored");
    assert_eq!(edge(&net, "laptop").via(desk).await, HostVia::Direct);
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop's link is untouched by the flood");
    println!(
        "a flood of 12 handshakes: {answered} answered, {ignored} ignored; the link carries on"
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_that_stops_reading_is_closed_while_one_beside_it_keeps_its_stream() {
    use provider_fakes::script::Step;

    // A small fan-out ring on the desk, and an agent that, once let go,
    // says six hundred things of eight kilobytes each, a couple of
    // milliseconds apart: easy going for a reader that reads, and far more
    // than the ring and every buffer between the desk and one that stopped.
    let net = Net::start_with(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .agent(
                testnet::AgentDecl::new("chatty", "desk")
                    .prompt("Report everything.")
                    .steps(vec![
                        Step::WaitFor { path: "go".into() },
                        Step::Repeat {
                            times: 600,
                            steps: vec![
                                Step::Text {
                                    chunks: vec!["x".repeat(8 * 1024)],
                                },
                                Step::Pause { ms: 2 },
                            ],
                        },
                        Step::Text {
                            chunks: vec!["done".to_owned()],
                        },
                        Step::TurnEnd,
                    ]),
            ),
        testnet::NetOptions {
            launch: Some(std::sync::Arc::new(|host, launch| {
                if host == "desk" {
                    launch.fanout_capacity = 64;
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let chatty = net.agent("chatty").unwrap().id;

    // Two subscriptions from the laptop, each on a stream of its own over
    // the direct link: one reads everything, the other reads nothing.
    let mut stalled = open_session(&edge(&net, "laptop"), desk, chatty)
        .await
        .unwrap();
    let mut reading = open_session(&edge(&net, "laptop"), desk, chatty)
        .await
        .unwrap();
    let reader = tokio::spawn(async move {
        let mut items = 0_usize;
        loop {
            let event = reading
                .message()
                .await
                .map_err(|status| format!("the reader's stream failed: {status:?}"))?
                .ok_or("the reader's stream ended")?;
            match event.of {
                Some(wire::session_event::Of::Lagged(_)) => {
                    return Err("the reader was told it lagged".to_owned());
                }
                Some(wire::session_event::Of::Item(item)) => {
                    items += 1;
                    if item.text == "done" {
                        return Ok(items);
                    }
                }
                _ => {}
            }
        }
    });
    net.open_gate("go").unwrap();
    let items = tokio::time::timeout(Duration::from_secs(120), reader)
        .await
        .expect("the reader keeps up to the end")
        .unwrap()
        .expect("the reader's stream carries everything");

    // The subscriber that stopped reading was told it lagged, and its
    // stream ended there: the desk never waited on it.
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    let end = loop {
        match tokio::time::timeout_at(deadline, stalled.message()).await {
            Err(_) => panic!("the stalled stream never ended"),
            Ok(Ok(Some(event))) => seen.push(event),
            Ok(Ok(None)) => break "ended".to_owned(),
            Ok(Err(status)) => break format!("{:?}", status.code()),
        }
    };
    let lagged = seen
        .iter()
        .position(|event| matches!(event.of, Some(wire::session_event::Of::Lagged(_))))
        .expect("the stalled subscriber is told it lagged");
    assert_eq!(lagged, seen.len() - 1, "nothing follows Lagged");
    println!(
        "the reader saw {items} items to the end; the stalled subscriber got {} events, \
         then Lagged, then its stream {end}",
        seen.len() - 1
    );

    net.shutdown().await.unwrap();
}
