//! Chapter 7 — Agent messaging and relationships.

use amux::testnet::{TestNet, Via};

/// A client may nominate a local live agent as the author of a message, but
/// an arbitrary UUID never becomes authenticated provenance. The daemon must
/// reject it before attempting the recipient's backend delivery carrier.
#[tokio::test]
async fn a2a_unknown_sender_refused() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    host.spawn_echo_agent("recipient").await;
    host.refuses_unknown_message_sender("recipient").await;
}

/// A message with no sender identity is authored by the daemon as human input,
/// then delivered through the recipient backend as transcript-visible tagged
/// text rather than an unauthenticated side record.
#[tokio::test]
async fn a2a_human_send_echoed() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    host.spawn_echo_agent("recipient").await;
    host.human_message_is_echoed("recipient", "hello from the human")
        .await;
}

/// A client supplies only a local agent id. The daemon resolves every
/// provenance field from its live registry before the recipient sees it.
#[tokio::test]
async fn a2a_daemon_authored_from() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let sender = host.spawn_echo_agent("sender").await;
    let recipient = host.spawn_echo_agent("recipient").await;
    host.agent_message_is_echoed(&host, &sender, &recipient, "hello from an agent")
        .await;
}

/// A client message crosses the direct device link through the peer agent
/// service and is delivered by the recipient daemon's local backend.
#[tokio::test]
async fn a2a_cross_device_over_tcp() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let sender = laptop.spawn_echo_agent("sender").await;
    let recipient = desktop.spawn_echo_agent("recipient").await;
    laptop
        .agent_message_is_echoed(&desktop, &sender, &recipient, "hello over tcp")
        .await;
}

/// Cloud-only devices use the same peer agent service while the in-process
/// relay forwards their opaque tunnel traffic.
#[tokio::test]
async fn a2a_cross_device_through_cloud() {
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

    let sender = laptop.spawn_echo_agent("sender").await;
    let recipient = phone.spawn_echo_agent("recipient").await;
    laptop
        .agent_message_is_echoed(&phone, &sender, &recipient, "hello through cloud")
        .await;
}

/// A human needs immediate feedback when a selected remote host cannot be
/// reached. Agent sends remain fire-and-forget: the daemon accepts the
/// envelope id and drops the message because no recipient carrier can run.
#[tokio::test]
async fn a2a_unreachable_recipient() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let sender = laptop.spawn_echo_agent("sender").await;
    let recipient = desktop.spawn_echo_agent("recipient").await;
    laptop
        .unreachable_recipient_message_policy(&desktop, &sender, &recipient)
        .await;
}
