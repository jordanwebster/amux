//! Chapter 3 — Presence.
//!
//! Who can see whom, and as what: cloud presence is per-user and
//! routing-driven, trust-store entries persist through outages as offline
//! hosts, and untrusted-but-online hosts surface only as local pairing
//! candidates. (docs/PROTOCOL.md "Routing: two rules"; docs/ARCHITECTURE.md
//! "Service surface map", "The cloud deployment")

use amux::testnet::{TestNet, Via};

/// Two daemons attached to the same cloud user see each other come online —
/// no trust required — and see each other disappear when one goes away.
#[tokio::test]
async fn cloud_attached_daemons_see_each_other_come_online_and_go_offline() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("desktop")
        .cloud_only()
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.sees(&desktop).await; // online through the relay, still untrusted
    desktop.sees(&laptop).await;

    desktop.stop().await;

    // An untrusted host has no trust-store entry to linger as: offline means
    // gone from the inventory.
    laptop.cannot_see(&desktop).await;
}

/// Stopping a cloud connector is a complete teardown operation: it returns
/// only after both ends removed the established link and its derived
/// presence. The testnet connector owns no duplicate socket that can make
/// this pass by force-closing the transport.
#[tokio::test]
async fn connector_teardown_makes_the_relay_and_paired_peer_see_the_daemon_offline() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("phone")
        .cloud_only()
        .paired("laptop", "phone", Via::Cloud)
        .start()
        .await;
    let [laptop, phone] = net.daemons(["laptop", "phone"]);
    let routed_stream = laptop.open_event_stream_to(&phone).await;

    phone.stop_cloud().await;

    net.cloud_relay_sees_offline(&phone).await;
    laptop.sees_offline(&phone).await;
    laptop.cannot_call(&phone).await;
    routed_stream.expect_disconnect().await;
}

/// A cloud-visible untrusted host is a pairing candidate, not a trusted
/// transport target. Discovering it must not open a normal device-mTLS
/// tunnel before pairing has established trust.
#[tokio::test]
async fn untrusted_cloud_pairing_candidates_do_not_trigger_trusted_tunnels() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("desktop")
        .cloud_only()
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop
        .sees_pairing_candidate_without_trusted_dial(&desktop)
        .await;
}

/// A restarting daemon is seen going down, then up again, and is callable
/// once it is back. (`restart()` itself blocks until every peer observed
/// the daemon offline, so the "seen down" half is part of the verb.)
#[tokio::test]
async fn a_restarting_daemon_is_seen_down_then_up_again() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("phone")
        .cloud_only()
        .paired("laptop", "phone", Via::Cloud)
        .start()
        .await;
    let [laptop, phone] = net.daemons(["laptop", "phone"]);

    laptop.sees(&phone).await;

    phone.restart().await; // laptop saw phone go down before this returned

    laptop.sees(&phone).await;
    laptop.can_call(&phone).await;
}

/// A trusted peer that goes offline is still listed — the trust store, not
/// routing, owns its entry — but with `online = false`, and calls to it
/// fail.
#[tokio::test]
async fn a_trusted_but_offline_peer_is_still_listed_as_offline() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("desktop")
        .cloud_only()
        .paired("laptop", "desktop", Via::Cloud)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    desktop.stop().await;

    laptop.sees_offline(&desktop).await;
    laptop.trusts(&desktop).await;
    laptop.cannot_call(&desktop).await;
}

/// Pairing candidates belong to installation administration; profile clients
/// see only the local host and trusted peers.
#[tokio::test]
async fn untrusted_online_hosts_are_absent_from_profile_inventory() {
    let net = TestNet::builder()
        .cloud()
        .installation("laptop")
        .profile("personal")
        .cloud_user("alice")
        .daemon("desktop")
        .cloud_user("alice")
        .cloud_only() // untrusted: same cloud user, never paired
        .daemon("phone")
        .no_cloud() // paired remote caller; its only path to laptop is direct
        .paired("phone", "laptop/personal", Via::Tcp)
        .start()
        .await;
    let laptop = net.installation("laptop").profile("personal");
    let [desktop, phone] = net.daemons(["desktop", "phone"]);

    // The installation front door offers the untrusted host for pairing.
    laptop.sees_pairing_candidate(&desktop).await;

    #[cfg(unix)]
    let local_client = laptop.socket_client().await;
    #[cfg(not(unix))]
    let local_client = laptop.client();
    let local_hosts = local_client.list_hosts().await.unwrap();
    assert!(!local_hosts.iter().any(|host| host.id == desktop.host_id()));
    assert!(local_hosts.iter().any(|host| host.id == laptop.host_id()));

    let hosts = phone
        .list_hosts_on(&laptop)
        .await
        .expect("a paired remote caller may list trusted inventory");
    assert!(
        !hosts.contains(&desktop.host_id()),
        "an untrusted host must never appear in a remote caller's inventory"
    );
    assert!(
        hosts.contains(&phone.host_id()),
        "trusted hosts stay visible to the remote caller"
    );
    let mut local_ids = local_hosts.iter().map(|host| host.id).collect::<Vec<_>>();
    let mut remote_ids = hosts.clone();
    local_ids.sort();
    remote_ids.sort();
    assert_eq!(local_ids.len(), 2);
    assert_eq!(local_ids, remote_ids);
    println!("Local profile ListHosts: {local_hosts:?}");
    println!("Paired tunnel ListHosts IDs: {hosts:?}");
    println!(
        "Front door ListPairingCandidates IDs: {:?}",
        laptop.pairing_candidates().await
    );
    println!(
        "Front door lists the unpaired cloud device for pairing; local profile client and paired tunnel both list only the local host and trusted phone."
    );
}

/// Two cloud users are isolated: the relay scopes presence per user, so one
/// user's daemons never see the other's — not even as pairing candidates.
#[tokio::test]
async fn two_cloud_users_are_isolated_from_each_other() {
    let net = TestNet::builder()
        .cloud()
        .daemon("alice-laptop")
        .cloud_only()
        .daemon("alice-desktop")
        .cloud_only()
        .daemon("bob-laptop")
        .cloud_only()
        .cloud_user("bob")
        .start()
        .await;
    let [alice_laptop, alice_desktop, bob_laptop] =
        net.daemons(["alice-laptop", "alice-desktop", "bob-laptop"]);

    // Presence flowed within alice's account…
    alice_laptop.sees(&alice_desktop).await;
    alice_laptop.sees_pairing_candidate(&alice_desktop).await;

    // …but across accounts, nothing.
    alice_laptop.cannot_see(&bob_laptop).await;
    bob_laptop.cannot_see(&alice_laptop).await;
    bob_laptop.cannot_see(&alice_desktop).await;
    assert!(
        !alice_laptop
            .pairing_candidates()
            .await
            .contains(&bob_laptop.host_id()),
        "another user's daemon must not be offered for pairing"
    );
}

/// A profile stop uses production teardown while the other profiles continue
/// routing over the relay. No testnet socket sever participates in shutdown.
#[tokio::test]
async fn profile_runtime_stop_leaves_other_runtimes_routing() {
    let net = TestNet::builder()
        .cloud()
        .daemon("work")
        .cloud_only()
        .daemon("personal")
        .cloud_only()
        .daemon("peer")
        .cloud_only()
        .paired("work", "peer", Via::Cloud)
        .paired("personal", "peer", Via::Cloud)
        .start()
        .await;
    let [work, personal, peer] = net.daemons(["work", "personal", "peer"]);
    let stopped_stream = peer.open_event_stream_to(&work).await;
    personal.can_call(&peer).await;

    work.stop().await;

    net.cloud_relay_sees_offline(&work).await;
    peer.sees_offline(&work).await;
    peer.cannot_call(&work).await;
    stopped_stream.expect_disconnect().await;
    personal.sees(&peer).await;
    personal.can_call(&peer).await;
    peer.can_call(&personal).await;
}

#[tokio::test]
async fn profile_runtime_stop_closes_direct_links_without_severing() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
    laptop.can_call(&desktop).await;

    desktop.stop().await;

    laptop.sees_offline(&desktop).await;
    laptop.cannot_call(&desktop).await;
}
