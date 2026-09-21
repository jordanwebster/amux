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

use agent_runtime::test_support::TEST_ECHO_COMMAND;
use client::{Client, ClientError};
use node::{
    Agent, AgentParent, AgentType, ArtifactId, ArtifactKind, ArtifactRef, CreateAgentRequest,
    DiffBase, DiffResponse, ProtocolError, SendInputRequest, SubscribeSessionEvent,
};
use uuid::Uuid;

use super::Daemon;
use super::assertions::{DEFAULT_TIMEOUT, eventually};

impl Daemon {
    /// Replace only the provider transport for SDK sessions created on this host.
    /// Creation still uses the service's validation, backend and live registry.
    pub async fn script_sdk_sessions(&self, script: super::sdk::Script) {
        self.inner
            .sources
            .script_sdk(super::sdk::Provider::new(script));
    }

    /// Provider-side observations for an SDK agent created on this daemon.
    pub async fn observed_sdk_inputs(&self, agent: Uuid) -> Option<Vec<serde_json::Value>> {
        self.inner.sources.sdk()?.observed(agent)
    }

    /// Spawn a Claude SDK agent against the daemon's installed scripted SDK
    /// transport, through the same create service used by production clients.
    pub async fn spawn_scripted_sdk_agent(
        &self,
        name: &str,
        working_dir: impl AsRef<Path>,
    ) -> Result<Agent, ClientError> {
        self.admin_client()
            .await
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(name.to_owned()),
                agent_type: AgentType::Claude {
                    driver: model::ClaudeDriver::Sdk,
                },
                working_dir: working_dir.as_ref().to_owned(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
    }

    /// Publish raw rows from the installed scripted SDK transport.
    pub async fn emit_scripted_sdk_rows(
        &self,
        agent: Uuid,
        rows: Vec<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let provider = self.inner.sources.sdk().ok_or_else(|| {
            anyhow::anyhow!("daemon '{}' has no scripted SDK transport", self.name())
        })?;
        provider.emit(agent, rows).await
    }

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
        agent_runtime::test_support::register_codex_session(
            parts.agent_host.as_ref(),
            name.into(),
            working_dir.as_ref().to_owned(),
            session,
        )
        .await
    }

    /// Hold every queue slot of an echo PTY until the returned permits drop.
    pub async fn hold_echo_input(
        &self,
        agent: &Agent,
    ) -> Vec<tokio::sync::mpsc::OwnedPermit<Vec<u8>>> {
        let parts = self.try_parts().await.expect("daemon is running");
        agent_runtime::test_support::hold_echo_input(parts.agent_host.as_ref(), agent.id).await
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
        if agent.name.as_deref() != Some(name) {
            panic!("spawned echo agent did not retain the requested name '{name}'");
        }
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
        agent_runtime::test_support::register_scripted_claude(
            parts.agent_host.as_ref(),
            CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::Claude {
                    driver: model::ClaudeDriver::Pty,
                },
                working_dir: working_dir.as_ref().to_path_buf(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("register scripted Claude agent '{name}': {error}"))
    }

    /// A scripted child of `parent`, answering with the same script, raised
    /// on the daemon that runs the parent.
    pub async fn spawn_child(
        &self,
        parent: &Agent,
        name: &str,
        script: super::script::Script,
    ) -> Result<(Agent, super::script::Provider), ProtocolError> {
        self.spawn_scripted_agent(
            name,
            &parent.working_dir,
            script,
            Some(AgentParent {
                agent_id: parent.id,
                host_id: parent.host_id,
            }),
        )
        .await
    }

    /// How many live links this daemon holds, its relay link included. RPC
    /// tunnels ride on links and are not counted.
    pub async fn connections(&self) -> usize {
        self.debug_dump(false).await["links"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
    }

    /// How many live links this daemon holds to one peer.
    pub async fn links_to(&self, other: &Daemon) -> usize {
        let peer = other.host_id().to_string();
        self.debug_dump(false).await["links"]
            .as_array()
            .map(|links| {
                links
                    .iter()
                    .filter(|link| link["peer"].as_str() == Some(peer.as_str()))
                    .count()
            })
            .unwrap_or_default()
    }

    /// What this daemon says it is holding: every agent on it, as it
    /// recorded them, and every device it trusts.
    pub async fn inventory(&self) -> Result<(Vec<Agent>, Vec<client::PeerEntry>), anyhow::Error> {
        let agents = self.admin_client().await.list_agents().await?;
        let devices = self.pairing_admin().await.list_peers().await?;
        Ok((agents, devices))
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
        // The session is built from scripted sources before the runtime
        // sees it; the runtime then runs its ordinary Claude backend over it
        // and reports the semantic input it accepts back to the script.
        let (session, provider) =
            super::script::session(script)
                .await
                .map_err(|error| ProtocolError::ServerError {
                    message: format!("scripted Claude session: {error}"),
                })?;
        let agent_id = Uuid::new_v4();
        self.inner.sources.attach_claude(agent_id, provider.clone());
        let agent = agent_runtime::test_support::register_claude_pty_session(
            parts.agent_host.as_ref(),
            CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(name.into()),
                agent_type: AgentType::Claude {
                    driver: model::ClaudeDriver::Pty,
                },
                working_dir: working_dir.as_ref().to_owned(),
                terminal_size: None,
                args: Vec::new(),
                parent,
                initial_prompt: None,
            },
            session,
        )
        .await?;
        Ok((agent, provider))
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

    /// Fetches through this daemon's local profile service, which resolves
    /// the remote owner and assigns the transfer its dedicated bulk channel.
    pub async fn fetch_artifact_via_profile(
        &self,
        owner: &Daemon,
        agent: &Agent,
        id: &ArtifactId,
    ) -> tokio::task::JoinHandle<Result<(ArtifactRef, Vec<u8>), ClientError>> {
        let name = agent.name.as_deref().expect("test agent has a name");
        self.sees_agent_on(owner, name).await;
        let client = self.admin_client().await;
        let agent_id = agent.id;
        let id = id.clone();
        tokio::spawn(async move { client.get_artifact(agent_id.into(), &id).await })
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
                input: model::SessionInput::TestEchoV1 {
                    payload: text.as_bytes().to_vec(),
                },
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
            .subscribe_session(node::SubscribeSessionRequest {
                agent: agent.id.into(),
                args: model::SessionArgs::ClaudePtyTranscriptV1(
                    model::ClaudePtyTranscriptV1Args::default(),
                ),
            })
            .await
            .unwrap_or_else(|error| panic!("subscribe to scripted Claude agent: {error}"));
        let expected_seq = replay_cursor(&mut stream).await;
        let input_id = Uuid::new_v4().as_bytes().to_vec();
        client
            .send_input(SendInputRequest {
                agent: agent.id.into(),
                input_id: input_id.clone(),
                input: model::SessionInput::ClaudePtyTranscriptV1(
                    model::ClaudePtyTranscriptV1Input {
                        expected_seq,
                        intent: model::ClaudePtyIntent::Prompt {
                            text: text.to_string(),
                        },
                    },
                ),
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
            .subscribe_session(node::SubscribeSessionRequest {
                agent: agent.id.into(),
                args: model::SessionArgs::ClaudePtyTranscriptV1(
                    model::ClaudePtyTranscriptV1Args::default(),
                ),
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

    /// Spawns an echo child with an initial prompt after its backend is
    /// available. The session replay retains that prompt for the caller to
    /// observe.
    pub async fn spawn_echo_child_with_prompt(
        &self,
        parent: &Agent,
        name: &str,
        prompt: &str,
    ) -> Agent {
        if parent.host_id != self.host_id() {
            panic!("an echo child's parent must belong to the creating daemon");
        }
        self.admin_client()
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
            .unwrap_or_else(|error| panic!("spawn echo child '{name}': {error}"))
    }

    /// Spawns an echo child on `owner` while preserving a parent local to the
    /// calling daemon. This exercises the same remote create route used by a
    /// model-facing spawn.
    pub async fn spawn_echo_child_on(&self, owner: &Daemon, parent: &Agent, name: &str) -> Agent {
        if parent.host_id != self.host_id() {
            panic!("an echo child's parent must belong to the creating daemon");
        }
        if owner.host_id() != self.host_id() {
            // The daemon dispatches this create over its own link to the
            // owner, and answers with whatever that dispatch met. Waiting for
            // a route here keeps the failure about the spawn.
            self.channel_once_routed(owner, &format!("a spawn on '{}'", owner.name()))
                .await;
        }
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

    /// Waits until this daemon's local client has observed every named agent,
    /// then returns those observations in the requested order.
    pub async fn observes_agents(&self, ids: &[Uuid]) -> Vec<Agent> {
        let client = self.admin_client().await;
        let mut observed = None;
        eventually(
            "daemon observes the requested agents",
            async || {
                let Ok(agents) = client.list_agents().await else {
                    return false;
                };
                if ids
                    .iter()
                    .all(|id| agents.iter().any(|agent| agent.id == *id))
                {
                    observed = Some(agents);
                    true
                } else {
                    false
                }
            },
            self.failure_dump(),
        )
        .await;
        let observed = observed.expect("the requested agents were observed");
        ids.iter()
            .map(|id| {
                observed
                    .iter()
                    .find(|agent| agent.id == *id)
                    .expect("observed agent remains in captured inventory")
                    .clone()
            })
            .collect()
    }

    /// Completes a process-free echo agent through the local host boundary.
    pub async fn complete_echo_agent(&self, agent: &Agent, result: &str) {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        agent_runtime::test_support::complete(
            parts.agent_host.as_ref(),
            agent.id,
            result.to_string(),
        )
        .await;
    }

    /// Parks every local agent and commits their suspend records.
    pub async fn suspend_agents(&self) -> u64 {
        let state_path = self.inner.data_dir.join("state.yaml");
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let suspended = parts
            .agent_host
            .prepare_suspend(state_path.clone())
            .await
            .expect("prepare suspend");
        parts.agent_host.commit_suspend().await;
        suspended
    }

    /// Resumes every committed suspend record after a daemon restart.
    pub async fn resume_agents(&self) -> (u64, u64) {
        let state_path = self.inner.data_dir.join("state.yaml");
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' did not restart", self.name()));
        parts
            .agent_host
            .resume(state_path, &host_api::OperationGate::default())
            .await
            .expect("resume suspended agents")
    }

    /// Restores a captured agent observation without restoring its owner.
    pub async fn restore_agent_observation(&self, agent: &Agent) {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        parts
            .client
            .apply_agent_event(node::harness::AgentEvent::AgentUp {
                agent: agent.clone(),
            })
            .await;
    }

    /// Registers a process-free Claude child with a possibly remote parent.
    pub async fn register_scripted_claude_child(&self, parent: &Agent) -> Agent {
        let child_id = Uuid::new_v4();
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        agent_runtime::test_support::register_scripted_claude(
            parts.agent_host.as_ref(),
            CreateAgentRequest {
                agent_id: child_id,
                host_id: None,
                name: Some("claude-child".to_string()),
                agent_type: AgentType::Claude {
                    driver: model::ClaudeDriver::Pty,
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
                parent: Some(AgentParent {
                    agent_id: parent.id,
                    host_id: parent.host_id,
                }),
                initial_prompt: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("register scripted Claude child: {error}"))
    }

    /// Delivers a scripted Claude Stop hook for a local child.
    pub async fn deliver_scripted_claude_completion(
        &self,
        child: &Agent,
        last_assistant_message: &str,
    ) {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": Uuid::new_v4(),
            "transcript_path": "/nonexistent/amux-scripted-claude.jsonl",
            "cwd": std::env::temp_dir(),
            "last_assistant_message": last_assistant_message,
            "stop_hook_active": false,
        }))
        .expect("scripted Stop hook serializes");
        agent_runtime::test_support::deliver_scripted_hook(
            parts.agent_host.as_ref(),
            child.id,
            payload,
        )
        .await
        .unwrap_or_else(|error| panic!("deliver scripted Stop hook: {error}"));
    }

    /// Ends a local process-free Claude session.
    pub async fn end_scripted_session(&self, child: &Agent) {
        let parts = self
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        agent_runtime::test_support::end_scripted_session(parts.agent_host.as_ref(), child.id)
            .await;
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
    /// A channel to `other`, waited for rather than demanded.
    ///
    /// When both machines dial each other at once they keep one of the two
    /// links and drop the other, and a call made in the moment between is
    /// refused for want of a link. A settled network reaches that state on its
    /// own, so a verb that gives up on the first refusal is asserting when the
    /// call was made rather than whether it can be made — which is why these
    /// were the suite's flakiest tests on a loaded machine.
    async fn channel_once_routed(
        &self,
        other: &Daemon,
        description: &str,
    ) -> tonic::transport::Channel {
        let mut routed = None;
        eventually(
            &format!("'{}' can route {description}", self.name()),
            async || {
                let Some(parts) = self.try_parts().await else {
                    return false;
                };
                routed = parts.connections.channel_to(other.host_id()).await.ok();
                routed.is_some()
            },
            self.failure_dump(),
        )
        .await;
        routed.expect("a routed channel once routing offers one")
    }

    pub async fn attach(&self, other: &Daemon, agent_name: &str) -> EchoSession {
        let description = format!(
            "echo session from '{}' to agent '{agent_name}' on '{}'",
            self.name(),
            other.name()
        );
        let client = if self.host_id() == other.host_id() {
            self.admin_client().await
        } else {
            Client::from_channel(self.channel_once_routed(other, &description).await)
        };
        let stream = client
            .subscribe_session(node::SubscribeSessionRequest {
                agent: agent_name.into(),
                args: model::SessionArgs::TestEchoV1,
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

    /// Attaches through this daemon's local profile service. Unlike a direct
    /// test call to the peer service, this exercises remote resolution and the
    /// dedicated session channel chosen for the resolved agent.
    pub async fn attach_via_profile(&self, other: &Daemon, agent_name: &str) -> EchoSession {
        self.sees_agent_on(other, agent_name).await;
        let description = format!(
            "profile-routed echo session from '{}' to agent '{agent_name}' on '{}'",
            self.name(),
            other.name()
        );
        let client = self.admin_client().await;
        let stream = client
            .subscribe_session(node::SubscribeSessionRequest {
                agent: agent_name.into(),
                args: model::SessionArgs::TestEchoV1,
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
        let channel = peer
            .channel_once_routed(self, "an administration call")
            .await;
        assert_admin_absent(channel, "peer tunnel").await;
        peer.can_call(self).await;
    }

    /// The same administration methods are absent from the plain profile socket.
    #[cfg(unix)]
    pub async fn rejects_admin_on_socket(&self, socket_path: std::path::PathBuf) {
        let config = node::Config {
            socket_path,
            ..Default::default()
        };
        let channel = client::connect_socket(&config.socket_path)
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
        let channel = self
            .channel_once_routed(
                peer,
                &format!("an administration call to '{}'", peer.name()),
            )
            .await;
        Client::from_channel(channel)
    }
}

async fn replay_cursor(stream: &mut node::SessionStream) -> u64 {
    let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
    let mut through = None;
    loop {
        let event = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("timed out waiting for scripted Claude replay cursor")
            .expect("scripted Claude replay failed");
        match event {
            SubscribeSessionEvent::Opened {
                replay: Some(facts),
            } => through = Some(facts.through),
            SubscribeSessionEvent::Opened { replay: None } => {
                panic!("scripted Claude replay omitted its facts")
            }
            SubscribeSessionEvent::ReplayComplete => {
                return through.expect("scripted Claude replay omitted its opening watermark");
            }
            SubscribeSessionEvent::Closed { reason } => {
                panic!("scripted Claude session closed during replay: {reason}")
            }
            SubscribeSessionEvent::Output(_) => {}
        }
    }
}

async fn attachment_refs(
    stream: &mut node::SessionStream,
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
            SubscribeSessionEvent::Output(model::SessionOutput::ClaudePtyTranscriptV1(output)) => {
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
            SubscribeSessionEvent::Opened { .. } | SubscribeSessionEvent::ReplayComplete => {}
            SubscribeSessionEvent::Output(_) => {
                panic!("scripted Claude session emitted output for the wrong protocol")
            }
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
    stream: &mut node::SessionStream,
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
            SubscribeSessionEvent::Output(model::SessionOutput::TestEchoV1 { payload }) => {
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
            SubscribeSessionEvent::Opened { .. } | SubscribeSessionEvent::ReplayComplete => {}
            SubscribeSessionEvent::Output(_) => {
                panic!("echo stream emitted output for the wrong protocol")
            }
        }
    }
}

/// A live routed echo session opened by [`Daemon::attach`]. Input sent with
/// [`Self::send`] is echoed straight back; [`Self::expect_output`] waits
/// (bounded) for the echo to arrive across the tunnel.
pub struct EchoSession {
    description: String,
    client: Client,
    stream: node::SessionStream,
    agent_name: String,
}

impl EchoSession {
    /// Waits for and returns the next complete authenticated message envelope.
    pub async fn expect_envelope(&mut self, description: &str) -> String {
        echoed_envelope(&mut self.stream, &self.agent_name, description).await
    }

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
                input: model::SessionInput::TestEchoV1 {
                    payload: input.as_bytes().to_vec(),
                },
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
                SubscribeSessionEvent::Output(model::SessionOutput::TestEchoV1 { payload }) => {
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
                SubscribeSessionEvent::Opened { .. } | SubscribeSessionEvent::ReplayComplete => {}
                SubscribeSessionEvent::Output(_) => {
                    panic!("echo stream emitted output for the wrong protocol")
                }
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
            "BeginPair",
            "ConfirmPair",
            "AbandonPair",
            "GetDeviceIdentity",
            "TrustSshPeer",
            "ListPeers",
            "GetPeer",
            "Unpair",
        ] {
            let path = format!("/amux.v1.{service}/{method}");
            let mut grpc = tonic::client::Grpc::new(channel.clone());
            grpc.ready().await.expect("profile channel ready");
            let result: Result<tonic::Response<wire::ListPeersResponse>, _> = grpc
                .unary(
                    tonic::Request::new(wire::ListPeersRequest {}),
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
