//! Chapter 7 — Agent messaging and relationships.

use amux::testnet::TestNet;

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
