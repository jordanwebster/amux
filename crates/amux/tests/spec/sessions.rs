//! Chapter 5 — Remote sessions & authority.
//!
//! Paired peers list agents, attach and round-trip terminal I/O across routes,
//! and keep sessions open through unrelated routing churn. Pairing and trust
//! administration belong to the installation owner and are absent from both
//! profile sockets and peer tunnels.

use amux::testnet::{TestNet, Via};

/// B lists A's agents and attaches over a *direct* link: input and echoed
/// output round-trip across the real tunnel. The laptop dials the desktop, so
/// the laptop (B) holds the route and drives the session into the desktop (A).
#[tokio::test]
async fn a_peer_attaches_to_a_remote_agent_over_a_direct_link() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    desktop.spawn_echo_agent("worker").await;
    laptop.sees_agent_on(&desktop, "worker").await;

    let mut session = laptop.attach(&desktop, "worker").await;
    session.send("ping-direct").await;
    session.expect_output("ping-direct").await;
}

/// The same flow through the cloud relay: two cloud-only peers that share only
/// the relay still attach end to end, the relay forwarding opaque tunnel bytes.
#[tokio::test]
async fn a_peer_attaches_to_a_remote_agent_through_the_cloud() {
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

    phone.spawn_echo_agent("worker").await;
    laptop.sees_agent_on(&phone, "worker").await;

    let mut session = laptop.attach(&phone, "worker").await;
    session.send("ping-cloud").await;
    session.expect_output("ping-cloud").await;
}

/// A long-lived session survives unrelated routing churn: while a laptop holds
/// an attached echo session into the desktop, a third daemon links to the
/// desktop, leaves, and re-joins — and the session keeps echoing throughout.
#[tokio::test]
async fn a_long_lived_session_survives_unrelated_routing_churn() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .daemon("tablet")
        .paired("laptop", "desktop", Via::Tcp)
        .paired("desktop", "tablet", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop, tablet] = net.daemons(["laptop", "desktop", "tablet"]);

    desktop.spawn_echo_agent("worker").await;
    let mut session = laptop.attach(&desktop, "worker").await;
    session.send("before-churn").await;
    session.expect_output("before-churn").await;

    // The third daemon leaves: the desktop loses its link to the tablet while
    // the laptop's session into the desktop is untouched.
    tablet.stop().await;
    session.send("after-leave").await;
    session.expect_output("after-leave").await;

    // …and re-joins: the desktop re-dials its stored reachability to the
    // tablet, churning its routing table again.
    tablet.restart().await;
    net.establish_direct(&desktop, &tablet).await;
    session.send("after-rejoin").await;
    session.expect_output("after-rejoin").await;
}

/// Paired peers can drive sessions while lifecycle administration stays with the owner.
#[tokio::test]
async fn a_paired_peer_cannot_shut_down_or_suspend_the_daemon() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
    desktop.spawn_echo_agent("worker").await;
    let mut session = laptop.attach(&desktop, "worker").await;
    desktop.rejects_remote_admin_from(&laptop).await;
    session.send("still-running").await;
    session.expect_output("still-running").await;
}
