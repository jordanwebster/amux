//! Chapter 5 — Native stream classes & lifecycle.
//!
//! Calls may share one channel, but a terminal session and a bulk transfer each
//! get a fresh native stream. The ordered relay fallback therefore preserves
//! application-stream independence even though TCP can still impose transport-
//! level head-of-line blocking under packet loss.

use node::ArtifactKind;
use testnet::{TestNet, Via, native_stream_lifecycle};

const LARGE_ARTIFACT_SIZE: usize = 32 * 1024 * 1024;

#[tokio::test]
async fn a_large_artifact_fetch_does_not_stall_a_live_session_stream() {
    let net = TestNet::builder()
        .cloud()
        .daemon("viewer")
        .cloud_only()
        .daemon("agent-host")
        .cloud_only()
        .paired("viewer", "agent-host", Via::Cloud)
        .start()
        .await;
    let [viewer, agent_host] = net.daemons(["viewer", "agent-host"]);
    let agent = agent_host.spawn_echo_agent("worker").await;
    let bytes = vec![0x5a; LARGE_ARTIFACT_SIZE];
    let artifact = agent_host
        .put_artifact_on(
            &agent_host,
            &agent,
            ArtifactKind::File,
            "large.bin",
            "application/octet-stream",
            bytes.clone(),
        )
        .await
        .expect("store 32 MiB artifact on its owner");
    let mut session = viewer.attach_via_profile(&agent_host, "worker").await;

    let fetch = viewer
        .fetch_artifact_via_profile(&agent_host, &agent, &artifact.id)
        .await;
    viewer.expects_active_bulk_stream_to(&agent_host).await;
    assert!(
        !fetch.is_finished(),
        "the bulk fetch must still be live when session traffic starts"
    );
    session.send("interactive-while-bulk-is-live").await;
    session
        .expect_output("interactive-while-bulk-is-live")
        .await;

    let (fetched, fetched_bytes) = fetch
        .await
        .expect("bulk fetch task")
        .expect("bulk fetch through relay");
    assert_eq!(fetched, artifact);
    assert_eq!(fetched_bytes, bytes);
}

#[tokio::test]
async fn a_session_stream_is_its_own_stream_and_a_second_agent_does_not_queue_behind_it() {
    let net = TestNet::builder()
        .cloud()
        .daemon("viewer")
        .cloud_only()
        .daemon("agent-host")
        .cloud_only()
        .paired("viewer", "agent-host", Via::Cloud)
        .start()
        .await;
    let [viewer, agent_host] = net.daemons(["viewer", "agent-host"]);
    let first_agent = agent_host.spawn_echo_agent("first").await;
    let second_agent = agent_host.spawn_echo_agent("second").await;

    let mut first = viewer.attach_via_profile(&agent_host, "first").await;
    let mut second = viewer.attach_via_profile(&agent_host, "second").await;
    let mut expected_streams = vec![first_agent.id, second_agent.id];
    expected_streams.sort_unstable();
    assert_eq!(
        viewer.active_session_streams_to(&agent_host).await,
        expected_streams
    );

    second.send("second-is-independent").await;
    second.expect_output("second-is-independent").await;
    first.send("first-remains-live").await;
    first.expect_output("first-remains-live").await;
}

#[tokio::test]
async fn a_finished_stream_is_a_close_and_a_reset_is_a_refusal_the_caller_can_name() {
    let (finished_reads_eof, refusal) = native_stream_lifecycle().await;
    assert!(finished_reads_eof);
    assert_eq!(refusal, "payment_required");
}

#[tokio::test]
async fn a_host_called_through_the_relay_can_call_back_on_the_same_link() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("phone")
        .cloud_only()
        .paired("laptop", "phone", Via::Cloud)
        .start()
        .await;
    let [laptop, phone] = net.daemons(["laptop", "phone"]);
    laptop.connects_to(&phone).via_cloud().await;
    phone.connects_to(&laptop).via_cloud().await;
    let laptop_links = laptop.cloud_link_ids().await;
    let phone_links = phone.cloud_link_ids().await;
    assert_eq!(laptop_links.len(), 1);
    assert_eq!(phone_links.len(), 1);

    laptop.can_call(&phone).await;
    phone.can_call(&laptop).await;

    assert_eq!(laptop.cloud_link_ids().await, laptop_links);
    assert_eq!(phone.cloud_link_ids().await, phone_links);
}
