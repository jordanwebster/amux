use tokio::sync::mpsc;
use uuid::Uuid;

use crate::protocol::message::{Message, RoutableMessage, SubscriptionId, TerminalSize};
use crate::protocol::route::Route;
use crate::server::connection::ConnectionContext;
use crate::server::handlers::subscription::{
    SubscriptionHandle, remove_subscription_if_reply_failed, spawn_subscription_stream,
    subscribe_error,
};
use crate::server::routing::handle_subscribe;
use crate::server::{SubscriptionMode, subscription_lease_ms};

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    agent_id: Uuid,
    terminal_size: Option<TerminalSize>,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let result = handle_subscribe(&ctx.user_state, &agent_id, terminal_size).await;

    match result {
        Ok(buffer_reader) => {
            let subscription_id = SubscriptionId::random();
            let (handle, cancel_rx) = SubscriptionHandle::register(
                ctx,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                reply_src.clone(),
                reply_dst.clone(),
            )
            .await;
            if tx
                .send(Message::routable(
                    reply_src.clone(),
                    reply_dst.clone(),
                    request_id,
                    &RoutableMessage::SubscribeRawResult {
                        subscription_id,
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
                mode = SubscriptionMode::Raw.as_str(),
                lease_ms = subscription_lease_ms(),
                "subscription created"
            );

            spawn_subscription_stream(
                buffer_reader,
                handle,
                cancel_rx,
                |subscription_id, data| RoutableMessage::RawOutput {
                    subscription_id,
                    data,
                },
                ctx,
            )
            .await;

            Ok(())
        }
        Err(e) => {
            let _ = tx
                .send(Message::routable(
                    reply_src,
                    reply_dst,
                    request_id,
                    &RoutableMessage::SubscribeRawResult {
                        subscription_id: SubscriptionId::nil(),
                        lease_ms: 0,
                        error: Some(subscribe_error(&e)),
                    },
                ))
                .await;
            Ok(())
        }
    }
}
