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
use crate::{AgentType, CreateAgentRequest, SendInputRequest, SubscribeSessionEvent};

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
    pub async fn spawn_echo_agent(&self, name: &str) {
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
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to spawn echo agent '{name}': {error}",
                    self.name()
                )
            });
        assert_eq!(agent.name.as_deref(), Some(name));
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
