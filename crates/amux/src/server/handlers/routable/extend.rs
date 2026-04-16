use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::protocol::message::{Message, ProtocolError, RoutableMessage, SubscriptionId};
use crate::protocol::route::Route;
use crate::server::connection::{ConnectionContext, extend_subscription};
use crate::server::{SUBSCRIPTION_LEASE_DURATION, subscription_lease_ms};

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    request_id: u64,
    subscription_id: SubscriptionId,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let lease_deadline = Instant::now() + SUBSCRIPTION_LEASE_DURATION;
    let response_message =
        match extend_subscription(&ctx.user_state, subscription_id, lease_deadline).await {
            Some(agent_id) => {
                tracing::debug!(
                    subscription_id = %subscription_id,
                    agent_id = %agent_id,
                    lease_ms = subscription_lease_ms(),
                    "subscription extended"
                );
                RoutableMessage::ExtendSubscriptionResult {
                    subscription_id,
                    lease_ms: subscription_lease_ms(),
                    error: None,
                }
            }
            None => {
                tracing::debug!(subscription_id = %subscription_id, "late or unknown extend");
                RoutableMessage::ExtendSubscriptionResult {
                    subscription_id,
                    lease_ms: 0,
                    error: Some(ProtocolError::UnknownSubscription),
                }
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
