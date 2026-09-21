//! System lane: real-time transport behavior.
//!
//! These cases deliberately wait for the socket transport's own clock. They
//! live outside the fast specification suite so a real QUIC timeout is never
//! mistaken for driven product-policy time.

use testnet::{TestNet, Via};

#[tokio::test]
async fn a_host_whose_udp_is_blocked_is_offline_to_its_direct_peers() {
    let net = TestNet::builder()
        .daemon("phone")
        .daemon("host")
        .paired("phone", "host", Via::Direct)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.connects_to_via_direct_quic(&host).await;
    host.sees(&phone).await;

    net.udp_blocked(&host, true);

    phone.cannot_see(&host).await;
    host.cannot_see(&phone).await;
}
