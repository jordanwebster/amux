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

/// Claude's Stop hook carries the child's final answer to a local parent;
/// process death remains a distinct exited notification.
#[tokio::test]
async fn a2a_claude_completion_local() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    host.claude_completion_reaches_parent(&host, &parent, "finished locally")
        .await;
}

/// Completion uses the same authenticated peer routing as ordinary agent
/// messages when the child's parent belongs to another paired host.
#[tokio::test]
async fn a2a_claude_completion_remote() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Tcp)
        .start()
        .await;
    let [parent_host, child_host] = net.daemons(["parent-host", "child-host"]);

    let parent = parent_host.spawn_echo_agent("parent").await;
    child_host
        .claude_completion_reaches_parent(&parent_host, &parent, "finished remotely")
        .await;
}

/// Creating a child records its family edge, preserves the parent's working
/// directory default, and injects the initial task through the normal message
/// carrier only after the echo backend can receive it.
#[tokio::test]
async fn a2a_spawn_initial_prompt() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    host.spawn_echo_child_with_prompt(&parent, "child", "inspect the lifecycle")
        .await;
}

/// A parent deletion walks local and remote descendants deepest-first. The
/// returned result names every removed child, including a grandchild owned by
/// the paired daemon.
#[tokio::test]
async fn a2a_cascade_delete() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Tcp)
        .start()
        .await;
    let [parent_host, child_host] = net.daemons(["parent-host", "child-host"]);

    let parent = parent_host.spawn_echo_agent("parent").await;
    let local_child = parent_host
        .spawn_echo_child_on(&parent_host, &parent, "local-child")
        .await;
    let remote_child = parent_host
        .spawn_echo_child_on(&child_host, &parent, "remote-child")
        .await;
    let grandchild = child_host
        .spawn_echo_child_on(&child_host, &remote_child, "grandchild")
        .await;

    parent_host
        .cascade_delete_family(&parent, &[&local_child, &remote_child, &grandchild])
        .await;
}

/// Route loss leaves a remote child in place and names it in the cascade
/// result while the reachable parent is still removed.
#[tokio::test]
async fn a2a_cascade_delete_reports_unreachable_children() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Tcp)
        .start()
        .await;
    let [parent_host, child_host] = net.daemons(["parent-host", "child-host"]);

    let parent = parent_host.spawn_echo_agent("parent").await;
    let child = parent_host
        .spawn_echo_child_on(&child_host, &parent, "remote-child")
        .await;
    parent_host
        .cascade_delete_reports_unreachable(&parent, &child_host, &child)
        .await;
}

/// The model-facing stop verb is child-scoped: an unrelated agent and the
/// child itself cannot use it to delete outside the caller's direct family.
/// Removing the child leaves its parent alive.
#[tokio::test]
async fn a2a_stop_child() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    let child = host.spawn_echo_child_on(&host, &parent, "child").await;
    let unrelated = host.spawn_echo_agent("unrelated").await;

    host.parent_alone_stops_child(&parent, &child, &unrelated)
        .await;
}

/// Child work is named from the bounded first prompt line, explicit status
/// changes carry a fresh timestamp through fleet events, and completion
/// clears the status without deleting the idle child.
#[tokio::test]
async fn a2a_working_on() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    host.working_on_lifecycle(&parent).await;
}

/// A daemon restart through the suspend record retains the family edge and
/// exact work-status timestamp, then republishes both in resumed inventory.
#[tokio::test]
async fn a2a_suspend_preserves() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    let child = host
        .spawn_echo_child_with_prompt(&parent, "child", "preserve this task")
        .await;
    host.suspend_restart_preserves_family(&parent, &child).await;
}
