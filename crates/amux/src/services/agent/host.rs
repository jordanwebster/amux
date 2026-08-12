//! The local agent runtime behind the [`LocalAgentHost`] seam.
//!
//! [`PtyAgentHost`] owns the live session registry ([`AgentServiceState`]),
//! the session-event loop, and the host's identity, and implements every
//! core→runtime call as a [`LocalAgentHost`] method. The rest of the core
//! holds an `Option<Arc<dyn LocalAgentHost>>` and never names these types.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::lifecycle::{
    CreateAgentError, RenameAgentError, commit_server_suspend, create_agent_record,
    delete_local_agent, prepare_server_suspend, rename_local_agent_record, resume_agents,
    shutdown_server, spawn_session_event_loop, withdraw_agent,
};
use super::{
    AgentServiceState, DebugAgent, LocalAgentHost, ResponseStream, SharedAgentServiceState,
    session_rpc,
};
use crate::agents::{
    Agent, AgentEvent, AgentSession, AgentType, CreateAgentConfig, CreateAgentRequest,
    CreateAgentRpcRequest, ExternalHookBootstrap, HookOutcome, RenameAgentRequest,
    SendInputRequest, SessionCloseReason, SessionEvent, StopPolicy, SubscribeSessionRequest,
    bootstrap_external_hook,
};
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
    /// Build the host and spawn its session-event loop. Needs only `host_id`;
    /// cloud-vs-device is decided by runtime guards in `AgentServiceCtx`, not
    /// by host presence.
    pub(crate) fn new(host_id: Uuid) -> Arc<Self> {
        let state = Arc::new(RwLock::new(AgentServiceState::new()));
        let (event_tx, event_rx) = mpsc::channel(256);
        spawn_session_event_loop(state.clone(), event_rx, host_id);
        Arc::new(Self {
            state,
            event_tx,
            host_id,
        })
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
}

#[async_trait]
impl LocalAgentHost for PtyAgentHost {
    async fn create(&self, request: CreateAgentRpcRequest) -> Result<Agent, ProtocolError> {
        let req = create_rpc_to_domain_request(request.agent_id, request)?;
        create_agent_record(self.state(), self.event_tx(), req, self.host_id())
            .await
            .map(Into::into)
            .map_err(create_error_to_protocol)
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
            .map(|context| context.session.to_agent(self.host_id()).agent_event())
            .collect();
        snapshot.sort_unstable_by_key(agent_event_sort_key);
        let rx = state.local_agent_events.subscribe_drop_on_overflow();
        (snapshot, rx)
    }

    async fn subscribe_agent_events(&self) -> mpsc::Receiver<AgentEvent> {
        self.state().write().await.local_agent_events.subscribe()
    }

    async fn handle_hook(
        &self,
        agent_id: Uuid,
        payload: Vec<u8>,
        external: bool,
    ) -> Result<(), ProtocolError> {
        tracing::debug!(%agent_id, external, "received Claude hook event");

        let mut session_to_stop = None;
        let result = {
            let mut state = self.state().write().await;
            if let Some(session) = state.agent_session_mut(&agent_id) {
                match session.handle_hook_payload(&payload).await {
                    Ok(HookOutcome::Noop | HookOutcome::KeepSession) => Ok(()),
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
                match bootstrap_external_hook(agent_id, &payload).await {
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
                record: context.session.to_agent(host_id),
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
    match request.agent {
        CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: request.name,
            agent_type: AgentType::Claude,
            working_dir,
            terminal_size,
            args,
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
