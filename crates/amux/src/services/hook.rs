use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use crate::agent::{AgentSession, ExternalHookBootstrap, HookOutcome, SessionEvent, StopPolicy};
use crate::protocol::message::{HookProvider, ProtocolError};
use crate::server::{ServerUserState, broadcast_topology_event, withdraw_agent};

pub(crate) struct HookService;

#[derive(Clone)]
pub(crate) struct HookServiceCtx {
    user_state: Arc<RwLock<ServerUserState>>,
    event_tx: mpsc::Sender<SessionEvent>,
    user_id: Uuid,
    host_id: Uuid,
}

impl HookServiceCtx {
    pub(crate) fn new(
        user_state: Arc<RwLock<ServerUserState>>,
        event_tx: mpsc::Sender<SessionEvent>,
        user_id: Uuid,
        host_id: Uuid,
    ) -> Self {
        Self {
            user_state,
            event_tx,
            user_id,
            host_id,
        }
    }

    fn user_state(&self) -> &Arc<RwLock<ServerUserState>> {
        &self.user_state
    }

    fn event_tx(&self) -> &mpsc::Sender<SessionEvent> {
        &self.event_tx
    }

    fn user_id(&self) -> Uuid {
        self.user_id
    }

    fn host_id(&self) -> Uuid {
        self.host_id
    }
}

impl HookService {
    pub(crate) async fn handle(
        ctx: &HookServiceCtx,
        agent_id: Uuid,
        provider: HookProvider,
        payload: Vec<u8>,
        external: bool,
    ) -> Result<(), ProtocolError> {
        tracing::debug!(%agent_id, ?provider, external, "received hook event");

        let mut session_to_stop = None;
        let result = {
            let mut us = ctx.user_state().write().await;
            if let Some(session) = us.agent_session_mut(&agent_id) {
                match session.handle_hook(provider, &payload).await {
                    Ok(HookOutcome::Noop | HookOutcome::KeepSession) => Ok(()),
                    Ok(HookOutcome::WithdrawSession) => {
                        session_to_stop = withdraw_agent(&mut us, agent_id);
                        Ok(())
                    }
                    Err(error) => Err(error.into_protocol_error()),
                }
            } else if !external {
                tracing::warn!(%agent_id, ?provider, "hook target not found");
                Err(ProtocolError::NoAgentFound)
            } else {
                match AgentSession::bootstrap_external_hook(agent_id, provider, &payload).await {
                    Ok(ExternalHookBootstrap::Noop) => Ok(()),
                    Ok(ExternalHookBootstrap::Register(session)) => {
                        let info = session.to_agent(ctx.host_id());
                        match us.insert_registered_local_agent(agent_id, session, info) {
                            Ok(announce) => {
                                if let Some(session) = us.agent_session_mut(&agent_id) {
                                    session.maybe_start_name_sniffer(ctx.user_id(), ctx.event_tx());
                                }
                                broadcast_topology_event(&mut us, &announce, None);
                                tracing::info!(%agent_id, ?provider, "created readonly session from external hook");
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
}
