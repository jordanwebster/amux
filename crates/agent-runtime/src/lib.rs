#![allow(clippy::result_large_err)]

pub mod agent_tools;
mod agents;
mod debug;
mod events;
mod host;
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
    }

    impl Factory {
        pub fn new(clock: Arc<dyn artifacts::Clock>) -> Self {
            Self { clock }
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
            AgentRuntime::new_with_artifact_clock(
                route,
                config.claude_user_keymap_dir,
                config.data_dir,
                Some(config.state_path),
                self.clock.clone(),
            )
            .map(|host| host as Arc<dyn LocalAgentHost>)
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
