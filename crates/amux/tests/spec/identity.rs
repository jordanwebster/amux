//! Chapter 1 — Identity & trust.
//!
//! Every device has a keypair plus a random `host_id`, both generated once
//! and persisted; trust is a strictly local registry bootstrapped only by
//! pairing. (docs/PROTOCOL.md "Identity and trust"; docs/ARCHITECTURE.md
//! "Identity and the trust store")

use amux::testnet::{TestNet, Via};

/// A fresh daemon creates its identity once; both the keypair and the
/// `host_id` survive a restart, and a paired peer's pinned-key mTLS still
/// accepts the restarted daemon.
#[tokio::test]
async fn identity_is_created_once_and_survives_restart() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let (host_id, pubkey) = desktop.identity_on_disk();
    assert_eq!(host_id, desktop.host_id());

    desktop.restart().await;

    assert_eq!(desktop.identity_on_disk(), (host_id, pubkey));
    net.establish_direct(&laptop, &desktop).await;
    laptop.connects_to(&desktop).via_direct().await;
    // The pinned-mTLS handshake must still accept the restarted daemon's
    // persisted key.
    laptop.can_call(&desktop).await;
}

/// Two unpaired daemons cannot talk, even when both are online at the same
/// relay: routed calls fail without trust, and anonymous pairing connections
/// are refused while the responder is not in pair-mode.
#[tokio::test]
async fn unpaired_daemons_cannot_talk() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.sees(&desktop).await; // online at the relay, but untrusted

    laptop
        .lists_agents_on(&desktop)
        .await
        .expect_err("routed calls must fail without mutual trust");
    laptop
        .pair(&desktop)
        .with_pin("123456")
        .await
        .expect_err("anonymous connections are refused while pair-mode is off");
    laptop.does_not_trust(&desktop).await;
    desktop.does_not_trust(&laptop).await;
}

/// Trust is local: unpairing on one side removes only that side's entry,
/// and the other side keeps trusting until it revokes too.
#[tokio::test]
async fn trust_is_local_unpairing_one_side_keeps_the_others_entry() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.unpair(&desktop).await;

    laptop.does_not_trust(&desktop).await;
    desktop.trusts(&laptop).await;
}
