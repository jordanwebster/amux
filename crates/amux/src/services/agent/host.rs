//! The local agent runtime behind the [`LocalAgentHost`] seam.
//!
//! [`PtyAgentHost`] owns the live session registry ([`AgentServiceState`]),
//! the session-event loop, and the host's identity, and implements every
//! core→runtime call as a [`LocalAgentHost`] method. The rest of the core
//! holds an `Option<Arc<dyn LocalAgentHost>>` and never names these types.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::lifecycle::{
    CreateAgentError, RenameAgentError, clear_working_on, commit_server_suspend,
    create_agent_record, delete_local_agent, parent_envelope, prepare_server_suspend,
    rename_local_agent_record, resume_agents, shutdown_server, spawn_session_event_loop,
    withdraw_agent,
};
use super::{
    AgentServiceState, DebugAgent, LocalAgentHost, ResponseStream, SharedAgentServiceState,
    session_rpc,
};
#[cfg(feature = "testnet")]
use crate::agents::claude::ClaudeSession;
use crate::agents::{
    Agent, AgentDeps, AgentEvent, AgentSession, AgentType, CreateAgentConfig, CreateAgentRequest,
    CreateAgentRpcRequest, DeliveryError, ExternalHookBootstrap, HookEnvironment, HookOutcome,
    McpLaunchRoute, RenameAgentRequest, SendInputRequest, SessionCloseReason, SessionEvent,
    SetAgentStatusRequest, SpawnInheritance, StopPolicy, SubscribeSessionRequest,
    bootstrap_external_hook,
};
use crate::envelope::{Envelope, EnvelopeKind};
use crate::protocol::{ProtocolError, wire};
use crate::server::ShutdownReason;
use crate::suspend;

/// The concrete PTY-backed agent runtime.
pub(crate) struct PtyAgentHost {
    state: SharedAgentServiceState,
    event_tx: mpsc::Sender<SessionEvent>,
    host_id: Uuid,
}

impl PtyAgentHost {
    /// Build a host against the default configured socket path.
    #[cfg(any(test, feature = "testnet"))]
    pub(crate) fn new(host_id: Uuid) -> Arc<Self> {
        let config = crate::config::Config::default();
        let route = McpLaunchRoute::for_current_process(&config, host_id)
            .expect("default managed MCP route should be usable");
        Self::new_with_mcp_launch_route(route)
            .expect("default Codex private socket path should be usable")
    }

    /// Build the host and spawn its session-event loop. Cloud-vs-device is
    /// decided by runtime guards in `AgentServiceCtx`, not by host presence.
    /// The private Codex fallback socket lives beside the configured amux
    /// socket; its short filename preserves as much `SUN_LEN` headroom as
    /// possible.
    pub(crate) fn new_with_mcp_launch_route(route: McpLaunchRoute) -> io::Result<Arc<Self>> {
        let server_socket_path = route.socket_path().to_path_buf();
        let runtime_dir = server_socket_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let host_id = route.host_id();
        let deps = AgentDeps::new(
            runtime_dir,
            codex_private_socket_path(&server_socket_path)?,
            route,
        );
        let state = Arc::new(RwLock::new(AgentServiceState::new(deps)));
        let (event_tx, event_rx) = mpsc::channel(256);
        spawn_session_event_loop(state.clone(), event_rx, host_id);
        Ok(Arc::new(Self {
            state,
            event_tx,
            host_id,
        }))
    }

    pub(crate) fn state(&self) -> &SharedAgentServiceState {
        &self.state
    }

    pub(crate) fn event_tx(&self) -> &mpsc::Sender<SessionEvent> {
        &self.event_tx
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    #[cfg(feature = "testnet")]
    pub(crate) async fn register_scripted_claude(
        &self,
        request: CreateAgentRequest,
    ) -> Result<Agent, ProtocolError> {
        let agent_id = request.agent_id;
        let mut state = self.state.write().await;
        let session: AgentSession = Box::new(ClaudeSession::scripted_for_testnet(
            &request,
            state.deps.runtime_dir.clone(),
            state.deps.claude_version_cache.clone(),
            state.deps.mcp_launch_route.clone(),
        ));
        let agent = session.to_agent(self.host_id).into();
        let announce = state
            .register_local_agent_context(self.host_id, agent_id, session)
            .map_err(|message| ProtocolError::ServerError { message })?;
        state.local_agent_events.emit(announce);
        Ok(agent)
    }

    #[cfg(feature = "testnet")]
    pub(crate) async fn end_scripted_session(&self, agent_id: Uuid) {
        self.event_tx
            .send(SessionEvent::Ended { agent_id })
            .await
            .expect("scripted session event loop should be running");
    }

    #[cfg(feature = "testnet")]
    pub(crate) async fn deliver_scripted_hook(
        &self,
        agent_id: Uuid,
        payload: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        <Self as LocalAgentHost>::handle_hook(
            self,
            agent_id,
            payload,
            HookEnvironment::new(),
            false,
        )
        .await
    }
}

fn codex_private_socket_path(server_socket_path: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let fallback_dir = PathBuf::from(format!("/tmp/amux-{uid}"));
        codex_private_socket_path_with_fallback(server_socket_path, &fallback_dir)
    }
    #[cfg(not(unix))]
    {
        let socket_dir = server_socket_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        Ok(socket_dir.join("cx.sock"))
    }
}

#[cfg(unix)]
fn codex_private_socket_path_with_fallback(
    server_socket_path: &Path,
    fallback_dir: &Path,
) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    const MAX_CODEX_SOCKET_PATH_BYTES: usize = 103;

    let socket_dir = server_socket_path
        .parent()
        .unwrap_or_else(|| Path::new("."));

    // Stable FNV-1a keeps servers with different configured socket paths
    // isolated without copying a potentially long filename into `sun_path`.
    let hash = server_socket_path
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    let file_name = format!("c{hash:016x}.sock");
    let adjacent = socket_dir.join(&file_name);
    if adjacent.as_os_str().as_bytes().len() <= MAX_CODEX_SOCKET_PATH_BYTES {
        Ok(adjacent)
    } else {
        // Move only the Codex runtime socket when the configured amux
        // directory leaves too little room for codex-sdk's sun_path cap.
        secure_codex_fallback_directory(fallback_dir)?;
        Ok(fallback_dir.join(file_name))
    }
}

#[cfg(unix)]
fn secure_codex_fallback_directory(path: &Path) -> io::Result<()> {
    use std::fs::{DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "failed to create secure Codex fallback directory {}: {error}",
                    path.display()
                ),
            ));
        }
    }

    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to inspect Codex fallback directory {}: {error}",
                path.display()
            ),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Codex fallback directory {} must not be a symlink",
                path.display()
            ),
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Codex fallback directory {} is not a directory",
                path.display()
            ),
        ));
    }

    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Codex fallback directory {} is owned by uid {}, expected effective uid {effective_uid}",
                path.display(),
                metadata.uid()
            ),
        ));
    }

    if metadata.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(path, Permissions::from_mode(0o700)).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to secure Codex fallback directory {} with mode 0700: {error}",
                    path.display()
                ),
            )
        })?;
    }

    Ok(())
}

#[async_trait]
impl LocalAgentHost for PtyAgentHost {
    async fn create(&self, request: CreateAgentRpcRequest) -> Result<Agent, ProtocolError> {
        let req = create_rpc_to_domain_request(request.agent_id, request)?;
        if matches!(req.agent_type, AgentType::Codex { .. }) {
            #[cfg(unix)]
            {
                let client = self.state().read().await.deps.codex_client.clone();
                client.ensure_authenticated().await.map_err(|error| {
                    ProtocolError::FailedPrecondition {
                        message: error.to_string(),
                    }
                })?;
            }
            #[cfg(not(unix))]
            return Err(ProtocolError::FailedPrecondition {
                message: "Codex agents are supported only on Unix platforms".to_string(),
            });
        }
        create_agent_record(self.state(), self.event_tx(), req, self.host_id())
            .await
            .map(Into::into)
            .map_err(create_error_to_protocol)
    }

    async fn spawn_inheritance(&self, agent_id: Uuid) -> Result<SpawnInheritance, ProtocolError> {
        self.state()
            .read()
            .await
            .local_agents
            .get(&agent_id)
            .map(|context| context.session.spawn_inheritance())
            .ok_or(ProtocolError::NoAgentFound)
    }

    async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError> {
        if request.name.is_empty() {
            return Err(ProtocolError::InvalidArgument {
                message: "RenameAgentRequest.name must not be empty".to_string(),
            });
        }
        let host_id = self.host_id();
        let mut us = self.state().write().await;
        rename_local_agent_record(&mut us, host_id, &request)
            .map(Into::into)
            .map_err(rename_error_to_protocol)
    }

    async fn delete(&self, agent_id: Uuid) -> Result<(), ProtocolError> {
        let session_to_stop = {
            let mut us = self.state().write().await;
            delete_local_agent_and_emit_session_close(&mut us, agent_id)
        };

        match session_to_stop {
            Some(session) => {
                session.stop(StopPolicy::Interrupt).await;
                Ok(())
            }
            None => Err(ProtocolError::NoAgentFound),
        }
    }

    async fn send_message(&self, envelope: Envelope) -> Result<(), ProtocolError> {
        let delivery_target = {
            let state = self.state().read().await;
            state
                .local_agents
                .get(&envelope.to.agent_id)
                .map(|context| context.session.delivery_target())
                .ok_or(ProtocolError::NoAgentFound)?
        };
        deliver_message(delivery_target, &envelope).await
    }

    async fn send_message_waiting(
        &self,
        envelope: Envelope,
        timeout: std::time::Duration,
    ) -> Result<(), ProtocolError> {
        let delivery_target = {
            let state = self.state().read().await;
            state
                .local_agents
                .get(&envelope.to.agent_id)
                .map(|context| context.session.delivery_target())
                .ok_or(ProtocolError::NoAgentFound)?
        };
        delivery_target
            .wait_until_live(timeout)
            .await
            .map_err(delivery_error_to_protocol)?;
        deliver_message(delivery_target, &envelope).await
    }

    async fn set_agent_status(&self, request: SetAgentStatusRequest) -> Result<(), ProtocolError> {
        let mut state = self.state().write().await;
        let context = state
            .local_agents
            .get_mut(&request.agent_id)
            .ok_or(ProtocolError::NoAgentFound)?;
        context.working_on = request.working_on.map(|text| crate::agents::WorkingOn {
            text,
            updated_at: chrono::Utc::now(),
        });
        let updated = context.record(self.host_id());
        state.local_agent_events.emit(updated.agent_updated_event());
        Ok(())
    }

    async fn send_input(&self, request: SendInputRequest) -> Result<(), ProtocolError> {
        session_rpc::send_session_input(self, request).await
    }

    async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<ResponseStream<wire::SubscribeSessionResponse>, ProtocolError> {
        session_rpc::subscribe_session_stream(self, request).await
    }

    async fn agent_events_snapshot(&self) -> (Vec<AgentEvent>, mpsc::Receiver<AgentEvent>) {
        let mut state = self.state().write().await;
        let mut snapshot: Vec<_> = state
            .local_agents
            .values()
            .map(|context| context.record(self.host_id()).agent_event())
            .collect();
        snapshot.sort_unstable_by_key(agent_event_sort_key);
        let rx = state.local_agent_events.subscribe_drop_on_overflow();
        (snapshot, rx)
    }

    async fn subscribe_agent_events(&self) -> mpsc::Receiver<AgentEvent> {
        self.state().write().await.local_agent_events.subscribe()
    }

    async fn subscribe_outbound_envelopes(&self) -> mpsc::Receiver<Envelope> {
        self.state().write().await.outbound_envelopes.subscribe()
    }

    async fn handle_hook(
        &self,
        agent_id: Uuid,
        payload: Vec<u8>,
        env: HookEnvironment,
        external: bool,
    ) -> Result<(), ProtocolError> {
        tracing::debug!(%agent_id, external, "received Claude hook event");

        let mut session_to_stop = None;
        let result = {
            let mut state = self.state().write().await;
            if let Some(session) = state.agent_session_mut(&agent_id) {
                match session.handle_hook_payload(&payload, &env).await {
                    Ok(HookOutcome::Noop | HookOutcome::KeepSession) => Ok(()),
                    Ok(HookOutcome::Completed { text }) => {
                        let envelope =
                            parent_envelope(session, self.host_id, EnvelopeKind::Completed, text);
                        clear_working_on(&mut state, self.host_id, agent_id);
                        if let Some(envelope) = envelope {
                            state.outbound_envelopes.emit(envelope);
                        }
                        Ok(())
                    }
                    Ok(HookOutcome::WithdrawSession) => {
                        session_to_stop = withdraw_agent(&mut state, agent_id);
                        Ok(())
                    }
                    Err(error) => Err(error.into_protocol_error()),
                }
            } else if !external {
                tracing::warn!(%agent_id, "hook target not found");
                Err(ProtocolError::NoAgentFound)
            } else {
                match bootstrap_external_hook(agent_id, &payload, &env).await {
                    Ok(ExternalHookBootstrap::Noop) => Ok(()),
                    Ok(ExternalHookBootstrap::Register(session)) => {
                        match state.insert_registered_local_agent(self.host_id(), agent_id, session)
                        {
                            Ok(announce) => {
                                if let Some(session) = state.agent_session_mut(&agent_id) {
                                    session.maybe_start_name_sniffer(self.event_tx());
                                }
                                state.local_agent_events.emit(announce);
                                tracing::info!(%agent_id, "created readonly session from external hook");
                                Ok(())
                            }
                            Err(e) => Err(ProtocolError::ServerError {
                                message: format!(
                                    "failed to register readonly agent {agent_id}: {e}"
                                ),
                            }),
                        }
                    }
                    Err(error) => Err(error.into_protocol_error()),
                }
            }
        };

        if let Some(session) = session_to_stop {
            session.stop(StopPolicy::Interrupt).await;
        }

        result
    }

    async fn resume(&self, state_path: PathBuf) -> Result<(u64, u64), ProtocolError> {
        let suspended =
            suspend::load_suspended(&state_path).map_err(|error| ProtocolError::ServerError {
                message: format!("failed to load state: {error}"),
            })?;
        let result = resume_agents(
            self.state(),
            self.event_tx(),
            suspended.agents,
            self.host_id(),
        )
        .await;
        if result.failed_agents.is_empty() {
            suspend::remove_suspended(&state_path).map_err(|error| ProtocolError::ServerError {
                message: format!("failed to remove state: {error}"),
            })?;
        } else {
            suspend::save_suspended(
                &state_path,
                &suspend::SuspendedServerState {
                    agents: result.failed_agents,
                },
            )
            .map_err(|error| ProtocolError::ServerError {
                message: format!("failed to save remaining state: {error}"),
            })?;
        }
        Ok((result.resumed_count as u64, result.failed_count as u64))
    }

    async fn stop_all(&self) {
        shutdown_server(self.state()).await;
    }

    async fn prepare_suspend(&self, state_path: PathBuf) -> Result<u64, ProtocolError> {
        let (suspended, errors) = prepare_server_suspend(self.state()).await;
        if !errors.is_empty() {
            return Err(ProtocolError::ServerError {
                message: errors.join("; "),
            });
        }
        let count = suspended.agents.len() as u64;
        if !suspended.agents.is_empty() {
            suspend::save_suspended(&state_path, &suspended).map_err(|error| {
                ProtocolError::ServerError {
                    message: format!("failed to save state: {error}"),
                }
            })?;
        }
        Ok(count)
    }

    async fn commit_suspend(&self) {
        commit_server_suspend(self.state()).await;
    }

    async fn notify_shutdown(&self, reason: ShutdownReason) {
        self.state()
            .write()
            .await
            .local_shutdown_events
            .emit(reason);
    }

    async fn agent_count(&self) -> usize {
        self.state().read().await.local_agent_count()
    }

    async fn debug_dump(&self, verbose: bool) -> Vec<DebugAgent> {
        let host_id = self.host_id();
        let state = self.state().read().await;
        let mut agents: Vec<DebugAgent> = state
            .local_agents
            .values()
            .map(|context| DebugAgent {
                record: context.record(host_id),
                session: verbose
                    .then(|| context.session.debug_json(verbose).ok())
                    .flatten(),
            })
            .collect();
        agents.sort_unstable_by(|a, b| {
            a.record
                .name
                .as_deref()
                .unwrap_or("")
                .cmp(b.record.name.as_deref().unwrap_or(""))
                .then_with(|| a.record.id.as_u128().cmp(&b.record.id.as_u128()))
        });
        agents
    }
}

async fn deliver_message(
    delivery_target: Box<dyn crate::agents::AgentDeliveryTarget>,
    envelope: &Envelope,
) -> Result<(), ProtocolError> {
    match delivery_target.deliver(envelope).await {
        Ok(delivery) => {
            tracing::info!(
                envelope_id = %envelope.id,
                recipient_agent_id = %envelope.to.agent_id,
                carrier = delivery.carrier(),
                "agent message delivered"
            );
            Ok(())
        }
        Err(error) => {
            tracing::info!(
                envelope_id = %envelope.id,
                recipient_agent_id = %envelope.to.agent_id,
                carrier = "none",
                error = %error,
                "agent message delivery failed"
            );
            Err(delivery_error_to_protocol(error))
        }
    }
}

fn delivery_error_to_protocol(error: DeliveryError) -> ProtocolError {
    match error {
        DeliveryError::UnsupportedAgentType(agent_type) => ProtocolError::Unimplemented {
            message: format!("{agent_type} agent message delivery is not implemented"),
        },
        DeliveryError::FailedPrecondition(message) => ProtocolError::FailedPrecondition { message },
        DeliveryError::Failed(message) => ProtocolError::ServerError { message },
    }
}

fn delete_local_agent_and_emit_session_close(
    us: &mut AgentServiceState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    let session = delete_local_agent(us, agent_id);
    if session.is_some() {
        us.local_session_close_events
            .emit((agent_id, SessionCloseReason::AgentDeleted));
    }
    session
}

fn create_error_to_protocol(error: CreateAgentError) -> ProtocolError {
    match error {
        err @ CreateAgentError::LimitReached { .. } => ProtocolError::ResourceExhausted {
            message: err.to_string(),
        },
        err @ CreateAgentError::AlreadyExists(_) => ProtocolError::AlreadyExists {
            message: err.to_string(),
        },
        err @ (CreateAgentError::Start(_) | CreateAgentError::Register(_)) => {
            ProtocolError::ServerError {
                message: err.to_string(),
            }
        }
    }
}

fn rename_error_to_protocol(error: RenameAgentError) -> ProtocolError {
    match error {
        RenameAgentError::NotFound(_) => ProtocolError::NoAgentFound,
        err @ RenameAgentError::AlreadyExists(_) => ProtocolError::AlreadyExists {
            message: err.to_string(),
        },
        err @ RenameAgentError::Update(_) => ProtocolError::ServerError {
            message: err.to_string(),
        },
    }
}

fn create_rpc_to_domain_request(
    agent_id: Uuid,
    request: CreateAgentRpcRequest,
) -> Result<CreateAgentRequest, ProtocolError> {
    let parent = request.parent;
    let initial_prompt = request.initial_prompt;
    match request.agent {
        CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::Claude {
                driver: crate::agents::ClaudeDriver::Pty,
            },
            working_dir,
            terminal_size,
            args,
            parent,
            initial_prompt,
        }),
        CreateAgentConfig::Codex {
            cwd,
            model,
            approval_policy,
            sandbox_policy,
            resume_thread_id,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::Codex {
                model,
                approval_policy,
                sandbox_policy,
                resume_thread_id,
            },
            working_dir: cwd,
            terminal_size: None,
            args: Vec::new(),
            parent,
            initial_prompt,
        }),
        #[cfg(any(debug_assertions, test))]
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::TestAgent { command },
            working_dir,
            terminal_size,
            args: Vec::new(),
            parent,
            initial_prompt,
        }),
        #[cfg(not(any(debug_assertions, test)))]
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => {
            let _ = (command, working_dir, terminal_size);
            Err(ProtocolError::Unimplemented {
                message: "test-agent creation over protobuf is not available in release builds"
                    .to_string(),
            })
        }
    }
}

fn agent_event_sort_key(event: &AgentEvent) -> (String, u128) {
    match event {
        AgentEvent::AgentUp { agent } => {
            (agent.name.clone().unwrap_or_default(), agent.id.as_u128())
        }
        AgentEvent::AgentUpdated { agent } => {
            (agent.name.clone().unwrap_or_default(), agent.id.as_u128())
        }
        AgentEvent::AgentDown { agent_id } => (String::new(), agent_id.as_u128()),
        AgentEvent::SnapshotComplete => (String::new(), 0),
    }
}

#[cfg(all(test, unix))]
mod socket_tests {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn managed_host_propagates_the_exact_route_into_agent_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("amux");
        let config = temp.path().join("amux.yaml");
        let socket = temp.path().join("custom.sock");
        std::fs::write(&executable, b"test executable").unwrap();
        std::fs::write(&config, b"host_name: test\n").unwrap();
        let route =
            McpLaunchRoute::new(executable, Some(config), socket, Uuid::from_u128(80)).unwrap();

        let host = PtyAgentHost::new_with_mcp_launch_route(route.clone()).unwrap();

        assert_eq!(host.host_id(), route.host_id());
        assert_eq!(host.state().read().await.deps.mcp_launch_route, route);
    }

    #[test]
    fn private_codex_socket_follows_configured_server_socket_dir() {
        let first =
            codex_private_socket_path(Path::new("/var/run/custom-amux/control.sock")).unwrap();
        let second =
            codex_private_socket_path(Path::new("/var/run/custom-amux/other.sock")).unwrap();

        assert_eq!(first.parent(), Some(Path::new("/var/run/custom-amux")));
        assert_eq!(second.parent(), first.parent());
        assert_ne!(first, second);
        assert!(first.file_name().unwrap().len() <= 22);
    }

    #[test]
    fn private_codex_socket_uses_short_fallback_dir_for_long_configured_dir() {
        let temp = tempfile::tempdir().unwrap();
        let fallback_dir = temp.path().join("codex-fallback");
        let long_dir = std::path::Path::new("/tmp").join("x".repeat(110));
        let server_socket = long_dir.join("control.sock");
        let socket =
            codex_private_socket_path_with_fallback(&server_socket, &fallback_dir).unwrap();

        assert_eq!(socket.parent(), Some(fallback_dir.as_path()));
        assert!(socket.as_os_str().as_bytes().len() <= 103);

        let hash = server_socket
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        assert_eq!(
            socket.file_name(),
            Some(std::ffi::OsStr::new(&format!("c{hash:016x}.sock")))
        );
    }

    #[test]
    fn secure_fallback_directory_creates_private_directory() {
        let temp = tempfile::tempdir().unwrap();
        let fallback_dir = temp.path().join("fresh");

        secure_codex_fallback_directory(&fallback_dir).unwrap();

        let metadata = std::fs::symlink_metadata(&fallback_dir).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn secure_fallback_directory_repairs_lax_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let fallback_dir = temp.path().join("lax");
        std::fs::create_dir(&fallback_dir).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        secure_codex_fallback_directory(&fallback_dir).unwrap();

        let metadata = std::fs::symlink_metadata(&fallback_dir).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn secure_fallback_directory_rejects_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let fallback_dir = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &fallback_dir).unwrap();

        let error = secure_codex_fallback_directory(&fallback_dir).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error
                .to_string()
                .contains(&fallback_dir.display().to_string())
        );
        assert!(error.to_string().contains("must not be a symlink"));
    }
}
