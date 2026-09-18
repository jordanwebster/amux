//! Chapter 6 — Relay carriers and opaque stream forwarding.

use testnet::{RelayTransport, TestNet, Via};

#[tokio::test]
async fn a_device_on_a_udp_blocked_network_falls_back_to_tcp_and_retries_quic_after_the_memory_expires()
 {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .udp_blocked()
        .udp_blocked_memory(std::time::Duration::from_secs(2))
        .daemon("host")
        .cloud_only()
        .paired("phone", "host", Via::Cloud)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);

    phone.uses_tcp_relay().await;
    host.uses_quic_relay().await;

    // The quick TCP win is not enough to classify the network. Leave the
    // black-holed QUIC probe its bounded candidate budget before checking the
    // remembered fallback.
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    net.udp_blocked(&phone, false);
    phone.restart_cloud_link().await;
    phone.uses_tcp_relay().await;

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    phone.restart_cloud_link().await;
    phone.uses_quic_relay().await;
}

#[tokio::test]
async fn a_phone_that_rebinds_mid_session_keeps_its_relayed_session() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .paired("phone", "host", Via::Cloud)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let agent = host.spawn_echo_agent("worker").await;
    let mut session = phone.attach_via_profile(&host, "worker").await;
    let open_sessions = phone.active_session_streams_to(&host).await;
    assert_eq!(open_sessions, vec![agent.id]);

    phone.uses_quic_relay().await;
    net.rebind_client(&phone).await;

    phone.uses_quic_relay().await;
    assert_eq!(phone.active_session_streams_to(&host).await, open_sessions);
    session.send("relay-after-rebind").await;
    session.expect_output("relay-after-rebind").await;
}

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
