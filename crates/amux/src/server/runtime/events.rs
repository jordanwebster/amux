use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agent::{SessionEvent, StopPolicy};
use crate::protocol::message::AgentType;
use crate::server::ServerState;
use crate::server::routing::{apply_local_name_candidate, withdraw_agent};

pub(in crate::server) async fn handle_session_event(
    state: &Arc<RwLock<ServerState>>,
    event: SessionEvent,
) {
    match event {
        SessionEvent::Ended { agent_id, user_id } => {
            let user_state = {
                let s = state.read().await;
                s.user_state(&user_id)
            };
            if let Some(user_state) = user_state {
                let mut us = user_state.write().await;
                let _ = withdraw_agent(&mut us, agent_id);
            }
        }
        SessionEvent::Created {
            agent_id,
            user_id,
            agent_type,
            args,
        } => {
            if matches!(agent_type, AgentType::Claude)
                && args.contains(&"--fork-session".to_string())
                && let Some(pos) = args.iter().position(|a| a == "--resume")
                && let Some(source_id_str) = args.get(pos + 1)
                && let Ok(source_id) = source_id_str.parse::<Uuid>()
            {
                let user_state = {
                    let s = state.read().await;
                    s.user_state(&user_id)
                };
                if let Some(user_state) = user_state {
                    let withdrawn_session = {
                        let mut us = user_state.write().await;
                        let is_readonly = us.agents.get(&source_id).is_some_and(|s| s.readonly());
                        if is_readonly {
                            withdraw_agent(&mut us, source_id)
                        } else {
                            None
                        }
                    };
                    if let Some(session) = withdrawn_session {
                        session.stop(StopPolicy::Interrupt).await;
                        tracing::info!(
                            source = %source_id,
                            fork = %agent_id,
                            "withdrew readonly session (forked)"
                        );
                    }
                }
            }
            tracing::debug!(agent_id = %agent_id, ?agent_type, ?args, "session created");
        }
        SessionEvent::NameCandidateChanged {
            agent_id,
            user_id,
            name,
            source,
        } => {
            let (user_state, host_id) = {
                let s = state.read().await;
                (s.user_state(&user_id), s.host_id)
            };
            if let Some(user_state) = user_state {
                let outcome = {
                    let mut us = user_state.write().await;
                    apply_local_name_candidate(&mut us, host_id, agent_id, name.clone(), source)
                };
                tracing::debug!(
                    agent_id = %agent_id,
                    candidate = %name,
                    ?source,
                    ?outcome,
                    "processed local name candidate"
                );
            }
        }
    }
}
