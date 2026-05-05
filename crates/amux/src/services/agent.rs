//! AgentService implementation for the protobuf AgentService surface.

use uuid::Uuid;

mod open_session;

use std::sync::Arc;

pub(crate) use open_session::OpenSessionCall;
use tokio::sync::{RwLock, mpsc};

use crate::agent::{SessionEvent, StopPolicy};
use crate::protocol;
use crate::protocol::message::{AgentType, CreateAgentRequest, ProtocolError, RenameAgentRequest};
use crate::protocol::wire::{CreateAgentConfig, CreateAgentRpcRequest};
use crate::server::{
    CreateAgentError, RenameAgentError, ServerUserState, begin_open_sessions_closing_for_agent,
    create_agent_record, delete_local_agent, finish_open_sessions_with_error,
    rename_local_agent_record,
};

pub(crate) struct AgentService;

#[derive(Clone)]
pub(crate) struct AgentServiceCtx {
    user_state: Arc<RwLock<ServerUserState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    user_id: Uuid,
    host_id: Uuid,
    is_cloud_server: bool,
}

impl AgentServiceCtx {
    pub(crate) fn new(
        user_state: Arc<RwLock<ServerUserState>>,
        event_tx: mpsc::Sender<SessionEvent>,
        user_id: Uuid,
        host_id: Uuid,
        is_cloud_server: bool,
    ) -> Self {
        Self {
            user_state,
            event_tx,
            user_id,
            host_id,
            is_cloud_server,
        }
    }

    pub(crate) fn user_state(&self) -> &Arc<RwLock<ServerUserState>> {
        &self.user_state
    }

    pub(crate) fn event_tx(&self) -> &mpsc::Sender<SessionEvent> {
        &self.event_tx
    }

    pub(crate) fn user_id(&self) -> Uuid {
        self.user_id
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }
}

impl AgentService {
    pub(crate) async fn list(ctx: &AgentServiceCtx) -> Vec<protocol::Agent> {
        ctx.user_state().read().await.list_agents()
    }

    pub(crate) async fn resolve(
        ctx: &AgentServiceCtx,
        identifier: &str,
    ) -> Option<protocol::Agent> {
        ctx.user_state().read().await.resolve_agent(identifier)
    }

    pub(crate) async fn create(
        ctx: &AgentServiceCtx,
        request: CreateAgentRpcRequest,
    ) -> Result<protocol::Agent, ProtocolError> {
        if ctx.is_cloud_server() {
            return Err(ProtocolError::ServerError {
                message: "cloud relays do not host local agents".to_string(),
            });
        }

        let agent_id = request.agent_id.unwrap_or_else(Uuid::new_v4);
        let req = create_rpc_to_domain_request(agent_id, request)?;

        create_agent_record(
            ctx.user_state(),
            ctx.event_tx(),
            req,
            ctx.user_id(),
            ctx.host_id(),
        )
        .await
        .map(Into::into)
        .map_err(create_error_to_protocol)
    }

    pub(crate) async fn rename(
        ctx: &AgentServiceCtx,
        request: RenameAgentRequest,
    ) -> Result<protocol::Agent, ProtocolError> {
        let host_id = ctx.host_id();
        let mut us = ctx.user_state().write().await;
        rename_local_agent_record(&mut us, host_id, &request)
            .map(Into::into)
            .map_err(rename_error_to_protocol)
    }

    pub(crate) async fn delete(ctx: &AgentServiceCtx, agent_id: Uuid) -> Result<(), ProtocolError> {
        let (session_to_stop, open_session_closings) = {
            let mut us = ctx.user_state().write().await;
            let session = delete_local_agent(&mut us, agent_id);
            let closings = if session.is_some() {
                begin_open_sessions_closing_for_agent(&mut us, agent_id)
            } else {
                Vec::new()
            };
            (session, closings)
        };

        finish_open_sessions_with_error(
            ctx.user_state(),
            open_session_closings,
            ProtocolError::Cancelled {
                message: format!("OpenSession cancelled because agent {agent_id} was deleted"),
            },
        )
        .await;

        match session_to_stop {
            Some(session) => {
                session.stop(StopPolicy::Interrupt).await;
                Ok(())
            }
            None => Err(ProtocolError::NoAgentFound),
        }
    }
}

fn create_error_to_protocol(error: CreateAgentError) -> ProtocolError {
    match error {
        err @ CreateAgentError::LimitReached { .. } => ProtocolError::ResourceExhausted {
            message: err.to_string(),
        },
        err @ CreateAgentError::AlreadyExists(_) => ProtocolError::AlreadyExists {
            message: err.to_string(),
        },
        err @ CreateAgentError::UnknownAgentType => ProtocolError::InvalidArgument {
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
            name: request.name,
            agent_type: AgentType::Claude,
            working_dir,
            terminal_size,
            args,
        }),
        CreateAgentConfig::ClaudeSdk { .. } => Err(ProtocolError::Unimplemented {
            message: "Claude SDK runtime is not implemented".to_string(),
        }),
        #[cfg(any(debug_assertions, test))]
        CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } => Ok(CreateAgentRequest {
            agent_id,
            name: request.name,
            agent_type: AgentType::TestAgent { command },
            working_dir,
            terminal_size,
            args: Vec::new(),
        }),
        #[cfg(not(any(debug_assertions, test)))]
        CreateAgentConfig::TestAgent { .. } => Err(ProtocolError::Unimplemented {
            message: "test-agent creation over protobuf is not available in release builds"
                .to_string(),
        }),
    }
}
