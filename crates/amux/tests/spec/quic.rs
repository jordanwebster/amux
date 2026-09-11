//! Chapter 5 — Direct QUIC behavior and network conditions.

use amux::ArtifactKind;
use amux::testnet::{TestNet, Via, WirePeer};

const LARGE_ARTIFACT_SIZE: usize = 32 * 1024 * 1024;

#[tokio::test]
async fn a_direct_quic_link_carries_a_session() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let _agent = host.spawn_echo_agent("worker").await;

    phone.connects_to_via_direct_quic(&host).await;
    let mut session = phone.attach_via_profile(&host, "worker").await;
    session.send("direct-quic").await;
    session.expect_output("direct-quic").await;
}

#[tokio::test]
async fn a_client_that_rebinds_its_socket_keeps_its_link_channels_and_open_session_stream() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let agent = host.spawn_echo_agent("worker").await;
    let mut session = phone.attach_via_profile(&host, "worker").await;
    let open_sessions = phone.active_session_streams_to(&host).await;
    assert_eq!(open_sessions, vec![agent.id]);

    net.rebind_client(&phone).await;

    phone.connects_to_via_direct_quic(&host).await;
    assert_eq!(phone.active_session_streams_to(&host).await, open_sessions);
    session.send("after-rebind").await;
    session.expect_output("after-rebind").await;
}

#[tokio::test]
async fn a_large_artifact_fetch_beside_a_live_session_does_not_stall_it() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let agent = host.spawn_echo_agent("worker").await;
    let bytes = vec![0x5a; LARGE_ARTIFACT_SIZE];
    let artifact = host
        .put_artifact_on(
            &host,
            &agent,
            ArtifactKind::File,
            "large.bin",
            "application/octet-stream",
            bytes.clone(),
        )
        .await
        .expect("store large artifact");
    let mut session = phone.attach_via_profile(&host, "worker").await;

    let fetch = phone
        .fetch_artifact_via_profile(&host, &agent, &artifact.id)
        .await;
    phone.expects_active_bulk_stream_to(&host).await;
    assert!(
        !fetch.is_finished(),
        "bulk transfer finished before overlap proof"
    );
    session.send("responsive-beside-bulk").await;
    session.expect_output("responsive-beside-bulk").await;

    let (fetched, fetched_bytes) = fetch
        .await
        .expect("bulk fetch task")
        .expect("fetch large artifact over direct QUIC");
    assert_eq!(fetched, artifact);
    assert_eq!(fetched_bytes, bytes);
}

#[tokio::test]
async fn revocation_closes_the_quic_connection_and_every_stream_at_once() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let _agent = host.spawn_echo_agent("worker").await;
    let session = phone.attach_via_profile(&host, "worker").await;
    let phone_events = phone.open_event_stream_to(&host).await;
    let host_events = host.open_event_stream_to(&phone).await;

    phone.unpair(&host).await;

    phone.does_not_trust(&host).await;
    phone.cannot_call(&host).await;
    host.cannot_call(&phone).await;
    session.expect_disconnect().await;
    phone_events.expect_disconnect().await;
    host_events.expect_disconnect().await;
}

#[tokio::test]
async fn a_client_presenting_a_session_ticket_gets_a_full_handshake() {
    let net = direct_pair().await;
    let [client, server] = net.daemons(["phone", "host"]);

    net.session_ticket_requires_full_handshake(&client, &server)
        .await;
}

#[tokio::test]
async fn handshake_floods_on_the_lan_listener_are_limited_per_source_before_allocation() {
    let net = direct_pair().await;
    let [ally, victim] = net.daemons(["phone", "host"]);

    ally.connects_to_via_direct_quic(&victim).await;
    WirePeer::flood_handshakes_until_rate_limited(&net, "host").await;
    ally.can_call(&victim).await;
}

#[tokio::test]
async fn a_session_survives_five_percent_loss_and_two_hundred_milliseconds_of_latency() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let _agent = host.spawn_echo_agent("worker").await;
    let mut session = phone.attach_via_profile(&host, "worker").await;

    net.loss(5);
    net.latency(200);
    session.send("impaired-network").await;
    session.expect_output("impaired-network").await;
    phone.connects_to_via_direct_quic(&host).await;
}

#[tokio::test]
async fn a_host_whose_udp_is_blocked_is_offline_to_its_direct_peers() {
    let net = direct_pair().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    phone.connects_to_via_direct_quic(&host).await;
    host.sees(&phone).await;

    net.udp_blocked(&host, true);

    phone.cannot_see(&host).await;
    host.cannot_see(&phone).await;
}

async fn direct_pair() -> TestNet {
    TestNet::builder()
        .daemon("phone")
        .daemon("host")
        .paired("phone", "host", Via::Direct)
        .start()
        .await
}
