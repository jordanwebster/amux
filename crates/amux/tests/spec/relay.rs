//! Chapter 6 — Relay carriers and opaque stream forwarding.

use amux::testnet::{RelayTransport, TestNet, Via};

#[tokio::test]
async fn a_relayed_session_over_quic() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .relay_transport(RelayTransport::Quic)
        .daemon("host")
        .cloud_only()
        .relay_transport(RelayTransport::Quic)
        .paired("phone", "host", Via::Cloud)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let _agent = host.spawn_echo_agent("worker").await;

    phone.uses_quic_relay().await;
    host.uses_quic_relay().await;
    let mut session = phone.attach_via_profile(&host, "worker").await;
    session.send("relay-quic").await;
    session.expect_output("relay-quic").await;
}

#[tokio::test]
async fn a_quic_device_and_a_tcp_device_reach_each_other_through_one_relay() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .relay_transport(RelayTransport::Quic)
        .daemon("host")
        .cloud_only()
        .relay_transport(RelayTransport::Tcp)
        .paired("phone", "host", Via::Cloud)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let _agent = host.spawn_echo_agent("worker").await;

    phone.uses_quic_relay().await;
    host.uses_tcp_relay().await;
    phone.can_call(&host).await;
    host.can_call(&phone).await;
    let mut session = phone.attach_via_profile(&host, "worker").await;
    session.send("cross-carrier").await;
    session.expect_output("cross-carrier").await;
}

#[tokio::test]
async fn a_stream_addressed_to_the_relay_itself_is_reset_not_adjacent() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .relay_transport(RelayTransport::Quic)
        .start()
        .await;
    let phone = net.daemon("phone");

    phone.uses_quic_relay().await;
    phone.relay_itself_is_not_adjacent().await;
}
