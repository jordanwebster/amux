//! Chapter 6 — Wire conformance (`WirePeer`).
//!
//! Adversarial cases the topology layer cannot express, driven by a scripted
//! protocol actor over a real daemon's external TCP listener, real device
//! mTLS, and the real `LinkService.Connect` dispatcher. These lock the
//! wire-visible contract: oversize frames close the link loudly, the Hello
//! host_id must match the mTLS-bound identity, unsupported versions are
//! rejected, malformed frames close the stream, frames for unknown or
//! just-closed tunnels are dropped without harming the link, and handshake
//! floods are rate-limited. (docs/PROTOCOL.md; docs/NETWORKING.md §8.9–8.10,
//! §10 N-TN-*, N-X-9)
//!
//! B1 (NETWORKING_REVIEW.md §1) — a frame in a tunnel's just-closed window
//! escalating to a link-wide LinkClose — is closed structurally by the
//! explicit `TunnelOpen`/`TunnelData`/`TunnelClose` lifecycle: only an Open
//! allocates, so a late frame is for an unknown id and is a deterministic
//! drop. The once-`#[ignore]`d marker below is now a real passing test.

use amux::testnet::{LinkCloseReason, TestNet, Via, WirePeer};

/// An oversize `TunnelData` (one byte past the 64 KiB payload cap, N-TN-7) is
/// a link-closing protocol violation: the daemon answers `LinkClose(PROTOCOL_
/// ERROR)` and closes the Connect stream. Unlike the lib-level test that hand-
/// feeds the handler, this rides the real external TCP listener, TLS, and
/// dispatcher.
#[tokio::test]
async fn an_oversize_tunnel_data_frame_closes_the_link_with_a_protocol_link_close() {
    let net = TestNet::builder().daemon("victim").start().await;
    let [victim] = net.daemons(["victim"]);

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    wire.send_tunnel_data(victim.host_id(), vec![0u8; 64 * 1024 + 1])
        .await;

    let error = wire
        .expect_link_close(LinkCloseReason::ProtocolError)
        .await
        .expect("the protocol LinkClose carries an error");
    assert!(
        error.message.contains("payload exceeds"),
        "LinkClose error should explain the cap violation, got: {}",
        error.message
    );
    wire.expect_stream_closed().await;
}

/// A Hello whose `host_id` differs from the mTLS-bound identity is rejected
/// (N-X-9): the device acceptor binds the peer's host_id to its presented
/// certificate, so a spoofed Hello earns an error `HelloAck` and a closed
/// stream.
#[tokio::test]
async fn a_hello_host_id_that_contradicts_the_mtls_identity_is_rejected() {
    let net = TestNet::builder().daemon("victim").start().await;

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.send_hello_spoofing_host_id().await;

    let error = wire.expect_hello_ack_error().await;
    assert!(
        error.message.contains("does not match TLS peer"),
        "expected a host_id/TLS-peer mismatch, got: {}",
        error.message
    );
    wire.expect_stream_closed().await;
}

/// A Hello advertising only an unsupported protocol version earns an error
/// `HelloAck` and a closed stream: the daemon requires its own protocol
/// version in the peer's supported set. (docs/NETWORKING.md §10, versioning)
#[tokio::test]
async fn an_unsupported_protocol_version_is_rejected() {
    let net = TestNet::builder().daemon("victim").start().await;

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.send_hello_with_unsupported_version().await;

    wire.expect_hello_ack_error().await;
    wire.expect_stream_closed().await;
}

/// A malformed first frame — a structurally-empty `Message` where a Hello is
/// required — closes the Connect stream. (The typed gRPC codec admits an empty
/// body but not arbitrary garbage; this is the realistic malformation through
/// a real client.)
#[tokio::test]
async fn a_malformed_frame_closes_the_connect_stream() {
    let net = TestNet::builder().daemon("victim").start().await;

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.send_malformed_first_frame().await;

    wire.expect_stream_closed().await;
}

/// Only a `TunnelOpen` allocates endpoint state: a `TunnelData` frame for a
/// tunnel that was never opened is a confused or stale peer's violation —
/// dropped on the floor, zero allocation, and the link stays up. It must not
/// escalate to a LinkClose.
#[tokio::test]
async fn a_data_frame_for_an_unknown_tunnel_is_dropped_and_the_link_stays_up() {
    let net = TestNet::builder().daemon("victim").start().await;
    let [victim] = net.daemons(["victim"]);

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    // Endpoint-addressed (dst = the victim itself) data frame under a
    // fresh, never-opened tunnel id: a principled drop.
    wire.send_tunnel_data(victim.host_id(), b"frame-for-a-ghost-tunnel".to_vec())
        .await;

    wire.expect_stream_stays_open().await;
}

/// The B1 finding (NETWORKING_REVIEW.md §1), closed structurally by the
/// explicit tunnel lifecycle: under v5's implicit opens, a frame landing in
/// a tunnel's just-closed window surfaced `InboundClosed` and escalated to a
/// link-wide `LinkClose(PROTOCOL_ERROR)`. Now closing is part of the wire
/// grammar — open a tunnel, close it, then send data for it: the frame is
/// for an unknown id, deterministically dropped, and the link lives on.
#[tokio::test]
async fn a_frame_for_a_just_closed_tunnel_does_not_kill_the_link() {
    let net = TestNet::builder().daemon("victim").start().await;
    let [victim] = net.daemons(["victim"]);

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    let tunnel_id = wire.send_tunnel_open(victim.host_id()).await;
    wire.send_tunnel_data_for(victim.host_id(), tunnel_id.clone(), b"alive".to_vec())
        .await;
    wire.send_tunnel_close(victim.host_id(), tunnel_id.clone())
        .await;

    // The frame for the tunnel that closed a moment ago: dropped, not a
    // link-closing violation.
    wire.send_tunnel_data_for(victim.host_id(), tunnel_id, b"too-late".to_vec())
        .await;

    wire.expect_stream_stays_open().await;
}

/// A handshake flood from one source IP is rate-limited (§10: 10/min per
/// source): after the budget is spent, fresh TLS handshakes against the
/// victim's listener are refused, while an already-established paired link
/// keeps working. All testnet connections share `127.0.0.1`, so this test is
/// isolated in its own net and its existing link predates the flood.
#[tokio::test]
async fn handshake_floods_are_rate_limited_without_disturbing_existing_links() {
    let net = TestNet::builder()
        .daemon("ally")
        .daemon("victim")
        .paired("ally", "victim", Via::Tcp)
        .start()
        .await;
    let [ally, victim] = net.daemons(["ally", "victim"]);

    // The ally's link (it dialed the victim at startup) predates the flood.
    ally.can_call(&victim).await;

    // Spend past the 10/min per-source budget until the limiter engages.
    WirePeer::flood_handshakes_until_rate_limited(&net, "victim").await;

    // The pre-existing paired link is unaffected by the flood.
    ally.can_call(&victim).await;
}
