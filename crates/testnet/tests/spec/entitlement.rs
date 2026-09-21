//! Relay entitlement belongs to the authenticated cloud link. Tokens must
//! name a known tier, and a same-user refresh replaces the tier observed by
//! the already-established link.

use node::{PairingError, ProtocolError, Tier};
use testnet::{TestNet, Via, link_tier_across_reauth, relay_refuses_token_without_tier};

/// The relay fails closed when a signed token omits the entitlement claim;
/// accepting such a token would silently grant an undefined account tier.
#[tokio::test]
async fn a_token_without_a_tier_is_refused_at_the_relay() {
    assert!(relay_refuses_token_without_tier().await);
}

/// Entitlement is live link state rather than handshake-only metadata, so a
/// subscription change takes effect when the same account reauthenticates.
#[tokio::test]
async fn a_reauth_that_changes_the_tier_takes_effect_on_the_link() {
    assert_eq!(link_tier_across_reauth().await, (Tier::Free, Tier::Pro));
}

/// The device completing a purchase refreshes immediately. Other free
/// devices use the same live-link reauth path on the bounded background
/// cadence, so neither daemon nor cloud link needs a restart.
#[tokio::test]
async fn a_free_daemon_refreshes_and_picks_up_pro_without_a_restart() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_user("alice")
        .cloud_only()
        .cloud_tier(Tier::Free)
        .daemon("desktop")
        .cloud_user("alice")
        .cloud_only()
        .cloud_tier(Tier::Free)
        .paired("phone", "desktop", Via::Cloud)
        .start()
        .await;
    let [phone, desktop] = net.daemons(["phone", "desktop"]);
    let phone_links = phone.cloud_link_ids().await;
    let desktop_links = desktop.cloud_link_ids().await;

    assert_eq!(
        phone.refused_call_error(&desktop).await,
        ProtocolError::PaymentRequired
    );
    net.cloud_user_tier("alice", Tier::Pro);
    assert_eq!(phone.refresh_entitlement().await, Tier::Pro);
    assert_eq!(
        phone.refused_call_error(&desktop).await,
        ProtocolError::PaymentRequired
    );
    assert_eq!(
        desktop.refused_call_error(&phone).await,
        ProtocolError::PaymentRequired
    );

    let cadence = node::harness::FREE_TIER_REFRESH_INTERVAL;
    net.advance(cadence - std::time::Duration::from_nanos(1));
    assert_eq!(
        desktop.refused_call_error(&phone).await,
        ProtocolError::PaymentRequired
    );

    net.advance(std::time::Duration::from_nanos(1));
    phone.can_call(&desktop).await;
    desktop.can_call(&phone).await;

    net.advance(std::time::Duration::from_nanos(1));
    phone.can_call(&desktop).await;
    desktop.can_call(&phone).await;

    assert_eq!(phone.cloud_link_ids().await, phone_links);
    assert_eq!(desktop.cloud_link_ids().await, desktop_links);
}

/// A free account retains cloud presence, including the route and pairing
/// inventory, but the relay refuses its first attempt to open a tunnel.
#[tokio::test]
async fn a_free_link_sees_its_hosts_and_gets_no_tunnels() {
    let net = TestNet::builder()
        .cloud()
        .daemon("free-laptop")
        .cloud_only()
        .cloud_tier(Tier::Free)
        .daemon("desktop")
        .cloud_only()
        .paired("free-laptop", "desktop", Via::Cloud)
        .start()
        .await;
    let [free_laptop, desktop] = net.daemons(["free-laptop", "desktop"]);

    free_laptop.sees(&desktop).await;
    free_laptop.connects_to(&desktop).via_cloud().await;
    assert_eq!(
        free_laptop.refused_call_error(&desktop).await,
        ProtocolError::PaymentRequired
    );
    free_laptop.has_no_active_tunnel_to(&desktop).await;
}

/// Pairing uses the same tunnel gate as ordinary calls. The responder keeps
/// its pairing window, because the relay refused the tunnel before the
/// one-shot secret reached it.
#[tokio::test]
async fn pairing_through_the_relay_on_a_free_link_is_refused_by_the_relay() {
    let net = TestNet::builder()
        .cloud()
        .daemon("free-phone")
        .cloud_only()
        .cloud_tier(Tier::Free)
        .daemon("desktop")
        .cloud_only()
        .start()
        .await;
    let [free_phone, desktop] = net.daemons(["free-phone", "desktop"]);
    free_phone.sees_pairing_candidate(&desktop).await;

    let pin = desktop.start_pairing().await;
    let error = free_phone
        .pair(&desktop)
        .with_cloud_pin(&pin)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<PairingError>(),
        Some(PairingError::PaymentRequired)
    ));
    desktop.pair_mode_active().await;
    free_phone.does_not_trust(&desktop).await;
}

/// Tier is per admitted link, not per account lookup at forwarding time: a
/// pro device cannot open toward a free sibling on the same account.
#[tokio::test]
async fn a_free_link_beside_a_pro_link_is_still_refused() {
    let net = TestNet::builder()
        .cloud()
        .daemon("free-desktop")
        .cloud_only()
        .cloud_tier(Tier::Free)
        .daemon("pro-phone")
        .cloud_only()
        .cloud_tier(Tier::Pro)
        .paired("free-desktop", "pro-phone", Via::Cloud)
        .start()
        .await;
    let [free_desktop, pro_phone] = net.daemons(["free-desktop", "pro-phone"]);

    pro_phone.sees(&free_desktop).await;
    assert_eq!(
        pro_phone.refused_call_error(&free_desktop).await,
        ProtocolError::PaymentRequired
    );
    pro_phone.has_no_active_tunnel_to(&free_desktop).await;
}

/// A device can relay between two pinned peers without an account. Pinned
/// links carry no tier, so the forwarding path has no entitlement to query.
#[tokio::test]
async fn a_self_hosted_relay_between_paired_peers_never_consults_a_tier() {
    let net = TestNet::builder()
        .daemon("phone")
        .daemon("home-relay")
        .daemon("desktop")
        .paired("phone", "home-relay", Via::Direct)
        .paired("home-relay", "desktop", Via::Direct)
        .trusted_without_discovery("phone", "desktop")
        .start()
        .await;
    let [phone, home_relay, desktop] = net.daemons(["phone", "home-relay", "desktop"]);

    phone.sees(&desktop).await;
    phone.connects_to(&desktop).via(&home_relay).await;
    phone.can_call(&desktop).await;
    desktop.can_call(&phone).await;
}
