//! Chapter 7 — Agent messaging and relationships.

use std::collections::HashSet;
use std::time::Duration;

use client::{AgentEventStream, ClientError};
use model::envelope::EnvelopeKind;
use node::{Agent, ProtocolError, SendMessageRequest, SetAgentStatusRequest};
use testnet::{TestNet, Via};
use uuid::Uuid;

const EVENT_DEADLINE: Duration = Duration::from_secs(5);

/// A client may nominate a local live agent as the author of a message, but
/// an arbitrary UUID never becomes authenticated provenance. The daemon must
/// reject it before attempting the recipient's backend delivery carrier.
#[tokio::test]
async fn a2a_unknown_sender_refused() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    host.spawn_echo_agent("recipient").await;
    let error = host
        .admin_client()
        .await
        .send_message(SendMessageRequest {
            to: "recipient".into(),
            text: "must not be delivered".to_string(),
            context: None,
            from_agent_id: Some(Uuid::new_v4()),
        })
        .await
        .expect_err("an unknown sender must be refused");
    assert!(matches!(
        error,
        ClientError::Protocol(ProtocolError::NoAgentFound)
    ));
}

/// A message with no sender identity is authored by the daemon as human input,
/// then delivered through the recipient backend as transcript-visible tagged
/// text rather than an unauthenticated side record.
#[tokio::test]
async fn a2a_human_send_echoed() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    host.spawn_echo_agent("recipient").await;
    let mut stream = host.attach(&host, "recipient").await;
    let envelope_id = host
        .admin_client()
        .await
        .send_message(SendMessageRequest {
            to: "recipient".into(),
            text: "hello from the human".to_string(),
            context: None,
            from_agent_id: None,
        })
        .await
        .expect("send a human message");
    let encoded = stream.expect_envelope("a human message").await;

    assert!(encoded.starts_with("<amux "));
    assert!(encoded.contains("from=\"human\""));
    let parsed = parse_envelope(&encoded);
    assert_eq!(parsed.id, envelope_id);
    assert_eq!(parsed.from, "human");
    assert_eq!(parsed.from_id, None);
    assert_eq!(parsed.from_kind, None);
    assert_eq!(parsed.kind, EnvelopeKind::Message);
    assert_eq!(parsed.text, "hello from the human");
}

/// A client supplies only a local agent id. The daemon resolves every
/// provenance field from its live registry before the recipient sees it.
#[tokio::test]
async fn a2a_daemon_authored_from() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let sender = host.spawn_echo_agent("sender").await;
    let recipient = host.spawn_echo_agent("recipient").await;
    host.observes_agents(&[sender.id, recipient.id]).await;
    let mut stream = host.attach(&host, "recipient").await;
    let envelope_id = host
        .admin_client()
        .await
        .send_message(SendMessageRequest {
            to: recipient.id.into(),
            text: "hello from an agent".to_string(),
            context: None,
            from_agent_id: Some(sender.id),
        })
        .await
        .expect("send an agent-authored message");
    let encoded = stream.expect_envelope("an agent message").await;
    let sender_name = sender.name.as_deref().expect("sender has a name");

    assert!(encoded.contains(&format!("from=\"{sender_name}/{}\"", sender.host_id)));
    assert!(encoded.contains(&format!("from-id=\"{}\"", sender.id)));
    assert!(encoded.contains(&format!("from-kind=\"{}\"", sender.kind.provider())));
    let parsed = parse_envelope(&encoded);
    assert_eq!(parsed.id, envelope_id);
    assert_eq!(parsed.from, format!("{sender_name}/{}", sender.host_id));
    assert_eq!(parsed.from_id, Some(sender.id));
    assert_eq!(parsed.from_kind.as_deref(), Some(sender.kind.provider()));
    assert_eq!(parsed.kind, EnvelopeKind::Message);
    assert_eq!(parsed.text, "hello from an agent");
}

/// A client message crosses the direct device link through the peer agent
/// service and is delivered by the recipient daemon's local backend.
#[tokio::test]
async fn a2a_cross_device_over_tcp() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Direct)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let sender = laptop.spawn_echo_agent("sender").await;
    let recipient = desktop.spawn_echo_agent("recipient").await;
    laptop.sees_agent_on(&desktop, "recipient").await;
    let mut stream = desktop.attach(&desktop, "recipient").await;
    let envelope_id = laptop
        .admin_client()
        .await
        .send_message(SendMessageRequest {
            to: recipient.id.into(),
            text: "hello over tcp".to_string(),
            context: None,
            from_agent_id: Some(sender.id),
        })
        .await
        .expect("send an agent-authored message over the direct link");
    let encoded = stream.expect_envelope("an agent message").await;
    let sender_name = sender.name.as_deref().expect("sender has a name");

    assert!(encoded.contains(&format!("from=\"{sender_name}/{}\"", sender.host_id)));
    assert!(encoded.contains(&format!("from-id=\"{}\"", sender.id)));
    assert!(encoded.contains(&format!("from-kind=\"{}\"", sender.kind.provider())));
    let parsed = parse_envelope(&encoded);
    assert_eq!(parsed.id, envelope_id);
    assert_eq!(parsed.from, format!("{sender_name}/{}", sender.host_id));
    assert_eq!(parsed.from_id, Some(sender.id));
    assert_eq!(parsed.from_kind.as_deref(), Some(sender.kind.provider()));
    assert_eq!(parsed.kind, EnvelopeKind::Message);
    assert_eq!(parsed.text, "hello over tcp");
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
    laptop.sees_agent_on(&phone, "recipient").await;
    let mut stream = phone.attach(&phone, "recipient").await;
    let envelope_id = laptop
        .admin_client()
        .await
        .send_message(SendMessageRequest {
            to: recipient.id.into(),
            text: "hello through cloud".to_string(),
            context: None,
            from_agent_id: Some(sender.id),
        })
        .await
        .expect("send an agent-authored message through the cloud");
    let encoded = stream.expect_envelope("an agent message").await;
    let sender_name = sender.name.as_deref().expect("sender has a name");

    assert!(encoded.contains(&format!("from=\"{sender_name}/{}\"", sender.host_id)));
    assert!(encoded.contains(&format!("from-id=\"{}\"", sender.id)));
    assert!(encoded.contains(&format!("from-kind=\"{}\"", sender.kind.provider())));
    let parsed = parse_envelope(&encoded);
    assert_eq!(parsed.id, envelope_id);
    assert_eq!(parsed.from, format!("{sender_name}/{}", sender.host_id));
    assert_eq!(parsed.from_id, Some(sender.id));
    assert_eq!(parsed.from_kind.as_deref(), Some(sender.kind.provider()));
    assert_eq!(parsed.kind, EnvelopeKind::Message);
    assert_eq!(parsed.text, "hello through cloud");
}

/// A human needs immediate feedback when a selected remote host cannot be
/// reached. Agent sends remain fire-and-forget: the daemon accepts the
/// envelope id and drops the message because no recipient carrier can run.
#[tokio::test]
async fn a2a_unreachable_recipient() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Direct)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    let sender = laptop.spawn_echo_agent("sender").await;
    let recipient = desktop.spawn_echo_agent("recipient").await;
    laptop.sees_agent_on(&desktop, "recipient").await;
    desktop.stop().await;
    laptop.restore_agent_observation(&recipient).await;
    let client = laptop.admin_client().await;

    let human_error = client
        .send_message(SendMessageRequest {
            to: recipient.id.into(),
            text: "unreachable human message".to_string(),
            context: None,
            from_agent_id: None,
        })
        .await
        .expect_err("a human sender must observe an unreachable recipient host");
    assert!(matches!(
        human_error,
        ClientError::Protocol(ProtocolError::Unreachable { .. })
    ));

    let envelope_id = client
        .send_message(SendMessageRequest {
            to: recipient.id.into(),
            text: "unreachable agent message".to_string(),
            context: None,
            from_agent_id: Some(sender.id),
        })
        .await
        .expect("an agent sender drops an unreachable fire-and-forget message");
    assert_ne!(envelope_id, Uuid::nil());
}

/// Claude's Stop hook carries the child's final answer to a local parent;
/// process death remains a distinct exited notification.
#[tokio::test]
async fn a2a_claude_completion_local() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    let child = host.register_scripted_claude_child(&parent).await;
    let mut stream = host.attach(&host, "parent").await;
    host.deliver_scripted_claude_completion(&child, "finished locally")
        .await;
    let completed = parse_envelope(&stream.expect_envelope("a completed message").await);
    assert_eq!(completed.from_id, Some(child.id));
    assert_eq!(completed.from_kind.as_deref(), Some("claude"));
    assert_eq!(completed.kind, EnvelopeKind::Completed);
    assert_eq!(completed.text, "finished locally");

    host.end_scripted_session(&child).await;
    let exited = parse_envelope(&stream.expect_envelope("an exited message").await);
    assert_eq!(exited.from_id, Some(child.id));
    assert_eq!(exited.from_kind.as_deref(), Some("claude"));
    assert_eq!(exited.kind, EnvelopeKind::Exited);
    assert_eq!(exited.text, "");
}

/// Completion uses the same authenticated peer routing as ordinary agent
/// messages when the child's parent belongs to another paired host.
#[tokio::test]
async fn a2a_claude_completion_remote() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Direct)
        .start()
        .await;
    let [parent_host, child_host] = net.daemons(["parent-host", "child-host"]);

    let parent = parent_host.spawn_echo_agent("parent").await;
    let child = child_host.register_scripted_claude_child(&parent).await;
    let mut stream = parent_host.attach(&parent_host, "parent").await;
    child_host
        .deliver_scripted_claude_completion(&child, "finished remotely")
        .await;
    let completed = parse_envelope(&stream.expect_envelope("a completed message").await);
    assert_eq!(completed.from_id, Some(child.id));
    assert_eq!(completed.from_kind.as_deref(), Some("claude"));
    assert_eq!(completed.kind, EnvelopeKind::Completed);
    assert_eq!(completed.text, "finished remotely");

    child_host.end_scripted_session(&child).await;
    let exited = parse_envelope(&stream.expect_envelope("an exited message").await);
    assert_eq!(exited.from_id, Some(child.id));
    assert_eq!(exited.from_kind.as_deref(), Some("claude"));
    assert_eq!(exited.kind, EnvelopeKind::Exited);
    assert_eq!(exited.text, "");
}

/// Creating a child records its family edge, preserves the parent's working
/// directory default, and injects the initial task through the normal message
/// carrier only after the echo backend can receive it.
#[tokio::test]
async fn a2a_spawn_initial_prompt() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    let child = host
        .spawn_echo_child_with_prompt(&parent, "child", "inspect the lifecycle")
        .await;
    assert_eq!(child.parent.map(|edge| edge.agent_id), Some(parent.id));
    assert_eq!(child.working_dir, parent.working_dir);

    let mut stream = host.attach(&host, "child").await;
    let prompt = parse_envelope(&stream.expect_envelope("an initial child prompt").await);
    assert_eq!(prompt.from_id, Some(parent.id));
    assert_eq!(prompt.from_kind.as_deref(), Some(parent.kind.provider()));
    assert_eq!(prompt.kind, EnvelopeKind::Message);
    assert_eq!(prompt.text, "inspect the lifecycle");
}

/// A parent deletion walks local and remote descendants deepest-first. The
/// returned result names every removed child, including a grandchild owned by
/// the paired daemon.
#[tokio::test]
async fn a2a_cascade_delete() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Direct)
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
    let expected = HashSet::from([local_child.id, remote_child.id, grandchild.id]);
    parent_host
        .observes_agents(&[parent.id, local_child.id, remote_child.id, grandchild.id])
        .await;

    let response = parent_host
        .admin_client()
        .await
        .delete_agent_with_summary(parent.id)
        .await
        .expect("cascade delete succeeds");
    let removed = response
        .removed_children
        .iter()
        .map(|agent| agent.id)
        .collect::<HashSet<_>>();
    assert_eq!(removed, expected);
    assert!(response.unreachable_children.is_empty());
}

/// Route loss leaves a remote child in place and names it in the cascade
/// result while the reachable parent is still removed.
#[tokio::test]
async fn a2a_cascade_delete_reports_unreachable_children() {
    let net = TestNet::builder()
        .daemon("parent-host")
        .daemon("child-host")
        .paired("parent-host", "child-host", Via::Direct)
        .start()
        .await;
    let [parent_host, child_host] = net.daemons(["parent-host", "child-host"]);

    let parent = parent_host.spawn_echo_agent("parent").await;
    let child = parent_host
        .spawn_echo_child_on(&child_host, &parent, "remote-child")
        .await;
    parent_host.observes_agents(&[parent.id, child.id]).await;
    child_host.stop().await;
    parent_host.restore_agent_observation(&child).await;
    let client = parent_host.admin_client().await;
    let response = client
        .delete_agent_with_summary(parent.id)
        .await
        .expect("local parent deletion succeeds despite route loss");

    assert!(response.removed_children.is_empty());
    assert_eq!(response.unreachable_children.len(), 1);
    assert_eq!(response.unreachable_children[0].id, child.id);
    assert!(
        !client
            .list_agents()
            .await
            .expect("list agents after cascade")
            .iter()
            .any(|agent| agent.id == parent.id)
    );
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
    let client = host.admin_client().await;
    let child_name = child.name.clone().expect("child has a name");
    let parent_name = parent.name.clone().expect("parent has a name");

    let unrelated_error = client
        .delete_child_agent(child_name.clone(), unrelated.id)
        .await
        .expect_err("an unrelated agent must not stop the child");
    assert!(
        unrelated_error
            .to_string()
            .contains("is not a child of the calling agent")
    );
    let child_error = client
        .delete_child_agent(parent_name, child.id)
        .await
        .expect_err("a child must not stop its parent");
    assert!(
        child_error
            .to_string()
            .contains("is not a child of the calling agent")
    );
    client
        .delete_child_agent(child_name, parent.id)
        .await
        .expect("the recorded parent stops its child");

    let agents = client.list_agents().await.expect("list agents after stop");
    assert!(agents.iter().any(|agent| agent.id == parent.id));
    assert!(!agents.iter().any(|agent| agent.id == child.id));
    assert!(agents.iter().any(|agent| agent.id == unrelated.id));
}

/// Child work is named from the bounded first prompt line, explicit status
/// changes carry a fresh timestamp through fleet events, and completion
/// clears the status without deleting the idle child.
#[tokio::test]
async fn a2a_working_on() {
    let net = TestNet::builder().daemon("host").start().await;
    let [host] = net.daemons(["host"]);

    let parent = host.spawn_echo_agent("parent").await;
    let first_line = "0123456789".repeat(9);
    let prompt = format!("{first_line}\nmore detail that is not part of the task name");
    let child = host
        .spawn_echo_child_with_prompt(&parent, "working-child", &prompt)
        .await;
    let auto = child
        .working_on
        .as_ref()
        .expect("a spawned child has an automatic work status");
    assert_eq!(auto.text, first_line.chars().take(80).collect::<String>());

    let client = host.admin_client().await;
    let mut events = client
        .subscribe_agents()
        .await
        .expect("subscribe to fleet events");
    finish_snapshot(&mut events).await;
    client
        .set_agent_status(SetAgentStatusRequest {
            agent: child.id.into(),
            working_on: Some("reviewing the result".to_string()),
        })
        .await
        .expect("set child work status");
    let explicit = next_agent_update(&mut events, child.id)
        .await
        .working_on
        .expect("status update carries working_on");
    assert_eq!(explicit.text, "reviewing the result");
    assert!(explicit.updated_at >= auto.updated_at);

    host.complete_echo_agent(&child, "done").await;
    let cleared = next_agent_update(&mut events, child.id).await;
    assert!(cleared.working_on.is_none());
    let listed = client
        .list_agents()
        .await
        .expect("list agents after completion");
    assert!(
        listed
            .iter()
            .find(|agent| agent.id == child.id)
            .expect("completed child remains in the fleet")
            .working_on
            .is_none()
    );
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
    let before = child
        .working_on
        .clone()
        .expect("spawned child has work to preserve");

    assert_eq!(host.suspend_agents().await, 2);
    host.restart().await;
    assert_eq!(host.resume_agents().await, (2, 0));
    let resumed = host.observes_agents(&[parent.id, child.id]).await;
    let resumed_parent = &resumed[0];
    let resumed_child = &resumed[1];
    assert!(resumed_parent.parent.is_none());
    assert_eq!(resumed_child.parent, child.parent);
    assert_eq!(resumed_child.working_on.as_ref(), Some(&before));
}

fn parse_envelope(encoded: &str) -> model::envelope::ParsedEnvelope {
    model::envelope::parse(encoded)
        .unwrap_or_else(|error| panic!("echoed envelope did not parse: {error}"))
}

async fn finish_snapshot(events: &mut AgentEventStream) {
    loop {
        if matches!(
            tokio::time::timeout(EVENT_DEADLINE, events.recv())
                .await
                .expect("fleet snapshot completes"),
            Ok(node::harness::AgentEvent::SnapshotComplete { .. })
        ) {
            return;
        }
    }
}

async fn next_agent_update(events: &mut AgentEventStream, id: Uuid) -> Agent {
    loop {
        let event = tokio::time::timeout(EVENT_DEADLINE, events.recv())
            .await
            .expect("agent update reaches the fleet stream")
            .expect("fleet stream remains open");
        if let node::harness::AgentEvent::AgentUpdated { agent } = event
            && agent.id == id
        {
            return agent;
        }
    }
}
