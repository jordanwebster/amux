use crate::protocol::message::{RoutableMessage, SubscriptionCloseReason, SubscriptionId};
use crate::server::connection::{ConnectionContext, unsubscribe_subscription};
use crate::server::{SubscriptionEntry, send_routable_via_full_dst};

pub(super) async fn handle(
    subscription_id: SubscriptionId,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if let Some(entry) = unsubscribe_subscription(&ctx.user_state, subscription_id).await {
        let SubscriptionEntry {
            subscription_id,
            agent_id,
            cancel,
            dst,
            ..
        } = entry;
        drop(cancel);
        tracing::info!(
            subscription_id = %subscription_id,
            agent_id = %agent_id,
            "explicit unsubscribe handled"
        );
        let _ = send_routable_via_full_dst(
            &ctx.user_state,
            &dst,
            &RoutableMessage::SubscriptionClosed {
                subscription_id,
                reason: SubscriptionCloseReason::Unsubscribed,
            },
        )
        .await;
    } else {
        tracing::debug!(subscription_id = %subscription_id, "late or unknown unsubscribe");
    }
    Ok(())
}
