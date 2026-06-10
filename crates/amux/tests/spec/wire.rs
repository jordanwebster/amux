//! Chapter 6 — Wire conformance (`WirePeer`).
//!
//! Adversarial cases the topology layer cannot express, driven by a scripted
//! protocol actor over a real daemon's external TCP listener, real device
//! mTLS, and the real `RoutingService.Connect` dispatcher. These lock the
//! wire-visible contract: oversize frames close the link loudly, the Hello
//! host_id must match the mTLS-bound identity, unsupported versions are
//! rejected, malformed frames close the stream, frames for unknown tunnels are
//! dropped without harming the link, and handshake floods are rate-limited.
//! (docs/NETWORKING.md §8.9–8.10, §10 N-TN-*, N-X-9, protocol versioning)

use amux::testnet::{GoAwayReason, TestNet, Via, WirePeer};

/// An oversize `TunnelFrame` (one byte past the 64 KiB payload cap, N-TN-7) is
/// a link-closing protocol violation: the daemon answers `GoAway(PROTOCOL_
/// ERROR)` and closes the Connect stream. Unlike the lib-level test that hand-
/// feeds the handler, this rides the real external TCP listener, TLS, and
/// dispatcher.
#[tokio::test]
async fn an_oversize_tunnel_frame_closes_the_link_with_a_protocol_goaway() {
    let net = TestNet::builder().daemon("victim").start().await;

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    wire.send_tunnel_frame(&["next"], vec![0u8; 64 * 1024 + 1])
        .await;

    let error = wire
        .expect_goaway(GoAwayReason::ProtocolError)
        .await
        .expect("the protocol GoAway carries an error");
    assert!(
        error.message.contains("payload exceeds"),
        "GoAway error should explain the cap violation, got: {}",
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

/// A `TunnelFrame` for a tunnel that never existed is dropped on the floor and
/// the Link stays up (N-TN-4: an unknown id may at most create an inbound
/// tunnel; here the acceptor holds no reverse route to the frame's initiator,
/// so it is simply discarded). The link must not escalate to a GoAway.
///
/// NOTE: this is the deterministic, non-racy half of the spec'd behavior. The
/// close-race variant — a frame arriving for a tunnel in its just-dropped
/// window yields `InboundClosed`, which the Connect handler escalates to
/// `GoAway(PROTOCOL_ERROR)` on the whole Link — is the B1 finding
/// (NETWORKING_REVIEW.md §1); it needs a real tunnel lifecycle a WirePeer
/// can't drive deterministically and is documented by the ignored test below.
#[tokio::test]
async fn a_frame_for_an_unknown_tunnel_is_dropped_and_the_link_stays_up() {
    let net = TestNet::builder().daemon("victim").start().await;

    let mut wire = WirePeer::connect_trusted(&net, "victim").await;
    wire.hello().await;

    // Endpoint-addressed (empty dst route) frame for a fresh, never-seen
    // tunnel id initiated by this peer. The victim is the acceptor of this
    // link and holds no route back, so the frame is dropped.
    wire.send_tunnel_frame(&[], b"frame-for-a-ghost-tunnel".to_vec())
        .await;

    wire.expect_stream_stays_open().await;
}

/// Documents the B1 close-race (NETWORKING_REVIEW.md §1): a frame for a tunnel
/// dropped a moment earlier surfaces `InboundClosed`, which the Connect
/// handler turns into a link-wide `GoAway(PROTOCOL_ERROR)` — contrary to the
/// spec, where only oversize payloads are link-closing. The race needs a real
/// tunnel created-then-closed under the frame, which the WirePeer harness
/// cannot stage deterministically; ignored until the bug is fixed or a
/// deterministic reproduction exists.
#[tokio::test]
#[ignore = "B1: tunnel-close race escalates a dead-tunnel frame to a link GoAway (NETWORKING_REVIEW.md §1)"]
async fn a_frame_for_a_just_closed_tunnel_should_not_kill_the_link() {
    // Intentionally empty: the deterministic case is covered above; this
    // marker keeps the B1 divergence visible in the suite.
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

    // Spend past the 10/min per-source budget. Early handshakes are admitted
    // (they complete TLS, then the dispatcher closes the anonymous stream);
    // once the budget is spent, handshakes are refused before TLS. We stop at
    // the first refusal that follows at least one admitted handshake, which is
    // the rate limiter engaging.
    let mut admitted = 0;
    let mut refused = false;
    for _ in 0..20 {
        if WirePeer::anonymous_tls_handshake_succeeds(&net, "victim").await {
            admitted += 1;
        } else if admitted > 0 {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "expected the per-source handshake rate limiter to refuse a handshake \
         after {admitted} were admitted"
    );

    // The pre-existing paired link is unaffected by the flood.
    ally.can_call(&victim).await;
}
