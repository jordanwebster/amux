//! Chapter 3 — Presence.
//!
//! Who can see whom, and as what: cloud presence is per-user and
//! routing-driven, trust-store entries persist through outages as offline
//! hosts, and untrusted-but-online hosts surface only as local pairing
//! candidates. (docs/NETWORKING.md §4.5, §8.11–8.12)

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

/// An untrusted-but-online host is a *local* affair: the daemon's own user
/// sees it as a pairing candidate, while a paired remote caller is refused
/// the pairing-candidate scope outright and never sees the untrusted host
/// in normal inventory either. (docs/NETWORKING.md §8.12)
#[tokio::test]
async fn untrusted_online_hosts_are_pairing_candidates_for_local_callers_only() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .cloud_only() // untrusted: same cloud user, never paired
        .daemon("phone")
        .no_cloud() // paired remote caller; its only path to laptop is direct
        .paired("phone", "laptop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop, phone] = net.daemons(["laptop", "desktop", "phone"]);

    // Local caller: the untrusted-but-online host is offered for pairing.
    laptop.sees_pairing_candidate(&desktop).await;

    // Paired remote caller: the pairing-candidate scope is rejected…
    let error = phone
        .list_pairing_candidates_on(&laptop)
        .await
        .expect_err("remote callers must not see pairing candidates");
    assert!(
        error
            .to_string()
            .contains("only available to local clients"),
        "the rejection should explain itself, got: {error}"
    );

    // …and normal inventory is filtered down to trusted hosts.
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
