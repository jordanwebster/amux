use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::protocol::message::{Message, ProtocolError, RoutableMessage};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;

pub(super) async fn handle_raw_input(
    agent_id: Uuid,
    data: Vec<u8>,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let us = ctx.user_state.read().await;
    if let Some(session) = us.agents.get(&agent_id)
        && let Some(pty) = session.pty_handle()
    {
        let _ = pty.send_input(data).await;
    }
    Ok(())
}

pub(super) struct StructuredInputReply {
    pub src: Route,
    pub dst: Route,
    pub request_id: u64,
}

pub(super) async fn handle_structured_input(
    tx: &mpsc::Sender<Message>,
    reply: StructuredInputReply,
    agent_id: Uuid,
    client_seq: u64,
    payload: Value,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    tracing::debug!(%agent_id, client_seq, "structured input received");
    let us = ctx.user_state.read().await;
    let error = if let Some(session) = us.agents.get(&agent_id) {
        session
            .send_structured_input(client_seq, payload)
            .await
            .err()
    } else {
        tracing::warn!(%agent_id, "structured input rejected: agent not found");
        Some(ProtocolError::NoAgentFound)
    };
    let _ = tx
        .send(Message::routable(
            reply.src,
            reply.dst,
            reply.request_id,
            &RoutableMessage::StructuredInputResult { agent_id, error },
        ))
        .await;
    Ok(())
}
