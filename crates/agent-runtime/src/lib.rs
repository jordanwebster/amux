#![allow(clippy::result_large_err)]

pub mod agent_tools;
mod agents;
mod debug;
mod events;
mod host;
mod repositories;
mod suspend;

pub use host::{AgentRuntime, AgentRuntimeFactory};

#[cfg(unix)]
mod test_support_provider;

#[doc(hidden)]
pub mod test_support {
    use std::io;
    use std::path::Path;
    use std::sync::Arc;

    use host_api::{HostConfig, LocalAgentHost, LocalAgentHostFactory};
    use model::{
        Agent, AgentId, AgentKind, AgentType, ClaudeDriver, CreateAgentRequest, Protocol,
        ProtocolError,
    };
    use tokio::sync::mpsc::OwnedPermit;
    use uuid::Uuid;

    use crate::AgentRuntime;
    use crate::agents::claude::sdk_io::ClaudeSdkV1Input;
    use crate::agents::{
        AgentSession, McpLaunchRoute, Plane, RawPtyTarget, SessionEvent, new_agent,
    };
    #[cfg(unix)]
    pub use crate::test_support_provider::{
        ClaudeSdkFixtureInput, CodexFixtureInput, FixtureRowReader, StructuredBackendAdapter,
        fixture_runtime, register_sdk_fixture,
    };
    pub type CodexSdkV1Input = model::CodexSdkInput;

    /// Construct a provider backend without starting an external process and
    /// ask it for the selected protocol plane.
    pub async fn open_in_process_plane(
        kind: AgentKind,
        protocol: Protocol,
    ) -> Result<(), ProtocolError> {
        let host = runtime(Uuid::new_v4());
        let agent_id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id,
            host_id: None,
            name: Some("typed-protocol-test".into()),
            agent_type: match kind {
                AgentKind::Claude { driver } => AgentType::Claude { driver },
                AgentKind::Codex => AgentType::Codex {
                    model: None,
                    approval_policy: None,
                    sandbox_policy: None,
                    resume_thread_id: None,
                },
                AgentKind::TestAgent => AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.into(),
                },
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        };
        let deps = host.state().read().await.deps.clone();
        let session: AgentSession = match kind {
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            } => Box::new(crate::agents::claude::ClaudeSession::scripted_for_testnet(
                &request,
                deps.runtime_dir.clone(),
                deps.claude_version_cache.clone(),
                deps.mcp_launch_route.clone(),
                deps.claude_user_keymap_dir.clone(),
            )),
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            }
            | AgentKind::Codex
            | AgentKind::TestAgent => {
                new_agent(&request, &deps).map_err(|error| ProtocolError::ServerError {
                    message: error.to_string(),
                })?
            }
        };
        session.plane(protocol).map(|_| ())
    }

    /// Prove the SDK composition can construct its backend without launching it.
    pub async fn create_sdk() -> Result<(), ProtocolError> {
        let host = runtime(Uuid::new_v4());
        let request = CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("sdk-placeholder".into()),
            parent: None,
            initial_prompt: None,
            agent_type: AgentType::Claude {
                driver: ClaudeDriver::Sdk,
            },
            working_dir: std::env::temp_dir(),
            args: Vec::new(),
            terminal_size: None,
        };
        let state = host.state().read().await;
        let session =
            new_agent(&request, &state.deps).map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
        if session.kind()
            != (AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            })
        {
            return Err(ProtocolError::ServerError {
                message: "SDK constructor returned the wrong backend kind".into(),
            });
        }
        Ok(())
    }

    pub const TEST_ECHO_COMMAND: &str = "__amux_test_echo__";
    pub const TEST_DELAYED_DELIVERY_COMMAND: &str = "__amux_test_delayed_delivery__";
    pub const TEST_FAILED_DELIVERY_COMMAND: &str = "__amux_test_failed_delivery__";
    pub const TEST_UNAVAILABLE_DELIVERY_COMMAND: &str = "__amux_test_unavailable_delivery__";
    pub const TEST_ECHO_V1: &str = "test_echo_v1";

    pub fn runtime(host_id: Uuid) -> Arc<AgentRuntime> {
        let data_dir = tempfile::Builder::new()
            .prefix("amux-agent-runtime-")
            .tempdir_in(std::env::temp_dir())
            .expect("create agent runtime test directory")
            .keep();
        let socket_path = data_dir.join("amux.sock");
        let route =
            McpLaunchRoute::new(std::env::current_exe().unwrap(), None, socket_path, host_id)
                .expect("create test MCP route");
        let mut runtime = AgentRuntime::new_with_mcp_launch_route(
            route,
            data_dir.join("keymaps"),
            data_dir.clone(),
        )
        .expect("create test agent runtime");
        Arc::get_mut(&mut runtime)
            .expect("new test runtime has one owner")
            .test_cleanup = Some(data_dir);
        runtime
    }

    fn concrete(host: &dyn LocalAgentHost) -> &AgentRuntime {
        host.as_any()
            .downcast_ref::<AgentRuntime>()
            .expect("test support requires AgentRuntime")
    }

    #[derive(Clone)]
    pub struct Factory {
        clock: Arc<dyn artifacts::Clock>,
        sources: Option<Arc<dyn ProviderSources>>,
    }

    impl Factory {
        pub fn new(clock: Arc<dyn artifacts::Clock>) -> Self {
            Self {
                clock,
                sources: None,
            }
        }

        /// Every runtime this factory creates draws its provider sessions
        /// from `sources` instead of launching providers.
        pub fn with_sources(mut self, sources: Arc<dyn ProviderSources>) -> Self {
            self.sources = Some(sources);
            self
        }
    }

    impl LocalAgentHostFactory for Factory {
        fn create(&self, config: HostConfig) -> Result<Arc<dyn LocalAgentHost>, io::Error> {
            let route = McpLaunchRoute::new(
                config.executable,
                config.profile_config_path,
                config.server_socket_path,
                config.host_id,
            )?;
            let host = AgentRuntime::new_with_artifact_clock(
                route,
                config.claude_user_keymap_dir,
                config.data_dir,
                Some(config.state_path),
                self.clock.clone(),
                config.repository_roots,
            )?;
            if let Some(sources) = &self.sources {
                // Nothing has been created yet, so the write lock is free.
                host.state()
                    .try_write()
                    .expect("fresh runtime state is unlocked")
                    .deps
                    .sources = Some(sources.clone());
            }
            Ok(host as Arc<dyn LocalAgentHost>)
        }

        fn restore_prepared(
            &self,
            state_path: &Path,
            state: host_api::PreparedHostState,
        ) -> Result<(), ProtocolError> {
            crate::host::restore_prepared_at(state_path, state)
        }
    }

    pub async fn hold_echo_input(
        host: &dyn LocalAgentHost,
        agent_id: AgentId,
    ) -> Vec<OwnedPermit<Vec<u8>>> {
        let host = concrete(host);
        let pty = {
            let state = host.state().read().await;
            match state.local_agents[&agent_id]
                .session
                .plane(Protocol::TestEchoV1)
                .expect("echo agent supports test protocol")
            {
                Plane::Terminal(RawPtyTarget::Existing(pty)) => pty,
                _ => panic!("expected echo PTY"),
            }
        };
        pty.hold_echo_input().await
    }

    pub async fn register_scripted_claude(
        host: &dyn LocalAgentHost,
        request: CreateAgentRequest,
    ) -> Result<Agent, ProtocolError> {
        concrete(host).register_scripted_claude(request).await
    }

    pub use crate::agents::{ClaudeSdkSource, ProviderSources};

    /// The runtime's own form of a Claude SDK input, from the shared value a
    /// client encoded.
    pub fn claude_sdk_input_from_model(
        input: model::ClaudeSdkInput,
    ) -> Result<ClaudeSdkV1Input, ProtocolError> {
        crate::host::claude_sdk_input(input)
    }

    /// Install the supplier of sessions and input observation for every agent
    /// this runtime creates from now on.
    pub async fn set_provider_sources(
        host: &dyn LocalAgentHost,
        sources: Arc<dyn ProviderSources>,
    ) {
        concrete(host).state().write().await.deps.sources = Some(sources);
    }

    /// Register a Claude PTY agent over a session built from supplied sources.
    pub async fn register_claude_pty_session(
        host: &dyn LocalAgentHost,
        request: CreateAgentRequest,
        session: claude::pty::Session,
    ) -> Result<Agent, ProtocolError> {
        concrete(host)
            .register_claude_pty_session(request, session)
            .await
    }

    /// Register a Codex agent over a supplied session.
    #[cfg(unix)]
    pub async fn register_codex_session(
        host: &dyn LocalAgentHost,
        name: String,
        working_dir: std::path::PathBuf,
        session: codex::Session,
    ) -> Result<Agent, ProtocolError> {
        concrete(host)
            .register_codex_session(name, working_dir, session)
            .await
    }

    pub async fn deliver_scripted_hook(
        host: &dyn LocalAgentHost,
        agent_id: AgentId,
        payload: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        concrete(host)
            .deliver_scripted_hook(agent_id, payload)
            .await
    }

    pub async fn end_scripted_session(host: &dyn LocalAgentHost, agent_id: AgentId) {
        concrete(host).end_scripted_session(agent_id).await;
    }

    pub async fn complete(host: &dyn LocalAgentHost, agent_id: AgentId, text: String) {
        concrete(host)
            .event_tx()
            .send(SessionEvent::Completed { agent_id, text })
            .await
            .expect("session event loop remains open");
    }

    pub fn suspended_agent_ids(state_path: &Path) -> Vec<AgentId> {
        crate::suspend::load_suspended(state_path)
            .expect("suspended state loads")
            .agents
            .iter()
            .map(crate::suspend::SuspendedAgent::agent_id)
            .collect()
    }

    pub fn sweep_artifacts(
        host: &dyn LocalAgentHost,
    ) -> Result<Vec<model::ArtifactId>, ProtocolError> {
        concrete(host).sweep_artifacts_for_test()
    }

    /// Daemon-owned rings and summarizers used by the release memory soak.
    /// The wrapper keeps private runtime machinery out of the test harness
    /// while ensuring the qualification allocates the daemon-owned structured
    /// ring, raw PTY replay, and summarizer state of a live Claude PTY agent.
    #[doc(hidden)]
    pub struct DaemonMemoryHarness {
        sources: Vec<crate::agents::StructuredLogSource>,
        ptys: Vec<crate::agents::PtyHandle>,
        summarizers: Vec<crate::agents::SummarizerHandle>,
        publications: tokio::sync::mpsc::UnboundedReceiver<crate::agents::SummarizerPublication>,
        publisher: tokio::sync::mpsc::UnboundedSender<crate::agents::SummarizerPublication>,
        active_from: usize,
        sequence: u64,
    }

    const PERFORMANCE_RING_BYTES: usize = 1024 * 1024;
    const PERFORMANCE_RING_ROW_BYTES: usize = 64 * 1024;
    const PERFORMANCE_WRITE_BATCH_ROWS: u64 = 200;

    impl Default for DaemonMemoryHarness {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DaemonMemoryHarness {
        pub fn new() -> Self {
            let (publisher, publications) = tokio::sync::mpsc::unbounded_channel();
            Self {
                sources: Vec::new(),
                ptys: Vec::new(),
                summarizers: Vec::new(),
                publications,
                publisher,
                active_from: 0,
                sequence: 0,
            }
        }

        pub async fn add_idle(&mut self, count: usize) {
            self.add_idle_agents(count).await;
            let first = self.active_from - count;
            self.fill_rings(first..self.active_from).await;

            // The memory sample describes agents after their last input, with
            // the same periodic fold and health work running as the daemon.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            self.drain_publications();
        }

        /// Start idle summarizer tasks without populating their rings. This is
        /// the production machinery used by the process-CPU qualification.
        pub async fn add_idle_summarizers(&mut self, count: usize) {
            self.add_idle_agents(count).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            self.drain_publications();
        }

        async fn add_idle_agents(&mut self, count: usize) {
            let first = self.sources.len();
            for offset in 0..count {
                self.add_agent(first + offset).await;
            }
            self.active_from = self.sources.len();
        }

        pub async fn add_active(&mut self, count: usize) {
            let first = self.sources.len();
            for offset in 0..count {
                self.add_agent(first + offset).await;
            }
        }

        async fn add_agent(&mut self, index: usize) {
            let source = crate::agents::StructuredLogSource::with_policy(
                crate::agents::RingPolicy::claude_pty(),
            );
            let pty = crate::agents::PtyHandle::test_echo();
            let handle = crate::agents::SummarizerHandle::attach(
                uuid::Uuid::from_u128(0xDAE0_0000 + index as u128),
                model::StructuredProtocol::ClaudePtyTranscript,
                source.clone(),
                self.publisher.clone(),
            )
            .await
            .expect("an open daemon performance source accepts a summarizer");
            handle.activate();
            self.sources.push(source);
            self.ptys.push(pty);
            self.summarizers.push(handle);
        }

        async fn fill_rings(&mut self, range: std::ops::Range<usize>) {
            let target_rows = PERFORMANCE_RING_BYTES / PERFORMANCE_RING_ROW_BYTES;
            assert_eq!(
                target_rows * PERFORMANCE_RING_ROW_BYTES,
                PERFORMANCE_RING_BYTES,
                "performance rows exactly tile the structured ring"
            );
            for index in range.clone() {
                for row in 0..target_rows {
                    self.sources[index]
                        .write(performance_row(index, row, PERFORMANCE_RING_ROW_BYTES))
                        .await;
                }
            }
            self.wait_until_folded(range.clone(), target_rows as u64)
                .await;
            for index in range {
                let debug = self.sources[index].debug_snapshot().await;
                assert_eq!(
                    debug.buffer.bytes, PERFORMANCE_RING_BYTES,
                    "idle performance ring {index} was not filled to its byte budget"
                );
            }
        }

        /// Write rows through every active daemon ring and wait until their
        /// live summarizers have consumed the measured cut.
        pub async fn consume_active_rows(&mut self, rows_per_agent: u64) -> u64 {
            assert!(self.active_from < self.sources.len(), "no active agents");
            let active_count = self.sources.len() - self.active_from;
            let initial_through = self.summarizers[self.active_from].snapshot().through;
            assert!(
                self.summarizers[self.active_from..]
                    .iter()
                    .all(|summarizer| summarizer.snapshot().through == initial_through),
                "active summarizers begin at one cut"
            );
            let mut first = 1;
            while first <= rows_per_agent {
                let through = (first + PERFORMANCE_WRITE_BATCH_ROWS - 1).min(rows_per_agent);
                for sequence in first..=through {
                    let source_sequence = initial_through + sequence;
                    for (offset, source) in self.sources[self.active_from..].iter().enumerate() {
                        source
                            .write(serde_json::json!({
                                "type": "user",
                                "uuid": uuid::Uuid::from_u128(
                                    ((offset as u128) << 64) | source_sequence as u128
                                ),
                                "message": {"content": format!("active {source_sequence}")},
                            }))
                            .await;
                    }
                }
                self.wait_until_folded(
                    self.active_from..self.sources.len(),
                    initial_through + through,
                )
                .await;
                self.drain_publications();
                first = through + 1;
            }
            u64::try_from(active_count)
                .expect("active agent count fits u64")
                .checked_mul(rows_per_agent)
                .expect("performance row count fits u64")
        }

        async fn wait_until_folded(&mut self, range: std::ops::Range<usize>, through: u64) {
            tokio::time::timeout(std::time::Duration::from_secs(60), async {
                loop {
                    if range
                        .clone()
                        .all(|index| self.summarizers[index].snapshot().through >= through)
                    {
                        return;
                    }
                    self.drain_publications();
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("summarizers reached the performance cut");
        }

        pub async fn pulse(&mut self, iteration: u64) {
            if iteration == 0 {
                self.seed_edge_cases().await;
            }
            if iteration == 200
                && let Some(source) = self.sources.get(self.active_from)
            {
                source
                    .semantic_reset(serde_json::json!({
                        "type": "amux.transcript_ready",
                        "reset": true,
                        "reason": "daemon-memory-reset",
                    }))
                    .await;
            }
            for (offset, source) in self.sources[self.active_from..].iter().enumerate() {
                // Fill the provider's structured byte ring during the
                // two-minute warm-up, then measure the bounded steady state.
                for _ in 0..4 {
                    self.sequence = self.sequence.saturating_add(1);
                    source
                        .write(serde_json::json!({
                            "type": "user",
                            "uuid": format!("{iteration:012x}-{offset:04x}-4000-8000-{:012x}", self.sequence),
                            "message": {"content": format!("daemon corpus row {}", self.sequence)},
                        }))
                        .await;
                }
                let pty = &self.ptys[self.active_from + offset];
                let mut repaint =
                    format!("\x1b[2J\x1b[H\x1b[38;5;42mdaemon {offset:02} pulse {iteration:08}")
                        .into_bytes();
                repaint.resize(1020, b' ');
                repaint.extend_from_slice(b"\x1b[0m");
                pty.send_input(repaint)
                    .await
                    .expect("daemon memory PTY echo remains open");
            }
            self.drain_publications();
        }

        async fn seed_edge_cases(&self) {
            let active = &self.sources[self.active_from..];
            if let Some(source) = active.first() {
                source
                    .write(serde_json::json!({
                        "type": "assistant",
                        "uuid": "daemon-oversized",
                        "message": {"content": "x".repeat(5 * 1024 * 1024)},
                    }))
                    .await;
            }
            for ask in 0..100 {
                let source = &active[ask % active.len()];
                source
                    .write(serde_json::json!({
                        "type": "hook.permission_request",
                        "session_id": "daemon-memory",
                        "tool_name": "Write",
                        "tool_input": {"file_path": format!("/tmp/daemon-{ask}")},
                        "permission_suggestions": [],
                    }))
                    .await;
            }
        }

        fn drain_publications(&mut self) {
            while let Ok(mut publication) = self.publications.try_recv() {
                if let Some(acknowledged) = publication.acknowledged.take() {
                    let _ = acknowledged.send(());
                }
            }
        }
    }

    fn performance_row(agent: usize, row: usize, encoded_bytes: usize) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "type": "user",
            "uuid": uuid::Uuid::from_u128(((agent as u128) << 64) | row as u128),
            "message": {"content": ""},
        });
        let base = serde_json::to_vec(&payload)
            .expect("performance row serializes")
            .len();
        assert!(
            base <= encoded_bytes,
            "performance row target is large enough"
        );
        payload["message"]["content"] = serde_json::Value::String("x".repeat(encoded_bytes - base));
        assert_eq!(
            serde_json::to_vec(&payload)
                .expect("filled performance row serializes")
                .len(),
            encoded_bytes
        );
        payload
    }

    #[cfg(test)]
    mod performance_harness_tests {
        use super::*;

        #[test]
        fn daemon_memory_row_has_exact_encoded_size() {
            let row = performance_row(7, 11, PERFORMANCE_RING_ROW_BYTES);
            assert_eq!(
                serde_json::to_vec(&row).unwrap().len(),
                PERFORMANCE_RING_ROW_BYTES
            );
        }

        #[tokio::test]
        async fn daemon_memory_idle_agent_holds_a_full_ring_and_live_tip() {
            let mut harness = DaemonMemoryHarness::new();
            harness.add_idle(1).await;

            let debug = harness.sources[0].debug_snapshot().await;
            assert_eq!(debug.buffer.bytes, PERFORMANCE_RING_BYTES);
            assert_eq!(harness.summarizers[0].snapshot().through, 16);
            assert_eq!(
                harness.summarizers[0].snapshot().summary.phase,
                model::AgentPhase::Running
            );
        }

        #[tokio::test]
        async fn daemon_memory_active_rows_reach_every_summarizer() {
            let mut harness = DaemonMemoryHarness::new();
            harness.add_active(2).await;

            assert_eq!(harness.consume_active_rows(257).await, 514);
            assert!(
                harness
                    .summarizers
                    .iter()
                    .all(|summarizer| summarizer.snapshot().through == 257)
            );
        }
    }
}

pub use model::{
    Agent, AgentEvent, AgentKind, AgentType, ArtifactRef, CreateAgentRequest, Protocol,
};

#[cfg(test)]
#[derive(Clone)]
struct Config {
    path: Option<std::path::PathBuf>,
    socket_path: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

#[cfg(test)]
impl Default for Config {
    fn default() -> Self {
        let data_dir = std::env::temp_dir().join("amux-agent-runtime-test");
        std::fs::create_dir_all(&data_dir).expect("create agent runtime test data directory");
        Self {
            path: None,
            socket_path: data_dir.join("amux.sock"),
            data_dir,
        }
    }
}

#[cfg(test)]
fn keymap_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("keymaps")
}

#[cfg(test)]
mod config {
    pub(crate) use super::Config;
}
