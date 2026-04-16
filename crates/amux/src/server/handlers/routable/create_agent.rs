use tokio::sync::mpsc;

use crate::protocol::message::{CreateAgentRequest, Message, ProtocolError, RoutableMessage};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;
use crate::server::routing::create_agent;

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    req: CreateAgentRequest,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let agent_id = req.agent_id;
    let (host_id, is_cloud_server) = {
        let state = ctx.state.read().await;
        (state.host_id, state.is_cloud_server)
    };
    let error = if is_cloud_server {
        Some("cloud relays do not host local agents".to_string())
    } else {
        create_agent(&ctx.user_state, &ctx.event_tx, req, ctx.user_id, host_id)
            .await
            .err()
    };
    let response_message = match error {
        None => RoutableMessage::CreateAgentResult {
            agent_id,
            error: None,
        },
        Some(message) => RoutableMessage::CreateAgentResult {
            agent_id,
            error: Some(ProtocolError::ServerError { message }),
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
