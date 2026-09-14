//! Chapter 2 — Pairing.
//!
//! Pairing is the out-of-band bootstrap of mutual trust: one SPAKE2
//! protocol whose secret arrives as a typed PIN (over direct TCP or
//! through the cloud) or as a QR-carried 256-bit secret, plus the SSH
//! identity exchange. Pair-mode is local, time-bounded, attempt-capped,
//! and one-shot. (docs/PROTOCOL.md "Identity and trust", invariants N-P-*)

use std::time::Duration;

use testnet::{TestNet, Via};

/// Discovery supplies the direct address, so no account or relay is needed.
#[tokio::test]
async fn pin_pairing_with_a_found_host_needs_no_account() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing().await;
    net.announce(&desktop);
    laptop.sees_pairing_candidate(&desktop).await;
    laptop
        .pair(&desktop)
        .with_found_pin(&pin)
        .await
        .expect("PIN pairing with a found host");

    laptop.trusts(&desktop).await;
    desktop.trusts(&laptop).await;
    laptop.connects_to(&desktop).via_direct().await;
    laptop.can_call(&desktop).await;
    desktop.can_call(&laptop).await;
}

/// A typed socket address is enough to begin pairing. The initiator leaves
/// host_id empty and pins the identity returned by the SPAKE2 handshake.
#[tokio::test]
async fn a_typed_address_learns_the_host_id_from_the_handshake() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let pin = desktop.start_pairing().await;
    laptop.pair(&desktop).with_pin(&pin).await.unwrap();

    laptop.trusts(&desktop).await;
    desktop.trusts(&laptop).await;
}

/// The QR carries routable addresses, so it still works when discovery is
/// suppressed and neither profile has a cloud account.
#[tokio::test]
async fn a_qr_with_addresses_pairs_with_multicast_blocked_and_no_cloud() {
    let net = TestNet::builder()
        .daemon("desktop")
        .outside_discovery("desktop")
        .daemon("phone")
        .start()
        .await;
    let [desktop, phone] = net.daemons(["desktop", "phone"]);

    let qr = desktop.start_qr_pairing().await;
    assert!(!qr.addrs.is_empty(), "the QR must carry listener addresses");
    assert_eq!(qr.cloud_url, None, "an unbound profile has no cloud URL");
    phone.pair(&desktop).with_qr(&qr).await.unwrap();

    phone.trusts(&desktop).await;
    desktop.trusts(&phone).await;
}

/// A stale VPN or virtual-interface address may accept no UDP traffic at all.
/// Pairing gives that candidate one bounded QUIC dial before moving to the
/// responder's working address.
#[tokio::test]
async fn pairing_moves_past_a_silent_candidate_address_promptly() {
    let net = TestNet::builder()
        .daemon("desktop")
        .outside_discovery("desktop")
        .daemon("phone")
        .start()
        .await;
    let [desktop, phone] = net.daemons(["desktop", "phone"]);

    let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut qr = desktop.start_qr_pairing().await;
    qr.addrs.insert(0, blackhole.local_addr().unwrap());

    tokio::time::timeout(Duration::from_secs(5), phone.pair(&desktop).with_qr(&qr))
        .await
        .expect("pairing should move past a silent address within one candidate budget")
        .expect("pair through the later working address");

    phone.trusts(&desktop).await;
    desktop.trusts(&phone).await;
}

/// An untrusted advertisement is offered to local callers as a direct
/// candidate, but never enters the trusted host dial path.
#[tokio::test]
async fn a_found_unpinned_host_is_a_candidate_for_local_callers_only_and_never_dialed_for_a_trusted_channel()
 {
    let net = TestNet::builder()
        .daemon("phone")
        .daemon("desktop")
        .start()
        .await;
    let [phone, desktop] = net.daemons(["phone", "desktop"]);

    net.announce(&desktop);
    phone
        .sees_pairing_candidate_without_trusted_dial(&desktop)
        .await;
    let candidate = phone
        .pairing_candidate_details()
        .await
        .into_iter()
        .find(|candidate| candidate.host.id == desktop.host_id())
        .expect("found host is a pairing candidate");
    assert_eq!(candidate.via, node::PeerVia::Direct);
    assert!(!candidate.addrs.is_empty());
    phone.does_not_trust(&desktop).await;
}

/// Discovery is only a dial hint. A spoof that claims a pinned host id but
/// presents a stranger's key is rejected before any trust is replaced.
#[tokio::test]
async fn an_advertisement_claiming_a_pinned_hosts_id_with_a_strangers_key_is_refused_at_the_handshake_and_the_dialer_sends_no_certificate()
 {
    let net = TestNet::builder()
        .daemon("phone")
        .daemon("desktop")
        .daemon("stranger")
        .trusted_without_discovery("phone", "desktop")
        .outside_discovery("stranger")
        .start()
        .await;
    let [phone, desktop, stranger] = net.daemons(["phone", "desktop", "stranger"]);

    let pin = stranger.start_pairing().await;
    net.announce_as(&stranger, desktop.host_id());
    phone.sees_found_address_for(&desktop).await;

    let error = phone
        .pair(&desktop)
        .with_found_pin(&pin)
        .await
        .expect_err("a discovery claim cannot replace a pinned identity");
    assert!(error.to_string().contains("INVALID_PIN"), "got: {error}");
    phone.trusts(&desktop).await;
    phone.does_not_trust(&stranger).await;
    stranger.does_not_trust(&phone).await;
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

/// QR pairing through the cloud: the scanned 256-bit secret feeds the same
/// SPAKE2 stream the typed PIN does — the secret never crosses the wire —
/// and is consumed by the first success.
#[tokio::test]
async fn qr_pairing_through_the_cloud() {
    let net = TestNet::builder()
        .cloud()
        .daemon("desktop")
        .cloud_only()
        .daemon("phone")
        .cloud_only()
        .start()
        .await;
    let [desktop, phone] = net.daemons(["desktop", "phone"]);
    phone.sees(&desktop).await;

    let qr = desktop.start_qr_pairing().await;
    assert!(
        qr.addrs.is_empty(),
        "a cloud-only responder must not put a direct route in its QR"
    );
    phone
        .pair(&desktop)
        .with_qr(&qr)
        .await
        .expect("QR pairing through the cloud");

    phone.trusts(&desktop).await;
    desktop.trusts(&phone).await;
    phone.connects_to(&desktop).via_cloud().await;
    phone.can_call(&desktop).await;
    desktop.can_call(&phone).await;
    desktop.pair_mode_ends().await; // the one-shot secret is consumed
}

/// SSH pairing exchanges identities over the already-authenticated SSH
/// stream (an in-memory stream here): the initiator stores the SSH target
/// as a reachability, the responder gains trust but no outbound
/// reachability — an incoming SSH session doesn't tell it how to dial back.
///
/// Yet once the initiator's link is up, *both* sides can call: tunnels are
/// opened by sending frames, and frames flow both ways on every link, so
/// the responder calls back over the link its peer established. What stays
/// asymmetric is only dialing — if the link dies, re-establishing it is the
/// initiator's job, because only the initiator holds a reachability.
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

    laptop.can_call(&server).await;
    server.can_call(&laptop).await; // back over the inbound link
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
        .with_pin(&pin.wrong_guess())
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
            .with_pin(&pin.wrong_guess())
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
        .paired("laptop", "desktop", Via::Direct)
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
    // Calls in both directions over the re-paired link prove the replaced
    // key is honored end to end.
    desktop.can_call(&laptop).await;
    laptop.can_call(&desktop).await;
}

/// Sealed name, fingerprint and expiry are available before either side writes
/// trust. Only explicit confirmation commits; that trust survives restart.
#[tokio::test]
async fn pairing_confirm_pin_returns_identity_before_mutual_trust() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.sees(&host).await;
    let client = phone.pairing_admin().await;
    let before = (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk());
    let start = chrono::Utc::now();
    let pin = host.start_pairing().await;
    let pending = client
        .begin_pair_pin(host.host_id(), &pin, &[])
        .await
        .unwrap();
    assert_eq!(pending.host_id, host.host_id());
    assert_eq!(pending.name, "host");
    assert_eq!(
        pending.fingerprint,
        model::public_key_fingerprint(&host.identity_on_disk().1)
    );
    assert!(pending.expires_at > start);
    assert!(pending.expires_at <= chrono::Utc::now() + chrono::Duration::minutes(5));
    phone.does_not_trust(&host).await;
    host.does_not_trust(&phone).await;
    assert_eq!(
        before,
        (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
    );
    println!(
        "pairing pre-trust identity: {}",
        serde_json::json!({
            "host_id": pending.host_id, "name": pending.name, "fingerprint": pending.fingerprint,
            "expires_at": pending.expires_at, "initiator_trust_unchanged": true, "responder_trust_unchanged": true,
        })
    );
    let peer = client.confirm_pair(pending).await.unwrap();
    assert_eq!(peer.host_id, host.host_id());
    assert_eq!(peer.pubkey, host.identity_on_disk().1);
    phone.trusts(&host).await;
    host.trusts(&phone).await;
    net.restart_daemon(&phone).await;
    net.restart_daemon(&host).await;
    phone.can_call(&host).await;
    host.can_call(&phone).await;
    println!("pairing confirmation: mutual trust persisted and calls succeed after restart");
    let client = phone.pairing_admin().await;
    let before = (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk());
    let pin = host.start_pairing().await;
    let pending = client
        .begin_pair_pin(host.host_id(), &pin, &[])
        .await
        .unwrap();
    client.abandon_pair(pending).await.unwrap();
    assert_eq!(
        before,
        (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
    );
    println!("pairing PIN abandonment: existing trust entries remain byte-for-byte unchanged");
}

/// Abandonment is acknowledged after the responder releases the attempt. More
/// cancellations than the PIN guess limit still leave the correct secret usable.
#[tokio::test]
async fn pairing_confirm_qr_can_be_abandoned_then_confirmed() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.sees(&host).await;
    let client = phone.pairing_admin().await;
    let start = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let node::PairingSecret::QrSecret(secret) = start.secret else {
        panic!("expected QR")
    };
    let payload = node::QrPairingPayload {
        host_id: host.host_id(),
        addrs: start.addrs.clone(),
        cloud_url: start.cloud_url,
        secret,
    };
    let before = (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk());
    let mut wrong = payload.clone();
    wrong.secret[0] ^= 1;
    assert!(matches!(
        client.begin_pair_qr(&wrong).await,
        Err(node::PairingError::InvalidPin)
    ));
    wrong.secret.pop();
    assert!(matches!(
        client.begin_pair_qr(&wrong).await,
        Err(node::PairingError::InvalidPin)
    ));
    for _ in 0..6 {
        let pending = client.begin_pair_qr(&payload).await.unwrap();
        assert_eq!(pending.name, "host");
        assert_eq!(pending.host_id, host.host_id());
        assert!(pending.expires_at > chrono::Utc::now());
        assert_eq!(
            before,
            (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
        );
        client.abandon_pair(pending).await.unwrap();
        phone.does_not_trust(&host).await;
        host.does_not_trust(&phone).await;
        assert_eq!(
            before,
            (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
        );
    }
    host.pair_mode_active().await;
    println!(
        "pairing abandonment: responder acknowledged six cancellations; both stores byte-for-byte unchanged"
    );
    let pending = client.begin_pair_qr(&payload).await.unwrap();
    client.confirm_pair(pending).await.unwrap();
    phone.can_call(&host).await;
    host.can_call(&phone).await;
    println!("pairing QR confirmation: mutual trust and calls succeed after cancellation");
}

/// Wrong, malformed, expired and no-longer-active secrets share one error.
/// Expiry while the confirmation is open also leaves both stores untouched.
#[tokio::test]
async fn pairing_confirm_secret_failures_are_indistinguishable() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.sees(&host).await;
    let client = phone.pairing_admin().await;
    let before = (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk());
    let pin = host.start_pairing().await;
    for invalid in [pin.wrong_guess().to_string(), "123".into()] {
        let error = client
            .begin_pair_pin(host.host_id(), &invalid, &[])
            .await
            .unwrap_err();
        assert!(matches!(error, node::PairingError::InvalidPin));
        assert_eq!(error.to_string(), "INVALID_PIN");
    }
    host.cancel_pairing().await;
    let pin = host
        .start_pairing_with_ttl(Duration::from_millis(800))
        .await;
    let pending = client
        .begin_pair_pin(host.host_id(), &pin, &[])
        .await
        .unwrap();
    host.pair_mode_ends().await;
    assert!(matches!(
        client.confirm_pair(pending).await,
        Err(node::PairingError::InvalidPin)
    ));
    let error = client
        .begin_pair_pin(host.host_id(), &pin, &[])
        .await
        .unwrap_err();
    assert!(matches!(error, node::PairingError::InvalidPin));
    phone.does_not_trust(&host).await;
    host.does_not_trust(&phone).await;
    assert_eq!(
        before,
        (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
    );
    println!(
        "pairing failures: wrong, malformed, expired and expired-during-confirmation all InvalidPin; no trust write"
    );
}

/// Forgetting a machine ends trust and live access, but not the relay's word
/// that the machine is online: the device can trust it again by the code that
/// machine prints, over the same relay and without either side reconnecting.
#[tokio::test]
async fn pin_pairing_after_revocation_over_the_cloud() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.sees(&host).await;

    let invitation = host.start_qr_pairing().await;
    phone
        .pair(&host)
        .with_qr(&invitation)
        .await
        .expect("pairing by the machine's own invitation");
    phone.trusts(&host).await;
    host.trusts(&phone).await;

    phone.unpair(&host).await;
    phone.does_not_trust(&host).await;

    let pin = host.start_pairing().await;
    phone
        .pair(&host)
        .with_cloud_pin(&pin)
        .await
        .expect("pairing again by the code the machine prints");
    phone.trusts(&host).await;
    host.trusts(&phone).await;
    phone.can_call(&host).await;
    println!(
        "revocation keeps the relay's claim: a forgotten machine is reachable for pairing again, and pairs by its printed code without either side reconnecting"
    );
}

/// A cloud URL in an invitation cannot supply a route. Both secret formats
/// require the host to be reachable through this device's authenticated relay.
#[tokio::test]
async fn pairing_on_another_cloud_fails_at_the_same_route_boundary_for_pin_and_qr() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .start()
        .await;
    let elsewhere = TestNet::builder()
        .cloud_url("https://other-cloud.example")
        .daemon("host")
        .cloud_only()
        .start()
        .await;
    let phone = net.daemon("phone");
    let host = elsewhere.daemon("host");
    let client = phone.pairing_admin().await;
    let responder = host.pairing_admin().await;
    let before = (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk());
    let pin = host.start_pairing().await;
    let pin_error = client
        .begin_pair_pin(host.host_id(), &pin, &[])
        .await
        .unwrap_err();
    responder.cancel_pairing().await.unwrap();
    let offer = responder.start_qr_pairing().await.unwrap();
    assert_eq!(offer.cloud_url.as_deref(), Some(elsewhere.cloud_url()));
    assert_ne!(offer.cloud_url.as_deref(), Some(net.cloud_url()));
    let node::PairingSecret::QrSecret(secret) = &offer.secret else {
        panic!("expected QR secret")
    };
    let qr =
        node::parse_qr_pairing_payload(&node::encode_qr_pairing_payload(&offer, secret).unwrap())
            .unwrap();
    let qr_error = client.begin_pair_qr(&qr).await.unwrap_err();
    assert!(matches!(pin_error, node::PairingError::Transport(_)));
    assert!(matches!(qr_error, node::PairingError::Transport(_)));
    assert_eq!(pin_error.to_string(), qr_error.to_string());
    assert!(
        pin_error.to_string().contains(
            "Pairing could not reach this host. Check that both devices are online and signed in to the same cloud account."
        ),
        "got: {pin_error}"
    );
    assert_eq!(
        before,
        (phone.trust_bytes_on_disk(), host.trust_bytes_on_disk())
    );
    host.pair_mode_active().await;
    println!(
        "printed code: {pin_error}\nQR invitation: {qr_error}\nBoth trust stores unchanged; host offer still active."
    );
    net.shutdown().await;
    elsewhere.shutdown().await;
}

/// A machine in the same room is paired with on this network even when the
/// relay can see it too — and on an account that has bought nothing, which is
/// the only way that pairing can succeed at all.
///
/// Both routes are open at once here: the machine advertises on this network
/// and is signed in to the same account as the device pairing with it, so the
/// relay has a route to it before any code is typed. The route the pairing
/// takes decides what the machine reads as afterwards — a device that paired
/// through the relay holds a relay link to a machine in the same room, and a
/// free account is told that machine is away — so the direct address must win
/// wherever there is one. The relay would refuse this pairing anyway (see the
/// entitlement chapter), which is the second half of the same rule: on a free
/// account a pairing either goes direct or does not happen.
#[tokio::test]
async fn a_found_machine_the_relay_can_also_see_is_paired_with_directly_on_a_free_account() {
    let net = TestNet::builder()
        .cloud()
        // On this network and on the relay: the default daemon keeps its
        // direct transports, and one cloud account is shared.
        .daemon("workstation")
        .daemon("phone")
        .cloud_tier(node::Tier::Free)
        .start()
        .await;
    let [workstation, phone] = net.daemons(["workstation", "phone"]);
    // The relay's route exists first, so choosing the direct address is a
    // choice and not the only thing left.
    phone.sees(&workstation).await;

    let pin = workstation.start_pairing().await;
    net.announce(&workstation);
    phone.sees_found_address_for(&workstation).await;

    let admin = phone.pairing_admin().await;
    let pending = admin
        .begin_pair_pin(workstation.host_id(), &pin, &[])
        .await
        .expect("a machine on this network authenticates a printed code");
    assert_eq!(
        pending.via,
        node::PeerVia::Direct,
        "the code was authenticated over {:?} with the machine on this network",
        pending.via
    );
    admin.confirm_pair(pending).await.expect("trust is written");

    phone.trusts(&workstation).await;
    workstation.trusts(&phone).await;
    phone.connects_to(&workstation).via_direct().await;
    phone.can_call(&workstation).await;
}
