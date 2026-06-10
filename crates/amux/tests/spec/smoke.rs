//! Harness smoke test; doubles as the canonical TestNet example.

use amux::testnet::{TestNet, Via};

#[tokio::test]
async fn paired_daemons_route_directly_and_survive_a_cloud_outage() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.sees(&desktop).await;
    laptop.trusts(&desktop).await;
    laptop.connects_to(&desktop).via_direct().await;
    laptop
        .lists_agents_on(&desktop)
        .await
        .expect("routed ListAgents over the direct link");

    net.cloud_offline().await;

    laptop
        .lists_agents_on(&desktop)
        .await
        .expect("routed ListAgents must keep working without the cloud");
}

/// Exercises every network-operator verb the harness offers. Chapter tests
/// will cover these behaviors properly; until then this keeps the harness
/// itself honest.
#[tokio::test]
async fn operator_verbs_cover_failover_outage_recovery_and_restart() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .daemon("phone")
        .cloud_only()
        .paired("laptop", "desktop", Via::Tcp)
        .paired("laptop", "phone", Via::Cloud)
        .start()
        .await;
    let [laptop, desktop, phone] = net.daemons(["laptop", "desktop", "phone"]);

    // Route shapes.
    laptop.connects_to(&desktop).via_direct().await;
    laptop.connects_to(&phone).via_cloud().await;
    laptop.lists_agents_on(&phone).await.unwrap();

    // Sever the direct link; laptop falls back to the cloud route.
    net.sever_direct(&laptop, &desktop).await;
    laptop.connects_to(&desktop).via_cloud().await;
    laptop.lists_agents_on(&desktop).await.unwrap();

    // Bring the direct link back; direct wins again.
    net.establish_direct(&laptop, &desktop).await;
    laptop.connects_to(&desktop).via_direct().await;

    // Cloud outage and recovery.
    net.cloud_offline().await;
    laptop.cannot_see(&phone).await;
    net.cloud_online().await;
    laptop.sees(&phone).await;
    laptop.lists_agents_on(&phone).await.unwrap();

    // Restart preserves identity. The restart killed laptop's direct link
    // (desktop holds no reachability back, so nothing re-dials it); traffic
    // settles on the cloud route once laptop evicts the dead direct route.
    let desktop_id = desktop.host_id();
    desktop.restart().await;
    assert_eq!(desktop.host_id(), desktop_id);
    laptop.sees(&desktop).await;
    laptop.connects_to(&desktop).via_cloud().await;
    laptop.lists_agents_on(&desktop).await.unwrap();
}
