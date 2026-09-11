//! Relay entitlement belongs to the authenticated cloud link. Tokens must
//! name a known tier, and a same-user refresh replaces the tier observed by
//! the already-established link.

use amux::Tier;
use amux::testnet::{link_tier_across_reauth, relay_refuses_token_without_tier};

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
