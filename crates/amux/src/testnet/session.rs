//! Remote-session verbs: spawning echo agents, attaching across a route, and
//! exercising runtime authority over a paired peer.
//!
//! The test agent is the in-process echo PtyHandle (`TEST_ECHO_COMMAND` /
//! `TEST_ECHO_V1`), the same one the `services` unit tests drive; it echoes
//! whatever input it receives straight back as session output. `attach`
//! opens a routed `ClientService.SubscribeSession` against the agent's owner
//! over this daemon's current route, so input and output cross the real
//! tunnel.

use std::path::Path;

use bytes::Bytes;
use uuid::Uuid;

use super::Daemon;
use super::assertions::{DEFAULT_TIMEOUT, eventually};
use crate::agents::{TEST_ECHO_COMMAND, TEST_ECHO_V1};
use crate::client::{Client, ClientError};
use crate::services::LocalAgentHost;
use crate::{
    Agent, AgentParent, AgentType, ArtifactId, ArtifactKind, ArtifactRef, CreateAgentRequest,
    DiffBase, DiffResponse, ProtocolError, SendInputRequest, SendMessageRequest,
    SubscribeSessionEvent,
};

impl Daemon {
    /// Register a recorded Codex thread through the normal backend ingest and
    /// input paths. The caller keeps the recording transport alive.
    #[cfg(unix)]
    pub async fn spawn_recorded_codex(
        &self,
        name: &str,
        working_dir: impl AsRef<Path>,
        session: codex::Session,
    ) -> Result<Agent, ProtocolError> {
        let parts = self
            .try_parts()
            .await
            .ok_or_else(|| ProtocolError::ServerError {
                message: format!("daemon '{}' is not running", self.name()),
            })?;
        parts
            .agent_host
            .register_recorded_codex(name.into(), working_dir.as_ref().to_owned(), session)
            .await
    }

    /// Hold every queue slot of an echo PTY until the returned permits drop.
    pub async fn hold_echo_input(
        &self,
        agent: &Agent,
    ) -> Vec<tokio::sync::mpsc::OwnedPermit<Vec<u8>>> {
        use crate::agents::{Plane, Protocol, RawPtyTarget};

        let parts = self.try_parts().await.expect("daemon is running");
        let pty = {
            let state = parts.agent_host.state().read().await;
            match state.local_agents[&agent.id]
                .session
                .plane(Protocol::TestEchoV1)
                .unwrap()
            {
                Plane::Terminal(RawPtyTarget::Existing(pty)) => pty,
                _ => panic!("expected echo PTY"),
            }
        };
        pty.hold_echo_input().await
    }

    /// Spawns a local echo (test) agent named `name` through this daemon's
    /// profile `ClientService`, exactly as the CLI would. The agent
    /// echoes session input back as output. Returns once the agent is in the
    /// daemon's own inventory.
    pub async fn spawn_echo_agent(&self, name: &str) -> Agent {
        self.spawn_echo_agent_in(name, std::env::temp_dir()).await
    }

    /// Spawns a local echo agent rooted at `working_dir`, for operations such
    /// as diff capture that observe the agent's checkout.
    pub async fn spawn_echo_agent_in(&self, name: &str, working_dir: impl AsRef<Path>) -> Agent {
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
                working_dir: working_dir.as_ref().to_path_buf(),
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

    /// Registers a process-free Claude PTY agent with a stable id. This is a
    /// testnet stand-in for a provider process reconnecting after restart.
    pub async fn register_scripted_claude_agent(
        &self,
        agent_id: Uuid,
        name: &str,
        working_dir: impl AsRef<Path>,
    ) -> Agent {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        parts
            .agent_host
            .register_scripted_claude(CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::Claude {
                    driver: crate::ClaudeDriver::Pty,
                },
                working_dir: working_dir.as_ref().to_path_buf(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .unwrap_or_else(|error| panic!("register scripted Claude agent '{name}': {error}"))
    }

    /// Registers a live scripted provider in the daemon's normal session inventory.
    pub async fn spawn_scripted_agent(
        &self,
        name: &str,
        working_dir: impl AsRef<Path>,
        script: super::script::Script,
        parent: Option<AgentParent>,
    ) -> Result<(Agent, super::script::Provider), ProtocolError> {
        let parts = self
            .try_parts()
            .await
            .ok_or_else(|| ProtocolError::ServerError {
                message: format!("daemon '{}' is not running", self.name()),
            })?;
        parts
            .agent_host
            .register_scripted_provider(
                CreateAgentRequest {
                    agent_id: Uuid::new_v4(),
                    host_id: None,
                    name: Some(name.into()),
                    agent_type: AgentType::Claude {
                        driver: crate::ClaudeDriver::Pty,
                    },
                    working_dir: working_dir.as_ref().to_owned(),
                    terminal_size: None,
                    args: Vec::new(),
                    parent,
                    initial_prompt: None,
                },
                script,
            )
            .await
    }

    /// Stores bytes on `owner` for `agent`, routing through this daemon.
    pub async fn put_artifact_on(
        &self,
        owner: &Daemon,
        agent: &Agent,
        kind: ArtifactKind,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, ClientError> {
        self.client_to(owner)
            .await
            .put_artifact(agent.id.into(), kind, name, mime, bytes)
            .await
    }

    /// Fetches bytes from `owner` for `agent`, routing through this daemon.
    pub async fn get_artifact_on(
        &self,
        owner: &Daemon,
        agent: &Agent,
        id: &ArtifactId,
    ) -> Result<(ArtifactRef, Vec<u8>), ClientError> {
        self.client_to(owner)
            .await
            .get_artifact(agent.id.into(), id)
            .await
    }

    /// Captures the checkout diff on `owner` for `agent` through this daemon.
    pub async fn diff_on(
        &self,
        owner: &Daemon,
        agent: &Agent,
        base: DiffBase,
    ) -> Result<DiffResponse, ClientError> {
        self.client_to(owner)
            .await
            .diff(agent.id.into(), base)
            .await
    }

    /// Deletes `agent` on `owner` through this daemon.
    pub async fn delete_agent_on(&self, owner: &Daemon, agent: &Agent) -> Result<(), ClientError> {
        self.client_to(owner).await.delete_agent(agent.id).await
    }

    /// Sends an echo-protocol input with explicit pins, preserving typed
    /// service errors for boundary assertions.
    pub async fn send_echo_with_pins_on(
        &self,
        owner: &Daemon,
        agent: &Agent,
        text: &str,
        pin: Vec<ArtifactId>,
    ) -> Result<(), ClientError> {
        self.client_to(owner)
            .await
            .send_input(SendInputRequest {
                agent: agent.id.into(),
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: TEST_ECHO_V1.to_string(),
                payload: Bytes::copy_from_slice(text.as_bytes()),
                pin: pin.into_iter().map(|id| id.to_string()).collect(),
            })
            .await
    }

    /// Pins artifacts with a Claude prompt and returns the attachment row
    /// observed on the same routed session.
    pub async fn send_pinned_claude_prompt_on(
        &self,
        owner: &Daemon,
        agent: &Agent,
        text: &str,
        pin: Vec<ArtifactId>,
    ) -> Vec<ArtifactRef> {
        let client = self.client_to(owner).await;
        let mut stream = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: agent.id.into(),
                io_protocol: crate::claude_io::PTY_TRANSCRIPT_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| panic!("subscribe to scripted Claude agent: {error}"));
        let expected_seq = replay_cursor(&mut stream).await;
        let input_id = Uuid::new_v4().as_bytes().to_vec();
        let payload = crate::claude_io::encode_pty_transcript_v1_input(
            crate::claude_io::ClaudePtyTranscriptV1Input {
                expected_seq,
                intent: crate::claude_io::Intent::Prompt {
                    text: text.to_string(),
                },
            },
        );
        client
            .send_input(SendInputRequest {
                agent: agent.id.into(),
                input_id: input_id.clone(),
                io_protocol: crate::claude_io::PTY_TRANSCRIPT_V1.to_string(),
                payload: payload.into(),
                pin: pin.into_iter().map(|id| id.to_string()).collect(),
            })
            .await
            .unwrap_or_else(|error| panic!("send pinned Claude prompt: {error}"));
        attachment_refs(&mut stream, Some(&input_id)).await
    }

    /// Opens a fresh Claude session and returns the synthetic row containing
    /// every artifact pinned in earlier messages.
    pub async fn replayed_artifacts_on(&self, owner: &Daemon, agent: &Agent) -> Vec<ArtifactRef> {
        let mut stream = self
            .client_to(owner)
            .await
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: agent.id.into(),
                io_protocol: crate::claude_io::PTY_TRANSCRIPT_V1.to_string(),
                args: None,
            })
            .await
            .unwrap_or_else(|error| panic!("subscribe for pinned artifact replay: {error}"));
        attachment_refs(&mut stream, None).await
    }

    async fn client_to(&self, owner: &Daemon) -> Client {
        if self.host_id() == owner.host_id() {
            self.admin_client().await
        } else {
            self.routed_admin_client_to(owner).await
        }
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
        assert_eq!(parsed.from_kind.as_deref(), Some(parent.kind.provider()));
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

        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = runtime.client_channel.clone();
        drop(guard);
        let mut client = crate::protocol::wire::client_service_client(channel);
        let response = client
            .delete_agent(crate::protocol::wire::ClientDeleteAgentRequest {
                agent: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        parent.id.as_bytes().to_vec(),
                    )),
                }),
                caller_agent_id: None,
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

    /// Exercises the model-facing stop authority: only the recorded parent
    /// may stop a child, and stopping the child does not remove its parent.
    pub async fn parent_alone_stops_child(&self, parent: &Agent, child: &Agent, unrelated: &Agent) {
        let client = self.admin_client().await;
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

    /// Proves automatic and explicit work status through both fleet snapshots
    /// and live updates, then completes the child and observes the clear.
    pub async fn working_on_lifecycle(&self, parent: &Agent) {
        let first_line = "0123456789".repeat(9);
        let prompt = format!("{first_line}\nmore detail that is not part of the task name");
        let child = self
            .spawn_echo_child_with_prompt(parent, "working-child", &prompt)
            .await;
        let auto = child
            .working_on
            .as_ref()
            .expect("a spawned child has an automatic work status");
        assert_eq!(auto.text, first_line.chars().take(80).collect::<String>());

        let client = self.admin_client().await;
        let mut events = self
            .admin_client()
            .await
            .subscribe_agents()
            .await
            .expect("subscribe to fleet events");
        loop {
            if matches!(
                tokio::time::timeout(DEFAULT_TIMEOUT, events.recv())
                    .await
                    .expect("fleet snapshot completes"),
                Ok(crate::agents::AgentEvent::SnapshotComplete)
            ) {
                break;
            }
        }

        client
            .set_agent_status(crate::SetAgentStatusRequest {
                agent: child.id.into(),
                working_on: Some("reviewing the result".to_string()),
            })
            .await
            .expect("set child work status");
        let explicit = loop {
            let event = tokio::time::timeout(DEFAULT_TIMEOUT, events.recv())
                .await
                .expect("status update reaches the fleet stream")
                .expect("fleet stream remains open");
            if let crate::agents::AgentEvent::AgentUpdated { agent } = event
                && agent.id == child.id
            {
                break agent.working_on.expect("status update carries working_on");
            }
        };
        assert_eq!(explicit.text, "reviewing the result");
        assert!(explicit.updated_at >= auto.updated_at);

        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        parts
            .agent_host
            .event_tx()
            .send(crate::agents::SessionEvent::Completed {
                agent_id: child.id,
                text: "done".to_string(),
            })
            .await
            .expect("session event loop remains open");

        loop {
            let event = tokio::time::timeout(DEFAULT_TIMEOUT, events.recv())
                .await
                .expect("completion clear reaches the fleet stream")
                .expect("fleet stream remains open");
            if let crate::agents::AgentEvent::AgentUpdated { agent } = event
                && agent.id == child.id
            {
                assert!(agent.working_on.is_none());
                break;
            }
        }
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

    /// Parks a parent and child through the production local-host suspend
    /// seam, restarts the daemon runtime, resumes the saved sessions, and
    /// verifies their relationship metadata through the client inventory.
    pub async fn suspend_restart_preserves_family(&self, parent: &Agent, child: &Agent) {
        let state_path = self.inner.data_dir.join("state.yaml");
        let before = child
            .working_on
            .clone()
            .expect("spawned child has work to preserve");
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let suspended = parts
            .agent_host
            .prepare_suspend(state_path.clone())
            .await
            .expect("prepare suspend");
        assert_eq!(suspended, 2);
        parts.agent_host.commit_suspend().await;

        self.restart().await;
        let resumed_parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' did not restart", self.name()));
        let (resumed, failed) = resumed_parts
            .agent_host
            .resume(state_path, &crate::installation::OperationGate::default())
            .await
            .expect("resume suspended agents");
        assert_eq!((resumed, failed), (2, 0));
        let resumed_client = self.admin_client().await;

        eventually(
            "resumed family metadata reaches the client inventory",
            async || {
                let Ok(listed) = resumed_client.list_agents().await else {
                    return false;
                };
                let Some(resumed_parent) = listed.iter().find(|agent| agent.id == parent.id) else {
                    return false;
                };
                let Some(resumed_child) = listed.iter().find(|agent| agent.id == child.id) else {
                    return false;
                };
                resumed_parent.parent.is_none()
                    && resumed_child.parent == child.parent
                    && resumed_child.working_on.as_ref() == Some(&before)
            },
            self.failure_dump(),
        )
        .await;
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

        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = runtime.client_channel.clone();
        drop(guard);
        let mut client = crate::protocol::wire::client_service_client(channel);
        let response = client
            .delete_agent(crate::protocol::wire::ClientDeleteAgentRequest {
                agent: Some(crate::protocol::wire::AgentRef {
                    identifier: Some(crate::protocol::wire::agent_ref::Identifier::AgentId(
                        parent.id.as_bytes().to_vec(),
                    )),
                }),
                caller_agent_id: None,
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
                agent_type: AgentType::Claude {
                    driver: crate::ClaudeDriver::Pty,
                },
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
        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = runtime.client_channel.clone();
        drop(guard);
        let mut client = crate::protocol::wire::client_service_client(channel);
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
            encoded.contains(&format!("from-kind=\"{}\"", sender.kind.provider())),
            "the echoed tag carries the daemon-resolved agent kind"
        );
        let parsed = crate::envelope::parse(&encoded)
            .unwrap_or_else(|error| panic!("echoed envelope did not parse: {error}"));
        assert_eq!(parsed.id, envelope_id);
        assert_eq!(parsed.from, format!("{sender_name}/{}", sender.host_id));
        assert_eq!(parsed.from_id, Some(sender.id));
        assert_eq!(parsed.from_kind.as_deref(), Some(sender.kind.provider()));
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

        let guard = self.runtime().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let channel = runtime.client_channel.clone();
        drop(guard);
        let mut client = crate::protocol::wire::client_service_client(channel);

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
        let client = if self.host_id() == other.host_id() {
            self.admin_client().await
        } else {
            let parts = self
                .try_parts()
                .await
                .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
            let channel = parts
                .connections
                .channel_to(other.host_id())
                .await
                .unwrap_or_else(|error| panic!("failed to route {description}: {error}"));
            Client::from_client_service_channel(channel)
        };
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

    /// Lifecycle, pairing and trust administration are absent from a peer's ClientService.
    pub async fn rejects_remote_admin_from(&self, peer: &Daemon) {
        let parts = peer.try_parts().await.expect("peer is running");
        let channel = parts
            .connections
            .channel_to(self.host_id())
            .await
            .expect("peer route");
        assert_admin_absent(channel, "peer tunnel").await;
        peer.can_call(self).await;
    }

    /// The same administration methods are absent from the plain profile socket.
    #[cfg(unix)]
    pub async fn rejects_admin_on_socket(&self, socket_path: std::path::PathBuf) {
        let config = crate::Config {
            socket_path,
            ..Default::default()
        };
        let channel = crate::client::connect_existing_client_service(&config)
            .await
            .expect("profile socket");
        assert_admin_absent(channel, "profile socket").await;
    }

    /// The installation owner can inspect trust in process.
    pub async fn allows_owner_trust_admin(&self) {
        self.pairing_admin()
            .await
            .list_peers()
            .await
            .expect("owner trust inventory");
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
        Client::from_client_service_channel(channel)
    }
}

async fn replay_cursor(stream: &mut crate::SessionStream) -> u64 {
    let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("timed out waiting for scripted Claude replay cursor")
            .expect("scripted Claude replay failed");
        match event {
            SubscribeSessionEvent::ReplayComplete {
                cursor: Some(cursor),
            } => {
                return crate::claude_io::decode_pty_transcript_v1_cursor(&cursor)
                    .expect("scripted Claude replay cursor decodes");
            }
            SubscribeSessionEvent::ReplayComplete { cursor: None } => {
                panic!("scripted Claude replay omitted its cursor")
            }
            SubscribeSessionEvent::Closed { reason } => {
                panic!("scripted Claude session closed during replay: {reason}")
            }
            SubscribeSessionEvent::Opened | SubscribeSessionEvent::Output { .. } => {}
        }
    }
}

async fn attachment_refs(
    stream: &mut crate::SessionStream,
    expected_input_id: Option<&[u8]>,
) -> Vec<ArtifactRef> {
    let expected_input_id = expected_input_id.map(hex_bytes);
    let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("timed out waiting for an attachment row")
            .expect("attachment row stream failed");
        match event {
            SubscribeSessionEvent::Output { payload } => {
                let output = crate::claude_io::decode_pty_transcript_v1_output(&payload)
                    .expect("Claude output decodes");
                let value: serde_json::Value =
                    serde_json::from_slice(&output.payload).expect("Claude output contains JSON");
                if value.get("type").and_then(serde_json::Value::as_str) != Some("amux.attachments")
                {
                    continue;
                }
                let input_matches = match (&expected_input_id, value.get("input_id")) {
                    (Some(expected), Some(serde_json::Value::String(actual))) => actual == expected,
                    (None, Some(serde_json::Value::Null)) => true,
                    _ => false,
                };
                if input_matches {
                    return serde_json::from_value(value["refs"].clone())
                        .expect("attachment row refs decode");
                }
            }
            SubscribeSessionEvent::Closed { reason } => {
                panic!("scripted Claude session closed before its attachment row: {reason}")
            }
            SubscribeSessionEvent::Opened | SubscribeSessionEvent::ReplayComplete { .. } => {}
        }
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut encoded, byte| {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        encoded
    })
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
    /// The existing subscription must close; opening a fresh call is no proof
    /// that an already accepted stream was torn down.
    pub async fn expect_disconnect(mut self) {
        tokio::time::timeout(DEFAULT_TIMEOUT, async {
            loop {
                match self.stream.recv().await {
                    Err(_) | Ok(SubscribeSessionEvent::Closed { .. }) => return,
                    Ok(_) => {}
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} stayed open", self.description));
    }

    /// Sends input to the agent across the route.
    pub async fn send(&self, input: &str) {
        self.client
            .send_input(SendInputRequest {
                agent: self.agent_name.as_str().into(),
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: TEST_ECHO_V1.to_string(),
                payload: bytes::Bytes::copy_from_slice(input.as_bytes()),
                pin: Vec::new(),
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

async fn assert_admin_absent(channel: tonic::transport::Channel, boundary: &str) {
    for service in ["ClientService", "ProfileService", "InstallationService"] {
        for method in [
            "Shutdown",
            "Suspend",
            "Resume",
            "SuspendAll",
            "ResumeAll",
            "CreateProfile",
            "BindProfile",
            "LogoutProfile",
            "PauseProfile",
            "ResumeProfile",
            "RenameProfile",
            "DeleteProfile",
            "ListPairingCandidates",
            "StartPairing",
            "GetPairingStatus",
            "CancelPairing",
            "PairPeer",
            "PairPinCloudPeer",
            "PairQrCloudPeer",
            "GetDeviceIdentity",
            "BeginPair",
            "ConfirmPair",
            "AbandonPair",
            "ListPeers",
            "GetPeer",
            "Unpair",
        ] {
            let path = format!("/amux.v1.{service}/{method}");
            let mut grpc = tonic::client::Grpc::new(channel.clone());
            grpc.ready().await.expect("profile channel ready");
            let result: Result<tonic::Response<crate::protocol::wire::ListPeersResponse>, _> = grpc
                .unary(
                    tonic::Request::new(crate::protocol::wire::ListPeersRequest {}),
                    path.parse().unwrap(),
                    tonic_prost::ProstCodec::default(),
                )
                .await;
            let status = result.expect_err("administration must not be served on ClientService");
            assert_eq!(
                status.code(),
                tonic::Code::Unimplemented,
                "{path}: {status}"
            );
            println!("{path}: UNIMPLEMENTED over {boundary}");
        }
    }
}
