//! Chapter 4 — Routing & failover.
//!
//! How traffic finds its way: shortest route wins, the cloud relay forwards
//! what it cannot read, failover and recovery are make-then-break, restarts
//! re-dial stored reachabilities, revocation evicts routes, and reachability
//! propagates through chains of links. (docs/NETWORKING.md §4.8–4.9, §8)

use std::time::Duration;

use amux::testnet::{TestNet, Via};

/// Direct beats cloud: with both paths available, traffic uses the direct
/// link. The reverse direction proves the cloud path exists and works — the
/// acceptor side of a direct link holds no route back over it
/// (NETWORKING_REVIEW.md B4), so the desktop reaches the laptop via the
/// relay while the laptop dials it directly.
#[tokio::test]
async fn direct_beats_cloud_when_both_are_available() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.connects_to(&desktop).via_direct().await;
    desktop.connects_to(&laptop).via_cloud().await;
    laptop.can_call(&desktop).await;
    desktop.can_call(&laptop).await;
}

/// Cloud-only peers communicate through an end-to-end encrypted tunnel the
/// relay merely forwards: calls flow both ways via the relay, yet the relay
/// itself — which carries every byte — cannot complete a call into either
/// peer. It has no device identity and no trust entry, and the peers accept
/// only pinned device-mTLS.
#[tokio::test]
async fn cloud_only_peers_tunnel_through_a_relay_that_cannot_call_them() {
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

    laptop.connects_to(&phone).via_cloud().await;
    phone.connects_to(&laptop).via_cloud().await;
    laptop.can_call(&phone).await;
    phone.can_call(&laptop).await;

    net.cloud_relay_cannot_call(&laptop).await;
    net.cloud_relay_cannot_call(&phone).await;
}

/// The direct link dies → traffic fails over to the cloud route.
#[tokio::test]
async fn a_dying_direct_link_fails_over_to_the_cloud_route() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.connects_to(&desktop).via_direct().await;

    net.sever_direct(&laptop, &desktop).await;

    laptop.connects_to(&desktop).via_cloud().await;
    laptop.can_call(&desktop).await;
    laptop.sees(&desktop).await; // presence survives on the cloud route
}

/// The direct link recovers → the swap back is make-then-break: the daemon
/// returns to the direct route, the in-flight stream riding the old cloud
/// tunnel breaks, and fresh calls succeed on the new route.
/// (docs/NETWORKING.md §8.7)
///
/// NOTE: the spec says the broken stream fails with UNAVAILABLE; what
/// actually surfaces when the swap drops the old tunnel is
/// `Unknown: h2 protocol error` (see `expect_disconnect` and
/// NETWORKING_REVIEW.md §6.4).
#[tokio::test]
async fn a_recovering_direct_link_wins_back_and_breaks_in_flight_cloud_streams() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    net.sever_direct(&laptop, &desktop).await;
    laptop.connects_to(&desktop).via_cloud().await;
    let stream = laptop.open_event_stream_to(&desktop).await;

    net.establish_direct(&laptop, &desktop).await;

    laptop.connects_to(&desktop).via_direct().await;
    stream.expect_disconnect().await; // the old route's tunnel was dropped
    laptop
        .lists_agents_on(&desktop)
        .await
        .expect("a fresh call goes out on the recovered direct route");
}

/// The cloud dies → directly-paired peers don't notice: their link, their
/// presence, and their calls survive the outage.
#[tokio::test]
async fn a_cloud_outage_does_not_affect_directly_paired_peers() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.connects_to(&desktop).via_direct().await;

    net.cloud_offline().await;

    laptop.sees(&desktop).await;
    laptop.connects_to(&desktop).via_direct().await;
    laptop
        .lists_agents_on(&desktop)
        .await
        .expect("the direct link must keep working without the cloud");
}

/// The cloud dies → cloud-only peers become unreachable; when the relay
/// returns, presence and calls recover.
#[tokio::test]
async fn cloud_only_peers_lose_each_other_in_an_outage_and_recover() {
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

    net.cloud_offline().await;

    laptop.cannot_see(&phone).await;
    laptop.cannot_call(&phone).await;

    net.cloud_online().await;

    laptop.sees(&phone).await;
    laptop.can_call(&phone).await;
    phone.can_call(&laptop).await;
}

/// A daemon restart re-establishes direct links from the reachabilities in
/// its own trust store — no nudge from the network: the restarted dialer
/// re-dials its stored `DirectTcp` address on startup.
/// (docs/NETWORKING.md §8.8)
#[tokio::test]
async fn restart_re_establishes_direct_links_from_stored_reachabilities() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    // The laptop is the side that stores the DirectTcp reachability (the
    // pairing initiator); its restart must bring the link back by itself.
    laptop.restart().await;

    laptop.connects_to(&desktop).via_direct().await;
    laptop.can_call(&desktop).await;
}

/// Revocation: the moment one side unpairs, the revoked peer's fresh calls
/// fail over every route — direct mTLS re-dials are refused by the live
/// trust check, and cloud tunnels die at the end-to-end handshake. Trust
/// stays local: the revoked side still holds its own entry, it just no
/// longer gets anything for it. (docs/NETWORKING.md §5.4)
///
/// NOTE: the spec intends in-flight streams to break too ("in-flight +
/// reconnect" both fail, §5.4 step 4 / §4.9). Today neither side's
/// in-flight streams break promptly: the revoked peer's inbound connection
/// is killed at the revoker without anything reaching the peer (its stream
/// goes silent until a transport keepalive would reap it), and the
/// revoker's own outbound streams survive because unregistering a 1-hop
/// Channel never severs the underlying socket. Locked in here as stalls;
/// see NETWORKING_REVIEW.md §6.5.
#[tokio::test]
async fn revocation_evicts_routes_and_fails_fresh_calls_but_strands_in_flight_streams() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    desktop.can_call(&laptop).await; // via the relay; the direct link is laptop→desktop
    let revoked_stream = desktop.open_event_stream_to(&laptop).await;
    let revoker_stream = laptop.open_event_stream_to(&desktop).await;

    laptop.unpair(&desktop).await;

    laptop.does_not_trust(&desktop).await;
    desktop.cannot_call(&laptop).await;
    laptop.cannot_call(&desktop).await; // revoking also dropped laptop's own key pin
    desktop.trusts(&laptop).await; // trust is local; access is not
    revoked_stream.expect_stalled_open().await; // NOTE: spec says break; code stalls
    revoker_stream.expect_stalled_open().await; // NOTE: spec says break; code stalls
}

/// Three daemons chained A–B–C: A learns of C through B (B advertises its
/// adjacency over the A–B link) and the two endpoints call each other
/// through B — chained relaying over a daemon hop, with end-to-end mTLS
/// between A and C, who are trusted but share no reachability.
///
/// NOTE: link direction decides whether this works at all. Only the
/// *dialer* of a link stores a route over it (acceptor-side suppression,
/// NETWORKING_REVIEW.md B4), and hosting an inbound tunnel requires a
/// route back to its initiator, so chained calls succeed only when B
/// dialed both links: then A and C each learn the other via B and hold
/// reverse routes for replies. The catalog's `a.connects_to(&c).via(&b)`
/// is not assertable in this arrangement — A holds no route to B itself
/// (B dialed A), so B never even shows as online on A. See
/// NETWORKING_REVIEW.md §6.6 and the companion test below for the
/// chain-order arrangement, where discovery works but calls cannot.
#[tokio::test]
async fn endpoints_call_each_other_through_a_chain_when_the_middle_dialed_both_links() {
    let net = TestNet::builder()
        .daemon("a")
        .daemon("b")
        .daemon("c")
        .paired("b", "a", Via::Tcp)
        .paired("b", "c", Via::Tcp)
        .trusted("a", "c")
        .start()
        .await;
    let [a, _b, c] = net.daemons(["a", "b", "c"]);

    a.sees(&c).await; // learned through b's HostUp propagation
    c.sees(&a).await;
    a.can_call(&c).await;
    c.can_call(&a).await;
}

/// The same chain dialed in chain order (A→B, B→C): A learns of C and
/// holds the two-hop route through B — `connects_to(..).via(&b)` from the
/// catalog holds here — but its calls can never complete: C, the acceptor
/// of both nothing-stored links, has no route back to A to host the
/// inbound tunnel, so A's frames are dropped on the floor at C.
///
/// NOTE: this locks a real gap, not a feature: under the spec's symmetric
/// link model (§8.5 register→HostUp for every endpoint) this arrangement
/// would work. See NETWORKING_REVIEW.md §6.6.
#[tokio::test]
async fn chain_order_links_propagate_discovery_but_replies_have_no_route() {
    let net = TestNet::builder()
        .daemon("a")
        .daemon("b")
        .daemon("c")
        .paired("a", "b", Via::Tcp)
        .paired("b", "c", Via::Tcp)
        .trusted("a", "c")
        .start()
        .await;
    let [a, b, c] = net.daemons(["a", "b", "c"]);

    a.sees(&c).await;
    a.connects_to(&c).via(&b).await;
    a.cannot_call(&c).await; // NOTE: spec-shaped routing would deliver this call
}

/// Four daemons chained A–B–C–D: reachability propagates hop by hop (the
/// hop cap is 8) and the endpoints call each other across three links.
/// The dial directions follow the same alignment rule as the three-node
/// chain: each endpoint must be dialed *by* its neighbour so that routes
/// toward both endpoints exist at every hop (NETWORKING_REVIEW.md §6.6).
#[tokio::test]
async fn endpoints_call_each_other_across_a_three_link_chain() {
    let net = TestNet::builder()
        .daemon("a")
        .daemon("b")
        .daemon("c")
        .daemon("d")
        .paired("b", "a", Via::Tcp)
        .paired("b", "c", Via::Tcp)
        .paired("c", "d", Via::Tcp)
        .trusted("a", "d")
        .start()
        .await;
    let [a, _b, _c, d] = net.daemons(["a", "b", "c", "d"]);

    a.sees(&d).await;
    d.sees(&a).await;
    a.can_call(&d).await;
    d.can_call(&a).await;
}

/// Cloud-only peers keep communicating across a JWT expiry: the connector
/// refreshes the token in-band (`Reauth`) before the old one expires, so
/// the cloud link — and calls over it — survive the expiry moment without
/// reconnecting. (docs/NETWORKING.md §8.5)
///
/// Hermetic via the testnet relay's per-token TTLs: the daemon reattaches
/// under a 2s token, which puts the production refresh point
/// (`exp - 300s`) immediately after establishment; the refresher then
/// mints a long-lived replacement, and the test outlives the initial
/// token's expiry.
#[tokio::test]
async fn cloud_peers_keep_communicating_across_a_jwt_expiry() {
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

    let jwt = laptop
        .reattach_cloud_with_expiring_jwt(Duration::from_secs(2))
        .await;
    laptop.can_call(&phone).await; // up and running under the short-lived JWT

    jwt.expired().await;

    // The link survived its initial token: had the Reauth not landed, the
    // relay would have torn the link down with LinkClose(AUTH_EXPIRED) and no
    // reconnect exists on this link.
    laptop.connects_to(&phone).via_cloud().await;
    laptop.can_call(&phone).await;
    phone.can_call(&laptop).await;
}
