//! Harness smoke test; doubles as the canonical TestNet example.
//!
//! The operator verbs (sever, outage, restart, …) are exercised by the
//! chapters that own their behaviors — presence (Ch. 3) and routing &
//! failover (Ch. 4).

use testnet::{TestNet, Via};

#[tokio::test]
async fn an_in_process_spec_can_start_a_journey_topology() {
    let topology = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../journeys/topologies/two-hosts.json");
    let net = TestNet::from_topology(topology)
        .await
        .expect("load the journey topology");
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.sees(&desktop).await;
    laptop.trusts(&desktop).await;
    laptop.connects_to(&desktop).via_cloud().await;
}

#[tokio::test]
async fn paired_daemons_see_trust_and_call_each_other() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Direct)
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
}

/// The three verbs the phone journeys drive a network with: putting a machine
/// on it, taking it off, and changing what an account buys.
///
/// Each one is a method on the harness, and the served control door names
/// them identically, so a journey outside this process says the same sentence
/// an in-process spec does.
#[tokio::test]
async fn announcing_withdrawing_and_a_tier_flip_are_observed_by_a_device() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_user("owner")
        .cloud_only()
        .cloud_tier(node::Tier::Free)
        .daemon("workstation")
        .outside_discovery("workstation")
        .start()
        .await;
    let [phone, workstation] = net.daemons(["phone", "workstation"]);

    // Nothing is on this network until something says so.
    assert!(net.discovery_events().is_empty());
    net.announce(&workstation);
    phone.sees_pairing_candidate(&workstation).await;

    net.withdraw(&workstation);
    assert_eq!(
        net.discovery_events()
            .into_iter()
            .filter(|event| matches!(
                event,
                node::discovery::DiscoveryEvent::Lost { host_id } if *host_id == workstation.host_id()
            ))
            .count(),
        1,
        "a withdrawal says goodbye for the machine that left"
    );

    // What the account buys changes where it is decided — at the relay — and
    // reaches this device the moment it asks again.
    net.cloud_user_tier("owner", node::Tier::Pro);
    assert_eq!(phone.refresh_entitlement().await, node::Tier::Pro);
}
