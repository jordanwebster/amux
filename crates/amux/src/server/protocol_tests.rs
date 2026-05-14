use std::time::Duration;

use uuid::Uuid;

use super::protocol_harness::Topology;
use crate::agent::TEST_ECHO_V1;
use crate::client::RpcClientError;
use crate::protocol::message::ProtocolError;
use crate::protocol::{Route, method};

#[tokio::test]
async fn local_list_agents_runs_through_real_connection_loop() {
    let net = Topology::new().await;
    let mut client = net.connect_local_client("local").await;

    assert!(client.list_agents().await.is_empty());
    client.close().await.unwrap();
}

#[tokio::test]
async fn agent_lifecycle_create_rename_delete_runs_through_rpc_client() {
    let net = Topology::new().await;
    let mut client = net.connect_local_client("local").await;
    let agent_id = Uuid::new_v4();

    let agent = client.create_test_agent(agent_id, "draft").await;
    assert_eq!(agent.id, agent_id);
    assert_eq!(agent.name.as_deref(), Some("draft"));
    assert!(
        agent
            .io_protocols
            .iter()
            .any(|protocol| protocol == TEST_ECHO_V1)
    );

    let renamed = client
        .rename_agent(agent_id, agent.route.clone(), "renamed")
        .await;
    assert_eq!(renamed.id, agent_id);
    assert_eq!(renamed.name.as_deref(), Some("renamed"));

    client.delete_agent(agent_id, renamed.route).await;
    client.expect_no_agent_named("renamed").await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn malformed_runtime_protobuf_sends_protocol_error_goaway() {
    let net = Topology::new().await;
    let mut client = net.connect_local("local").await;

    client.write_malformed_runtime_frame(b"not protobuf").await;
    client.expect_protocol_error_goaway().await;
    client.expect_closed_after_protocol_decode_error().await;
}

#[tokio::test]
async fn missing_routed_hop_returns_routing_error_to_sender() {
    let net = Topology::new().await;
    let mut client = net.connect_local("local").await;
    let probe = client.send_to_missing_route("missing", b"opaque").await;

    client.expect_unreachable(probe).await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn routed_unary_receives_unreachable_when_peer_link_closes_while_pending() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;
    let client = net.connect_local_client("local").await;
    let agent_id = Uuid::new_v4();
    let agent_route = Route::from_link(peer.link());

    let rename_task = tokio::spawn(async move {
        let mut client = client;
        let result = client
            .rename_agent_result(agent_id, agent_route, "renamed")
            .await;
        let close_result = client.close().await;
        (result, close_result)
    });

    peer.expect_routed_request_method(method::AGENT_RENAME_NAME)
        .await;
    peer.close().await.unwrap();

    let (result, close_result) = rename_task.await.expect("rename task should not panic");
    assert!(matches!(
        result,
        Err(RpcClientError::Protocol(ProtocolError::Unreachable { .. }))
    ));
    close_result.unwrap();
}

#[tokio::test]
async fn application_frame_forwarding_preserves_opaque_payload_and_accumulates_src() {
    let net = Topology::new().await;
    let mut local = net.connect_local("local").await;
    let mut peer = net.connect_peer("peer").await;
    let probe = local
        .send_opaque_to(peer.link(), b"not decoded by relay")
        .await;

    peer.expect_forwarded_opaque(probe, b"not decoded by relay")
        .await;
    local.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn application_frame_from_peer_with_spoofed_source_is_dropped() {
    let net = Topology::new().await;
    let mut sender = net.connect_peer("sender").await;
    let mut target = net.connect_peer("target").await;

    sender
        .send_opaque_with_spoofed_source(target.link(), target.link(), b"forged")
        .await;

    target.expect_no_message().await;
    sender.close().await.unwrap();
    target.close().await.unwrap();
}

#[tokio::test]
async fn local_request_from_peer_connection_is_rejected() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;

    peer.send_local_list_agents_request().await;

    peer.expect_no_message().await;
    peer.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn heartbeat_dialer_sends_ping_before_idle_timeout() {
    let net = Topology::new().await;
    let mut peer = net
        .connect_peer_with_heartbeat("peer", Duration::from_secs(30))
        .await;

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    peer.expect_ping().await;
    peer.send_pong().await;
    peer.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn heartbeat_pong_keeps_connection_alive_before_idle_deadline() {
    let net = Topology::new().await;
    let mut peer = net
        .connect_peer_with_heartbeat("peer", Duration::from_secs(30))
        .await;

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    peer.expect_ping().await;
    peer.send_pong().await;

    tokio::time::advance(Duration::from_secs(29)).await;
    tokio::task::yield_now().await;
    peer.send_pong().await;
    peer.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn heartbeat_timeout_closes_silent_peer_connection() {
    let net = Topology::new().await;
    let peer = net
        .connect_peer_with_heartbeat("peer", Duration::from_secs(30))
        .await;

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;
    peer.expect_heartbeat_timeout().await;
}

#[tokio::test]
async fn reauth_is_accepted_on_non_cloud_peer_connection() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;

    peer.send_reauth("refreshed-token").await;
    peer.expect_reauth_accepted().await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn reauth_on_non_cloud_peer_does_not_apply_cloud_client_minimum_version() {
    let net = Topology::new().await;
    net.require_minimum_client_version("cli", "999.0.0").await;
    let mut peer = net.connect_peer("peer").await;

    peer.send_reauth("refreshed-token").await;
    peer.expect_reauth_accepted().await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn peer_routing_subscription_sends_snapshot_complete_without_self_host() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_routing_events().await;

    subscription.expect_snapshot_complete(&mut peer).await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn agent_subscription_snapshot_includes_existing_agents_before_complete() {
    let net = Topology::new().await;
    let agent_id = net.spawn_test_echo_agent("echo").await;
    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_agent_events(net.host_id().await).await;

    subscription
        .expect_agent_up(&mut peer, agent_id, "echo", TEST_ECHO_V1)
        .await;
    subscription.expect_snapshot_complete(&mut peer).await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn agent_subscription_snapshot_handles_large_known_inventory() {
    let net = Topology::new().await;
    let mut expected = Vec::new();
    for idx in 0..300 {
        let name = format!("echo-{idx:03}");
        let agent_id = net.spawn_test_echo_agent(&name).await;
        expected.push((agent_id, name));
    }

    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_agent_events(net.host_id().await).await;

    for (agent_id, name) in expected {
        subscription
            .expect_agent_up(&mut peer, agent_id, &name, TEST_ECHO_V1)
            .await;
    }
    subscription.expect_snapshot_complete(&mut peer).await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_peer_routing_subscription_returns_already_exists() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_routing_events().await;

    subscription.expect_snapshot_complete(&mut peer).await;
    peer.expect_duplicate_routing_subscription_rejected().await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn agent_subscription_streams_live_agent_announcements_after_snapshot() {
    let net = Topology::new().await;
    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_agent_events(net.host_id().await).await;

    subscription.expect_snapshot_complete(&mut peer).await;

    let agent_id = net.spawn_test_echo_agent("echo").await;

    subscription
        .expect_agent_up(&mut peer, agent_id, "echo", TEST_ECHO_V1)
        .await;

    net.withdraw_agent(agent_id).await;

    subscription.expect_agent_down(&mut peer, agent_id).await;
    peer.close().await.unwrap();
}

#[tokio::test]
async fn agent_subscription_rejects_host_with_no_supported_agent_types() {
    let net = Topology::new().await;
    net.set_cloud_server(true).await;
    let mut peer = net.connect_peer("peer").await;
    let subscription = peer.subscribe_agent_events(net.host_id().await).await;

    let error = subscription.expect_error(&mut peer).await;
    assert!(matches!(error, ProtocolError::FailedPrecondition { .. }));
    peer.close().await.unwrap();
}

#[tokio::test]
async fn peer_routing_subscription_streams_remote_host_down_when_link_closes() {
    let home = Topology::named("home").await;
    let host = Topology::named("host").await;
    let mut observer = home.connect_peer("observer").await;
    let subscription = observer.subscribe_routing_events().await;

    subscription.expect_snapshot_complete(&mut observer).await;

    let link = home.connect_peer_topology("host", &host).await;
    let host_id = host.host_id().await;
    let host_route = Route::from_link(link.local_link());
    subscription
        .expect_host_up(&mut observer, host_id, "host", host_route.clone())
        .await;

    link.close().await;

    subscription
        .expect_host_down(&mut observer, host_id, host_route)
        .await;
    observer.close().await.unwrap();
}

#[tokio::test]
async fn subscribe_session_test_echo_roundtrips_input_as_output() {
    let net = Topology::new().await;
    let mut client = net.connect_local_client("local").await;
    let agent_id = net.spawn_test_echo_agent("echo").await;
    let session = client.subscribe_session(agent_id, TEST_ECHO_V1).await;

    session.expect_replay_complete().await;
    assert_eq!(client.list_agents().await.len(), 1);
    session.send_bytes(b"hello").await;
    session.expect_output_bytes(b"hello").await;
    session.cancel().await;
    session.expect_terminal_cancelled().await;
    client.close_after_session(session).await.unwrap();
}

#[tokio::test]
async fn subscribe_session_accepts_input_sent_immediately_after_request() {
    let net = Topology::new().await;
    let mut client = net.connect_local("local").await;
    let agent_id = net.spawn_test_echo_agent("echo").await;
    let session = client
        .subscribe_session_with_queued_raw_input(agent_id, TEST_ECHO_V1, b"hello before opened")
        .await;

    session.expect_opened(&mut client).await;
    session.expect_replay_complete(&mut client).await;
    session
        .expect_output_bytes(&mut client, b"hello before opened")
        .await;
    session.cancel(&mut client).await;
    session.expect_terminal_cancelled(&mut client).await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn subscribe_session_routes_to_agent_learned_from_peer() {
    let home = Topology::named("home").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let peer_link = home.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    assert!(agent.is_remote());
    assert!(
        agent
            .io_protocols
            .iter()
            .any(|protocol| protocol == TEST_ECHO_V1)
    );

    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;

    session.expect_replay_complete().await;
    session.send_bytes(b"hello from home").await;
    session.expect_output_bytes(b"hello from home").await;
    session.cancel().await;
    session.expect_terminal_cancelled().await;
    client.close_after_session(session).await.unwrap();
    peer_link.close().await;
}

#[tokio::test]
async fn subscribe_session_routes_to_agent_learned_through_relay() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    assert!(agent.is_remote());
    assert_eq!(agent.route.to_string(), "relay.host");

    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;

    session.expect_replay_complete().await;
    session.send_bytes(b"hello through relay").await;
    session.expect_output_bytes(b"hello through relay").await;
    session.cancel().await;
    session.expect_terminal_cancelled().await;
    client.close_after_session(session).await.unwrap();
    home_to_relay.close().await;
    relay_to_host.close().await;
}

#[tokio::test]
async fn remote_subscribe_session_is_cancelled_when_local_client_disconnects_idle() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;
    session.expect_replay_complete().await;

    client.close_after_session(session).await.unwrap();
    host.expect_no_session_subscriptions().await;

    home_to_relay.close().await;
    relay_to_host.close().await;
}

#[tokio::test]
async fn idle_subscribe_session_receives_unreachable_when_remote_route_closes() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;
    session.expect_replay_complete().await;

    relay_to_host.close().await;
    session.expect_route_unreachable().await;

    client.close_after_session(session).await.unwrap();
    home_to_relay.close().await;
}

#[tokio::test]
async fn idle_subscribe_session_receives_unreachable_when_first_peer_link_closes() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;
    session.expect_replay_complete().await;

    home_to_relay.close().await;
    session.expect_route_unreachable().await;

    client.close_after_session(session).await.unwrap();
    relay_to_host.close().await;
}

#[tokio::test]
async fn learned_agent_disappears_when_peer_route_closes() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    assert_eq!(agent.route.to_string(), "relay.host");

    relay_to_host.close().await;
    client.expect_no_agent_named("echo").await;

    client.close().await.unwrap();
    home_to_relay.close().await;
}

#[tokio::test]
async fn send_input_after_downstream_route_closes_returns_unreachable() {
    let home = Topology::named("home").await;
    let relay = Topology::named("relay").await;
    let host = Topology::named("host").await;
    host.spawn_test_echo_agent("echo").await;
    let home_to_relay = home.connect_peer_topology("relay", &relay).await;
    let relay_to_host = relay.connect_peer_topology("host", &host).await;
    let mut client = home.connect_local_client("local").await;

    let agent = client.expect_agent_named("echo").await;
    assert_eq!(agent.route.to_string(), "relay.host");
    let session = client.subscribe_agent_session(&agent, TEST_ECHO_V1).await;

    session.expect_replay_complete().await;
    relay_to_host.close().await;

    session
        .expect_send_bytes_unreachable(b"after route close")
        .await;
    session.expect_route_unreachable().await;

    client.close_after_session(session).await.unwrap();
    home_to_relay.close().await;
}

#[tokio::test]
async fn subscribe_session_allows_multiple_subscribers_for_same_agent() {
    let net = Topology::new().await;
    let mut client = net.connect_local_client("local").await;
    let agent_id = net.spawn_test_echo_agent("echo").await;
    let first = client.subscribe_session(agent_id, TEST_ECHO_V1).await;
    let second = client.subscribe_session(agent_id, TEST_ECHO_V1).await;

    first.expect_replay_complete().await;
    second.expect_replay_complete().await;
    first.cancel().await;
    second.cancel().await;
    first.expect_terminal_cancelled().await;
    second.expect_terminal_cancelled().await;
    drop(first);
    client.close_after_session(second).await.unwrap();
}

#[tokio::test]
async fn deleting_agent_cancels_subscribe_session_for_that_agent() {
    let net = Topology::new().await;
    let mut client = net.connect_local_client("local").await;
    let agent_id = net.spawn_test_echo_agent("echo").await;
    let session = client.subscribe_session(agent_id, TEST_ECHO_V1).await;

    session.expect_replay_complete().await;
    client.delete_agent(agent_id, Route::empty()).await;
    session.expect_terminal_cancelled().await;
    client.close_after_session(session).await.unwrap();
}
