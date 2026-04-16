use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agent::StopPolicy;
use crate::protocol::message::{Message, ProtocolError, RoutableMessage};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;
use crate::server::routing::delete_local_agent;

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    agent_id: Uuid,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let session_to_stop = {
        let mut us = ctx.user_state.write().await;
        delete_local_agent(&mut us, agent_id)
    };
    let response_message = match session_to_stop {
        Some(session) => {
            session.stop(StopPolicy::Interrupt).await;
            RoutableMessage::DeleteAgentResult {
                agent_id,
                error: None,
            }
        }
        None => RoutableMessage::DeleteAgentResult {
            agent_id,
            error: Some(ProtocolError::ServerError {
                message: format!("Agent not found: {agent_id}"),
            }),
        },
    };
    let _ = tx
        .send(Message::routable(
            reply_src,
            reply_dst,
            request_id,
            &response_message,
        ))
        .await;
    Ok(())
}
