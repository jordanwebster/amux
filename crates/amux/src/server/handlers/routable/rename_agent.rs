use tokio::sync::mpsc;

use crate::protocol::message::{Message, ProtocolError, RenameAgentRequest, RoutableMessage};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;
use crate::server::routing::rename_local_agent;

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    req: RenameAgentRequest,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let agent_id = req.agent_id;
    let host_id = {
        let state = ctx.state.read().await;
        state.host_id
    };
    let response_message = {
        let mut us = ctx.user_state.write().await;
        match rename_local_agent(&mut us, host_id, &req) {
            Ok(_) => RoutableMessage::RenameAgentResult {
                agent_id,
                error: None,
            },
            Err(e) => RoutableMessage::RenameAgentResult {
                agent_id,
                error: Some(ProtocolError::ServerError {
                    message: e.to_string(),
                }),
            },
        }
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
