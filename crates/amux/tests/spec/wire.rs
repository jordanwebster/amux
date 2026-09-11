//! Chapter 6 — link control and native-stream wire conformance.
//!
//! `WirePeer` speaks protocol version 2 over the same symmetric ordered
//! multiplexer used by the TCP fallback. These chapters exercise the
//! carrier-independent control runtime rather than a gRPC service.

use amux::testnet::{LinkCloseReason, TestNet, Via, WirePeer};

#[tokio::test]
async fn the_same_link_runtime_establishes_both_roles() {
    let net = TestNet::builder()
        .daemon("connector")
        .daemon("acceptor")
        .start()
        .await;
    let [connector, acceptor] = net.daemons(["connector", "acceptor"]);

    WirePeer::connect_runtime_pair(&net, "connector", "acceptor").await;

    connector.sees(&acceptor).await;
    acceptor.sees(&connector).await;
}

#[tokio::test]
async fn an_oversize_control_message_closes_the_link_with_a_protocol_link_close() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    wire.send_oversize_control_message().await;
    let error = wire
        .expect_link_close(LinkCloseReason::ProtocolError)
        .await
        .expect("protocol close should explain the framing violation");
    assert!(
        error.message.contains("limit"),
        "unexpected error: {error:?}"
    );
    wire.expect_stream_closed().await;
}

#[tokio::test]
async fn a_stream_without_a_preface_is_reset_not_adjacent() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    assert_eq!(
        wire.open_stream_without_preface().await as i32,
        4,
        "NOT_ADJACENT is the protocol-v2 refusal code 4"
    );
    wire.expect_stream_stays_open().await;
}

#[tokio::test]
async fn a_hello_whose_host_id_contradicts_the_carrier_peer_is_refused() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.send_hello_spoofing_host_id().await;

    let error = wire.expect_hello_ack_error().await;
    assert!(
        error.message.contains("does not match carrier peer"),
        "unexpected binding error: {error:?}"
    );
    wire.expect_stream_closed().await;
}

#[tokio::test]
async fn hello_and_hello_ack_negotiate_protocol_version_two() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;
    wire.expect_stream_stays_open().await;
}

#[tokio::test]
async fn an_unsupported_protocol_version_is_refused() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.send_hello_with_unsupported_version().await;

    wire.expect_hello_ack_error().await;
    wire.expect_stream_closed().await;
}

#[tokio::test]
async fn a_failed_reauth_closes_an_authenticated_link_auth_expired() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_authenticated(&net, "victim").await;
    wire.hello().await;

    wire.send_reauth("wire-expired").await;
    wire.expect_link_close(LinkCloseReason::AuthExpired).await;
    wire.expect_stream_closed().await;
}

#[tokio::test]
async fn an_invalid_token_in_hello_closes_the_link_auth_expired() {
    let net = TestNet::builder().daemon("victim").start().await;
    let mut wire = WirePeer::connect_authenticated(&net, "victim").await;

    wire.send_hello_with_auth_token("wire-expired").await;
    wire.expect_link_close(LinkCloseReason::AuthExpired).await;
    wire.expect_stream_closed().await;
}

#[tokio::test]
async fn hello_neighbor_snapshot_and_later_deltas_drive_presence() {
    let net = TestNet::builder()
        .daemon("victim")
        .daemon("snapshot")
        .daemon("delta")
        .start()
        .await;
    let [victim, snapshot, delta] = net.daemons(["victim", "snapshot", "delta"]);
    let mut wire = WirePeer::connect_trusted(&net, "victim").await;

    wire.send_hello_with_neighbors(
        wire.host_id(),
        vec![2],
        &[(snapshot.host_id(), snapshot.name())],
    )
    .await;
    wire.expect_hello_ack_accepted().await;
    victim.sees(&snapshot).await;

    wire.send_neighbor_up(delta.host_id(), delta.name()).await;
    victim.sees(&delta).await;
    wire.send_neighbor_down(snapshot.host_id()).await;
    victim.cannot_see(&snapshot).await;
}

/// Fresh TLS handshakes from one source are bounded before the dispatcher
/// allocates link state. A paired link established before the flood remains
/// usable because admission limits only new connections.
#[tokio::test]
async fn handshake_floods_are_rate_limited_without_disturbing_existing_links() {
    let net = TestNet::builder()
        .daemon("ally")
        .daemon("victim")
        .paired("ally", "victim", Via::Direct)
        .start()
        .await;
    let [ally, victim] = net.daemons(["ally", "victim"]);

    ally.can_call(&victim).await;
    WirePeer::flood_handshakes_until_rate_limited(&net, "victim").await;
    ally.can_call(&victim).await;
}
