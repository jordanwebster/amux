use tokio::sync::mpsc;
use uuid::Uuid;

use crate::protocol::message::{
    Message, ProtocolError, RoutableMessage, SubscribeQuery, SubscriptionId,
};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;
use crate::server::handlers::subscription::{
    SubscriptionHandle, remove_subscription_if_reply_failed, spawn_subscription_stream,
};
use crate::server::{SubscriptionMode, subscription_lease_ms};

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    agent_id: Uuid,
    query: Option<SubscribeQuery>,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if matches!(query, Some(SubscribeQuery::Unknown)) {
        let _ = tx
            .send(Message::routable(
                reply_src,
                reply_dst,
                request_id,
                &RoutableMessage::SubscribeStructuredResult {
                    subscription_id: SubscriptionId::nil(),
                    seq: 0,
                    structured_protocol: None,
                    lease_ms: 0,
                    error: Some(ProtocolError::UnsupportedSubscribeQuery),
                },
            ))
            .await;
        return Ok(());
    }
    let subscribed = {
        let us = ctx.user_state.read().await;
        let Some(session) = us.agents.get(&agent_id) else {
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &RoutableMessage::SubscribeStructuredResult {
                        subscription_id: SubscriptionId::nil(),
                        seq: 0,
                        structured_protocol: None,
                        lease_ms: 0,
                        error: Some(ProtocolError::NoAgentFound),
                    },
                ))
                .await;
            return Ok(());
        };
        session
            .subscribe_with_query(query)
            .await
            .map(|(reader, current_seq)| (reader, current_seq, session.structured_protocol()))
    };

    let Some((reader, current_seq, structured_protocol)) = subscribed else {
        let _ = tx
            .send(Message::routable(
                reply_src,
                reply_dst,
                request_id,
                &RoutableMessage::SubscribeStructuredResult {
                    subscription_id: SubscriptionId::nil(),
                    seq: 0,
                    structured_protocol: None,
                    lease_ms: 0,
                    error: Some(ProtocolError::NoAgentFound),
                },
            ))
            .await;
        return Ok(());
    };

    let subscription_id = SubscriptionId::random();
    let (handle, cancel_rx) = SubscriptionHandle::register(
        ctx,
        subscription_id,
        agent_id,
        SubscriptionMode::Structured,
        reply_src.clone(),
        reply_dst.clone(),
    )
    .await;
    if tx
        .send(Message::routable(
            reply_src.clone(),
            reply_dst.clone(),
            request_id,
            &RoutableMessage::SubscribeStructuredResult {
                subscription_id,
                seq: current_seq,
                structured_protocol,
                lease_ms: subscription_lease_ms(),
                error: None,
            },
        ))
        .await
        .is_err()
    {
        remove_subscription_if_reply_failed(&ctx.user_state, subscription_id).await;
        tracing::debug!(
            subscription_id = %subscription_id,
            agent_id = %agent_id,
            "subscribe result send failed before stream spawn"
        );
        return Ok(());
    }

    tracing::info!(
        subscription_id = %subscription_id,
        agent_id = %agent_id,
        mode = SubscriptionMode::Structured.as_str(),
        lease_ms = subscription_lease_ms(),
        "subscription created"
    );

    spawn_subscription_stream(
        reader,
        handle,
        cancel_rx,
        |subscription_id, envelope| RoutableMessage::StructuredOutput {
            subscription_id,
            seq: envelope.seq,
            payload: envelope.payload,
        },
        ctx,
    )
    .await;

    Ok(())
}
