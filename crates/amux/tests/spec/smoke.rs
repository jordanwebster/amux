//! Harness smoke test; doubles as the canonical TestNet example.
//!
//! The operator verbs (sever, outage, restart, …) are exercised by the
//! chapters that own their behaviors — presence (Ch. 3) and routing &
//! failover (Ch. 4).

use amux::testnet::{TestNet, Via};

#[tokio::test]
async fn paired_daemons_see_trust_and_call_each_other() {
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
}
