//! Remote-session verbs: spawning echo agents, attaching across a route, and
//! exercising runtime authority over a paired peer.
//!
//! The test agent is the in-process echo PtyHandle (`TEST_ECHO_COMMAND` /
//! `TEST_ECHO_V1`), the same one the `services` unit tests drive; it echoes
//! whatever input it receives straight back as session output. `attach`
//! opens a routed `ClientService.SubscribeSession` against the agent's owner
//! over this daemon's current route, so input and output cross the real
//! tunnel.

use uuid::Uuid;

use super::Daemon;
use super::assertions::{DEFAULT_TIMEOUT, eventually};
use crate::agents::{TEST_ECHO_COMMAND, TEST_ECHO_V1};
use crate::client::{Client, ClientError};
use crate::protocol::ProtocolError;
use crate::{
    Agent, AgentParent, AgentType, CreateAgentRequest, SendInputRequest, SendMessageRequest,
    SubscribeSessionEvent,
};

/// Asserts a routed local-admin RPC was rejected with permission-denied.
fn assert_permission_denied(rpc: &str, result: Result<(), ClientError>) {
    match result {
        Err(ClientError::Protocol(ProtocolError::PermissionDenied { .. })) => {}
        Err(other) => panic!("routed {rpc} failed with {other}, expected permission-denied"),
        Ok(()) => panic!("routed {rpc} unexpectedly succeeded; a remote peer must not invoke it"),
    }
}

impl Daemon {
    /// Spawns a local echo (test) agent named `name` through this daemon's
    /// local-admin `ClientService`, exactly as the CLI would. The agent
    /// echoes session input back as output. Returns once the agent is in the
    /// daemon's own inventory.
    pub async fn spawn_echo_agent(&self, name: &str) -> Agent {
        let agent = self
            .admin_client()
            .await
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to spawn echo agent '{name}': {error}",
                    self.name()
                )
            });
        assert_eq!(agent.name.as_deref(), Some(name));
        agent
    }

    /// Spawns an echo child and proves its first input is an authenticated
    /// message from the parent after the child backend is available.
    pub async fn spawn_echo_child_with_prompt(
        &self,
        parent: &Agent,
        name: &str,
        prompt: &str,
    ) -> Agent {
        assert_eq!(parent.host_id, self.host_id());
        let child = self
            .admin_client()
            .await
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                },
                working_dir: parent.working_dir.clone(),
                terminal_size: None,
                args: Vec::new(),
                parent: Some(AgentParent {
                    agent_id: parent.id,
                    host_id: parent.host_id,
                }),
                initial_prompt: Some(prompt.to_string()),
            })
            .await
            .unwrap_or_else(|error| panic!("spawn echo child '{name}': {error}"));

        assert_eq!(child.parent.map(|edge| edge.agent_id), Some(parent.id));
        assert_eq!(child.working_dir, parent.working_dir);

        let mut stream = self
            .admin_client()
            .await
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: child.id.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| panic!("subscribe to echo child '{name}': {error}"));
        let encoded = echoed_envelope(&mut stream, name, "an initial child prompt").await;
        let parsed = crate::envelope::parse(&encoded)
            .unwrap_or_else(|error| panic!("initial child prompt did not parse: {error}"));
        assert_eq!(parsed.from_id, Some(parent.id));
        assert_eq!(
            parsed.from_kind.as_deref(),
            Some(parent.agent_type.as_str())
        );
        assert_eq!(parsed.kind, crate::envelope::EnvelopeKind::Message);
        assert_eq!(parsed.text, prompt);
        child
    }

    /// Spawns an echo child on `owner` while preserving a parent local to the
    /// calling daemon. This exercises the same remote create route used by a
    /// model-facing spawn.
    pub async fn spawn_echo_child_on(&self, owner: &Daemon, parent: &Agent, name: &str) -> Agent {
        assert_eq!(parent.host_id, self.host_id());
        self.admin_client()
            .await
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: (owner.host_id() != self.host_id()).then_some(owner.host_id()),
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                },
                working_dir: parent.working_dir.clone(),
                terminal_size: None,
                args: Vec::new(),
                parent: Some(AgentParent {
                    agent_id: parent.id,
                    host_id: parent.host_id,
                }),
                initial_prompt: None,
            })
            .await
            .unwrap_or_else(|error| {
                panic!("spawn echo child '{name}' on '{}': {error}", owner.name())
            })
    }

    /// Deletes a family through the raw client RPC so the cascade result can
    /// be asserted before higher-level clients choose how to present it.
    pub async fn cascade_delete_family(&self, parent: &Agent, expected_children: &[&Agent]) {
        let expected_ids: std::collections::HashSet<_> =
            expected_children.iter().map(|agent| agent.id).collect();
        eventually(
            "deleting daemon mirrors the complete family",
            async || {
                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                let ids: std::collections::HashSet<_> = parts
                    .client
                    .list_agents()
                    .await
                    .into_iter()
                    .map(|agent| agent.id)
                    .collect();
                ids.contains(&parent.id) && expected_ids.is_subset(&ids)
            },
            self.failure_dump(),
        )
        .await;

        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let (channel, _accept_task) = runtime.services.open_in_process_client_channel();
        drop(guard);
        let mut client =
            crate::protocol::wire::client_service_client::ClientServiceClient::new(channel);
        let response = client
            .delete_agent(crate::protocol::wire::ClientDeleteAgentRequest {
                agent: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        parent.id.as_bytes().to_vec(),
                    )),
                }),
            })
            .await
            .expect("cascade delete succeeds")
            .into_inner();
        let removed_ids: std::collections::HashSet<_> = response
            .removed_children
            .into_iter()
            .map(|agent| {
                crate::agents::agent_from_wire(agent)
                    .expect("removed child decodes")
                    .id
            })
            .collect();
        assert_eq!(removed_ids, expected_ids);
        assert!(response.unreachable_children.is_empty());
    }

    /// A family deletion still removes its local root when a mirrored remote
    /// child cannot be reached, and reports that child as an orphan candidate.
    pub async fn cascade_delete_reports_unreachable(
        &self,
        parent: &Agent,
        child_owner: &Daemon,
        child: &Agent,
    ) {
        child_owner.stop().await;
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        parts
            .client
            .apply_agent_event(crate::agents::AgentEvent::AgentUp {
                agent: child.clone(),
            })
            .await;

        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let (channel, _accept_task) = runtime.services.open_in_process_client_channel();
        drop(guard);
        let mut client =
            crate::protocol::wire::client_service_client::ClientServiceClient::new(channel);
        let response = client
            .delete_agent(crate::protocol::wire::ClientDeleteAgentRequest {
                agent: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        parent.id.as_bytes().to_vec(),
                    )),
                }),
            })
            .await
            .expect("local parent deletion succeeds despite route loss")
            .into_inner();

        assert!(response.removed_children.is_empty());
        let unreachable = response
            .unreachable_children
            .into_iter()
            .map(|agent| crate::agents::agent_from_wire(agent).expect("unreachable child decodes"))
            .collect::<Vec<_>>();
        assert_eq!(unreachable.len(), 1);
        assert_eq!(unreachable[0].id, child.id);
        assert!(
            !parts
                .client
                .list_agents()
                .await
                .iter()
                .any(|agent| agent.id == parent.id)
        );
    }

    /// Registers a process-free Claude child, delivers a scripted Stop hook,
    /// and observes the resulting lifecycle messages in the parent's own echo
    /// stream. `parent_owner` may be this daemon or a paired remote daemon.
    pub async fn claude_completion_reaches_parent(
        &self,
        parent_owner: &Daemon,
        parent: &Agent,
        last_assistant_message: &str,
    ) {
        assert_eq!(parent.host_id, parent_owner.host_id());
        let child_id = Uuid::new_v4();
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let child = parts
            .agent_host
            .register_scripted_claude(CreateAgentRequest {
                agent_id: child_id,
                host_id: None,
                name: Some("claude-child".to_string()),
                agent_type: AgentType::Claude,
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
                parent: Some(AgentParent {
                    agent_id: parent.id,
                    host_id: parent.host_id,
                }),
                initial_prompt: None,
            })
            .await
            .unwrap_or_else(|error| panic!("register scripted Claude child: {error}"));

        let parent_name = parent
            .name
            .as_deref()
            .expect("the echo parent should have a name");
        let client = parent_owner.admin_client().await;
        let mut stream = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: parent.id.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| panic!("subscribe to echo parent '{parent_name}': {error}"));

        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": Uuid::new_v4(),
            "transcript_path": "/nonexistent/amux-scripted-claude.jsonl",
            "cwd": std::env::temp_dir(),
            "last_assistant_message": last_assistant_message,
            "stop_hook_active": false,
        }))
        .expect("scripted Stop hook serializes");
        parts
            .agent_host
            .deliver_scripted_hook(child_id, payload)
            .await
            .unwrap_or_else(|error| panic!("deliver scripted Stop hook: {error}"));

        let completed = echoed_envelope(&mut stream, parent_name, "a completed message").await;
        assert_parent_lifecycle_envelope(
            &completed,
            &child,
            crate::envelope::EnvelopeKind::Completed,
            last_assistant_message,
        );

        parts.agent_host.end_scripted_session(child_id).await;
        let exited = echoed_envelope(&mut stream, parent_name, "an exited message").await;
        assert_parent_lifecycle_envelope(
            &exited,
            &child,
            crate::envelope::EnvelopeKind::Exited,
            "",
        );
    }

    /// Asserts that the daemon rejects an agent-authored message when the
    /// claimed sender is not one of its live local agents. Sender identity is
    /// resolved before delivery, so the unavailable carrier implementation
    /// cannot mask this authority check.
    pub async fn refuses_unknown_message_sender(&self, recipient: &str) {
        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let (channel, _accept_task) = runtime.services.open_in_process_client_channel();
        drop(guard);
        let mut client =
            crate::protocol::wire::client_service_client::ClientServiceClient::new(channel);
        let error = client
            .send_message(crate::protocol::wire::ClientSendMessageRequest {
                to: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::Name(
                        recipient.to_string(),
                    )),
                }),
                text: "must not be delivered".to_string(),
                context: None,
                from_agent_id: Some(Uuid::new_v4().as_bytes().to_vec()),
            })
            .await
            .expect_err("an unknown sender must be refused");
        assert_eq!(error.code(), tonic::Code::NotFound);
    }

    /// Sends a human-authored message through the local client service and
    /// asserts that the recipient's own PTY output contains the authenticated
    /// generic envelope unchanged.
    pub async fn human_message_is_echoed(&self, recipient: &str, text: &str) {
        let client = self.admin_client().await;
        let mut stream = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: recipient.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to subscribe to echo agent '{recipient}': {error}",
                    self.name()
                )
            });
        let envelope_id = client
            .send_message(SendMessageRequest {
                to: recipient.into(),
                text: text.to_string(),
                context: None,
                from_agent_id: None,
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to send a human message to '{recipient}': {error}",
                    self.name()
                )
            });

        let encoded = echoed_envelope(&mut stream, recipient, "a human message").await;

        assert!(
            encoded.starts_with("<amux "),
            "test delivery uses the generic tag"
        );
        assert!(
            encoded.contains("from=\"human\""),
            "the echoed tag carries human provenance"
        );
        let parsed = crate::envelope::parse(&encoded)
            .unwrap_or_else(|error| panic!("echoed envelope did not parse: {error}"));
        assert_eq!(parsed.id, envelope_id);
        assert_eq!(parsed.from, "human");
        assert_eq!(parsed.from_id, None);
        assert_eq!(parsed.from_kind, None);
        assert_eq!(parsed.kind, crate::envelope::EnvelopeKind::Message);
        assert_eq!(parsed.text, text);
    }

    /// Sends on behalf of a live agent owned by this daemon and asserts the
    /// recipient's transcript carries only the identity the daemon resolved.
    pub async fn agent_message_is_echoed(
        &self,
        recipient_owner: &Daemon,
        sender: &Agent,
        recipient: &Agent,
        text: &str,
    ) {
        assert_eq!(sender.host_id, self.host_id());
        assert_eq!(recipient.host_id, recipient_owner.host_id());

        let assertion = format!(
            "'{}' mirrors recipient {} from '{}'",
            self.name(),
            recipient.id,
            recipient_owner.name()
        );
        eventually(
            &assertion,
            async || {
                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                parts
                    .client
                    .list_agents()
                    .await
                    .iter()
                    .any(|agent| agent.id == recipient.id)
            },
            self.failure_dump(),
        )
        .await;

        let recipient_name = recipient
            .name
            .as_deref()
            .expect("the echo recipient should have a name");
        let client = recipient_owner.admin_client().await;
        let mut stream = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: recipient.id.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to subscribe to echo agent '{recipient_name}': {error}",
                    recipient_owner.name()
                )
            });
        let envelope_id = self
            .admin_client()
            .await
            .send_message(SendMessageRequest {
                to: recipient.id.into(),
                text: text.to_string(),
                context: None,
                from_agent_id: Some(sender.id),
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to send from agent '{}' to '{recipient_name}': {error}",
                    self.name(),
                    sender.name.as_deref().unwrap_or("<unnamed>")
                )
            });

        let encoded = echoed_envelope(&mut stream, recipient_name, "an agent message").await;
        let sender_name = sender
            .name
            .as_deref()
            .expect("the echo sender should have a name");
        assert!(
            encoded.contains(&format!("from=\"{sender_name}/{}\"", sender.host_id)),
            "the echoed tag carries the daemon-resolved name and host"
        );
        assert!(
            encoded.contains(&format!("from-id=\"{}\"", sender.id)),
            "the echoed tag carries the daemon-resolved agent id"
        );
        assert!(
            encoded.contains(&format!("from-kind=\"{}\"", sender.agent_type)),
            "the echoed tag carries the daemon-resolved agent kind"
        );
        let parsed = crate::envelope::parse(&encoded)
            .unwrap_or_else(|error| panic!("echoed envelope did not parse: {error}"));
        assert_eq!(parsed.id, envelope_id);
        assert_eq!(parsed.from, format!("{sender_name}/{}", sender.host_id));
        assert_eq!(parsed.from_id, Some(sender.id));
        assert_eq!(
            parsed.from_kind.as_deref(),
            Some(sender.agent_type.as_str())
        );
        assert_eq!(parsed.kind, crate::envelope::EnvelopeKind::Message);
        assert_eq!(parsed.text, text);
    }

    /// Takes a recipient host offline after its agent was observed, then
    /// restores that last inventory observation to reproduce a route loss
    /// between target selection and remote dispatch. Human callers see the
    /// failed delivery; live local agents retain fire-and-forget semantics.
    pub async fn unreachable_recipient_message_policy(
        &self,
        recipient_owner: &Daemon,
        sender: &Agent,
        recipient: &Agent,
    ) {
        assert_eq!(sender.host_id, self.host_id());
        assert_eq!(recipient.host_id, recipient_owner.host_id());

        recipient_owner.stop().await;
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        parts
            .client
            .apply_agent_event(crate::agents::AgentEvent::AgentUp {
                agent: recipient.clone(),
            })
            .await;

        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let (channel, _accept_task) = runtime.services.open_in_process_client_channel();
        drop(guard);
        let mut client =
            crate::protocol::wire::client_service_client::ClientServiceClient::new(channel);

        let human_error = client
            .send_message(crate::protocol::wire::ClientSendMessageRequest {
                to: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        recipient.id.as_bytes().to_vec(),
                    )),
                }),
                text: "unreachable human message".to_string(),
                context: None,
                from_agent_id: None,
            })
            .await
            .expect_err("a human sender must observe an unreachable recipient host");
        assert_eq!(human_error.code(), tonic::Code::Unavailable);

        let response = client
            .send_message(crate::protocol::wire::ClientSendMessageRequest {
                to: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        recipient.id.as_bytes().to_vec(),
                    )),
                }),
                text: "unreachable agent message".to_string(),
                context: None,
                from_agent_id: Some(sender.id.as_bytes().to_vec()),
            })
            .await
            .expect("an agent sender drops an unreachable fire-and-forget message")
            .into_inner();
        Uuid::from_slice(&response.envelope_id)
            .expect("the dropped response retains a valid envelope id");
    }

    /// Assertion: `agent_name` (eventually) appears in the inventory `other`
    /// serves to this daemon over the route — a real routed
    /// `ClientService.ListAgents`.
    pub async fn sees_agent_on(&self, other: &Daemon, agent_name: &str) {
        let assertion = format!(
            "'{}' lists agent '{agent_name}' on '{}'",
            self.name(),
            other.name()
        );
        eventually(
            &assertion,
            async || {
                self.lists_agents_on(other)
                    .await
                    .map(|agents| agents.iter().any(|name| name == agent_name))
                    .unwrap_or(false)
            },
            self.failure_dump(),
        )
        .await;
    }

    /// Attaches to a remote echo agent over this daemon's current route: opens
    /// a routed `ClientService.SubscribeSession` against `other` for the agent
    /// named `agent_name`. The returned [`EchoSession`] sends input and reads
    /// echoed output across the tunnel.
    pub async fn attach(&self, other: &Daemon, agent_name: &str) -> EchoSession {
        let description = format!(
            "echo session from '{}' to agent '{agent_name}' on '{}'",
            self.name(),
            other.name()
        );
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = parts
            .connections
            .channel_to(other.host_id())
            .await
            .unwrap_or_else(|error| panic!("failed to route {description}: {error}"));
        let client = Client::from_client_service_channel(channel, None);
        let stream = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: agent_name.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| panic!("failed to open {description}: {error}"));
        EchoSession {
            description,
            client,
            stream,
            agent_name: agent_name.to_string(),
        }
    }

    /// Runtime authority: `peer` (a paired remote) drives this daemon's
    /// `ClientService.Shutdown` over the route — a disruptive op reserved for
    /// fully-trusted callers, unlike the local-admin RPCs of [`Daemon`]'s
    /// pairing surface. The routed call is honored (not permission-denied),
    /// and the daemon goes down: this waits until `peer` can no longer see or
    /// reach it.
    ///
    /// The shutdown tears down the very link the call rode, so the RPC itself
    /// may surface a transport error; the observable contract is the daemon
    /// going offline, which this asserts.
    pub async fn shutdown_via(&self, peer: &Daemon) {
        let client = peer.routed_admin_client_to(self).await;
        // The reply races the socket teardown the shutdown triggers, so a
        // transport error here is expected and not a failure.
        let _ = client.shutdown().await;
        peer.cannot_call(self).await;
        peer.cannot_see(self).await;
    }

    /// Authority boundary (N-S-2): the local-admin trust RPCs `ListPeers`,
    /// `GetPeer`, and `Unpair` are rejected with permission-denied when a
    /// paired remote `peer` invokes them over the route, even though `peer` is
    /// fully trusted for runtime ops. None of them mutate state — the gate
    /// rejects before the handler body runs.
    pub async fn rejects_remote_trust_admin_from(&self, peer: &Daemon) {
        let client = peer.routed_admin_client_to(self).await;
        assert_permission_denied("ListPeers", client.list_peers().await.map(|_| ()));
        assert_permission_denied("GetPeer", client.get_peer(self.host_id()).await.map(|_| ()));
        assert_permission_denied(
            "Unpair",
            client
                .unpair(peer.host_id(), "spec authority probe")
                .await
                .map(|_| ()),
        );
    }

    /// Positive control for [`Self::rejects_remote_trust_admin_from`]: the
    /// same `ListPeers` RPC succeeds over this daemon's local Unix socket.
    pub async fn allows_local_trust_admin(&self) {
        self.admin_client()
            .await
            .list_peers()
            .await
            .unwrap_or_else(|error| {
                panic!("'{}' rejected a local ListPeers: {error}", self.name())
            });
    }

    /// Opens a `Client` over `peer`'s *routed* `ClientService` — a remote
    /// mTLS caller, not the local Unix socket. Used to assert what a paired
    /// remote peer may and may not invoke.
    pub(crate) async fn routed_admin_client_to(&self, peer: &Daemon) -> Client {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = parts
            .connections
            .channel_to(peer.host_id())
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' could not route to '{}': {error}",
                    self.name(),
                    peer.name()
                )
            });
        Client::from_client_service_channel(channel, None)
    }
}

async fn echoed_envelope(
    stream: &mut crate::SessionStream,
    recipient: &str,
    description: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
    let opening = b"<amux ";
    let closing = b"</amux>";
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = match tokio::time::timeout(remaining, stream.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => {
                panic!("echo stream for '{recipient}' ended before {description} arrived: {error}")
            }
            Err(_) => panic!(
                "'{recipient}' did not echo {description} within {DEFAULT_TIMEOUT:?} (saw {:?})",
                String::from_utf8_lossy(&seen)
            ),
        };
        match event {
            SubscribeSessionEvent::Output { payload } => {
                seen.extend_from_slice(&payload);
                let Some(start) = seen.windows(opening.len()).position(|part| part == opening)
                else {
                    continue;
                };
                let Some(relative_end) = seen[start..]
                    .windows(closing.len())
                    .position(|part| part == closing)
                else {
                    continue;
                };
                let end = start + relative_end + closing.len();
                return std::str::from_utf8(&seen[start..end])
                    .expect("the formatted message envelope is UTF-8")
                    .to_string();
            }
            SubscribeSessionEvent::Closed { reason } => {
                panic!("echo stream for '{recipient}' closed before delivery: {reason:?}")
            }
            SubscribeSessionEvent::Opened | SubscribeSessionEvent::ReplayComplete { .. } => {}
        }
    }
}

fn assert_parent_lifecycle_envelope(
    encoded: &str,
    child: &Agent,
    kind: crate::envelope::EnvelopeKind,
    text: &str,
) {
    let parsed = crate::envelope::parse(encoded)
        .unwrap_or_else(|error| panic!("parent lifecycle envelope did not parse: {error}"));
    assert_eq!(parsed.from_id, Some(child.id));
    assert_eq!(parsed.from_kind.as_deref(), Some("claude"));
    assert_eq!(parsed.kind, kind);
    assert_eq!(parsed.text, text);
}

/// A live routed echo session opened by [`Daemon::attach`]. Input sent with
/// [`Self::send`] is echoed straight back; [`Self::expect_output`] waits
/// (bounded) for the echo to arrive across the tunnel.
pub struct EchoSession {
    description: String,
    client: Client,
    stream: crate::SessionStream,
    agent_name: String,
}

impl EchoSession {
    /// Sends input to the agent across the route.
    pub async fn send(&self, input: &str) {
        self.client
            .send_input(SendInputRequest {
                agent: self.agent_name.as_str().into(),
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: TEST_ECHO_V1.to_string(),
                payload: bytes::Bytes::copy_from_slice(input.as_bytes()),
            })
            .await
            .unwrap_or_else(|error| {
                panic!("failed to send input on {}: {error}", self.description)
            });
    }

    /// Waits (bounded by the assertion timeout) for `expected` to arrive as
    /// session output over the route. Output that arrives in fragments is
    /// accumulated, so a partially-delivered echo still satisfies the match.
    pub async fn expect_output(&mut self, expected: &str) {
        let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let event = match tokio::time::timeout(remaining, self.stream.recv()).await {
                Ok(Ok(event)) => event,
                Ok(Err(error)) => panic!(
                    "{} ended with an error before producing '{expected}': {error}",
                    self.description
                ),
                Err(_) => panic!(
                    "{} did not echo '{expected}' within {DEFAULT_TIMEOUT:?} (saw {:?})",
                    self.description,
                    String::from_utf8_lossy(&seen)
                ),
            };
            match event {
                SubscribeSessionEvent::Output { payload } => {
                    seen.extend_from_slice(&payload);
                    if seen
                        .windows(expected.len())
                        .any(|window| window == expected.as_bytes())
                    {
                        return;
                    }
                }
                SubscribeSessionEvent::Closed { reason } => panic!(
                    "{} closed ({reason:?}) before echoing '{expected}'",
                    self.description
                ),
                // Stream lifecycle markers carry no echo payload.
                SubscribeSessionEvent::Opened | SubscribeSessionEvent::ReplayComplete { .. } => {}
            }
        }
    }
}
