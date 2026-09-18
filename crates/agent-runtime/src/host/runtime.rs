//! The local agent runtime behind the [`LocalAgentHost`] seam.
//!
//! [`AgentRuntime`] owns the live session registry ([`AgentServiceState`]),
//! the session-event loop, and the host's identity, and implements every
//! core→runtime call as a [`LocalAgentHost`] method. The rest of the core
//! holds an `Option<Arc<dyn LocalAgentHost>>` and never names these types.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use host_api::{
    ArtifactBlob, HostConfig, HostDebugAgent, HostResourceInventory, HostResumeBatch,
    HostResumeResult, HostResumeStatus, HostSessionStream, HostSetAgentStatus, LocalAgentHost,
    LocalAgentHostFactory, OperationBarrier, PreparedHostState, SessionInputRequest,
    SessionRequest,
};
use model::envelope::Envelope;
use model::{ProtocolError, ShutdownReason};
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::lifecycle::{
    CreateAgentError, RenameAgentError, abort_server_suspend, attach_summarizer,
    commit_server_suspend, create_agent_record, delete_local_agent, prepare_server_suspend,
    rename_local_agent_record, resume_agents, shutdown_server, spawn_session_event_loop,
    spawn_summarizer_publication_loop, withdraw_agent,
};
use super::{AgentServiceState, SharedAgentServiceState, session};
use crate::agents::claude::ClaudeSession;
use crate::agents::{
    Agent, AgentDeps, AgentEvent, AgentSession, AgentType, ArtifactOwners, CreateAgentRequest,
    DeliveryError, ExternalHookBootstrap, HookEnvironment, HookError, HookOutcome, McpLaunchRoute,
    RenameAgentRequest, SessionCloseReason, SessionEvent, SpawnInheritance, StopPolicy,
    bootstrap_external_hook, compute_diff, store_error,
};
use crate::suspend;

/// The concrete PTY-backed agent runtime.
pub struct AgentRuntime {
    state: SharedAgentServiceState,
    event_tx: mpsc::Sender<SessionEvent>,
    host_id: Uuid,
    repository_roots: Vec<PathBuf>,
    state_path: PathBuf,
    resume_lock: tokio::sync::Mutex<()>,
    resume_attempts: std::sync::Mutex<HashMap<Uuid, Uuid>>,
    artifact_owners: Arc<ArtifactOwners>,
    artifact_sweeper: tokio::task::JoinHandle<()>,
    pub(crate) test_cleanup: Option<PathBuf>,
}

/// Creates one isolated provider runtime for each desktop profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentRuntimeFactory;

impl AgentRuntime {
    /// Build a host against the default configured socket path.
    #[cfg(test)]
    pub(crate) fn new(host_id: Uuid) -> Arc<Self> {
        // Test agent registrations must never update the operator's recent projects.
        let data_dir = tempfile::tempdir()
            .expect("test host data directory")
            .keep();
        let config = crate::config::Config {
            data_dir: data_dir.clone(),
            ..Default::default()
        };
        let route = McpLaunchRoute::for_current_process(&config, host_id)
            .expect("default managed MCP route should be usable");
        let mut host = Self::new_with_mcp_launch_route(
            route,
            crate::keymap_dir(&config.data_dir),
            config.data_dir,
        )
        .expect("default agent host resources should be usable");
        Arc::get_mut(&mut host)
            .expect("new test host is unshared")
            .test_cleanup = Some(data_dir);
        host
    }

    /// Build the host and spawn its session-event loop. Cloud-vs-device is
    /// decided by runtime guards in `AgentServiceCtx`, not by host presence.
    /// The private Codex fallback socket lives beside the configured amux
    /// socket; its short filename preserves as much `SUN_LEN` headroom as
    /// possible.
    pub(crate) fn new_with_mcp_launch_route(
        route: McpLaunchRoute,
        claude_user_keymap_dir: PathBuf,
        data_dir: PathBuf,
    ) -> io::Result<Arc<Self>> {
        Self::new_with_artifact_clock(
            route,
            claude_user_keymap_dir,
            data_dir,
            None,
            Arc::new(artifacts::SystemClock),
            Vec::new(),
        )
    }

    pub(crate) fn new_with_artifact_clock(
        route: McpLaunchRoute,
        claude_user_keymap_dir: PathBuf,
        data_dir: PathBuf,
        state_path: Option<PathBuf>,
        artifact_clock: Arc<dyn artifacts::Clock>,
        repository_roots: Vec<PathBuf>,
    ) -> io::Result<Arc<Self>> {
        let server_socket_path = route.socket_path().to_path_buf();
        let runtime_dir = server_socket_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let host_id = route.host_id();
        let state_path = state_path.unwrap_or_else(|| data_dir.join("state.yaml"));
        let artifact_owners = Arc::new(
            ArtifactOwners::open(data_dir.clone(), artifact_clock).map_err(io::Error::other)?,
        );
        let deps = AgentDeps::new(
            data_dir,
            runtime_dir,
            codex_private_socket_path(&server_socket_path)?,
            route,
            claude_user_keymap_dir,
        )?;
        let (summarizer_tx, summarizer_rx) = mpsc::unbounded_channel();
        let mut service_state =
            AgentServiceState::new_with_revision_path(deps, &state_path, host_id)?;
        service_state.summarizer_publications = Some(summarizer_tx);
        let state = Arc::new(RwLock::new(service_state));
        let (event_tx, event_rx) = mpsc::channel(256);
        spawn_session_event_loop(state.clone(), event_rx, host_id);
        spawn_summarizer_publication_loop(state.clone(), summarizer_rx, host_id);
        let artifact_sweeper = crate::agents::spawn_artifact_sweeper(artifact_owners.clone());
        Ok(Arc::new(Self {
            state,
            event_tx,
            host_id,
            repository_roots,
            state_path,
            resume_lock: tokio::sync::Mutex::new(()),
            resume_attempts: std::sync::Mutex::new(HashMap::new()),
            artifact_owners,
            artifact_sweeper,
            test_cleanup: None,
        }))
    }

    pub(crate) fn state(&self) -> &SharedAgentServiceState {
        &self.state
    }

    pub(crate) fn event_tx(&self) -> &mpsc::Sender<SessionEvent> {
        &self.event_tx
    }

    pub fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn sweep_artifacts_for_test(&self) -> Result<Vec<model::ArtifactId>, ProtocolError> {
        self.artifact_owners.sweep_loaded(artifacts::EPHEMERAL_TTL)
    }

    #[cfg(unix)]
    #[allow(dead_code)] // Consumed only by opt-in recorded-provider harnesses.
    pub(crate) async fn register_sdk_fixture(
        &self,
        record: crate::agents::AgentRecord,
        provider: claude::sdk::Session,
    ) -> anyhow::Result<crate::agents::MultiplexStructuredReader> {
        let agent_id = record.id;
        let mut session: AgentSession = Box::new(
            crate::agents::claude::ClaudeSdkBackend::with_session(record, provider),
        );
        let crate::agents::Plane::Structured { log, .. } =
            session.plane(crate::agents::Protocol::ClaudeSdkV1)?
        else {
            anyhow::bail!("SDK fixture needs a structured plane");
        };
        let reader = log.subscribe().await.expect("fresh SDK log is open");
        let mut state = self.state.write().await;
        let summarizer = attach_summarizer(&state, agent_id, &session)
            .await
            .map_err(anyhow::Error::msg)?;
        let exit_handle = session.start(&self.event_tx)?;
        state
            .register_local_agent_context_with_summarizer(
                self.host_id,
                agent_id,
                session,
                None,
                summarizer,
            )
            .map_err(anyhow::Error::msg)?;
        if let Some(summarizer) = state
            .local_agents
            .get(&agent_id)
            .and_then(|context| context.summarizer.as_ref())
        {
            summarizer.activate();
        }
        super::lifecycle::monitor_session_exit(exit_handle, self.event_tx.clone(), agent_id);
        Ok(reader)
    }

    pub(crate) async fn register_scripted_claude(
        &self,
        request: CreateAgentRequest,
    ) -> Result<Agent, ProtocolError> {
        let agent_id = request.agent_id;
        let mut state = self.state.write().await;
        let mut session: AgentSession = Box::new(ClaudeSession::scripted_for_testnet(
            &request,
            state.deps.runtime_dir.clone(),
            state.deps.claude_version_cache.clone(),
            state.deps.mcp_launch_route.clone(),
            state.deps.claude_user_keymap_dir.clone(),
        ));
        let summarizer = attach_summarizer(&state, agent_id, &session)
            .await
            .map_err(|message| ProtocolError::ServerError { message })?;
        let exit_handle =
            session
                .start(&self.event_tx)
                .map_err(|error| ProtocolError::ServerError {
                    message: error.to_string(),
                })?;
        let announce = state
            .register_local_agent_context_with_summarizer(
                self.host_id,
                agent_id,
                session,
                None,
                summarizer,
            )
            .map_err(|message| ProtocolError::ServerError { message })?;
        if let Some(summarizer) = state
            .local_agents
            .get(&agent_id)
            .and_then(|context| context.summarizer.as_ref())
        {
            summarizer.activate();
        }
        let AgentEvent::AgentUp { agent } = &announce else {
            unreachable!("registration always announces AgentUp")
        };
        let agent = agent.clone();
        state.local_agent_events.emit(announce);
        super::lifecycle::monitor_session_exit(exit_handle, self.event_tx.clone(), agent_id);
        Ok(agent)
    }

    /// Register a Claude PTY agent whose session was supplied rather than
    /// launched: the same backend, hooks and folds as a real one.
    pub(crate) async fn register_claude_pty_session(
        &self,
        request: CreateAgentRequest,
        session: claude::pty::Session,
    ) -> Result<Agent, ProtocolError> {
        let protocol_error = |error: String| ProtocolError::ServerError { message: error };
        let mut state = self.state.write().await;
        let mut session: AgentSession = Box::new(ClaudeSession::with_supplied_session(
            &request,
            &state.deps,
            session,
        ));
        let summarizer = attach_summarizer(&state, request.agent_id, &session)
            .await
            .map_err(protocol_error)?;
        let exit_handle = session
            .start(&self.event_tx)
            .map_err(|error| protocol_error(error.to_string()))?;
        let announce = state
            .register_local_agent_context_with_summarizer(
                self.host_id,
                request.agent_id,
                session,
                None,
                summarizer,
            )
            .map_err(protocol_error)?;
        if let Some(summarizer) = state
            .local_agents
            .get(&request.agent_id)
            .and_then(|context| context.summarizer.as_ref())
        {
            summarizer.activate();
        }
        let AgentEvent::AgentUp { agent } = &announce else {
            unreachable!("registration always announces AgentUp")
        };
        let agent = agent.clone();
        state.local_agent_events.emit(announce);
        super::lifecycle::monitor_session_exit(
            exit_handle,
            self.event_tx.clone(),
            request.agent_id,
        );
        Ok(agent)
    }

    /// Register a Codex agent over a supplied session (a recording, usually).
    #[cfg(unix)]
    pub(crate) async fn register_codex_session(
        &self,
        name: String,
        working_dir: PathBuf,
        provider: codex::Session,
    ) -> Result<Agent, ProtocolError> {
        use crate::agents::codex::CodexBackend;
        use crate::agents::{AgentBackend, AgentKind, AgentRecord};
        let record = AgentRecord {
            id: Uuid::new_v4(),
            host_id: self.host_id,
            name: Some(name),
            command: "codex".into(),
            working_dir,
            kind: AgentKind::Codex,
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        };
        let backend = CodexBackend::with_session(record, provider);
        let agent_id = backend.agent_id();
        let mut session: AgentSession = Box::new(backend);
        let mut state = self.state.write().await;
        let summarizer = attach_summarizer(&state, agent_id, &session)
            .await
            .map_err(|message| ProtocolError::ServerError { message })?;
        session
            .start(&self.event_tx)
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
        let announce = state
            .register_local_agent_context_with_summarizer(
                self.host_id,
                agent_id,
                session,
                None,
                summarizer,
            )
            .map_err(|message| ProtocolError::ServerError { message })?;
        if let Some(summarizer) = state
            .local_agents
            .get(&agent_id)
            .and_then(|context| context.summarizer.as_ref())
        {
            summarizer.activate();
        }
        let AgentEvent::AgentUp { agent } = &announce else {
            unreachable!("registration always announces AgentUp")
        };
        let agent = agent.clone();
        state.local_agent_events.emit(announce);
        Ok(agent)
    }

    pub(crate) async fn end_scripted_session(&self, agent_id: Uuid) {
        self.event_tx
            .send(SessionEvent::Ended { agent_id })
            .await
            .expect("scripted session event loop should be running");
    }

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

impl Drop for AgentRuntime {
    fn drop(&mut self) {
        self.artifact_sweeper.abort();
        if let Some(path) = self.test_cleanup.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl LocalAgentHostFactory for AgentRuntimeFactory {
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
            Arc::new(artifacts::SystemClock),
            config.repository_roots,
        )
        .map(|host| host as Arc<dyn LocalAgentHost>)
    }

    fn restore_prepared(
        &self,
        state_path: &Path,
        state: PreparedHostState,
    ) -> Result<(), ProtocolError> {
        restore_prepared_at(state_path, state)
    }
}

pub(crate) fn restore_prepared_at(
    state_path: &Path,
    state: PreparedHostState,
) -> Result<(), ProtocolError> {
    let agents: Vec<suspend::SuspendedAgent> =
        serde_json::from_slice(&state.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to decode prepared host state: {error}"),
        })?;
    let mut retained =
        suspend::load_suspended(state_path).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to load retained state: {error}"),
        })?;
    let prepared_ids: std::collections::HashSet<_> = agents
        .iter()
        .map(suspend::SuspendedAgent::agent_id)
        .collect();
    retained
        .agents
        .retain(|agent| !prepared_ids.contains(&agent.agent_id()));
    retained.agents.extend(agents);
    if !retained.agents.is_empty() {
        suspend::save_suspended(state_path, &retained).map_err(|error| {
            ProtocolError::ServerError {
                message: format!("failed to restore retained state: {error}"),
            }
        })?;
    }
    Ok(())
}

async fn abort_prepared_after_error(
    agent_state: &SharedAgentServiceState,
    state_path: &Path,
    agents: &[suspend::SuspendedAgent],
    primary: String,
) -> ProtocolError {
    let message = match abort_server_suspend(agent_state, state_path, agents).await {
        Ok(()) => primary,
        Err(abort) => format!("{primary}; abort also failed: {abort}"),
    };
    ProtocolError::ServerError { message }
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
    fn adjacent_codex_socket_path(server_socket_path: &Path) -> PathBuf {
        let hash = server_socket_path
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        server_socket_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("c{hash:016x}.sock"))
    }

    let adjacent = adjacent_codex_socket_path(server_socket_path);
    if adjacent.as_os_str().as_bytes().len() <= MAX_CODEX_SOCKET_PATH_BYTES {
        Ok(adjacent)
    } else {
        // Move only the Codex runtime socket when the configured amux
        // directory leaves too little room for codex's sun_path cap.
        secure_codex_fallback_directory(fallback_dir)?;
        Ok(fallback_dir.join(adjacent.file_name().expect("Codex socket has a filename")))
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
impl LocalAgentHost for AgentRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn capabilities(&self) -> model::Capabilities {
        model::Capabilities {
            features: Vec::new(),
            supported_agent_types: vec![
                model::SupportedAgentType {
                    agent_type: model::AGENT_TYPE_CLAUDE.into(),
                },
                #[cfg(unix)]
                model::SupportedAgentType {
                    agent_type: model::AGENT_TYPE_CODEX.into(),
                },
                #[cfg(debug_assertions)]
                model::SupportedAgentType {
                    agent_type: model::AGENT_TYPE_TEST_AGENT.into(),
                },
            ],
        }
    }

    async fn agent(&self, agent_id: Uuid) -> Result<Agent, ProtocolError> {
        self.state()
            .read()
            .await
            .local_agents
            .get(&agent_id)
            .map(|context| context.record(self.host_id()).into())
            .ok_or(ProtocolError::NoAgentFound)
    }

    async fn list_repositories(
        &self,
        query: Option<String>,
        limit: u32,
    ) -> Result<model::ListRepositoriesResponse, ProtocolError> {
        let roots = self.repository_roots.clone();
        let recent = self.state.read().await.recent_projects.snapshot();
        tokio::task::spawn_blocking(move || crate::repositories::list(roots, recent, query, limit))
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })
    }

    async fn create(
        &self,
        request: CreateAgentRequest,
        operations: &host_api::OperationGate,
    ) -> Result<Agent, ProtocolError> {
        let req = request;
        // An agent runs in a directory, and one that is not here is a typo or
        // a path from another machine. Refused now, in the words of the host
        // that owns the path: started anyway, the session dies the moment its
        // process cannot enter its own working directory, and whoever asked
        // for it gets a conversation that vanishes instead of a reason.
        if !req.working_dir.is_dir() {
            return Err(ProtocolError::FailedPrecondition {
                message: format!(
                    "There is no directory at {} on this machine.",
                    req.working_dir.display()
                ),
            });
        }
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
        create_agent_record(
            self.state(),
            self.event_tx(),
            req,
            self.host_id(),
            operations,
        )
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

    async fn delete(
        &self,
        agent_id: Uuid,
        _operation: OperationBarrier,
    ) -> Result<(), ProtocolError> {
        let session_to_stop = {
            let mut us = self.state().write().await;
            delete_local_agent_and_emit_session_close(&mut us, self.host_id, agent_id)
        };

        match session_to_stop {
            Some(session) => {
                session.stop(StopPolicy::Interrupt).await;
                self.artifact_owners.delete_agent(agent_id)?;
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

    async fn set_agent_status(&self, request: HostSetAgentStatus) -> Result<(), ProtocolError> {
        let mut state = self.state().write().await;
        let context = state
            .local_agents
            .get_mut(&request.agent_id)
            .ok_or(ProtocolError::NoAgentFound)?;
        context.working_on = request.working_on.map(|text| crate::agents::WorkingOn {
            text,
            updated_at: chrono::Utc::now(),
        });
        let event = state
            .updated_agent_event(self.host_id(), request.agent_id)
            .map_err(|message| ProtocolError::ServerError { message })?;
        state.local_agent_events.emit(event);
        Ok(())
    }

    async fn send_input(
        &self,
        request: SessionInputRequest,
        operation: host_api::OperationLease,
    ) -> Result<(), ProtocolError> {
        let attachment_owner = if request.pin.is_empty() {
            None
        } else {
            Some(self.artifact_owners.owner(request.agent_id)?)
        };
        session::send_session_input(self, request, attachment_owner, operation).await
    }

    async fn put_artifact(
        &self,
        agent_id: Uuid,
        kind: model::ArtifactKind,
        name: String,
        mime: String,
        bytes: Vec<u8>,
        _operation: host_api::OperationLease,
    ) -> Result<model::ArtifactRef, ProtocolError> {
        self.agent(agent_id).await?;
        self.artifact_owners
            .owner(agent_id)?
            .put(kind, &name, &mime, &bytes)
            .map(artifacts::ArtifactMeta::into_reference)
            .map_err(store_error)
    }

    async fn put_artifact_by_agent(
        &self,
        agent_id: Uuid,
        kind: model::ArtifactKind,
        name: String,
        mime: String,
        bytes: Vec<u8>,
        _operation: host_api::OperationLease,
    ) -> Result<model::ArtifactRef, ProtocolError> {
        let state = self.state().read().await;
        let log = state
            .local_agents
            .get(&agent_id)
            .map(|context| &context.session)
            .ok_or(ProtocolError::NoAgentFound)?
            .attachment_log()
            .ok_or_else(|| ProtocolError::FailedPrecondition {
                message: "agent session has no structured output log".to_string(),
            })?;
        let owner = self.artifact_owners.owner(agent_id)?;
        let artifact = owner
            .put(kind, &name, &mime, &bytes)
            .map_err(store_error)?
            .into_reference();
        owner
            .pin(std::slice::from_ref(&artifact.id))
            .map_err(store_error)?;
        log.write(crate::agents::attachments_row(
            None,
            std::slice::from_ref(&artifact),
        ))
        .await;
        Ok(artifact)
    }

    async fn get_artifact(
        &self,
        agent_id: Uuid,
        id: model::ArtifactId,
        _operation: host_api::OperationLease,
    ) -> Result<ArtifactBlob, ProtocolError> {
        self.agent(agent_id).await?;
        let (artifact, bytes) = self
            .artifact_owners
            .owner(agent_id)?
            .get(&id)
            .map_err(store_error)?;
        Ok(ArtifactBlob {
            artifact: artifact.into_reference(),
            bytes,
        })
    }

    async fn diff(
        &self,
        agent_id: Uuid,
        base: model::DiffBase,
        _operation: host_api::OperationLease,
    ) -> Result<model::DiffResponse, ProtocolError> {
        let agent = self.agent(agent_id).await?;
        let owner = self.artifact_owners.owner(agent_id)?;
        compute_diff(&owner, &agent, base).await
    }

    async fn subscribe_session(
        &self,
        request: SessionRequest,
    ) -> Result<HostSessionStream, ProtocolError> {
        // An exact-cursor client already persisted the attachment index at
        // that cursor. Replaying the synthetic seq-0 attachment row would
        // violate the exclusive `after` contract and make every reconnect
        // appear to contain old data. Cold/tail opens still need the snapshot.
        let replay_attachments = (!is_exact_cursor(&request.args))
            .then(|| {
                self.artifact_owners
                    .owner(request.agent_id)
                    .ok()
                    .map(|owner| {
                        owner
                            .pinned()
                            .into_iter()
                            .map(artifacts::ArtifactMeta::into_reference)
                            .collect()
                    })
            })
            .flatten();
        session::subscribe_session_stream(self, request, replay_attachments).await
    }

    async fn agent_events_snapshot(&self) -> (Vec<AgentEvent>, mpsc::Receiver<AgentEvent>) {
        let mut state = self.state().write().await;
        let mut agents: Vec<_> = state
            .local_agents
            .values()
            .map(|context| context.record(self.host_id()).into())
            .collect();
        agents.sort_unstable_by_key(|agent: &Agent| agent.id);
        let through_revision = state.through_inventory_revision();
        let rx = state.local_agent_events.subscribe_drop_on_overflow();
        (
            vec![AgentEvent::HostInventory {
                host_id: self.host_id(),
                agents,
                through_revision,
            }],
            rx,
        )
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
                    Ok(HookOutcome::WithdrawSession) => {
                        session_to_stop = withdraw_agent(&mut state, self.host_id, agent_id);
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
                    Ok(ExternalHookBootstrap::Register(mut session)) => {
                        let summarizer = attach_summarizer(&state, agent_id, &session)
                            .await
                            .map_err(|message| ProtocolError::ServerError { message })?;
                        // Queue the bootstrap hook only after the tail-0 reader exists. The
                        // inert external session cannot publish it until start below.
                        session
                            .handle_hook_payload(&payload, &env)
                            .await
                            .map_err(HookError::into_protocol_error)?;
                        let exit_handle = session.start(self.event_tx()).map_err(|error| {
                            ProtocolError::ServerError {
                                message: error.to_string(),
                            }
                        })?;
                        match state.register_local_agent_context_with_summarizer(
                            self.host_id(),
                            agent_id,
                            session,
                            None,
                            summarizer,
                        ) {
                            Ok(announce) => {
                                if let Some(context) = state.local_agents.get_mut(&agent_id) {
                                    context.session.maybe_start_name_sniffer(self.event_tx());
                                    if let Some(summarizer) = &context.summarizer {
                                        summarizer.activate();
                                    }
                                }
                                state.local_agent_events.emit(announce);
                                super::lifecycle::monitor_session_exit(
                                    exit_handle,
                                    self.event_tx().clone(),
                                    agent_id,
                                );
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

    async fn resume(
        &self,
        state_path: PathBuf,
        operations: &host_api::OperationGate,
    ) -> Result<(u64, u64), ProtocolError> {
        let _resume = self.resume_lock.lock().await;
        let operation = operations.admit_mutation().await?;
        let suspended =
            suspend::load_suspended(&state_path).map_err(|error| ProtocolError::ServerError {
                message: format!("failed to load state: {error}"),
            })?;
        drop(operation);
        let mut pending = Vec::new();
        let mut already_running = 0usize;
        for agent in suspended.agents {
            let previous_attempt = agent.seal().and_then(|seal| {
                self.resume_attempts
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get(&seal.id)
                    .copied()
            });
            if let Some(attempt) = previous_attempt
                && self.state().read().await.contains_agent_id(&attempt)
            {
                already_running += 1;
            } else {
                pending.push(agent);
            }
        }
        let result = resume_agents(
            self.state(),
            self.event_tx(),
            pending,
            self.host_id(),
            operations,
            false,
            &state_path,
        )
        .await;
        if !result.resumed_agents.is_empty() {
            let mut attempts = self
                .resume_attempts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            attempts.extend(result.resumed_agents.iter().copied());
        }
        let _operation = operations.admit_mutation().await?;
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
        Ok((
            (result.resumed_count + already_running) as u64,
            result.failed_count as u64,
        ))
    }

    async fn prepare_update(&self) -> Result<PreparedHostState, ProtocolError> {
        let _resume = self.resume_lock.lock().await;
        let (state, errors) = prepare_server_suspend(self.state()).await;
        if !errors.is_empty() {
            return Err(ProtocolError::ServerError {
                message: errors.join("; "),
            });
        }
        let agent_ids = state
            .agents
            .iter()
            .map(suspend::SuspendedAgent::agent_id)
            .collect();
        let payload = match serde_json::to_vec(&state.agents) {
            Ok(payload) => payload,
            Err(error) => {
                return Err(abort_prepared_after_error(
                    self.state(),
                    &self.state_path,
                    &state.agents,
                    format!("failed to encode prepared agent state: {error}"),
                )
                .await);
            }
        };
        // The runtime owns its persistence format. Retain older failed
        // sessions and replace only records that are live again.
        let mut retained = match suspend::load_suspended(&self.state_path) {
            Ok(retained) => retained,
            Err(error) => {
                return Err(abort_prepared_after_error(
                    self.state(),
                    &self.state_path,
                    &state.agents,
                    format!("failed to load retained state: {error}"),
                )
                .await);
            }
        };
        if !state.agents.is_empty() {
            let active: std::collections::HashSet<_> = state
                .agents
                .iter()
                .map(suspend::SuspendedAgent::agent_id)
                .collect();
            retained
                .agents
                .retain(|agent| !active.contains(&agent.agent_id()));
            retained.agents.extend(state.agents.clone());
            if let Err(error) = suspend::save_suspended(&self.state_path, &retained) {
                return Err(abort_prepared_after_error(
                    self.state(),
                    &self.state_path,
                    &state.agents,
                    format!("failed to save retained state: {error}"),
                )
                .await);
            }
        }
        Ok(PreparedHostState { agent_ids, payload })
    }

    async fn abort_update(&self, state: PreparedHostState) -> Result<(), ProtocolError> {
        let _resume = self.resume_lock.lock().await;
        let agents: Vec<suspend::SuspendedAgent> =
            serde_json::from_slice(&state.payload).map_err(|error| ProtocolError::ServerError {
                message: format!("failed to decode prepared host state: {error}"),
            })?;
        abort_server_suspend(self.state(), &self.state_path, &agents)
            .await
            .map_err(|message| ProtocolError::ServerError { message })
    }

    async fn resume_update(
        &self,
        state: PreparedHostState,
        operations: &host_api::OperationGate,
    ) -> HostResumeBatch {
        let _resume = self.resume_lock.lock().await;
        let mut reports = Vec::new();
        let agents: Vec<suspend::SuspendedAgent> = match serde_json::from_slice(&state.payload) {
            Ok(agents) => agents,
            Err(error) => {
                return HostResumeBatch {
                    agents: state
                        .agent_ids
                        .into_iter()
                        .map(|agent_id| HostResumeResult {
                            agent_id,
                            status: HostResumeStatus::Failed,
                        })
                        .collect(),
                    cleanup_error: Some(format!("failed to decode prepared host state: {error}")),
                };
            }
        };
        for agent in &agents {
            let agent_id = agent.agent_id();
            let previous_attempt = agent.seal().and_then(|seal| {
                self.resume_attempts
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get(&seal.id)
                    .copied()
            });
            let previous_attempt_running = match previous_attempt {
                Some(attempt) => self.state().read().await.contains_agent_id(&attempt),
                None => false,
            };
            // Recovery may repeat after a process started but before its result
            // was persisted. Never construct or start that identity twice.
            let status = if previous_attempt_running
                || self.state().read().await.contains_agent_id(&agent_id)
            {
                HostResumeStatus::AlreadyRunning
            } else {
                let result = resume_agents(
                    self.state(),
                    self.event_tx(),
                    vec![agent.clone()],
                    self.host_id(),
                    operations,
                    true,
                    &self.state_path,
                )
                .await;
                if !result.resumed_agents.is_empty() {
                    let mut attempts = self
                        .resume_attempts
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    attempts.extend(result.resumed_agents.iter().copied());
                }
                if result.failed_count == 0 {
                    HostResumeStatus::Resumed
                } else {
                    HostResumeStatus::Failed
                }
            };
            reports.push(HostResumeResult { agent_id, status });
        }
        let successful: std::collections::HashSet<_> = reports
            .iter()
            .filter(|agent| agent.status != HostResumeStatus::Failed)
            .map(|agent| agent.agent_id)
            .collect();
        let cleanup = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut retained = suspend::load_suspended(&self.state_path)?;
            retained
                .agents
                .retain(|agent| !successful.contains(&agent.agent_id()));
            let retained_ids: std::collections::HashSet<_> = retained
                .agents
                .iter()
                .map(suspend::SuspendedAgent::agent_id)
                .collect();
            retained.agents.extend(
                agents
                    .iter()
                    .filter(|agent| {
                        !successful.contains(&agent.agent_id())
                            && !retained_ids.contains(&agent.agent_id())
                    })
                    .cloned(),
            );
            if retained.agents.is_empty() {
                suspend::remove_suspended(&self.state_path)?;
            } else {
                suspend::save_suspended(&self.state_path, &retained)?;
            }
            Ok(())
        })();
        HostResumeBatch {
            agents: reports,
            cleanup_error: cleanup.err().map(|error| error.to_string()),
        }
    }

    async fn stop_all(&self) {
        shutdown_server(self.state()).await;
    }

    async fn prepare_suspend(&self, state_path: PathBuf) -> Result<u64, ProtocolError> {
        let _resume = self.resume_lock.lock().await;
        let (suspended, errors) = prepare_server_suspend(self.state()).await;
        if !errors.is_empty() {
            return Err(ProtocolError::ServerError {
                message: errors.join("; "),
            });
        }
        let count = suspended.agents.len() as u64;
        if !suspended.agents.is_empty() {
            // A failed resume can leave older sessions on disk. Keep them when
            // preparing the live sessions, replacing only stale copies of an
            // agent that is running again under the same identity.
            let mut retained = match suspend::load_suspended(&state_path) {
                Ok(retained) => retained,
                Err(error) => {
                    return Err(abort_prepared_after_error(
                        self.state(),
                        &state_path,
                        &suspended.agents,
                        format!("failed to load retained state: {error}"),
                    )
                    .await);
                }
            };
            let active: std::collections::HashSet<_> = suspended
                .agents
                .iter()
                .map(|agent| agent.agent_id())
                .collect();
            retained
                .agents
                .retain(|agent| !active.contains(&agent.agent_id()));
            retained.agents.extend(suspended.agents.clone());
            if let Err(error) = suspend::save_suspended(&state_path, &retained) {
                return Err(abort_prepared_after_error(
                    self.state(),
                    &state_path,
                    &suspended.agents,
                    format!("failed to save state: {error}"),
                )
                .await);
            }
        }
        Ok(count)
    }

    async fn commit_suspend(&self) {
        commit_server_suspend(self.state(), self.host_id).await;
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

    async fn resource_inventory(
        &self,
        state_path: PathBuf,
    ) -> Result<HostResourceInventory, ProtocolError> {
        let live = self.state().read().await.local_agent_count();
        let suspended = crate::suspend::load_suspended(&state_path)
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?
            .agents
            .len();
        Ok(HostResourceInventory {
            agents: live.max(suspended),
            retained_artifacts: self.artifact_owners.retained_count().map_err(|error| {
                ProtocolError::ServerError {
                    message: error.to_string(),
                }
            })?,
        })
    }

    async fn debug_dump(&self, verbose: bool) -> Vec<HostDebugAgent> {
        let host_id = self.host_id();
        let state = self.state().read().await;
        let mut agents = Vec::with_capacity(state.local_agents.len());
        for context in state.local_agents.values() {
            agents.push(HostDebugAgent {
                agent: context.record(host_id).into(),
                session: if verbose {
                    context.session.debug_json(verbose).await.ok()
                } else {
                    None
                },
            });
        }
        agents.sort_unstable_by(|a, b| {
            a.agent
                .name
                .as_deref()
                .unwrap_or("")
                .cmp(b.agent.name.as_deref().unwrap_or(""))
                .then_with(|| a.agent.id.as_u128().cmp(&b.agent.id.as_u128()))
        });
        agents
    }
}

fn is_exact_cursor(args: &model::SessionArgs) -> bool {
    use model::{ReplayQuery, SessionArgs};
    let query = match args {
        SessionArgs::ClaudePtyTranscriptV1(args) => args.replay_query.as_ref(),
        SessionArgs::ClaudeSdkV1(args) => args.replay_query.as_ref(),
        SessionArgs::CodexSdkV1(args) => args.replay_query.as_ref(),
        SessionArgs::TerminalV1(_) | SessionArgs::TestEchoV1 => None,
    };
    matches!(query, Some(ReplayQuery::After { .. }))
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
    host_id: Uuid,
    agent_id: Uuid,
) -> Option<AgentSession> {
    let session = delete_local_agent(us, host_id, agent_id);
    if session.is_some() {
        us.local_session_close_events
            .emit((agent_id, SessionCloseReason::AgentDeleted));
    }
    session
}

fn create_error_to_protocol(error: CreateAgentError) -> ProtocolError {
    match error {
        CreateAgentError::Unavailable(error) => error,
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

#[cfg(test)]
mod replay_query_tests {
    use super::*;

    #[test]
    fn only_structured_after_queries_are_exact_cursors() {
        let after = model::ReplayQuery::After {
            after: 42,
            tail_bound: Some(1_000),
        };
        let tail = model::ReplayQuery::TailCount {
            count: 1_000,
            tail_bound: Some(1_000),
        };
        assert!(is_exact_cursor(&model::SessionArgs::ClaudeSdkV1(
            model::ClaudeSdkV1Args {
                replay_query: Some(after),
            },
        )));
        assert!(!is_exact_cursor(&model::SessionArgs::ClaudeSdkV1(
            model::ClaudeSdkV1Args {
                replay_query: Some(tail),
            },
        )));
        assert!(!is_exact_cursor(&model::SessionArgs::TestEchoV1));
    }
}

#[cfg(test)]
mod summarizer_registration_tests {
    use std::future;

    use claude::pty::{DelaySource, HookSource, PtySource, Session, Sources, TranscriptSource};
    use tokio::sync::mpsc;

    use super::*;

    fn permission_hook(agent_id: Uuid) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "session_id": agent_id,
            "transcript_path": "/tmp/amux-summarizer-registration.jsonl",
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": {"command": "echo one"},
        }))
        .unwrap()
    }

    fn stop_hook(agent_id: Uuid) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": agent_id,
            "transcript_path": "/tmp/amux-summarizer-registration.jsonl",
            "cwd": "/tmp",
            "last_assistant_message": "finished once",
            "stop_hook_active": false,
        }))
        .unwrap()
    }

    fn supplied_session() -> (Session, mpsc::Sender<claude::hooks::HookPayload>) {
        let (_output_tx, output) = mpsc::channel(1);
        let (hooks, hook_tx) = HookSource::channel(8);
        let session = claude::pty::from_sources(
            Sources {
                pty: PtySource {
                    output,
                    writer: Box::new(tokio::io::sink()),
                    handle: None,
                    exit: Box::pin(future::pending()),
                },
                hooks,
                transcript: TranscriptSource::live(),
                version: claude::version::ClaudeVersion(semver::Version::new(2, 1, 251)),
                delays: DelaySource::live(),
            },
            &claude::pty::keymap::KeymapSources::default(),
        );
        (session, hook_tx)
    }

    async fn assert_first_row_reached_summary(host: &AgentRuntime, agent_id: Uuid) {
        let summary = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let summary = host
                    .state
                    .read()
                    .await
                    .local_agents
                    .get(&agent_id)
                    .and_then(|context| context.summary.clone());
                if summary.as_ref().is_some_and(|summary| {
                    summary.through >= 1
                        && summary.summary.attention
                            == model::Attention::NeedsYou {
                                why: model::Why::Permission,
                            }
                }) {
                    break summary.unwrap();
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let summary = match summary {
            Ok(summary) => summary,
            Err(_) => {
                let (summary, log) = {
                    let state = host.state.read().await;
                    let context = state.local_agents.get(&agent_id).unwrap();
                    (
                        context.summary.clone(),
                        context.session.attachment_log().unwrap(),
                    )
                };
                panic!(
                    "row one did not reach the registered agent summary; summary={summary:?}, log_through={}",
                    log.current_seq().await
                );
            }
        };

        assert_eq!(
            summary.summary.attention,
            model::Attention::NeedsYou {
                why: model::Why::Permission
            }
        );
        assert!(
            !summary
                .summary
                .unknown
                .contains(&model::SummaryField::Attention),
            "row one must be folded from a Start baseline, not skipped behind Truncated"
        );
    }

    #[tokio::test]
    async fn daemon_summarizer_registration_paths_fold_the_immediate_first_row() {
        let external_host = AgentRuntime::new(Uuid::new_v4());
        let external_id = Uuid::new_v4();
        <AgentRuntime as LocalAgentHost>::handle_hook(
            &external_host,
            external_id,
            permission_hook(external_id),
            HookEnvironment::new(),
            true,
        )
        .await
        .unwrap();
        assert_first_row_reached_summary(&external_host, external_id).await;

        let supplied_host = AgentRuntime::new(Uuid::new_v4());
        let supplied_id = Uuid::new_v4();
        let (session, hook_tx) = supplied_session();
        hook_tx
            .send(
                claude::hooks::parse(&permission_hook(supplied_id))
                    .expect("permission hook fixture parses"),
            )
            .await
            .unwrap();
        supplied_host
            .register_claude_pty_session(
                CreateAgentRequest {
                    agent_id: supplied_id,
                    host_id: None,
                    name: Some("supplied-summary".into()),
                    agent_type: AgentType::Claude {
                        driver: model::ClaudeDriver::Pty,
                    },
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                    args: Vec::new(),
                    parent: None,
                    initial_prompt: None,
                },
                session,
            )
            .await
            .unwrap();
        assert_first_row_reached_summary(&supplied_host, supplied_id).await;

        external_host.stop_all().await;
        supplied_host.stop_all().await;
    }

    #[tokio::test]
    async fn daemon_scripted_stop_publishes_one_parent_completion() {
        let host_id = Uuid::new_v4();
        let host = AgentRuntime::new(host_id);
        let agent_id = Uuid::new_v4();
        let parent_id = Uuid::new_v4();
        host.register_scripted_claude(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: Some("scripted-child".into()),
            agent_type: AgentType::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
            parent: Some(model::AgentParent {
                agent_id: parent_id,
                host_id,
            }),
            initial_prompt: None,
        })
        .await
        .unwrap();
        let mut envelopes = host.state.write().await.outbound_envelopes.subscribe();

        host.deliver_scripted_hook(agent_id, stop_hook(agent_id))
            .await
            .unwrap();

        let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), envelopes.recv())
            .await
            .expect("completion arrives")
            .expect("outbound envelope stream stays open");
        assert_eq!(envelope.kind, model::envelope::EnvelopeKind::Completed);
        assert_eq!(envelope.text, "finished once");
        assert_eq!(envelope.to.agent_id, parent_id);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), envelopes.recv())
                .await
                .is_err(),
            "one Stop hook must not publish two completion envelopes"
        );

        host.stop_all().await;
    }
}

#[cfg(test)]
mod suspend_tests {
    use super::*;
    use crate::agents::{AgentBackend, TestAgentSession};
    use crate::suspend::{SuspendedAgent, SuspendedServerState};

    fn host(root: &Path) -> Arc<AgentRuntime> {
        std::fs::create_dir(root.join("data")).unwrap();
        let route = McpLaunchRoute::new(
            std::env::current_exe().unwrap(),
            None,
            root.join("amux.sock"),
            Uuid::new_v4(),
        )
        .unwrap();
        AgentRuntime::new_with_mcp_launch_route(route, root.join("keymaps"), root.join("data"))
            .unwrap()
    }

    fn record(id: Uuid, name: &str) -> SuspendedAgent {
        TestAgentSession::echo_for_tests(id, Some(name.into()))
            .suspended_state()
            .unwrap()
    }

    async fn register(host: &AgentRuntime, id: Uuid) {
        host.state
            .write()
            .await
            .insert_registered_local_agent(
                host.host_id,
                id,
                Box::new(TestAgentSession::echo_for_tests(id, Some("live".into()))),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn prepare_suspend_preserves_older_records_and_replaces_stale_active_copies() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let state_path = root.path().join("state.yaml");
        let older = record(Uuid::new_v4(), "previously suspended");
        let older_yaml = serde_yaml::to_string(&older).unwrap();
        let active_id = Uuid::new_v4();
        suspend::save_suspended(
            &state_path,
            &SuspendedServerState {
                agents: vec![older.clone(), record(active_id, "stale")],
            },
        )
        .unwrap();
        register(&host, active_id).await;

        for _ in 0..2 {
            assert_eq!(host.prepare_suspend(state_path.clone()).await.unwrap(), 1);
            assert_eq!(host.agent_count().await, 1);
            let saved = suspend::load_suspended(&state_path).unwrap();
            assert_eq!(saved.agents.len(), 2);
            assert_eq!(serde_yaml::to_string(&saved.agents[0]).unwrap(), older_yaml);
            assert_eq!(saved.agents[1].agent_id(), active_id);
            assert_eq!(saved.agents[1].name(), Some("live"));
        }
        host.commit_suspend().await;
        assert_eq!(host.agent_count().await, 0);
        assert_eq!(
            suspend::load_suspended(&state_path).unwrap().agents.len(),
            2
        );
    }

    #[tokio::test]
    async fn prepare_suspend_refuses_unreadable_retained_state_without_stopping_agents() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let state_path = root.path().join("state.yaml");
        let saved_path = root.path().join("suspended.yaml");
        let original = b"agents: [invalid";
        std::fs::write(&saved_path, original).unwrap();
        let active_id = Uuid::new_v4();
        register(&host, active_id).await;

        assert!(
            host.prepare_suspend(state_path)
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to load retained state")
        );
        assert_eq!(std::fs::read(&saved_path).unwrap(), original);
        assert!(host.agent(active_id).await.is_ok());
        host.stop_all().await;
    }

    #[tokio::test]
    async fn prepare_suspend_write_failure_preserves_saved_and_live_agents() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let state_path = root.path().join("state.yaml");
        suspend::save_suspended(
            &state_path,
            &SuspendedServerState {
                agents: vec![record(Uuid::new_v4(), "older")],
            },
        )
        .unwrap();
        let saved_path = root.path().join("suspended.yaml");
        let original = std::fs::read(&saved_path).unwrap();
        std::fs::create_dir(root.path().join("suspended.yaml.tmp")).unwrap();
        let active_id = Uuid::new_v4();
        register(&host, active_id).await;

        assert!(
            host.prepare_suspend(state_path)
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to save state")
        );
        assert_eq!(std::fs::read(&saved_path).unwrap(), original);
        assert!(host.agent(active_id).await.is_ok());
        host.stop_all().await;
    }

    #[tokio::test]
    async fn prepare_suspend_without_live_agents_leaves_saved_state_untouched() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let state_path = root.path().join("state.yaml");
        suspend::save_suspended(
            &state_path,
            &SuspendedServerState {
                agents: vec![record(Uuid::new_v4(), "older")],
            },
        )
        .unwrap();
        let saved_path = root.path().join("suspended.yaml");
        let original = std::fs::read(&saved_path).unwrap();

        assert_eq!(host.prepare_suspend(state_path).await.unwrap(), 0);
        assert_eq!(std::fs::read(&saved_path).unwrap(), original);
    }

    #[tokio::test]
    async fn daemon_protocol_seal_abort_invalidates_before_emitting_provider_unparks() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let agent_id = Uuid::new_v4();
        register(&host, agent_id).await;
        let log = host
            .state
            .read()
            .await
            .local_agents
            .get(&agent_id)
            .unwrap()
            .session
            .attachment_log()
            .unwrap();
        assert_eq!(
            log.try_write(serde_json::json!({"type": "pre-seal"}))
                .await
                .unwrap(),
            1
        );

        let prepared = host.prepare_update().await.unwrap();
        let agents: Vec<SuspendedAgent> = serde_json::from_slice(&prepared.payload).unwrap();
        let seal = agents[0].seal().unwrap();
        assert_eq!(seal.through, 1);

        let emitting_log = log.clone();
        let emitting = tokio::spawn(async move {
            emitting_log
                .write(serde_json::json!({"type": "held-input-result"}))
                .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(log.current_seq().await, seal.through);

        host.abort_update(prepared).await.unwrap();
        emitting.await.unwrap();
        assert_eq!(log.current_seq().await, seal.through + 1);
        assert!(
            suspend::load_suspended(&host.state_path)
                .unwrap()
                .agents
                .is_empty()
        );
        assert!(
            suspend::consume_seals(&host.state_path, &agents)
                .unwrap()
                .contains(&agent_id)
        );

        host.stop_all().await;
    }

    #[tokio::test]
    async fn daemon_protocol_seal_replayed_resume_does_not_start_publishers_twice() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let original_id = Uuid::new_v4();
        let agent = SuspendedAgent::TestAgent {
            agent_id: original_id,
            name: Some("consumed-before-start".to_string()),
            command: crate::agents::TEST_ECHO_COMMAND.to_string(),
            working_dir: root.path().to_path_buf(),
            terminal_size: None,
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
            seal: Some(crate::agents::SealedAt {
                id: Uuid::new_v4(),
                through: 12,
            }),
        };
        suspend::consume_seals(&host.state_path, std::slice::from_ref(&agent)).unwrap();
        let prepared = PreparedHostState {
            agent_ids: vec![original_id],
            payload: serde_json::to_vec(&vec![agent]).unwrap(),
        };
        let operations = host_api::OperationGate::default();

        let first = host.resume_update(prepared.clone(), &operations).await;
        assert_eq!(first.agents[0].status, HostResumeStatus::Resumed);
        let replacement = *host.state.read().await.local_agents.keys().next().unwrap();
        assert_ne!(replacement, original_id);

        let repeated = host.resume_update(prepared, &operations).await;
        assert_eq!(repeated.agents[0].status, HostResumeStatus::AlreadyRunning);
        assert_eq!(host.agent_count().await, 1);
        assert!(host.state.read().await.contains_agent_id(&replacement));

        host.stop_all().await;
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

        let keymap_dir = temp.path().join("keymaps");
        let host = AgentRuntime::new_with_mcp_launch_route(
            route.clone(),
            keymap_dir.clone(),
            temp.path().to_path_buf(),
        )
        .unwrap();

        assert_eq!(host.host_id(), route.host_id());
        let state = host.state().read().await;
        let deps = &state.deps;
        assert_eq!(deps.mcp_launch_route, route);
        assert_eq!(deps.claude_user_keymap_dir, keymap_dir);
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
