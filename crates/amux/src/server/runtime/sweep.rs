use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::time::Instant;

use super::forward::try_send_routable_via_full_dst;
use crate::protocol::message::{RoutableMessage, SubscriptionCloseReason};
use crate::server::{ServerState, SubscriptionEntry};

pub(in crate::server) async fn sweep_expired_subscriptions(state: &Arc<RwLock<ServerState>>) {
    let user_states: Vec<_> = {
        let s = state.read().await;
        s.users.values().cloned().collect()
    };
    let now = Instant::now();

    for user_state in user_states {
        let expired = {
            let mut us = user_state.write().await;
            let expired_ids: Vec<_> = us
                .active_subscriptions
                .iter()
                .filter_map(|(subscription_id, entry)| {
                    (entry.lease_deadline <= now).then_some(*subscription_id)
                })
                .collect();

            expired_ids
                .into_iter()
                .filter_map(|subscription_id| us.active_subscriptions.remove(&subscription_id))
                .collect::<Vec<_>>()
        };

        for entry in expired {
            let SubscriptionEntry {
                subscription_id,
                agent_id,
                mode,
                cancel,
                dst,
                ..
            } = entry;
            drop(cancel);
            tracing::info!(
                subscription_id = %subscription_id,
                agent_id = %agent_id,
                mode = mode.as_str(),
                "subscription lease expired"
            );
            let _ = try_send_routable_via_full_dst(
                &user_state,
                &dst,
                &RoutableMessage::SubscriptionClosed {
                    subscription_id,
                    reason: SubscriptionCloseReason::LeaseExpired,
                },
            )
            .await;
        }
    }
}
