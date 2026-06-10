//! Chapter 2 — Pairing.
//!
//! Pairing is the out-of-band bootstrap of mutual trust: PIN (SPAKE2) over
//! direct TCP or through the cloud, QR one-shot token through the cloud,
//! and the SSH identity exchange. Pair-mode is local, time-bounded,
//! attempt-capped, and one-shot. (docs/NETWORKING.md §4.3, §5,
//! invariants N-P-*)

use std::time::Duration;

use amux::testnet::{Pin, TestNet, Via};

/// A PIN that is definitely not `pin`, still six digits.
fn wrong_pin(pin: &Pin) -> String {
    let first = pin.as_bytes()[0];
    let flipped = if first == b'9' {
        '0'
    } else {
        (first + 1) as char
    };
    format!("{flipped}{}", &pin[1..])
}

/// PIN pairing over direct TCP establishes mutual trust; afterwards both
/// sides can call each other (the initiator over its new direct link, the
/// responder back through the relay they share).
#[tokio::test]
async fn pin_pairing_over_direct_tcp_establishes_mutual_trust() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing().await;
    laptop
        .pair(&desktop)
        .with_pin(&pin)
        .await
        .expect("PIN pairing over direct TCP");

    laptop.trusts(&desktop).await;
    desktop.trusts(&laptop).await;
    laptop.connects_to(&desktop).via_direct().await;
    laptop.can_call(&desktop).await;
    desktop.can_call(&laptop).await;
}

/// PIN pairing works between peers that share only the relay: SPAKE2 runs
/// inside a cloud-routed tunnel the relay cannot read.
#[tokio::test]
async fn pin_pairing_through_the_cloud() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("desktop")
        .cloud_only()
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
    laptop.sees(&desktop).await;

    let pin = desktop.start_pairing().await;
    laptop
        .pair(&desktop)
        .with_cloud_pin(&pin)
        .await
        .expect("PIN pairing through the cloud");

    laptop.trusts(&desktop).await;
    desktop.trusts(&laptop).await;
    laptop.can_call(&desktop).await;
    desktop.can_call(&laptop).await;
}

/// QR pairing through the cloud: the initiator verifies the responder's
/// QR-advertised pubkey at the TLS layer and presents the one-shot token;
/// the token is consumed by the first success.
#[tokio::test]
async fn qr_pairing_through_the_cloud() {
    let net = TestNet::builder()
        .cloud()
        .daemon("desktop")
        .daemon("phone")
        .cloud_only()
        .start()
        .await;
    let [desktop, phone] = net.daemons(["desktop", "phone"]);
    phone.sees(&desktop).await;

    let qr = desktop.start_qr_pairing().await;
    phone
        .pair(&desktop)
        .with_qr(&qr)
        .await
        .expect("QR pairing through the cloud");

    phone.trusts(&desktop).await;
    desktop.trusts(&phone).await;
    phone.can_call(&desktop).await;
    desktop.can_call(&phone).await;
    desktop.pair_mode_ends().await; // the one-shot token is consumed
}

/// SSH pairing exchanges identities over the already-authenticated SSH
/// stream (an in-memory stream here): the initiator stores the SSH target
/// as a reachability, the responder gains trust but no outbound
/// reachability — an incoming SSH session doesn't tell it how to dial back.
#[tokio::test]
async fn ssh_pairing_gives_the_responder_trust_but_no_reachability() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("server")
        .start()
        .await;
    let [laptop, server] = net.daemons(["laptop", "server"]);

    laptop.pair(&server).over_ssh().await.expect("SSH pairing");

    laptop.trusts(&server).await;
    server.trusts_without_reachability(&laptop).await;
}

/// A wrong PIN fails with the same opaque INVALID_PIN as every other
/// pairing failure mode, commits nothing, and leaves pair-mode active: the
/// correct PIN still works afterwards.
#[tokio::test]
async fn wrong_pin_fails_opaquely_and_the_correct_pin_still_works() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing().await;
    let error = laptop
        .pair(&desktop)
        .with_pin(&wrong_pin(&pin))
        .await
        .expect_err("a wrong PIN must not pair");
    assert!(
        error.to_string().contains("INVALID_PIN"),
        "wrong PIN must fail with the opaque INVALID_PIN, got: {error}"
    );
    laptop.does_not_trust(&desktop).await;
    desktop.does_not_trust(&laptop).await;

    laptop
        .pair(&desktop)
        .with_pin(&pin)
        .await
        .expect("the correct PIN still works after a failed guess");
    laptop.trusts(&desktop).await;
    desktop.trusts(&laptop).await;
}

/// Five failed PIN guesses cancel pair-mode; after that even the correct
/// PIN is refused until the user starts pairing again.
#[tokio::test]
async fn five_pin_failures_cancel_pair_mode() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing().await;
    for _ in 0..5 {
        laptop
            .pair(&desktop)
            .with_pin(&wrong_pin(&pin))
            .await
            .expect_err("a wrong PIN must not pair");
    }

    desktop.pair_mode_ends().await;
    laptop
        .pair(&desktop)
        .with_pin(&pin)
        .await
        .expect_err("the correct PIN is refused once the attempt cap cancelled pair-mode");
    desktop.does_not_trust(&laptop).await;
}

/// The PIN is one-shot: the first successful pairing consumes it and ends
/// pair-mode, so a second peer cannot race in on the same PIN.
#[tokio::test]
async fn the_pin_is_consumed_by_the_first_successful_pairing() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .daemon("intruder")
        .start()
        .await;
    let [laptop, desktop, intruder] = net.daemons(["laptop", "desktop", "intruder"]);

    let pin = desktop.start_pairing().await;
    laptop.pair(&desktop).with_pin(&pin).await.unwrap();

    desktop.pair_mode_ends().await;
    intruder
        .pair(&desktop)
        .with_pin(&pin)
        .await
        .expect_err("a consumed PIN must not pair a second peer");
    desktop.does_not_trust(&intruder).await;
}

/// Pair-mode expires after its TTL: the PIN stops working and a new
/// responder can start. (Driven through a short-TTL test seam; the real
/// `StartPairing` RPC always uses the production ~5-minute TTL.)
#[tokio::test]
async fn pair_mode_expires_after_its_ttl() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing_with_ttl(Duration::from_secs(2)).await;
    desktop.pair_mode_active().await;

    desktop.pair_mode_ends().await; // expiry, with no admin action
    laptop
        .pair(&desktop)
        .with_pin(&pin)
        .await
        .expect_err("the PIN must not work after pair-mode expired");
    desktop.start_pairing().await; // a fresh responder may start now
}

/// Starting a second responder while pair-mode is active fails with a
/// useful error, for PIN and QR alike; cancelling clears the way.
#[tokio::test]
async fn a_second_responder_while_pair_mode_is_active_is_refused() {
    let net = TestNet::builder().cloud().daemon("desktop").start().await;
    let desktop = net.daemon("desktop");

    let _pin = desktop.start_pairing().await;

    let error = desktop.try_start_pairing().await.unwrap_err();
    assert!(
        error.to_string().contains("PAIR_MODE_ALREADY_ACTIVE"),
        "second PIN responder must explain itself, got: {error}"
    );
    let error = desktop.try_start_qr_pairing().await.unwrap_err();
    assert!(
        error.to_string().contains("PAIR_MODE_ALREADY_ACTIVE"),
        "second QR responder must explain itself, got: {error}"
    );

    desktop.cancel_pairing().await;
    desktop.pair_mode_ends().await;
    desktop.start_qr_pairing().await;
}

/// Self-pairing is rejected: by host_id before any flow starts, and by the
/// exchanged identity when a daemon ends up talking SPAKE2 to itself.
#[tokio::test]
async fn self_pairing_is_rejected() {
    let net = TestNet::builder().daemon("solo").start().await;
    let solo = net.daemon("solo");

    let pin = solo.start_pairing().await;

    let error = solo.pair(&solo).with_cloud_pin(&pin).await.unwrap_err();
    assert!(
        error.to_string().contains("SELF_PAIRING"),
        "cloud PIN self-pairing must be rejected by host_id, got: {error}"
    );

    let error = solo.pair(&solo).with_pin(&pin).await.unwrap_err();
    assert!(
        error.to_string().contains("SELF_PAIRING"),
        "direct self-pairing must be rejected by the exchanged identity, got: {error}"
    );

    solo.does_not_trust(&solo).await;
}

/// Re-pairing a known host after key rotation: the old pinned key stops
/// working the moment the rotated daemon comes back, and re-pairing
/// replaces the trust entry's pubkey so calls flow again.
#[tokio::test]
async fn re_pairing_a_rotated_key_replaces_the_old_entry() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
    laptop.can_call(&desktop).await;
    let (_, old_pubkey) = desktop.identity_on_disk();

    desktop.restart_with_new_key().await;

    let (_, new_pubkey) = desktop.identity_on_disk();
    assert_ne!(old_pubkey, new_pubkey, "rotation must mint a fresh keypair");
    laptop.cannot_see(&desktop).await; // the old-key link died with the restart
    laptop
        .lists_agents_on(&desktop)
        .await
        .expect_err("the old pinned key must not reach the rotated daemon");

    let pin = laptop.start_pairing().await;
    desktop
        .pair(&laptop)
        .with_pin(&pin)
        .await
        .expect("re-pairing the rotated daemon");

    laptop.trusts_current_key_of(&desktop).await;
    desktop.connects_to(&laptop).via_direct().await;
    // Only the re-pairing dialer holds a route in this LAN-only topology;
    // its calls prove the replaced key is honored end to end.
    desktop.can_call(&laptop).await;
}
